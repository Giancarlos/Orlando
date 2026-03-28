use std::time::Duration;

use async_trait::async_trait;

use crate::grain_context::GrainContext;
use crate::Message;

#[async_trait]
pub trait Grain: Send + 'static {
    type State: Default + Send + 'static;

    async fn on_activate(_state: &mut Self::State, _ctx: &GrainContext) {}
    async fn on_deactivate(_state: &mut Self::State, _ctx: &GrainContext) {}

    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }
}

#[async_trait]
pub trait GrainHandler<M: Message>: Grain {
    async fn handle(state: &mut Self::State, msg: M, ctx: &GrainContext) -> M::Result;
}
