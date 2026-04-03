use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, GrainId, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistenceError, PersistentGrain, PersistentSilo, StateStore,
    VersionedGrain, migrate_state,
};

// --- V1 state (legacy) ---

#[derive(Serialize, Deserialize)]
struct StateV0 {
    name: String,
}

// --- V1 state (adds email) ---

#[derive(Serialize, Deserialize)]
struct StateV1 {
    name: String,
    email: String,
}

// --- V2 state (current — adds age) ---

#[derive(Default, Serialize, Deserialize)]
struct StateV2 {
    name: String,
    email: String,
    age: u32,
}

// --- Grain ---

struct UserGrain;

#[async_trait]
impl Grain for UserGrain {
    type State = StateV2;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

impl PersistentGrain for UserGrain {}

impl VersionedGrain for UserGrain {
    fn state_version() -> u32 {
        2
    }

    fn migrate(from_version: u32, bytes: Vec<u8>) -> Result<Vec<u8>, PersistenceError> {
        match from_version {
            0 => migrate_state::<StateV0, StateV1>(bytes, |old| StateV1 {
                name: old.name,
                email: String::from("unknown@example.com"),
            }),
            1 => migrate_state::<StateV1, StateV2>(bytes, |old| StateV2 {
                name: old.name,
                email: old.email,
                age: 0,
            }),
            _ => Err(PersistenceError::Deserialization(format!(
                "unknown version: {}",
                from_version
            ))),
        }
    }
}

// --- Messages ---

struct GetName;
impl Message for GetName {
    type Result = String;
}

struct GetEmail;
impl Message for GetEmail {
    type Result = String;
}

struct GetAge;
impl Message for GetAge {
    type Result = u32;
}

struct SetName {
    name: String,
}
impl Message for SetName {
    type Result = ();
}

// --- Handlers ---

#[async_trait]
impl GrainHandler<GetName> for UserGrain {
    async fn handle(state: &mut StateV2, _msg: GetName, _ctx: &GrainContext) -> String {
        state.name.clone()
    }
}

#[async_trait]
impl GrainHandler<GetEmail> for UserGrain {
    async fn handle(state: &mut StateV2, _msg: GetEmail, _ctx: &GrainContext) -> String {
        state.email.clone()
    }
}

#[async_trait]
impl GrainHandler<GetAge> for UserGrain {
    async fn handle(state: &mut StateV2, _msg: GetAge, _ctx: &GrainContext) -> u32 {
        state.age
    }
}

#[async_trait]
impl GrainHandler<SetName> for UserGrain {
    async fn handle(state: &mut StateV2, msg: SetName, _ctx: &GrainContext) {
        state.name = msg.name;
    }
}

// --- Helper to store raw bytes in the store ---

async fn store_raw_state<S: Serialize>(
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
    state: &S,
) {
    let bytes = bincode::serde::encode_to_vec(state, bincode::config::standard()).unwrap();
    store.save(grain_id, &bytes).await.unwrap();
}

fn user_grain_id(key: &str) -> GrainId {
    GrainId {
        type_name: std::any::type_name::<UserGrain>(),
        key: key.to_string(),
    }
}

// --- Tests ---

#[tokio::test]
async fn versioned_save_and_load() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // Save via versioned grain, then reload
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let user = silo.versioned_get_ref::<UserGrain>("v-user-1");
        user.ask(SetName {
            name: "Alice".into(),
        })
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Reload — should have the name and version 2
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let user = silo.versioned_get_ref::<UserGrain>("v-user-1");
        let name = user.ask(GetName).await.unwrap();
        assert_eq!(name, "Alice");
    }
}

#[tokio::test]
async fn migrate_from_v0_to_current() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let grain_id = user_grain_id("v0-user");

    // Store raw V0 state (no version metadata)
    store_raw_state(&store, &grain_id, &StateV0 {
        name: "Bob".into(),
    })
    .await;

    // Load via versioned grain — should migrate v0 → v1 → v2
    let silo = PersistentSilo::builder().store_arc(store.clone()).build();
    let user = silo.versioned_get_ref::<UserGrain>("v0-user");

    let name = user.ask(GetName).await.unwrap();
    assert_eq!(name, "Bob", "name should survive migration");

    let email = user.ask(GetEmail).await.unwrap();
    assert_eq!(email, "unknown@example.com", "email should be default from v0→v1 migration");

    let age = user.ask(GetAge).await.unwrap();
    assert_eq!(age, 0, "age should be default from v1→v2 migration");
}

#[tokio::test]
async fn migrate_chain_v0_v1_v2() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let grain_id = user_grain_id("chain-user");

    // Store V0 state
    store_raw_state(&store, &grain_id, &StateV0 {
        name: "Charlie".into(),
    })
    .await;

    // Load and let it deactivate (saves as v2)
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let user = silo.versioned_get_ref::<UserGrain>("chain-user");
        let name = user.ask(GetName).await.unwrap();
        assert_eq!(name, "Charlie");

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Reload — should be at v2 now, no migration needed
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let user = silo.versioned_get_ref::<UserGrain>("chain-user");
        let name = user.ask(GetName).await.unwrap();
        assert_eq!(name, "Charlie");
        let email = user.ask(GetEmail).await.unwrap();
        assert_eq!(email, "unknown@example.com");
    }
}

#[tokio::test]
async fn no_migration_when_at_current_version() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // Save as versioned grain first (creates version metadata)
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let user = silo.versioned_get_ref::<UserGrain>("current-user");
        user.ask(SetName {
            name: "Dave".into(),
        })
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Reload — version matches, no migration needed
    {
        let silo = PersistentSilo::builder().store_arc(store.clone()).build();
        let user = silo.versioned_get_ref::<UserGrain>("current-user");
        let name = user.ask(GetName).await.unwrap();
        assert_eq!(name, "Dave");
    }
}

#[tokio::test]
async fn legacy_persistent_state_migrated_via_versioned_ref() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // Store state via regular PersistentGrain (no version metadata)
    // This simulates existing data from before versioning was added.
    // We store a V0 struct since that's what the "original" grain had.
    let grain_id = user_grain_id("legacy-user");
    store_raw_state(&store, &grain_id, &StateV0 {
        name: "Eve".into(),
    })
    .await;

    // Now access via versioned_get_ref — should treat as v0 and migrate
    let silo = PersistentSilo::builder().store_arc(store.clone()).build();
    let user = silo.versioned_get_ref::<UserGrain>("legacy-user");

    let name = user.ask(GetName).await.unwrap();
    assert_eq!(name, "Eve");

    let email = user.ask(GetEmail).await.unwrap();
    assert_eq!(email, "unknown@example.com");
}
