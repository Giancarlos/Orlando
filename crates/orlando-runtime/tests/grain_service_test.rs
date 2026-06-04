//! Tests for co-hosted grain services (background tasks in a silo).

use std::sync::Arc;
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
