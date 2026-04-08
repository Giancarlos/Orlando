use async_trait::async_trait;
use sqlx::SqlitePool;
use orlando_core::GrainId;

use crate::store::{ETag, PersistenceError, StateStore};

/// SQLite-backed state store for durable grain persistence.
/// Stores grain state as a binary blob in a single table keyed by (type_name, key).
/// Supports optimistic concurrency via a `version` column.
#[derive(Debug, Clone)]
pub struct SqliteStateStore {
    pool: SqlitePool,
}

impl SqliteStateStore {
    /// Create a new SqliteStateStore and ensure the schema exists.
    /// `url` is a SQLite connection string, e.g. `"sqlite://grains.db"` or `"sqlite::memory:"`.
    pub async fn new(url: &str) -> Result<Self, PersistenceError> {
        let pool = SqlitePool::connect(url).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS grain_state (
                type_name TEXT NOT NULL,
                key       TEXT NOT NULL,
                data      BLOB NOT NULL,
                version   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (type_name, key)
            )",
        )
        .execute(&pool)
        .await?;

        // Migration for existing databases that lack the version column.
        // ALTER TABLE ADD COLUMN is a no-op error if the column already exists in SQLite,
        // so we ignore the error.
        let _ = sqlx::query(
            "ALTER TABLE grain_state ADD COLUMN version INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&pool)
        .await;

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
            "INSERT INTO grain_state (type_name, key, data, version) VALUES (?, ?, ?, 0)
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
    ) -> Result<Option<(Vec<u8>, Option<ETag>)>, PersistenceError> {
        let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT data, version FROM grain_state WHERE type_name = ? AND key = ?",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(data, version)| (data, Some(ETag(version.to_string())))))
    }

    async fn save_with_etag(
        &self,
        grain_id: &GrainId,
        data: &[u8],
        expected_etag: Option<&ETag>,
    ) -> Result<Option<ETag>, PersistenceError> {
        if let Some(expected) = expected_etag {
            let expected_version: i64 = expected.0.parse().map_err(|_| {
                PersistenceError::EtagMismatch {
                    expected: expected.0.clone(),
                    actual: "unparseable".into(),
                }
            })?;

            // Conditional update: only succeeds if the version matches.
            let result = sqlx::query(
                "UPDATE grain_state SET data = ?, version = version + 1
                 WHERE type_name = ? AND key = ? AND version = ?",
            )
            .bind(data)
            .bind(grain_id.type_name)
            .bind(&grain_id.key)
            .bind(expected_version)
            .execute(&self.pool)
            .await?;

            if result.rows_affected() == 0 {
                // Fetch the actual version for a useful error message.
                let actual: Option<(i64,)> = sqlx::query_as(
                    "SELECT version FROM grain_state WHERE type_name = ? AND key = ?",
                )
                .bind(grain_id.type_name)
                .bind(&grain_id.key)
                .fetch_optional(&self.pool)
                .await?;

                let actual_str = actual
                    .map(|(v,)| v.to_string())
                    .unwrap_or_else(|| "not found".into());

                return Err(PersistenceError::EtagMismatch {
                    expected: expected.0.clone(),
                    actual: actual_str,
                });
            }

            // Read back the new version.
            let new_version: (i64,) = sqlx::query_as(
                "SELECT version FROM grain_state WHERE type_name = ? AND key = ?",
            )
            .bind(grain_id.type_name)
            .bind(&grain_id.key)
            .fetch_one(&self.pool)
            .await?;

            Ok(Some(ETag(new_version.0.to_string())))
        } else {
            // No etag check — upsert and return the new version.
            sqlx::query(
                "INSERT INTO grain_state (type_name, key, data, version) VALUES (?, ?, ?, 1)
                 ON CONFLICT (type_name, key) DO UPDATE SET data = excluded.data, version = version + 1",
            )
            .bind(grain_id.type_name)
            .bind(&grain_id.key)
            .bind(data)
            .execute(&self.pool)
            .await?;

            let new_version: (i64,) = sqlx::query_as(
                "SELECT version FROM grain_state WHERE type_name = ? AND key = ?",
            )
            .bind(grain_id.type_name)
            .bind(&grain_id.key)
            .fetch_one(&self.pool)
            .await?;

            Ok(Some(ETag(new_version.0.to_string())))
        }
    }
}
