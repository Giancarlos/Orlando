//! Stateless worker pool example.
//!
//! A stateless worker grain runs a *pool* of independent activations for the
//! same key, and messages are dispatched round-robin across them. Unlike a
//! normal grain (one activation, one message at a time), this lets CPU- or
//! IO-heavy work run concurrently — at the cost of no shared state between
//! activations (each has its own `State::default()`).
//!
//! Run with: `cargo run -p orlando-runtime --example stateless_worker`

use std::time::Duration;

use orlando_core::GrainContext;
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

// ── State ───────────────────────────────────────────────────────
// Each activation in the pool owns its own copy of this state.

#[derive(Default)]
struct WorkerState {
    processed: u64,
}

// ── Grain ───────────────────────────────────────────────────────
// `stateless_worker` makes this a pool; `max_activations` sets its size.

#[grain(state = WorkerState, stateless_worker, max_activations = 4)]
struct HashWorker;

// ── Messages ────────────────────────────────────────────────────

#[message(result = u64)]
struct Hash {
    input: String,
}

// ── Handlers ────────────────────────────────────────────────────

#[grain_handler(HashWorker)]
async fn handle_hash(state: &mut WorkerState, msg: Hash, _ctx: &GrainContext) -> u64 {
    // Simulate a little work so concurrency across the pool is observable.
    tokio::time::sleep(Duration::from_millis(50)).await;
    state.processed += 1;

    // FNV-1a hash of the input.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in msg.input.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ── Main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let silo = Silo::new();
    let worker = silo.get_worker_ref::<HashWorker>("hashers");
    println!("worker pool size: {}", worker.pool_size());

    // Fire 8 requests concurrently. With a pool of 4 and ~50ms each, all 8
    // complete in ~2 rounds rather than 8 sequential delays — the messages are
    // spread across the pool's activations.
    let started = tokio::time::Instant::now();
    let mut handles = Vec::new();
    for i in 0..8 {
        let worker = worker.clone();
        handles.push(tokio::spawn(async move {
            let input = format!("payload-{i}");
            let hash = worker.ask(Hash { input: input.clone() }).await.unwrap();
            (input, hash)
        }));
    }

    for handle in handles {
        let (input, hash) = handle.await.unwrap();
        println!("  {input} -> {hash:#018x}");
    }

    println!("8 requests completed in {:?} (pool of 4)", started.elapsed());
}
