//! Exponential-backoff retry for store initialization.
//!
//! Production stores (Postgres, SQLite-network, Redis) can hit transient
//! failures during K8s pod startup when the DB is still warming up. A bare
//! `connect()` fails the silo before it ever serves traffic. This helper
//! retries with exponential backoff so a transient DB outage does not
//! trigger a crashloop.

use std::future::Future;
use std::time::Duration;

use backoff::ExponentialBackoff;
use backoff::future::retry;

/// Default backoff for store init: 500 ms start, 60 s cap, give up after 15 min.
///
/// 15 minutes is intentionally generous — orchestrators (K8s, Nomad) typically
/// retry the pod after the readiness probe fails, so we want to outlast normal
/// dependency-startup races without blocking forever.
pub fn store_init_backoff() -> ExponentialBackoff {
    ExponentialBackoff {
        initial_interval: Duration::from_millis(500),
        max_interval: Duration::from_secs(60),
        max_elapsed_time: Some(Duration::from_secs(15 * 60)),
        multiplier: 2.0,
        ..Default::default()
    }
}

/// Retry `operation` with `store_init_backoff()` until it succeeds or the
/// max elapsed time is reached. Each failure is logged at `warn`.
///
/// The operation's error must be `Display` so we can include it in the log.
/// Errors are treated as `transient` — backoff will keep retrying.
pub async fn retry_store_init<T, E, F, Fut>(
    operation_name: &'static str,
    mut operation: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    retry(store_init_backoff(), || {
        let fut = operation();
        async move {
            match fut.await {
                Ok(v) => Ok(v),
                Err(e) => {
                    tracing::warn!(
                        operation = operation_name,
                        error = %e,
                        "store init failed, retrying with backoff"
                    );
                    Err(backoff::Error::transient(e))
                }
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn retry_eventually_succeeds() {
        let attempts = AtomicUsize::new(0);
        let result: Result<&'static str, &'static str> =
            retry_store_init("test", || async {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n < 2 { Err("transient") } else { Ok("ok") }
            })
            .await;
        assert_eq!(result, Ok("ok"));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }
}
