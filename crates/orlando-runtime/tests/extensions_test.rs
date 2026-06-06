use std::time::Duration;

use async_trait::async_trait;
use orlando_core::{Extensions, Grain, GrainContext, GrainHandler, Message};
use orlando_runtime::Silo;

// --- Extension type ---

struct RateLimitConfig {
    max_per_second: u32,
}

// --- Grain that installs an extension in on_activate ---

#[derive(Default)]
struct ExtState;

struct ExtGrain;

#[async_trait]
impl Grain for ExtGrain {
    type State = ExtState;

    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }

    async fn on_activate(_state: &mut ExtState, ctx: &GrainContext) {
        ctx.extensions()
            .insert(RateLimitConfig { max_per_second: 100 });
    }
}

struct GetRateLimit;

impl Message for GetRateLimit {
    type Result = u32;
}

#[async_trait]
impl GrainHandler<GetRateLimit> for ExtGrain {
    async fn handle(_state: &mut ExtState, _msg: GetRateLimit, ctx: &GrainContext) -> u32 {
        ctx.extensions()
            .get::<RateLimitConfig>()
            .map(|c| c.max_per_second)
            .unwrap_or(0)
    }
}

// --- Tests ---

#[tokio::test]
async fn extension_set_in_activate_accessible_in_handler() {
    let silo = Silo::new();
    let grain = silo.get_ref::<ExtGrain>("test");
    let limit = grain.ask(GetRateLimit).await.unwrap();
    assert_eq!(limit, 100);
}

#[tokio::test]
async fn extension_not_set_returns_none() {
    let ext = Extensions::new();
    assert!(ext.get::<RateLimitConfig>().is_none());
    assert!(!ext.contains::<RateLimitConfig>());
}

#[tokio::test]
async fn extension_insert_and_remove() {
    let ext = Extensions::new();
    ext.insert(RateLimitConfig { max_per_second: 50 });
    assert_eq!(ext.get::<RateLimitConfig>().unwrap().max_per_second, 50);

    let removed = ext.remove::<RateLimitConfig>().unwrap();
    assert_eq!(removed.max_per_second, 50);
    assert!(ext.get::<RateLimitConfig>().is_none());
}

// --- Ergonomic ctx.set_extension / get_extension API ---

struct Tenant(String);

struct SetTenant(String);
impl Message for SetTenant {
    type Result = ();
}

struct GetTenant;
impl Message for GetTenant {
    type Result = Option<String>;
}

#[async_trait]
impl GrainHandler<SetTenant> for ExtGrain {
    async fn handle(_s: &mut ExtState, msg: SetTenant, ctx: &GrainContext) {
        ctx.set_extension(Tenant(msg.0));
    }
}

#[async_trait]
impl GrainHandler<GetTenant> for ExtGrain {
    async fn handle(_s: &mut ExtState, _m: GetTenant, ctx: &GrainContext) -> Option<String> {
        ctx.get_extension::<Tenant>().map(|t| t.0.clone())
    }
}

#[tokio::test]
async fn ctx_set_get_extension_persists_across_messages() {
    let silo = Silo::new();
    let grain = silo.get_ref::<ExtGrain>("tenant-doc");

    // Not set yet.
    assert_eq!(grain.ask(GetTenant).await.unwrap(), None);

    // One message attaches the extension; a later message on the same
    // activation still sees it (per-activation state).
    grain.ask(SetTenant("acme".into())).await.unwrap();
    assert_eq!(grain.ask(GetTenant).await.unwrap(), Some("acme".to_string()));
}
