use std::future::Future;
use std::time::Duration;

use orlando_core::GrainError;

use crate::store::PersistenceError;

/// Unique identifier for a multi-grain transaction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TxId(pub String);

impl TxId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for TxId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors from multi-grain transactions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransactionError {
    #[error("transaction {tx_id} aborted: {source}")]
    Aborted {
        tx_id: TxId,
        source: GrainError,
    },

    #[error("transaction {0} timed out after {1:?}")]
    Timeout(TxId, Duration),

    #[error("persistence error in transaction {tx_id}: {source}")]
    Persistence {
        tx_id: TxId,
        source: PersistenceError,
    },
}

/// Coordinates a multi-grain transaction using the saga pattern.
///
/// Each participant grain must be accessed via `TransactionalGrainRef`, which
/// provides automatic rollback on handler failure. The coordinator orchestrates
/// the overall operation: if any grain call fails, the error propagates and
/// previously-successful grains rely on compensating actions or the natural
/// single-grain rollback that `TransactionalGrainRef::ask` provides.
///
/// # Usage
///
/// ```ignore
/// let tx = TransactionCoordinator::new();
/// let result = tx.execute(|tx_id| async move {
///     let a_count = account_a.ask(Debit { amount: 100 }).await?;
///     let b_count = account_b.ask(Credit { amount: 100 }).await?;
///     Ok((a_count, b_count))
/// }).await?;
/// ```
///
/// If `account_b.ask(Credit { ... })` fails, the error returns as
/// `TransactionError::Aborted`. The `Debit` on `account_a` already committed
/// in-grain, so the caller should handle compensation (e.g., re-credit).
/// For operations where all grains must atomically succeed or fail, ensure
/// that grain handlers are idempotent or use compensating messages.
#[derive(Debug)]
pub struct TransactionCoordinator {
    tx_id: TxId,
    timeout: Duration,
}

impl TransactionCoordinator {
    /// Create a new coordinator with a fresh transaction ID.
    pub fn new() -> Self {
        Self {
            tx_id: TxId::new(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Set the timeout for the entire transaction.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The transaction's unique identifier.
    pub fn tx_id(&self) -> &TxId {
        &self.tx_id
    }

    /// Execute a multi-grain transaction.
    ///
    /// The closure receives the `TxId` (for logging/correlation) and should
    /// make all grain calls via `TransactionalGrainRef::ask`. Each individual
    /// grain automatically rolls back on handler error.
    ///
    /// If the closure returns `Ok`, the transaction is considered committed.
    /// If it returns `Err`, the error is wrapped in `TransactionError::Aborted`.
    ///
    /// The entire operation is subject to the coordinator's timeout.
    pub async fn execute<F, Fut, T>(&self, f: F) -> Result<T, TransactionError>
    where
        F: FnOnce(TxId) -> Fut,
        Fut: Future<Output = Result<T, GrainError>>,
    {
        tracing::debug!(tx_id = %self.tx_id, "multi-grain transaction started");

        let result = tokio::time::timeout(self.timeout, f(self.tx_id.clone())).await;

        match result {
            Ok(Ok(value)) => {
                tracing::debug!(tx_id = %self.tx_id, "multi-grain transaction committed");
                Ok(value)
            }
            Ok(Err(grain_err)) => {
                tracing::debug!(tx_id = %self.tx_id, error = %grain_err, "multi-grain transaction aborted");
                Err(TransactionError::Aborted {
                    tx_id: self.tx_id.clone(),
                    source: grain_err,
                })
            }
            Err(_elapsed) => {
                tracing::debug!(tx_id = %self.tx_id, timeout = ?self.timeout, "multi-grain transaction timed out");
                Err(TransactionError::Timeout(
                    self.tx_id.clone(),
                    self.timeout,
                ))
            }
        }
    }
}

impl Default for TransactionCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_id_is_unique() {
        let a = TxId::new();
        let b = TxId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn tx_id_display() {
        let id = TxId("test-123".to_string());
        assert_eq!(id.to_string(), "test-123");
    }

    #[tokio::test]
    async fn execute_returns_ok_on_success() {
        let tx = TransactionCoordinator::new();
        let result = tx
            .execute(|_tx_id| async move { Ok::<i32, GrainError>(42) })
            .await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn execute_returns_aborted_on_grain_error() {
        let tx = TransactionCoordinator::new();
        let result = tx
            .execute(|_tx_id| async move {
                Err::<i32, GrainError>(GrainError::HandlerFailed("boom".into()))
            })
            .await;
        assert!(matches!(result, Err(TransactionError::Aborted { .. })));
    }

    #[tokio::test]
    async fn execute_returns_timeout() {
        let tx = TransactionCoordinator::new().with_timeout(Duration::from_millis(10));
        let result = tx
            .execute(|_tx_id| async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<i32, GrainError>(42)
            })
            .await;
        assert!(matches!(result, Err(TransactionError::Timeout(_, _))));
    }
}
