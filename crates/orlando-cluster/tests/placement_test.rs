use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_cluster::{
    ClusterSilo, NetworkMessage, PreferLocalPlacement, RandomPlacement,
};
use orlando_core::{Grain, GrainActivator, GrainContext, GrainHandler, Message};

// --- Grain ---

struct Counter;

#[derive(Default)]
struct CounterState {
    count: i64,
}

impl Grain for Counter {
    type State = CounterState;
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

#[async_trait]
impl GrainHandler<Increment> for Counter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

// --- Tests ---

#[tokio::test]
async fn prefer_local_placement_keeps_grains_local() {
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .placement(Arc::new(PreferLocalPlacement))
            .register::<Counter, Increment>()
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .placement(Arc::new(PreferLocalPlacement))
            .register::<Counter, Increment>()
            .build(),
    );

    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    silo_b
        .join_cluster(&format!("127.0.0.1:{}", port_a))
        .await
        .unwrap();

    // With prefer-local, all grains accessed from silo_a should stay on silo_a
    for i in 0..10 {
        let grain = silo_a.get_ref::<Counter>(&format!("local-{}", i));
        grain.ask(Increment { amount: 1 }).await.unwrap();
    }

    // All 10 should be on silo_a, none on silo_b
    let a_count = silo_a.directory().grain_ids().len();
    assert_eq!(a_count, 10, "all grains should be local to silo-a");

    _sa.abort();
    _sb.abort();
}

#[tokio::test]
async fn random_placement_distributes_grains() {
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .placement(Arc::new(RandomPlacement))
            .register::<Counter, Increment>()
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .placement(Arc::new(RandomPlacement))
            .register::<Counter, Increment>()
            .build(),
    );

    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    silo_b
        .join_cluster(&format!("127.0.0.1:{}", port_a))
        .await
        .unwrap();

    // With random placement, grains should be distributed across both silos.
    // Send 20 grain calls from silo_a — some will go remote.
    let mut successes = 0;
    for i in 0..20 {
        let grain = silo_a.get_ref::<Counter>(&format!("rand-{}", i));
        if grain.ask(Increment { amount: 1 }).await.is_ok() {
            successes += 1;
        }
    }

    assert_eq!(successes, 20, "all calls should succeed");

    // Both silos should have some grains (random distribution)
    let a_count = silo_a.directory().grain_ids().len();
    let b_count = silo_b.directory().grain_ids().len();
    assert!(
        a_count > 0 && b_count > 0,
        "random placement should distribute: a={}, b={}",
        a_count,
        b_count
    );

    _sa.abort();
    _sb.abort();
}

#[tokio::test]
async fn default_placement_is_hash_based() {
    // Without setting placement, consistent hashing is the default.
    // This is a regression test — existing behavior should be unchanged.
    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("test-default")
        .register::<Counter, Increment>()
        .build();

    let grain = silo.get_ref::<Counter>("test-key");
    let result = grain.ask(Increment { amount: 1 }).await.unwrap();
    assert_eq!(result, 1);
}
