//! Background task that pumps `ReplicationEntry` values from a channel
//! into the `ReplicationLog`. The persistent mailbox sends entries to the
//! channel via `ReplicationSink`; this task appends them durably.

use std::sync::Arc;

use tokio::sync::mpsc;

use orlando_core::replication::ReplicationEntry;
use orlando_persistence::ReplicationLog;

/// Spawn a background task that reads entries from `rx` and appends them
/// to the replication log. Returns the `JoinHandle` so the caller can
/// monitor for unexpected termination.
pub fn spawn_replication_pump(
    mut rx: mpsc::Receiver<ReplicationEntry>,
    log: Arc<dyn ReplicationLog>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(entry) = rx.recv().await {
            let grain_type = entry.grain_type.clone();
            let grain_key = entry.grain_key.clone();
            let seq = entry.sequence;

            if let Err(e) = log.append(entry).await {
                tracing::warn!(
                    %grain_type,
                    %grain_key,
                    sequence = seq,
                    error = %e,
                    "replication pump: failed to append entry"
                );
            } else {
                tracing::trace!(
                    %grain_type,
                    %grain_key,
                    sequence = seq,
                    "replication pump: entry appended"
                );
            }
        }
        tracing::debug!("replication pump: channel closed, shutting down");
    })
}
