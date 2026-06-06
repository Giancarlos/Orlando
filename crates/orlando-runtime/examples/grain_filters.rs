//! Grain call filters (interceptors) example.
//!
//! A `GrainCallFilter` runs cross-cutting logic around every `ask()` — logging,
//! metrics, auth — without touching grain code. Filters see call *metadata*
//! (grain id, message type, start time), not the payload. `on_before` can reject
//! a call by returning `Err`; `on_after` observes the outcome.
//!
//! Run with: `cargo run -p orlando-runtime --example grain_filters`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use orlando_core::{GrainCallFilter, GrainCallInfo, GrainContext};
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

// ── A logging filter: prints each call and its latency ──────────

struct LoggingFilter;

#[async_trait]
impl GrainCallFilter for LoggingFilter {
    async fn on_before(&self, info: &GrainCallInfo) -> Result<(), String> {
        println!("→ {}::{} on {}", info.grain_id.type_name, info.message_type, info.grain_id.key);
        Ok(())
    }

    async fn on_after(&self, info: &GrainCallInfo, result_ok: bool) {
        let status = if result_ok { "ok" } else { "err" };
        println!(
            "← {}::{} [{status}] in {:?}",
            info.grain_id.type_name,
            info.message_type,
            info.started_at.elapsed()
        );
    }
}

// ── A metrics filter: counts total calls across all grains ──────

#[derive(Default)]
struct CountingFilter {
    calls: AtomicUsize,
}

#[async_trait]
impl GrainCallFilter for CountingFilter {
    async fn on_before(&self, _info: &GrainCallInfo) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

// ── Grain ───────────────────────────────────────────────────────

#[derive(Default)]
struct CounterState {
    count: i64,
}

#[grain(state = CounterState)]
struct Counter;

#[message(result = i64)]
struct Increment {
    amount: i64,
}

#[grain_handler(Counter)]
async fn handle_increment(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
    state.count += msg.amount;
    state.count
}

// ── Main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let counting = Arc::new(CountingFilter::default());

    // Filters run in registration order, around every ask().
    let silo = Silo::builder()
        .filter(Arc::new(LoggingFilter))
        .filter(counting.clone())
        .build();

    let counter = silo.get_ref::<Counter>("demo");

    for amount in [5, 3, 10] {
        let count = counter.ask(Increment { amount }).await.unwrap();
        println!("  count = {count}\n");
    }

    println!("total calls observed by CountingFilter: {}", counting.calls.load(Ordering::Relaxed));
}
