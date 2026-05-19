//! Persistent counter grain using PostgreSQL.
//!
//! Demonstrates grain state surviving deactivation with a Postgres backend.
//!
//! Prerequisites:
//!   docker run -d --name orlando-pg -e POSTGRES_PASSWORD=test -e POSTGRES_DB=orlando_test -p 5432:5432 postgres:16
//!
//! Run with:
//!   cargo run -p orlando-persistence --features postgres --example postgres_counter

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_persistence::{PersistentGrain, PersistentSilo, PostgresStateStore};

// ── State ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct CounterState {
    count: i64,
}

// ── Grain ────────────────────────────────────────────────────────

struct PgCounter;

#[async_trait]
impl Grain for PgCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(500)
    }

    fn grain_type_name() -> &'static str {
        "PgCounter"
    }
}

impl PersistentGrain for PgCounter {}

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
impl GrainHandler<Increment> for PgCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for PgCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// ── Main ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let url = std::env::var("POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:test@localhost/orlando_test".to_string());

    let store = PostgresStateStore::new(&url).await.unwrap();
    let silo = PersistentSilo::builder().store(store).build();

    // First activation
    println!("Activating grain and incrementing...");
    let counter = silo.persistent_get_ref::<PgCounter>("demo");
    let count = counter.ask(Increment { amount: 10 }).await.unwrap();
    println!("  count = {count}");

    let count = counter.ask(Increment { amount: 5 }).await.unwrap();
    println!("  count = {count}");

    // Wait for idle deactivation (saves state to Postgres)
    println!("Waiting for idle deactivation (500ms)...");
    tokio::time::sleep(Duration::from_millis(800)).await;
    println!("  grain deactivated");

    // Re-activate: state restored from Postgres
    println!("Re-activating grain...");
    let counter = silo.persistent_get_ref::<PgCounter>("demo");
    let count = counter.ask(GetCount).await.unwrap();
    println!("  count = {count} (restored from PostgreSQL!)");

    assert_eq!(count, 15, "state should survive deactivation");
    println!();
    println!("Done! State persisted through deactivation and reactivation.");
}
