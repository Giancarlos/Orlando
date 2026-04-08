use std::time::Duration;

use async_trait::async_trait;
use orlando_core::{Grain, GrainContext, GrainError, GrainHandler, Message};
use orlando_runtime::Silo;

// --- Grain A: calls Grain B ---

#[derive(Default)]
struct StateA;

struct GrainA;

#[async_trait]
impl Grain for GrainA {
    type State = StateA;

    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }
}

struct CallB;

impl Message for CallB {
    type Result = String;
}

#[async_trait]
impl GrainHandler<CallB> for GrainA {
    async fn handle(_state: &mut StateA, _msg: CallB, ctx: &GrainContext) -> String {
        // A calls B, which will try to call back to A (creating a cycle)
        let b = ctx.get_ref::<GrainB>("b");
        match b.ask(CallA).await {
            Ok(v) => v,
            Err(GrainError::DeadlockDetected(chain)) => format!("deadlock: {}", chain),
            Err(e) => format!("error: {}", e),
        }
    }
}

// --- Grain B: calls Grain A back (completing the cycle) ---

#[derive(Default)]
struct StateB;

struct GrainB;

#[async_trait]
impl Grain for GrainB {
    type State = StateB;

    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }
}

struct CallA;

impl Message for CallA {
    type Result = String;
}

#[async_trait]
impl GrainHandler<CallA> for GrainB {
    async fn handle(_state: &mut StateB, _msg: CallA, ctx: &GrainContext) -> String {
        // B calls A back -- this should detect the deadlock
        let a = ctx.get_ref::<GrainA>("a");
        match a.ask(CallB).await {
            Ok(v) => v,
            Err(GrainError::DeadlockDetected(chain)) => format!("deadlock: {}", chain),
            Err(e) => format!("error: {}", e),
        }
    }
}

// --- Tests ---

#[tokio::test]
async fn detects_circular_grain_call() {
    let silo = Silo::new();
    let a = silo.get_ref::<GrainA>("a");
    let result = a.ask(CallB).await.unwrap();
    assert!(
        result.starts_with("deadlock:"),
        "expected deadlock detection, got: {}",
        result
    );
}
