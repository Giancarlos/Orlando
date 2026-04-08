use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, Message};
use orlando_persistence::{
    InMemoryJournalStore, InMemoryStateStore, JournalStore, JournaledGrain, JournaledGrainRef,
    JournaledHandler, PersistentSilo,
};

// --- Events ---

#[derive(Debug, Clone, Serialize, Deserialize)]
enum LedgerEvent {
    Credited(i64),
    Debited(i64),
}

// --- State ---

#[derive(Default, Debug, Serialize, Deserialize)]
struct LedgerState {
    balance: i64,
    event_count: u64,
}

// --- Grain ---

struct LedgerGrain;

#[async_trait]
impl Grain for LedgerGrain {
    type State = LedgerState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

#[async_trait]
impl JournaledGrain for LedgerGrain {
    type Event = LedgerEvent;

    fn apply(state: &mut LedgerState, event: &LedgerEvent) {
        match event {
            LedgerEvent::Credited(amount) => state.balance += amount,
            LedgerEvent::Debited(amount) => state.balance -= amount,
        }
        state.event_count += 1;
    }

    fn snapshot_interval() -> u64 {
        5
    }
}

// --- Messages ---

struct Credit {
    amount: i64,
}
impl Message for Credit {
    type Result = i64; // returns new balance
}

struct Debit {
    amount: i64,
}
impl Message for Debit {
    type Result = i64; // returns new balance
}

struct GetBalance;
impl Message for GetBalance {
    type Result = i64;
}

struct GetEventCount;
impl Message for GetEventCount {
    type Result = u64;
}

// --- Handlers ---

#[async_trait]
impl JournaledHandler<Credit> for LedgerGrain {
    async fn handle(
        state: &LedgerState,
        msg: Credit,
        _ctx: &GrainContext,
    ) -> (i64, Vec<LedgerEvent>) {
        let new_balance = state.balance + msg.amount;
        (new_balance, vec![LedgerEvent::Credited(msg.amount)])
    }
}

#[async_trait]
impl JournaledHandler<Debit> for LedgerGrain {
    async fn handle(
        state: &LedgerState,
        msg: Debit,
        _ctx: &GrainContext,
    ) -> (i64, Vec<LedgerEvent>) {
        let new_balance = state.balance - msg.amount;
        (new_balance, vec![LedgerEvent::Debited(msg.amount)])
    }
}

#[async_trait]
impl JournaledHandler<GetBalance> for LedgerGrain {
    async fn handle(
        state: &LedgerState,
        _msg: GetBalance,
        _ctx: &GrainContext,
    ) -> (i64, Vec<LedgerEvent>) {
        // Read-only: no events produced
        (state.balance, vec![])
    }
}

#[async_trait]
impl JournaledHandler<GetEventCount> for LedgerGrain {
    async fn handle(
        state: &LedgerState,
        _msg: GetEventCount,
        _ctx: &GrainContext,
    ) -> (u64, Vec<LedgerEvent>) {
        (state.event_count, vec![])
    }
}

// --- Helper ---

fn build_silo_and_ref(
    journal: Arc<dyn JournalStore>,
) -> (PersistentSilo, JournaledGrainRef<LedgerGrain>) {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();
    let ledger = silo.journaled_get_ref::<LedgerGrain>("ledger-1", journal);
    (silo, ledger)
}

// --- Tests ---

#[tokio::test]
async fn journaled_grain_accumulates_events() {
    let journal: Arc<dyn JournalStore> = Arc::new(InMemoryJournalStore::new());
    let (_silo, ledger) = build_silo_and_ref(journal.clone());

    ledger.ask(Credit { amount: 100 }).await.unwrap();
    ledger.ask(Credit { amount: 50 }).await.unwrap();
    ledger.ask(Debit { amount: 30 }).await.unwrap();

    let balance = ledger.ask(GetBalance).await.unwrap();
    assert_eq!(balance, 120);

    let event_count = ledger.ask(GetEventCount).await.unwrap();
    assert_eq!(event_count, 3);

    // Verify events are actually in the journal
    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<LedgerGrain>(),
        key: "ledger-1".to_string(),
    };
    let entries = journal.load_events(&grain_id).await.unwrap();
    assert_eq!(entries.len(), 3);
}

#[tokio::test]
async fn state_restored_from_journal_replay() {
    let journal: Arc<dyn JournalStore> = Arc::new(InMemoryJournalStore::new());

    // First silo: credit 100, debit 25, then let grain idle-deactivate
    {
        let (_silo, ledger) = build_silo_and_ref(journal.clone());
        ledger.ask(Credit { amount: 100 }).await.unwrap();
        ledger.ask(Debit { amount: 25 }).await.unwrap();

        let balance = ledger.ask(GetBalance).await.unwrap();
        assert_eq!(balance, 75);

        // Wait for idle timeout + deactivation
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Second silo: state should be restored from journal replay
    {
        let (_silo, ledger) = build_silo_and_ref(journal.clone());
        let balance = ledger.ask(GetBalance).await.unwrap();
        assert_eq!(balance, 75);

        let event_count = ledger.ask(GetEventCount).await.unwrap();
        assert_eq!(event_count, 2);
    }
}

#[tokio::test]
async fn snapshot_created_after_interval() {
    let journal: Arc<dyn JournalStore> = Arc::new(InMemoryJournalStore::new());
    let (_silo, ledger) = build_silo_and_ref(journal.clone());

    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<LedgerGrain>(),
        key: "ledger-1".to_string(),
    };

    // Snapshot interval is 5. Send 5 credits to trigger it.
    for i in 1..=5 {
        ledger.ask(Credit { amount: i }).await.unwrap();
    }

    // Balance should be 1+2+3+4+5 = 15
    let balance = ledger.ask(GetBalance).await.unwrap();
    assert_eq!(balance, 15);

    // Snapshot should exist now
    let snapshot = journal.load_snapshot(&grain_id).await.unwrap();
    assert!(snapshot.is_some(), "snapshot should exist after 5 events");
    let (seq, _bytes) = snapshot.unwrap();
    assert_eq!(seq, 5);

    // Send 3 more events (below snapshot interval, no new snapshot)
    for i in 1..=3 {
        ledger.ask(Credit { amount: i * 10 }).await.unwrap();
    }

    // Balance: 15 + 10 + 20 + 30 = 75
    let balance = ledger.ask(GetBalance).await.unwrap();
    assert_eq!(balance, 75);

    // Wait for deactivation
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Reactivate — should load from snapshot at seq 5, then replay events 6-8
    let (_silo2, ledger2) = build_silo_and_ref(journal.clone());
    let balance = ledger2.ask(GetBalance).await.unwrap();
    assert_eq!(balance, 75);
}

#[tokio::test]
async fn read_only_handler_produces_no_events() {
    let journal: Arc<dyn JournalStore> = Arc::new(InMemoryJournalStore::new());
    let (_silo, ledger) = build_silo_and_ref(journal.clone());

    // Only read operations
    let balance = ledger.ask(GetBalance).await.unwrap();
    assert_eq!(balance, 0);
    let balance = ledger.ask(GetBalance).await.unwrap();
    assert_eq!(balance, 0);

    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<LedgerGrain>(),
        key: "ledger-1".to_string(),
    };
    let entries = journal.load_events(&grain_id).await.unwrap();
    assert_eq!(entries.len(), 0, "read-only handlers should not produce events");
}
