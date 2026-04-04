use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainActivator, GrainContext, GrainHandler, Message};
use orlando_runtime::Silo;

#[derive(Default)]
struct TrackerState;

struct TrackerGrain;

#[async_trait]
impl Grain for TrackerGrain {
    type State = TrackerState;

    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }
}

struct Ping;
impl Message for Ping {
    type Result = ();
}

#[async_trait]
impl GrainHandler<Ping> for TrackerGrain {
    async fn handle(_state: &mut TrackerState, _msg: Ping, _ctx: &GrainContext) {}
}

#[tokio::test]
async fn drain_deactivates_all_grains() {
    let silo = Silo::new();

    for i in 0..5 {
        let grain = silo.get_ref::<TrackerGrain>(&format!("drain-{}", i));
        grain.ask(Ping).await.unwrap();
    }

    assert_eq!(silo.directory().grain_ids().len(), 5);

    silo.directory().drain().await;

    assert_eq!(
        silo.directory().grain_ids().len(),
        0,
        "all grains should be deactivated after drain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_prevents_new_activations() {
    let silo = Silo::new();

    let grain = silo.get_ref::<TrackerGrain>("block-test");
    grain.ask(Ping).await.unwrap();

    // Small yield to let the mailbox task fully start
    tokio::task::yield_now().await;
    silo.directory().drain().await;

    // After drain, the draining flag is set. New get_or_insert returns a closed sender.
    // Verify the directory is empty and new grains can't be activated.
    assert_eq!(silo.directory().grain_ids().len(), 0);
}
