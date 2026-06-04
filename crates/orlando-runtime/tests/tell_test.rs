//! Tests for one-way messages (`GrainRef::tell`): fire-and-forget delivery with
//! in-order processing relative to later `ask`s.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainHandler, Message, StatelessWorker};
use orlando_runtime::Silo;

#[derive(Default)]
struct CounterState {
    count: i64,
}

struct Counter;

#[async_trait]
impl Grain for Counter {
    type State = CounterState;
}

struct Increment(i64);
impl Message for Increment {
    type Result = i64;
}

struct GetCount;
impl Message for GetCount {
    type Result = i64;
}

#[async_trait]
impl GrainHandler<Increment> for Counter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.0;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for Counter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

#[tokio::test]
async fn tell_is_fire_and_forget_and_ordered() {
    let silo = Silo::new();
    let counter = silo.get_ref::<Counter>("c");

    // tell returns once enqueued, without waiting for the handler/result.
    counter.tell(Increment(5)).await.unwrap();
    counter.tell(Increment(3)).await.unwrap();

    // A later ask is enqueued after the tells, so it observes their effects
    // (one-message-at-a-time, FIFO).
    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 8, "earlier tells must be processed before a later ask");
}

#[tokio::test]
async fn tell_on_closed_mailbox_errors() {
    use orlando_core::GrainError;

    // A ref whose mailbox is closed: build a channel, drop the receiver.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);
    let counter = orlando_core::GrainRef::<Counter>::new(tx);

    let result = counter.tell(Increment(1)).await;
    assert!(matches!(result, Err(GrainError::MailboxClosed)));
}

// --- Worker-pool one-way fan-out ---

static PROCESSED: AtomicUsize = AtomicUsize::new(0);

struct PoolWorker;

#[async_trait]
impl Grain for PoolWorker {
    type State = ();
}

impl StatelessWorker for PoolWorker {
    fn max_activations() -> usize {
        3
    }
}

struct Work;
impl Message for Work {
    type Result = ();
}

#[async_trait]
impl GrainHandler<Work> for PoolWorker {
    async fn handle(_state: &mut (), _msg: Work, _ctx: &GrainContext) {
        PROCESSED.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn worker_tell_delivers_every_message_across_the_pool() {
    let silo = Silo::new();
    let worker = silo.get_worker_ref::<PoolWorker>("w");
    assert_eq!(worker.pool_size(), 3);

    for _ in 0..9 {
        worker.tell(Work).await.unwrap();
    }

    // Fire-and-forget: handlers run async across the pool. Give them a moment.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        PROCESSED.load(Ordering::SeqCst),
        9,
        "all 9 one-way messages must be processed across the pool"
    );
}
