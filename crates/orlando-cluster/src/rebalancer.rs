use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;

use tokio::sync::broadcast;

use orlando_core::GrainActivator;
use orlando_runtime::GrainDirectory;

use crate::failure_detector::MembershipChange;
use crate::hash_ring::HashRing;

pub struct Rebalancer {
    ring: Arc<ArcSwap<HashRing>>,
    directory: Arc<GrainDirectory>,
    local_silo_id: String,
    change_rx: broadcast::Receiver<MembershipChange>,
}

impl Rebalancer {
    pub fn new(
        ring: Arc<ArcSwap<HashRing>>,
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
                    self.rebalance().await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "rebalancer lagged, running full rebalance");
                    self.rebalance().await;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!("membership change channel closed, rebalancer stopping");
                    break;
                }
            }
        }
    }

    async fn rebalance(&self) {
        let grain_ids = self.directory.grain_ids();
        if grain_ids.is_empty() {
            return;
        }

        let to_remove: Vec<_> = {
            let ring = self.ring.load();
            grain_ids
                .into_iter()
                .filter(|id| {
                    let grain_key = format!("{}/{}", id.type_name, id.key);
                    match ring.get(&grain_key) {
                        Some(target) => target.silo_id != self.local_silo_id,
                        None => false,
                    }
                })
                .collect()
        };

        if to_remove.is_empty() {
            return;
        }

        // Drop senders and await graceful deactivation so persistent grains
        // run on_deactivate (which saves state) before shutting down.
        let grace = Duration::from_secs(5);
        let deadline = tokio::time::Instant::now() + grace;

        for grain_id in &to_remove {
            tracing::info!(%grain_id, "grain rehashed to different silo, deactivating locally");
            if let Some(activation) = self.directory.remove(grain_id) {
                drop(activation.sender);
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    activation.task.abort();
                } else if tokio::time::timeout(remaining, activation.task).await.is_err() {
                    tracing::warn!(%grain_id, "grain did not deactivate within rebalance grace period");
                }
            }
        }

        tracing::info!(count = to_remove.len(), "rebalance complete");
    }
}
