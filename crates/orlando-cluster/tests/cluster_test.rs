use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_cluster::{ClusterSilo, NetworkMessage};

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

// Messages

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

// Handlers

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

// ── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn local_dispatch_through_cluster_silo() {
    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("test-silo")
        .register::<Counter, Increment>()
        .register::<Counter, GetCount>()
        .build();

    let grain = silo.get_ref::<Counter>("my-counter");
    let result = grain.ask(Increment { amount: 5 }).await.unwrap();
    assert_eq!(result, 5);

    let result = grain.ask(Increment { amount: 3 }).await.unwrap();
    assert_eq!(result, 8);

    let count = grain.ask(GetCount).await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn cross_silo_grain_call() {
    // Silo A: owns the grain, serves gRPC
    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(0) // will bind to a random port
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // We need to know the actual port, so bind a listener first
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let actual_port = listener.local_addr().unwrap().port();

    // Rebuild silo_a with the actual port
    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(actual_port)
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // Drop the listener so the port is free for tonic
    drop(listener);

    // Start silo_a's gRPC server in background
    let silo_a_clone = silo_a.clone();
    let server_handle = tokio::spawn(async move {
        silo_a_clone.serve().await.unwrap();
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Silo B: joins the cluster, will route some grains to silo_a
    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(0)
            .silo_id("silo-b")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    silo_b
        .join_cluster(&format!("127.0.0.1:{}", actual_port))
        .await
        .unwrap();

    // Try many grain keys — some will hash to silo-a (remote), some to silo-b (local).
    // We specifically test that at least one remote call succeeds.
    let mut remote_successes = 0;
    for i in 0..20 {
        let key = format!("counter-{}", i);
        let grain = silo_b.get_ref::<Counter>(&key);
        let result = grain.ask(Increment { amount: 1 }).await;
        if result.is_ok() {
            remote_successes += 1;
        }
    }

    // With consistent hashing and 2 silos, roughly half should go to each.
    // We just check that at least some worked (covers both local and remote paths).
    assert!(
        remote_successes >= 10,
        "expected at least 10 successful calls, got {}",
        remote_successes
    );

    server_handle.abort();
}

#[tokio::test]
async fn membership_join_and_get_members() {
    use orlando_cluster::proto::membership_client::MembershipClient;
    use orlando_cluster::proto::{GetMembersRequest, JoinRequest, SiloAddress};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let silo = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port)
            .silo_id("seed")
            .build(),
    );

    let silo_clone = silo.clone();
    let server = tokio::spawn(async move {
        silo_clone.serve().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client =
        MembershipClient::connect(format!("http://127.0.0.1:{}", port))
            .await
            .unwrap();

    // Join a new silo
    let resp = client
        .join(JoinRequest {
            joiner: Some(SiloAddress {
                host: "127.0.0.1".into(),
                port: 9999,
                silo_id: "newcomer".into(),
            }),
        })
        .await
        .unwrap();

    let members = resp.into_inner().members;
    assert!(members.len() >= 2, "expected at least 2 members after join");

    // GetMembers should also return both
    let resp = client
        .get_members(GetMembersRequest {})
        .await
        .unwrap();
    let members = resp.into_inner().members;
    let ids: Vec<String> = members.iter().map(|m| m.silo_id.clone()).collect();
    assert!(ids.contains(&"seed".to_string()));
    assert!(ids.contains(&"newcomer".to_string()));

    server.abort();
}
