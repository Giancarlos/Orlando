//! Explicit finite state machine for a grain activation's lifecycle.
//!
//! The mailbox loop ([`crate::mailbox::run_mailbox`]) is driven by this FSM so
//! that an activation is **always** in one well-defined state, no matter how a
//! turn ends — normal completion, idle timeout, channel close, a handler panic,
//! or a store error. [`ActivationState::next`] is a *total* transition function:
//! every `(state, event)` pair maps to a defined state, and any unexpected
//! combination is a no-op that keeps the current state rather than landing the
//! activation in an undefined condition.
//!
//! ```text
//!  Activating --ActivateSucceeded--> Idle
//!  Activating --ActivateFailed-----> Faulted
//!  Idle       --MessageReceived----> Processing
//!  Idle       --ChannelClosed------> Draining
//!  Idle       --IdleTimeout--------> Draining
//!  Processing --HandlerCompleted---> Idle          (non-persistent grains)
//!  Processing --PersistStarted-----> Persisting    (persistent grains)
//!  Processing --HandlerPanicked----> Faulted
//!  Persisting --PersistSucceeded---> Idle
//!  Persisting --PersistFailed------> Faulted
//!  Draining   --DrainComplete------> Deactivating
//!  Deactivating --DeactivateComplete--> Closed
//!  Faulted      --DeactivateComplete--> Closed     (on_deactivate is skipped)
//!  Closed       -- (any) ----------> Closed         (terminal)
//! ```

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The lifecycle state of a single grain activation.
///
/// `Persisting` is only entered by persistence-backed mailbox loops; the
/// in-memory [`crate::mailbox::run_mailbox`] uses the rest of the states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationState {
    /// Constructing state and running `on_activate`.
    Activating,
    /// Waiting for the next message (subject to the idle timeout).
    Idle,
    /// A handler is running for the current message.
    Processing,
    /// State is being written to the store after a handler (persistent grains).
    Persisting,
    /// Mailbox is shutting down gracefully; in-flight work is being finished.
    Draining,
    /// Running `on_deactivate` before final teardown.
    Deactivating,
    /// A handler/lifecycle panic or store failure occurred; state is considered
    /// corrupt and the activation is discarded without running `on_deactivate`.
    Faulted,
    /// Terminal: the activation has been torn down and removed from the directory.
    Closed,
}

/// An event that can drive an [`ActivationState`] transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationEvent {
    /// `on_activate` returned normally.
    ActivateSucceeded,
    /// `on_activate` panicked.
    ActivateFailed,
    /// A message was dequeued from the mailbox.
    MessageReceived,
    /// A handler returned normally and no persistence step follows.
    HandlerCompleted,
    /// A handler returned normally and a persistence step must run.
    PersistStarted,
    /// A handler panicked.
    HandlerPanicked,
    /// State was persisted successfully.
    PersistSucceeded,
    /// Persisting state failed.
    PersistFailed,
    /// The mailbox channel was closed (all senders dropped).
    ChannelClosed,
    /// No message arrived within the idle timeout.
    IdleTimeout,
    /// Draining of in-flight work finished.
    DrainComplete,
    /// `on_deactivate` (or faulted cleanup) finished.
    DeactivateComplete,
}

impl ActivationState {
    /// Total transition function. Every `(state, event)` pair returns a defined
    /// state; unexpected combinations are no-ops that preserve the current state
    /// so the activation can never be left in an undefined condition.
    #[must_use]
    pub fn next(self, event: ActivationEvent) -> ActivationState {
        use ActivationEvent as E;
        use ActivationState as S;

        match (self, event) {
            (S::Activating, E::ActivateSucceeded) => S::Idle,
            (S::Activating, E::ActivateFailed) => S::Faulted,

            (S::Idle, E::MessageReceived) => S::Processing,
            (S::Idle, E::ChannelClosed) => S::Draining,
            (S::Idle, E::IdleTimeout) => S::Draining,

            (S::Processing, E::HandlerCompleted) => S::Idle,
            (S::Processing, E::PersistStarted) => S::Persisting,
            (S::Processing, E::HandlerPanicked) => S::Faulted,

            (S::Persisting, E::PersistSucceeded) => S::Idle,
            (S::Persisting, E::PersistFailed) => S::Faulted,

            (S::Draining, E::DrainComplete) => S::Deactivating,

            (S::Deactivating, E::DeactivateComplete) => S::Closed,
            (S::Faulted, E::DeactivateComplete) => S::Closed,

            // Terminal state and all unexpected (state, event) pairs are no-ops.
            (state, _) => state,
        }
    }

    /// Whether this is the terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, ActivationState::Closed)
    }
}

type BoxFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// A future adapter that catches a panic raised while polling the inner future
/// and reports it as `Err(payload)` instead of unwinding into the caller.
///
/// The inner future is already heap-pinned (`Pin<Box<..>>`, hence `Unpin`), so
/// this needs no `unsafe` pin projection.
struct CatchUnwind<'a> {
    inner: Option<BoxFuture<'a>>,
}

