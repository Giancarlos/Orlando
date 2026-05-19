pub trait Message: Send + 'static {
    type Result: Send + 'static;
}

/// Marker trait for messages that only read grain state (no mutations).
///
/// On secondary clusters, read-only messages can be served from the local
/// replica if the replica's staleness is within `ReplicatedGrain::max_staleness()`.
/// Write messages are always forwarded to the primary.
pub trait ReadOnlyMessage: Message {}
