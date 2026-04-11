use async_trait::async_trait;
use orlando_core::{Grain, GrainContext, GrainId, Message};
use serde::{Serialize, de::DeserializeOwned};

use crate::store::PersistenceError;
use crate::transaction::TransactionContext;

/// Trait for grains whose state is automatically persisted.
/// State is loaded from the store on activation and saved on deactivation.
///
/// To make a grain persistent, implement both `Grain` and `PersistentGrain`,
/// and ensure your `State` type derives `Serialize` and `Deserialize`.
///
/// # Persistence Lifecycle Callbacks
///
/// Override the `on_before_*` / `on_after_*` methods to hook into persistence
/// events — useful for audit logging, cache invalidation, or side effects.
/// All callbacks have default no-op implementations, so existing grains
/// are unaffected.
#[async_trait]
pub trait PersistentGrain: Grain
where
    Self::State: Serialize + DeserializeOwned,
{
    /// Called before state is loaded from the store.
    async fn on_before_load(_grain_id: &GrainId) {}

    /// Called after state is successfully loaded from the store.
    async fn on_after_load(_grain_id: &GrainId) {}

    /// Called before state is saved to the store.
    async fn on_before_save(_grain_id: &GrainId) {}

    /// Called after state is successfully saved to the store.
    async fn on_after_save(_grain_id: &GrainId) {}

    /// Called before state is cleared/deleted from the store.
    async fn on_before_clear(_grain_id: &GrainId) {}

    /// Called after state is successfully cleared/deleted from the store.
    async fn on_after_clear(_grain_id: &GrainId) {}
}

/// Marker trait for grains with transactional state management.
///
/// Transactional grains support automatic rollback on handler failure
/// and mid-handler state persistence via `TransactionContext::save_state()`.
///
/// State must implement `Clone` so the mailbox can snapshot before each handler
/// and revert on failure.
pub trait TransactionalGrain: PersistentGrain
where
    Self::State: Serialize + DeserializeOwned + Clone,
{
}

/// Handler trait for transactional grains. Returns `Result` so the mailbox
/// can detect failure and roll back state to the pre-handler snapshot.
#[async_trait]
pub trait TransactionalHandler<M: Message>: TransactionalGrain
where
    Self::State: Serialize + DeserializeOwned + Clone,
{
    async fn handle(
        state: &mut Self::State,
        msg: M,
        ctx: &GrainContext,
        tx: &TransactionContext,
    ) -> Result<M::Result, PersistenceError>;
}
