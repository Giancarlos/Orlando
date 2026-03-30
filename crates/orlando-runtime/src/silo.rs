use std::sync::Arc;

use orlando_core::{Grain, GrainContext, GrainId, GrainRef};

use crate::directory::GrainDirectory;

pub struct Silo {
    directory: Arc<GrainDirectory>,
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

    /// Get a reference to a grain, activating it if necessary.
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
}

impl std::fmt::Debug for Silo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Silo").finish()
    }
}

#[derive(Debug)]
pub struct SiloBuilder {
    _private: (),
}

impl SiloBuilder {
    fn new() -> Self {
        Self { _private: () }
    }

    pub fn build(self) -> Silo {
        Silo {
            directory: Arc::new(GrainDirectory::new()),
        }
    }
}
