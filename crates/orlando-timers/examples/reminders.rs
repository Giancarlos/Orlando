//! Durable reminders example using SQLite.
//!
//! Registers a reminder that fires every 200ms, delivering ReminderTick
//! messages to the grain. The reminder survives grain idle deactivation
//! because it's persisted in SQLite.
//!
//! Run with: cargo run -p orlando-timers --example reminders

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainActivator, GrainContext, GrainHandler, GrainId, Message};
use orlando_runtime::Silo;
use orlando_timers::{ReminderService, ReminderTick, SqliteReminderStore};

// ── State ────────────────────────────────────────────────────────

#[derive(Default)]
struct HeartbeatState {
    beats: i64,
}

// ── Grain ────────────────────────────────────────────────────────

struct Heartbeat;

impl Grain for Heartbeat {
    type State = HeartbeatState;
}

// ── Messages ─────────────────────────────────────────────────────

struct GetBeats;

impl Message for GetBeats {
    type Result = i64;
}

// ── Handlers ─────────────────────────────────────────────────────

#[async_trait]
impl GrainHandler<ReminderTick> for Heartbeat {
    async fn handle(state: &mut HeartbeatState, tick: ReminderTick, _ctx: &GrainContext) {
        state.beats += 1;
        println!("  [{}] beat #{}", tick.name, state.beats);
    }
}

#[async_trait]
impl GrainHandler<GetBeats> for Heartbeat {
    async fn handle(state: &mut HeartbeatState, _msg: GetBeats, _ctx: &GrainContext) -> i64 {
        state.beats
    }
}

// ── Main ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let silo = Silo::new();
    let activator: Arc<dyn GrainActivator> = silo.directory().clone();

    // Use SQLite for durable reminder storage
    let store = Arc::new(
        SqliteReminderStore::new("sqlite::memory:")
            .await
            .unwrap(),
    );

    let service = ReminderService::with_poll_interval(
        store,
        activator,
        Duration::from_millis(100), // poll every 100ms
    );
    service.register_grain_type::<Heartbeat>();

    // Activate the grain
    let grain = silo.get_ref::<Heartbeat>("heart-1");
    let beats = grain.ask(GetBeats).await.unwrap();
    println!("Initial beats: {beats}");

    // Register a reminder that fires every 200ms
    let grain_id = GrainId {
        type_name: std::any::type_name::<Heartbeat>(),
        key: "heart-1".into(),
    };
    service
        .register_reminder(&grain_id, "pulse", Duration::from_millis(200))
        .await
        .unwrap();

    println!("Reminder registered. Waiting for beats...");
    println!();

    // Start the reminder service
    let _handle = service.start();

    // Let it run for a bit
    tokio::time::sleep(Duration::from_secs(2)).await;

    let beats = grain.ask(GetBeats).await.unwrap();
    println!();
    println!("Total beats after 2 seconds: {beats}");
    println!("Done!");
}
