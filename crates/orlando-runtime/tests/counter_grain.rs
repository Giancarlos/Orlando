use async_trait::async_trait;
use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_runtime::Silo;

// --- State ---

#[derive(Default)]
struct CounterState {
    count: i64,
}

// --- Grain ---

struct CounterGrain;

#[async_trait]
impl Grain for CounterGrain {
    type State = CounterState;
}

// --- Messages ---

struct Increment {
    amount: i64,
}

impl Message for Increment {
    type Result = ();
}

struct GetCount;

impl Message for GetCount {
    type Result = i64;
}

// --- Handlers ---

#[async_trait]
impl GrainHandler<Increment> for CounterGrain {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> () {
        state.count += msg.amount;
    }
}

#[async_trait]
impl GrainHandler<GetCount> for CounterGrain {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// --- Tests ---

#[tokio::test]
async fn increment_and_get() {
    let silo = Silo::new();

    let counter = silo.get_ref::<CounterGrain>("my-counter");

    counter.ask(Increment { amount: 5 }).await.unwrap();
    counter.ask(Increment { amount: 3 }).await.unwrap();

    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn separate_grains_have_separate_state() {
    let silo = Silo::new();

    let a = silo.get_ref::<CounterGrain>("counter-a");
    let b = silo.get_ref::<CounterGrain>("counter-b");

    a.ask(Increment { amount: 10 }).await.unwrap();
    b.ask(Increment { amount: 3 }).await.unwrap();

    assert_eq!(a.ask(GetCount).await.unwrap(), 10);
    assert_eq!(b.ask(GetCount).await.unwrap(), 3);
}

#[tokio::test]
async fn same_key_returns_same_grain() {
    let silo = Silo::new();

    let ref1 = silo.get_ref::<CounterGrain>("shared");
    ref1.ask(Increment { amount: 7 }).await.unwrap();

    let ref2 = silo.get_ref::<CounterGrain>("shared");
    let count = ref2.ask(GetCount).await.unwrap();

    assert_eq!(count, 7);
}

// --- Grain-to-grain communication via GrainContext ---

struct Doubler;

#[derive(Default)]
struct DoublerState;

struct DoubleCount {
    counter_key: String,
}

impl Message for DoubleCount {
    type Result = i64;
}

#[async_trait]
impl Grain for Doubler {
    type State = DoublerState;
}

#[async_trait]
impl GrainHandler<DoubleCount> for Doubler {
    async fn handle(_state: &mut DoublerState, msg: DoubleCount, ctx: &GrainContext) -> i64 {
        let counter = ctx.get_ref::<CounterGrain>(&msg.counter_key);
        let count = counter.ask(GetCount).await.unwrap();
        count * 2
    }
}

#[tokio::test]
async fn grain_calls_another_grain_via_context() {
    let silo = Silo::new();

    // Set up a counter with value 5
    let counter = silo.get_ref::<CounterGrain>("my-counter");
    counter.ask(Increment { amount: 5 }).await.unwrap();

    // Doubler reads the counter via GrainContext and doubles it
    let doubler = silo.get_ref::<Doubler>("my-doubler");
    let doubled = doubler
        .ask(DoubleCount {
            counter_key: "my-counter".into(),
        })
        .await
        .unwrap();

    assert_eq!(doubled, 10);
}
