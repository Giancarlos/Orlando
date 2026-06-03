//! Types for cross-cluster state replication.
//!
//! The primary cluster produces `ReplicationEntry` values after each handler
//! invocation. Secondary clusters consume them to maintain read-only replicas.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cluster_id::ClusterId;

/// A single replication log entry carrying a grain state snapshot or delta.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicationEntry {
    /// The grain this entry belongs to.
    pub grain_type: String,
    pub grain_key: String,
    /// Monotonically increasing sequence number per grain.
    pub sequence: u64,
    /// When the entry was created (millis since UNIX epoch).
    pub timestamp_millis: i64,
    /// Cluster that produced this entry.
    pub source_cluster: ClusterId,
    /// Whether this is a full snapshot or incremental delta.
    pub entry_type: ReplicationEntryType,
    /// Serialized grain state (format matches the grain's serializer).
    pub payload: Vec<u8>,
}

/// What kind of state the entry carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplicationEntryType {
    /// Complete state snapshot — replaces replica state entirely.
    FullState,
    /// Incremental change (future optimization, not used in v1).
    Delta,
}

/// How often a primary ships state to secondaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplicationMode {
    /// Replicate after every handler invocation.
    Immediate,
    /// Batch replication entries at a fixed interval.
    Batched { interval: Duration },
}
