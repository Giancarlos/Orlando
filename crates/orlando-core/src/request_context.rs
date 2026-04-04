use std::collections::HashMap;
use std::sync::Arc;

/// Key-value context that flows through grain-to-grain calls.
///
/// Used for distributed tracing IDs, correlation IDs, tenant IDs, etc.
/// Immutable once created — handlers read values via `get()`.
/// Propagated automatically when a handler calls `ctx.get_ref().ask()`.
#[derive(Clone, Debug, Default)]
pub struct RequestContext {
    values: Arc<HashMap<String, String>>,
}

impl RequestContext {
    /// Create an empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a context with initial key-value pairs.
    pub fn with_values(values: HashMap<String, String>) -> Self {
        Self {
            values: Arc::new(values),
        }
    }

    /// Get a value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    /// Create a new context with an additional key-value pair.
    /// Does not mutate the original — returns a new context.
    pub fn with(&self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let mut new_values = (*self.values).clone();
        new_values.insert(key.into(), value.into());
        Self {
            values: Arc::new(new_values),
        }
    }

    /// Whether the context has any values.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Iterate over all key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Convert to a HashMap for serialization (e.g., proto map field).
    pub fn to_map(&self) -> HashMap<String, String> {
        (*self.values).clone()
    }
}

tokio::task_local! {
    /// Task-local request context for cross-silo propagation.
    /// Set by the transport layer before dispatching a grain call,
    /// read by handlers via `RequestContext::current()`.
    static CURRENT: RequestContext;
}

impl RequestContext {
    /// Get the current request context from the task-local, if set.
    /// Returns an empty context if none is set.
    pub fn current() -> Self {
        CURRENT.try_with(|c| c.clone()).unwrap_or_default()
    }

    /// Run a future with this request context set as the task-local.
    pub async fn scope<F: std::future::Future>(self, f: F) -> F::Output {
        CURRENT.scope(self, f).await
    }
}
