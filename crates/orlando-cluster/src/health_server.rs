//! HTTP health endpoints for orchestrator probes (Kubernetes, Nomad, etc.).
//!
//! Two endpoints:
//! - `GET /healthz` — process liveness. Always returns `200 OK` as long as
//!   the axum runtime is serving. Use as the liveness probe.
//! - `GET /readyz` — readiness. Returns `200 OK` when the silo has joined
//!   the ring AND the optional store probe succeeds; `503 Service
//!   Unavailable` otherwise. Use as the readiness probe.

use std::net::SocketAddr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::sync::watch;

use crate::hash_ring::HashRing;

/// Async probe checking that a backing store (DB, Redis, etc.) is reachable.
///
/// Returns `Ok(())` when the dependency is healthy. Implementations should
/// keep this cheap (a single round-trip or cached probe) — readiness probes
/// run frequently.
pub type StoreProbe = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone)]
struct HealthState {
    ring: Arc<ArcSwap<HashRing>>,
    store_probe: Option<StoreProbe>,
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(state): State<HealthState>) -> impl IntoResponse {
    if state.ring.load().members().is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "ring empty");
    }
    if let Some(probe) = &state.store_probe
        && let Err(e) = probe().await
    {
        tracing::debug!(error = %e, "readyz: store probe failed");
        return (StatusCode::SERVICE_UNAVAILABLE, "store probe failed");
    }
    (StatusCode::OK, "ready")
}

/// Run the health HTTP server until `shutdown_rx` flips to `true`.
///
/// Intended to be spawned via `tokio::spawn`. Errors are logged and the
/// task returns; the silo continues running.
pub async fn run_health_server(
    addr: SocketAddr,
    ring: Arc<ArcSwap<HashRing>>,
    store_probe: Option<StoreProbe>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let state = HealthState { ring, store_probe };
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(addr = %addr, error = %e, "health server: failed to bind");
            return;
        }
    };

    tracing::info!(addr = %addr, "health server listening");

    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = shutdown_rx.changed().await;
    });

    if let Err(e) = serve.await {
        tracing::error!(error = %e, "health server exited with error");
    }
}
