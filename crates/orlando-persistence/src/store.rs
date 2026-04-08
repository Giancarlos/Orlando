use async_trait::async_trait;
use orlando_core::GrainId;
use thiserror::Error;

/// An opaque version tag for optimistic concurrency control.
/// Stores use this to detect concurrent writes to the same grain state.
#[derive(Debug, Clone, PartialEq, Eq)]
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

    #[error("etag mismatch: expected {expected}, got {actual}")]
    EtagMismatch { expected: String, actual: String },
}

/// Pluggable backend for grain state persistence.
/// Implementations store and retrieve raw bytes keyed by GrainId.
///
/// The `load_with_etag` and `save_with_etag` methods provide optimistic concurrency
/// control. Their default implementations delegate to the basic `load`/`save` methods
/// without etag checking, so existing stores continue to work unchanged.
#[async_trait]
pub trait StateStore: Send + Sync + 'static {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError>;
    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError>;
    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError>;

    /// Load state together with its ETag for optimistic concurrency.
    /// Default: delegates to `load()` and returns `None` as the etag.
    async fn load_with_etag(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(Vec<u8>, Option<ETag>)>, PersistenceError> {
        Ok(self.load(grain_id).await?.map(|data| (data, None)))
    }

    /// Save state with an ETag check. If `expected_etag` is `Some` and doesn't match
    /// the current stored etag, returns `EtagMismatch`.
    /// Returns the new ETag on success, or `None` if the store doesn't track etags.
    /// Default: delegates to `save()` ignoring the etag.
    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        data: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<Option<ETag>, PersistenceError> {
        let _ = expected_etag;
        self.save(grain_id, data).await?;
        Ok(None)
    }
}
