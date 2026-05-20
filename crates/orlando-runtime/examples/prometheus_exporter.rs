//! Runnable example: expose grain metrics over a Prometheus scrape endpoint.
//!
//! Installs `metrics-exporter-prometheus` as the global recorder, builds a
//! `Silo` with `MetricsFilter`, and drives a `Counter` grain in a loop to
//! generate traffic. The `MetricsFilter` records call counts, latency, and
//! errors for every `ask()`; the exporter publishes them at
//! `http://127.0.0.1:9090/metrics`.
//!
//! Run with:
//! ```sh
//! cargo run -p orlando-runtime --example prometheus_exporter
//! # then, in another shell:
//! curl -s http://127.0.0.1:9090/metrics | grep orlando
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use metrics_exporter_prometheus::PrometheusBuilder;
use orlando_core::GrainContext;
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::{MetricsFilter, Silo};

#[derive(Default)]
struct CounterState {
    count: i64,
}

#[grain(state = CounterState)]
struct Counter;

#[message(result = i64)]
struct Increment {
    amount: i64,
}

#[grain_handler(Counter)]
async fn handle_increment(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
    state.count += msg.amount;
    state.count
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Install the Prometheus recorder + background HTTP listener.
    // Must run inside a tokio runtime — install() spawns the listener task.
    let listen: SocketAddr = "127.0.0.1:9090".parse().unwrap();
    PrometheusBuilder::new()
        .with_http_listener(listen)
        .install()
        .expect("failed to install Prometheus exporter");

    println!("Prometheus metrics available at http://{listen}/metrics");
    println!("Generating grain traffic (Ctrl-C to stop)...");

    let silo = Silo::builder()
        .filter(Arc::new(MetricsFilter::new()))
        .build();

    let counter = silo.get_ref::<Counter>("demo");

    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        tick.tick().await;
        let total = counter.ask(Increment { amount: 1 }).await.unwrap();
        if total % 25 == 0 {
            println!("  counter = {total} (scrape /metrics to see orlando.grain.* series)");
        }
    }
}
