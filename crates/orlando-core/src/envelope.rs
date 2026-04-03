use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use tokio::sync::oneshot;

use crate::error::GrainError;
use crate::grain::GrainHandler;
use crate::grain_context::GrainContext;
use crate::message::Message;

pub type HandleFn = Box<
    dyn for<'a> FnOnce(
            &'a mut (dyn Any + Send),
            &'a GrainContext,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send,
>;

pub struct Envelope {
    pub(crate) handle_fn: HandleFn,
    /// Optional debug label for identifying the message type in logs.
    pub(crate) debug_label: &'static str,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope")
            .field("message", &self.debug_label)
            .finish()
    }
}

impl Envelope {
    pub fn new(handle_fn: HandleFn) -> Self {
        Self {
            handle_fn,
            debug_label: "unknown",
        }
    }

    pub fn with_label(handle_fn: HandleFn, label: &'static str) -> Self {
        Self {
            handle_fn,
            debug_label: label,
        }
    }

    pub async fn handle(self, state: &mut (dyn Any + Send), ctx: &GrainContext) {
        (self.handle_fn)(state, ctx).await;
    }
}

/// Build an envelope + oneshot pair for a grain handler call.
/// Shared by `GrainRef::ask` and `WorkerGrainRef::ask`.
pub fn build_ask_envelope<G, M>(
    msg: M,
) -> (Envelope, oneshot::Receiver<Box<dyn Any + Send>>)
where
    G: GrainHandler<M>,
    M: Message,
{
    let (tx, rx) = oneshot::channel::<Box<dyn Any + Send>>();

    let envelope = Envelope::with_label(
        Box::new(
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
        std::any::type_name::<M>(),
    );

    (envelope, rx)
}

/// Await a oneshot response and downcast it to the expected result type.
/// Times out after `timeout` duration, returning `GrainError::Timeout`.
pub async fn recv_ask_response<R: 'static>(
    rx: oneshot::Receiver<Box<dyn Any + Send>>,
    timeout: std::time::Duration,
) -> Result<R, GrainError> {
    let response = tokio::time::timeout(timeout, rx)
        .await
        .map_err(|_| GrainError::Timeout(timeout))?
        .map_err(|_| GrainError::MailboxClosed)?;
    response
        .downcast::<R>()
        .map(|boxed| *boxed)
        .map_err(|_| GrainError::ReplyTypeMismatch)
}
