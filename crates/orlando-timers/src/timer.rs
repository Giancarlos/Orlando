use std::time::Duration;

use tokio::sync::oneshot;

use orlando_core::{Envelope, GrainContext, GrainHandler, Message};

/// Message delivered to a grain on each timer tick.
/// The grain must implement `GrainHandler<TimerTick>` to receive these.
#[derive(Debug)]
pub struct TimerTick {
    pub name: String,
}

impl Message for TimerTick {
    type Result = ();
}

/// Handle to a registered timer. Dropping this cancels the timer.
#[derive(Debug)]
pub struct TimerHandle {
    _cancel: oneshot::Sender<()>,
}

/// Register a volatile timer that fires periodically, delivering a `TimerTick`
/// into the grain's own mailbox.
///
/// - The timer is cancelled when the returned `TimerHandle` is dropped.
/// - The timer is also cancelled if the grain's mailbox sender closes (grain deactivated).
/// - The first tick fires after one `period`, not immediately.
///
/// The grain type `G` must implement `GrainHandler<TimerTick>`.
pub fn register_timer<G>(
    ctx: &GrainContext,
    name: impl Into<String>,
    period: Duration,
) -> TimerHandle
where
    G: GrainHandler<TimerTick>,
{
    let name = name.into();
    let sender = ctx
        .self_sender()
        .expect("grain must be active to register a timer");
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.tick().await; // skip the immediate first tick

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let tick_name = name.clone();
                    let envelope = Envelope::new(Box::new(move |state_any, ctx| {
                        let Some(state) = state_any.downcast_mut::<G::State>() else {
                            tracing::error!("grain state type mismatch in timer tick — dropped");
                            return Box::pin(async {});
                        };
                        let tick = TimerTick { name: tick_name };
                        Box::pin(async move {
                            <G as GrainHandler<TimerTick>>::handle(state, tick, ctx).await;
                        })
                    }));
                    if sender.send(envelope).await.is_err() {
                        tracing::debug!("timer {:?}: grain mailbox closed, stopping", name);
                        break;
                    }
                }
                _ = &mut cancel_rx => {
                    tracing::debug!("timer {:?}: cancelled", name);
                    break;
                }
            }
        }
    });

    TimerHandle {
        _cancel: cancel_tx,
    }
}
