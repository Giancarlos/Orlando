use std::sync::Arc;

use orlando_core::{
    CancellationToken, FilterChain, Grain, GrainCallFilter, GrainContext, GrainId, GrainRef,
    StatelessWorker, WorkerGrainRef,
};
use tokio::task::JoinHandle;

use crate::directory::GrainDirectory;
use crate::service::GrainService;

type LifecycleHook = Box<dyn FnOnce() + Send>;

pub struct Silo {
    directory: Arc<GrainDirectory>,
    filters: FilterChain,
    shutdown_hooks: std::sync::Mutex<Vec<LifecycleHook>>,
    /// Cancellation token shared with co-hosted services; cancelled on shutdown.
    service_cancel: CancellationToken,
    service_handles: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Default for Silo {
    fn default() -> Self {
        Self::new()
    }
}

impl Silo {
    pub fn builder() -> SiloBuilder {
        SiloBuilder::new()
    }

    pub fn new() -> Self {
        SiloBuilder::new().build()
    }

    /// Access the underlying grain directory.
    pub fn directory(&self) -> &Arc<GrainDirectory> {
        &self.directory
    }

    fn make_context(&self) -> GrainContext {
        GrainContext::with_filters(
            GrainId {
                type_name: "silo",
                key: String::new(),
            },
            self.directory.clone(),
            self.filters.clone(),
        )
    }

    /// Get a reference to a grain, activating it if necessary.
    pub fn get_ref<G: Grain>(&self, key: impl Into<String>) -> GrainRef<G> {
        self.make_context().get_ref::<G>(key)
    }

    /// Get a reference to a stateless worker grain pool.
    pub fn get_worker_ref<G: StatelessWorker>(&self, key: impl Into<String>) -> WorkerGrainRef<G> {
        self.make_context().get_worker_ref::<G>(key)
    }

    /// Signal co-hosted [`GrainService`]s to stop (via their cancellation token)
    /// and await their completion. Call during graceful shutdown.
    pub async fn shutdown_services(&self) {
        self.service_cancel.cancel();
        let handles: Vec<JoinHandle<()>> = {
            let mut g = self
                .service_handles
                .lock()
                .expect("service_handles lock poisoned");
            std::mem::take(&mut *g)
        };
        for h in handles {
            let _ = h.await;
        }
    }

    /// Run shutdown hooks. Call this before dropping the silo.
    pub fn run_shutdown_hooks(&self) {
        let hooks: Vec<LifecycleHook> = {
            let mut guard = self.shutdown_hooks.lock().expect("shutdown_hooks lock poisoned");
            std::mem::take(&mut *guard)
        };
        for hook in hooks {
            hook();
        }
    }
}

impl std::fmt::Debug for Silo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hook_count = self
            .shutdown_hooks
            .lock()
            .map(|h| h.len())
            .unwrap_or(0);
        f.debug_struct("Silo")
            .field("shutdown_hooks", &hook_count)
            .finish()
    }
}

impl Drop for Silo {
    /// Cancel co-hosted services if the silo is dropped without an explicit
    /// `shutdown_services()`. Without this, the spawned service tasks would run
    /// forever (dropping their `JoinHandle`s only detaches them). Cancellation
    /// is synchronous; the tasks observe it and exit on their own (we can't
    /// await them here). Idempotent with `shutdown_services()`.
    fn drop(&mut self) {
        self.service_cancel.cancel();
    }
}

pub struct SiloBuilder {
    filters: Vec<Arc<dyn GrainCallFilter>>,
    max_activations: Option<usize>,
    on_startup: Vec<LifecycleHook>,
    on_shutdown: Vec<LifecycleHook>,
    services: Vec<Arc<dyn GrainService>>,
}

impl std::fmt::Debug for SiloBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiloBuilder")
            .field("filter_count", &self.filters.len())
            .finish()
    }
}

impl SiloBuilder {
    fn new() -> Self {
        Self {
            filters: Vec::new(),
            max_activations: None,
            on_startup: Vec::new(),
            on_shutdown: Vec::new(),
            services: Vec::new(),
        }
    }

    /// Register a co-hosted background [`GrainService`], spawned when the silo
    /// is built and stopped via [`Silo::shutdown_services`].
    pub fn add_service(mut self, service: Arc<dyn GrainService>) -> Self {
        self.services.push(service);
        self
    }

    /// Add a grain call filter (interceptor) to the silo.
    pub fn filter(mut self, filter: Arc<dyn GrainCallFilter>) -> Self {
        self.filters.push(filter);
        self
    }

    /// Set the maximum number of grain activations on this silo.
    pub fn max_activations(mut self, limit: usize) -> Self {
        self.max_activations = Some(limit);
        self
    }

    /// Register a callback that runs after the silo is fully initialized.
    pub fn on_startup(mut self, hook: impl FnOnce() + Send + 'static) -> Self {
        self.on_startup.push(Box::new(hook));
        self
    }

    /// Register a callback that runs during shutdown, after all grains have drained.
    pub fn on_shutdown(mut self, hook: impl FnOnce() + Send + 'static) -> Self {
        self.on_shutdown.push(Box::new(hook));
        self
    }

    pub fn build(self) -> Silo {
        let directory = Arc::new(GrainDirectory::new());
        if let Some(limit) = self.max_activations {
            directory.set_max_activations(limit);
        }

        for hook in self.on_startup {
            hook();
        }

        let filters = FilterChain::new(self.filters);

        // Spawn co-hosted services with a context that shares a cancellation
        // token, so shutdown_services() can stop them.
        let service_cancel = CancellationToken::new();
        let mut service_handles = Vec::with_capacity(self.services.len());
        for svc in self.services {
            let ctx = GrainContext::with_filters(
                GrainId {
                    type_name: "silo-service",
                    key: String::new(),
                },
                directory.clone(),
                filters.clone(),
            )
            .with_cancellation(service_cancel.clone());
            service_handles.push(tokio::spawn(async move { svc.run(ctx).await }));
        }

        Silo {
            directory,
            filters,
            shutdown_hooks: std::sync::Mutex::new(self.on_shutdown),
            service_cancel,
            service_handles: std::sync::Mutex::new(service_handles),
        }
    }
}
