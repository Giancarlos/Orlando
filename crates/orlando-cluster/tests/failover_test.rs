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

/// list_owned_by returns all grains owned by a given cluster.
#[tokio::test]
async fn list_owned_by_returns_matching_grains() {
    let dir = InMemoryCrossClusterDirectory::new();
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");

    dir.register(&grain("g1"), &cluster_a, 1).await.unwrap();
    dir.register(&grain("g2"), &cluster_a, 1).await.unwrap();
    dir.register(&grain("g3"), &cluster_b, 1).await.unwrap();

    let owned_by_a = dir.list_owned_by(&cluster_a).await.unwrap();
    assert_eq!(owned_by_a.len(), 2);
    let mut keys: Vec<_> = owned_by_a.iter().map(|(g, _)| g.key.clone()).collect();
    keys.sort();
    assert_eq!(keys, vec!["g1", "g2"]);

    let owned_by_b = dir.list_owned_by(&cluster_b).await.unwrap();
    assert_eq!(owned_by_b.len(), 1);
    assert_eq!(owned_by_b[0].0.key, "g3");
}

/// End-to-end: manager iterates list_owned_by and promotes each grain.
#[tokio::test]
async fn promotion_sweep_promotes_every_grain_owned_by_failed_cluster() {
    let dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let cluster_a = ClusterId::new("cluster-a");
    let cluster_b = ClusterId::new("cluster-b");

    // A owns three grains at epoch 1
    for k in ["x", "y", "z"] {
        dir.register(&grain(k), &cluster_a, 1).await.unwrap();
    }
    // B owns one grain at epoch 1 — must not be promoted to itself again
    dir.register(&grain("untouched"), &cluster_b, 1).await.unwrap();

    let owned_by_a = dir.list_owned_by(&cluster_a).await.unwrap();
    assert_eq!(owned_by_a.len(), 3);

    // Simulate the FailoverManager::Promoting sweep by calling promote_grain
    // for each grain owned by the failed cluster.
    let config = orlando_cluster::MultiClusterConfig::new("cluster-b")
        .peer("cluster-a", "127.0.0.1:1");
    let pool = Arc::new(orlando_cluster::ConnectionPool::new());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let health = Arc::new(orlando_cluster::ClusterHealth::new(
        config, pool, shutdown_rx.clone(),
    ));
    let manager = FailoverManager::new(
        cluster_b.clone(),
        FailoverConfig::default(),
        health,
        dir.clone(),
        shutdown_rx,
    );

    for (gid, ownership) in owned_by_a {
        let r = manager.promote_grain(&gid, &ownership).await.unwrap();
        assert_eq!(r.cluster_id, cluster_b);
        assert_eq!(r.epoch, 2);
    }

    // Every A-owned grain is now owned by B at epoch 2.
    for k in ["x", "y", "z"] {
        let o = dir.lookup(&grain(k)).await.unwrap().unwrap();
        assert_eq!(o.cluster_id, cluster_b);
        assert_eq!(o.epoch, 2);
    }
    // The untouched B grain is still owned by B at epoch 1.
    let o = dir.lookup(&grain("untouched")).await.unwrap().unwrap();
    assert_eq!(o.cluster_id, cluster_b);
    assert_eq!(o.epoch, 1);
}
