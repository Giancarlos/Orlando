use std::any::Any;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;
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

fn serialize_state<S: Serialize>(state: &S) -> Result<Vec<u8>, PersistenceError> {
    bincode::serde::encode_to_vec(state, bincode::config::standard())
        .map_err(|e| PersistenceError::Serialization(e.to_string()))
}
