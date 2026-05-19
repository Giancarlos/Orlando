//! Local replica store: holds the latest replicated state for grains
//! on secondary clusters. Read-only — only the `ReplicationConsumer`
//! writes to it.

use std::collections::HashMap;
use std::sync::Mutex;

/// A single replica entry with its metadata.
#[derive(Debug, Clone)]
pub struct ReplicaEntry {
    /// Serialized grain state.
    pub payload: Vec<u8>,
    /// Replication sequence number.
    pub sequence: u64,
    /// Timestamp (millis since UNIX epoch) when the entry was produced.
    pub timestamp_millis: i64,
}

/// Thread-safe store of replicated grain state. The `ReplicationConsumer`
/// writes entries here; `ClusterGrainRef` reads from it for stale-read
/// serving.
#[derive(Debug, Default)]
pub struct ReplicaStore {
    entries: Mutex<HashMap<String, ReplicaEntry>>,
}

impl ReplicaStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(grain_type: &str, grain_key: &str) -> String {
        format!("{}/{}", grain_type, grain_key)
    }

    /// Update the replica for a grain. Called by `ReplicationConsumer`.
    pub fn update(
        &self,
        grain_type: &str,
        grain_key: &str,
        payload: Vec<u8>,
        sequence: u64,
        timestamp_millis: i64,
    ) {
        let mut entries = self.entries.lock().expect("replica store lock poisoned");
        entries.insert(
            Self::key(grain_type, grain_key),
            ReplicaEntry {
                payload,
                sequence,
                timestamp_millis,
            },
        );
    }

    /// Get the latest replica for a grain. Returns `None` if no replica exists.
    pub fn get(&self, grain_type: &str, grain_key: &str) -> Option<ReplicaEntry> {
        let entries = self.entries.lock().expect("replica store lock poisoned");
        entries.get(&Self::key(grain_type, grain_key)).cloned()
    }

    /// Check if a replica exists and is within the staleness threshold.
    pub fn is_fresh(&self, grain_type: &str, grain_key: &str, max_staleness_millis: i64) -> bool {
        let entries = self.entries.lock().expect("replica store lock poisoned");
        if let Some(entry) = entries.get(&Self::key(grain_type, grain_key)) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            (now - entry.timestamp_millis) <= max_staleness_millis
        } else {
            false
        }
    }

    /// Remove a replica entry.
    pub fn remove(&self, grain_type: &str, grain_key: &str) {
        let mut entries = self.entries.lock().expect("replica store lock poisoned");
        entries.remove(&Self::key(grain_type, grain_key));
    }
}
