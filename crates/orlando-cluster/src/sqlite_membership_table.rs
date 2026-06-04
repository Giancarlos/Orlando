//! Durable SQLite-backed [`MembershipTable`] (PROD-15, Phase A).
//!
//! Linearizable single-activation rests on a strongly-consistent membership
//! view. SQLite gives us that on a single node / shared file: all writes go
//! through one connection and the version compare-and-swap runs inside a
//! transaction, so the monotonic version row totally-orders membership changes
//! exactly as the in-memory backend does — but durably across restarts.
//!
//! The pool is capped at a single connection: it makes `sqlite::memory:` usable
//! (a multi-connection pool would give each connection its own empty database),
//! and it serializes the CAS without relying on SQLite's busy-handler. The
//! membership write rate is low, so one connection is not a bottleneck.

use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::hash_ring::SiloAddress;
use crate::membership_table::{
    MemberEntry, MembershipError, MembershipStatus, MembershipTable, MembershipView,
};

/// SQLite-backed membership table.
#[derive(Debug, Clone)]
pub struct SqliteMembershipTable {
    pool: SqlitePool,
}

impl SqliteMembershipTable {
    /// Connect and ensure the schema + the single version row exist.
    ///
    /// `url` is a SQLite connection string, e.g. `"sqlite::memory:"` (tests) or
    /// `"sqlite://cluster.db"` (the file is created if missing).
    pub async fn connect(url: &str) -> Result<Self, MembershipError> {
        let opts = SqliteConnectOptions::from_str(url)
            .map_err(be)?
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(be)?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS membership_member (
                 silo_id     TEXT PRIMARY KEY,
                 host        TEXT NOT NULL,
                 port        INTEGER NOT NULL,
                 status      TEXT NOT NULL,
                 generation  INTEGER NOT NULL,
                 suspectors  TEXT NOT NULL,
                 i_am_alive  INTEGER NOT NULL
             )",
        )
        .execute(&pool)
        .await
        .map_err(be)?;

        // Single monotonic version row (id is pinned to 0).
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS membership_version (
                 id      INTEGER PRIMARY KEY CHECK (id = 0),
                 version INTEGER NOT NULL
             )",
        )
        .execute(&pool)
        .await
        .map_err(be)?;
        sqlx::query("INSERT OR IGNORE INTO membership_version (id, version) VALUES (0, 0)")
            .execute(&pool)
            .await
            .map_err(be)?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl MembershipTable for SqliteMembershipTable {
    async fn read_all(&self) -> Result<MembershipView, MembershipError> {
        let (version,): (i64,) = sqlx::query_as("SELECT version FROM membership_version WHERE id = 0")
            .fetch_one(&self.pool)
            .await
            .map_err(be)?;

        // ORDER BY silo_id => deterministic order, matching InMemoryMembershipTable.
        let rows: Vec<(String, String, i64, String, i64, String, i64)> = sqlx::query_as(
            "SELECT silo_id, host, port, status, generation, suspectors, i_am_alive
             FROM membership_member ORDER BY silo_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(be)?;

        let members = rows.into_iter().map(row_to_entry).collect();
        Ok(MembershipView { version: version as u64, members })
    }

    async fn try_update(
        &self,
        entry: MemberEntry,
        expected_version: u64,
    ) -> Result<MembershipView, MembershipError> {
        let mut tx = self.pool.begin().await.map_err(be)?;

        let (current,): (i64,) = sqlx::query_as("SELECT version FROM membership_version WHERE id = 0")
            .fetch_one(&mut *tx)
            .await
            .map_err(be)?;
        if current as u64 != expected_version {
            // Dropping `tx` without commit rolls the transaction back.
            return Err(MembershipError::VersionConflict {
                expected: expected_version,
                actual: current as u64,
            });
        }

        sqlx::query(
            "INSERT INTO membership_member
                 (silo_id, host, port, status, generation, suspectors, i_am_alive)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(silo_id) DO UPDATE SET
                 host=excluded.host, port=excluded.port, status=excluded.status,
                 generation=excluded.generation, suspectors=excluded.suspectors,
                 i_am_alive=excluded.i_am_alive",
        )
        .bind(&entry.addr.silo_id)
        .bind(&entry.addr.host)
        .bind(i64::from(entry.addr.port))
        .bind(status_to_str(entry.status))
        .bind(entry.generation as i64)
        .bind(join_suspectors(&entry.suspectors))
        .bind(systemtime_to_millis(entry.i_am_alive))
        .execute(&mut *tx)
        .await
        .map_err(be)?;

        sqlx::query("UPDATE membership_version SET version = version + 1 WHERE id = 0")
            .execute(&mut *tx)
            .await
            .map_err(be)?;

        tx.commit().await.map_err(be)?;
        self.read_all().await
    }

    async fn update_i_am_alive(&self, silo_id: &str) -> Result<(), MembershipError> {
        // Diagnostic-only: deliberately does not touch the version row.
        let result = sqlx::query("UPDATE membership_member SET i_am_alive = ? WHERE silo_id = ?")
            .bind(systemtime_to_millis(SystemTime::now()))
            .bind(silo_id)
            .execute(&self.pool)
            .await
            .map_err(be)?;
        if result.rows_affected() == 0 {
            return Err(MembershipError::NotFound(silo_id.to_string()));
        }
        Ok(())
    }
}

// --- helpers ---

fn be<E: std::fmt::Display>(e: E) -> MembershipError {
    MembershipError::Backend(e.to_string())
}

fn row_to_entry(
    (silo_id, host, port, status, generation, suspectors, alive): (
        String,
        String,
        i64,
        String,
        i64,
        String,
        i64,
    ),
) -> MemberEntry {
    MemberEntry {
        addr: SiloAddress { host, port: port as u16, silo_id },
        status: status_from_str(&status),
        generation: generation as u64,
        suspectors: split_suspectors(&suspectors),
        i_am_alive: millis_to_systemtime(alive),
    }
}

fn status_to_str(s: MembershipStatus) -> &'static str {
    match s {
        MembershipStatus::Joining => "joining",
        MembershipStatus::Active => "active",
        MembershipStatus::Suspect => "suspect",
        MembershipStatus::Dead => "dead",
    }
}

