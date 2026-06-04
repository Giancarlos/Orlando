//! Observers / fan-out notifications example.
//!
//! An `ObserverSet<N>` held in a grain's state lets it broadcast a message `N` to
//! many subscribed grains at once (publish/subscribe). Subscribers handle `N`
//! like any other message. (`StreamProducer` is built on this; use `ObserverSet`
//! directly for event notifications without stream sequencing.)
//!
//! Run with: `cargo run -p orlando-runtime --example observers`

use std::time::Duration;

use orlando_core::{GrainContext, ObserverSet};
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

// ── Topic: broadcasts NewsItem to all subscribers ──────────────

#[derive(Default)]
struct TopicState {
    observers: ObserverSet<NewsItem>,
}

#[grain(state = TopicState)]
struct Topic;

// The broadcast message must be Clone (delivered to each subscriber).
#[message(result = ())]
#[derive(Clone)]
struct NewsItem {
    headline: String,
}

#[message(result = usize)] // returns subscriber count
struct SubscribeReader {
    key: String,
}

#[message(result = ())]
struct Publish {
    headline: String,
}

#[grain_handler(Topic)]
async fn topic_subscribe(state: &mut TopicState, msg: SubscribeReader, ctx: &GrainContext) -> usize {
    let reader = ctx.get_ref::<Reader>(msg.key);
    state.observers.subscribe::<Reader>(reader.sender().clone());
    state.observers.len()
}

#[grain_handler(Topic)]
async fn topic_publish(state: &mut TopicState, msg: Publish, _ctx: &GrainContext) {
    state.observers.notify(NewsItem { headline: msg.headline });
}

// ── Reader: a subscriber grain ──────────────────────────────────

#[derive(Default)]
struct ReaderState {
    inbox: Vec<String>,
}

#[grain(state = ReaderState)]
struct Reader;

#[grain_handler(Reader)]
async fn reader_receive(state: &mut ReaderState, item: NewsItem, _ctx: &GrainContext) {
    state.inbox.push(item.headline);
}

#[message(result = Vec<String>)]
struct GetInbox;

#[grain_handler(Reader)]
async fn reader_inbox(state: &mut ReaderState, _msg: GetInbox, _ctx: &GrainContext) -> Vec<String> {
    state.inbox.clone()
}

#[tokio::main]
async fn main() {
    let silo = Silo::new();
    let topic = silo.get_ref::<Topic>("news");

    let n = topic.ask(SubscribeReader { key: "alice".into() }).await.unwrap();
    let n = n.max(topic.ask(SubscribeReader { key: "bob".into() }).await.unwrap());
    println!("subscribers: {n}");

    topic.ask(Publish { headline: "Orlando 1.0 released".into() }).await.unwrap();
    topic.ask(Publish { headline: "Grains now self-aware".into() }).await.unwrap();

    // Notifications are fire-and-forget into each subscriber's mailbox.
    tokio::time::sleep(Duration::from_millis(20)).await;

    for who in ["alice", "bob"] {
        let inbox = silo.get_ref::<Reader>(who).ask(GetInbox).await.unwrap();
        println!("{who} received: {inbox:?}");
        assert_eq!(inbox.len(), 2, "each subscriber gets every published item");
    }
    println!("both subscribers received both broadcasts ✓");
}
