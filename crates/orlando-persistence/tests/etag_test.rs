use orlando_core::GrainId;
use orlando_persistence::{ETag, InMemoryStateStore, SqliteStateStore, StateStore};

fn test_grain_id() -> GrainId {
    GrainId {
        type_name: "ETagTestGrain",
        key: "k1".into(),
    }
}

// ---------------------------------------------------------------------------
// InMemoryStateStore etag tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_save_and_load_with_etag() {
    let store = InMemoryStateStore::new();
    let id = test_grain_id();

    // First save with no expected etag.
    let etag = store.save_with_etag(&id, b"hello", None).await.unwrap();
    assert!(etag.is_some());

    // Load returns the same etag.
    let (data, loaded_etag) = store.load_with_etag(&id).await.unwrap().unwrap();
    assert_eq!(data, b"hello");
    assert_eq!(loaded_etag, etag);

    // Save with correct etag succeeds and bumps version.
    let etag2 = store
        .save_with_etag(&id, b"world", loaded_etag.as_ref())
        .await
        .unwrap();
    assert!(etag2.is_some());
    assert_ne!(etag2, etag);

    // Save with stale etag fails.
    let result = store.save_with_etag(&id, b"stale", etag.as_ref()).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, orlando_persistence::PersistenceError::EtagMismatch { .. }),
        "expected EtagMismatch, got: {err:?}"
    );
}

#[tokio::test]
async fn memory_save_with_etag_on_missing_key_fails() {
    let store = InMemoryStateStore::new();
    let id = test_grain_id();

    // Saving with an expected etag when no entry exists should fail.
    let bogus = ETag("999".into());
    let result = store.save_with_etag(&id, b"data", Some(&bogus)).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn memory_basic_ops_still_work() {
    let store = InMemoryStateStore::new();
    let id = test_grain_id();

    // Basic save/load (no etag) must still work.
    store.save(&id, b"abc").await.unwrap();
    let loaded = store.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded, b"abc");

    // Delete still works.
    store.delete(&id).await.unwrap();
    assert!(store.load(&id).await.unwrap().is_none());
}

// ---------------------------------------------------------------------------
// SqliteStateStore etag tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sqlite_save_and_load_with_etag() {
    let store = SqliteStateStore::new("sqlite::memory:").await.unwrap();
    let id = test_grain_id();

    // First save with no expected etag.
    let etag = store.save_with_etag(&id, b"hello", None).await.unwrap();
    assert!(etag.is_some());

    // Load returns the same etag.
    let (data, loaded_etag) = store.load_with_etag(&id).await.unwrap().unwrap();
    assert_eq!(data, b"hello");
    assert_eq!(loaded_etag, etag);

    // Save with correct etag succeeds and bumps version.
    let etag2 = store
        .save_with_etag(&id, b"world", loaded_etag.as_ref())
        .await
        .unwrap();
    assert!(etag2.is_some());
    assert_ne!(etag2, etag);

    // Save with stale etag fails.
    let result = store.save_with_etag(&id, b"stale", etag.as_ref()).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, orlando_persistence::PersistenceError::EtagMismatch { .. }),
        "expected EtagMismatch, got: {err:?}"
    );
}

#[tokio::test]
async fn sqlite_save_with_etag_on_missing_key_fails() {
    let store = SqliteStateStore::new("sqlite::memory:").await.unwrap();
    let id = test_grain_id();

    // Saving with an expected etag when no entry exists should fail.
    let bogus = ETag("999".into());
    let result = store.save_with_etag(&id, b"data", Some(&bogus)).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn sqlite_basic_ops_still_work() {
    let store = SqliteStateStore::new("sqlite::memory:").await.unwrap();
    let id = test_grain_id();

    // Basic save/load (no etag) must still work.
    store.save(&id, b"abc").await.unwrap();
    let loaded = store.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded, b"abc");

    // Delete still works.
    store.delete(&id).await.unwrap();
    assert!(store.load(&id).await.unwrap().is_none());
}
