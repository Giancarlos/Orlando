use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_persistence::{InMemoryStateStore, PersistentGrain, PersistentSilo, StateStore};

// --- State ---

#[derive(Default, Serialize, Deserialize)]
struct CounterState {
    count: i64,
}

// --- Grain: reentrant + persistent ---

struct ReentrantPersistentCounter;

#[async_trait]
impl Grain for ReentrantPersistentCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }

    fn reentrant() -> bool {
        true
    }
}

impl PersistentGrain for ReentrantPersistentCounter {}

// --- Messages ---

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

// --- Handlers ---

#[async_trait]
impl GrainHandler<Increment> for ReentrantPersistentCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for ReentrantPersistentCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// --- Tests ---

#[tokio::test]
async fn reentrant_persistent_increment_and_get() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let counter = silo.persistent_get_ref::<ReentrantPersistentCounter>("rp-1");
    counter.ask(Increment { amount: 5 }).await.unwrap();
    counter.ask(Increment { amount: 3 }).await.unwrap();

    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn reentrant_persistent_state_survives_deactivation() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // First silo: increment, then let grain idle-deactivate (saves state)
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.persistent_get_ref::<ReentrantPersistentCounter>("rp-survive");
        counter.ask(Increment { amount: 42 }).await.unwrap();

        // Wait for idle timeout (50ms) + deactivation
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Second silo: state should be restored from store
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.persistent_get_ref::<ReentrantPersistentCounter>("rp-survive");
        let count = counter.ask(GetCount).await.unwrap();
        assert_eq!(count, 42, "state should survive deactivation with reentrant+persistent");
    }
}

#[tokio::test]
async fn reentrant_persistent_concurrent_increments() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let counter = silo.persistent_get_ref::<ReentrantPersistentCounter>("rp-concurrent");

    // Send 50 concurrent increments
    let mut handles = Vec::new();
    for _ in 0..50 {
        let c = counter.clone();
        handles.push(tokio::spawn(async move {
            c.ask(Increment { amount: 1 }).await.unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 50, "all 50 concurrent increments must be applied");
}
