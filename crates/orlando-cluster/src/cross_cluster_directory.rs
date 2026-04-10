use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use orlando_core::{ClusterId, GrainId};

/// Tracks which cluster owns each grain activation globally.
/// Implementations must provide CAS semantics: only one cluster can own
/// a grain at a time.
#[async_trait]
pub trait CrossClusterDirectory: Send + Sync + 'static {
    /// Look up which cluster owns a grain.
    async fn lookup(&self, grain_id: &GrainId) -> Result<Option<GrainOwnership>, DirectoryError>;

    /// Register ownership. Returns the actual owner -- if another cluster
    /// already registered, returns their ownership (first-writer-wins CAS).
    async fn register(
        &self,
        grain_id: &GrainId,
        cluster_id: &ClusterId,
    ) -> Result<GrainOwnership, DirectoryError>;

    /// Release ownership.
    async fn deregister(
        &self,
        grain_id: &GrainId,
        cluster_id: &ClusterId,
    ) -> Result<(), DirectoryError>;
}

/// Who owns a grain activation.
#[derive(Debug, Clone)]
pub struct GrainOwnership {
    pub cluster_id: ClusterId,
    pub registered_at: SystemTime,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DirectoryError {
    #[error("directory backend unavailable: {0}")]
    Unavailable(String),
    #[error("directory backend error: {0}")]
    Backend(String),
}

/// In-memory implementation for testing and single-process multi-cluster setups.
#[derive(Debug, Default)]
pub struct InMemoryCrossClusterDirectory {
    entries: Mutex<HashMap<String, GrainOwnership>>,
}

impl InMemoryCrossClusterDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(grain_id: &GrainId) -> String {
        format!("{}/{}", grain_id.type_name, grain_id.key)
    }
}

#[async_trait]
impl CrossClusterDirectory for InMemoryCrossClusterDirectory {
    async fn lookup(&self, grain_id: &GrainId) -> Result<Option<GrainOwnership>, DirectoryError> {
        let entries = self
            .entries
            .lock()
            .map_err(|e| DirectoryError::Backend(e.to_string()))?;
        Ok(entries.get(&Self::key(grain_id)).cloned())
    }

    async fn register(
        &self,
        grain_id: &GrainId,
        cluster_id: &ClusterId,
    ) -> Result<GrainOwnership, DirectoryError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|e| DirectoryError::Backend(e.to_string()))?;
        let key = Self::key(grain_id);

        // CAS: first writer wins
        if let Some(existing) = entries.get(&key) {
            return Ok(existing.clone());
        }

        let ownership = GrainOwnership {
            cluster_id: cluster_id.clone(),
            registered_at: SystemTime::now(),
        };
        entries.insert(key, ownership.clone());
        Ok(ownership)
    }

    async fn deregister(
        &self,
        grain_id: &GrainId,
        cluster_id: &ClusterId,
    ) -> Result<(), DirectoryError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|e| DirectoryError::Backend(e.to_string()))?;
        let key = Self::key(grain_id);

        // Only the owning cluster can deregister
        if let Some(existing) = entries.get(&key)
            && existing.cluster_id == *cluster_id
        {
            entries.remove(&key);
        }
        Ok(())
    }
}
