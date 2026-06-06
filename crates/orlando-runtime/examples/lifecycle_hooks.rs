//! Silo lifecycle hooks example.
//!
//! `on_startup` hooks run while the silo is built (e.g. warm caches, register
//! metrics); `on_shutdown` hooks run when you call `run_shutdown_hooks()` during
//! a graceful stop (e.g. flush buffers, close connections).
//!
//! Run with: `cargo run -p orlando-runtime --example lifecycle_hooks`

use orlando_core::GrainContext;
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

#[derive(Default)]
struct CounterState {
    count: i64,
}

#[grain(state = CounterState)]
struct Counter;

#[message(result = i64)]
struct Increment(i64);

#[grain_handler(Counter)]
async fn handle_increment(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
    state.count += msg.0;
    state.count
}

#[tokio::main]
async fn main() {
    let silo = Silo::builder()
        .on_startup(|| println!("[startup] warming up"))
        .on_shutdown(|| println!("[shutdown] flushing buffers"))
        .build();
    println!("silo built (startup hooks have run)");

    let counter = silo.get_ref::<Counter>("demo");
    let count = counter.ask(Increment(7)).await.unwrap();
    println!("did some work: count = {count}");

    // Graceful stop: run the shutdown hooks.
    println!("shutting down...");
    silo.run_shutdown_hooks();
    println!("done");
}
