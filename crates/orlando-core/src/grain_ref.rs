use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use tokio::sync::{mpsc, oneshot};

use crate::envelope::Envelope;
use crate::error::GrainError;
use crate::grain::{Grain, GrainHandler};
use crate::grain_context::GrainContext;
use crate::message::Message;

#[derive(Debug)]
pub struct GrainRef<G: Grain> {
    pub(crate) sender: mpsc::Sender<Envelope>,
    pub(crate) _marker: PhantomData<G>,
}

impl<G: Grain> Clone for GrainRef<G> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            _marker: PhantomData,
        }
    }
}

impl<G: Grain> GrainRef<G> {
    pub fn new(sender: mpsc::Sender<Envelope>) -> Self {
        Self {
            sender,
            _marker: PhantomData,
        }
    }

    pub async fn ask<M>(&self, msg: M) -> Result<M::Result, GrainError>
    where
        M: Message,
        G: GrainHandler<M>,
    {
        let (tx, rx) = oneshot::channel::<Box<dyn Any + Send>>();

        let envelope = Envelope {
            handle_fn: Box::new(
                move |state_any: &mut (dyn Any + Send), ctx: &GrainContext|
                    -> Pin<Box<dyn Future<Output = ()> + Send + '_>>
                {
                    let state = state_any
                        .downcast_mut::<G::State>()
                        .expect("grain state type mismatch");
                    Box::pin(async move {
                        let result = <G as GrainHandler<M>>::handle(state, msg, ctx).await;
                        let _ = tx.send(Box::new(result) as Box<dyn Any + Send>);
                    })
                },
            ),
        };

        self.sender
            .send(envelope)
            .await
            .map_err(|_| GrainError::MailboxClosed)?;

        let response = rx.await.map_err(|_| GrainError::MailboxClosed)?;

        response
            .downcast::<M::Result>()
            .map(|boxed| *boxed)
            .map_err(|_| GrainError::ReplyTypeMismatch)
    }
}
