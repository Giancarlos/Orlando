//! Tests for epoch-based directory CAS and failover promotion.

use std::sync::Arc;

use orlando_cluster::{CrossClusterDirectory, InMemoryCrossClusterDirectory};
use orlando_core::{ClusterId, GrainId};

fn grain(key: &str) -> GrainId {
    GrainId {
        type_name: "TestGrain",
        key: key.to_string(),
    }
}

#[tokio::test]
async fn first_writer_wins_at_same_epoch() {
    let dir = InMemoryCrossClusterDirectory::new();
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");
    let grain = grain("counter-1");

    // Cluster A registers first at epoch 1
    let ownership = dir.register(&grain, &cluster_a, 1).await.unwrap();
    assert_eq!(ownership.cluster_id, cluster_a);
    assert_eq!(ownership.epoch, 1);

    // Cluster B tries to register at the same epoch — A wins
    let ownership = dir.register(&grain, &cluster_b, 1).await.unwrap();
    assert_eq!(ownership.cluster_id, cluster_a, "first writer should win");
    assert_eq!(ownership.epoch, 1);
}

#[tokio::test]
async fn higher_epoch_reclaims_ownership() {
    let dir = InMemoryCrossClusterDirectory::new();
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");
    let grain = grain("counter-2");

    // Cluster A owns at epoch 1
    dir.register(&grain, &cluster_a, 1).await.unwrap();

    // Cluster B promotes with epoch 2 (failover)
    let ownership = dir.register(&grain, &cluster_b, 2).await.unwrap();
    assert_eq!(ownership.cluster_id, cluster_b, "higher epoch should win");
    assert_eq!(ownership.epoch, 2);

    // Verify lookup confirms B
    let lookup = dir.lookup(&grain).await.unwrap().unwrap();
    assert_eq!(lookup.cluster_id, cluster_b);
    assert_eq!(lookup.epoch, 2);
}

#[tokio::test]
async fn stale_epoch_cannot_reclaim() {
    let dir = InMemoryCrossClusterDirectory::new();
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");
    let grain = grain("counter-3");

    // B promoted at epoch 2
    dir.register(&grain, &cluster_b, 2).await.unwrap();

    // A comes back and tries to reclaim at epoch 1 (stale)
    let ownership = dir.register(&grain, &cluster_a, 1).await.unwrap();
    assert_eq!(
        ownership.cluster_id, cluster_b,
        "stale epoch should not reclaim"
    );
    assert_eq!(ownership.epoch, 2);
}

#[tokio::test]
async fn deregister_only_works_for_owner() {
    let dir = InMemoryCrossClusterDirectory::new();
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");
    let grain = grain("counter-4");

    dir.register(&grain, &cluster_a, 1).await.unwrap();

    // B tries to deregister — should be a no-op
    dir.deregister(&grain, &cluster_b).await.unwrap();
    let ownership = dir.lookup(&grain).await.unwrap();
    assert!(ownership.is_some(), "B should not be able to deregister A's grain");
    assert_eq!(ownership.unwrap().cluster_id, cluster_a);

    // A deregisters successfully
    dir.deregister(&grain, &cluster_a).await.unwrap();
    let ownership = dir.lookup(&grain).await.unwrap();
    assert!(ownership.is_none(), "A should be able to deregister its own grain");
}

#[tokio::test]
async fn epoch_increases_through_successive_promotions() {
    let dir = InMemoryCrossClusterDirectory::new();
    let a = ClusterId::new("a");
    let b = ClusterId::new("b");
    let c = ClusterId::new("c");
    let grain = grain("counter-5");

    // A registers at epoch 1
    dir.register(&grain, &a, 1).await.unwrap();

    // B promotes at epoch 2
    dir.register(&grain, &b, 2).await.unwrap();

    // C promotes at epoch 3
    let ownership = dir.register(&grain, &c, 3).await.unwrap();
    assert_eq!(ownership.cluster_id, c);
    assert_eq!(ownership.epoch, 3);

    // A tries epoch 2 — too low
    let ownership = dir.register(&grain, &a, 2).await.unwrap();
    assert_eq!(ownership.cluster_id, c, "epoch 2 should not beat epoch 3");
}
