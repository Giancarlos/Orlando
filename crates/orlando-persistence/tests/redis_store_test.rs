//! Integration tests for RedisStateStore.
//!
//! These tests require a running Redis instance. Set the
//! `ORLANDO_TEST_REDIS_URL` environment variable to run them:
//!
//! ```bash
//! # Start Redis (e.g., via Docker):
//! docker run -d --name orlando-redis -p 6379:6379 redis:7
//!
//! # Run the tests:
//! ORLANDO_TEST_REDIS_URL="redis://localhost:6379" \
//!     cargo test --package orlando-persistence --features redis --test redis_store_test
//! ```

#[cfg(feature = "redis")]
mod tests {
    use orlando_core::GrainId;
    use orlando_persistence::{RedisStateStore, StateStore};

    fn test_url() -> String {
        std::env::var("ORLANDO_TEST_REDIS_URL")
            .expect("set ORLANDO_TEST_REDIS_URL to run Redis tests")
    }

    fn grain(key: &str) -> GrainId {
        GrainId {
            type_name: "RedisTestGrain",
            key: key.to_string(),
        }
    }

    #[tokio::test]
    #[ignore = "requires running Redis (set ORLANDO_TEST_REDIS_URL)"]
    async fn save_and_load() {
        let store = RedisStateStore::new(&test_url()).await.unwrap();
        let id = grain("save-load-1");

        store.delete(&id).await.unwrap();

        store.save(&id, b"hello redis").await.unwrap();
        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded, b"hello redis");

        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running Redis (set ORLANDO_TEST_REDIS_URL)"]
    async fn load_missing_returns_none() {
        let store = RedisStateStore::new(&test_url()).await.unwrap();
        let id = grain("nonexistent-redis");

        let loaded = store.load(&id).await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    #[ignore = "requires running Redis (set ORLANDO_TEST_REDIS_URL)"]
    async fn save_overwrites_existing() {
        let store = RedisStateStore::new(&test_url()).await.unwrap();
        let id = grain("overwrite-1");

        store.delete(&id).await.unwrap();

        store.save(&id, b"first").await.unwrap();
        store.save(&id, b"second").await.unwrap();

        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded, b"second");

        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running Redis (set ORLANDO_TEST_REDIS_URL)"]
    async fn delete_removes_state() {
        let store = RedisStateStore::new(&test_url()).await.unwrap();
        let id = grain("delete-1");

        store.save(&id, b"to-be-deleted").await.unwrap();
        assert!(store.load(&id).await.unwrap().is_some());

        store.delete(&id).await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    #[ignore = "requires running Redis (set ORLANDO_TEST_REDIS_URL)"]
    async fn delete_nonexistent_is_ok() {
        let store = RedisStateStore::new(&test_url()).await.unwrap();
        let id = grain("never-existed-redis");

        store.delete(&id).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running Redis (set ORLANDO_TEST_REDIS_URL)"]
    async fn independent_grains() {
        let store = RedisStateStore::new(&test_url()).await.unwrap();
        let id_a = grain("independent-a");
        let id_b = grain("independent-b");

        store.delete(&id_a).await.unwrap();
        store.delete(&id_b).await.unwrap();

        store.save(&id_a, b"aaa").await.unwrap();
        store.save(&id_b, b"bbb").await.unwrap();

        assert_eq!(store.load(&id_a).await.unwrap().unwrap(), b"aaa");
        assert_eq!(store.load(&id_b).await.unwrap().unwrap(), b"bbb");

        store.delete(&id_a).await.unwrap();
        assert!(store.load(&id_a).await.unwrap().is_none());
        assert_eq!(store.load(&id_b).await.unwrap().unwrap(), b"bbb");

        store.delete(&id_b).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires running Redis (set ORLANDO_TEST_REDIS_URL)"]
    async fn binary_data_round_trip() {
        let store = RedisStateStore::new(&test_url()).await.unwrap();
        let id = grain("binary-redis");

        store.delete(&id).await.unwrap();

        let data: Vec<u8> = (0u8..=255).collect();
        store.save(&id, &data).await.unwrap();

        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded, data);

        store.delete(&id).await.unwrap();
    }
}
