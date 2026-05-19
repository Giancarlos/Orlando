use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use orlando_core::{ClusterId, GrainId};

/// Tracks which cluster owns each grain activation globally.
/// Implementations must provide CAS semantics: only one cluster can own
/// a grain at a time. Concurrent registrations are resolved by first-writer-wins.
#[async_trait]
pub trait CrossClusterDirectory: Send + Sync + 'static {
    /// Look up which cluster owns a grain.
    async fn lookup(&self, grain_id: &GrainId) -> Result<Option<GrainOwnership>, DirectoryError>;

    /// CAS register. Returns actual owner (may differ from requested if another
    /// cluster won the race). The epoch is a monotonic fencing token — stale
    /// registrations with a lower epoch are rejected.
    async fn register(
        &self,
        grain_id: &GrainId,
        cluster_id: &ClusterId,
        epoch: u64,
    ) -> Result<GrainOwnership, DirectoryError>;

    /// Release ownership. Only the owning cluster can deregister.
    async fn deregister(
        &self,
        grain_id: &GrainId,
        cluster_id: &ClusterId,
    ) -> Result<(), DirectoryError>;

    /// Extend TTL for ownership. No-op for backends without TTL (e.g., PostgreSQL).
    async fn renew(
        &self,
        _grain_id: &GrainId,
        _cluster_id: &ClusterId,
    ) -> Result<(), DirectoryError> {
        Ok(())
    }
}

/// Who owns a grain activation.
#[derive(Debug, Clone)]
pub struct GrainOwnership {
    pub cluster_id: ClusterId,
    pub epoch: u64,
    pub registered_at: SystemTime,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DirectoryError {
    #[error("directory backend unavailable: {0}")]
    Unavailable(String),
    #[error("stale epoch: current is {current}, requested {requested}")]
    StaleEpoch { current: u64, requested: u64 },
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
        epoch: u64,
    ) -> Result<GrainOwnership, DirectoryError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|e| DirectoryError::Backend(e.to_string()))?;
        let key = Self::key(grain_id);

        // CAS: first writer wins, but higher epoch can reclaim
        if let Some(existing) = entries.get(&key) {
            if existing.cluster_id != *cluster_id && epoch <= existing.epoch {
                // Another cluster owns it at a higher or equal epoch
                return Ok(existing.clone());
            }
            if existing.cluster_id == *cluster_id && epoch <= existing.epoch {
                // We already own it at this or higher epoch
                return Ok(existing.clone());
            }
            // Higher epoch — allow re-registration (failover promotion)
        }

        let ownership = GrainOwnership {
            cluster_id: cluster_id.clone(),
            epoch,
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
