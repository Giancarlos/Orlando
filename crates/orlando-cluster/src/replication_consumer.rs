//! Background task that polls the `ReplicationLog` for new entries and
//! applies them to a local replica store. Secondary clusters run one
//! consumer per replicated grain to maintain read-only copies.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use orlando_core::ClusterId;
use orlando_persistence::ReplicationLog;

use crate::store_wrapper::ReplicaStore;

/// Tracks replication state for a single grain on a secondary cluster.
pub struct ReplicationConsumer {
    grain_type: String,
    grain_key: String,
    #[allow(dead_code)]
    local_cluster: ClusterId,
    log: Arc<dyn ReplicationLog>,
    replica_store: Arc<ReplicaStore>,
    poll_interval: Duration,
    shutdown_rx: watch::Receiver<bool>,
}

impl ReplicationConsumer {
    pub fn new(
        grain_type: String,
        grain_key: String,
        local_cluster: ClusterId,
        log: Arc<dyn ReplicationLog>,
        replica_store: Arc<ReplicaStore>,
        poll_interval: Duration,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            grain_type,
            grain_key,
            local_cluster,
            log,
            replica_store,
            poll_interval,
            shutdown_rx,
        }
    }

    /// Run the consumer loop. Call via `tokio::spawn`.
    pub async fn run(mut self) {
        let mut last_sequence = 0u64;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.poll_interval) => {}
                _ = self.shutdown_rx.changed() => {
                    tracing::debug!(
                        grain_type = %self.grain_type,
                        grain_key = %self.grain_key,
                        "replication consumer shutting down"
                    );
                    return;
                }
            }

            match self
                .log
                .read_from(&self.grain_type, &self.grain_key, last_sequence, 100)
                .await
            {
                Ok(entries) if entries.is_empty() => {
                    // No new entries, continue polling
                }
                Ok(entries) => {
                    for entry in &entries {
                        tracing::debug!(
                            grain_type = %entry.grain_type,
                            grain_key = %entry.grain_key,
                            sequence = entry.sequence,
                            source = %entry.source_cluster,
                            "replication consumer: applying entry"
                        );
                    }

                    // Apply the latest full-state snapshot (skip intermediates)
                    if let Some(latest) = entries.last() {
                        self.replica_store.update(
                            &self.grain_type,
                            &self.grain_key,
                            latest.payload.clone(),
                            latest.sequence,
                            latest.timestamp_millis,
                        );
                        last_sequence = latest.sequence;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        grain_type = %self.grain_type,
                        grain_key = %self.grain_key,
                        error = %e,
                        "replication consumer: failed to read from log"
                    );
                }
            }
        }
    }
}
