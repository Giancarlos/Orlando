//! Backpressure example.
//!
//! A grain processes one message at a time, so a slow handler lets work pile up
//! in its mailbox. `try_ask()` fails fast with `GrainError::MailboxFull` instead
//! of waiting when the mailbox (capacity `MAILBOX_CAPACITY`) is full, and
//! `mailbox_pressure()` reports utilization (0.0–1.0) — together they let
//! callers shed load rather than queue unboundedly.
//!
//! Run with: `cargo run -p orlando-runtime --example backpressure`

use std::time::Duration;

use orlando_core::{GrainContext, GrainError, MAILBOX_CAPACITY};
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

#[derive(Default)]
struct WorkerState;

#[grain(state = WorkerState)]
struct SlowWorker;

#[message(result = ())]
struct Work;

#[grain_handler(SlowWorker)]
async fn handle_work(_state: &mut WorkerState, _msg: Work, _ctx: &GrainContext) {
    // Slow handler: the mailbox fills faster than it drains.
    tokio::time::sleep(Duration::from_millis(5)).await;
}

#[tokio::main]
async fn main() {
    let silo = Silo::new();
    let worker = silo.get_ref::<SlowWorker>("slow");

    // Flood the grain with far more requests than its mailbox can hold.
    let burst = MAILBOX_CAPACITY * 2;
    println!("mailbox capacity: {MAILBOX_CAPACITY}; firing {burst} requests with try_ask...");

    let mut handles = Vec::with_capacity(burst);
    for _ in 0..burst {
        let worker = worker.clone();
        handles.push(tokio::spawn(async move { worker.try_ask(Work).await }));
    }

    let (mut accepted, mut rejected) = (0u32, 0u32);
    for handle in handles {
        match handle.await.unwrap() {
            Ok(()) => accepted += 1,
            Err(GrainError::MailboxFull) => rejected += 1,
            Err(e) => println!("  unexpected error: {e}"),
        }
    }

    println!("accepted: {accepted}, rejected (MailboxFull): {rejected}");
    println!(
        "backpressure shed {rejected} requests instead of queueing unboundedly; \
         final pressure = {:.2}",
        worker.mailbox_pressure()
    );
}
