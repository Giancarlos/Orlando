//! Persistent counter grain using Redis.
//!
//! Demonstrates grain state surviving deactivation with a Redis backend.
//!
//! Prerequisites:
//!   docker run -d --name orlando-redis -p 6379:6379 redis:7
//!
//! Run with:
//!   cargo run -p orlando-persistence --features redis --example redis_counter

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_persistence::{PersistentGrain, PersistentSilo, RedisStateStore};

// ── State ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct CounterState {
    count: i64,
}

// ── Grain ────────────────────────────────────────────────────────

struct RedisCounter;

#[async_trait]
impl Grain for RedisCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(500)
    }

    fn grain_type_name() -> &'static str {
        "RedisCounter"
    }
}

impl PersistentGrain for RedisCounter {}

// ── Messages ─────────────────────────────────────────────────────

struct Increment {
    amount: i64,
}

impl Message for Increment {
    type Result = i64;
}

struct GetCount;

impl Message for GetCount {
    type Result = i64;
}

// ── Handlers ─────────────────────────────────────────────────────

#[async_trait]
impl GrainHandler<Increment> for RedisCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for RedisCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// ── Main ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://localhost:6379".to_string());

    let store = RedisStateStore::new(&url).await.unwrap();
    let silo = PersistentSilo::builder().store(store).build();

    // First activation
    println!("Activating grain and incrementing...");
    let counter = silo.persistent_get_ref::<RedisCounter>("demo");
    let count = counter.ask(Increment { amount: 10 }).await.unwrap();
    println!("  count = {count}");

    let count = counter.ask(Increment { amount: 5 }).await.unwrap();
    println!("  count = {count}");

    // Wait for idle deactivation (saves state to Redis)
    println!("Waiting for idle deactivation (500ms)...");
    tokio::time::sleep(Duration::from_millis(800)).await;
    println!("  grain deactivated");

    // Re-activate: state restored from Redis
    println!("Re-activating grain...");
    let counter = silo.persistent_get_ref::<RedisCounter>("demo");
    let count = counter.ask(GetCount).await.unwrap();
    println!("  count = {count} (restored from Redis!)");

    assert_eq!(count, 15, "state should survive deactivation");
    println!();
    println!("Done! State persisted through deactivation and reactivation.");
}
