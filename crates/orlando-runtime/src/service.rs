//! Co-hosted background services (grain services).
//!
//! A [`GrainService`] runs alongside grains inside a [`crate::Silo`], spawned
//! when the silo is built. Unlike a grain it is not addressed by identity and
//! has no mailbox — it is a long-running task (cleanup loops, periodic jobs,
//! metrics emitters, external-event pumps) that can call grains through the
//! [`GrainContext`] it is given. Analogous to Orleans' `IGrainService` /
//! .NET `IHostedService`.

use async_trait::async_trait;
use orlando_core::GrainContext;

/// A background service co-hosted in a silo.
///
/// `run` is spawned once when the silo is built and should loop until the
/// context is cancelled. Use `ctx.get_ref::<G>(key)` to call grains and
/// `ctx.is_cancelled()` (or `ctx.cancellation_token()`) to stop on shutdown.
#[async_trait]
pub trait GrainService: Send + Sync + 'static {
    /// Run the service to completion. Should return promptly once the silo
    /// signals shutdown via the context's cancellation token.
    async fn run(&self, ctx: GrainContext);
}
