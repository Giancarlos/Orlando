use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainHandler, Message, StatelessWorker};
use orlando_runtime::Silo;

// --- State ---

#[derive(Default)]
struct WorkerState {
    call_count: u64,
}

// --- Grains ---

struct EchoWorker;

#[async_trait]
impl Grain for EchoWorker {
    type State = WorkerState;
}

impl StatelessWorker for EchoWorker {
    fn max_activations() -> usize {
        3
    }
}

// Worker with short idle timeout for reactivation testing
struct ShortLivedWorker;

#[async_trait]
impl Grain for ShortLivedWorker {
    type State = WorkerState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(50)
    }
}

impl StatelessWorker for ShortLivedWorker {
    fn max_activations() -> usize {
        2
    }
}

// --- Messages ---

struct Echo {
    value: i64,
}
impl Message for Echo {
    type Result = i64;
}

struct SlowEcho {
    value: i64,
}
impl Message for SlowEcho {
    type Result = i64;
}

struct GetCallCount;
impl Message for GetCallCount {
    type Result = u64;
}

// --- Handlers ---

#[async_trait]
impl GrainHandler<Echo> for EchoWorker {
    async fn handle(_state: &mut WorkerState, msg: Echo, _ctx: &GrainContext) -> i64 {
        msg.value
    }
}

#[async_trait]
impl GrainHandler<SlowEcho> for EchoWorker {
    async fn handle(_state: &mut WorkerState, msg: SlowEcho, _ctx: &GrainContext) -> i64 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        msg.value
    }
}

#[async_trait]
impl GrainHandler<GetCallCount> for EchoWorker {
    async fn handle(
        state: &mut WorkerState,
        _msg: GetCallCount,
        _ctx: &GrainContext,
    ) -> u64 {
        state.call_count += 1;
        state.call_count
    }
}

#[async_trait]
impl GrainHandler<Echo> for ShortLivedWorker {
    async fn handle(_state: &mut WorkerState, msg: Echo, _ctx: &GrainContext) -> i64 {
        msg.value
    }
}

#[async_trait]
impl GrainHandler<GetCallCount> for ShortLivedWorker {
    async fn handle(
        state: &mut WorkerState,
        _msg: GetCallCount,
        _ctx: &GrainContext,
    ) -> u64 {
        state.call_count += 1;
        state.call_count
    }
}

// --- Tests ---

#[tokio::test]
async fn worker_pool_creates_correct_size() {
    let silo = Silo::new();
    let worker = silo.get_worker_ref::<EchoWorker>("pool-size");
    assert_eq!(worker.pool_size(), 3);
}

#[tokio::test]
async fn worker_handles_messages() {
    let silo = Silo::new();
    let worker = silo.get_worker_ref::<EchoWorker>("echo");

    let result = worker.ask(Echo { value: 42 }).await.unwrap();
    assert_eq!(result, 42);

    let result = worker.ask(Echo { value: 99 }).await.unwrap();
    assert_eq!(result, 99);
}

#[tokio::test]
async fn worker_round_robin_dispatch() {
    let silo = Silo::new();
    let worker = silo.get_worker_ref::<EchoWorker>("round-robin");

    // Send GetCallCount to each worker via round-robin.
    // Each worker has independent state, so each should return 1 on first call.
    let c1 = worker.ask(GetCallCount).await.unwrap();
    let c2 = worker.ask(GetCallCount).await.unwrap();
    let c3 = worker.ask(GetCallCount).await.unwrap();

    // Each activation saw exactly one call
    assert_eq!(c1, 1);
    assert_eq!(c2, 1);
    assert_eq!(c3, 1);

    // Fourth call wraps to first activation: call_count becomes 2
    let c4 = worker.ask(GetCallCount).await.unwrap();
    assert_eq!(c4, 2);
}

#[tokio::test]
async fn worker_concurrent_throughput() {
    let silo = Silo::new();
    let worker = silo.get_worker_ref::<EchoWorker>("throughput");

    let completed = Arc::new(AtomicUsize::new(0));

    // Spawn 9 concurrent slow tasks (3 workers, 50ms each).
    // With 3 workers processing in parallel, should complete in ~150ms (3 batches of 3)
    // rather than ~450ms (sequential).
    let mut handles = Vec::new();
    for i in 0..9 {
        let w = worker.clone();
        let c = completed.clone();
        handles.push(tokio::spawn(async move {
            let result = w.ask(SlowEcho { value: i }).await.unwrap();
            c.fetch_add(1, Ordering::Relaxed);
            result
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(completed.load(Ordering::Relaxed), 9);
}

#[tokio::test]
async fn worker_pool_reactivation_after_idle_timeout() {
    let silo = Silo::new();

    // Send a message to create the pool
    let worker = silo.get_worker_ref::<ShortLivedWorker>("reactivate");
    let result = worker.ask(Echo { value: 1 }).await.unwrap();
    assert_eq!(result, 1);

    // Wait for all workers to idle-timeout and deactivate (50ms + margin)
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get a new ref — should create a fresh pool
    let worker = silo.get_worker_ref::<ShortLivedWorker>("reactivate");

    // Messages should work on the fresh pool
    let result = worker.ask(Echo { value: 42 }).await.unwrap();
    assert_eq!(result, 42);

    // Call count should be 1 on each (fresh state from reactivation)
    let c1 = worker.ask(GetCallCount).await.unwrap();
    let c2 = worker.ask(GetCallCount).await.unwrap();
    assert_eq!(c1, 1, "first worker should have fresh state after reactivation");
    assert_eq!(c2, 1, "second worker should have fresh state after reactivation");
}

#[tokio::test]
async fn worker_independent_state_per_activation() {
    let silo = Silo::new();
    let worker = silo.get_worker_ref::<EchoWorker>("independent");

    // Send 6 GetCallCount messages (2 per worker with pool size 3)
    let mut counts = Vec::new();
    for _ in 0..6 {
        counts.push(worker.ask(GetCallCount).await.unwrap());
    }

    // Round-robin: workers 0,1,2 get call 1, then workers 0,1,2 get call 2
    assert_eq!(counts, vec![1, 1, 1, 2, 2, 2]);
}
