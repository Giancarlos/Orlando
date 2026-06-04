/// A message a grain can handle. Each message declares the reply type it
/// produces via [`Result`](Message::Result).
pub trait Message: Send + 'static {
    /// The reply type returned by the handler for this message.
    type Result: Send + 'static;
}

/// Marker trait for messages that only read grain state (no mutations).
///
/// On secondary clusters, read-only messages can be served from the local
/// replica if the replica's staleness is within `ReplicatedGrain::max_staleness()`.
/// Write messages are always forwarded to the primary.
pub trait ReadOnlyMessage: Message {}
