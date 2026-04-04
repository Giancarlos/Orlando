use std::any::Any;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;

use orlando_core::{Envelope, Grain, GrainActivator, GrainContext, GrainId};

use crate::persistent_grain::PersistentGrain;
use crate::store::{PersistenceError, StateStore};
use crate::versioned_grain::VersionedGrain;

// --- Public entry points ---

/// Standard persistent mailbox. Handles reentrant grains automatically.
pub(crate) async fn run<G>(
    grain_id: GrainId,
    rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    store: Arc<dyn StateStore>,
) where
    G: PersistentGrain,
    G::State: Serialize + DeserializeOwned,
{
    let initial = load_or_default::<G::State>(&store, &grain_id).await;
    let ctx = GrainContext::new(grain_id.clone(), activator);

    let final_state = run_lifecycle::<G>(initial, rx, &ctx, &grain_id).await;

    // Serialize synchronously (no &state across await → no Sync bound)
    match serialize_state(&final_state) {
        Ok(bytes) => match store.save(&grain_id, &bytes).await {
            Ok(()) => tracing::debug!(%grain_id, "grain state saved"),
            Err(e) => tracing::warn!(%grain_id, error = %e, "failed to save grain state"),
        },
        Err(e) => tracing::warn!(%grain_id, error = %e, "failed to serialize grain state"),
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

    let ctx = GrainContext::new(grain_id.clone(), activator);

    let final_state = run_lifecycle::<G>(initial, rx, &ctx, &grain_id).await;

    // Serialize synchronously then save
    match serialize_state(&final_state) {
        Ok(state_bytes) => {
            if let Err(e) = store.save(&grain_id, &state_bytes).await {
                tracing::warn!(%grain_id, error = %e, "failed to save versioned grain state");
            } else {
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
        }
        Err(e) => tracing::warn!(%grain_id, error = %e, "failed to serialize versioned grain state"),
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
) -> G::State {
    G::on_activate(&mut state, ctx).await;

    let mut state = if G::reentrant() {
        reentrant_loop::<G>(state, rx, ctx, grain_id).await
    } else {
        sequential_loop::<G>(state, rx, ctx, grain_id).await
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
) -> G::State {
    loop {
        match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                tracing::debug!(%grain_id, "persistent grain handling message");
                envelope.handle(&mut state as &mut (dyn Any + Send), ctx).await;
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
) -> G::State {
    let state: Arc<tokio::sync::Mutex<Box<dyn Any + Send>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(initial)));
    let mut tasks = JoinSet::new();

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
                        let s = state.clone();
                        let c = ctx.clone();
                        tasks.spawn(async move {
                            let mut guard = s.lock().await;
                            envelope.handle(&mut **guard, &c).await;
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
    match load_state::<S>(store, grain_id).await {
        Ok(Some(s)) => {
            tracing::debug!(%grain_id, "grain state loaded from store");
            s
        }
        Ok(None) => {
            tracing::debug!(%grain_id, "no persisted state, using default");
            S::default()
        }
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "failed to load state, using default");
            S::default()
        }
    }
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

// --- Versioned load/save ---

fn version_grain_id(grain_id: &GrainId) -> GrainId {
    GrainId {
        type_name: grain_id.type_name,
        key: format!("{}/__v", grain_id.key),
    }
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

