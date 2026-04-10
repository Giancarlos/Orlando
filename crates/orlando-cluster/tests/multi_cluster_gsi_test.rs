use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_cluster::{
    ClusterSilo, CrossClusterDirectory, InMemoryCrossClusterDirectory, NetworkMessage,
};
use orlando_core::{ClusterId, Grain, GrainActivator, GrainContext, GrainHandler, Message};

// ── Test grain ──────────────────────────────────────────────────

struct Counter;

#[derive(Default)]
struct CounterState {
    count: i64,
}

impl Grain for Counter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_secs(5)
    }
}

#[derive(Serialize, Deserialize)]
struct Increment {
    amount: i64,
}

impl Message for Increment {
    type Result = i64;
}

impl NetworkMessage for Increment {
    fn message_type_name() -> &'static str {
        "Increment"
    }
}

#[derive(Serialize, Deserialize)]
struct GetCount;

impl Message for GetCount {
    type Result = i64;
}

impl NetworkMessage for GetCount {
    fn message_type_name() -> &'static str {
        "GetCount"
    }
}

#[async_trait]
impl GrainHandler<Increment> for Counter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for Counter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// ── Helper to bind a port ───────────────────────────────────────

async fn pick_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// ── Tests ───────────────────────────────────────────────────────

/// Grain call from cluster A dispatches locally (no forwarding).
#[tokio::test]
async fn grain_call_dispatches_locally_on_owning_cluster() {
    let shared_dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let port_a = pick_port().await;

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a1")
            .cluster_id("cluster-a")
            .cross_cluster_directory(shared_dir.clone())
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_a_clone = silo_a.clone();
    let _server_a = tokio::spawn(async move {
        silo_a_clone.serve().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Local call: cluster A is the only cluster, so it should own the grain
    let grain = silo_a.get_ref::<Counter>("my-counter");
    let result = grain.ask(Increment { amount: 5 }).await.unwrap();
    assert_eq!(result, 5);

    let count = grain.ask(GetCount).await.unwrap();
    assert_eq!(count, 5);

    // The grain should be active on silo A's directory
    assert_eq!(silo_a.directory().grain_ids().len(), 1);

    silo_a.shutdown();
}

/// Grain call from cluster B forwards to cluster A (the owner).
#[tokio::test]
async fn grain_call_from_non_owner_forwards_to_owning_cluster() {
    let shared_dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let port_a = pick_port().await;
    let port_b = pick_port().await;

    let cluster_a_id: ClusterId = "cluster-a".into();
    let cluster_b_id: ClusterId = "cluster-b".into();

    // Cluster A: will own grains via first-writer-wins CAS
    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a1")
            .cluster_id(cluster_a_id.clone())
            .cross_cluster_directory(shared_dir.clone())
            .peer_cluster(cluster_b_id.clone(), format!("127.0.0.1:{}", port_b))
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // Cluster B: will forward to cluster A
    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b1")
            .cluster_id(cluster_b_id.clone())
            .cross_cluster_directory(shared_dir.clone())
            .peer_cluster(cluster_a_id.clone(), format!("127.0.0.1:{}", port_a))
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // Start both servers
    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Step 1: Activate the grain on cluster A via the gRPC transport (raw invoke).
    // This ensures cluster A registers ownership in the cross-cluster directory.
    {
        use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
        use orlando_cluster::proto::InvokeRequest;

        let grain_type = std::any::type_name::<Counter>();
        let payload = bincode::serde::encode_to_vec(
            &Increment { amount: 10 },
            bincode::config::standard(),
        )
        .unwrap();

        let mut client_a =
            GrainTransportClient::connect(format!("http://127.0.0.1:{}", port_a))
                .await
                .unwrap();

        let resp = client_a
            .invoke(InvokeRequest {
                grain_type: grain_type.to_string(),
                grain_key: "shared-counter".to_string(),
                message_type: "Increment".to_string(),
                payload,
                encoding: 0,
                request_context: std::collections::HashMap::new(),
                message_version: 0,
            })
            .await
            .unwrap();

        let inner = resp.into_inner();
        assert!(inner.error.is_empty(), "cluster A invoke should succeed: {}", inner.error);
    }

    // Step 2: Now call from cluster B. The cross-cluster directory shows cluster A
    // owns the grain, so B should forward the request to A via the gateway.
    {
        use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
        use orlando_cluster::proto::InvokeRequest;

        let grain_type = std::any::type_name::<Counter>();
        let payload = bincode::serde::encode_to_vec(
            &Increment { amount: 7 },
            bincode::config::standard(),
        )
        .unwrap();

        let mut client_b =
            GrainTransportClient::connect(format!("http://127.0.0.1:{}", port_b))
                .await
                .unwrap();

        let resp = client_b
            .invoke(InvokeRequest {
                grain_type: grain_type.to_string(),
                grain_key: "shared-counter".to_string(),
                message_type: "Increment".to_string(),
                payload,
                encoding: 0,
                request_context: std::collections::HashMap::new(),
                message_version: 0,
            })
            .await
            .unwrap();

        let inner = resp.into_inner();
        assert!(
            inner.error.is_empty(),
            "cluster B forward invoke should succeed: {}",
            inner.error
        );

        // Deserialize and verify: 10 + 7 = 17
        let (result, _): (i64, _) = bincode::serde::decode_from_slice(
            &inner.payload,
            bincode::config::standard(),
        )
        .unwrap();
        assert_eq!(result, 17, "forwarded call should accumulate on cluster A's grain");
    }

    // The grain should only exist on cluster A, NOT on cluster B
    assert_eq!(silo_a.directory().grain_ids().len(), 1);
    assert_eq!(
        silo_b.directory().grain_ids().len(),
        0,
        "cluster B should not have activated the grain locally"
    );

    silo_a.shutdown();
    silo_b.shutdown();
}

/// After cluster A deregisters, cluster B can register and own the grain.
#[tokio::test]
async fn deregister_allows_new_owner() {
    let shared_dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let port_a = pick_port().await;
    let port_b = pick_port().await;

    let cluster_a_id: ClusterId = "cluster-a".into();
    let cluster_b_id: ClusterId = "cluster-b".into();

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a1")
            .cluster_id(cluster_a_id.clone())
            .cross_cluster_directory(shared_dir.clone())
            .peer_cluster(cluster_b_id.clone(), format!("127.0.0.1:{}", port_b))
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b1")
            .cluster_id(cluster_b_id.clone())
            .cross_cluster_directory(shared_dir.clone())
            .peer_cluster(cluster_a_id.clone(), format!("127.0.0.1:{}", port_a))
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Register grain on cluster A via raw invoke
    {
        use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
        use orlando_cluster::proto::InvokeRequest;

        let grain_type = std::any::type_name::<Counter>();
        let payload = bincode::serde::encode_to_vec(
            &Increment { amount: 1 },
            bincode::config::standard(),
        )
        .unwrap();

        let mut client =
            GrainTransportClient::connect(format!("http://127.0.0.1:{}", port_a))
                .await
                .unwrap();

        client
            .invoke(InvokeRequest {
                grain_type: grain_type.to_string(),
                grain_key: "handoff-counter".to_string(),
                message_type: "Increment".to_string(),
                payload,
                encoding: 0,
                request_context: std::collections::HashMap::new(),
                message_version: 0,
            })
            .await
            .unwrap();
    }

    // Verify cluster A owns it
    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<Counter>(),
        key: "handoff-counter".to_string(),
    };
    let ownership = shared_dir.lookup(&grain_id).await.unwrap();
    assert!(ownership.is_some(), "grain should be registered");
    assert_eq!(ownership.unwrap().cluster_id, cluster_a_id);

    // Deregister cluster A's ownership
    shared_dir.deregister(&grain_id, &cluster_a_id).await.unwrap();

    // Verify it's gone
    let ownership = shared_dir.lookup(&grain_id).await.unwrap();
    assert!(ownership.is_none(), "grain should be deregistered");

    // Now invoke from cluster B. Since no one owns it, B should register itself.
    {
        use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
        use orlando_cluster::proto::InvokeRequest;

        let grain_type = std::any::type_name::<Counter>();
        let payload = bincode::serde::encode_to_vec(
            &Increment { amount: 100 },
            bincode::config::standard(),
        )
        .unwrap();

        let mut client =
            GrainTransportClient::connect(format!("http://127.0.0.1:{}", port_b))
                .await
                .unwrap();

        let resp = client
            .invoke(InvokeRequest {
                grain_type: grain_type.to_string(),
                grain_key: "handoff-counter".to_string(),
                message_type: "Increment".to_string(),
                payload,
                encoding: 0,
                request_context: std::collections::HashMap::new(),
                message_version: 0,
            })
            .await
            .unwrap();

        let inner = resp.into_inner();
        assert!(inner.error.is_empty(), "cluster B should now dispatch locally: {}", inner.error);

        // Result should be 100 (fresh activation on B, not carrying state from A)
        let (result, _): (i64, _) = bincode::serde::decode_from_slice(
            &inner.payload,
            bincode::config::standard(),
        )
        .unwrap();
        assert_eq!(result, 100, "grain re-activated on cluster B with fresh state");
    }

    // Verify cluster B now owns it
    let ownership = shared_dir.lookup(&grain_id).await.unwrap();
    assert!(ownership.is_some(), "grain should be re-registered");
    assert_eq!(ownership.unwrap().cluster_id, cluster_b_id);

    // And it should be active on B, not A (A's activation is separate/stale)
    assert!(
        silo_b.directory().grain_ids().len() >= 1,
        "cluster B should have the grain active"
    );

    silo_a.shutdown();
    silo_b.shutdown();
}

/// The ClusterPing gRPC endpoint responds correctly.
#[tokio::test]
async fn cluster_ping_responds() {
    let shared_dir = Arc::new(InMemoryCrossClusterDirectory::new());
    let port = pick_port().await;

    let silo = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port)
            .silo_id("silo-1")
            .cluster_id("test-cluster")
            .cross_cluster_directory(shared_dir)
            .register::<Counter, Increment>()
            .build(),
    );

    let silo_clone = silo.clone();
    let _server = tokio::spawn(async move { silo_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    use orlando_cluster::proto::cluster_gateway_client::ClusterGatewayClient;
    use orlando_cluster::proto::ClusterPingRequest;

    let mut client =
        ClusterGatewayClient::connect(format!("http://127.0.0.1:{}", port))
            .await
            .unwrap();

    let resp = client
        .cluster_ping(ClusterPingRequest {
            cluster_id: "remote-cluster".to_string(),
        })
        .await
        .unwrap();

    let inner = resp.into_inner();
    assert_eq!(inner.cluster_id, "test-cluster");

    silo.shutdown();
}

/// Single-cluster mode (no cross-cluster config) behaves exactly as before.
#[tokio::test]
async fn single_cluster_mode_unchanged() {
    // No cross_cluster_directory, no cluster_id, no peer_cluster
    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("solo-silo")
        .register::<Counter, Increment>()
        .register::<Counter, GetCount>()
        .build();

    let grain = silo.get_ref::<Counter>("solo-counter");
    let result = grain.ask(Increment { amount: 42 }).await.unwrap();
    assert_eq!(result, 42);

    let count = grain.ask(GetCount).await.unwrap();
    assert_eq!(count, 42);
}
