use async_trait::async_trait;
use orlando_core::GrainId;
use sqlx::PgPool;

use crate::store::{ETag, PersistenceError, StateStore};

fn pg(e: sqlx::Error) -> PersistenceError {
    PersistenceError::Postgres(e)
}

/// PostgreSQL-backed state store for production grain persistence.
/// Stores grain state as binary in a single table keyed by (type_name, key),
/// with an integer version column used as the ETag for optimistic concurrency.
///
/// Enable with the `postgres` feature flag on `orlando-persistence`.
#[derive(Debug, Clone)]
pub struct PostgresStateStore {
    pool: PgPool,
}

impl PostgresStateStore {
    /// Create a new store and ensure the schema exists.
    /// `url` is a Postgres connection string, e.g. `"postgres://user:pass@localhost/orlando"`.
    pub async fn new(url: &str) -> Result<Self, PersistenceError> {
        let pool = crate::backoff::retry_store_init("postgres_connect", || async {
            PgPool::connect(url).await
        })
        .await
        .map_err(pg)?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS grain_state (
                type_name TEXT NOT NULL,
                key       TEXT NOT NULL,
                data      BYTEA NOT NULL,
                version   BIGINT NOT NULL DEFAULT 1,
                PRIMARY KEY (type_name, key)
            )",
        )
        .execute(&pool)
        .await
        .map_err(pg)?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl StateStore for PostgresStateStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT data FROM grain_state WHERE type_name = $1 AND key = $2",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .fetch_optional(&self.pool)
        .await
        .map_err(pg)?;

        Ok(row.map(|(data,)| data))
    }

    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError> {
        sqlx::query(
            "INSERT INTO grain_state (type_name, key, data, version) VALUES ($1, $2, $3, 1)
             ON CONFLICT (type_name, key) DO UPDATE SET data = EXCLUDED.data, version = grain_state.version + 1",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(pg)?;

        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM grain_state WHERE type_name = $1 AND key = $2")
            .bind(grain_id.type_name)
            .bind(&grain_id.key)
            .execute(&self.pool)
            .await
            .map_err(pg)?;

        Ok(())
    }

    async fn load_with_etag(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(Vec<u8>, ETag)>, PersistenceError> {
        let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT data, version FROM grain_state WHERE type_name = $1 AND key = $2",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .fetch_optional(&self.pool)
        .await
        .map_err(pg)?;

        Ok(row.map(|(data, version)| (data, ETag(version.to_string()))))
    }

    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        data: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<ETag, PersistenceError> {
        match expected_etag {
            None => {
                // Expect no existing row — plain INSERT, fails on unique violation.
                let result = sqlx::query(
                    "INSERT INTO grain_state (type_name, key, data, version) VALUES ($1, $2, $3, 1)",
                )
                .bind(grain_id.type_name)
                .bind(&grain_id.key)
                .bind(data)
                .execute(&self.pool)
                .await;

                match result {
                    Ok(_) => Ok(ETag("1".to_string())),
                    Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
                        let current = self.load_with_etag(grain_id).await?;
                        Err(PersistenceError::EtagMismatch {
                            expected: None,
                            actual: current.map(|(_, etag)| etag),
                        })
                    }
                    Err(e) => Err(PersistenceError::Postgres(e)),
                }
            }
            Some(expected) => {
                let expected_version: i64 = expected
                    .0
                    .parse()
                    .map_err(|e: std::num::ParseIntError| {
                        PersistenceError::Serialization(format!("invalid etag: {e}"))
                    })?;
                let new_version = expected_version + 1;

                let result = sqlx::query(
                    "UPDATE grain_state
                     SET data = $1, version = $2
                     WHERE type_name = $3 AND key = $4 AND version = $5",
                )
                .bind(data)
                .bind(new_version)
                .bind(grain_id.type_name)
                .bind(&grain_id.key)
                .bind(expected_version)
                .execute(&self.pool)
                .await
                .map_err(pg)?;

                if result.rows_affected() == 0 {
                    let current = self.load_with_etag(grain_id).await?;
                    Err(PersistenceError::EtagMismatch {
                        expected: Some(expected.clone()),
                        actual: current.map(|(_, etag)| etag),
                    })
                } else {
                    Ok(ETag(new_version.to_string()))
                }
            }
        }
    }
}
