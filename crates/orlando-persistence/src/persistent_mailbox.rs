use std::any::Any;
use std::sync::{Arc, Once};

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;

use orlando_core::{
    ActivationEvent, ActivationState, CancellationToken, Envelope, Grain, GrainActivator,
    GrainContext, GrainId, catch_panic,
};

use crate::persistent_grain::PersistentGrain;
use crate::replication_sink::ReplicationSink;
use crate::serializer::SerializerFormat;
use crate::store::{PersistenceError, PersistenceStrategy, StateStore};
use crate::versioned_grain::VersionedGrain;

/// Emits a one-time warning per process when grains run under
/// `PersistenceStrategy::WriteOnDeactivate`. State changes between activation
/// and idle-deactivation are lost if the silo crashes — switch to
/// `WriteThrough` or `WriteBack` for crash-durability.
fn warn_once_if_write_on_deactivate(strategy: &PersistenceStrategy) {
    static WARNED: Once = Once::new();
    if matches!(strategy, PersistenceStrategy::WriteOnDeactivate) {
        WARNED.call_once(|| {
            tracing::warn!(
                "persistence: at least one grain opts into PersistenceStrategy::WriteOnDeactivate. \
                 State changes are lost on silo crash before idle deactivation. The durable \
                 default is WriteThrough; use WriteBack(interval) for a periodic flush tradeoff."
            );
        });
    }
}

// --- Public entry points ---

/// Standard persistent mailbox. Handles reentrant grains automatically.
///
/// If `replication_sink` is `Some`, serialized state snapshots are sent to the
/// sink after each handler invocation (for cross-cluster replication).
pub(crate) async fn run<G>(
    grain_id: GrainId,
    rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    store: Arc<dyn StateStore>,
    strategy: PersistenceStrategy,
    cancellation: CancellationToken,
    serializer: SerializerFormat,
    replication_sink: Option<Arc<ReplicationSink>>,
) where
    G: PersistentGrain,
    G::State: Serialize + DeserializeOwned,
{
    warn_once_if_write_on_deactivate(&strategy);
    G::on_before_load(&grain_id).await;
    let initial = load_or_default::<G::State>(&store, &grain_id, serializer).await;
    G::on_after_load(&grain_id).await;
    tracing::debug!(%grain_id, "persistence callbacks: load complete");

    let ctx = GrainContext::new(grain_id.clone(), activator)
        .with_cancellation(cancellation);

    let (final_state, faulted) = run_lifecycle::<G>(
        initial, rx, &ctx, &grain_id, &strategy, &store, &serializer, &replication_sink,
    )
    .await;

    // Skip the final save on a faulted activation: state may be corrupt, so the
    // last good persisted state is preserved for the next activation. Directory
    // removal still happens below regardless.
    if !faulted {
        // Serialize synchronously (no &state across await -> no Sync bound), then save with retry
        G::on_before_save(&grain_id).await;
        match serializer.serialize(&final_state) {
            Ok(bytes) => {
                let saved = save_with_retry(&store, &grain_id, &bytes).await;
                if saved {
                    G::on_after_save(&grain_id).await;
                    // Final replication entry on deactivation
                    if let Some(sink) = replication_sink {
                        sink.send(bytes);
                    }
                }
            }
            Err(e) => tracing::error!(%grain_id, error = %e, "failed to serialize grain state"),
        }
    }

    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, faulted, "persistent grain deactivated");
}

