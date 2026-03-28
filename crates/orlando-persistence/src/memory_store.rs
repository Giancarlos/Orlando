use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use orlando_core::GrainId;

use crate::store::{PersistenceError, StateStore};

/// In-memory state store for testing.
/// Data is lost when the store is dropped.
#[derive(Debug)]
pub struct InMemoryStateStore {
    data: Mutex<HashMap<GrainId, Vec<u8>>>,
}

impl InMemoryStateStore {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl StateStore for InMemoryStateStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        let data = self.data.lock().unwrap();
        Ok(data.get(grain_id).cloned())
    }

    async fn save(&self, grain_id: &GrainId, bytes: &[u8]) -> Result<(), PersistenceError> {
        let mut data = self.data.lock().unwrap();
        data.insert(grain_id.clone(), bytes.to_vec());
        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        let mut data = self.data.lock().unwrap();
        data.remove(grain_id);
        Ok(())
    }
}
