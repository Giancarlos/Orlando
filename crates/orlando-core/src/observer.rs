use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::envelope::Envelope;
use crate::grain::GrainHandler;
use crate::grain_context::GrainContext;
use crate::message::Message;

/// Opaque identifier for a subscription in an `ObserverSet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

/// Type-erased function that builds an Envelope from a notification message.
type EnvelopeBuilder<N> = Arc<dyn Fn(N) -> Envelope + Send + Sync>;

struct Subscriber<N: Message<Result = ()> + Clone> {
    id: SubscriptionId,
    sender: mpsc::Sender<Envelope>,
    build_envelope: EnvelopeBuilder<N>,
}

/// A set of observers that receive fire-and-forget notifications of type `N`.
///
/// Stored in the publishing grain's state. When the publisher calls `notify()`,
/// the notification is sent to every subscriber's mailbox. Dead subscribers
/// (closed senders) are automatically pruned.
///
/// Notifications are fire-and-forget: `N::Result` must be `()`.
/// The notification type must be `Clone` for broadcasting to multiple subscribers.
///
/// ```ignore
/// // In the publisher grain's state:
/// struct MyState {
///     observers: ObserverSet<PriceUpdate>,
/// }
///
/// // In a handler:
/// state.observers.notify(PriceUpdate { price: 42.0 });
/// ```
pub struct ObserverSet<N: Message<Result = ()> + Clone> {
    subscribers: Vec<Subscriber<N>>,
    next_id: u64,
}

impl<N: Message<Result = ()> + Clone> Default for ObserverSet<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Message<Result = ()> + Clone> ObserverSet<N> {
    pub fn new() -> Self {
        Self {
            subscribers: Vec::new(),
            next_id: 0,
        }
    }

    /// Subscribe a grain to receive notifications.
    ///
    /// The grain type `G` must implement `GrainHandler<N>`. The `sender` is
    /// the grain's mailbox sender (obtained via `ctx.self_sender()` or
    /// from a `GrainRef`).
    pub fn subscribe<G>(&mut self, sender: mpsc::Sender<Envelope>) -> SubscriptionId
    where
        G: GrainHandler<N>,
    {
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;

        let build_envelope: EnvelopeBuilder<N> = Arc::new(move |msg: N| {
            Envelope::with_label(
                Box::new(
                    move |state_any: &mut (dyn Any + Send), ctx: &GrainContext|
                        -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
                        let state = state_any
                            .downcast_mut::<G::State>()
                            .expect("grain state type mismatch");
                        Box::pin(async move {
                            <G as GrainHandler<N>>::handle(state, msg, ctx).await;
                        })
                    },
                ),
                std::any::type_name::<N>(),
            )
        });

        self.subscribers.push(Subscriber {
            id,
            sender,
            build_envelope,
        });

        tracing::debug!(subscription_id = id.0, "observer subscribed");
        id
    }

    /// Remove a subscription. Returns true if found and removed.
    pub fn unsubscribe(&mut self, id: &SubscriptionId) -> bool {
        let before = self.subscribers.len();
        self.subscribers.retain(|s| s.id != *id);
        let removed = self.subscribers.len() < before;
        if removed {
            tracing::debug!(subscription_id = id.0, "observer unsubscribed");
        }
        removed
    }

    /// Broadcast a notification to all subscribers.
    ///
    /// Uses `try_send` (non-blocking). If a subscriber's mailbox is full,
    /// the notification for that subscriber is dropped. Dead subscribers
    /// (closed senders) are automatically pruned.
    pub fn notify(&mut self, msg: N) {
        self.subscribers.retain(|sub| {
            if sub.sender.is_closed() {
                tracing::debug!(subscription_id = sub.id.0, "pruning dead observer");
                return false;
            }
            let envelope = (sub.build_envelope)(msg.clone());
            if sub.sender.try_send(envelope).is_err() {
                tracing::debug!(subscription_id = sub.id.0, "observer mailbox full, dropping notification");
            }
            true
        });
    }

    /// Number of active subscribers.
    pub fn len(&self) -> usize {
        self.subscribers.len()
    }

    /// Whether there are no subscribers.
    pub fn is_empty(&self) -> bool {
        self.subscribers.is_empty()
    }
}

impl<N: Message<Result = ()> + Clone> std::fmt::Debug for ObserverSet<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObserverSet")
            .field("subscriber_count", &self.subscribers.len())
            .finish()
    }
}
