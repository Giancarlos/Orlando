//! Versioned grain / state migration example.
//!
//! When a grain's state schema changes across deployments, old persisted bytes
//! must be upgraded. A `VersionedGrain` declares `state_version()` and a
//! `migrate()` chain (v0 → v1 → v2 → …) that runs automatically on load, so a
//! grain activated against old data transparently sees the current schema.
//!
//! Run with: `cargo run -p orlando-persistence --example versioned_grain`

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, GrainHandler, GrainId, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistenceError, PersistentGrain, PersistentSilo, StateStore,
    VersionedGrain, migrate_state,
};

// ── Schema history ──────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct ProfileV0 {
    name: String,
} // original schema

#[derive(Serialize, Deserialize)]
struct ProfileV1 {
    name: String,
    email: String,
} // added email

#[derive(Default, Serialize, Deserialize)]
struct ProfileV2 {
    name: String,
    email: String,
    age: u32,
} // current: added age

// ── Grain (current schema = V2) ─────────────────────────────────

struct Profile;

#[async_trait]
impl Grain for Profile {
    type State = ProfileV2;
}

impl PersistentGrain for Profile {}

impl VersionedGrain for Profile {
    fn state_version() -> u32 {
        2
    }

    // Each step upgrades by exactly one version; the runtime chains them.
    fn migrate(from_version: u32, bytes: Vec<u8>) -> Result<Vec<u8>, PersistenceError> {
        match from_version {
            0 => migrate_state::<ProfileV0, ProfileV1>(bytes, |old| ProfileV1 {
                name: old.name,
                email: "unknown@example.com".into(), // sensible default for the new field
            }),
            1 => migrate_state::<ProfileV1, ProfileV2>(bytes, |old| ProfileV2 {
                name: old.name,
                email: old.email,
                age: 0,
            }),
            other => Err(PersistenceError::Deserialization(format!(
                "unknown state version {other}"
            ))),
        }
    }
}

// ── Messages ────────────────────────────────────────────────────

struct GetProfile;
impl Message for GetProfile {
    type Result = (String, String, u32);
}

#[async_trait]
impl GrainHandler<GetProfile> for Profile {
    async fn handle(state: &mut ProfileV2, _msg: GetProfile, _ctx: &GrainContext) -> (String, String, u32) {
        (state.name.clone(), state.email.clone(), state.age)
    }
}

#[tokio::main]
async fn main() {
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());

    // Simulate data left by an OLD deployment: raw V0 bytes, no version metadata.
    let grain_id = GrainId { type_name: std::any::type_name::<Profile>(), key: "ada".into() };
    let v0 = ProfileV0 { name: "Ada".into() };
    let bytes = bincode::serde::encode_to_vec(&v0, bincode::config::standard()).unwrap();
    store.save(&grain_id, &bytes).await.unwrap();
    println!("seeded store with legacy V0 profile: name only (\"Ada\")");

    // A new deployment activates the grain at V2 — migration runs on load.
    let silo = PersistentSilo::builder().store_arc(store.clone()).build();
    let profile = silo.versioned_get_ref::<Profile>("ada");

    let (name, email, age) = profile.ask(GetProfile).await.unwrap();
    println!("after load (migrated v0 -> v1 -> v2): name={name:?} email={email:?} age={age}");
    assert_eq!(name, "Ada", "name preserved across migration");
    assert_eq!(email, "unknown@example.com", "email defaulted by v0->v1");
    assert_eq!(age, 0, "age defaulted by v1->v2");
    println!("legacy state migrated to the current schema automatically ✓");
}