fn status_from_str(s: &str) -> MembershipStatus {
    match s {
        "joining" => MembershipStatus::Joining,
        "active" => MembershipStatus::Active,
        "suspect" => MembershipStatus::Suspect,
        // An unrecognized status is treated as Dead: fail safe (not Active).
        _ => MembershipStatus::Dead,
    }
}

// Suspectors are silo ids joined by '\n' (newlines never appear in a silo id).
fn join_suspectors(s: &[String]) -> String {
    s.join("\n")
}

fn split_suspectors(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split('\n').map(str::to_string).collect()
    }
}

fn systemtime_to_millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn millis_to_systemtime(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(id: &str, generation: u64) -> MemberEntry {
        MemberEntry {
            addr: SiloAddress { host: "127.0.0.1".into(), port: 7000, silo_id: id.into() },
            status: MembershipStatus::Active,
            generation,
            suspectors: Vec::new(),
            i_am_alive: UNIX_EPOCH + Duration::from_millis(1_000),
        }
    }

    async fn table() -> SqliteMembershipTable {
        SqliteMembershipTable::connect("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn empty_starts_at_version_zero() {
        let t = table().await;
        let view = t.read_all().await.unwrap();
        assert_eq!(view.version, 0);
        assert!(view.members.is_empty());
    }

    #[tokio::test]
    async fn try_update_inserts_and_bumps_version() {
        let t = table().await;
        let view = t.try_update(active("silo-a", 1), 0).await.unwrap();
        assert_eq!(view.version, 1);
        assert_eq!(view.members.len(), 1);
        assert_eq!(view.member("silo-a").unwrap().generation, 1);
    }

    #[tokio::test]
    async fn stale_version_conflicts_and_does_not_write() {
        let t = table().await;
        t.try_update(active("silo-a", 1), 0).await.unwrap(); // -> 1
        let err = t.try_update(active("silo-b", 1), 0).await.unwrap_err();
        assert_eq!(err, MembershipError::VersionConflict { expected: 0, actual: 1 });
        // The conflicting write must have rolled back: still one member, version 1.
        let view = t.read_all().await.unwrap();
        assert_eq!(view.version, 1);
        assert_eq!(view.members.len(), 1);
    }

    #[tokio::test]
    async fn roundtrip_preserves_fields_and_is_sorted() {
        let t = table().await;
        let mut v = 0;
        for (id, g) in [("silo-c", 3u64), ("silo-a", 1), ("silo-b", 2)] {
            v = t.try_update(active(id, g), v).await.unwrap().version;
        }
        let mut suspected = active("silo-a", 1);
        suspected.status = MembershipStatus::Suspect;
        suspected.suspectors = vec!["silo-b".into(), "silo-c".into()];
        t.try_update(suspected, v).await.unwrap();

        let view = t.read_all().await.unwrap();
        let ids: Vec<_> = view.members.iter().map(|m| m.addr.silo_id.as_str()).collect();
        assert_eq!(ids, ["silo-a", "silo-b", "silo-c"], "deterministic order");
        let a = view.member("silo-a").unwrap();
        assert_eq!(a.status, MembershipStatus::Suspect);
        assert_eq!(a.suspectors, vec!["silo-b".to_string(), "silo-c".to_string()]);
        assert_eq!(a.addr.port, 7000);
    }

    #[tokio::test]
    async fn i_am_alive_does_not_bump_version() {
        let t = table().await;
        t.try_update(active("silo-a", 1), 0).await.unwrap();
        t.update_i_am_alive("silo-a").await.unwrap();
        assert_eq!(t.read_all().await.unwrap().version, 1);
        assert_eq!(
            t.update_i_am_alive("ghost").await.unwrap_err(),
            MembershipError::NotFound("ghost".into())
        );
    }
}
