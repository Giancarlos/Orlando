use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_runtime::Silo;

// --- Reentrant counter grain ---

#[derive(Default)]
struct CounterState {
    count: i64,
}

struct ReentrantCounter;

#[async_trait]
impl Grain for ReentrantCounter {
    type State = CounterState;

    fn reentrant() -> bool {
        true
    }
}

struct Increment {
    amount: i64,
}
impl Message for Increment {
    type Result = i64;
}

struct GetCount;
impl Message for GetCount {
    type Result = i64;
}

struct SlowIncrement {
    amount: i64,
}
impl Message for SlowIncrement {
    type Result = i64;
}

#[async_trait]
impl GrainHandler<Increment> for ReentrantCounter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for ReentrantCounter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

#[async_trait]
impl GrainHandler<SlowIncrement> for ReentrantCounter {
    async fn handle(state: &mut CounterState, msg: SlowIncrement, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        // Release the lock implicitly when the handler completes;
        // but the sleep here holds the lock, proving concurrent dispatching
        // (messages are dequeued concurrently even though the mutex serializes access)
        tokio::time::sleep(Duration::from_millis(50)).await;
        state.count
    }
}

// --- Tests ---

#[tokio::test]
async fn reentrant_basic_increment() {
    let silo = Silo::new();
    let counter = silo.get_ref::<ReentrantCounter>("reentrant-1");

    let count = counter.ask(Increment { amount: 5 }).await.unwrap();
    assert_eq!(count, 5);

    let count = counter.ask(Increment { amount: 3 }).await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn reentrant_concurrent_dispatch() {
    // The reentrant mailbox dispatches messages concurrently.
    // Even though the mutex serializes state access, multiple messages
    // are dequeued and dispatched without waiting for the previous to finish.
    let silo = Silo::new();
    let counter = silo.get_ref::<ReentrantCounter>("concurrent");

    let completed = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..5 {
        let c = counter.clone();
        let done = completed.clone();
        handles.push(tokio::spawn(async move {
            c.ask(SlowIncrement { amount: i + 1 }).await.unwrap();
            done.fetch_add(1, Ordering::Relaxed);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(completed.load(Ordering::Relaxed), 5);

    // All 5 increments should have been applied (1+2+3+4+5 = 15)
    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 15);
}

#[tokio::test]
async fn reentrant_state_consistency() {
    // Send 100 concurrent increments of 1 each. Final count must be exactly 100.
    let silo = Silo::new();
    let counter = silo.get_ref::<ReentrantCounter>("consistency");

    let mut handles = Vec::new();
    for _ in 0..100 {
        let c = counter.clone();
        handles.push(tokio::spawn(async move {
            c.ask(Increment { amount: 1 }).await.unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 100, "all 100 increments must be applied");
}

// --- Cross-grain call from reentrant grain (non-circular) ---

#[derive(Default)]
struct AdderState {
    total: i64,
}

struct AdderGrain;

#[async_trait]
impl Grain for AdderGrain {
    type State = AdderState;
}

struct Add {
    amount: i64,
}
impl Message for Add {
    type Result = i64;
}

#[async_trait]
impl GrainHandler<Add> for AdderGrain {
    async fn handle(state: &mut AdderState, msg: Add, _ctx: &GrainContext) -> i64 {
        state.total += msg.amount;
        state.total
    }
}

// Reentrant grain that calls another (non-reentrant) grain
struct DelegatorGrain;

#[derive(Default)]
struct DelegatorState;

#[async_trait]
impl Grain for DelegatorGrain {
    type State = DelegatorState;

    fn reentrant() -> bool {
        true
    }
}

struct DelegateAdd {
    target_key: String,
    amount: i64,
}
impl Message for DelegateAdd {
    type Result = i64;
}

#[async_trait]
impl GrainHandler<DelegateAdd> for DelegatorGrain {
    async fn handle(
        _state: &mut DelegatorState,
        msg: DelegateAdd,
        ctx: &GrainContext,
    ) -> i64 {
        let adder = ctx.get_ref::<AdderGrain>(&msg.target_key);
        adder.ask(Add { amount: msg.amount }).await.unwrap()
    }
}

#[tokio::test]
async fn reentrant_grain_calls_other_grain() {
    let silo = Silo::new();
    let delegator = silo.get_ref::<DelegatorGrain>("d1");

    // Multiple concurrent calls from reentrant grain to an adder grain.
    // All delegate to the same adder, so the final total should be 1+2+3 = 6.
    let mut handles = Vec::new();
    for i in 1..=3 {
        let d = delegator.clone();
        handles.push(tokio::spawn(async move {
            d.ask(DelegateAdd {
                target_key: "adder-1".to_string(),
                amount: i,
            })
            .await
            .unwrap()
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    // Verify the adder accumulated all values
    let adder = silo.get_ref::<AdderGrain>("adder-1");
    let total = adder.ask(Add { amount: 0 }).await.unwrap();
    assert_eq!(total, 6);
}
