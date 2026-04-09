use async_trait::async_trait;
use orlando_core::GrainId;
use sqlx::PgPool;

use crate::store::{PersistenceError, StateStore};

/// PostgreSQL-backed state store for production grain persistence.
/// Stores grain state as binary in a single table keyed by (type_name, key).
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
        let pool = PgPool::connect(url).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS grain_state (
                type_name TEXT NOT NULL,
                key       TEXT NOT NULL,
                data      BYTEA NOT NULL,
                PRIMARY KEY (type_name, key)
            )",
        )
        .execute(&pool)
        .await?;

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
        .await?;

        Ok(row.map(|(data,)| data))
    }

    async fn save(&self, grain_id: &GrainId, data: &[u8]) -> Result<(), PersistenceError> {
        sqlx::query(
            "INSERT INTO grain_state (type_name, key, data) VALUES ($1, $2, $3)
             ON CONFLICT (type_name, key) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .bind(data)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM grain_state WHERE type_name = $1 AND key = $2")
            .bind(grain_id.type_name)
            .bind(&grain_id.key)
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}
