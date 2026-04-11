use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use orlando_core::GrainId;

use crate::persistent_mailbox::serialize_state;
use crate::store::{ETag, PersistenceError, StateStore};

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

    /// Re-read the latest persisted state from the store.
    ///
    /// Returns `Ok(Some(S))` if state exists, `Ok(None)` if nothing is persisted,
    /// or an error on I/O / deserialization failure. Does not mutate the handler's
    /// in-memory state — the caller decides whether to apply the refreshed value.
    pub async fn read_state<S>(&self) -> Result<Option<S>, PersistenceError>
    where
        S: DeserializeOwned,
    {
        let Some(bytes) = self.store.load(&self.grain_id).await? else {
            tracing::debug!(grain_id = %self.grain_id, "transactional read_state: no persisted state");
            return Ok(None);
        };
        let (state, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
        tracing::debug!(grain_id = %self.grain_id, "transactional read_state completed");
        Ok(Some(state))
    }

    /// Delete persisted state from the store.
    pub async fn clear_state(&self) -> Result<(), PersistenceError> {
        self.store.delete(&self.grain_id).await?;
        tracing::debug!(grain_id = %self.grain_id, "transactional clear_state completed");
        Ok(())
    }

    /// Conditionally persist state using optimistic concurrency control.
    ///
    /// If `expected_etag` is `Some`, the save succeeds only when the stored
    /// ETag matches the expected value. If `None`, the save succeeds only when
    /// no prior state exists for this grain.
    ///
    /// Returns `Ok(Some(ETag))` with the new ETag on success, or
    /// `Err(PersistenceError::EtagMismatch)` on conflict.
    pub async fn save_with_etag<S: Serialize>(
        &self,
        state: &S,
        expected_etag: Option<&ETag>,
    ) -> Result<ETag, PersistenceError> {
        let bytes = serialize_state(state)?;
        let new_etag = self
            .store
            .save_with_etag(&self.grain_id, &bytes, expected_etag)
            .await?;
        tracing::debug!(
            grain_id = %self.grain_id,
            new_etag = %new_etag.0,
            "transactional save_with_etag completed"
        );
        Ok(new_etag)
    }

    /// Fetch the current ETag without loading the full state.
    ///
    /// Returns `Ok(None)` if no state is persisted for this grain,
    /// or `Ok(Some(etag))` with the current version tag.
    pub async fn load_etag(&self) -> Result<Option<ETag>, PersistenceError> {
        let result = self.store.load_with_etag(&self.grain_id).await?;
        let etag = result.map(|(_, etag)| etag);
        tracing::debug!(
            grain_id = %self.grain_id,
            etag = ?etag,
            "transactional load_etag completed"
        );
        Ok(etag)
    }
}

impl std::fmt::Debug for TransactionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionContext")
            .field("grain_id", &self.grain_id)
            .finish()
    }
}
