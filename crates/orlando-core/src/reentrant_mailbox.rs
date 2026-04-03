use std::any::Any;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::envelope::Envelope;
use crate::grain::Grain;
use crate::grain_context::{GrainActivator, GrainContext};
use crate::grain_id::GrainId;

/// Reentrant mailbox loop: dequeues messages concurrently into a JoinSet,
/// allowing multiple messages to be in-flight at once. State access is
/// serialized via a `tokio::sync::Mutex` — each handler holds the lock
/// for its full duration, so concurrent handlers execute one at a time
/// with respect to state mutations.
///
/// This is *not* true Orleans-style reentrancy (which interleaves at await
/// points within a single task). The benefit here is that the mailbox can
/// accept and dispatch messages without waiting for prior handlers to
/// complete, reducing head-of-line blocking on the mpsc channel.
pub async fn run_reentrant_mailbox<G: Grain>(
    grain_id: GrainId,
    mut rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
) {
    let state: Arc<tokio::sync::Mutex<Box<dyn Any + Send>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(G::State::default())));
    let ctx = GrainContext::new(grain_id.clone(), activator);

    tracing::debug!(%grain_id, "reentrant grain activating");
    {
        let mut guard = state.lock().await;
        let s = guard.downcast_mut::<G::State>().expect("grain state type mismatch");
        G::on_activate(s, &ctx).await;
    }

    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            biased;

            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(e)) = result {
                    tracing::warn!(%grain_id, error = %e, "reentrant handler panicked");
                }
            }

            msg = timeout(G::idle_timeout(), rx.recv()) => {
                match msg {
                    Ok(Some(envelope)) => {
                        tracing::debug!(%grain_id, "reentrant grain dispatching message");
                        let state = state.clone();
                        let ctx = ctx.clone();
                        tasks.spawn(async move {
                            let mut guard = state.lock().await;
                            envelope.handle(&mut **guard, &ctx).await;
                        });
                    }
                    Ok(None) => {
                        tracing::debug!(%grain_id, "reentrant grain mailbox closed");
                        break;
                    }
                    Err(_) => {
                        tracing::debug!(%grain_id, "reentrant grain idle, deactivating");
                        break;
                    }
                }
            }
        }
    }

    // Drain in-flight handlers before deactivation
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            tracing::warn!(%grain_id, error = %e, "reentrant handler panicked during shutdown");
        }
    }

    {
        let mut guard = state.lock().await;
        let s = guard.downcast_mut::<G::State>().expect("grain state type mismatch");
        G::on_deactivate(s, &ctx).await;
    }

    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, "reentrant grain deactivated");
}
