use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::envelope::Envelope;
use crate::grain::Grain;
use crate::grain_id::GrainId;
use crate::grain_ref::GrainRef;
use crate::mailbox;

/// Trait for the backing store that tracks active grains.
/// Implemented by GrainDirectory in orlando-runtime.
pub trait GrainActivator: Send + Sync + 'static {
    fn get_sender(&self, grain_id: &GrainId) -> Option<mpsc::Sender<Envelope>>;
    fn register(&self, grain_id: GrainId, sender: mpsc::Sender<Envelope>, task: JoinHandle<()>);

    /// Remove a grain's activation entry. Called by the mailbox loop on deactivation.
    fn remove(&self, grain_id: &GrainId);

    /// List all active grain IDs. Used by clustering for rebalancing.
    fn grain_ids(&self) -> Vec<GrainId> {
        vec![]
    }

    /// Atomically get an existing sender or create and insert a new activation.
    /// The closure receives a clone of the grain_id for spawning the mailbox task.
    /// GrainDirectory overrides this with DashMap::entry() for atomicity.
    fn get_or_insert(
        &self,
        grain_id: GrainId,
        create: Box<dyn FnOnce(GrainId) -> (mpsc::Sender<Envelope>, JoinHandle<()>) + Send>,
    ) -> mpsc::Sender<Envelope> {
        if let Some(sender) = self.get_sender(&grain_id) {
            return sender;
        }
        let (sender, task) = create(grain_id.clone());
        self.register(grain_id, sender.clone(), task);
        sender
    }
}

/// Passed to every grain handler and lifecycle hook.
/// Provides the grain's own identity and the ability to call other grains.
#[derive(Clone)]
pub struct GrainContext {
    grain_id: GrainId,
    activator: Arc<dyn GrainActivator>,
}

impl GrainContext {
    pub fn new(grain_id: GrainId, activator: Arc<dyn GrainActivator>) -> Self {
        Self {
            grain_id,
            activator,
        }
    }

    pub fn grain_id(&self) -> &GrainId {
        &self.grain_id
    }

    /// Get this grain's own mailbox sender.
    /// Returns `None` if the grain isn't registered in the directory (shouldn't happen from inside a handler).
    pub fn self_sender(&self) -> Option<mpsc::Sender<Envelope>> {
        self.activator.get_sender(&self.grain_id)
    }

    /// Access the underlying grain activator (directory).
    pub fn activator(&self) -> &Arc<dyn GrainActivator> {
        &self.activator
    }

    /// Get a reference to a grain, activating it if necessary.
    pub fn get_ref<G: Grain>(&self, key: impl Into<String>) -> GrainRef<G> {
        let grain_id = GrainId {
            type_name: std::any::type_name::<G>(),
            key: key.into(),
        };

        let activator = self.activator.clone();
        let sender = self.activator.get_or_insert(
            grain_id,
            Box::new(move |id| {
                let (tx, rx) = mpsc::channel(256);
                let task = tokio::spawn(async move {
                    mailbox::run_mailbox::<G>(id, rx, activator).await;
                });
                (tx, task)
            }),
        );
        GrainRef::new(sender)
    }
}

impl std::fmt::Debug for GrainContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrainContext")
            .field("grain_id", &self.grain_id)
            .finish()
    }
}
