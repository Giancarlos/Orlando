use thiserror::Error;

#[derive(Debug, Error)]
pub enum GrainError {
    #[error("grain mailbox closed")]
    MailboxClosed,

    #[error("reply type mismatch")]
    ReplyTypeMismatch,

    #[error("remote call failed: {0}")]
    RemoteCallFailed(String),
}
