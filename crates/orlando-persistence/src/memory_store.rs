use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use orlando_core::GrainId;

use crate::store::{ETag, PersistenceError, StateStore};

/// In-memory state store for testing.
/// Data is lost when the store is dropped.
#[derive(Debug)]
pub struct InMemoryStateStore {
    /// Stores (bytes, etag_string) per grain.
    data: Mutex<HashMap<GrainId, (Vec<u8>, String)>>,
    /// Monotonic counter for generating unique ETags.
    version: AtomicU64,
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
            version: AtomicU64::new(1),
        }
    }

    fn next_etag(&self) -> String {
        self.version.fetch_add(1, Ordering::Relaxed).to_string()
    }
}

#[async_trait]
impl StateStore for InMemoryStateStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        let data = self.data.lock().unwrap();
        Ok(data.get(grain_id).map(|(bytes, _)| bytes.clone()))
    }

    async fn save(&self, grain_id: &GrainId, bytes: &[u8]) -> Result<(), PersistenceError> {
        let etag = self.next_etag();
        let mut data = self.data.lock().unwrap();
        data.insert(grain_id.clone(), (bytes.to_vec(), etag));
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
    ) -> Result<Option<(Vec<u8>, ETag)>, PersistenceError> {
        let data = self.data.lock().unwrap();
        Ok(data
            .get(grain_id)
            .map(|(bytes, etag)| (bytes.clone(), ETag(etag.clone()))))
    }

    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        bytes: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<ETag, PersistenceError> {
        let new_etag_str = self.next_etag();
        let mut data = self.data.lock().unwrap();

        let current = data.get(grain_id);
        match (expected_etag, current) {
            // Expecting no prior state, and none exists — insert.
            (None, None) => {}
            // Expecting no prior state, but state exists — conflict.
            (None, Some((_, actual_etag))) => {
                return Err(PersistenceError::EtagMismatch {
                    expected: None,
                    actual: Some(ETag(actual_etag.clone())),
                });
            }
            // Expecting a specific etag, but no state exists — conflict.
            (Some(expected), None) => {
                return Err(PersistenceError::EtagMismatch {
                    expected: Some(expected.clone()),
                    actual: None,
                });
            }
            // Expecting a specific etag — verify it matches.
            (Some(expected), Some((_, actual_etag))) => {
                if expected.0 != *actual_etag {
                    return Err(PersistenceError::EtagMismatch {
                        expected: Some(expected.clone()),
                        actual: Some(ETag(actual_etag.clone())),
                    });
                }
            }
        }

        let new_etag = ETag(new_etag_str);
        data.insert(grain_id.clone(), (bytes.to_vec(), new_etag.0.clone()));
        Ok(new_etag)
    }
}
