//! Integration tests for SqliteReplicationLog.
//!
//! Uses an in-memory SQLite database per test to avoid filesystem state.

use orlando_core::ClusterId;
use orlando_core::replication::{ReplicationEntry, ReplicationEntryType};
use orlando_persistence::{ReplicationLog, SqliteReplicationLog};

fn entry(grain_key: &str, seq: u64, cluster: &str) -> ReplicationEntry {
    ReplicationEntry {
        grain_type: "Counter".to_string(),
        grain_key: grain_key.to_string(),
        sequence: seq,
        timestamp_millis: 1_700_000_000_000 + seq as i64,
        source_cluster: ClusterId::new(cluster),
        entry_type: ReplicationEntryType::FullState,
        payload: vec![seq as u8; 8],
    }
}

#[tokio::test]
async fn append_and_read_roundtrip() {
    let log = SqliteReplicationLog::new("sqlite::memory:").await.unwrap();

    log.append(entry("k", 1, "us-east")).await.unwrap();
    log.append(entry("k", 2, "us-east")).await.unwrap();
    log.append(entry("k", 3, "us-east")).await.unwrap();

    let got = log.read_from("Counter", "k", 0, 100).await.unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].sequence, 1);
    assert_eq!(got[2].sequence, 3);
    assert_eq!(got[0].source_cluster.as_str(), "us-east");
    assert_eq!(got[2].payload, vec![3u8; 8]);
}

#[tokio::test]
async fn read_from_with_after_sequence() {
    let log = SqliteReplicationLog::new("sqlite::memory:").await.unwrap();
    for s in 1..=5 {
        log.append(entry("k", s, "us-east")).await.unwrap();
    }
    let got = log.read_from("Counter", "k", 2, 100).await.unwrap();
    assert_eq!(got.iter().map(|e| e.sequence).collect::<Vec<_>>(), vec![3, 4, 5]);
}

#[tokio::test]
async fn limit_caps_returned_entries() {
    let log = SqliteReplicationLog::new("sqlite::memory:").await.unwrap();
    for s in 1..=10 {
        log.append(entry("k", s, "us-east")).await.unwrap();
    }
    let got = log.read_from("Counter", "k", 0, 3).await.unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got.last().unwrap().sequence, 3);
}

#[tokio::test]
async fn latest_sequence_tracks_appends() {
    let log = SqliteReplicationLog::new("sqlite::memory:").await.unwrap();
    assert_eq!(log.latest_sequence("Counter", "k").await.unwrap(), 0);

    log.append(entry("k", 1, "us-east")).await.unwrap();
    log.append(entry("k", 7, "us-east")).await.unwrap();
    assert_eq!(log.latest_sequence("Counter", "k").await.unwrap(), 7);
}

#[tokio::test]
async fn truncate_removes_entries_before_sequence() {
    let log = SqliteReplicationLog::new("sqlite::memory:").await.unwrap();
    for s in 1..=5 {
        log.append(entry("k", s, "us-east")).await.unwrap();
    }
    let removed = log.truncate("Counter", "k", 4).await.unwrap();
    assert_eq!(removed, 3);
    let remaining = log.read_from("Counter", "k", 0, 100).await.unwrap();
    assert_eq!(remaining.iter().map(|e| e.sequence).collect::<Vec<_>>(), vec![4, 5]);
}

#[tokio::test]
async fn non_monotonic_sequence_rejected() {
    let log = SqliteReplicationLog::new("sqlite::memory:").await.unwrap();
    log.append(entry("k", 5, "us-east")).await.unwrap();

    let err = log
        .append(entry("k", 3, "us-east"))
        .await
        .expect_err("sequence regression must fail");
    matches!(err, orlando_persistence::ReplicationError::SequenceConflict { .. })
        .then_some(())
        .expect("expected SequenceConflict");
}

#[tokio::test]
async fn entries_are_isolated_per_grain() {
    let log = SqliteReplicationLog::new("sqlite::memory:").await.unwrap();
    log.append(entry("a", 1, "us-east")).await.unwrap();
    log.append(entry("b", 1, "eu-west")).await.unwrap();
    log.append(entry("a", 2, "us-east")).await.unwrap();

    let a = log.read_from("Counter", "a", 0, 100).await.unwrap();
    let b = log.read_from("Counter", "b", 0, 100).await.unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].source_cluster.as_str(), "eu-west");
}

#[tokio::test]
async fn entries_survive_pool_reopen() {
    // Use a real on-disk database so we can confirm durability across pool drop.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("repl.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());

    {
        let log = SqliteReplicationLog::new(&url).await.unwrap();
        log.append(entry("k", 1, "us-east")).await.unwrap();
        log.append(entry("k", 2, "us-east")).await.unwrap();
    } // pool dropped

    let log2 = SqliteReplicationLog::new(&url).await.unwrap();
    let got = log2.read_from("Counter", "k", 0, 100).await.unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].sequence, 2);
}
