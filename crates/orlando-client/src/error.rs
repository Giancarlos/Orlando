use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClientError {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("no silo available for grain")]
    NoSiloAvailable,

    #[error("grain error: {0}")]
    GrainError(String),

    #[error("serialization failed: {0}")]
    Serialization(String),

    #[error("deserialization failed: {0}")]
    Deserialization(String),
}
