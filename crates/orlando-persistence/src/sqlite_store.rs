use async_trait::async_trait;
use sqlx::SqlitePool;
use orlando_core::GrainId;

use crate::store::{ETag, PersistenceError, StateStore};

/// SQLite-backed state store for durable grain persistence.
/// Stores grain state as a binary blob in a single table keyed by (type_name, key),
/// with an integer version column used as the ETag for optimistic concurrency.
#[derive(Debug, Clone)]
pub struct SqliteStateStore {
    pool: SqlitePool,
}

impl SqliteStateStore {
    /// Create a new SqliteStateStore and ensure the schema exists.
    /// `url` is a SQLite connection string, e.g. `"sqlite://grains.db"` or `"sqlite::memory:"`.
    pub async fn new(url: &str) -> Result<Self, PersistenceError> {
        let pool = crate::backoff::retry_store_init("sqlite_connect", || async {
            SqlitePool::connect(url).await
        })
        .await?;

        sqlx::migrate!("migrations/sqlite").run(&pool).await?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl StateStore for SqliteStateStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT data FROM grain_state WHERE type_name = ? AND key = ?",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(data,)| data))
    }

    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError> {
        sqlx::query(
            "INSERT INTO grain_state (type_name, key, data, version) VALUES (?, ?, ?, 1)
             ON CONFLICT (type_name, key) DO UPDATE SET data = excluded.data, version = version + 1",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .bind(data)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        sqlx::query(
            "DELETE FROM grain_state WHERE type_name = ? AND key = ?",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn load_with_etag(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(Vec<u8>, ETag)>, PersistenceError> {
        let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT data, version FROM grain_state WHERE type_name = ? AND key = ?",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .fetch_optional(&self.pool)
        .await?;

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
                // Expect no existing row. Use INSERT without ON CONFLICT so it
                // fails if the row already exists.
                let result = sqlx::query(
                    "INSERT INTO grain_state (type_name, key, data, version) VALUES (?, ?, ?, 1)",
                )
                .bind(grain_id.type_name)
                .bind(&grain_id.key)
                .bind(data)
                .execute(&self.pool)
                .await;

                match result {
                    Ok(_) => Ok(ETag("1".to_string())),
                    Err(sqlx::Error::Database(ref db_err))
                        if db_err.message().contains("UNIQUE constraint failed") =>
                    {
                        // Row exists; fetch its current etag for the error.
                        let current = self.load_with_etag(grain_id).await?;
                        Err(PersistenceError::EtagMismatch {
                            expected: None,
                            actual: current.map(|(_, etag)| etag),
                        })
                    }
                    Err(e) => Err(PersistenceError::Sqlite(e)),
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
                     SET data = ?, version = ?
                     WHERE type_name = ? AND key = ? AND version = ?",
                )
                .bind(data)
                .bind(new_version)
                .bind(grain_id.type_name)
                .bind(&grain_id.key)
                .bind(expected_version)
                .execute(&self.pool)
                .await?;

                if result.rows_affected() == 0 {
                    // Either the row doesn't exist or the version didn't match.
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
