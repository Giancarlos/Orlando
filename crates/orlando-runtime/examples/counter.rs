use orlando_core::GrainContext;
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

// ── State ───────────────────────────────────────────────────────

#[derive(Default)]
struct CounterState {
    count: i64,
}

// ── Grain ───────────────────────────────────────────────────────

#[grain(state = CounterState)]
struct Counter;

// ── Messages ────────────────────────────────────────────────────

#[message(result = i64)]
struct Increment {
    amount: i64,
}

#[message(result = i64)]
struct GetCount;

// ── Handlers ────────────────────────────────────────────────────

#[grain_handler(Counter)]
async fn handle_increment(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
    state.count += msg.amount;
    state.count
}

#[grain_handler(Counter)]
async fn handle_get(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
    state.count
}

// ── Main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let silo = Silo::new();

    let counter = silo.get_ref::<Counter>("my-counter");

    println!("Incrementing by 5...");
    let count = counter.ask(Increment { amount: 5 }).await.unwrap();
    println!("  count = {count}");

    println!("Incrementing by 3...");
    let count = counter.ask(Increment { amount: 3 }).await.unwrap();
    println!("  count = {count}");

    let count = counter.ask(GetCount).await.unwrap();
    println!("Final count: {count}");
}
