use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GrainError {
    #[error("grain mailbox closed")]
    MailboxClosed,

    #[error("reply type mismatch")]
    ReplyTypeMismatch,

    #[error("remote call failed: {0}")]
    RemoteCallFailed(String),

    #[error("handler failed: {0}")]
    HandlerFailed(String),

    #[error("grain call timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error("grain mailbox full — backpressure")]
    MailboxFull,

    #[error("silo activation limit exceeded")]
    SiloOverloaded,

    #[error("deadlock detected: grain call cycle {0}")]
    DeadlockDetected(String),
}
