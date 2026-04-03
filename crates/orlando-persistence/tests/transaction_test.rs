use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, GrainId, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistenceError, PersistentGrain, PersistentSilo, StateStore,
    TransactionContext, TransactionalGrain, TransactionalHandler,
};

// --- State ---

#[derive(Default, Clone, Serialize, Deserialize)]
struct CounterState {
    count: i64,
}

// --- Grain ---

struct TxCounter;

#[async_trait]
impl Grain for TxCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

impl PersistentGrain for TxCounter {}
impl TransactionalGrain for TxCounter {}

// --- Messages ---

struct Increment {
    amount: i64,
}
impl Message for Increment {
    type Result = i64;
}

struct IncrementAndFail {
    amount: i64,
}
impl Message for IncrementAndFail {
    type Result = i64;
}

struct SaveAndIncrement {
    amount: i64,
}
impl Message for SaveAndIncrement {
    type Result = i64;
}

struct GetCount;
impl Message for GetCount {
    type Result = i64;
}

// Message that calls save_state with a store that will fail
struct SaveAndFail;
impl Message for SaveAndFail {
    type Result = i64;
}

// --- Failing store for testing ---

/// A store that fails on save but succeeds on load.
struct FailingSaveStore {
    inner: InMemoryStateStore,
}

impl FailingSaveStore {
    fn new() -> Self {
        Self {
            inner: InMemoryStateStore::new(),
        }
    }
}

#[async_trait]
impl StateStore for FailingSaveStore {
    async fn load(&self, grain_id: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
        self.inner.load(grain_id).await
    }

    async fn save(&self, _grain_id: &GrainId, _data: &[u8]) -> Result<(), PersistenceError> {
        Err(PersistenceError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "simulated store failure",
        )))
    }

    async fn delete(&self, grain_id: &GrainId) -> Result<(), PersistenceError> {
        self.inner.delete(grain_id).await
    }
}

// --- Handlers ---

// Transactional increment — succeeds normally
#[async_trait]
impl TransactionalHandler<Increment> for TxCounter {
    async fn handle(
        state: &mut CounterState,
        msg: Increment,
        _ctx: &GrainContext,
        _tx: &TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.count += msg.amount;
        Ok(state.count)
    }
}

// Transactional handler that increments then fails — state should roll back
#[async_trait]
impl TransactionalHandler<IncrementAndFail> for TxCounter {
    async fn handle(
        state: &mut CounterState,
        msg: IncrementAndFail,
        _ctx: &GrainContext,
        _tx: &TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.count += msg.amount;
        Err(PersistenceError::Serialization("intentional failure".into()))
    }
}

// Transactional handler that saves mid-handler then increments further
#[async_trait]
impl TransactionalHandler<SaveAndIncrement> for TxCounter {
    async fn handle(
        state: &mut CounterState,
        msg: SaveAndIncrement,
        _ctx: &GrainContext,
        tx: &TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.count += msg.amount;
        tx.save_state(state).await?;
        state.count += 1; // extra increment after save
        Ok(state.count)
    }
}

// Handler that calls save_state which will fail — tests error propagation
#[async_trait]
impl TransactionalHandler<SaveAndFail> for TxCounter {
    async fn handle(
        state: &mut CounterState,
        _msg: SaveAndFail,
        _ctx: &GrainContext,
        tx: &TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.count += 100;
        // save_state will fail because the store returns an error
        tx.save_state(state).await?;
        Ok(state.count)
    }
}

// Regular (non-transactional) handler for GetCount via the persistent path
#[async_trait]
impl GrainHandler<GetCount> for TxCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// --- Tests ---

