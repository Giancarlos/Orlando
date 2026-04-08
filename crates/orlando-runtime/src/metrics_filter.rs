use orlando_core::{GrainCallFilter, GrainCallInfo};

/// A grain call filter that records metrics for every `ask()` call.
///
/// Records the following metrics (using the `metrics` crate):
/// - `orlando.grain.calls_total` (counter) -- total calls, labeled by grain type and message type
/// - `orlando.grain.call_duration_seconds` (histogram) -- call latency, labeled by grain type
/// - `orlando.grain.errors_total` (counter) -- failed calls, labeled by grain type and message type
///
/// Install a metrics exporter (e.g., `metrics-exporter-prometheus`) to expose these.
/// If no recorder is installed, all metric operations are no-ops.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use orlando_runtime::{Silo, MetricsFilter};
///
/// let silo = Silo::builder()
///     .filter(Arc::new(MetricsFilter::new()))
///     .build();
/// ```
pub struct MetricsFilter {
    _private: (),
}

impl MetricsFilter {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for MetricsFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MetricsFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsFilter").finish()
    }
}

#[async_trait::async_trait]
impl GrainCallFilter for MetricsFilter {
    async fn on_before(&self, info: &GrainCallInfo) -> Result<(), String> {
        metrics::counter!("orlando.grain.calls_total",
            "grain_type" => info.grain_id.type_name.to_string(),
            "message_type" => info.message_type.to_string(),
        )
        .increment(1);
        Ok(())
    }

    async fn on_after(&self, info: &GrainCallInfo, result_ok: bool) {
        let duration = info.started_at.elapsed();
        metrics::histogram!("orlando.grain.call_duration_seconds",
            "grain_type" => info.grain_id.type_name.to_string(),
        )
        .record(duration.as_secs_f64());

        if !result_ok {
            metrics::counter!("orlando.grain.errors_total",
                "grain_type" => info.grain_id.type_name.to_string(),
                "message_type" => info.message_type.to_string(),
            )
            .increment(1);
        }
    }
}