/// Versioned persistent mailbox. Migration on load, version metadata on save.
/// Handles reentrant grains automatically.
pub(crate) async fn run_versioned<G>(
    grain_id: GrainId,
    rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    store: Arc<dyn StateStore>,
    strategy: PersistenceStrategy,
    cancellation: CancellationToken,
    serializer: SerializerFormat,
) where
    G: VersionedGrain,
    G::State: Serialize + DeserializeOwned,
{
    warn_once_if_write_on_deactivate(&strategy);
    G::on_before_load(&grain_id).await;
    let initial = match load_versioned_state::<G>(&store, &grain_id, serializer).await {
        Ok(Some(s)) => {
            tracing::debug!(%grain_id, "versioned grain state loaded");
            s
        }
        Ok(None) => {
            tracing::debug!(%grain_id, "no persisted state, using default");
            G::State::default()
        }
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "failed to load versioned state, using default");
            G::State::default()
        }
    };
    G::on_after_load(&grain_id).await;

    let ctx = GrainContext::new(grain_id.clone(), activator)
        .with_cancellation(cancellation);

    let (final_state, faulted) =
        run_lifecycle::<G>(initial, rx, &ctx, &grain_id, &strategy, &store, &serializer, &None).await;

    // Skip the final save (state + version metadata) on a faulted activation.
    // Serialize synchronously, then save with retry + version metadata.
    let state_saved = if faulted {
        false
    } else {
        G::on_before_save(&grain_id).await;
        match serializer.serialize(&final_state) {
            Ok(bytes) => {
                let saved = save_with_retry(&store, &grain_id, &bytes).await;
                if saved {
                    G::on_after_save(&grain_id).await;
                }
                saved
            }
            Err(e) => {
                tracing::error!(%grain_id, error = %e, "failed to serialize versioned grain state");
                false
            }
        }
    };
    if state_saved {
        let version_id = version_grain_id(&grain_id);
        match bincode::serde::encode_to_vec(G::state_version(), bincode::config::standard()) {
            Ok(vb) => {
                if let Err(e) = store.save(&version_id, &vb).await {
                    tracing::warn!(%grain_id, error = %e, "failed to save version metadata");
                }
            }
            Err(e) => tracing::warn!(%grain_id, error = %e, "failed to serialize version"),
        }
    }

    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, "versioned grain deactivated");
}

// --- Unified lifecycle ---

/// Runs activate -> message loop -> deactivate, returning the final state and
/// whether the activation **faulted** (handler/lifecycle panic or a crash-durable
/// persist failure). A faulted activation has corrupt state: `on_deactivate` is
/// skipped and the caller must skip the final save so the last good persisted
/// state survives for the next activation.
async fn run_lifecycle<G: PersistentGrain>(
    mut state: G::State,
    rx: mpsc::Receiver<Envelope>,
    ctx: &GrainContext,
    grain_id: &GrainId,
    strategy: &PersistenceStrategy,
    store: &Arc<dyn StateStore>,
    serializer: &SerializerFormat,
    replication_sink: &Option<Arc<ReplicationSink>>,
) -> (G::State, bool)
where
    G::State: Serialize + DeserializeOwned,
{
    // Contain an on_activate panic: skip the loop and fault so the caller still
    // removes the directory entry instead of leaking it on a panicked task.
    if catch_panic(G::on_activate(&mut state, ctx)).await.is_err() {
        tracing::error!(%grain_id, "on_activate panicked — persistent activation faulted");
        return (state, true);
    }

    let (mut state, faulted) = if G::reentrant() {
        // Reentrant handlers run in a JoinSet; panics are captured there as
        // JoinErrors (directory cleanup is not skipped), so the loop never faults.
        let s = reentrant_loop::<G>(
            state, rx, ctx, grain_id, strategy, store, serializer, replication_sink,
        )
        .await;
        (s, false)
    } else {
        sequential_loop::<G>(state, rx, ctx, grain_id, strategy, store, serializer, replication_sink)
            .await
    };

    if !faulted {
        let _ = catch_panic(G::on_deactivate(&mut state, ctx)).await;
    }
    (state, faulted)
}

// --- Message loops ---

