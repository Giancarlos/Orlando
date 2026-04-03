use std::sync::Arc;

use crate::grain_id::GrainId;

/// Metadata about a grain call, passed to interceptors.
#[derive(Debug, Clone)]
pub struct GrainCallInfo {
    /// The grain being called.
    pub grain_id: GrainId,
    /// The message type name (e.g., "Increment").
    pub message_type: &'static str,
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
    pub fn new(filters: Vec<Arc<dyn GrainCallFilter>>) -> Self {
        Self {
            filters: Arc::new(filters),
        }
    }

    pub fn empty() -> Self {
        Self {
            filters: Arc::new(Vec::new()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    pub async fn run_before(&self, info: &GrainCallInfo) -> Result<(), String> {
        for filter in self.filters.iter() {
            filter.on_before(info).await?;
        }
        Ok(())
    }

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
