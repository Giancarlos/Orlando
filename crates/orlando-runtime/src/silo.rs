use std::sync::Arc;

use orlando_core::{
    FilterChain, Grain, GrainCallFilter, GrainContext, GrainId, GrainRef, StatelessWorker,
    WorkerGrainRef,
};

use crate::directory::GrainDirectory;

pub struct Silo {
    directory: Arc<GrainDirectory>,
    filters: FilterChain,
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
    /// Creates `G::max_activations()` concurrent mailbox tasks on first access.
    pub fn get_worker_ref<G: StatelessWorker>(&self, key: impl Into<String>) -> WorkerGrainRef<G> {
        self.make_context().get_worker_ref::<G>(key)
    }
}

impl std::fmt::Debug for Silo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Silo").finish()
    }
}

pub struct SiloBuilder {
    filters: Vec<Arc<dyn GrainCallFilter>>,
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
        }
    }

    /// Add a grain call filter (interceptor) to the silo.
    /// Filters are called in order on every `ask()` call.
    pub fn filter(mut self, filter: Arc<dyn GrainCallFilter>) -> Self {
        self.filters.push(filter);
        self
    }

    pub fn build(self) -> Silo {
        Silo {
            directory: Arc::new(GrainDirectory::new()),
            filters: FilterChain::new(self.filters),
        }
    }
}
