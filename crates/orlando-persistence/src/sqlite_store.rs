use async_trait::async_trait;
use sqlx::SqlitePool;
use orlando_core::GrainId;

use crate::store::{PersistenceError, StateStore};

/// SQLite-backed state store for durable grain persistence.
/// Stores grain state as a binary blob in a single table keyed by (type_name, key).
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
                PRIMARY KEY (type_name, key)
            )",
        )
        .execute(&pool)
        .await?;

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
            "INSERT INTO grain_state (type_name, key, data) VALUES (?, ?, ?)
             ON CONFLICT (type_name, key) DO UPDATE SET data = excluded.data",
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
}
