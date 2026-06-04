//! Tests for co-hosted grain services (background tasks in a silo).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_runtime::{GrainService, Silo};

#[derive(Default)]
struct CounterState {
    count: i64,
}

struct Counter;

#[async_trait]
impl Grain for Counter {
    type State = CounterState;
}

struct Bump;
impl Message for Bump {
    type Result = ();
}

struct GetCount;
impl Message for GetCount {
    type Result = i64;
}

#[async_trait]
impl GrainHandler<Bump> for Counter {
    async fn handle(state: &mut CounterState, _msg: Bump, _ctx: &GrainContext) {
        state.count += 1;
    }
}

#[async_trait]
impl GrainHandler<GetCount> for Counter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

/// A service that bumps a counter grain three times, then idles until cancelled.
struct BumperService;

#[async_trait]
impl GrainService for BumperService {
    async fn run(&self, ctx: GrainContext) {
        let counter = ctx.get_ref::<Counter>("svc-counter");
        for _ in 0..3 {
            let _ = counter.ask(Bump).await;
        }
        // Idle until the silo signals shutdown.
        while !ctx.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

#[tokio::test]
async fn grain_service_runs_and_calls_grains() {
    let silo = Silo::builder().add_service(Arc::new(BumperService)).build();

    // Give the service time to run its three bumps.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let count = silo.get_ref::<Counter>("svc-counter").ask(GetCount).await.unwrap();
    assert_eq!(count, 3, "the co-hosted service should have bumped the counter 3 times");

    // Shutdown cancels the service; its task should complete promptly.
    tokio::time::timeout(Duration::from_secs(2), silo.shutdown_services())
        .await
        .expect("services must stop promptly on shutdown");
}

// A service that ticks a shared counter until cancelled.
static TICKS: AtomicUsize = AtomicUsize::new(0);

struct TickerService;

#[async_trait]
impl GrainService for TickerService {
    async fn run(&self, ctx: GrainContext) {
        while !ctx.is_cancelled() {
            TICKS.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// Dropping the silo without calling shutdown_services() must still stop the
/// service (the Drop impl cancels the token) — otherwise the task leaks forever.
#[tokio::test]
async fn dropping_silo_stops_services() {
    {
        let _silo = Silo::builder().add_service(Arc::new(TickerService)).build();
        tokio::time::sleep(Duration::from_millis(30)).await;
        // _silo dropped here — no explicit shutdown_services().
    }

    // Give the service a moment to observe cancellation and exit, then confirm
    // the tick count has stopped advancing.
    tokio::time::sleep(Duration::from_millis(40)).await;
    let a = TICKS.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(40)).await;
    let b = TICKS.load(Ordering::SeqCst);

    assert!(a > 0, "service should have ticked while the silo was alive");
    assert_eq!(a, b, "service must stop after the silo is dropped (a={a}, b={b}) — no leak");
}
