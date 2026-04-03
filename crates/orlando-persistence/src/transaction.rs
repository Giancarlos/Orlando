use std::sync::Arc;

use serde::Serialize;

use orlando_core::GrainId;

use crate::persistent_mailbox::serialize_state;
use crate::store::{PersistenceError, StateStore};

/// Handle for mid-handler state persistence within a transactional grain.
///
/// Passed to `TransactionalHandler::handle` so handlers can explicitly
/// checkpoint state to the store without waiting for deactivation.
#[derive(Clone)]
pub struct TransactionContext {
    store: Arc<dyn StateStore>,
    grain_id: GrainId,
}

impl TransactionContext {
    pub(crate) fn new(store: Arc<dyn StateStore>, grain_id: GrainId) -> Self {
        Self { store, grain_id }
    }

    /// Persist the current state to the store immediately.
    pub async fn save_state<S: Serialize>(&self, state: &S) -> Result<(), PersistenceError> {
        let bytes = serialize_state(state)?;
        self.store.save(&self.grain_id, &bytes).await?;
        tracing::debug!(grain_id = %self.grain_id, "transactional save_state completed");
        Ok(())
    }

    /// Delete persisted state from the store.
    pub async fn clear_state(&self) -> Result<(), PersistenceError> {
        self.store.delete(&self.grain_id).await?;
        tracing::debug!(grain_id = %self.grain_id, "transactional clear_state completed");
        Ok(())
    }
}

impl std::fmt::Debug for TransactionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionContext")
            .field("grain_id", &self.grain_id)
            .finish()
    }
}
