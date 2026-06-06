use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use orlando_core::{Grain, GrainContext, GrainHandler, GrainId, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistentGrain, PersistentSilo, PersistenceStrategy,
};

// --- Shared event log for tracking callback order ---

type EventLog = Arc<Mutex<Vec<String>>>;

// --- State ---

#[derive(Default, Serialize, Deserialize)]
struct CounterState {
    count: i64,
}

// --- Messages ---

struct Increment {
    amount: i64,
}

impl Message for Increment {
    type Result = ();
}

struct GetCount;

impl Message for GetCount {
    type Result = i64;
}

// --- CallbackCounter: grain with persistence callbacks (uses a global log) ---

static EVENT_LOG: std::sync::OnceLock<EventLog> = std::sync::OnceLock::new();

fn event_log() -> EventLog {
    EVENT_LOG
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone()
}

struct CallbackCounter;

#[async_trait]
impl Grain for CallbackCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

#[async_trait]
impl PersistentGrain for CallbackCounter {
    async fn on_before_load(_grain_id: &GrainId) {
        event_log().lock().await.push("before_load".to_string());
    }

    async fn on_after_load(_grain_id: &GrainId) {
        event_log().lock().await.push("after_load".to_string());
    }

    async fn on_before_save(_grain_id: &GrainId) {
        event_log().lock().await.push("before_save".to_string());
    }

    async fn on_after_save(_grain_id: &GrainId) {
        event_log().lock().await.push("after_save".to_string());
    }

    async fn on_before_clear(_grain_id: &GrainId) {
        event_log().lock().await.push("before_clear".to_string());
    }

    async fn on_after_clear(_grain_id: &GrainId) {
        event_log().lock().await.push("after_clear".to_string());
    }
}

#[async_trait]
impl GrainHandler<Increment> for CallbackCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) {
        state.count += msg.amount;
    }
}

#[async_trait]
impl GrainHandler<GetCount> for CallbackCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// --- PlainPersistent: grain with default (no-op) callbacks ---

struct PlainPersistent;

#[async_trait]
impl Grain for PlainPersistent {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

impl PersistentGrain for PlainPersistent {}

#[async_trait]
impl GrainHandler<Increment> for PlainPersistent {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) {
        state.count += msg.amount;
    }
}

#[async_trait]
impl GrainHandler<GetCount> for PlainPersistent {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// --- Tests ---

/// Combined test to avoid global event log contention between parallel tests.
/// Tests WriteOnDeactivate callbacks, WriteThrough callbacks, and no-op defaults.
#[tokio::test]
async fn persistence_lifecycle_callbacks() {
    // --- Part 1: WriteOnDeactivate (explicit opt-in; WriteThrough is now the default) ---
    {
        event_log().lock().await.clear();

        let store = InMemoryStateStore::new();
        let silo = PersistentSilo::builder().store(store).build();

        let counter = silo.persistent_get_ref_with_strategy::<CallbackCounter>(
            "cb-1",
            PersistenceStrategy::WriteOnDeactivate,
        );
        counter.ask(Increment { amount: 5 }).await.unwrap();
        let count = counter.ask(GetCount).await.unwrap();
        assert_eq!(count, 5);

        // Wait for idle timeout to trigger deactivation + save
        tokio::time::sleep(Duration::from_millis(200)).await;

        let events = event_log().lock().await.clone();
        assert_eq!(
            events,
            vec!["before_load", "after_load", "before_save", "after_save"],
            "WriteOnDeactivate: callbacks should fire in order: before_load -> after_load -> before_save -> after_save"
        );
    }

    // --- Part 2: WriteThrough fires save callbacks after each message ---
    {
        event_log().lock().await.clear();

        let store = InMemoryStateStore::new();
        let silo = PersistentSilo::builder().store(store).build();

        let counter = silo.persistent_get_ref_with_strategy::<CallbackCounter>(
            "cb-wt",
            PersistenceStrategy::WriteThrough,
        );

        counter.ask(Increment { amount: 1 }).await.unwrap();
        counter.ask(Increment { amount: 2 }).await.unwrap();

        // Wait for idle timeout to trigger deactivation
        tokio::time::sleep(Duration::from_millis(200)).await;

        let events = event_log().lock().await.clone();
        // Load callbacks, then save per message (write-through), then deactivation save.
        assert_eq!(
            events,
            vec![
                "before_load",
                "after_load",
                "before_save", // write-through msg 1
                "after_save",
                "before_save", // write-through msg 2
                "after_save",
                "before_save", // deactivation save
                "after_save",
            ],
            "WriteThrough: should fire save callbacks after each message plus deactivation"
        );
    }

    // --- Part 3: Default no-op callbacks do not break existing grains ---
    {
        let store = InMemoryStateStore::new();
        let silo = PersistentSilo::builder().store(store).build();

        let counter = silo.persistent_get_ref::<PlainPersistent>("plain-1");
        counter.ask(Increment { amount: 42 }).await.unwrap();
        let count = counter.ask(GetCount).await.unwrap();
        assert_eq!(count, 42, "PlainPersistent with no-op callbacks should work normally");
    }
}
