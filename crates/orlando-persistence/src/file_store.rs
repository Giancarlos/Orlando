use std::path::PathBuf;

use async_trait::async_trait;
use orlando_core::GrainId;

use crate::store::{PersistenceError, StateStore};

/// File-based state store for development.
/// Each grain's state is stored as a separate file under the base directory.
/// Layout: `<base_dir>/<sanitized_type_name>/<key>.bin`
#[derive(Debug)]
pub struct FileStateStore {
    base_dir: PathBuf,
}

impl FileStateStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    fn path_for(&self, grain_id: &GrainId) -> PathBuf {
        let type_dir = grain_id.type_name.replace("::", "__");
        let file_name = format!("{}.bin", grain_id.key);
        self.base_dir.join(type_dir).join(file_name)
    }
}

#[async_trait]
impl StateStore for FileStateStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        let path = self.path_for(grain_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError> {
        let path = self.path_for(grain_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, data).await?;
        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        let path = self.path_for(grain_id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }
}
