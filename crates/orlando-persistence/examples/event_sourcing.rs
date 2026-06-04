//! Event sourcing example (`JournaledGrain`).
//!
//! Instead of mutating state in place, a journaled grain's handlers return
//! *events*. Events are appended to a journal and applied via `apply()`. On
//! (re)activation the journal is replayed to reconstruct state — so state
//! survives deactivation without a separate "save" step, and you keep a full
//! audit trail. Periodic snapshots bound replay cost.
//!
//! Run with: `cargo run -p orlando-persistence --example event_sourcing`

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, Message};
use orlando_persistence::{
    InMemoryJournalStore, JournalStore, JournaledGrain, JournaledHandler, PersistentSilo,
};

// ── Events: the source of truth ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
enum LedgerEvent {
    Credited(i64),
    Debited(i64),
}

// ── State: a fold over the events ───────────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct LedgerState {
    balance: i64,
    event_count: u64,
}

struct Ledger;

#[async_trait]
impl Grain for Ledger {
    type State = LedgerState;
    fn idle_timeout() -> Duration {
        // Short, so the grain deactivates quickly and we can show replay.
        Duration::from_millis(50)
    }
}

#[async_trait]
impl JournaledGrain for Ledger {
    type Event = LedgerEvent;

    // Pure: rebuilds state from events (also used during replay).
    fn apply(state: &mut LedgerState, event: &LedgerEvent) {
        match event {
            LedgerEvent::Credited(a) => state.balance += a,
            LedgerEvent::Debited(a) => state.balance -= a,
        }
        state.event_count += 1;
    }

    fn snapshot_interval() -> u64 {
        100
    }
}

// ── Messages ────────────────────────────────────────────────────

struct Credit(i64);
impl Message for Credit {
    type Result = i64;
}
struct Debit(i64);
impl Message for Debit {
    type Result = i64;
}
struct Balance;
impl Message for Balance {
    type Result = (i64, u64); // (balance, events applied)
}

// ── Handlers: return events, never mutate state directly ────────

#[async_trait]
impl JournaledHandler<Credit> for Ledger {
    async fn handle(state: &LedgerState, msg: Credit, _ctx: &GrainContext) -> (i64, Vec<LedgerEvent>) {
        (state.balance + msg.0, vec![LedgerEvent::Credited(msg.0)])
    }
}

#[async_trait]
impl JournaledHandler<Debit> for Ledger {
    async fn handle(state: &LedgerState, msg: Debit, _ctx: &GrainContext) -> (i64, Vec<LedgerEvent>) {
        (state.balance - msg.0, vec![LedgerEvent::Debited(msg.0)])
    }
}

#[async_trait]
impl JournaledHandler<Balance> for Ledger {
    async fn handle(state: &LedgerState, _msg: Balance, _ctx: &GrainContext) -> ((i64, u64), Vec<LedgerEvent>) {
        // Read-only: produces no events.
        ((state.balance, state.event_count), vec![])
    }
}

#[tokio::main]
async fn main() {
    let journal: Arc<dyn JournalStore> = Arc::new(InMemoryJournalStore::new());
    let silo = PersistentSilo::builder().store(orlando_persistence::InMemoryStateStore::new()).build();

    let ledger = silo.journaled_get_ref::<Ledger>("acct-42", journal.clone());

    println!("credit 100 -> balance {}", ledger.ask(Credit(100)).await.unwrap());
    println!("credit 50  -> balance {}", ledger.ask(Credit(50)).await.unwrap());
    println!("debit 30   -> balance {}", ledger.ask(Debit(30)).await.unwrap());

    let (balance, events) = ledger.ask(Balance).await.unwrap();
    println!("balance = {balance}, events applied = {events}");
    assert_eq!(balance, 120);

    // Let the grain deactivate (idle timeout). A held ref caches the mailbox
    // sender, which closes on deactivation — so re-acquire the handle by
    // identity. That reactivates the grain, replaying the journal to rebuild
    // state from scratch.
    println!("\n...idling until deactivation, then reactivating...");
    drop(ledger);
    tokio::time::sleep(Duration::from_millis(120)).await;

    let ledger = silo.journaled_get_ref::<Ledger>("acct-42", journal.clone());
    let (balance, events) = ledger.ask(Balance).await.unwrap();
    println!("after replay: balance = {balance}, events applied = {events}");
    assert_eq!(balance, 120, "balance rebuilt from the journal");
    println!("state reconstructed from the event journal ✓");
}
