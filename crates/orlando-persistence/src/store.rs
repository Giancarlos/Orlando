use async_trait::async_trait;
use orlando_core::GrainId;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersistenceError {
    #[error("serialization failed: {0}")]
    Serialization(String),

    #[error("deserialization failed: {0}")]
    Deserialization(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] sqlx::Error),
}

/// Controls when grain state is persisted to the store.
#[derive(Debug, Clone)]
pub enum PersistenceStrategy {
    /// Save state only when the grain deactivates (default).
    /// Lowest overhead, but state changes are lost if the silo crashes.
    WriteOnDeactivate,
    /// Save state after every message handler completes.
    /// Highest durability, but adds I/O latency to every message.
    WriteThrough,
    /// Save state periodically at the given interval, plus on deactivation.
    /// Balances durability and performance — at most `interval` of work is lost on crash.
    /// Note: in reentrant grains, this falls back to WriteOnDeactivate behavior.
    WriteBack(std::time::Duration),
}

impl Default for PersistenceStrategy {
    fn default() -> Self {
        Self::WriteOnDeactivate
    }
}

/// Pluggable backend for grain state persistence.
/// Implementations store and retrieve raw bytes keyed by GrainId.
#[async_trait]
pub trait StateStore: Send + Sync + 'static {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError>;
    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError>;
    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError>;
}
