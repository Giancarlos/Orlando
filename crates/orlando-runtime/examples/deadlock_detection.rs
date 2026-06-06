//! Deadlock detection example.
//!
//! Because a grain processes one message at a time, a circular call chain
//! (A → B → A) would deadlock: A is busy awaiting B, which is awaiting A. Orlando
//! tracks the call chain in the request context and breaks the cycle by returning
//! `GrainError::DeadlockDetected` instead of hanging forever.
//!
//! Run with: `cargo run -p orlando-runtime --example deadlock_detection`

use orlando_core::GrainContext;
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

#[derive(Default)]
struct AState;
#[derive(Default)]
struct BState;

#[grain(state = AState)]
struct A;
#[grain(state = BState)]
struct B;

#[message(result = String)]
struct Start;

#[message(result = String)]
struct CallBack;

// A's handler calls into B...
#[grain_handler(A)]
async fn a_start(_s: &mut AState, _m: Start, ctx: &GrainContext) -> String {
    let r = ctx.get_ref::<B>("b").ask(CallBack).await;
    format!("A called B -> {r:?}")
}

// ...and B's handler calls back into A, completing the cycle A -> B -> A.
#[grain_handler(B)]
async fn b_callback(_s: &mut BState, _m: CallBack, ctx: &GrainContext) -> String {
    match ctx.get_ref::<A>("a").ask(Start).await {
        Ok(s) => format!("B's call to A returned: {s}"),
        Err(e) => format!("B's call back to A was rejected: {e}"),
    }
}

#[tokio::main]
async fn main() {
    let silo = Silo::new();
    let a = silo.get_ref::<A>("a");

    // Without cycle detection this would hang forever; instead the inner A->...->A
    // call returns GrainError::DeadlockDetected and the chain unwinds.
    let result = a.ask(Start).await.unwrap();
    println!("{result}");
    assert!(result.contains("deadlock detected"), "the cycle must be detected, got: {result}");
    println!("\ncircular call chain detected and broken (no hang) ✓");
}
