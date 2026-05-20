use async_trait::async_trait;
use orlando_core::GrainId;
use thiserror::Error;

/// Opaque version tag for optimistic concurrency control.
/// Each successful write produces a new ETag; a conditional write
/// with a stale ETag is rejected with `PersistenceError::EtagMismatch`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ETag(pub String);

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

    #[error("Postgres error: {0}")]
    Postgres(sqlx::Error),

    #[error("schema migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("etag mismatch: expected {expected:?}, found {actual:?}")]
    EtagMismatch {
        expected: Option<ETag>,
        actual: Option<ETag>,
    },
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

    /// Load state bytes together with the current ETag.
    /// Returns `None` if no state exists for the given grain.
    async fn load_with_etag(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(Vec<u8>, ETag)>, PersistenceError>;

    /// Conditionally save state bytes. If `expected_etag` is `Some`, the write
    /// succeeds only when the stored ETag matches; if `None`, the write succeeds
    /// only when no prior state exists. Returns the new ETag on success.
    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        data: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<ETag, PersistenceError>;
}
