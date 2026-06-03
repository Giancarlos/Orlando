//! SQLite-backed durable replication log.
//!
//! Entries persist across process restarts. Primary keys on
//! `(grain_type, grain_key, sequence)` enforce monotonic sequencing per grain.

use async_trait::async_trait;
use sqlx::SqlitePool;

use orlando_core::ClusterId;
use orlando_core::replication::{ReplicationEntry, ReplicationEntryType};

use crate::replication_log::{ReplicationError, ReplicationLog};

fn backend(e: impl std::fmt::Display) -> ReplicationError {
    ReplicationError::Backend(e.to_string())
}

fn entry_type_to_i64(t: &ReplicationEntryType) -> i64 {
    match t {
        ReplicationEntryType::FullState => 0,
        ReplicationEntryType::Delta => 1,
    }
}

fn entry_type_from_i64(v: i64) -> Result<ReplicationEntryType, ReplicationError> {
    match v {
        0 => Ok(ReplicationEntryType::FullState),
        1 => Ok(ReplicationEntryType::Delta),
        other => Err(ReplicationError::Deserialization(format!(
            "unknown entry_type discriminant: {other}"
        ))),
    }
}

/// SQLite-backed `ReplicationLog`.
///
/// Schema:
/// ```sql
/// CREATE TABLE replication_log (
///   grain_type        TEXT    NOT NULL,
///   grain_key         TEXT    NOT NULL,
///   sequence          INTEGER NOT NULL,
///   timestamp_millis  INTEGER NOT NULL,
///   source_cluster    TEXT    NOT NULL,
///   entry_type        INTEGER NOT NULL,
///   payload           BLOB    NOT NULL,
///   PRIMARY KEY (grain_type, grain_key, sequence)
/// );
/// ```
#[derive(Debug, Clone)]
pub struct SqliteReplicationLog {
    pool: SqlitePool,
}

impl SqliteReplicationLog {
    /// Connect and ensure the schema exists.
    pub async fn new(url: &str) -> Result<Self, ReplicationError> {
        let pool = SqlitePool::connect(url).await.map_err(backend)?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS replication_log (
                grain_type        TEXT    NOT NULL,
                grain_key         TEXT    NOT NULL,
                sequence          INTEGER NOT NULL,
                timestamp_millis  INTEGER NOT NULL,
                source_cluster    TEXT    NOT NULL,
                entry_type        INTEGER NOT NULL,
                payload           BLOB    NOT NULL,
                PRIMARY KEY (grain_type, grain_key, sequence)
            )",
        )
        .execute(&pool)
        .await
        .map_err(backend)?;

        Ok(Self { pool })
    }

    /// Wrap an existing pool (useful for sharing connections with a state store).
    pub async fn with_pool(pool: SqlitePool) -> Result<Self, ReplicationError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS replication_log (
                grain_type        TEXT    NOT NULL,
                grain_key         TEXT    NOT NULL,
                sequence          INTEGER NOT NULL,
                timestamp_millis  INTEGER NOT NULL,
                source_cluster    TEXT    NOT NULL,
                entry_type        INTEGER NOT NULL,
                payload           BLOB    NOT NULL,
                PRIMARY KEY (grain_type, grain_key, sequence)
            )",
        )
        .execute(&pool)
        .await
        .map_err(backend)?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl ReplicationLog for SqliteReplicationLog {
    async fn append(&self, entry: ReplicationEntry) -> Result<u64, ReplicationError> {
        // Enforce monotonic sequence inside a transaction so concurrent appenders
        // can't race past each other with stale latest_sequence reads.
        let mut tx = self.pool.begin().await.map_err(backend)?;

        let latest: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM replication_log WHERE grain_type = ? AND grain_key = ?",
        )
        .bind(&entry.grain_type)
        .bind(&entry.grain_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;

        if let Some(last) = latest {
            if (entry.sequence as i64) <= last {
                return Err(ReplicationError::SequenceConflict {
                    expected: (last as u64) + 1,
                    actual: entry.sequence,
                });
            }
        }

        sqlx::query(
            "INSERT INTO replication_log
             (grain_type, grain_key, sequence, timestamp_millis, source_cluster, entry_type, payload)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&entry.grain_type)
        .bind(&entry.grain_key)
        .bind(entry.sequence as i64)
        .bind(entry.timestamp_millis)
        .bind(entry.source_cluster.as_str())
        .bind(entry_type_to_i64(&entry.entry_type))
        .bind(&entry.payload)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;

        tx.commit().await.map_err(backend)?;
        Ok(entry.sequence)
    }

    async fn read_from(
        &self,
        grain_type: &str,
        grain_key: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<ReplicationEntry>, ReplicationError> {
        let rows: Vec<(i64, i64, String, i64, Vec<u8>)> = sqlx::query_as(
            "SELECT sequence, timestamp_millis, source_cluster, entry_type, payload
             FROM replication_log
             WHERE grain_type = ? AND grain_key = ? AND sequence > ?
             ORDER BY sequence ASC
             LIMIT ?",
        )
        .bind(grain_type)
        .bind(grain_key)
        .bind(after_sequence as i64)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(backend)?;

        let mut out = Vec::with_capacity(rows.len());
        for (sequence, timestamp_millis, source_cluster, entry_type, payload) in rows {
            out.push(ReplicationEntry {
                grain_type: grain_type.to_string(),
                grain_key: grain_key.to_string(),
                sequence: sequence as u64,
                timestamp_millis,
                source_cluster: ClusterId::new(source_cluster),
                entry_type: entry_type_from_i64(entry_type)?,
                payload,
            });
        }
        Ok(out)
    }

    async fn latest_sequence(
        &self,
        grain_type: &str,
        grain_key: &str,
    ) -> Result<u64, ReplicationError> {
        let latest: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM replication_log WHERE grain_type = ? AND grain_key = ?",
        )
        .bind(grain_type)
        .bind(grain_key)
        .fetch_one(&self.pool)
        .await
        .map_err(backend)?;

        Ok(latest.map(|v| v as u64).unwrap_or(0))
    }

    async fn truncate(
        &self,
        grain_type: &str,
        grain_key: &str,
        before_sequence: u64,
    ) -> Result<u64, ReplicationError> {
        let result = sqlx::query(
            "DELETE FROM replication_log
             WHERE grain_type = ? AND grain_key = ? AND sequence < ?",
        )
        .bind(grain_type)
        .bind(grain_key)
        .bind(before_sequence as i64)
        .execute(&self.pool)
        .await
        .map_err(backend)?;

        Ok(result.rows_affected())
    }
}
