use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainError, GrainHandler, Message};
use orlando_runtime::Silo;

// --- Grain with a very short ask timeout ---

#[derive(Default)]
struct SlowState;

struct SlowGrain;

#[async_trait]
impl Grain for SlowGrain {
    type State = SlowState;

    fn ask_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

struct SlowMessage;
impl Message for SlowMessage {
    type Result = ();
}

struct FastMessage;
impl Message for FastMessage {
    type Result = i32;
}

#[async_trait]
impl GrainHandler<SlowMessage> for SlowGrain {
    async fn handle(_state: &mut SlowState, _msg: SlowMessage, _ctx: &GrainContext) {
        // Simulate a handler that takes too long
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[async_trait]
impl GrainHandler<FastMessage> for SlowGrain {
    async fn handle(_state: &mut SlowState, _msg: FastMessage, _ctx: &GrainContext) -> i32 {
        42
    }
}

// --- Tests ---

#[tokio::test]
async fn ask_timeout_returns_error() {
    let silo = Silo::new();
    let grain = silo.get_ref::<SlowGrain>("timeout-1");

    let result = grain.ask(SlowMessage).await;
    assert!(
        matches!(result, Err(GrainError::Timeout(_))),
        "expected Timeout error, got {:?}",
        result
    );
}

#[tokio::test]
async fn fast_handler_completes_within_timeout() {
    let silo = Silo::new();
    let grain = silo.get_ref::<SlowGrain>("timeout-2");

    let result = grain.ask(FastMessage).await;
    assert_eq!(result.unwrap(), 42);
}
