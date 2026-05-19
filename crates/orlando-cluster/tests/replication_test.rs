//! Tests for the replication producer pump and replica store.

use std::sync::Arc;
use std::time::Duration;

use orlando_cluster::{ReplicaStore, spawn_replication_pump};
use orlando_core::replication::{ReplicationEntry, ReplicationEntryType};
use orlando_core::ClusterId;
use orlando_persistence::{InMemoryReplicationLog, ReplicationLog};

fn make_entry(grain_key: &str, seq: u64, payload: &[u8]) -> ReplicationEntry {
    ReplicationEntry {
        grain_type: "TestGrain".to_string(),
        grain_key: grain_key.to_string(),
        sequence: seq,
        timestamp_millis: 1000 + seq as i64,
        source_cluster: ClusterId::new("us-east"),
        entry_type: ReplicationEntryType::FullState,
        payload: payload.to_vec(),
    }
}

/// The replication pump reads from a channel and appends to the log.
#[tokio::test]
async fn replication_pump_appends_to_log() {
    let log = Arc::new(InMemoryReplicationLog::new());
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    let _handle = spawn_replication_pump(rx, log.clone());

    // Send entries through the channel
    tx.send(make_entry("k1", 1, &[10])).await.unwrap();
    tx.send(make_entry("k1", 2, &[20])).await.unwrap();
    tx.send(make_entry("k1", 3, &[30])).await.unwrap();

    // Drop sender to close the channel
    drop(tx);

    // Give the pump a moment to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify entries are in the log
    let entries = log.read_from("TestGrain", "k1", 0, 100).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].payload, vec![10]);
    assert_eq!(entries[2].payload, vec![30]);
}

/// ReplicaStore: update, get, freshness check.
#[test]
fn replica_store_basic_operations() {
    let store = ReplicaStore::new();

    // Initially empty
    assert!(store.get("TestGrain", "k1").is_none());
    assert!(!store.is_fresh("TestGrain", "k1", 5000));

    // Add a replica
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    store.update("TestGrain", "k1", vec![42], 1, now);

    let entry = store.get("TestGrain", "k1").unwrap();
    assert_eq!(entry.payload, vec![42]);
    assert_eq!(entry.sequence, 1);

    // Should be fresh (within 5 seconds)
    assert!(store.is_fresh("TestGrain", "k1", 5000));

    // Update to newer state
    store.update("TestGrain", "k1", vec![99], 2, now);
    let entry = store.get("TestGrain", "k1").unwrap();
    assert_eq!(entry.payload, vec![99]);
    assert_eq!(entry.sequence, 2);
}

/// ReplicaStore: stale data detection.
#[test]
fn replica_store_staleness() {
    let store = ReplicaStore::new();

    // Add an entry with a very old timestamp
    store.update("TestGrain", "k1", vec![1], 1, 1000);

    // Should NOT be fresh (1000ms timestamp is way in the past)
    assert!(!store.is_fresh("TestGrain", "k1", 5000));
}

/// ReplicaStore: remove.
#[test]
fn replica_store_remove() {
    let store = ReplicaStore::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    store.update("TestGrain", "k1", vec![1], 1, now);
    assert!(store.get("TestGrain", "k1").is_some());

    store.remove("TestGrain", "k1");
    assert!(store.get("TestGrain", "k1").is_none());
}

/// ReplicaStore: different grains are independent.
#[test]
fn replica_store_independent_grains() {
    let store = ReplicaStore::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    store.update("TestGrain", "k1", vec![1], 1, now);
    store.update("TestGrain", "k2", vec![2], 1, now);
    store.update("OtherGrain", "k1", vec![3], 1, now);

    assert_eq!(store.get("TestGrain", "k1").unwrap().payload, vec![1]);
    assert_eq!(store.get("TestGrain", "k2").unwrap().payload, vec![2]);
    assert_eq!(store.get("OtherGrain", "k1").unwrap().payload, vec![3]);
}
