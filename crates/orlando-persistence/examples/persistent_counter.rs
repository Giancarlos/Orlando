//! Persistent counter grain using SQLite.
//!
//! Demonstrates state surviving grain deactivation. The grain is activated,
//! incremented, deactivated (idle timeout), and re-activated — its count
//! is restored from SQLite.
//!
//! Run with: cargo run -p orlando-persistence --example persistent_counter

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_persistence::{PersistentGrain, PersistentSilo, SqliteStateStore};

// ── State ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct CounterState {
    count: i64,
}

// ── Grain ────────────────────────────────────────────────────────

struct PersistentCounter;

#[async_trait]
impl Grain for PersistentCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(500) // fast timeout for demo
    }
}

impl PersistentGrain for PersistentCounter {}

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
impl GrainHandler<Increment> for PersistentCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for PersistentCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// ── Main ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let store = SqliteStateStore::new("sqlite::memory:").await.unwrap();
    let silo = PersistentSilo::builder().store(store).build();

    // First activation: increment the counter
    println!("Activating grain and incrementing...");
    let counter = silo.persistent_get_ref::<PersistentCounter>("demo");
    let count = counter.ask(Increment { amount: 10 }).await.unwrap();
    println!("  count = {count}");

    let count = counter.ask(Increment { amount: 5 }).await.unwrap();
    println!("  count = {count}");

    // Wait for idle timeout to trigger deactivation (saves state to SQLite)
    println!("Waiting for idle deactivation (500ms)...");
    tokio::time::sleep(Duration::from_millis(800)).await;
    println!("  grain deactivated");

    // Re-activate: state should be restored from SQLite
    println!("Re-activating grain...");
    let counter = silo.persistent_get_ref::<PersistentCounter>("demo");
    let count = counter.ask(GetCount).await.unwrap();
    println!("  count = {count} (restored from SQLite!)");

    assert_eq!(count, 15, "state should survive deactivation");
    println!();
    println!("Done! State persisted through deactivation and reactivation.");
}
