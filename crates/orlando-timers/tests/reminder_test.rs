use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainActivator, GrainContext, GrainHandler, Message};
use orlando_runtime::Silo;
use orlando_timers::{InMemoryReminderStore, ReminderService, ReminderTick, SqliteReminderStore};

// --- State ---

#[derive(Default)]
struct ReminderCounterState {
    count: i64,
}

// --- Grain ---

struct ReminderCounter;

#[async_trait]
impl Grain for ReminderCounter {
    type State = ReminderCounterState;

    fn idle_timeout() -> Duration {
        Duration::from_secs(5)
    }
}

// --- Messages ---

struct GetCount;

impl Message for GetCount {
    type Result = i64;
}

// --- Handlers ---

#[async_trait]
impl GrainHandler<ReminderTick> for ReminderCounter {
    async fn handle(
        state: &mut ReminderCounterState,
        _tick: ReminderTick,
        _ctx: &GrainContext,
    ) {
        state.count += 1;
    }
}

#[async_trait]
impl GrainHandler<GetCount> for ReminderCounter {
    async fn handle(
        state: &mut ReminderCounterState,
        _msg: GetCount,
        _ctx: &GrainContext,
    ) -> i64 {
        state.count
    }
}

// --- Tests ---

#[tokio::test]
async fn reminder_fires_and_delivers_tick() {
    let silo = Silo::new();
    let store = Arc::new(InMemoryReminderStore::new());
    let activator: Arc<dyn GrainActivator> = silo.directory().clone();

    let service = ReminderService::with_poll_interval(
        store,
        activator,
        Duration::from_millis(50), // fast poll for testing
    );
    service.register_grain_type::<ReminderCounter>();
    let _poll_handle = service.start();

    // Activate the grain first
    let counter = silo.get_ref::<ReminderCounter>("rc-1");
    assert_eq!(counter.ask(GetCount).await.unwrap(), 0);

    // Register a reminder with 100ms period
    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<ReminderCounter>(),
        key: "rc-1".into(),
    };
    service
        .register_reminder(&grain_id, "tick", Duration::from_millis(100))
        .await
        .unwrap();

    // Wait for reminder to fire (100ms period + 50ms poll + margin)
    tokio::time::sleep(Duration::from_millis(300)).await;

    let count = counter.ask(GetCount).await.unwrap();
    assert!(count >= 1, "expected at least 1 reminder tick, got {count}");
}

#[tokio::test]
async fn reminder_activates_inactive_grain() {
    let silo = Silo::new();
    let store = Arc::new(InMemoryReminderStore::new());
    let activator: Arc<dyn GrainActivator> = silo.directory().clone();

    let service = ReminderService::with_poll_interval(
        store,
        activator,
        Duration::from_millis(50),
    );
    service.register_grain_type::<ReminderCounter>();

    // Register a reminder BEFORE the grain is activated.
    // The service's dispatch will activate it via get_or_insert.
    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<ReminderCounter>(),
        key: "rc-lazy".into(),
    };
    service
        .register_reminder(&grain_id, "wake-up", Duration::from_millis(100))
        .await
        .unwrap();

    let _poll_handle = service.start();

    // Wait for reminder to fire and activate the grain
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Now get a ref to the grain — it should already be active with count > 0
    let counter = silo.get_ref::<ReminderCounter>("rc-lazy");
    let count = counter.ask(GetCount).await.unwrap();
    assert!(
        count >= 1,
        "expected reminder to activate grain and deliver at least 1 tick, got {count}"
    );
}

#[tokio::test]
async fn reminder_can_be_unregistered() {
    let silo = Silo::new();
    let store = Arc::new(InMemoryReminderStore::new());
    let activator: Arc<dyn GrainActivator> = silo.directory().clone();

    let service = ReminderService::with_poll_interval(
        store,
        activator,
        Duration::from_millis(50),
    );
    service.register_grain_type::<ReminderCounter>();
    let _poll_handle = service.start();

    let counter = silo.get_ref::<ReminderCounter>("rc-unreg");
    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<ReminderCounter>(),
        key: "rc-unreg".into(),
    };

    // Register then immediately unregister
    service
        .register_reminder(&grain_id, "temp", Duration::from_millis(100))
        .await
        .unwrap();
    service
        .unregister_reminder(&grain_id, "temp")
        .await
        .unwrap();

    // Wait and verify no ticks delivered
    tokio::time::sleep(Duration::from_millis(300)).await;
    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 0, "unregistered reminder should not fire");
}

// --- SQLite reminder store tests ---

#[tokio::test]
async fn sqlite_reminder_fires_and_delivers_tick() {
    let store = Arc::new(
        SqliteReminderStore::new("sqlite::memory:")
            .await
            .unwrap(),
    );

    let silo = Silo::new();
    let activator: Arc<dyn GrainActivator> = silo.directory().clone();

    let service = ReminderService::with_poll_interval(
        store,
        activator,
        Duration::from_millis(50),
    );
    service.register_grain_type::<ReminderCounter>();
    let _poll_handle = service.start();

    let counter = silo.get_ref::<ReminderCounter>("sqlite-rc-1");
    assert_eq!(counter.ask(GetCount).await.unwrap(), 0);

    let grain_id = orlando_core::GrainId {
        type_name: std::any::type_name::<ReminderCounter>(),
        key: "sqlite-rc-1".into(),
    };
    service
        .register_reminder(&grain_id, "tick", Duration::from_millis(100))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    let count = counter.ask(GetCount).await.unwrap();
    assert!(count >= 1, "expected at least 1 reminder tick, got {count}");
}

#[tokio::test]
async fn sqlite_reminder_survives_store_reconnect() {
    use orlando_timers::ReminderStore;
    use std::time::SystemTime;

    // Use a temp file so the DB persists across connections
    let dir = std::env::temp_dir().join("orlando_test_reminders");
    let _ = std::fs::create_dir_all(&dir);
    let db_path = dir.join("test_durable.db");
    let _ = std::fs::remove_file(&db_path); // clean slate
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    // First connection: save a reminder
    {
        let store = SqliteReminderStore::new(&url).await.unwrap();
        let grain_id = orlando_core::GrainId {
            type_name: "TestGrain",
            key: "key-1".into(),
        };
        let reg = orlando_timers::ReminderRegistration {
            grain_id,
            name: "durable-tick".into(),
            period: Duration::from_secs(60),
            due_at: SystemTime::now(),
        };
        store.save(&reg).await.unwrap();
    }

    // Second connection: the reminder should still be there
    {
        let store = SqliteReminderStore::new(&url).await.unwrap();
        let due = store.load_due(SystemTime::now()).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].grain_id.type_name, "TestGrain");
        assert_eq!(due[0].grain_id.key, "key-1");
        assert_eq!(due[0].name, "durable-tick");
        assert_eq!(due[0].period, Duration::from_secs(60));
    }

    // Cleanup
    let _ = std::fs::remove_file(&db_path);
}
