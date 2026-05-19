//! Tests for the failover promotion mechanism.

use std::sync::Arc;

use orlando_cluster::{
    CrossClusterDirectory, FailoverConfig, FailoverManager,
    InMemoryCrossClusterDirectory,
};
use orlando_core::{ClusterId, GrainId};

fn grain(key: &str) -> GrainId {
    GrainId {
        type_name: "Counter",
        key: key.to_string(),
    }
}

/// promote_grain CAS-registers with incremented epoch.
#[tokio::test]
async fn promote_grain_increments_epoch() {
    let dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");
    let grain_id = grain("failover-1");

    // Cluster A owns at epoch 1
    let ownership = dir.register(&grain_id, &cluster_a, 1).await.unwrap();
    assert_eq!(ownership.cluster_id, cluster_a);

    // Set up a failover manager for cluster B
    let config = orlando_cluster::MultiClusterConfig::new("cluster-b")
        .peer("cluster-a", "127.0.0.1:1"); // unreachable
    let pool = Arc::new(orlando_cluster::ConnectionPool::new());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let health = Arc::new(orlando_cluster::ClusterHealth::new(
        config,
        pool,
        shutdown_rx.clone(),
    ));

    let manager = FailoverManager::new(
        cluster_b.clone(),
        FailoverConfig::default(),
        health,
        dir.clone(),
        shutdown_rx,
    );

    // Promote grain from A to B
    let result = manager.promote_grain(&grain_id, &ownership).await.unwrap();
    assert_eq!(result.cluster_id, cluster_b);
    assert_eq!(result.epoch, 2);

    // Verify directory reflects new owner
    let lookup = dir.lookup(&grain_id).await.unwrap().unwrap();
    assert_eq!(lookup.cluster_id, cluster_b);
    assert_eq!(lookup.epoch, 2);
}

/// When two clusters race to promote, only one wins.
#[tokio::test]
async fn concurrent_promotion_only_one_wins() {
    let dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");
    let cluster_c = ClusterId::new("cluster-c");
    let grain_id = grain("failover-2");

    // A owns at epoch 1
    let _ownership = dir.register(&grain_id, &cluster_a, 1).await.unwrap();

    // B promotes at epoch 2
    let result_b = dir.register(&grain_id, &cluster_b, 2).await.unwrap();
    assert_eq!(result_b.cluster_id, cluster_b);
    assert_eq!(result_b.epoch, 2);

    // C also tries epoch 2 — but B already won at epoch 2
    let result_c = dir.register(&grain_id, &cluster_c, 2).await.unwrap();
    assert_eq!(
        result_c.cluster_id, cluster_b,
        "C at same epoch should not win over B"
    );
}

/// Stale primary cannot reclaim after promotion.
#[tokio::test]
async fn stale_primary_fenced_after_promotion() {
    let dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");
    let grain_id = grain("failover-3");

    // A owns at epoch 1, B promotes at epoch 2
    dir.register(&grain_id, &cluster_a, 1).await.unwrap();
    dir.register(&grain_id, &cluster_b, 2).await.unwrap();

    // A comes back and tries epoch 1 — fenced
    let result = dir.register(&grain_id, &cluster_a, 1).await.unwrap();
    assert_eq!(result.cluster_id, cluster_b);
    assert_eq!(result.epoch, 2);

    // A tries epoch 2 — still can't beat B who already has epoch 2
    let result = dir.register(&grain_id, &cluster_a, 2).await.unwrap();
    assert_eq!(result.cluster_id, cluster_b);
}
