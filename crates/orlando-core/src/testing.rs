use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::envelope::Envelope;
use crate::grain_context::{GrainActivator, GrainContext};
use crate::grain_id::GrainId;

/// A minimal GrainActivator for unit tests.
/// Supports grain-to-grain calls within the test, without needing a full Silo.
#[derive(Debug)]
pub struct FakeActivator {
    senders: Mutex<HashMap<GrainId, mpsc::Sender<Envelope>>>,
}

impl FakeActivator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            senders: Mutex::new(HashMap::new()),
        })
    }
}

impl GrainActivator for FakeActivator {
    fn get_sender(&self, grain_id: &GrainId) -> Option<mpsc::Sender<Envelope>> {
        self.senders.lock().unwrap().get(grain_id).cloned()
    }

    fn register(&self, grain_id: GrainId, sender: mpsc::Sender<Envelope>, _task: JoinHandle<()>) {
        self.senders.lock().unwrap().insert(grain_id, sender);
    }

    fn remove(&self, grain_id: &GrainId) {
        self.senders.lock().unwrap().remove(grain_id);
    }
}

/// Create a GrainContext backed by a FakeActivator for unit testing.
/// Grains activated through this context run real mailbox loops (via tokio::spawn),
/// so tests must use `#[tokio::test]`.
pub fn fake_grain_context(grain_id: GrainId) -> GrainContext {
    let activator = FakeActivator::new();
    GrainContext::new(grain_id, activator)
}
