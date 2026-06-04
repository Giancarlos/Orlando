use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use orlando_core::{Envelope, GrainId};

/// A pool of activations for a stateless worker grain.
/// Each activation has its own mailbox task and independent state.
pub struct WorkerPool {
    /// The stateless-worker grain id this pool serves.
    pub grain_id: GrainId,
    /// Mailbox senders for the pooled activations (round-robin dispatch).
    pub senders: Vec<mpsc::Sender<Envelope>>,
    /// Join handles for the pooled activations' mailbox tasks.
    pub tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for WorkerPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerPool")
            .field("grain_id", &self.grain_id)
            .field("pool_size", &self.senders.len())
            .finish()
    }
}
