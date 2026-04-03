use std::any::Any;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;

use orlando_core::{Envelope, GrainActivator, GrainContext, GrainId};

use crate::persistent_grain::PersistentGrain;
use crate::store::{PersistenceError, StateStore};

pub(crate) async fn run<G>(
    grain_id: GrainId,
    mut rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    store: Arc<dyn StateStore>,
) where
    G: PersistentGrain,
    G::State: Serialize + DeserializeOwned,
{
    // Load state from store, or use default
    let mut state = match load_state::<G::State>(&store, &grain_id).await {
        Ok(Some(loaded)) => {
            tracing::debug!(%grain_id, "grain state loaded from store");
            loaded
        }
        Ok(None) => {
            tracing::debug!(%grain_id, "no persisted state, using default");
            G::State::default()
        }
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "failed to load state, using default");
            G::State::default()
        }
    };

    let ctx = GrainContext::new(grain_id.clone(), activator);

    tracing::debug!(%grain_id, "persistent grain activating");
    G::on_activate(&mut state, &ctx).await;

    loop {
        match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                tracing::debug!(%grain_id, "persistent grain handling message");
                envelope.handle(&mut state as &mut (dyn Any + Send), &ctx).await;
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

    G::on_deactivate(&mut state, &ctx).await;

    // Serialize synchronously, then save async (avoids &state crossing await → no Sync bound needed)
    match serialize_state::<G::State>(&state) {
        Ok(bytes) => match store.save(&grain_id, &bytes).await {
            Ok(()) => tracing::debug!(%grain_id, "grain state saved to store"),
            Err(e) => tracing::warn!(%grain_id, error = %e, "failed to save grain state"),
        },
        Err(e) => tracing::warn!(%grain_id, error = %e, "failed to serialize grain state"),
    }

    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, "persistent grain deactivated");
}

/// Reentrant persistent mailbox: concurrent handler execution with state
/// loaded from store on activation and saved on deactivation.
pub(crate) async fn run_reentrant<G>(
    grain_id: GrainId,
    mut rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    store: Arc<dyn StateStore>,
) where
    G: PersistentGrain,
    G::State: Serialize + DeserializeOwned,
{
    let mut initial_state = match load_state::<G::State>(&store, &grain_id).await {
        Ok(Some(loaded)) => {
            tracing::debug!(%grain_id, "reentrant persistent grain state loaded");
            loaded
        }
        Ok(None) => {
            tracing::debug!(%grain_id, "no persisted state, using default");
            G::State::default()
        }
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "failed to load state, using default");
            G::State::default()
        }
    };

    let ctx = GrainContext::new(grain_id.clone(), activator);

    tracing::debug!(%grain_id, "reentrant persistent grain activating");
    G::on_activate(&mut initial_state, &ctx).await;

    let state: Arc<tokio::sync::Mutex<Box<dyn Any + Send>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(initial_state)));

    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            biased;

            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(e)) = result {
                    tracing::warn!(%grain_id, error = %e, "reentrant persistent handler panicked");
                }
            }

            msg = timeout(G::idle_timeout(), rx.recv()) => {
                match msg {
                    Ok(Some(envelope)) => {
                        tracing::debug!(%grain_id, "reentrant persistent grain dispatching message");
                        let state = state.clone();
                        let ctx = ctx.clone();
                        tasks.spawn(async move {
                            let mut guard = state.lock().await;
                            envelope.handle(&mut **guard, &ctx).await;
                        });
                    }
                    Ok(None) => {
                        tracing::debug!(%grain_id, "reentrant persistent grain mailbox closed");
                        break;
                    }
                    Err(_) => {
                        tracing::debug!(%grain_id, "reentrant persistent grain idle, deactivating");
                        break;
                    }
                }
            }
        }
    }

    // Drain in-flight handlers
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            tracing::warn!(%grain_id, error = %e, "reentrant persistent handler panicked during shutdown");
        }
    }

    // Deactivate and save
    {
        let mut guard = state.lock().await;
        let s = guard.downcast_mut::<G::State>().expect("grain state type mismatch");
        G::on_deactivate(s, &ctx).await;

        match serialize_state::<G::State>(s) {
            Ok(bytes) => match store.save(&grain_id, &bytes).await {
                Ok(()) => tracing::debug!(%grain_id, "reentrant persistent grain state saved"),
                Err(e) => tracing::warn!(%grain_id, error = %e, "failed to save grain state"),
            },
            Err(e) => tracing::warn!(%grain_id, error = %e, "failed to serialize grain state"),
        }
    }

    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, "reentrant persistent grain deactivated");
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

// --- Versioned grain support ---

use crate::versioned_grain::VersionedGrain;

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

    // Load stored version (default to 0 for legacy/unversioned state)
    let version_id = version_grain_id(grain_id);
    let stored_version: u32 = match store.load(&version_id).await? {
        Some(bytes) => {
            let (v, _) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
            v
        }
        None => 0,
    };

    // Load state bytes
    let Some(mut state_bytes) = store.load(grain_id).await? else {
        return Ok(None);
    };

    // Run migration chain if needed
    if stored_version > current_version {
        return Err(PersistenceError::Deserialization(format!(
            "stored version {} is newer than current version {} — cannot downgrade",
            stored_version, current_version
        )));
    }

    if stored_version < current_version {
        tracing::info!(
            %grain_id,
            from = stored_version,
            to = current_version,
            "migrating grain state"
        );
        for v in stored_version..current_version {
            state_bytes = G::migrate(v, state_bytes)?;
        }
    }

    let (state, _) =
        bincode::serde::decode_from_slice(&state_bytes, bincode::config::standard())
            .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
    Ok(Some(state))
}

/// Versioned persistent mailbox: loads state with version migration, saves with version.
pub(crate) async fn run_versioned<G>(
    grain_id: GrainId,
    mut rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    store: Arc<dyn StateStore>,
) where
    G: VersionedGrain,
    G::State: Serialize + DeserializeOwned,
{
    let mut state = match load_versioned_state::<G>(&store, &grain_id).await {
        Ok(Some(loaded)) => {
            tracing::debug!(%grain_id, "versioned grain state loaded");
            loaded
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

    tracing::debug!(%grain_id, "versioned grain activating");
    G::on_activate(&mut state, &ctx).await;

    loop {
        match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                tracing::debug!(%grain_id, "versioned grain handling message");
                envelope.handle(&mut state as &mut (dyn Any + Send), &ctx).await;
            }
            Ok(None) => {
                tracing::debug!(%grain_id, "versioned grain mailbox closed");
                break;
            }
            Err(_) => {
                tracing::debug!(%grain_id, "versioned grain idle, deactivating");
                break;
            }
        }
    }

    G::on_deactivate(&mut state, &ctx).await;

    // Serialize synchronously, then save async (avoids &state crossing await → no Sync bound)
    match serialize_state::<G::State>(&state) {
        Ok(state_bytes) => {
            // Save state bytes
            if let Err(e) = store.save(&grain_id, &state_bytes).await {
                tracing::warn!(%grain_id, error = %e, "failed to save versioned grain state");
            } else {
                // Save version
                let version_id = version_grain_id(&grain_id);
                match bincode::serde::encode_to_vec(G::state_version(), bincode::config::standard()) {
                    Ok(version_bytes) => {
                        if let Err(e) = store.save(&version_id, &version_bytes).await {
                            tracing::warn!(%grain_id, error = %e, "failed to save version metadata");
                        } else {
                            tracing::debug!(%grain_id, version = G::state_version(), "versioned grain state saved");
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
