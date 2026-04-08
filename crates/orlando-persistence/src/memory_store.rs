use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use orlando_core::GrainId;

use crate::store::{ETag, PersistenceError, StateStore};

/// Entry stored per grain: the raw bytes and a monotonic version number.
#[derive(Debug, Clone)]
struct Entry {
    bytes: Vec<u8>,
    version: u64,
}

/// In-memory state store for testing.
/// Data is lost when the store is dropped.
/// Supports optimistic concurrency via monotonically increasing version ETags.
#[derive(Debug)]
pub struct InMemoryStateStore {
    data: Mutex<HashMap<GrainId, Entry>>,
    next_version: AtomicU64,
}

impl Default for InMemoryStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStateStore {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
            next_version: AtomicU64::new(1),
        }
    }

    fn alloc_version(&self) -> u64 {
        self.next_version.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl StateStore for InMemoryStateStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        let data = self.data.lock().unwrap();
        Ok(data.get(grain_id).map(|e| e.bytes.clone()))
    }

    async fn save(&self, grain_id: &GrainId, bytes: &[u8]) -> Result<(), PersistenceError> {
        let version = self.alloc_version();
        let mut data = self.data.lock().unwrap();
        data.insert(grain_id.clone(), Entry { bytes: bytes.to_vec(), version });
        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        let mut data = self.data.lock().unwrap();
        data.remove(grain_id);
        Ok(())
    }

    async fn load_with_etag(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(Vec<u8>, Option<ETag>)>, PersistenceError> {
        let data = self.data.lock().unwrap();
        Ok(data.get(grain_id).map(|e| {
            (e.bytes.clone(), Some(ETag(e.version.to_string())))
        }))
    }

    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        bytes: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<Option<ETag>, PersistenceError> {
        let new_version = self.alloc_version();
        let mut data = self.data.lock().unwrap();

        if let Some(expected) = expected_etag {
            let current_version = data.get(grain_id).map(|e| e.version.to_string());
            let actual = current_version.unwrap_or_default();
            if actual != expected.0 {
                return Err(PersistenceError::EtagMismatch {
                    expected: expected.0.clone(),
                    actual,
                });
            }
        }

        data.insert(grain_id.clone(), Entry { bytes: bytes.to_vec(), version: new_version });
        Ok(Some(ETag(new_version.to_string())))
    }
}
