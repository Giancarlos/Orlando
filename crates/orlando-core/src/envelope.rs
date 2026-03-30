use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use crate::grain_context::GrainContext;

pub type HandleFn = Box<
    dyn for<'a> FnOnce(
            &'a mut (dyn Any + Send),
            &'a GrainContext,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send,
>;

pub struct Envelope {
    pub(crate) handle_fn: HandleFn,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope").finish_non_exhaustive()
    }
}

impl Envelope {
    pub fn new(handle_fn: HandleFn) -> Self {
        Self { handle_fn }
    }

    pub async fn handle(self, state: &mut (dyn Any + Send), ctx: &GrainContext) {
        (self.handle_fn)(state, ctx).await;
    }
}
