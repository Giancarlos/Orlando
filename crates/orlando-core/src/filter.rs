use std::sync::Arc;
use std::time::Instant;

use crate::grain_id::GrainId;

/// Metadata about a grain call, passed to interceptors.
#[derive(Debug, Clone)]
pub struct GrainCallInfo {
    /// The grain being called.
    pub grain_id: GrainId,
    /// The message type name (e.g., "Increment").
    pub message_type: &'static str,
    /// When the call was initiated. Filters can use this to compute latency.
    pub started_at: Instant,
}

/// Interceptor for grain calls. Implement this to add cross-cutting concerns
/// like logging, metrics, or authorization to every grain `ask()` call.
///
/// Filters are called before sending the message and after receiving the response.
/// They do not have access to the message payload (type-erased), only metadata.
#[async_trait::async_trait]
pub trait GrainCallFilter: Send + Sync + 'static {
    /// Called before the message is sent to the grain's mailbox.
    /// Return `Err` to reject the call without sending.
    async fn on_before(&self, info: &GrainCallInfo) -> Result<(), String> {
        let _ = info;
        Ok(())
    }

    /// Called after the grain handler has responded (or after an error).
    /// `result_ok` is true if the handler returned successfully.
    async fn on_after(&self, info: &GrainCallInfo, result_ok: bool) {
        let _ = (info, result_ok);
    }
}

/// A chain of filters applied in order.
#[derive(Clone)]
pub struct FilterChain {
    filters: Arc<Vec<Arc<dyn GrainCallFilter>>>,
}

impl FilterChain {
    /// Build a chain from an ordered list of filters (run in order).
    pub fn new(filters: Vec<Arc<dyn GrainCallFilter>>) -> Self {
        Self {
            filters: Arc::new(filters),
        }
    }

    /// An empty chain that runs no filters.
    pub fn empty() -> Self {
        Self {
            filters: Arc::new(Vec::new()),
        }
    }

    /// Whether the chain has no filters.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Run each filter's `on_before` hook in order; short-circuits with `Err`
    /// (rejecting the call) if any filter returns one.
    pub async fn run_before(&self, info: &GrainCallInfo) -> Result<(), String> {
        for filter in self.filters.iter() {
            filter.on_before(info).await?;
        }
        Ok(())
    }

    /// Run each filter's `on_after` hook in order, passing whether the call
    /// succeeded.
    pub async fn run_after(&self, info: &GrainCallInfo, result_ok: bool) {
        for filter in self.filters.iter() {
            filter.on_after(info, result_ok).await;
        }
    }
}

impl std::fmt::Debug for FilterChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterChain")
            .field("count", &self.filters.len())
            .finish()
    }
}
