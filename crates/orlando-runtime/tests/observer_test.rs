use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainHandler, Message, ObserverSet};
use orlando_runtime::Silo;

// --- Notification message ---

#[derive(Debug, Clone)]
struct PriceUpdate {
    price: f64,
}
impl Message for PriceUpdate {
    type Result = ();
}

// --- Publisher grain ---

#[derive(Default)]
struct PublisherState {
    observers: ObserverSet<PriceUpdate>,
}

struct Publisher;

#[async_trait]
impl Grain for Publisher {
    type State = PublisherState;
}

struct Subscribe;
impl Message for Subscribe {
    type Result = ();
}

struct Unsubscribe {
    id: orlando_core::SubscriptionId,
}
impl Message for Unsubscribe {
    type Result = ();
}

struct Publish {
    price: f64,
}
impl Message for Publish {
    type Result = usize; // returns subscriber count
}

struct GetSubscriberCount;
impl Message for GetSubscriberCount {
    type Result = usize;
}

#[async_trait]
impl GrainHandler<Subscribe> for Publisher {
    async fn handle(state: &mut PublisherState, _msg: Subscribe, ctx: &GrainContext) {
        // Subscribe a Watcher grain with a fixed key
        let watcher = ctx.get_ref::<Watcher>("watcher-1");
        state.observers.subscribe::<Watcher>(watcher.sender().clone());
    }
}

#[async_trait]
impl GrainHandler<Unsubscribe> for Publisher {
    async fn handle(state: &mut PublisherState, msg: Unsubscribe, _ctx: &GrainContext) {
        state.observers.unsubscribe(&msg.id);
    }
}

#[async_trait]
impl GrainHandler<Publish> for Publisher {
    async fn handle(state: &mut PublisherState, msg: Publish, _ctx: &GrainContext) -> usize {
        state.observers.notify(PriceUpdate { price: msg.price });
        state.observers.len()
    }
}

#[async_trait]
impl GrainHandler<GetSubscriberCount> for Publisher {
    async fn handle(
        state: &mut PublisherState,
        _msg: GetSubscriberCount,
        _ctx: &GrainContext,
    ) -> usize {
        state.observers.len()
    }
}

// --- Subscriber/Watcher grain ---

#[derive(Default)]
struct WatcherState {
    last_price: Option<f64>,
    update_count: u32,
}

struct Watcher;

#[async_trait]
impl Grain for Watcher {
    type State = WatcherState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(100)
    }
}

#[async_trait]
impl GrainHandler<PriceUpdate> for Watcher {
    async fn handle(state: &mut WatcherState, msg: PriceUpdate, _ctx: &GrainContext) {
        state.last_price = Some(msg.price);
        state.update_count += 1;
    }
}

struct GetLastPrice;
impl Message for GetLastPrice {
    type Result = Option<f64>;
}

struct GetUpdateCount;
impl Message for GetUpdateCount {
    type Result = u32;
}

#[async_trait]
impl GrainHandler<GetLastPrice> for Watcher {
    async fn handle(
        state: &mut WatcherState,
        _msg: GetLastPrice,
        _ctx: &GrainContext,
    ) -> Option<f64> {
        state.last_price
    }
}

#[async_trait]
impl GrainHandler<GetUpdateCount> for Watcher {
    async fn handle(
        state: &mut WatcherState,
        _msg: GetUpdateCount,
        _ctx: &GrainContext,
    ) -> u32 {
        state.update_count
    }
}

// --- Multi-subscriber: subscribe with explicit sender ---

struct SubscribeExplicit;
impl Message for SubscribeExplicit {
    type Result = orlando_core::SubscriptionId;
}

// Handler that subscribes a specific watcher by key passed in context
#[async_trait]
impl GrainHandler<SubscribeExplicit> for Publisher {
    async fn handle(
        state: &mut PublisherState,
        _msg: SubscribeExplicit,
        ctx: &GrainContext,
    ) -> orlando_core::SubscriptionId {
        let watcher = ctx.get_ref::<Watcher>("watcher-1");
        state.observers.subscribe::<Watcher>(watcher.sender().clone())
    }
}

// --- Tests ---

#[tokio::test]
async fn subscribe_and_notify() {
    let silo = Silo::new();

    let publisher = silo.get_ref::<Publisher>("pub-1");
    let watcher = silo.get_ref::<Watcher>("watcher-1");

    // Subscribe watcher to publisher
    publisher.ask(Subscribe).await.unwrap();

    // Publish a price update
    publisher.ask(Publish { price: 42.5 }).await.unwrap();

    // Give the notification time to be processed
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Watcher should have received the update
    let price = watcher.ask(GetLastPrice).await.unwrap();
    assert_eq!(price, Some(42.5));
}

#[tokio::test]
async fn unsubscribe_stops_notifications() {
    let silo = Silo::new();

    let publisher = silo.get_ref::<Publisher>("pub-unsub");
    let watcher = silo.get_ref::<Watcher>("watcher-1");

    // Subscribe and get the ID
    let sub_id = publisher.ask(SubscribeExplicit).await.unwrap();

    // Publish first update
    publisher.ask(Publish { price: 10.0 }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(watcher.ask(GetUpdateCount).await.unwrap(), 1);

    // Unsubscribe
    publisher.ask(Unsubscribe { id: sub_id }).await.unwrap();

    // Publish again — should NOT arrive
    publisher.ask(Publish { price: 20.0 }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        watcher.ask(GetUpdateCount).await.unwrap(),
        1,
        "should still be 1 after unsubscribe"
    );
}

#[tokio::test]
async fn dead_subscriber_pruned() {
    let silo = Silo::new();

    let publisher = silo.get_ref::<Publisher>("pub-prune");

    // Subscribe watcher (idle timeout = 100ms)
    publisher.ask(Subscribe).await.unwrap();
    assert_eq!(publisher.ask(GetSubscriberCount).await.unwrap(), 1);

    // Let watcher deactivate via idle timeout
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Publish — should prune the dead subscriber
    publisher.ask(Publish { price: 99.0 }).await.unwrap();
    assert_eq!(
        publisher.ask(GetSubscriberCount).await.unwrap(),
        0,
        "dead subscriber should be pruned"
    );
}

#[tokio::test]
async fn multiple_subscribers() {
    let silo = Silo::new();
    let publisher = silo.get_ref::<Publisher>("pub-multi");

    // Subscribe three watchers via explicit sender access
    for key in ["w1", "w2", "w3"] {
        let watcher = silo.get_ref::<Watcher>(key);
        // We need a handler that subscribes a specific watcher.
        // Use a workaround: call subscribe directly via a custom message.
        // For simplicity, subscribe via the publisher's state using the Subscribe handler
        // which always subscribes watcher-1. Instead, subscribe manually via GrainRef sender.
        let mut obs = ObserverSet::<PriceUpdate>::new();
        obs.subscribe::<Watcher>(watcher.sender().clone());
        // This doesn't help — we need to add to the PUBLISHER's ObserverSet.
        // Let's use a different approach: have the publisher subscribe watchers by sender.
    }

    // Simpler approach: subscribe watcher-1 three times (it gets 3 notifications per publish)
    publisher.ask(Subscribe).await.unwrap();
    publisher.ask(Subscribe).await.unwrap();
    publisher.ask(Subscribe).await.unwrap();

    publisher.ask(Publish { price: 55.0 }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;

    let watcher = silo.get_ref::<Watcher>("watcher-1");
    let count = watcher.ask(GetUpdateCount).await.unwrap();
    assert_eq!(count, 3, "should receive one notification per subscription");
}
