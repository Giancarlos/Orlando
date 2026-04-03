use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::envelope::{Envelope, build_ask_envelope, recv_ask_response};
use crate::error::GrainError;
use crate::grain::{GrainHandler, StatelessWorker};
use crate::message::Message;

/// A grain reference that dispatches messages round-robin across a pool
/// of stateless worker activations.
pub struct WorkerGrainRef<G: StatelessWorker> {
    senders: Arc<Vec<mpsc::Sender<Envelope>>>,
    next: Arc<AtomicUsize>,
    _marker: PhantomData<G>,
}

impl<G: StatelessWorker> Clone for WorkerGrainRef<G> {
    fn clone(&self) -> Self {
        Self {
            senders: self.senders.clone(),
            next: self.next.clone(),
            _marker: PhantomData,
        }
    }
}

impl<G: StatelessWorker> std::fmt::Debug for WorkerGrainRef<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerGrainRef")
            .field("pool_size", &self.senders.len())
            .finish()
    }
}

impl<G: StatelessWorker> WorkerGrainRef<G> {
    pub fn new(senders: Vec<mpsc::Sender<Envelope>>) -> Self {
        Self {
            senders: Arc::new(senders),
            next: Arc::new(AtomicUsize::new(0)),
            _marker: PhantomData,
        }
    }

    /// Send a message to the next worker in the pool and await the reply.
    pub async fn ask<M>(&self, msg: M) -> Result<M::Result, GrainError>
    where
        M: Message,
        G: GrainHandler<M>,
    {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.senders.len();
        let sender = &self.senders[idx];

        let (envelope, rx) = build_ask_envelope::<G, M>(msg);

        sender
            .send(envelope)
            .await
            .map_err(|_| GrainError::MailboxClosed)?;

        recv_ask_response(rx, G::ask_timeout()).await
    }

    /// Number of worker activations in the pool.
    pub fn pool_size(&self) -> usize {
        self.senders.len()
    }
}
