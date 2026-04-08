use std::time::Duration;

use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_runtime::Silo;

#[derive(Default)]
struct CancelState;

struct CancelGrain;

#[async_trait]
impl Grain for CancelGrain {
    type State = CancelState;

    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }
}

struct CheckCancelled;
impl Message for CheckCancelled {
    type Result = bool;
}

#[async_trait]
impl GrainHandler<CheckCancelled> for CancelGrain {
    async fn handle(
        _state: &mut CancelState,
        _msg: CheckCancelled,
        ctx: &GrainContext,
    ) -> bool {
        ctx.is_cancelled()
    }
}

/// Verify that ctx.is_cancelled() returns false under normal operation.
/// This confirms the CancellationToken is properly wired from the Activation
/// through to the GrainContext inside the mailbox.
#[tokio::test]
async fn cancellation_token_is_false_before_drain() {
    let silo = Silo::new();
    let grain = silo.get_ref::<CancelGrain>("test");
    let cancelled = grain.ask(CheckCancelled).await.unwrap();
    assert!(!cancelled, "should not be cancelled before drain");
}

/// Verify that the CancellationToken on the Activation is the SAME token
/// seen by the GrainContext inside the mailbox. We do this by cancelling
/// the token on the Activation directly and checking from the handler.
#[tokio::test]
async fn cancellation_token_shared_between_activation_and_context() {
    let silo = Silo::new();
    let grain = silo.get_ref::<CancelGrain>("shared-token");

    // Activate the grain
    let cancelled = grain.ask(CheckCancelled).await.unwrap();
    assert!(!cancelled);

    // Find the activation and cancel its token directly
    let directory = silo.directory();
    let grain_id = orlando_core::GrainId {
        type_name: CancelGrain::grain_type_name(),
        key: "shared-token".to_string(),
    };

    // Access the activation's cancellation token via the DashMap
    // and cancel it. The next handler call should see is_cancelled() == true.
    for entry in directory.activations().iter() {
        if entry.key() == &grain_id {
            entry.value().cancellation.cancel();
        }
    }

    let cancelled = grain.ask(CheckCancelled).await.unwrap();
    assert!(
        cancelled,
        "handler should see is_cancelled() == true after activation token is cancelled"
    );
}
