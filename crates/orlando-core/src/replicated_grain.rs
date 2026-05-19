//! Trait for grains with state replicated across clusters.
//!
//! Primary cluster handles all writes. Secondaries maintain read-only
//! replicas and can serve stale reads within the staleness window.

use std::time::Duration;

use crate::replication::ReplicationMode;

/// Marker trait for grains that participate in cross-cluster replication.
///
/// The primary cluster runs the authoritative activation. After each handler
/// invocation, the mailbox loop serializes state and sends it to the
/// replication log. Secondary clusters consume the log asynchronously.
///
/// Grains that implement this trait must also implement `PersistentGrain`
/// (state must be serializable for replication).
pub trait ReplicatedGrain: crate::Grain
where
    Self::State: serde::Serialize + serde::de::DeserializeOwned,
{
    /// Maximum acceptable replication lag for local reads on secondaries.
    ///
    /// `Duration::ZERO` = always forward to primary (strong consistency).
    /// `Duration::MAX` = always serve stale (best latency, weakest consistency).
    /// Default: 5 seconds.
    fn max_staleness() -> Duration {
        Duration::from_secs(5)
    }

    /// How state is shipped to secondaries.
    /// Default: after every handler invocation.
    fn replication_mode() -> ReplicationMode {
        ReplicationMode::Immediate
    }

    /// Interval for periodic full-state snapshots, even when no writes occur.
    /// Useful for compacting the replication log.
    /// Default: 60 seconds.
    fn snapshot_interval() -> Duration {
        Duration::from_secs(60)
    }
}
