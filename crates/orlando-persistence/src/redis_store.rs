use async_trait::async_trait;
use fred::prelude::*;
use orlando_core::GrainId;

use crate::store::{PersistenceError, StateStore};

/// Redis-backed state store for production grain persistence.
///
/// Stores grain state as a binary blob in Redis, keyed by `grain:{type_name}:{key}`.
/// Enable with the `redis` feature flag.
#[derive(Clone)]
pub struct RedisStateStore {
    client: Client,
}

impl RedisStateStore {
    /// Connect to Redis. `url` is a Redis connection string, e.g. `"redis://localhost:6379"`.
    pub async fn new(url: &str) -> Result<Self, PersistenceError> {
        let config = Config::from_url(url).map_err(redis_err)?;
        let client = Builder::from_config(config).build().map_err(redis_err)?;
        client.init().await.map_err(redis_err)?;
        Ok(Self { client })
    }

    fn key_for(grain_id: &GrainId) -> String {
        format!("grain:{}:{}", grain_id.type_name, grain_id.key)
    }
}

impl std::fmt::Debug for RedisStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisStateStore").finish()
    }
}

fn redis_err(e: impl std::fmt::Display) -> PersistenceError {
    PersistenceError::Io(std::io::Error::other(e.to_string()))
}

#[async_trait]
impl StateStore for RedisStateStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        let key = Self::key_for(grain_id);
        let data: Option<Vec<u8>> = self.client.get(&key).await.map_err(redis_err)?;
        Ok(data)
    }

    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError> {
        let key = Self::key_for(grain_id);
        self.client
            .set::<(), _, _>(&key, data.to_vec(), None, None, false)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        let key = Self::key_for(grain_id);
        self.client.del::<(), _>(&key).await.map_err(redis_err)?;
        Ok(())
    }
}