impl Future for CatchUnwind<'_> {
    type Output = Result<(), Box<dyn Any + Send>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let poll_result = match this.inner.as_mut() {
            Some(fut) => std::panic::catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))),
            // Polled after completion; a Ready future must not be polled again,
            // so this is unreachable in correct use. Stay pending defensively.
            None => return Poll::Pending,
        };
        match poll_result {
            Ok(Poll::Ready(())) => Poll::Ready(Ok(())),
            Ok(Poll::Pending) => Poll::Pending,
            Err(payload) => {
                // Drop the (possibly half-completed) future so it is never polled again.
                this.inner = None;
                Poll::Ready(Err(payload))
            }
        }
    }
}

/// Run a boxed grain future, catching any panic and returning it as `Err`.
///
/// Requires the `panic = "unwind"` strategy (the default). Under `panic = "abort"`
/// the process terminates on panic and this cannot intercept it.
///
/// Exposed beyond this crate so persistence-backed mailbox loops
/// (`orlando-persistence`) can contain handler/lifecycle panics the same way.
pub async fn catch_panic(fut: BoxFuture<'_>) -> Result<(), Box<dyn Any + Send>> {
    CatchUnwind { inner: Some(fut) }.await
}

#[cfg(test)]
mod tests {
    use super::ActivationEvent as E;
    use super::ActivationState as S;

    #[test]
    fn happy_path_reaches_closed() {
        let s = S::Activating;
        let s = s.next(E::ActivateSucceeded);
        assert_eq!(s, S::Idle);
        let s = s.next(E::MessageReceived);
        assert_eq!(s, S::Processing);
        let s = s.next(E::HandlerCompleted);
        assert_eq!(s, S::Idle);
        let s = s.next(E::IdleTimeout);
        assert_eq!(s, S::Draining);
        let s = s.next(E::DrainComplete);
        assert_eq!(s, S::Deactivating);
        let s = s.next(E::DeactivateComplete);
        assert_eq!(s, S::Closed);
        assert!(s.is_terminal());
    }

    #[test]
    fn persistence_path() {
        let s = S::Processing.next(E::PersistStarted);
        assert_eq!(s, S::Persisting);
        assert_eq!(s.next(E::PersistSucceeded), S::Idle);
        assert_eq!(s.next(E::PersistFailed), S::Faulted);
    }

    #[test]
    fn activate_failure_faults_then_closes() {
        let s = S::Activating.next(E::ActivateFailed);
        assert_eq!(s, S::Faulted);
        // Faulted skips on_deactivate but still reaches the terminal state.
        assert_eq!(s.next(E::DeactivateComplete), S::Closed);
    }

    #[test]
    fn handler_panic_faults() {
        let s = S::Idle.next(E::MessageReceived);
        assert_eq!(s, S::Processing);
        assert_eq!(s.next(E::HandlerPanicked), S::Faulted);
    }

    #[test]
    fn channel_close_drains() {
        assert_eq!(S::Idle.next(E::ChannelClosed), S::Draining);
    }

    #[test]
    fn closed_is_absorbing() {
        for ev in [
            E::ActivateSucceeded,
            E::MessageReceived,
            E::HandlerCompleted,
            E::HandlerPanicked,
            E::IdleTimeout,
            E::DeactivateComplete,
        ] {
            assert_eq!(S::Closed.next(ev), S::Closed, "Closed must absorb {ev:?}");
        }
    }

    #[test]
    fn unexpected_events_are_noops() {
        // An event with no defined transition from a state leaves it unchanged,
        // guaranteeing the activation is always in a valid state.
        assert_eq!(S::Idle.next(E::HandlerCompleted), S::Idle);
        assert_eq!(S::Processing.next(E::IdleTimeout), S::Processing);
        assert_eq!(S::Draining.next(E::MessageReceived), S::Draining);
        assert_eq!(S::Deactivating.next(E::MessageReceived), S::Deactivating);
        assert_eq!(S::Faulted.next(E::MessageReceived), S::Faulted);
    }

    #[test]
    fn transition_function_is_total_and_never_panics() {
        let states = [
            S::Activating,
            S::Idle,
            S::Processing,
            S::Persisting,
            S::Draining,
            S::Deactivating,
            S::Faulted,
            S::Closed,
        ];
        let events = [
            E::ActivateSucceeded,
            E::ActivateFailed,
            E::MessageReceived,
            E::HandlerCompleted,
            E::PersistStarted,
            E::HandlerPanicked,
            E::PersistSucceeded,
            E::PersistFailed,
            E::ChannelClosed,
            E::IdleTimeout,
            E::DrainComplete,
            E::DeactivateComplete,
        ];
        // Every (state, event) pair must resolve to one of the known states.
        for s in states {
            for e in events {
                let next = s.next(e);
                assert!(states.contains(&next), "{s:?} + {e:?} -> {next:?} not a known state");
            }
        }
    }
}
