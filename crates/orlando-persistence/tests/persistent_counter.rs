use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use std::time::Duration;

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistentGrain, PersistentSilo, SqliteStateStore, StateStore,
};

// --- State (must be Serialize + Deserialize for persistence) ---

#[derive(Default, Serialize, Deserialize)]
struct CounterState {
    count: i64,
}

// --- Grain ---

struct PersistentCounter;

#[async_trait]
impl Grain for PersistentCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

impl PersistentGrain for PersistentCounter {}

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

// --- Handlers ---

#[async_trait]
impl GrainHandler<Increment> for PersistentCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) {
        state.count += msg.amount;
    }
}

#[async_trait]
impl GrainHandler<GetCount> for PersistentCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// --- Tests ---

#[tokio::test]
async fn persistent_grain_increment_and_get() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let counter = silo.persistent_get_ref::<PersistentCounter>("pc-1");
    counter.ask(Increment { amount: 5 }).await.unwrap();
    counter.ask(Increment { amount: 3 }).await.unwrap();

    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn state_survives_deactivation() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // First silo: increment to 10, then let the grain idle-timeout and persist
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.persistent_get_ref::<PersistentCounter>("survive-1");

        counter.ask(Increment { amount: 10 }).await.unwrap();
        assert_eq!(counter.ask(GetCount).await.unwrap(), 10);

        // Wait for idle timeout (50ms) + deactivation/save to complete
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Second silo: state should be restored from the store
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.persistent_get_ref::<PersistentCounter>("survive-1");

        let count = counter.ask(GetCount).await.unwrap();
        assert_eq!(count, 10);
    }
}

#[tokio::test]
async fn separate_persistent_grains_have_separate_state() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let a = silo.persistent_get_ref::<PersistentCounter>("pa");
    let b = silo.persistent_get_ref::<PersistentCounter>("pb");

    a.ask(Increment { amount: 10 }).await.unwrap();
    b.ask(Increment { amount: 3 }).await.unwrap();

    assert_eq!(a.ask(GetCount).await.unwrap(), 10);
    assert_eq!(b.ask(GetCount).await.unwrap(), 3);
}

// --- SQLite tests ---

#[tokio::test]
async fn sqlite_state_survives_deactivation() {
    let store: Arc<dyn StateStore> = Arc::new(
        SqliteStateStore::new("sqlite::memory:")
            .await
            .expect("failed to create sqlite store"),
    );

    // First silo: increment to 42, then let idle timeout persist
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.persistent_get_ref::<PersistentCounter>("sql-1");

        counter.ask(Increment { amount: 42 }).await.unwrap();
        assert_eq!(counter.ask(GetCount).await.unwrap(), 42);

        // Wait for idle timeout (50ms) + deactivation/save
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Second silo with same store: state should be restored
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.persistent_get_ref::<PersistentCounter>("sql-1");

        let count = counter.ask(GetCount).await.unwrap();
        assert_eq!(count, 42);
    }
}
