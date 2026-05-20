//! Smoke tests for sqlx schema migrations in SqliteStateStore.

use orlando_core::GrainId;
use orlando_persistence::SqliteStateStore;
use orlando_persistence::StateStore;

fn gid(key: &str) -> GrainId {
    GrainId {
        type_name: "Counter",
        key: key.to_string(),
    }
}

/// `::new` runs migrations cleanly on a fresh in-memory database.
#[tokio::test]
async fn migrate_fresh_in_memory() {
    let store = SqliteStateStore::new("sqlite::memory:").await.unwrap();
    // Schema exists: a write/read roundtrip works.
    let etag = store
        .save_with_etag(&gid("a"), b"hello", None)
        .await
        .unwrap();
    let (data, read_etag) = store.load_with_etag(&gid("a")).await.unwrap().unwrap();
    assert_eq!(data, b"hello");
    assert_eq!(read_etag, etag);
}

/// Re-opening an already-migrated file database does not error
/// (idempotent: _sqlx_migrations records 0001 as applied; IF NOT EXISTS
/// also covers DBs created by the legacy inline-CREATE path).
#[tokio::test]
async fn migrate_idempotent_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grains.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());

    {
        let store = SqliteStateStore::new(&url).await.unwrap();
        store.save(&gid("persist"), b"v1").await.unwrap();
    }

    // Second open re-runs migrate!() against an already-migrated DB.
    let store = SqliteStateStore::new(&url).await.unwrap();
    let data = store.load(&gid("persist")).await.unwrap().unwrap();
    assert_eq!(data, b"v1");
}
