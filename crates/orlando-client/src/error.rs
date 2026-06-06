use thiserror::Error;

/// Errors returned by the external client when calling grains.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClientError {
    /// Failed to establish a connection to a silo.
    #[error("connection failed: {0}")]
    Connection(String),

    /// A transport-level (gRPC) error occurred during the call.
    #[error("transport error: {0}")]
    Transport(String),

    /// No silo could be resolved to host the target grain.
    #[error("no silo available for grain")]
    NoSiloAvailable,

    /// The grain returned an error.
    #[error("grain error: {0}")]
    GrainError(String),

    /// The request payload could not be serialized.
    #[error("serialization failed: {0}")]
    Serialization(String),

    /// The response payload could not be deserialized.
    #[error("deserialization failed: {0}")]
    Deserialization(String),
}
