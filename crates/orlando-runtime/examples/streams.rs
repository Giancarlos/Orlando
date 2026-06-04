//! Grain-to-grain streams example.
//!
//! A `StreamProducer<T>` held in a producer grain's state fans an item out to
//! every subscribed consumer grain as a `StreamItem<T>` message (which consumers
//! handle like any other message). Items carry a monotonic per-stream sequence.
//!
//! Run with: `cargo run -p orlando-runtime --example streams`

use std::time::Duration;

use orlando_core::{GrainContext, StreamItem, StreamProducer};
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

// ── Producer ────────────────────────────────────────────────────
// The #[grain] macro has no on_activate hook, so name the stream in Default.

struct ProducerState {
    stream: StreamProducer<i64>,
}

impl Default for ProducerState {
    fn default() -> Self {
        Self { stream: StreamProducer::new("numbers") }
    }
}

#[grain(state = ProducerState)]
struct Producer;

#[message(result = ())]
struct Subscribe;

#[message(result = u64)] // returns the published item's sequence
struct Publish {
    value: i64,
}

#[grain_handler(Producer)]
async fn handle_subscribe(state: &mut ProducerState, _msg: Subscribe, ctx: &GrainContext) {
    let consumer = ctx.get_ref::<Consumer>("consumer-1");
    state.stream.subscribe::<Consumer>(consumer.sender().clone());
}

#[grain_handler(Producer)]
async fn handle_publish(state: &mut ProducerState, msg: Publish, _ctx: &GrainContext) -> u64 {
    state.stream.send(msg.value);
    state.stream.sequence()
}

// ── Consumer ────────────────────────────────────────────────────

#[derive(Default)]
struct ConsumerState {
    received: Vec<(u64, i64)>,
}

#[grain(state = ConsumerState)]
struct Consumer;

// Consumers receive stream items as a regular message.
#[grain_handler(Consumer)]
async fn handle_item(state: &mut ConsumerState, msg: StreamItem<i64>, _ctx: &GrainContext) {
    state.received.push((msg.sequence, msg.item));
}

#[message(result = Vec<(u64, i64)>)]
struct GetReceived;

#[grain_handler(Consumer)]
async fn handle_get(state: &mut ConsumerState, _msg: GetReceived, _ctx: &GrainContext) -> Vec<(u64, i64)> {
    state.received.clone()
}

#[tokio::main]
async fn main() {
    let silo = Silo::new();
    let producer = silo.get_ref::<Producer>("prod");
    let consumer = silo.get_ref::<Consumer>("consumer-1");

    producer.ask(Subscribe).await.unwrap();

    for value in [10, 20, 30] {
        producer.ask(Publish { value }).await.unwrap();
        println!("published {value}");
    }

    // Stream delivery is fire-and-forget into the consumer's mailbox; give it a
    // moment to process before reading back.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let received = consumer.ask(GetReceived).await.unwrap();
    println!("consumer received: {received:?}");
    assert_eq!(received, vec![(0, 10), (1, 20), (2, 30)]);
    println!("all 3 items delivered in order with sequence numbers ✓");
}
