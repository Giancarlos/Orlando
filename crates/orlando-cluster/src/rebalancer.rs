use std::sync::{Arc, RwLock};

use tokio::sync::broadcast;

use orlando_core::GrainActivator;
use orlando_runtime::GrainDirectory;

use crate::failure_detector::MembershipChange;
use crate::hash_ring::HashRing;

pub struct Rebalancer {
    ring: Arc<RwLock<HashRing>>,
    directory: Arc<GrainDirectory>,
    local_silo_id: String,
    change_rx: broadcast::Receiver<MembershipChange>,
}

impl Rebalancer {
    pub fn new(
        ring: Arc<RwLock<HashRing>>,
        directory: Arc<GrainDirectory>,
        local_silo_id: String,
        change_rx: broadcast::Receiver<MembershipChange>,
    ) -> Self {
        Self {
            ring,
            directory,
            local_silo_id,
            change_rx,
        }
    }

    pub async fn run(mut self) {
        loop {
            match self.change_rx.recv().await {
                Ok(change) => {
                    tracing::info!(?change, "membership changed, checking grain placement");
                    self.rebalance();
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "rebalancer lagged, running full rebalance");
                    self.rebalance();
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!("membership change channel closed, rebalancer stopping");
                    break;
                }
            }
        }
    }

    fn rebalance(&self) {
        let grain_ids = self.directory.grain_ids();
        if grain_ids.is_empty() {
            return;
        }

        // Pass 1: collect grains that need to move (hold read lock briefly)
        let to_remove: Vec<_> = {
            let ring = self.ring.read().expect("ring lock poisoned");
            grain_ids
                .into_iter()
                .filter(|id| {
                    let grain_key = format!("{}/{}", id.type_name, id.key);
                    match ring.get(&grain_key) {
                        Some(target) => target.silo_id != self.local_silo_id,
                        None => false, // ring empty, keep local
                    }
                })
                .collect()
        };

        // Pass 2: deactivate misplaced grains (no lock held)
        for grain_id in &to_remove {
            tracing::info!(%grain_id, "grain rehashed to different silo, deactivating locally");
            self.directory.remove(grain_id);
        }

        if !to_remove.is_empty() {
            tracing::info!(count = to_remove.len(), "rebalance complete");
        }
    }
}
