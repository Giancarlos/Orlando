use thiserror::Error;

/// Errors returned when sending a message to a grain via [`crate::GrainRef`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GrainError {
    /// The grain's mailbox was closed (the activation's task ended) before the
    /// message could be delivered or its reply received.
    #[error("grain mailbox closed")]
    MailboxClosed,

    /// The reply could not be downcast to the message's declared result type.
    #[error("reply type mismatch")]
    ReplyTypeMismatch,

    /// A cross-silo call failed at the transport layer (connection/encoding).
    #[error("remote call failed: {0}")]
    RemoteCallFailed(String),

    /// The grain handler returned an application-level failure.
    #[error("handler failed: {0}")]
    HandlerFailed(String),

    /// No reply arrived within the call's timeout.
    #[error("grain call timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// The mailbox is at capacity; the send was rejected (backpressure).
    #[error("grain mailbox full — backpressure")]
    MailboxFull,

    /// The silo is at its activation limit and cannot activate the grain.
    #[error("silo activation limit exceeded")]
    SiloOverloaded,

    /// A circular grain-call chain was detected; the call cycle is reported.
    #[error("deadlock detected: grain call cycle {0}")]
    DeadlockDetected(String),
}
