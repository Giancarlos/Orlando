use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{mpsc, oneshot};

use orlando_core::{Envelope, Grain, GrainActivator, GrainContext, GrainError, GrainId, GrainRef, Message};
use orlando_runtime::GrainDirectory;

use crate::persistent_grain::{PersistentGrain, TransactionalGrain, TransactionalHandler};
use crate::versioned_grain::VersionedGrain;
use crate::persistent_mailbox;
use crate::store::StateStore;
use crate::transaction::TransactionContext;

/// A silo that supports both regular and persistent grains.
pub struct PersistentSilo {
    directory: Arc<GrainDirectory>,
    store: Arc<dyn StateStore>,
}

impl PersistentSilo {
    pub fn builder() -> PersistentSiloBuilder {
        PersistentSiloBuilder { store: None }
    }

    /// Get a reference to a regular (non-persistent) grain.
    pub fn get_ref<G: Grain>(&self, key: impl Into<String>) -> GrainRef<G> {
        let ctx = GrainContext::new(
            GrainId {
                type_name: "silo",
                key: String::new(),
            },
            self.directory.clone(),
        );
        ctx.get_ref::<G>(key)
    }

    /// Get a reference to a persistent grain.
    /// State is automatically loaded from the store on activation
    /// and saved back on deactivation.
    pub fn persistent_get_ref<G>(&self, key: impl Into<String>) -> GrainRef<G>
    where
        G: PersistentGrain,
        G::State: Serialize + DeserializeOwned,
    {
        let grain_id = GrainId {
            type_name: std::any::type_name::<G>(),
            key: key.into(),
        };

        let activator: Arc<dyn GrainActivator> = self.directory.clone();
        let store = self.store.clone();

        let activator_for_closure = activator.clone();
        let sender = activator.get_or_insert(
            grain_id,
            Box::new(move |id| {
                let (tx, rx) = mpsc::channel(orlando_core::MAILBOX_CAPACITY);
                let task = if G::reentrant() {
                    tokio::spawn(async move {
                        persistent_mailbox::run_reentrant::<G>(
                            id, rx, activator_for_closure, store,
                        )
                        .await;
                    })
                } else {
                    tokio::spawn(async move {
                        persistent_mailbox::run::<G>(id, rx, activator_for_closure, store).await;
                    })
                };
                (tx, task)
            }),
        );

        GrainRef::new(sender)
    }

    /// Get a reference to a transactional grain.
    ///
    /// Transactional grains support automatic rollback on handler failure
    /// and mid-handler `save_state()` via `TransactionContext`.
    /// Uses the same persistent mailbox — rollback logic is in the envelope closure.
    pub fn transactional_get_ref<G>(&self, key: impl Into<String>) -> TransactionalGrainRef<G>
    where
        G: TransactionalGrain,
        G::State: Serialize + DeserializeOwned + Clone,
    {
        let grain_id = GrainId {
            type_name: std::any::type_name::<G>(),
            key: key.into(),
        };

        let activator: Arc<dyn GrainActivator> = self.directory.clone();
        let store = self.store.clone();

        let activator_for_closure = activator.clone();
        let sender = activator.get_or_insert(
            grain_id.clone(),
            Box::new(move |id| {
                let (tx, rx) = mpsc::channel(orlando_core::MAILBOX_CAPACITY);
                let task = tokio::spawn(async move {
                    persistent_mailbox::run::<G>(id, rx, activator_for_closure, store).await;
                });
                (tx, task)
            }),
        );

        TransactionalGrainRef::new(sender, self.store.clone(), grain_id)
    }

    /// Get a reference to a versioned grain.
    ///
    /// Versioned grains support automatic state migration on load.
    /// When the stored state version is older than `G::state_version()`,
    /// the migration chain runs to bring the state up to date.
    pub fn versioned_get_ref<G>(&self, key: impl Into<String>) -> GrainRef<G>
    where
        G: VersionedGrain,
        G::State: Serialize + DeserializeOwned,
    {
        let grain_id = GrainId {
            type_name: std::any::type_name::<G>(),
            key: key.into(),
        };

        let activator: Arc<dyn GrainActivator> = self.directory.clone();
        let store = self.store.clone();

        let activator_for_closure = activator.clone();
        let sender = activator.get_or_insert(
            grain_id,
            Box::new(move |id| {
                let (tx, rx) = mpsc::channel(orlando_core::MAILBOX_CAPACITY);
                let task = tokio::spawn(async move {
                    persistent_mailbox::run_versioned::<G>(id, rx, activator_for_closure, store)
                        .await;
                });
                (tx, task)
            }),
        );

        GrainRef::new(sender)
    }

