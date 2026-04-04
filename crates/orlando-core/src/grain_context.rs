use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::envelope::Envelope;
use crate::filter::FilterChain;
use crate::request_context::RequestContext;
use crate::grain::{Grain, StatelessWorker};
use crate::grain_id::GrainId;
use crate::grain_ref::GrainRef;
use crate::mailbox;
use crate::reentrant_mailbox;
use crate::worker_ref::WorkerGrainRef;

pub type ActivationFactory =
    Box<dyn FnOnce(GrainId) -> (mpsc::Sender<Envelope>, JoinHandle<()>) + Send>;

pub type PoolFactory =
    Box<dyn Fn(GrainId) -> (mpsc::Sender<Envelope>, JoinHandle<()>) + Send>;

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
        create: ActivationFactory,
    ) -> mpsc::Sender<Envelope> {
        if let Some(sender) = self.get_sender(&grain_id) {
            return sender;
        }
        let (sender, task) = create(grain_id.clone());
        self.register(grain_id, sender.clone(), task);
        sender
    }

    /// Get or create a pool of activations for a stateless worker grain.
    /// Returns the list of senders for round-robin dispatch.
    /// GrainDirectory overrides this with atomic pool management.
    fn get_or_insert_pool(
        &self,
        grain_id: GrainId,
        create: PoolFactory,
        pool_size: usize,
    ) -> Vec<mpsc::Sender<Envelope>> {
        let mut senders = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let (sender, task) = create(grain_id.clone());
            self.register(grain_id.clone(), sender.clone(), task);
            senders.push(sender);
        }
        senders
    }
}

/// Passed to every grain handler and lifecycle hook.
/// Provides the grain's own identity and the ability to call other grains.
#[derive(Clone)]
pub struct GrainContext {
    grain_id: GrainId,
    activator: Arc<dyn GrainActivator>,
    filters: FilterChain,
    request_context: RequestContext,
}

impl GrainContext {
    pub fn new(grain_id: GrainId, activator: Arc<dyn GrainActivator>) -> Self {
        Self {
            grain_id,
            activator,
            filters: FilterChain::empty(),
            request_context: RequestContext::new(),
        }
    }

    pub fn with_filters(
        grain_id: GrainId,
        activator: Arc<dyn GrainActivator>,
        filters: FilterChain,
    ) -> Self {
        Self {
            grain_id,
            activator,
            filters,
            request_context: RequestContext::new(),
        }
    }

    /// Access the filter chain.
    pub fn filters(&self) -> &FilterChain {
        &self.filters
    }

    /// Access the request context (trace IDs, correlation IDs, etc.).
    ///
    /// Checks the task-local first (set by cross-silo transport), then falls
    /// back to the context stored on this `GrainContext`.
    pub fn request_context(&self) -> RequestContext {
        let task_local = RequestContext::current();
        if !task_local.is_empty() {
            task_local
        } else {
            self.request_context.clone()
        }
    }

    /// Create a new context with an updated request context.
    /// Used to set context values before making grain-to-grain calls.
    pub fn with_request_context(&self, ctx: RequestContext) -> Self {
        Self {
            grain_id: self.grain_id.clone(),
            activator: self.activator.clone(),
            filters: self.filters.clone(),
            request_context: ctx,
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

    /// Get a reference to a stateless worker grain pool, creating it if necessary.
    pub fn get_worker_ref<G: StatelessWorker>(&self, key: impl Into<String>) -> WorkerGrainRef<G> {
        let grain_id = GrainId {
            type_name: G::grain_type_name(),
            key: key.into(),
        };

        let activator = self.activator.clone();
        let senders = self.activator.get_or_insert_pool(
            grain_id,
            Box::new(move |id| {
                let activator_clone = activator.clone();
                let (tx, rx) = mpsc::channel(crate::MAILBOX_CAPACITY);
                let task = tokio::spawn(async move {
                    mailbox::run_mailbox::<G>(id, rx, activator_clone).await;
                });
                (tx, task)
            }),
            G::max_activations(),
        );
        WorkerGrainRef::new(senders)
    }

    /// Get a reference to a grain, activating it if necessary.
    /// Automatically uses the reentrant mailbox if `G::reentrant()` is true.
    pub fn get_ref<G: Grain>(&self, key: impl Into<String>) -> GrainRef<G> {
        let grain_id = GrainId {
            type_name: G::grain_type_name(),
            key: key.into(),
        };

        let activator = self.activator.clone();
        let filters = self.filters.clone();
        let sender = self.activator.get_or_insert(
            grain_id.clone(),
            Box::new(move |id| {
                let (tx, rx) = mpsc::channel(crate::MAILBOX_CAPACITY);
                let task = if G::reentrant() {
                    tokio::spawn(async move {
                        reentrant_mailbox::run_reentrant_mailbox::<G>(id, rx, activator).await;
                    })
                } else {
                    tokio::spawn(async move {
                        mailbox::run_mailbox::<G>(id, rx, activator).await;
                    })
                };
                (tx, task)
            }),
        );
        GrainRef::with_id(sender, grain_id, filters)
    }
}

impl std::fmt::Debug for GrainContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrainContext")
            .field("grain_id", &self.grain_id)
            .finish()
    }
}
