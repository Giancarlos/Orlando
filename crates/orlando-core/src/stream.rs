use tokio::sync::mpsc;

use crate::envelope::Envelope;
use crate::grain::GrainHandler;
use crate::message::Message;
use crate::observer::{ObserverSet, SubscriptionId};

/// A stream item delivered to consumer grains.
///
/// Consumer grains handle this as a regular message by implementing
/// `GrainHandler<StreamItem<T>>`.
#[derive(Debug, Clone)]
pub struct StreamItem<T: Send + 'static> {
    pub stream_name: String,
    pub item: T,
    pub sequence: u64,
}

impl<T: Send + 'static> Message for StreamItem<T> {
    type Result = ();
}

/// Producer-side handle for a grain-to-grain stream.
///
/// Wraps an `ObserverSet<StreamItem<T>>` with a sequence counter.
/// Items are delivered to consumers' mailboxes as `StreamItem<T>` messages,
/// preserving turn-based execution.
///
/// ```ignore
/// // In producer grain state:
/// struct MyState {
///     stream: StreamProducer<f64>,
/// }
///
/// // In a handler:
/// state.stream.send(42.0);
/// ```
pub struct StreamProducer<T: Clone + Send + 'static> {
    name: String,
    observers: ObserverSet<StreamItem<T>>,
    sequence: u64,
}

impl<T: Clone + Send + 'static> Default for StreamProducer<T> {
    fn default() -> Self {
        Self::new("default")
    }
}

impl<T: Clone + Send + 'static> StreamProducer<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            observers: ObserverSet::new(),
            sequence: 0,
        }
    }

    /// Subscribe a consumer grain to this stream.
    /// The consumer must implement `GrainHandler<StreamItem<T>>`.
    pub fn subscribe<G>(&mut self, sender: mpsc::Sender<Envelope>) -> SubscriptionId
    where
        G: GrainHandler<StreamItem<T>>,
        StreamItem<T>: Clone,
    {
        self.observers.subscribe::<G>(sender)
    }

    /// Remove a consumer from this stream.
    pub fn unsubscribe(&mut self, id: &SubscriptionId) -> bool {
        self.observers.unsubscribe(id)
    }

    /// Send an item to all consumers.
    /// Increments the sequence counter and broadcasts via the observer set.
    pub fn send(&mut self, item: T) {
        let stream_item = StreamItem {
            stream_name: self.name.clone(),
            item,
            sequence: self.sequence,
        };
        self.sequence += 1;
        self.observers.notify(stream_item);
    }

    /// Current sequence number (number of items sent so far).
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Number of active consumers.
    pub fn consumer_count(&self) -> usize {
        self.observers.len()
    }

    /// Stream name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<T: Clone + Send + 'static> std::fmt::Debug for StreamProducer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamProducer")
            .field("name", &self.name)
            .field("sequence", &self.sequence)
            .field("consumers", &self.observers.len())
            .finish()
    }
}
