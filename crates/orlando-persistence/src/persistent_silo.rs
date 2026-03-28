use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;

use orlando_core::{Grain, GrainActivator, GrainContext, GrainId, GrainRef};
use orlando_runtime::GrainDirectory;

use crate::persistent_grain::PersistentGrain;
use crate::persistent_mailbox;
use crate::store::StateStore;

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
                let (tx, rx) = mpsc::channel(256);
                let task = tokio::spawn(async move {
                    persistent_mailbox::run::<G>(id, rx, activator_for_closure, store).await;
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
