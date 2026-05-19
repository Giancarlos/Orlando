//! Append-only replication log for cross-cluster state synchronization.
//!
//! The primary cluster appends entries after handler invocations. Secondary
//! clusters read from the log to maintain read-only replicas.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use orlando_core::replication::ReplicationEntry;

/// Append-only log of state changes per grain.
///
/// Primary appends after handler invocations; secondaries read to maintain
/// replicas. Implementations must be durable (entries survive process restart).
#[async_trait]
pub trait ReplicationLog: Send + Sync + 'static {
    /// Append an entry. Returns the assigned sequence number.
    /// Sequences must be strictly monotonic per grain.
    async fn append(&self, entry: ReplicationEntry) -> Result<u64, ReplicationError>;

    /// Read entries after a sequence number, capped by `limit`.
    async fn read_from(
        &self,
        grain_type: &str,
        grain_key: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<ReplicationEntry>, ReplicationError>;

    /// Latest sequence number for a grain (0 if no entries exist).
    async fn latest_sequence(
        &self,
        grain_type: &str,
        grain_key: &str,
    ) -> Result<u64, ReplicationError>;

    /// Delete entries before a sequence number. Returns count deleted.
    /// Used for log compaction after a full-state snapshot.
    async fn truncate(
        &self,
        grain_type: &str,
        grain_key: &str,
        before_sequence: u64,
    ) -> Result<u64, ReplicationError>;
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReplicationError {
    #[error("replication log backend unavailable: {0}")]
    Unavailable(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("deserialization error: {0}")]
    Deserialization(String),
    #[error("sequence conflict: expected {expected}, got {actual}")]
    SequenceConflict { expected: u64, actual: u64 },
    #[error("replication backend error: {0}")]
    Backend(String),
}

// ---------------------------------------------------------------------------
// In-memory implementation (testing)
// ---------------------------------------------------------------------------

/// In-memory replication log for tests and single-process multi-cluster setups.
#[derive(Debug, Default)]
pub struct InMemoryReplicationLog {
    /// grain_key_composite -> sorted Vec of entries
    entries: Mutex<HashMap<String, Vec<ReplicationEntry>>>,
}

impl InMemoryReplicationLog {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(grain_type: &str, grain_key: &str) -> String {
        format!("{}/{}", grain_type, grain_key)
    }
}

#[async_trait]
impl ReplicationLog for InMemoryReplicationLog {
    async fn append(&self, entry: ReplicationEntry) -> Result<u64, ReplicationError> {
        let mut map = self
            .entries
            .lock()
            .map_err(|e| ReplicationError::Backend(e.to_string()))?;

        let key = Self::key(&entry.grain_type, &entry.grain_key);
        let log = map.entry(key).or_default();

        // Validate monotonic sequence
        if let Some(last) = log.last() {
            if entry.sequence <= last.sequence {
                return Err(ReplicationError::SequenceConflict {
                    expected: last.sequence + 1,
                    actual: entry.sequence,
                });
            }
        }

        let seq = entry.sequence;
        log.push(entry);
        Ok(seq)
    }

    async fn read_from(
        &self,
        grain_type: &str,
        grain_key: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<ReplicationEntry>, ReplicationError> {
        let map = self
            .entries
            .lock()
            .map_err(|e| ReplicationError::Backend(e.to_string()))?;

        let key = Self::key(grain_type, grain_key);
        let Some(log) = map.get(&key) else {
            return Ok(Vec::new());
        };

        let result: Vec<ReplicationEntry> = log
            .iter()
            .filter(|e| e.sequence > after_sequence)
            .take(limit)
            .cloned()
            .collect();

        Ok(result)
    }

    async fn latest_sequence(
        &self,
        grain_type: &str,
        grain_key: &str,
    ) -> Result<u64, ReplicationError> {
        let map = self
            .entries
            .lock()
            .map_err(|e| ReplicationError::Backend(e.to_string()))?;

        let key = Self::key(grain_type, grain_key);
        Ok(map
            .get(&key)
            .and_then(|log| log.last())
            .map(|e| e.sequence)
            .unwrap_or(0))
    }

    async fn truncate(
        &self,
        grain_type: &str,
        grain_key: &str,
        before_sequence: u64,
    ) -> Result<u64, ReplicationError> {
        let mut map = self
            .entries
            .lock()
            .map_err(|e| ReplicationError::Backend(e.to_string()))?;

        let key = Self::key(grain_type, grain_key);
        let Some(log) = map.get_mut(&key) else {
            return Ok(0);
        };

        let before_len = log.len();
        log.retain(|e| e.sequence >= before_sequence);
        Ok((before_len - log.len()) as u64)
    }
}
