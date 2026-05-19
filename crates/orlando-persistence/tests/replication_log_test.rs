//! Tests for the ReplicationLog trait and InMemoryReplicationLog backend.

use orlando_core::replication::{ReplicationEntry, ReplicationEntryType};
use orlando_core::ClusterId;
use orlando_persistence::{InMemoryReplicationLog, ReplicationLog};

fn make_entry(grain_key: &str, seq: u64, cluster: &str) -> ReplicationEntry {
    ReplicationEntry {
        grain_type: "Counter".to_string(),
        grain_key: grain_key.to_string(),
        sequence: seq,
        timestamp_millis: 1000 + seq as i64,
        source_cluster: ClusterId::new(cluster),
        entry_type: ReplicationEntryType::FullState,
        payload: vec![seq as u8; 4],
    }
}

#[tokio::test]
async fn append_and_read_back() {
    let log = InMemoryReplicationLog::new();

    log.append(make_entry("k1", 1, "us-east")).await.unwrap();
    log.append(make_entry("k1", 2, "us-east")).await.unwrap();
    log.append(make_entry("k1", 3, "us-east")).await.unwrap();

    let entries = log.read_from("Counter", "k1", 0, 100).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].sequence, 1);
    assert_eq!(entries[2].sequence, 3);
}

#[tokio::test]
async fn read_from_offset() {
    let log = InMemoryReplicationLog::new();

    for seq in 1..=5 {
        log.append(make_entry("k1", seq, "us-east")).await.unwrap();
    }

    // Read entries after sequence 3
    let entries = log.read_from("Counter", "k1", 3, 100).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].sequence, 4);
    assert_eq!(entries[1].sequence, 5);
}

#[tokio::test]
async fn read_respects_limit() {
    let log = InMemoryReplicationLog::new();

    for seq in 1..=10 {
        log.append(make_entry("k1", seq, "us-east")).await.unwrap();
    }

    let entries = log.read_from("Counter", "k1", 0, 3).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[2].sequence, 3);
}

#[tokio::test]
async fn latest_sequence() {
    let log = InMemoryReplicationLog::new();

    assert_eq!(log.latest_sequence("Counter", "k1").await.unwrap(), 0);

    log.append(make_entry("k1", 1, "us-east")).await.unwrap();
    assert_eq!(log.latest_sequence("Counter", "k1").await.unwrap(), 1);

    log.append(make_entry("k1", 5, "us-east")).await.unwrap();
    assert_eq!(log.latest_sequence("Counter", "k1").await.unwrap(), 5);
}

#[tokio::test]
async fn truncate_removes_old_entries() {
    let log = InMemoryReplicationLog::new();

    for seq in 1..=5 {
        log.append(make_entry("k1", seq, "us-east")).await.unwrap();
    }

    let deleted = log.truncate("Counter", "k1", 3).await.unwrap();
    assert_eq!(deleted, 2, "should delete sequences 1 and 2");

    let remaining = log.read_from("Counter", "k1", 0, 100).await.unwrap();
    assert_eq!(remaining.len(), 3);
    assert_eq!(remaining[0].sequence, 3);
}

#[tokio::test]
async fn non_monotonic_sequence_rejected() {
    let log = InMemoryReplicationLog::new();

    log.append(make_entry("k1", 5, "us-east")).await.unwrap();

    // Sequence 3 is lower than 5 — should fail
    let result = log.append(make_entry("k1", 3, "us-east")).await;
    assert!(result.is_err(), "non-monotonic sequence should be rejected");

    // Same sequence should also fail
    let result = log.append(make_entry("k1", 5, "us-east")).await;
    assert!(result.is_err(), "duplicate sequence should be rejected");
}

#[tokio::test]
async fn separate_grains_have_independent_logs() {
    let log = InMemoryReplicationLog::new();

    log.append(make_entry("grain-a", 1, "us-east"))
        .await
        .unwrap();
    log.append(make_entry("grain-a", 2, "us-east"))
        .await
        .unwrap();
    log.append(make_entry("grain-b", 1, "eu-west"))
        .await
        .unwrap();

    assert_eq!(
        log.latest_sequence("Counter", "grain-a").await.unwrap(),
        2
    );
    assert_eq!(
        log.latest_sequence("Counter", "grain-b").await.unwrap(),
        1
    );

    let a_entries = log.read_from("Counter", "grain-a", 0, 100).await.unwrap();
    assert_eq!(a_entries.len(), 2);

    let b_entries = log.read_from("Counter", "grain-b", 0, 100).await.unwrap();
    assert_eq!(b_entries.len(), 1);
    assert_eq!(b_entries[0].source_cluster.as_str(), "eu-west");
}

#[tokio::test]
async fn read_from_nonexistent_grain_returns_empty() {
    let log = InMemoryReplicationLog::new();

    let entries = log
        .read_from("Counter", "nonexistent", 0, 100)
        .await
        .unwrap();
    assert!(entries.is_empty());

    assert_eq!(
        log.latest_sequence("Counter", "nonexistent").await.unwrap(),
        0
    );
}
