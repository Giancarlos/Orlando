use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, GrainId, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistentGrain, PersistentSilo, SerializerFormat, StateStore,
};

// --- State ---

#[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
struct CounterState {
    count: i64,
}

// --- Grain ---

struct JsonCounter;

#[async_trait]
impl Grain for JsonCounter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

impl PersistentGrain for JsonCounter {}

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

#[async_trait]
impl GrainHandler<Increment> for JsonCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for JsonCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

/// Verify that a grain using JSON serialization persists and reloads correctly.
#[tokio::test]
async fn json_serializer_round_trips_state() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // First silo: increment counter, let it deactivate (persists with JSON)
    {
        let silo = PersistentSilo::builder()
            .named_store_arc_with_serializer("default", store.clone(), SerializerFormat::Json)
            .build();

        let counter = silo.persistent_get_ref::<JsonCounter>("json-1");
        let result = counter.ask(Increment { amount: 7 }).await.unwrap();
        assert_eq!(result, 7);

        // Wait for idle deactivation → state saved as JSON
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Verify the stored bytes are valid JSON
    let grain_id = GrainId {
        type_name: std::any::type_name::<JsonCounter>(),
        key: "json-1".to_string(),
    };
    let raw = store
        .load(&grain_id)
        .await
        .unwrap()
        .expect("state should be persisted");
    let json_str = std::str::from_utf8(&raw).expect("JSON should be valid UTF-8");
    assert!(
        json_str.contains("\"count\""),
        "stored bytes should be JSON, got: {json_str}"
    );

    // Second silo: reload from JSON and verify state survived
    {
        let silo = PersistentSilo::builder()
            .named_store_arc_with_serializer("default", store.clone(), SerializerFormat::Json)
            .build();

        let counter = silo.persistent_get_ref::<JsonCounter>("json-1");
        let count = counter.ask(GetCount).await.unwrap();
        assert_eq!(count, 7, "state should survive JSON round-trip");
    }
}

/// Verify that named stores can use different serializers.
#[tokio::test]
async fn different_serializers_per_named_store() {
    let json_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let bincode_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    let silo = PersistentSilo::builder()
        .named_store_arc_with_serializer("default", json_store.clone(), SerializerFormat::Json)
        .named_store_arc_with_serializer("fast", bincode_store.clone(), SerializerFormat::Bincode)
        .build();

    let counter = silo.persistent_get_ref::<JsonCounter>("multi-1");
    counter.ask(Increment { amount: 3 }).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify JSON store has data
    let grain_id = GrainId {
        type_name: std::any::type_name::<JsonCounter>(),
        key: "multi-1".to_string(),
    };
    let raw = json_store.load(&grain_id).await.unwrap();
    assert!(raw.is_some(), "JSON store should have the grain state");

    // Verify bincode store is empty (grain used default store, not "fast")
    let raw2 = bincode_store.load(&grain_id).await.unwrap();
    assert!(
        raw2.is_none(),
        "bincode store should be empty — grain uses default"
    );
}
