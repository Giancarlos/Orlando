//! Lightweight channel-based sink for shipping state snapshots to the
//! replication log. The persistent mailbox calls `send()` after each handler
//! completion; a background task on the cluster side consumes entries and
//! appends them to the `ReplicationLog`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use orlando_core::replication::{ReplicationEntry, ReplicationEntryType};
use orlando_core::ClusterId;

/// A sink that the persistent mailbox pushes serialized state into after
/// each handler invocation. Non-blocking (try_send); if the channel is full
/// the entry is dropped and logged.
pub struct ReplicationSink {
    tx: mpsc::Sender<ReplicationEntry>,
    grain_type: String,
    grain_key: String,
    source_cluster: ClusterId,
    next_sequence: AtomicU64,
}

impl ReplicationSink {
    /// Create a new sink. `start_sequence` is typically the latest sequence
    /// from the replication log + 1.
    pub fn new(
        tx: mpsc::Sender<ReplicationEntry>,
        grain_type: String,
        grain_key: String,
        source_cluster: ClusterId,
        start_sequence: u64,
    ) -> Self {
        Self {
            tx,
            grain_type,
            grain_key,
            source_cluster,
            next_sequence: AtomicU64::new(start_sequence),
        }
    }

    /// Send a state snapshot to the replication channel. Non-blocking.
    pub fn send(&self, payload: Vec<u8>) {
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let entry = ReplicationEntry {
            grain_type: self.grain_type.clone(),
            grain_key: self.grain_key.clone(),
            sequence,
            timestamp_millis: now,
            source_cluster: self.source_cluster.clone(),
            entry_type: ReplicationEntryType::FullState,
            payload,
        };

        if let Err(e) = self.tx.try_send(entry) {
            tracing::warn!(
                grain_type = %self.grain_type,
                grain_key = %self.grain_key,
                sequence,
                "replication sink full or closed, entry dropped: {}",
                e
            );
        }
    }
}

impl std::fmt::Debug for ReplicationSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplicationSink")
            .field("grain_type", &self.grain_type)
            .field("grain_key", &self.grain_key)
            .field("source_cluster", &self.source_cluster)
            .finish()
    }
}