/// FSM-driven sequential loop. Each turn runs:
/// `Idle -MessageReceived-> Processing -(handler)->` then either
/// `HandlerCompleted -> Idle` (no per-message write), `PersistStarted ->
/// Persisting -PersistSucceeded-> Idle`, or, on a handler panic or a crash-durable
/// persist failure, `-> Faulted`. Returns `(state, faulted)`.
async fn sequential_loop<G: PersistentGrain>(
    mut state: G::State,
    mut rx: mpsc::Receiver<Envelope>,
    ctx: &GrainContext,
    grain_id: &GrainId,
    strategy: &PersistenceStrategy,
    store: &Arc<dyn StateStore>,
    serializer: &SerializerFormat,
    replication_sink: &Option<Arc<ReplicationSink>>,
) -> (G::State, bool)
where
    G::State: Serialize + DeserializeOwned,
{
    let mut last_save = tokio::time::Instant::now();
    let mut fsm = ActivationState::Idle;

    while fsm == ActivationState::Idle {
        match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                fsm = fsm.next(ActivationEvent::MessageReceived); // Idle -> Processing
                tracing::debug!(%grain_id, "persistent grain handling message");
                let handler = envelope.into_handler_future(&mut state as &mut (dyn Any + Send), ctx);
                if catch_panic(handler).await.is_err() {
                    tracing::error!(%grain_id, "handler panicked — faulting activation, state discarded");
                    fsm = fsm.next(ActivationEvent::HandlerPanicked); // -> Faulted
                    continue;
                }

                // Decide + serialize synchronously so no `&state` is held across
                // an await (grains are intentionally not `Sync`); replication
                // sends are synchronous and handled inside prepare_persist.
                let plan = prepare_persist(
                    &state, grain_id, strategy, *serializer, replication_sink, &mut last_save,
                );
                // `None` => no durable write this turn; `Some(true/false)` =>
                // a write was attempted and (succeeded/failed).
                let persist_result: Option<bool> = match plan {
                    PersistPlan::Skip => None,
                    PersistPlan::SerializeFailed => Some(false),
                    PersistPlan::Write { bytes, label } => {
                        G::on_before_save(grain_id).await;
                        let ok = flush_bytes(&bytes, store, grain_id, label).await;
                        if ok {
                            G::on_after_save(grain_id).await;
                            if let Some(sink) = replication_sink {
                                sink.send(bytes);
                            }
                        }
                        Some(ok)
                    }
                };
                match persist_result {
                    None => fsm = fsm.next(ActivationEvent::HandlerCompleted), // -> Idle
                    Some(true) => {
                        fsm = fsm.next(ActivationEvent::PersistStarted); // -> Persisting
                        fsm = fsm.next(ActivationEvent::PersistSucceeded); // -> Idle
                    }
                    Some(false) => {
                        fsm = fsm.next(ActivationEvent::PersistStarted); // -> Persisting
                        fsm = fsm.next(ActivationEvent::PersistFailed); // -> Faulted
                    }
                }
            }
            Ok(None) => {
                tracing::debug!(%grain_id, "persistent grain mailbox closed");
                fsm = fsm.next(ActivationEvent::ChannelClosed); // -> Draining
            }
            Err(_) => {
                tracing::debug!(%grain_id, "persistent grain idle, deactivating");
                fsm = fsm.next(ActivationEvent::IdleTimeout); // -> Draining
            }
        }
    }

    (state, fsm == ActivationState::Faulted)
}

/// The durable action decided after a handler runs. Computed synchronously so the
/// `&state` borrow never crosses an await point (grains are intentionally not
/// `Sync`); the caller performs the async store flush with the owned `bytes`.
enum PersistPlan {
    /// No durable write this turn (replication, if any, was already sent).
    Skip,
    /// A write was required but state could not be serialized.
    SerializeFailed,
    /// Flush these bytes to the store (and replicate on success).
    Write { bytes: Vec<u8>, label: &'static str },
}

/// Decide the per-message persistence action and serialize synchronously.
/// Replication sends are synchronous and performed here for the no-write case
/// (`WriteOnDeactivate`); for write strategies the caller replicates after a
/// successful flush. Returning owned bytes keeps `&state` out of the caller's
/// await, so `G::State` need not be `Sync`.
fn prepare_persist<S: Serialize>(
    state: &S,
    grain_id: &GrainId,
    strategy: &PersistenceStrategy,
    serializer: SerializerFormat,
    replication_sink: &Option<Arc<ReplicationSink>>,
    last_save: &mut tokio::time::Instant,
) -> PersistPlan {
    match strategy {
        PersistenceStrategy::WriteThrough => {
            match try_serialize(state, grain_id, "write-through", serializer) {
                Some(bytes) => PersistPlan::Write { bytes, label: "write-through" },
                None => PersistPlan::SerializeFailed,
            }
        }
        PersistenceStrategy::WriteBack(interval) => {
            if last_save.elapsed() < *interval {
                return PersistPlan::Skip;
            }
            *last_save = tokio::time::Instant::now();
            match try_serialize(state, grain_id, "write-back", serializer) {
                Some(bytes) => PersistPlan::Write { bytes, label: "write-back" },
                None => PersistPlan::SerializeFailed,
            }
        }
        PersistenceStrategy::WriteOnDeactivate => {
            // No per-message durable write; replicate if a sink is present
            // (replication and persistence are independent concerns).
            if let Some(sink) = replication_sink
                && let Some(bytes) = try_serialize(state, grain_id, "replication", serializer)
            {
                sink.send(bytes);
            }
            PersistPlan::Skip
        }
    }
}

async fn reentrant_loop<G: Grain>(
    initial: G::State,
    mut rx: mpsc::Receiver<Envelope>,
    ctx: &GrainContext,
    grain_id: &GrainId,
    strategy: &PersistenceStrategy,
    store: &Arc<dyn StateStore>,
    serializer: &SerializerFormat,
    replication_sink: &Option<Arc<ReplicationSink>>,
) -> G::State
where
    G::State: Serialize,
{
    let state: Arc<tokio::sync::Mutex<Box<dyn Any + Send>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(initial)));
    let mut tasks = JoinSet::new();
    const MAX_CONCURRENT: usize = 256;

    // WriteBack falls back to WriteOnDeactivate for reentrant grains because
    // coordinating a periodic timer with the concurrent task set adds complexity
    // without clear benefit (deactivation save is the safety net).
    let write_through = matches!(strategy, PersistenceStrategy::WriteThrough);
    let replicate = replication_sink.is_some();

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
                        while tasks.len() >= MAX_CONCURRENT {
                            if let Some(Err(e)) = tasks.join_next().await {
                                tracing::warn!(%grain_id, error = %e, "reentrant handler panicked");
                            }
                        }
                        let s = state.clone();
                        let c = ctx.clone();
                        let store_ref = store.clone();
                        let gid = grain_id.clone();
                        let ser = *serializer;
                        let sink = replication_sink.clone();
                        tasks.spawn(async move {
                            // Serialize inside the lock, save outside --
                            // avoids holding &state across the store.save() await.
                            let save_bytes = {
                                let mut guard = s.lock().await;
                                envelope.handle(&mut **guard, &c).await;
                                if write_through || replicate {
                                    guard
                                        .downcast_ref::<G::State>()
                                        .and_then(|typed| try_serialize(typed, &gid, "write-through", ser))
                                } else {
                                    None
                                }
                            };
                            if let Some(bytes) = save_bytes {
                                if write_through {
                                    flush_bytes(&bytes, &store_ref, &gid, "write-through").await;
                                }
                                if let Some(ref sink) = sink {
                                    sink.send(bytes);
                                }
                            }
                        });
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }

    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            tracing::warn!(%grain_id, error = %e, "reentrant handler panicked during shutdown");
        }
    }

    let boxed = Arc::try_unwrap(state)
        .expect("all tasks drained, Arc should have single owner")
        .into_inner();
    *boxed.downcast::<G::State>().expect("state type unchanged")
}

