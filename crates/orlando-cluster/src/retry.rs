use std::time::Duration;

use orlando_core::GrainError;

/// Configurable retry policy for remote grain calls.
///
/// Controls how `ClusterGrainRef::ask()` handles transient failures
/// (network errors, timeouts). Application-level errors (`HandlerFailed`)
/// are never retried.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retries, just the initial call).
    pub max_retries: u32,
    /// Base delay between retries. Actual delay = base * 2^attempt (exponential backoff).
    pub base_delay: Duration,
    /// Maximum delay between retries (caps the exponential backoff).
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// No retries — fail immediately on any error.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    /// Create a policy with a specific number of retries.
    pub fn with_retries(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Default::default()
        }
    }

    /// Whether an error is retryable.
    /// Only transient errors (network, timeout) are retried.
    /// Application errors (handler failures, type mismatches) are not.
    pub fn is_retryable(error: &GrainError) -> bool {
        matches!(
            error,
            GrainError::RemoteCallFailed(_)
                | GrainError::Timeout(_)
                | GrainError::MailboxClosed
        )
    }

    /// Compute the delay for a given attempt number (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay = self.base_delay.saturating_mul(1 << attempt.min(10));
        delay.min(self.max_delay)
    }
}
