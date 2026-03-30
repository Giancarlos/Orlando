//! Multi-silo cluster example.
//!
//! Starts two silos in the same process and demonstrates cross-silo grain calls
//! with transparent routing via the consistent hash ring.
//!
//! Run with: cargo run -p orlando-cluster --example cluster

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_cluster::{ClusterSilo, NetworkMessage};
use orlando_core::{Grain, GrainContext, GrainHandler, Message};

// ── Grain ────────────────────────────────────────────────────────

#[derive(Default)]
struct CounterState {
    count: i64,
}

struct Counter;

impl Grain for Counter {
    type State = CounterState;
}

// ── Messages ─────────────────────────────────────────────────────

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

// ── Handlers ─────────────────────────────────────────────────────

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

// ── Main ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Bind ports up front so we know the addresses
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    // Build two silos
    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // Start gRPC servers
    let a_clone = silo_a.clone();
    tokio::spawn(async move { a_clone.serve().await.unwrap() });

    let b_clone = silo_b.clone();
    tokio::spawn(async move { b_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Form the cluster: B joins A
    silo_b
        .join_cluster(&format!("127.0.0.1:{}", port_a))
        .await
        .unwrap();

    println!("Cluster formed: silo-a (:{port_a}) + silo-b (:{port_b})");
    println!();

    // Send messages to several grains from silo B.
    // Some will be local to B, others will be routed to A via gRPC.
    for i in 0..6 {
        let key = format!("counter-{}", i);
        let grain = silo_b.get_ref::<Counter>(&key);
        let result = grain.ask(Increment { amount: i + 1 }).await.unwrap();
        println!("  {key}: increment by {} -> count = {result}", i + 1);
    }

    println!();

    // Read them back from silo A's perspective
    for i in 0..6 {
        let key = format!("counter-{}", i);
        let grain = silo_a.get_ref::<Counter>(&key);
        let count = grain.ask(GetCount).await.unwrap();
        println!("  {key}: count = {count} (via silo-a)");
    }

    println!();
    println!("Done! Grains are transparently distributed across silos.");
}