// --- Load helpers ---

async fn load_or_default<S: Default + DeserializeOwned>(
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
    serializer: SerializerFormat,
) -> S {
    // Retry transient load failures before falling back to default.
    // A silent fallback to Default on a transient store error (network blip,
    // SQLite lock contention) would discard persisted state.
    for attempt in 1..=3u64 {
        match load_state::<S>(store, grain_id, serializer).await {
            Ok(Some(s)) => {
                tracing::debug!(%grain_id, "grain state loaded from store");
                return s;
            }
            Ok(None) => {
                tracing::debug!(%grain_id, "no persisted state, using default");
                return S::default();
            }
            Err(e) => {
                if attempt < 3 {
                    tracing::warn!(%grain_id, attempt, error = %e, "failed to load state, retrying");
                    tokio::time::sleep(std::time::Duration::from_millis(100 * attempt)).await;
                } else {
                    tracing::error!(%grain_id, error = %e, "failed to load state after 3 attempts, using default");
                }
            }
        }
    }
    S::default()
}

async fn load_state<S: DeserializeOwned>(
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
    serializer: SerializerFormat,
) -> Result<Option<S>, PersistenceError> {
    let Some(bytes) = store.load(grain_id).await? else {
        return Ok(None);
    };
    let state = serializer.deserialize(&bytes)?;
    Ok(Some(state))
}

pub(crate) fn serialize_state<S: Serialize>(state: &S) -> Result<Vec<u8>, PersistenceError> {
    bincode::serde::encode_to_vec(state, bincode::config::standard())
        .map_err(|e| PersistenceError::Serialization(e.to_string()))
}

// --- Save helpers ---

/// Serialize state synchronously, then save asynchronously.
/// This two-step approach avoids holding `&state` across an await point,
/// which would require `Sync` -- a bound grains intentionally do not have.
/// Best-effort single attempt; deactivation saves use `save_with_retry` as the safety net.
fn try_serialize<S: Serialize>(
    state: &S,
    grain_id: &GrainId,
    label: &str,
    serializer: SerializerFormat,
) -> Option<Vec<u8>> {
    match serializer.serialize(state) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "{} serialization failed", label);
            None
        }
    }
}