    /// Access the underlying state store.
    pub fn store(&self) -> &Arc<dyn StateStore> {
        &self.store
    }
}

impl std::fmt::Debug for PersistentSilo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentSilo").finish()
    }
}

pub struct PersistentSiloBuilder {
    store: Option<Arc<dyn StateStore>>,
}

impl PersistentSiloBuilder {
    pub fn store(mut self, store: impl StateStore) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    pub fn store_arc(mut self, store: Arc<dyn StateStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn build(self) -> PersistentSilo {
        let store = self.store.expect("PersistentSilo requires a StateStore — call .store() on the builder");
        PersistentSilo {
            directory: Arc::new(GrainDirectory::new()),
            store,
        }
    }
}

/// A grain reference that supports transactional semantics.
///
/// Unlike `GrainRef`, the `ask` method on this type uses `TransactionalHandler`
/// which returns `Result` — on `Err`, the grain's state is rolled back to
/// the snapshot taken before the handler was called.
pub struct TransactionalGrainRef<G: Grain> {
    sender: mpsc::Sender<Envelope>,
    store: Arc<dyn StateStore>,
    grain_id: GrainId,
    _marker: std::marker::PhantomData<G>,
}

impl<G: Grain> std::fmt::Debug for TransactionalGrainRef<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionalGrainRef")
            .field("grain_id", &self.grain_id)
            .finish()
    }
}

impl<G: Grain> Clone for TransactionalGrainRef<G> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            store: self.store.clone(),
            grain_id: self.grain_id.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<G: TransactionalGrain> TransactionalGrainRef<G>
where
    G::State: Serialize + DeserializeOwned + Clone,
{
    pub(crate) fn new(
        sender: mpsc::Sender<Envelope>,
        store: Arc<dyn StateStore>,
        grain_id: GrainId,
    ) -> Self {
        Self {
            sender,
            store,
            grain_id,
            _marker: std::marker::PhantomData,
        }
    }

    /// Send a message to this transactional grain and await the result.
    ///
    /// If the handler returns `Err`, the grain's state is rolled back to
    /// the snapshot taken before the handler was invoked.
    pub async fn ask<M>(&self, msg: M) -> Result<M::Result, GrainError>
    where
        M: Message,
        G: TransactionalHandler<M>,
    {
        let (tx, rx) = oneshot::channel::<Box<dyn Any + Send>>();
        let store = self.store.clone();
        let grain_id = self.grain_id.clone();

        let envelope = Envelope::new(Box::new(
            move |state_any: &mut (dyn Any + Send), ctx: &GrainContext|
                -> Pin<Box<dyn Future<Output = ()> + Send + '_>>
            {
                let state = state_any
                    .downcast_mut::<G::State>()
                    .expect("grain state type mismatch");

                // Snapshot before handler runs
                let snapshot = state.clone();
                let tx_ctx = TransactionContext::new(store, grain_id);

                Box::pin(async move {
                    match <G as TransactionalHandler<M>>::handle(state, msg, ctx, &tx_ctx).await {
                        Ok(result) => {
                            let _ = tx.send(Box::new(Ok::<M::Result, String>(result))
                                as Box<dyn Any + Send>);
                        }
                        Err(e) => {
                            // Rollback state
                            *state = snapshot;
                            tracing::debug!("transactional handler failed, state rolled back");
                            let _ = tx.send(Box::new(Err::<M::Result, String>(e.to_string()))
                                as Box<dyn Any + Send>);
                        }
                    }
                })
            },
        ));

        self.sender
            .send(envelope)
            .await
            .map_err(|_| GrainError::MailboxClosed)?;

        let response = tokio::time::timeout(G::ask_timeout(), rx)
            .await
            .map_err(|_| GrainError::Timeout(G::ask_timeout()))?
            .map_err(|_| GrainError::MailboxClosed)?;

        // The response is Result<M::Result, String>
        match response.downcast::<Result<M::Result, String>>() {
            Ok(boxed) => match *boxed {
                Ok(result) => Ok(result),
                Err(e) => Err(GrainError::HandlerFailed(e)),
            },
            Err(_) => Err(GrainError::ReplyTypeMismatch),
        }
    }
}
