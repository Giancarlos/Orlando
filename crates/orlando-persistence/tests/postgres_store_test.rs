//! Integration tests for PostgresStateStore.
//!
//! These tests require a running PostgreSQL instance. Set the
//! `ORLANDO_TEST_POSTGRES_URL` environment variable to run them:
//!
//! ```bash
//! # Start Postgres (e.g., via Docker):
//! docker run -d --name orlando-pg -e POSTGRES_PASSWORD=test -e POSTGRES_DB=orlando_test -p 5432:5432 postgres:16
//!
//! # Run the tests:
//! ORLANDO_TEST_POSTGRES_URL="postgres://postgres:test@localhost/orlando_test" \
//!     cargo test --package orlando-persistence --features postgres --test postgres_store_test
//! ```

#[cfg(feature = "postgres")]
mod tests {
    use orlando_core::GrainId;
    use orlando_persistence::{PersistenceError, PostgresStateStore, StateStore};

    fn test_url() -> String {
        std::env::var("ORLANDO_TEST_POSTGRES_URL")
            .expect("set ORLANDO_TEST_POSTGRES_URL to run Postgres tests")
    }

    fn grain(key: &str) -> GrainId {
        GrainId {
            type_name: "PgTestGrain",
            key: key.to_string(),
        }
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn save_and_load() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("save-load-1");

        // Clean up from prior runs
        store.delete(&id).await.unwrap();

        store.save(&id, b"hello postgres").await.unwrap();
        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded, b"hello postgres");

        // Cleanup
        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn load_missing_returns_none() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("nonexistent-pg");

        let loaded = store.load(&id).await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn save_overwrites_existing() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("overwrite-1");

        store.delete(&id).await.unwrap();

        store.save(&id, b"first").await.unwrap();
        store.save(&id, b"second").await.unwrap();

        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded, b"second");

        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn delete_removes_state() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("delete-1");

        store.save(&id, b"to-be-deleted").await.unwrap();
        assert!(store.load(&id).await.unwrap().is_some());

        store.delete(&id).await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn delete_nonexistent_is_ok() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("never-existed-pg");

        // Should not error
        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn independent_grains() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id_a = grain("independent-a");
        let id_b = grain("independent-b");

        store.delete(&id_a).await.unwrap();
        store.delete(&id_b).await.unwrap();

        store.save(&id_a, b"aaa").await.unwrap();
        store.save(&id_b, b"bbb").await.unwrap();

        assert_eq!(store.load(&id_a).await.unwrap().unwrap(), b"aaa");
        assert_eq!(store.load(&id_b).await.unwrap().unwrap(), b"bbb");

        // Deleting one doesn't affect the other
        store.delete(&id_a).await.unwrap();
        assert!(store.load(&id_a).await.unwrap().is_none());
        assert_eq!(store.load(&id_b).await.unwrap().unwrap(), b"bbb");

        store.delete(&id_b).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn binary_data_round_trip() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("binary-pg");

        store.delete(&id).await.unwrap();

        // Store raw bytes including nulls and high bytes
        let data: Vec<u8> = (0u8..=255).collect();
        store.save(&id, &data).await.unwrap();

        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded, data);

        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn etag_mismatch_on_duplicate_insert() {
        // First save_with_etag(None) inserts; second must detect the unique
        // violation via is_unique_violation() and surface EtagMismatch — not a
        // raw Postgres error.
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("etag-dup-insert");
        store.delete(&id).await.unwrap();

        let etag = store.save_with_etag(&id, b"first", None).await.unwrap();
        assert_eq!(etag.0, "1");

        let err = store
            .save_with_etag(&id, b"second", None)
            .await
            .expect_err("second insert must fail");
        match err {
            PersistenceError::EtagMismatch { expected, actual } => {
                assert!(expected.is_none());
                assert_eq!(actual.unwrap().0, "1");
            }
            other => panic!("expected EtagMismatch, got {other:?}"),
        }

        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running PostgreSQL (set ORLANDO_TEST_POSTGRES_URL)"]
    async fn etag_mismatch_on_stale_update() {
        let store = PostgresStateStore::new(&test_url()).await.unwrap();
        let id = grain("etag-stale-update");
        store.delete(&id).await.unwrap();

        let v1 = store.save_with_etag(&id, b"v1", None).await.unwrap();
        let v2 = store.save_with_etag(&id, b"v2", Some(&v1)).await.unwrap();

        // Reusing v1 (stale) must produce EtagMismatch carrying v2 as actual.
        let err = store
            .save_with_etag(&id, b"v3", Some(&v1))
            .await
            .expect_err("stale etag update must fail");
        match err {
            PersistenceError::EtagMismatch { expected, actual } => {
                assert_eq!(expected.unwrap().0, v1.0);
                assert_eq!(actual.unwrap().0, v2.0);
            }
            other => panic!("expected EtagMismatch, got {other:?}"),
        }

        store.delete(&id).await.unwrap();
    }
}
