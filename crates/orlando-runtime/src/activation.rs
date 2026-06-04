use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use orlando_core::{CancellationToken, Envelope, GrainId};

/// A live grain activation: its identity, mailbox sender, the task running the
/// mailbox loop, and the token used to signal cooperative shutdown.
pub struct Activation {
    /// Identity of the activated grain.
    pub grain_id: GrainId,
    /// Sender for the grain's mailbox channel.
    pub sender: mpsc::Sender<Envelope>,
    /// Join handle for the spawned mailbox-loop task.
    pub task: JoinHandle<()>,
    /// Token signalled to ask the activation to drain and deactivate.
    pub cancellation: CancellationToken,
}

impl std::fmt::Debug for Activation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Activation")
            .field("grain_id", &self.grain_id)
            .finish()
    }
}