/// Flush serialized bytes to the store. Returns `true` on success; a failure is
/// logged and surfaced so the caller can fault a crash-durable activation.
async fn flush_bytes(
    bytes: &[u8],
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
    label: &str,
) -> bool {
    if let Err(e) = store.save(grain_id, bytes).await {
        tracing::warn!(%grain_id, error = %e, "{} save failed", label);
        return false;
    }
    true
}

// --- Versioned load/save ---

fn version_grain_id(grain_id: &GrainId) -> GrainId {
    GrainId {
        type_name: grain_id.type_name,
        key: format!("{}/__v", grain_id.key),
    }
}

/// Save bytes with up to 3 retries. Returns true if saved successfully.
async fn save_with_retry(
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
    bytes: &[u8],
) -> bool {
    for attempt in 1..=3u64 {
        match store.save(grain_id, bytes).await {
            Ok(()) => {
                tracing::debug!(%grain_id, "grain state saved");
                return true;
            }
            Err(e) => {
                tracing::warn!(%grain_id, attempt, error = %e, "failed to save grain state");
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(100 * attempt)).await;
                }
            }
        }
    }
    tracing::error!(%grain_id, "grain state save failed after 3 attempts -- state may be lost");
    false
}

pub(crate) async fn load_versioned_state<G>(
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
    serializer: SerializerFormat,
) -> Result<Option<G::State>, PersistenceError>
where
    G: VersionedGrain,
    G::State: Serialize + DeserializeOwned,
{
    let current_version = G::state_version();
    let version_id = version_grain_id(grain_id);

    let stored_version: u32 = match store.load(&version_id).await? {
        Some(bytes) => {
            let (v, _) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
            v
        }
        None => 0,
    };

    let Some(mut state_bytes) = store.load(grain_id).await? else {
        return Ok(None);
    };

    if stored_version > current_version {
        return Err(PersistenceError::Deserialization(format!(
            "stored version {} is newer than current version {} -- cannot downgrade",
            stored_version, current_version
        )));
    }

    if stored_version < current_version {
        tracing::info!(%grain_id, from = stored_version, to = current_version, "migrating grain state");
        for v in stored_version..current_version {
            state_bytes = G::migrate(v, state_bytes)?;
        }
    }

    let state = serializer.deserialize(&state_bytes)?;
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};

    use orlando_core::testing::FakeActivator;
    use orlando_core::{
        CancellationToken, Grain, GrainActivator, GrainContext, GrainHandler, GrainId, Message,
        build_ask_envelope,
    };

    use super::run;
    use crate::InMemoryStateStore;
    use crate::persistent_grain::PersistentGrain;
    use crate::serializer::SerializerFormat;
    use crate::store::{ETag, PersistenceError, PersistenceStrategy, StateStore};

    #[derive(Default, Serialize, Deserialize)]
    struct CounterState {
        count: i64,
    }

    struct CounterGrain;

    #[async_trait]
    impl Grain for CounterGrain {
        type State = CounterState;
        fn idle_timeout() -> Duration {
            // Short so the graceful idle path resolves quickly under test.
            Duration::from_millis(50)
        }
    }

    #[async_trait]
    impl PersistentGrain for CounterGrain {}

    struct Add(i64);
    impl Message for Add {
        type Result = i64;
    }

    #[async_trait]
    impl GrainHandler<Add> for CounterGrain {
        async fn handle(s: &mut CounterState, m: Add, _c: &GrainContext) -> i64 {
            s.count += m.0;
            s.count
        }
    }

    struct Boom;
    impl Message for Boom {
        type Result = ();
    }

    #[async_trait]
    impl GrainHandler<Boom> for CounterGrain {
        async fn handle(_s: &mut CounterState, _m: Boom, _c: &GrainContext) {
            panic!("intentional handler panic");
        }
    }

    /// A store whose `save` always fails — used to drive the `PersistFailed -> Faulted`
    /// transition under a crash-durable strategy.
    #[derive(Debug)]
    struct FailingStore;

    #[async_trait]
    impl StateStore for FailingStore {
        async fn load(&self, _: &GrainId) -> Result<Option<Vec<u8>>, PersistenceError> {
            Ok(None)
        }
        async fn save(&self, _: &GrainId, _: &[u8]) -> Result<(), PersistenceError> {
            Err(PersistenceError::Io(std::io::Error::other("forced save failure")))
        }
        async fn delete(&self, _: &GrainId) -> Result<(), PersistenceError> {
            Ok(())
        }
        async fn load_with_etag(
            &self,
            _: &GrainId,
        ) -> Result<Option<(Vec<u8>, ETag)>, PersistenceError> {
            Ok(None)
        }
        async fn save_with_etag(
            &self,
            _: &GrainId,
            _: &[u8],
            _: Option<&ETag>,
        ) -> Result<ETag, PersistenceError> {
            Err(PersistenceError::Io(std::io::Error::other("forced save failure")))
        }
    }

    fn gid() -> GrainId {
        GrainId {
            type_name: "CounterGrain",
            key: "k".into(),
        }
    }

    /// A handler panic in the sequential persistent loop must be contained (the
    /// mailbox task must not panic), the activation must be removed from the
    /// directory, and the corrupt final state must NOT be written to the store.
    #[tokio::test]
    async fn handler_panic_is_contained_and_final_save_skipped() {
        let activator = FakeActivator::new();
        let store = Arc::new(InMemoryStateStore::new());
        let (tx, rx) = tokio::sync::mpsc::channel(8);

        let task = tokio::spawn(run::<CounterGrain>(
            gid(),
            rx,
            activator.clone(),
            store.clone(),
            PersistenceStrategy::WriteOnDeactivate,
            CancellationToken::new(),
            SerializerFormat::Bincode,
            None,
        ));
        activator.register(gid(), tx.clone(), tokio::spawn(async {}));

        let (env, _reply) = build_ask_envelope::<CounterGrain, Boom>(Boom);
        tx.send(env).await.unwrap();

        // Panic faults the FSM, which exits the loop; the task must finish cleanly.
        task.await.expect("mailbox task must not panic");
        assert!(
            activator.get_sender(&gid()).is_none(),
            "faulted activation must be removed from the directory"
        );
        assert!(
            store.load(&gid()).await.unwrap().is_none(),
            "faulted activation must not persist corrupt state"
        );
    }

    /// Control: a normal message under WriteOnDeactivate persists on graceful
    /// (channel-close) deactivation and removes the directory entry.
    #[tokio::test]
    async fn graceful_deactivation_persists_state() {
        let activator = FakeActivator::new();
        let store = Arc::new(InMemoryStateStore::new());
        let (tx, rx) = tokio::sync::mpsc::channel(8);

        let task = tokio::spawn(run::<CounterGrain>(
            gid(),
            rx,
            activator.clone(),
            store.clone(),
            PersistenceStrategy::WriteOnDeactivate,
            CancellationToken::new(),
            SerializerFormat::Bincode,
            None,
        ));
        activator.register(gid(), tx.clone(), tokio::spawn(async {}));

        let (env, reply) = build_ask_envelope::<CounterGrain, Add>(Add(5));
        tx.send(env).await.unwrap();
        let result = reply.await.unwrap().downcast::<i64>().unwrap();
        assert_eq!(*result, 5);

        drop(tx); // close channel -> graceful drain -> deactivation save
        task.await.expect("mailbox task must not panic");
        assert!(
            store.load(&gid()).await.unwrap().is_some(),
            "graceful deactivation must persist state"
        );
        assert!(activator.get_sender(&gid()).is_none());
    }

    /// A store write failure under a crash-durable strategy (WriteThrough) drives
    /// PersistFailed -> Faulted: the handler still replied, but the activation
    /// faults, exits, and is removed without the task panicking.
    #[tokio::test]
    async fn persist_failure_faults_activation() {
        let activator = FakeActivator::new();
        let store = Arc::new(FailingStore);
        let (tx, rx) = tokio::sync::mpsc::channel(8);

        let task = tokio::spawn(run::<CounterGrain>(
            gid(),
            rx,
            activator.clone(),
            store,
            PersistenceStrategy::WriteThrough,
            CancellationToken::new(),
            SerializerFormat::Bincode,
            None,
        ));
        activator.register(gid(), tx.clone(), tokio::spawn(async {}));

        let (env, reply) = build_ask_envelope::<CounterGrain, Add>(Add(5));
        tx.send(env).await.unwrap();
        // Handler completes and replies before the persist step fails.
        let result = reply.await.unwrap().downcast::<i64>().unwrap();
        assert_eq!(*result, 5);

        // The persist failure faults the FSM, exiting the loop without a hang.
        task.await.expect("mailbox task must not panic");
        assert!(
            activator.get_sender(&gid()).is_none(),
            "persist-faulted activation must be removed from the directory"
        );
    }
}
