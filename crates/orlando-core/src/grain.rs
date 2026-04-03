use std::time::Duration;

use async_trait::async_trait;

use crate::grain_context::GrainContext;
use crate::Message;

#[async_trait]
pub trait Grain: Send + 'static {
    type State: Default + Send + 'static;

    async fn on_activate(_state: &mut Self::State, _ctx: &GrainContext) {}
    async fn on_deactivate(_state: &mut Self::State, _ctx: &GrainContext) {}

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

#[async_trait]
pub trait GrainHandler<M: Message>: Grain {
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
