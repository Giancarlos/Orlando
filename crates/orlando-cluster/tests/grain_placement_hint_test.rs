use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_cluster::{ClusterSilo, NetworkMessage};
use orlando_core::{Grain, GrainActivator, GrainContext, GrainHandler, Message};

// --- Grain with prefer_local placement hint ---

struct LocalGrain;

#[derive(Default)]
struct LocalState;

#[async_trait]
impl Grain for LocalGrain {
    type State = LocalState;

    fn grain_type_name() -> &'static str {
        "LocalGrain"
    }

    fn placement_hint() -> Option<&'static str> {
        Some("prefer_local")
    }
}

// --- Grain with no placement hint (uses silo default) ---

struct DefaultGrain;

#[derive(Default)]
struct DefaultState;

#[async_trait]
impl Grain for DefaultGrain {
    type State = DefaultState;

    fn grain_type_name() -> &'static str {
        "DefaultGrain"
    }
}

// --- Messages ---

#[derive(Serialize, Deserialize)]
struct Ping;
impl Message for Ping {
    type Result = String;
}
impl NetworkMessage for Ping {
    fn message_type_name() -> &'static str {
        "Ping"
    }
}

#[async_trait]
impl GrainHandler<Ping> for LocalGrain {
    async fn handle(_state: &mut LocalState, _msg: Ping, _ctx: &GrainContext) -> String {
        "pong".to_string()
    }
}

#[async_trait]
impl GrainHandler<Ping> for DefaultGrain {
    async fn handle(_state: &mut DefaultState, _msg: Ping, _ctx: &GrainContext) -> String {
        "pong".to_string()
    }
}

// --- Tests ---

#[tokio::test]
async fn prefer_local_hint_keeps_grain_local() {
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("hint-silo-a")
            .register::<LocalGrain, Ping>()
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("hint-silo-b")
            .register::<LocalGrain, Ping>()
            .build(),
    );

    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    silo_b
        .join_cluster(&format!("127.0.0.1:{}", port_a))
        .await
        .unwrap();

    // With prefer_local hint, all grains accessed from silo_a should stay on silo_a
    // even though the silo-level strategy is default (hash-based).
    for i in 0..10 {
        let grain = silo_a.get_ref::<LocalGrain>(&format!("hint-key-{}", i));
        grain.ask(Ping).await.unwrap();
    }

    let a_count = silo_a.directory().grain_ids().len();
    assert_eq!(
        a_count, 10,
        "all grains should be local due to prefer_local hint"
    );

    _sa.abort();
    _sb.abort();
}

#[tokio::test]
async fn no_hint_uses_silo_default_placement() {
    // Without a placement hint, the silo-level strategy (hash) is used.
    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("hint-default")
        .register::<DefaultGrain, Ping>()
        .build();

    let grain = silo.get_ref::<DefaultGrain>("key-1");
    let result = grain.ask(Ping).await.unwrap();
    assert_eq!(result, "pong");
}

#[tokio::test]
async fn placement_hint_returns_none_by_default() {
    // Verify the trait default returns None
    assert_eq!(DefaultGrain::placement_hint(), None);
}

#[tokio::test]
async fn placement_hint_returns_configured_value() {
    assert_eq!(LocalGrain::placement_hint(), Some("prefer_local"));
}
