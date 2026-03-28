use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use orlando_core::{Envelope, GrainId};

pub struct Activation {
    pub grain_id: GrainId,
    pub sender: mpsc::Sender<Envelope>,
    pub task: JoinHandle<()>,
}

impl std::fmt::Debug for Activation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Activation")
            .field("grain_id", &self.grain_id)
            .finish()
    }
}
