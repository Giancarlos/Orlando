use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClusterError {
    #[error("no silo available for grain")]
    NoSiloAvailable,

    #[error("transport error: {0}")]
    Transport(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("unknown grain type: {0}")]
    UnknownGrainType(String),

    #[error("unknown message type: {0}")]
    UnknownMessageType(String),

    #[error("handler error: {0}")]
    HandlerError(String),

    #[error("unsupported encoding for message {0}: {1}")]
    UnsupportedEncoding(String, String),
}
