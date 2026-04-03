use std::marker::PhantomData;

use tokio::sync::mpsc;

use crate::envelope::{Envelope, build_ask_envelope, recv_ask_response};
use crate::error::GrainError;
use crate::filter::{FilterChain, GrainCallInfo};
use crate::grain::{Grain, GrainHandler};
use crate::grain_id::GrainId;
use crate::message::Message;

#[derive(Debug)]
pub struct GrainRef<G: Grain> {
    pub(crate) sender: mpsc::Sender<Envelope>,
    pub(crate) grain_id: GrainId,
    pub(crate) filters: FilterChain,
    pub(crate) _marker: PhantomData<G>,
}

impl<G: Grain> Clone for GrainRef<G> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            grain_id: self.grain_id.clone(),
            filters: self.filters.clone(),
            _marker: PhantomData,
        }
    }
}

impl<G: Grain> GrainRef<G> {
    pub fn new(sender: mpsc::Sender<Envelope>) -> Self {
        Self {
            sender,
            grain_id: GrainId {
                type_name: G::grain_type_name(),
                key: String::new(),
            },
            filters: FilterChain::empty(),
            _marker: PhantomData,
        }
    }

    /// Access the underlying mailbox sender.
    /// Useful for subscribing to observers or other low-level operations.
    pub fn sender(&self) -> &mpsc::Sender<Envelope> {
        &self.sender
    }

    pub fn with_id(sender: mpsc::Sender<Envelope>, grain_id: GrainId, filters: FilterChain) -> Self {
        Self {
            sender,
            grain_id,
            filters,
            _marker: PhantomData,
        }
    }

    pub async fn ask<M>(&self, msg: M) -> Result<M::Result, GrainError>
    where
        M: Message,
        G: GrainHandler<M>,
    {
        let info = GrainCallInfo {
            grain_id: self.grain_id.clone(),
            message_type: std::any::type_name::<M>(),
        };

        // Run before filters
        if !self.filters.is_empty() {
            self.filters
                .run_before(&info)
                .await
                .map_err(GrainError::HandlerFailed)?;
        }

        let (envelope, rx) = build_ask_envelope::<G, M>(msg);

        let send_result = self
            .sender
            .send(envelope)
            .await
            .map_err(|_| GrainError::MailboxClosed);

        if let Err(e) = send_result {
            if !self.filters.is_empty() {
                self.filters.run_after(&info, false).await;
            }
            return Err(e);
        }

        let result = recv_ask_response(rx, G::ask_timeout()).await;

        if !self.filters.is_empty() {
            self.filters.run_after(&info, result.is_ok()).await;
        }

        result
    }
}
