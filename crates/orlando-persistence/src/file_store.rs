use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use orlando_core::GrainId;

use crate::store::{ETag, PersistenceError, StateStore};

/// File-based state store for development.
/// Each grain's state is stored as a separate file under the base directory.
/// Layout: `<base_dir>/<sanitized_type_name>/<key>.bin`
/// ETags are stored in a companion `<key>.etag` file alongside the data.
#[derive(Debug)]
pub struct FileStateStore {
    base_dir: PathBuf,
    version: AtomicU64,
}

impl FileStateStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            version: AtomicU64::new(1),
        }
    }

    fn path_for(&self, grain_id: &GrainId) -> PathBuf {
        let type_dir = grain_id.type_name.replace("::", "__");
        let file_name = format!("{}.bin", grain_id.key);
        self.base_dir.join(type_dir).join(file_name)
    }

    fn etag_path_for(&self, grain_id: &GrainId) -> PathBuf {
        let type_dir = grain_id.type_name.replace("::", "__");
        let file_name = format!("{}.etag", grain_id.key);
        self.base_dir.join(type_dir).join(file_name)
    }

    fn next_etag(&self) -> String {
        self.version.fetch_add(1, Ordering::Relaxed).to_string()
    }

    async fn read_etag(&self, grain_id: &GrainId) -> Result<Option<String>, PersistenceError> {
        let path = self.etag_path_for(grain_id);
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    async fn write_etag(
        &self,
        grain_id: &GrainId,
        etag: &str,
    ) -> Result<(), PersistenceError> {
        let path = self.etag_path_for(grain_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, etag).await?;
        Ok(())
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
        // Also write a new etag for consistency.
        let etag = self.next_etag();
        self.write_etag(grain_id, &etag).await?;
        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        let path = self.path_for(grain_id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(PersistenceError::Io(e)),
        }
        // Also remove etag file.
        let etag_path = self.etag_path_for(grain_id);
        match tokio::fs::remove_file(&etag_path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PersistenceError::Io(e)),
        }
    }

    async fn load_with_etag(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(Vec<u8>, ETag)>, PersistenceError> {
        let path = self.path_for(grain_id);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(PersistenceError::Io(e)),
        };
        // If data exists but etag is missing, generate one and persist it.
        let etag = match self.read_etag(grain_id).await? {
            Some(e) => e,
            None => {
                let e = self.next_etag();
                self.write_etag(grain_id, &e).await?;
                e
            }
        };
        Ok(Some((bytes, ETag(etag))))
    }

    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        data: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<ETag, PersistenceError> {
        let current_etag = self.read_etag(grain_id).await?;

        match (expected_etag, &current_etag) {
            (None, None) => {}
            (None, Some(actual)) => {
                return Err(PersistenceError::EtagMismatch {
                    expected: None,
                    actual: Some(ETag(actual.clone())),
                });
            }
            (Some(expected), None) => {
                return Err(PersistenceError::EtagMismatch {
                    expected: Some(expected.clone()),
                    actual: None,
                });
            }
            (Some(expected), Some(actual)) => {
                if expected.0 != *actual {
                    return Err(PersistenceError::EtagMismatch {
                        expected: Some(expected.clone()),
                        actual: Some(ETag(actual.clone())),
                    });
                }
            }
        }

        let path = self.path_for(grain_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, data).await?;

        let new_etag = self.next_etag();
        self.write_etag(grain_id, &new_etag).await?;
        Ok(ETag(new_etag))
    }
}
