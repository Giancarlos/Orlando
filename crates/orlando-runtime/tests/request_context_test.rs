use async_trait::async_trait;

use orlando_core::{Grain, GrainContext, GrainHandler, Message, RequestContext};
use orlando_runtime::Silo;

// --- Grain that reads and returns request context values ---

#[derive(Default)]
struct EchoState;

struct ContextEchoGrain;

#[async_trait]
impl Grain for ContextEchoGrain {
    type State = EchoState;
}

struct GetTraceId;
impl Message for GetTraceId {
    type Result = Option<String>;
}

#[async_trait]
impl GrainHandler<GetTraceId> for ContextEchoGrain {
    async fn handle(
        _state: &mut EchoState,
        _msg: GetTraceId,
        ctx: &GrainContext,
    ) -> Option<String> {
        ctx.request_context().get("trace-id").map(|s| s.to_string())
    }
}

// --- Tests ---

#[tokio::test]
async fn request_context_accessible_in_handler() {
    let silo = Silo::new();
    let grain = silo.get_ref::<ContextEchoGrain>("ctx-1");

    // Without context, trace-id is None
    let result = grain.ask(GetTraceId).await.unwrap();
    assert_eq!(result, None);
}

#[tokio::test]
async fn request_context_with_values() {
    let ctx = RequestContext::new().with("trace-id", "abc-123");
    assert_eq!(ctx.get("trace-id"), Some("abc-123"));
    assert_eq!(ctx.get("missing"), None);
}

#[tokio::test]
async fn request_context_immutable_with() {
    let ctx1 = RequestContext::new().with("a", "1");
    let ctx2 = ctx1.with("b", "2");

    // ctx1 is unchanged
    assert_eq!(ctx1.get("a"), Some("1"));
    assert_eq!(ctx1.get("b"), None);

    // ctx2 has both
    assert_eq!(ctx2.get("a"), Some("1"));
    assert_eq!(ctx2.get("b"), Some("2"));
}
