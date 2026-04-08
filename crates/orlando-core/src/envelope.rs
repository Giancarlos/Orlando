use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use tokio::sync::oneshot;

use crate::error::GrainError;
use crate::grain::GrainHandler;
use crate::grain_context::GrainContext;
use crate::message::Message;
use crate::request_context::RequestContext;

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

    // Capture the caller's request context now (at ask-time, inside the caller's task).
    // This will be restored inside the target grain's mailbox task so the call chain
    // propagates across task boundaries.
    let caller_ctx = RequestContext::current();

    let envelope = Envelope::with_label(
        Box::new(
            move |state_any: &mut (dyn Any + Send), ctx: &GrainContext|
                -> Pin<Box<dyn Future<Output = ()> + Send + '_>>
            {
                let Some(state) = state_any.downcast_mut::<G::State>() else {
                    tracing::error!("grain state type mismatch — message dropped");
                    return Box::pin(async {});
                };
                Box::pin(async move {
                    // Restore the caller's context, then add this grain to the call chain
                    // so outgoing grain calls from this handler see the updated chain
                    // and can detect circular call cycles (deadlock).
                    let req_ctx = caller_ctx
                        .with_call_chain_entry(ctx.grain_id());
                    let result = req_ctx.scope(async {
                        <G as GrainHandler<M>>::handle(state, msg, ctx).await
                    }).await;
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
