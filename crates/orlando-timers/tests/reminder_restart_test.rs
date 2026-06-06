//! PROD-25: verify a durable reminder registered before a silo "restart" still
//! fires afterwards, loaded from the persistent store without re-registration.
//!
//! A restart is simulated by dropping the silo + reminder service and creating
//! fresh ones over the SAME `SqliteReminderStore` (the durable medium survives a
//! restart; the in-memory silo/service do not).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use orlando_core::{Grain, GrainActivator, GrainContext, GrainHandler, GrainId, Message};
use orlando_runtime::Silo;
use orlando_timers::{ReminderService, ReminderStore, ReminderTick, SqliteReminderStore};

#[derive(Default)]
struct BeatState {
    beats: i64,
}

struct Beat;

impl Grain for Beat {
    type State = BeatState;
}

#[async_trait]
impl GrainHandler<ReminderTick> for Beat {
    async fn handle(state: &mut BeatState, _tick: ReminderTick, _ctx: &GrainContext) {
        state.beats += 1;
    }
}

struct GetBeats;
impl Message for GetBeats {
    type Result = i64;
}

#[async_trait]
impl GrainHandler<GetBeats> for Beat {
    async fn handle(state: &mut BeatState, _msg: GetBeats, _ctx: &GrainContext) -> i64 {
        state.beats
    }
}

fn beat_id() -> GrainId {
    GrainId {
        type_name: std::any::type_name::<Beat>(),
        key: "b1".into(),
    }
}

#[tokio::test]
async fn durable_reminder_fires_after_restart() {
    // Durable store, shared across the simulated restart.
    let store = Arc::new(SqliteReminderStore::new("sqlite::memory:").await.unwrap());

    // --- Before restart: register a reminder, then drop silo + service without
    //     ever starting the poll loop (simulating a crash right after register). ---
    {
        let silo = Silo::new();
        let activator: Arc<dyn GrainActivator> = silo.directory().clone();
        let service =
            ReminderService::with_poll_interval(store.clone(), activator, Duration::from_millis(40));
        service.register_grain_type::<Beat>();
        service
            .register_reminder(&beat_id(), "pulse", Duration::from_millis(40))
            .await
            .unwrap();
    }

    // The registration must have been persisted to the store.
    let persisted = store
        .load_due(SystemTime::now() + Duration::from_secs(60))
        .await
        .unwrap();
    assert!(
        persisted.iter().any(|r| r.name == "pulse"),
        "reminder must survive in the store after the silo is dropped"
    );

    // --- After restart: brand-new silo + service over the SAME store. We
    //     re-register the grain TYPE (runtime dispatch wiring, not persisted)
    //     but do NOT re-register the reminder — it should load from the store. ---
    let silo = Silo::new();
    let activator: Arc<dyn GrainActivator> = silo.directory().clone();
    let service =
        ReminderService::with_poll_interval(store.clone(), activator, Duration::from_millis(40));
    service.register_grain_type::<Beat>();
    let _handle = service.start();

    // Give the poll loop time to load the persisted reminder and fire it.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let beats = silo.get_ref::<Beat>("b1").ask(GetBeats).await.unwrap();
    assert!(
        beats > 0,
        "a reminder registered before restart must fire after restart, got {beats} beats"
    );
}
