use async_trait::async_trait;
use fred::prelude::*;
use orlando_core::GrainId;

use crate::store::{ETag, PersistenceError, StateStore};

/// Redis-backed state store for production grain persistence.
///
/// Uses a Redis hash per grain with `data` and `version` fields.
/// Key format: `grain:{type_name}:{key}`.
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
        let data: Option<Vec<u8>> = self.client.hget(&key, "data").await.map_err(redis_err)?;
        Ok(data)
    }

    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError> {
        let key = Self::key_for(grain_id);
        // Atomically set data and increment version
        let script = r#"
            redis.call('HSET', KEYS[1], 'data', ARGV[1])
            redis.call('HINCRBY', KEYS[1], 'version', 1)
            return 1
        "#;
        self.client
            .eval::<i64, _, _, _>(script, vec![&key], vec![data.to_vec()])
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        let key = Self::key_for(grain_id);
        self.client.del::<(), _>(&key).await.map_err(redis_err)?;
        Ok(())
    }

    async fn load_with_etag(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(Vec<u8>, ETag)>, PersistenceError> {
        let key = Self::key_for(grain_id);
        let result: Vec<Option<Vec<u8>>> = self
            .client
            .hmget(&key, vec!["data", "version"])
            .await
            .map_err(redis_err)?;

        match (result.first().and_then(|v| v.clone()), result.get(1).and_then(|v| v.clone())) {
            (Some(data), Some(version_bytes)) => {
                let version = String::from_utf8(version_bytes)
                    .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
                Ok(Some((data, ETag(version))))
            }
            _ => Ok(None),
        }
    }

    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        data: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<ETag, PersistenceError> {
        let key = Self::key_for(grain_id);

        match expected_etag {
            None => {
                // Expect no existing key — use HSETNX-style check via Lua
                let script = r#"
                    if redis.call('EXISTS', KEYS[1]) == 1 then
                        return redis.call('HGET', KEYS[1], 'version')
                    end
                    redis.call('HSET', KEYS[1], 'data', ARGV[1], 'version', '1')
                    return nil
                "#;
                let result: Option<String> = self
                    .client
                    .eval(script, vec![&key], vec![data.to_vec()])
                    .await
                    .map_err(redis_err)?;

                match result {
                    None => Ok(ETag("1".to_string())),
                    Some(actual_version) => Err(PersistenceError::EtagMismatch {
                        expected: None,
                        actual: Some(ETag(actual_version)),
                    }),
                }
            }
            Some(expected) => {
                let expected_version = &expected.0;
                let new_version: i64 = expected_version
                    .parse::<i64>()
                    .map_err(|e| PersistenceError::Serialization(format!("invalid etag: {e}")))?
                    + 1;
                let new_version_str = new_version.to_string();

                // Atomic compare-and-swap via Lua
                let script = r#"
                    local current = redis.call('HGET', KEYS[1], 'version')
                    if current == false then
                        return {'MISSING', ''}
                    end
                    if current ~= ARGV[2] then
                        return {'MISMATCH', current}
                    end
                    redis.call('HSET', KEYS[1], 'data', ARGV[1], 'version', ARGV[3])
                    return {'OK', ARGV[3]}
                "#;
                let result: Vec<String> = self
                    .client
                    .eval(
                        script,
                        vec![&key],
                        vec![
                            data.to_vec(),
                            expected_version.as_bytes().to_vec(),
                            new_version_str.as_bytes().to_vec(),
                        ],
                    )
                    .await
                    .map_err(redis_err)?;

                match result.first().map(|s| s.as_str()) {
                    Some("OK") => Ok(ETag(new_version_str)),
                    Some("MISSING") => Err(PersistenceError::EtagMismatch {
                        expected: Some(expected.clone()),
                        actual: None,
                    }),
                    Some("MISMATCH") => Err(PersistenceError::EtagMismatch {
                        expected: Some(expected.clone()),
                        actual: result.get(1).map(|v| ETag(v.clone())),
                    }),
                    _ => Err(PersistenceError::Io(std::io::Error::other(
                        "unexpected Lua script result",
                    ))),
                }
            }
        }
    }
}
