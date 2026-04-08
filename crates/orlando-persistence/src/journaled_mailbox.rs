use std::any::Any;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::mpsc;
use tokio::time::timeout;

use orlando_core::{Envelope, GrainActivator, GrainContext, GrainId};

use crate::journal_store::JournalStore;
use crate::journaled_grain::JournaledGrain;
use crate::persistent_mailbox::serialize_state;

/// Wrapper that bundles grain state with journal metadata.
/// The envelope closure downcasts to this type to access both state and journal info.
pub(crate) struct JournaledState<S> {
    pub(crate) state: S,
    pub(crate) sequence: u64,
    pub(crate) events_since_snapshot: u64,
    pub(crate) snapshot_interval: u64,
}

pub(crate) async fn run<G>(
    grain_id: GrainId,
    mut rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
    journal: Arc<dyn JournalStore>,
) where
    G: JournaledGrain,
    G::State: Serialize + DeserializeOwned,
    G::Event: Serialize + DeserializeOwned,
{
    let ctx = GrainContext::new(grain_id.clone(), activator);

    // Load state from snapshot + replay events
    let (state, sequence) = load_state::<G>(&journal, &grain_id).await;

    let mut journaled = JournaledState::<G::State> {
        state,
        sequence,
        events_since_snapshot: 0,
        snapshot_interval: G::snapshot_interval(),
    };

    G::on_activate(&mut journaled.state, &ctx).await;

    // Message loop — envelope closures handle event persistence internally
    loop {
        match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                tracing::debug!(%grain_id, "journaled grain handling message");
                envelope
                    .handle(&mut journaled as &mut (dyn Any + Send), &ctx)
                    .await;
            }
            Ok(None) => {
                tracing::debug!(%grain_id, "journaled grain mailbox closed");
                break;
            }
            Err(_) => {
                tracing::debug!(%grain_id, "journaled grain idle, deactivating");
                break;
            }
        }
    }

    // Save final snapshot on deactivation
    G::on_deactivate(&mut journaled.state, &ctx).await;
    match serialize_state(&journaled.state) {
        Ok(bytes) => {
            if let Err(e) = journal
                .save_snapshot(&grain_id, journaled.sequence, &bytes)
                .await
            {
                tracing::warn!(%grain_id, error = %e, "failed to save snapshot on deactivation");
            }
        }
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "failed to serialize state for snapshot");
        }
    }

    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, "journaled grain deactivated");
}

async fn load_state<G>(journal: &Arc<dyn JournalStore>, grain_id: &GrainId) -> (G::State, u64)
where
    G: JournaledGrain,
    G::State: Serialize + DeserializeOwned,
    G::Event: DeserializeOwned,
{
    // Try loading snapshot
    let (mut state, mut sequence) = match journal.load_snapshot(grain_id).await {
        Ok(Some((seq, bytes))) => {
            match bincode::serde::decode_from_slice::<G::State, _>(
                &bytes,
                bincode::config::standard(),
            ) {
                Ok((s, _)) => {
                    tracing::debug!(%grain_id, seq, "loaded snapshot");
                    (s, seq)
                }
                Err(e) => {
                    tracing::warn!(
                        %grain_id,
                        error = %e,
                        "failed to deserialize snapshot, starting fresh"
                    );
                    (G::State::default(), 0)
                }
            }
        }
        Ok(None) => (G::State::default(), 0),
        Err(e) => {
            tracing::warn!(
                %grain_id,
                error = %e,
                "failed to load snapshot, starting fresh"
            );
            (G::State::default(), 0)
        }
    };

    // Replay events after snapshot
    let events = match journal.load_events_after(grain_id, sequence).await {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(%grain_id, error = %e, "failed to load events");
            Vec::new()
        }
    };

    for entry in &events {
        match bincode::serde::decode_from_slice::<G::Event, _>(
            &entry.event_bytes,
            bincode::config::standard(),
        ) {
            Ok((event, _)) => {
                G::apply(&mut state, &event);
                sequence = entry.sequence;
            }
            Err(e) => {
                tracing::warn!(
                    %grain_id,
                    seq = entry.sequence,
                    error = %e,
                    "failed to deserialize event, skipping"
                );
            }
        }
    }

    if !events.is_empty() {
        tracing::debug!(%grain_id, replayed = events.len(), "events replayed");
    }

    (state, sequence)
}
