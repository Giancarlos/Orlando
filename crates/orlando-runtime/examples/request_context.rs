//! Request-context propagation example.
//!
//! A `RequestContext` (trace IDs, tenant IDs, …) set at the top of a call flows
//! automatically through grain-to-grain calls — handlers never thread it
//! manually. Set it for a call with `RequestContext::scope(...)`, read it in a
//! handler via `ctx.request_context()`.
//!
//! Run with: `cargo run -p orlando-runtime --example request_context`

use orlando_core::{GrainContext, RequestContext};
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

// ── Audit grain: a downstream grain that reads the propagated trace id ──

#[derive(Default)]
struct AuditState;

#[grain(state = AuditState)]
struct AuditGrain;

#[message(result = Option<String>)]
struct RecordAccess;

#[grain_handler(AuditGrain)]
async fn handle_record(_state: &mut AuditState, _msg: RecordAccess, ctx: &GrainContext) -> Option<String> {
    // No trace id was threaded into this call explicitly — it propagated.
    ctx.request_context().get("trace-id").map(str::to_string)
}

// ── Front grain: calls the audit grain mid-handler ──────────────

#[derive(Default)]
struct FrontState;

#[grain(state = FrontState)]
struct FrontGrain;

#[message(result = Option<String>)]
struct PlaceOrder;

#[grain_handler(FrontGrain)]
async fn handle_order(_state: &mut FrontState, _msg: PlaceOrder, ctx: &GrainContext) -> Option<String> {
    let seen_here = ctx.request_context().get("trace-id").map(str::to_string);
    println!("  FrontGrain sees trace-id: {seen_here:?}");

    // Grain-to-grain call — the context flows automatically.
    let audit = ctx.get_ref::<AuditGrain>("audit");
    audit.ask(RecordAccess).await.unwrap()
}

#[tokio::main]
async fn main() {
    let silo = Silo::new();
    let front = silo.get_ref::<FrontGrain>("front");

    // Set the request context once, at the edge.
    let ctx = RequestContext::new().with("trace-id", "trace-xyz");
    let audit_saw = ctx.scope(async { front.ask(PlaceOrder).await.unwrap() }).await;

    println!("  AuditGrain (downstream) saw trace-id: {audit_saw:?}");
    assert_eq!(audit_saw.as_deref(), Some("trace-xyz"));
    println!("trace-id propagated front -> audit with no manual threading ✓");

    // Without a scope, no context propagates.
    let none_saw = front.ask(PlaceOrder).await.unwrap();
    println!("without scope, downstream saw: {none_saw:?}");
}
