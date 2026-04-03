use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use orlando_core::{
    Grain, GrainCallFilter, GrainCallInfo, GrainContext, GrainHandler, Message,
};
use orlando_runtime::Silo;

// --- Grain ---

#[derive(Default)]
struct CounterState {
    count: i64,
}

struct Counter;

#[async_trait]
impl Grain for Counter {
    type State = CounterState;
}

struct Increment {
    amount: i64,
}
impl Message for Increment {
    type Result = i64;
}

#[async_trait]
impl GrainHandler<Increment> for Counter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

// --- Counting filter ---

struct CountingFilter {
    before_count: AtomicUsize,
    after_count: AtomicUsize,
    after_ok_count: AtomicUsize,
}

impl CountingFilter {
    fn new() -> Self {
        Self {
            before_count: AtomicUsize::new(0),
            after_count: AtomicUsize::new(0),
            after_ok_count: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl GrainCallFilter for CountingFilter {
    async fn on_before(&self, _info: &GrainCallInfo) -> Result<(), String> {
        self.before_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn on_after(&self, _info: &GrainCallInfo, result_ok: bool) {
        self.after_count.fetch_add(1, Ordering::Relaxed);
        if result_ok {
            self.after_ok_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// --- Rejecting filter ---

struct RejectFilter;

#[async_trait]
impl GrainCallFilter for RejectFilter {
    async fn on_before(&self, _info: &GrainCallInfo) -> Result<(), String> {
        Err("access denied".to_string())
    }
}

// --- Tests ---

#[tokio::test]
async fn filter_called_on_every_ask() {
    let filter = Arc::new(CountingFilter::new());
    let silo = Silo::builder().filter(filter.clone()).build();

    let counter = silo.get_ref::<Counter>("f1");
    counter.ask(Increment { amount: 1 }).await.unwrap();
    counter.ask(Increment { amount: 2 }).await.unwrap();
    counter.ask(Increment { amount: 3 }).await.unwrap();

    assert_eq!(filter.before_count.load(Ordering::Relaxed), 3);
    assert_eq!(filter.after_count.load(Ordering::Relaxed), 3);
    assert_eq!(filter.after_ok_count.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn reject_filter_blocks_call() {
    let silo = Silo::builder()
        .filter(Arc::new(RejectFilter))
        .build();

    let counter = silo.get_ref::<Counter>("reject");
    let result = counter.ask(Increment { amount: 1 }).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("access denied"),
        "expected rejection error, got: {}",
        err
    );
}

#[tokio::test]
async fn filter_receives_grain_id_and_message_type() {
    use std::sync::Mutex;

    struct RecordingFilter {
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl GrainCallFilter for RecordingFilter {
        async fn on_before(&self, info: &GrainCallInfo) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((info.grain_id.key.clone(), info.message_type.to_string()));
            Ok(())
        }
    }

    let filter = Arc::new(RecordingFilter {
        calls: Mutex::new(Vec::new()),
    });
    let silo = Silo::builder().filter(filter.clone()).build();

    let counter = silo.get_ref::<Counter>("my-key");
    counter.ask(Increment { amount: 1 }).await.unwrap();

    let calls = filter.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "my-key");
    assert!(calls[0].1.contains("Increment"));
}

#[tokio::test]
async fn multiple_filters_run_in_order() {
    use std::sync::Mutex;

    struct OrderFilter {
        id: &'static str,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl GrainCallFilter for OrderFilter {
        async fn on_before(&self, _info: &GrainCallInfo) -> Result<(), String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("before:{}", self.id));
            Ok(())
        }

        async fn on_after(&self, _info: &GrainCallInfo, _ok: bool) {
            self.log
                .lock()
                .unwrap()
                .push(format!("after:{}", self.id));
        }
    }

    let log = Arc::new(Mutex::new(Vec::new()));
    let f1 = Arc::new(OrderFilter {
        id: "A",
        log: log.clone(),
    });
    let f2 = Arc::new(OrderFilter {
        id: "B",
        log: log.clone(),
    });

    let silo = Silo::builder()
        .filter(f1 as Arc<dyn GrainCallFilter>)
        .filter(f2 as Arc<dyn GrainCallFilter>)
        .build();

    let counter = silo.get_ref::<Counter>("order");
    counter.ask(Increment { amount: 1 }).await.unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(
        *entries,
        vec!["before:A", "before:B", "after:A", "after:B"]
    );
}
