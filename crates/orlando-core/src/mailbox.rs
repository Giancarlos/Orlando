use std::any::Any;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::envelope::Envelope;
use crate::grain::Grain;
use crate::grain_context::{GrainActivator, GrainContext};
use crate::grain_id::GrainId;

pub async fn run_mailbox<G: Grain>(
    grain_id: GrainId,
    mut rx: mpsc::Receiver<Envelope>,
    activator: Arc<dyn GrainActivator>,
) {
    let mut state = G::State::default();
    let ctx = GrainContext::new(grain_id.clone(), activator);

    tracing::debug!(%grain_id, "grain activating");
    G::on_activate(&mut state, &ctx).await;

    loop {
        match timeout(G::idle_timeout(), rx.recv()).await {
            Ok(Some(envelope)) => {
                tracing::debug!(%grain_id, "grain handling message");
                envelope.handle(&mut state as &mut (dyn Any + Send), &ctx).await;
            }
            Ok(None) => {
                tracing::debug!(%grain_id, "grain mailbox closed");
                break;
            }
            Err(_) => {
                tracing::debug!(%grain_id, "grain idle, deactivating");
                break;
            }
        }
    }

    G::on_deactivate(&mut state, &ctx).await;
    ctx.activator().remove(&grain_id);
    tracing::debug!(%grain_id, "grain deactivated");
}
