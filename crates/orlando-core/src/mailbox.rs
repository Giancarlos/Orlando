use std::any::Any;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::activation_state::{ActivationEvent, ActivationState, catch_panic};
use crate::envelope::Envelope;
use crate::grain::Grain;
use crate::grain_context::{GrainActivator, GrainContext};
use crate::grain_id::GrainId;

/// The grain mailbox loop, driven by an explicit [`ActivationState`] FSM.
///
/// The activation is always in exactly one well-defined state. A handler panic
/// is contained (via `catch_panic`) and routed to `Faulted` rather than
/// aborting the task, so the directory cleanup at the end always runs — a
/// panicking handler can never leave a stale directory entry pointing at a dead
/// mailbox. The single-message-at-a-time invariant is preserved: exactly one
/// message is fully handled before the next is dequeued.
pub async fn run_mailbox<G: Grain>(
    grain_id: GrainId,
    mut rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    cancellation: CancellationToken,
) {
    let mut state = G::State::default();
    let ctx = GrainContext::new(grain_id.clone(), activator).with_cancellation(cancellation);

    let mut fsm = ActivationState::Activating;
    tracing::debug!(%grain_id, "grain activating");

    // Activating -> Idle | Faulted
    fsm = fsm.next(match catch_panic(G::on_activate(&mut state, &ctx)).await {
        Ok(()) => ActivationEvent::ActivateSucceeded,
        Err(_) => {
            tracing::error!(%grain_id, "on_activate panicked — activation faulted");
            ActivationEvent::ActivateFailed
        }
    });

    while fsm == ActivationState::Idle {
        let event = match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                fsm = fsm.next(ActivationEvent::MessageReceived); // Idle -> Processing
                tracing::debug!(%grain_id, "grain handling message");
                let handler = envelope.into_handler_future(&mut state as &mut (dyn Any + Send), &ctx);
                match catch_panic(handler).await {
                    Ok(()) => ActivationEvent::HandlerCompleted,
                    Err(_) => {
                        tracing::error!(%grain_id, "handler panicked — faulting activation, state discarded");
                        ActivationEvent::HandlerPanicked
                    }
                }
            }
            Ok(None) => {
                tracing::debug!(%grain_id, "grain mailbox closed");
                ActivationEvent::ChannelClosed
            }
            Err(_) => {
                tracing::debug!(%grain_id, "grain idle, deactivating");
                ActivationEvent::IdleTimeout
            }
        };
        fsm = fsm.next(event);
    }

    // Teardown — guaranteed regardless of how the loop exited.
    match fsm {
        ActivationState::Draining => {
            fsm = fsm.next(ActivationEvent::DrainComplete); // -> Deactivating
            // A panic in on_deactivate must not skip directory removal.
            let _ = catch_panic(G::on_deactivate(&mut state, &ctx)).await;
            fsm = fsm.next(ActivationEvent::DeactivateComplete); // -> Closed
        }
        ActivationState::Faulted => {
            // State may be corrupt: skip on_deactivate. The next call to this
            // grain re-activates a fresh State::default().
            fsm = fsm.next(ActivationEvent::DeactivateComplete); // -> Closed
        }
        other => {
            // Defensive: the loop only exits via Draining or Faulted.
            tracing::warn!(%grain_id, ?other, "unexpected activation state at teardown");
        }
    }

    ctx.activator().remove(&grain_id);
    debug_assert_eq!(fsm, ActivationState::Closed, "activation must end in Closed");
    tracing::debug!(%grain_id, "grain deactivated");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    use super::run_mailbox;
    use crate::envelope::build_ask_envelope;
    use crate::grain::{Grain, GrainHandler};
    use crate::grain_context::{GrainActivator, GrainContext};
    use crate::grain_id::GrainId;
    use crate::message::Message;
    use crate::testing::FakeActivator;

    #[derive(Default)]
    struct TestState {
        count: i64,
    }

    struct TestGrain;

    #[async_trait]
    impl Grain for TestGrain {
        type State = TestState;
        fn idle_timeout() -> Duration {
            // Short so the graceful idle path resolves quickly under test.
            Duration::from_millis(50)
        }
    }

    struct Boom;
    impl Message for Boom {
        type Result = ();
    }

    #[async_trait]
    impl GrainHandler<Boom> for TestGrain {
        async fn handle(_s: &mut TestState, _m: Boom, _c: &GrainContext) {
            panic!("intentional handler panic");
        }
    }

    struct Add(i64);
    impl Message for Add {
        type Result = i64;
    }

    #[async_trait]
    impl GrainHandler<Add> for TestGrain {
        async fn handle(s: &mut TestState, m: Add, _c: &GrainContext) -> i64 {
            s.count += m.0;
            s.count
        }
    }

    fn gid() -> GrainId {
        GrainId {
            type_name: "TestGrain",
            key: "k".into(),
        }
    }

    /// A panicking handler must be contained: the mailbox task itself must not
    /// panic, and the activation must still be removed from the directory.
    #[tokio::test]
    async fn handler_panic_is_contained_and_directory_is_cleaned() {
        let activator = FakeActivator::new();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn(run_mailbox::<TestGrain>(
            gid(),
            rx,
            activator.clone(),
            CancellationToken::new(),
        ));
        activator.register(gid(), tx.clone(), tokio::spawn(async {}));

        let (env, _reply) = build_ask_envelope::<TestGrain, Boom>(Boom);
        tx.send(env).await.unwrap();
        drop(tx); // close the channel so the loop drains after the panic

        // The task must complete cleanly (panic contained, not propagated).
        task.await.expect("mailbox task must not panic");
        assert!(
            activator.get_sender(&gid()).is_none(),
            "faulted activation must be removed from the directory"
        );
    }

    /// Normal processing followed by channel close: handler runs, replies, and
    /// the activation deactivates gracefully and is removed.
    #[tokio::test]
    async fn graceful_close_processes_then_deactivates() {
        let activator = FakeActivator::new();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn(run_mailbox::<TestGrain>(
            gid(),
            rx,
            activator.clone(),
            CancellationToken::new(),
        ));
        activator.register(gid(), tx.clone(), tokio::spawn(async {}));

        let (env, reply) = build_ask_envelope::<TestGrain, Add>(Add(5));
        tx.send(env).await.unwrap();
        let result = reply.await.unwrap().downcast::<i64>().unwrap();
        assert_eq!(*result, 5);

        drop(tx);
        task.await.expect("mailbox task must not panic");
        assert!(activator.get_sender(&gid()).is_none());
    }
}
