use thiserror::Error;

#[derive(Debug, Error)]
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
}
