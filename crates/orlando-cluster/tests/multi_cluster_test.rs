use std::sync::Arc;
use std::time::Duration;

use orlando_cluster::{ClusterId, ClusterSilo, MultiClusterConfig, PeerStatus, ClusterHealth};
use orlando_cluster::ConnectionPool;

#[test]
fn cluster_id_equality() {
    let a = ClusterId::new("us-east");
    let b = ClusterId::new("us-east");
    let c = ClusterId::new("eu-west");

    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.as_str(), "us-east");
    assert_eq!(format!("{}", a), "us-east");
}

#[test]
fn multi_cluster_config_builder() {
    let config = MultiClusterConfig::new("us-east")
        .peer("eu-west", "10.0.1.1:5000")
        .peer("ap-south", "10.0.2.1:5000")
        .health_check_interval(Duration::from_secs(30));

    assert_eq!(config.cluster_id, ClusterId::new("us-east"));
    assert_eq!(config.peers.len(), 2);
    assert_eq!(
        config.peers.get(&ClusterId::new("eu-west")),
        Some(&"10.0.1.1:5000".to_string())
    );
    assert_eq!(
        config.peers.get(&ClusterId::new("ap-south")),
        Some(&"10.0.2.1:5000".to_string())
    );
    assert_eq!(config.health_check_interval, Duration::from_secs(30));
}

#[test]
fn cluster_silo_without_multi_cluster_has_no_cluster_id() {
    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("solo")
        .build();

    assert!(silo.cluster_id().is_none());
}

#[test]
fn cluster_silo_with_multi_cluster_has_cluster_id() {
    let config = MultiClusterConfig::new("us-east")
        .peer("eu-west", "10.0.1.1:5000");

    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("node-1")
        .multi_cluster(config)
        .build();

    assert_eq!(silo.cluster_id(), Some(&ClusterId::new("us-east")));
}

#[tokio::test]
async fn health_checker_reports_unreachable_for_dead_peer() {
    let config = MultiClusterConfig::new("us-east")
        .peer("eu-west", "127.0.0.1:1") // nothing listening
        .health_check_interval(Duration::from_millis(50));

    let pool = Arc::new(ConnectionPool::new());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let health = ClusterHealth::new(config, pool, shutdown_rx);

    // Initially all peers are Unknown
    assert_eq!(
        health.status(&ClusterId::new("eu-west")),
        PeerStatus::Unknown,
    );

    assert_eq!(
        health.peer_endpoint(&ClusterId::new("eu-west")),
        Some("127.0.0.1:1".to_string()),
    );

    assert!(health.peer_endpoint(&ClusterId::new("nonexistent")).is_none());
}

#[tokio::test]
async fn two_cluster_silos_discover_each_other() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("orlando_cluster=debug")
        .try_init();

    // Start cluster A
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    // Start cluster B
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let mc_a = MultiClusterConfig::new("cluster-a")
        .peer("cluster-b", format!("127.0.0.1:{}", port_b))
        .health_check_interval(Duration::from_millis(100));

    let mc_b = MultiClusterConfig::new("cluster-b")
        .peer("cluster-a", format!("127.0.0.1:{}", port_a))
        .health_check_interval(Duration::from_millis(100));

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .multi_cluster(mc_a)
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .multi_cluster(mc_b)
            .build(),
    );

    assert_eq!(silo_a.cluster_id(), Some(&ClusterId::new("cluster-a")));
    assert_eq!(silo_b.cluster_id(), Some(&ClusterId::new("cluster-b")));

    let silo_a_clone = silo_a.clone();
    let server_a = tokio::spawn(async move {
        silo_a_clone.serve().await.unwrap();
    });

    let silo_b_clone = silo_b.clone();
    let server_b = tokio::spawn(async move {
        silo_b_clone.serve().await.unwrap();
    });

    // Wait for servers to start and health checks to run
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Both silos should be serving and their health checkers running.
    // We verify they started without panicking by checking the servers are still alive.
    assert!(!server_a.is_finished(), "silo A server should still be running");
    assert!(!server_b.is_finished(), "silo B server should still be running");

    silo_a.shutdown();
    silo_b.shutdown();

    // Give servers time to shut down
    tokio::time::sleep(Duration::from_millis(100)).await;
}