#[tokio::test]
async fn transactional_increment_succeeds() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let counter = silo.transactional_get_ref::<TxCounter>("tx-1");
    let count = counter.ask(Increment { amount: 5 }).await.unwrap();
    assert_eq!(count, 5);

    let count = counter.ask(Increment { amount: 3 }).await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn rollback_on_handler_error() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let counter = silo.transactional_get_ref::<TxCounter>("tx-rollback");

    // Successful increment to 10
    counter.ask(Increment { amount: 10 }).await.unwrap();

    // This handler increments by 100 then fails — state should revert to 10
    let result = counter.ask(IncrementAndFail { amount: 100 }).await;
    assert!(result.is_err());

    // Verify state was rolled back by reading via the persistent ref
    let reader = silo.persistent_get_ref::<TxCounter>("tx-rollback");
    let count = reader.ask(GetCount).await.unwrap();
    assert_eq!(count, 10, "state should have rolled back to 10");
}

#[tokio::test]
async fn messages_after_rollback_work() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let counter = silo.transactional_get_ref::<TxCounter>("tx-after");

    counter.ask(Increment { amount: 5 }).await.unwrap();

    // Fail — should roll back
    let _ = counter.ask(IncrementAndFail { amount: 100 }).await;

    // Subsequent successful increment should work from rolled-back state
    let count = counter.ask(Increment { amount: 3 }).await.unwrap();
    assert_eq!(count, 8, "should be 5 + 3 = 8 after rollback");
}

#[tokio::test]
async fn save_state_mid_handler() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let silo = PersistentSilo::builder().store_arc(store.clone()).build();

    let counter = silo.transactional_get_ref::<TxCounter>("tx-save");

    // This handler: count += 10, save_state, count += 1, returns 11
    let count = counter
        .ask(SaveAndIncrement { amount: 10 })
        .await
        .unwrap();
    assert_eq!(count, 11);

    // The store should have the state at the save_state() checkpoint (count=10)
    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<TxCounter>(),
        key: "tx-save".to_string(),
    };
    let saved_bytes = store.load(&grain_id).await.unwrap().unwrap();
    let (saved_state, _): (CounterState, _) =
        bincode::serde::decode_from_slice(&saved_bytes, bincode::config::standard()).unwrap();
    assert_eq!(saved_state.count, 10, "store should have mid-handler checkpoint at 10");
}

#[tokio::test]
async fn transactional_state_persists_on_deactivation() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // Increment via transactional ref, then let grain deactivate
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.transactional_get_ref::<TxCounter>("tx-persist");
        counter.ask(Increment { amount: 42 }).await.unwrap();

        // Wait for idle timeout + deactivation
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Reactivate — state should be restored
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let counter = silo.persistent_get_ref::<TxCounter>("tx-persist");
        let count = counter.ask(GetCount).await.unwrap();
        assert_eq!(count, 42);
    }
}

#[tokio::test]
async fn save_state_failure_propagates_and_rolls_back() {
    // Use a store that always fails on save
    let store: Arc<dyn StateStore> = Arc::new(FailingSaveStore::new());
    let silo = PersistentSilo::builder().store_arc(store).build();

    let counter = silo.transactional_get_ref::<TxCounter>("tx-fail-save");

    // First, successfully increment (no save_state call)
    counter.ask(Increment { amount: 5 }).await.unwrap();

    // Now call SaveAndFail which does save_state -> store fails -> handler returns Err
    // The Err should trigger rollback (state reverts from 105 to 5)
    let result = counter.ask(SaveAndFail).await;
    assert!(result.is_err(), "save_state failure should propagate as handler error");

    // Verify state was rolled back
    let reader = silo.persistent_get_ref::<TxCounter>("tx-fail-save");
    let count = reader.ask(GetCount).await.unwrap();
    assert_eq!(count, 5, "state should have rolled back to 5 after save_state failure");
}

#[tokio::test]
async fn multiple_rollbacks_dont_corrupt_state() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let counter = silo.transactional_get_ref::<TxCounter>("tx-multi-rollback");

    counter.ask(Increment { amount: 10 }).await.unwrap();

    // Three consecutive failures — each should roll back independently
    for _ in 0..3 {
        let result = counter.ask(IncrementAndFail { amount: 999 }).await;
        assert!(result.is_err());
    }

    // State should still be 10 after all rollbacks
    let count = counter.ask(Increment { amount: 0 }).await.unwrap();
    assert_eq!(count, 10, "state should remain 10 after three rollbacks");
}
