//! Integration tests for PostgresReplicationLog.
//!
//! Requires a running PostgreSQL instance. Set `ORLANDO_TEST_POSTGRES_URL`
//! to enable, e.g.:
//!
//! ```bash
//! docker run -d --name orlando-pg -e POSTGRES_PASSWORD=test \
//!     -e POSTGRES_DB=orlando_test -p 5432:5432 postgres:16
//! ORLANDO_TEST_POSTGRES_URL=postgres://postgres:test@localhost/orlando_test \
//!     cargo test -p orlando-persistence --features postgres -- --ignored
//! ```

#![cfg(feature = "postgres")]

use orlando_core::ClusterId;
use orlando_core::replication::{ReplicationEntry, ReplicationEntryType};
use orlando_persistence::{PostgresReplicationLog, ReplicationError, ReplicationLog};

fn test_url() -> String {
    std::env::var("ORLANDO_TEST_POSTGRES_URL")
        .expect("set ORLANDO_TEST_POSTGRES_URL to run Postgres tests")
}

fn entry(grain_key: &str, seq: u64, cluster: &str) -> ReplicationEntry {
    ReplicationEntry {
        grain_type: "PgReplCounter".to_string(),
        grain_key: grain_key.to_string(),
        sequence: seq,
        timestamp_millis: 1_700_000_000_000 + seq as i64,
        source_cluster: ClusterId::new(cluster),
        entry_type: ReplicationEntryType::FullState,
        payload: vec![seq as u8; 16],
    }
}

async fn fresh_log() -> PostgresReplicationLog {
    let log = PostgresReplicationLog::new(&test_url()).await.unwrap();
    // Make tests independent — truncate before each.
    log.truncate("PgReplCounter", "k", u64::MAX).await.unwrap();
    log.truncate("PgReplCounter", "a", u64::MAX).await.unwrap();
    log.truncate("PgReplCounter", "b", u64::MAX).await.unwrap();
    log
}

#[tokio::test]
#[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
async fn append_and_read_roundtrip() {
    let log = fresh_log().await;
    log.append(entry("k", 1, "us-east")).await.unwrap();
    log.append(entry("k", 2, "us-east")).await.unwrap();

    let got = log.read_from("PgReplCounter", "k", 0, 100).await.unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].sequence, 1);
    assert_eq!(got[1].payload, vec![2u8; 16]);
}

#[tokio::test]
#[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
async fn truncate_removes_before_sequence() {
    let log = fresh_log().await;
    for s in 1..=5 {
        log.append(entry("k", s, "us-east")).await.unwrap();
    }
    let n = log.truncate("PgReplCounter", "k", 4).await.unwrap();
    assert_eq!(n, 3);
    let remaining = log.read_from("PgReplCounter", "k", 0, 100).await.unwrap();
    assert_eq!(remaining.iter().map(|e| e.sequence).collect::<Vec<_>>(), vec![4, 5]);
}

#[tokio::test]
#[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
async fn non_monotonic_sequence_rejected() {
    let log = fresh_log().await;
    log.append(entry("k", 5, "us-east")).await.unwrap();
    let err = log.append(entry("k", 3, "us-east")).await.unwrap_err();
    assert!(matches!(err, ReplicationError::SequenceConflict { .. }));
}

#[tokio::test]
#[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
async fn entries_survive_pool_reopen() {
    {
        let log = fresh_log().await;
        log.append(entry("k", 1, "us-east")).await.unwrap();
        log.append(entry("k", 2, "us-east")).await.unwrap();
    }
    let log2 = PostgresReplicationLog::new(&test_url()).await.unwrap();
    let got = log2.read_from("PgReplCounter", "k", 0, 100).await.unwrap();
    assert_eq!(got.len(), 2);
    log2.truncate("PgReplCounter", "k", u64::MAX).await.unwrap();
}
