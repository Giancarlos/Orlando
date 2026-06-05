use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::activation_state::{ActivationEvent, ActivationState, catch_panic};
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
    cancellation: CancellationToken,
) {
    let state: Arc<tokio::sync::Mutex<Box<dyn Any + Send>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(G::State::default())));
    let ctx = GrainContext::new(grain_id.clone(), activator)
        .with_cancellation(cancellation);

    tracing::debug!(%grain_id, "reentrant grain activating");

    // Per-message handler concurrency is governed by the JoinSet below, so the
    // FSM here tracks the activation's *lifecycle phase* rather than a single
    // in-flight message: Activating -> Idle (accepting messages) -> Draining ->
    // Deactivating -> Closed. A panic in on_activate is contained and routed to
    // Faulted so the directory cleanup at the end still runs.
    let mut fsm = ActivationState::Activating;
    let activate = {
        let state = state.clone();
        let ctx = ctx.clone();
        Box::pin(async move {
            let mut guard = state.lock().await;
            // The box was created here as G::State, so this downcast cannot fail;
            // handle it gracefully anyway (no panic in library code) rather than
            // expect(). A None would mean a corrupt invariant — log and skip.
            match guard.downcast_mut::<G::State>() {
                Some(s) => G::on_activate(s, &ctx).await,
                None => tracing::error!(grain_id = %ctx.grain_id(), "reentrant grain state type mismatch — skipping on_activate"),
            }
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    };
    fsm = fsm.next(match catch_panic(activate).await {
        Ok(()) => ActivationEvent::ActivateSucceeded,
        Err(_) => {
            tracing::error!(%grain_id, "on_activate panicked — reentrant activation faulted");
            ActivationEvent::ActivateFailed
        }
    });

    let mut tasks = JoinSet::new();
    // Cap concurrent handlers to prevent unbounded task growth from message floods
    const MAX_CONCURRENT: usize = 256;

    while fsm == ActivationState::Idle {
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
                        // Drain a task if at capacity before spawning another
                        while tasks.len() >= MAX_CONCURRENT {
                            if let Some(Err(e)) = tasks.join_next().await {
                                tracing::warn!(%grain_id, error = %e, "reentrant handler panicked");
                            }
                        }
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
                        fsm = fsm.next(ActivationEvent::ChannelClosed); // Idle -> Draining
                        break;
                    }
                    Err(_) => {
                        tracing::debug!(%grain_id, "reentrant grain idle, deactivating");
                        fsm = fsm.next(ActivationEvent::IdleTimeout); // Idle -> Draining
                        break;
                    }
                }
            }
        }
    }

    // Drain in-flight handlers before deactivation (no-op if activation faulted).
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            tracing::warn!(%grain_id, error = %e, "reentrant handler panicked during shutdown");
        }
    }

    // Teardown — directory removal is guaranteed regardless of how we got here.
    match fsm {
        ActivationState::Draining => {
            fsm = fsm.next(ActivationEvent::DrainComplete); // -> Deactivating
            let deactivate = {
                let state = state.clone();
                let ctx = ctx.clone();
                Box::pin(async move {
                    let mut guard = state.lock().await;
                    match guard.downcast_mut::<G::State>() {
                        Some(s) => G::on_deactivate(s, &ctx).await,
                        None => tracing::error!(grain_id = %ctx.grain_id(), "reentrant grain state type mismatch — skipping on_deactivate"),
                    }
                }) as Pin<Box<dyn Future<Output = ()> + Send>>
            };
            let _ = catch_panic(deactivate).await;
            fsm = fsm.next(ActivationEvent::DeactivateComplete); // -> Closed
        }
        ActivationState::Faulted => {
            // on_activate panicked: state may be corrupt, skip on_deactivate.
            fsm = fsm.next(ActivationEvent::DeactivateComplete); // -> Closed
        }
        other => tracing::warn!(%grain_id, ?other, "unexpected reentrant state at teardown"),
    }

    ctx.activator().remove(&grain_id);
    debug_assert_eq!(fsm, ActivationState::Closed, "activation must end in Closed");
    tracing::debug!(%grain_id, "reentrant grain deactivated");
}
