use std::time::Duration;

use async_trait::async_trait;

use crate::grain_context::GrainContext;
use crate::Message;

/// A virtual-actor grain type: its state and lifecycle configuration.
///
/// Implement this (usually via `#[grain]`) to define a grain. The runtime
/// activates a grain on first message and deactivates it after
/// [`idle_timeout`](Grain::idle_timeout). Per the no-threading invariant, one
/// activation processes one message at a time and owns its `State` exclusively.
#[async_trait]
pub trait Grain: Send + 'static {
    /// The grain's private state, constructed via `Default` on activation.
    type State: Default + Send + 'static;

    /// Lifecycle hook run once after the state is constructed/loaded, before the
    /// first message. Default: no-op.
    async fn on_activate(_state: &mut Self::State, _ctx: &GrainContext) {}
    /// Lifecycle hook run once during graceful deactivation, after the last
    /// message. Not guaranteed to run on a crash. Default: no-op.
    async fn on_deactivate(_state: &mut Self::State, _ctx: &GrainContext) {}

    /// How long an activation may sit idle (no messages) before the runtime
    /// deactivates it to free resources. Default: 5 minutes.
    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }

    /// Maximum time to wait for a handler to respond to an `ask()` call.
    /// If the handler takes longer, the caller receives `GrainError::Timeout`.
    fn ask_timeout() -> Duration {
        Duration::from_secs(30)
    }

    /// Stable, human-readable name for this grain type.
    ///
    /// Used as the registry key and hash ring key for grain placement.
    /// External clients use this name to address grains (e.g., "Counter").
    /// Defaults to `std::any::type_name::<Self>()` for backward compatibility.
    /// Override via `#[grain(state = T, name = "Counter")]` for external access.
    fn grain_type_name() -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Optional placement hint for this grain type.
    ///
    /// Returns a strategy name: `"hash"`, `"prefer_local"`, `"random"`, or `None`
    /// (use silo default). Override via `#[grain(state = S, placement = "prefer_local")]`.
    fn placement_hint() -> Option<&'static str> {
        None
    }

    /// Named storage provider for this grain's persisted state.
    ///
    /// When using `PersistentSilo` with multiple registered stores,
    /// this determines which backend is used. Defaults to `"default"`.
    /// Override via `#[grain(state = S, storage = "postgres")]`.
    fn storage_provider() -> &'static str {
        "default"
    }

    /// Clusters where this grain type may be activated (data residency).
    ///
    /// Returns `None` for no restriction (activate in any cluster).
    /// Returns `Some(&["eu-west"])` to pin activations to the EU cluster only.
    /// The cross-cluster directory and transport enforce this constraint:
    /// if the local cluster is not in the allowed list, the request is
    /// forwarded to an allowed cluster instead of activating locally.
    fn allowed_clusters() -> Option<&'static [&'static str]> {
        None
    }

    /// Whether this grain uses the reentrant mailbox for concurrent message dispatch.
    ///
    /// When true, messages are dequeued into concurrent tasks instead of being
    /// processed strictly one at a time. State access is serialized via an async
    /// mutex, so handlers still execute one at a time with respect to state — but
    /// the mailbox is not blocked while a handler awaits outgoing calls.
    ///
    /// Note: handlers hold the state mutex for their full duration, so circular
    /// grain call chains (A -> B -> A) will still deadlock.
    fn reentrant() -> bool {
        false
    }
}

/// Handles one message type `M` for a grain. Implement once per message type
/// the grain accepts (typically via `#[grain_handler]`).
#[async_trait]
pub trait GrainHandler<M: Message>: Grain {
    /// Process `msg` with exclusive access to `state`, returning the reply.
    async fn handle(state: &mut Self::State, msg: M, ctx: &GrainContext) -> M::Result;
}

/// Marker trait for stateless worker grains that can run multiple
/// concurrent activations for the same grain key.
///
/// Each activation has its own independent `State::default()` and mailbox.
/// Messages are dispatched round-robin across the pool.
pub trait StatelessWorker: Grain {
    /// Maximum number of concurrent activations per grain key.
    fn max_activations() -> usize {
        4
    }
}
