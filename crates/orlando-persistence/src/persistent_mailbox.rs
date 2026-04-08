use std::any::Any;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;

use orlando_core::{CancellationToken, Envelope, Grain, GrainActivator, GrainContext, GrainId};

use crate::persistent_grain::PersistentGrain;
use crate::store::{PersistenceError, PersistenceStrategy, StateStore};
use crate::versioned_grain::VersionedGrain;

// --- Public entry points ---

/// Standard persistent mailbox. Handles reentrant grains automatically.
pub(crate) async fn run<G>(
    grain_id: GrainId,
    rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    store: Arc<dyn StateStore>,
    strategy: PersistenceStrategy,
    cancellation: CancellationToken,
) where
    G: PersistentGrain,
    G::State: Serialize + DeserializeOwned,
{
    let initial = load_or_default::<G::State>(&store, &grain_id).await;
    let ctx = GrainContext::new(grain_id.clone(), activator)
        .with_cancellation(cancellation);

    let final_state = run_lifecycle::<G>(initial, rx, &ctx, &grain_id, &strategy, &store).await;

    // Serialize synchronously (no &state across await → no Sync bound), then save with retry
    match serialize_state(&final_state) {
        Ok(bytes) => { save_with_retry(&store, &grain_id, &bytes).await; }
        Err(e) => tracing::error!(%grain_id, error = %e, "failed to serialize grain state"),
    }

    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, "persistent grain deactivated");
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
) where
    G: VersionedGrain,
    G::State: Serialize + DeserializeOwned,
{
    let initial = match load_versioned_state::<G>(&store, &grain_id).await {
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

    let ctx = GrainContext::new(grain_id.clone(), activator)
        .with_cancellation(cancellation);

    let final_state = run_lifecycle::<G>(initial, rx, &ctx, &grain_id, &strategy, &store).await;

    // Serialize synchronously, then save with retry + version metadata
    let state_saved = match serialize_state(&final_state) {
        Ok(bytes) => save_with_retry(&store, &grain_id, &bytes).await,
        Err(e) => {
            tracing::error!(%grain_id, error = %e, "failed to serialize versioned grain state");
            false
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

async fn run_lifecycle<G: Grain>(
    mut state: G::State,
    rx: mpsc::Receiver<Envelope>,
    ctx: &GrainContext,
    grain_id: &GrainId,
    strategy: &PersistenceStrategy,
    store: &Arc<dyn StateStore>,
) -> G::State
where
    G::State: Serialize,
{
    G::on_activate(&mut state, ctx).await;

    let mut state = if G::reentrant() {
        reentrant_loop::<G>(state, rx, ctx, grain_id, strategy, store).await
    } else {
        sequential_loop::<G>(state, rx, ctx, grain_id, strategy, store).await
    };

    G::on_deactivate(&mut state, ctx).await;
    state
}

// --- Message loops ---

async fn sequential_loop<G: Grain>(
    mut state: G::State,
    mut rx: mpsc::Receiver<Envelope>,
    ctx: &GrainContext,
    grain_id: &GrainId,
    strategy: &PersistenceStrategy,
    store: &Arc<dyn StateStore>,
) -> G::State
where
    G::State: Serialize,
{
    let mut last_save = tokio::time::Instant::now();

    loop {
        match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                tracing::debug!(%grain_id, "persistent grain handling message");
                envelope.handle(&mut state as &mut (dyn Any + Send), ctx).await;

                // Apply persistence strategy after each message.
                // Serialize synchronously (no &state across await → no Sync needed),
                // then flush the bytes to the store asynchronously.
                match strategy {
                    PersistenceStrategy::WriteThrough => {
                        if let Some(bytes) = serialize_for_save(&state, grain_id, "write-through") {
                            flush_bytes(&bytes, store, grain_id, "write-through").await;
                        }
                    }
                    PersistenceStrategy::WriteBack(interval) => {
                        if last_save.elapsed() >= *interval {
                            if let Some(bytes) = serialize_for_save(&state, grain_id, "write-back") {
                                flush_bytes(&bytes, store, grain_id, "write-back").await;
                            }
                            last_save = tokio::time::Instant::now();
                        }
                    }
                    PersistenceStrategy::WriteOnDeactivate => {} // save only at the end
                }
            }
            Ok(None) => {
                tracing::debug!(%grain_id, "persistent grain mailbox closed");
                break;
            }
            Err(_) => {
                tracing::debug!(%grain_id, "persistent grain idle, deactivating");
                break;
            }
        }
    }
    state
}

async fn reentrant_loop<G: Grain>(
    initial: G::State,
    mut rx: mpsc::Receiver<Envelope>,
    ctx: &GrainContext,
    grain_id: &GrainId,
    strategy: &PersistenceStrategy,
    store: &Arc<dyn StateStore>,
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
                        tasks.spawn(async move {
                            // Serialize inside the lock, save outside —
                            // avoids holding &state across the store.save() await.
                            let save_bytes = {
                                let mut guard = s.lock().await;
                                envelope.handle(&mut **guard, &c).await;
                                if write_through {
                                    guard
                                        .downcast_ref::<G::State>()
                                        .and_then(|typed| serialize_for_save(typed, &gid, "write-through"))
                                } else {
                                    None
                                }
                            };
                            if let Some(bytes) = save_bytes {
                                flush_bytes(&bytes, &store_ref, &gid, "write-through").await;
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
) -> S {
    // Retry transient load failures before falling back to default.
    // A silent fallback to Default on a transient store error (network blip,
    // SQLite lock contention) would discard persisted state.
    for attempt in 1..=3u64 {
        match load_state::<S>(store, grain_id).await {
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
) -> Result<Option<S>, PersistenceError> {
    let Some(bytes) = store.load(grain_id).await? else {
        return Ok(None);
    };
    let (state, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
    Ok(Some(state))
}

pub(crate) fn serialize_state<S: Serialize>(state: &S) -> Result<Vec<u8>, PersistenceError> {
    bincode::serde::encode_to_vec(state, bincode::config::standard())
        .map_err(|e| PersistenceError::Serialization(e.to_string()))
}

// --- Save helpers ---

/// Serialize state synchronously, then save asynchronously.
/// This two-step approach avoids holding `&state` across an await point,
/// which would require `Sync` — a bound grains intentionally do not have.
/// Best-effort single attempt; deactivation saves use `save_with_retry` as the safety net.
fn serialize_for_save<S: Serialize>(
    state: &S,
    grain_id: &GrainId,
    label: &str,
) -> Option<Vec<u8>> {
    match serialize_state(state) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "{} serialization failed", label);
            None
        }
    }
}

async fn flush_bytes(
    bytes: &[u8],
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
    label: &str,
) {
    if let Err(e) = store.save(grain_id, bytes).await {
        tracing::warn!(%grain_id, error = %e, "{} save failed", label);
    }
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
    tracing::error!(%grain_id, "grain state save failed after 3 attempts — state may be lost");
    false
}

pub(crate) async fn load_versioned_state<G>(
    store: &Arc<dyn StateStore>,
    grain_id: &GrainId,
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
            "stored version {} is newer than current version {} — cannot downgrade",
            stored_version, current_version
        )));
    }

    if stored_version < current_version {
        tracing::info!(%grain_id, from = stored_version, to = current_version, "migrating grain state");
        for v in stored_version..current_version {
            state_bytes = G::migrate(v, state_bytes)?;
        }
    }

    let (state, _) =
        bincode::serde::decode_from_slice(&state_bytes, bincode::config::standard())
            .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
    Ok(Some(state))
}

