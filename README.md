# Orlando

A virtual actor framework in Rust, inspired by [Microsoft Orleans](https://github.com/dotnet/orleans).

## What is a virtual actor?

Traditional actors (Erlang, Akka) require you to manually create, manage, and destroy actor instances. Virtual actors flip this: **every actor conceptually always exists**. You never create or destroy one — you just talk to it by identity, and the runtime handles the rest.

- **Automatic lifecycle** — A grain (virtual actor) is activated the first time someone sends it a message. After sitting idle, the runtime deactivates it to free resources. If someone talks to it again later, it reactivates transparently.
- **Single-threaded by design** — Each grain processes exactly one message at a time. No mutexes, no data races, no locks. Your handler is a plain `async fn` that owns its state exclusively.
- **Location transparency** — Callers address grains by type + key (e.g. `Counter/"room-42"`), not by address. In a cluster, the runtime routes messages to the correct silo automatically.

This model was pioneered by [Microsoft Orleans](https://www.microsoft.com/en-us/research/project/orleans-virtual-actors/) for building distributed systems like Halo's backend services. Orlando brings the same programming model to Rust.

## Features

### Core Runtime
- **Turn-based execution** — Each grain processes one message at a time via a mailbox loop. Handlers are `async fn` with exclusive `&mut State`.
- **Reentrant grains** — Opt-in concurrent message dispatch via `#[grain(reentrant)]`. Multiple handlers run concurrently, state access serialized by async mutex.
- **Stateless workers** — Pool of identical grain instances for compute-heavy workloads. Round-robin dispatch via `#[grain(stateless_worker)]`.
- **Typed grain references** — `GrainRef<G>` is a cheap, cloneable handle. `.ask(msg).await` sends a message and returns the reply.
- **Grain call filters** — Cross-cutting interceptors (logging, metrics, auth) on every `ask()` call via `GrainCallFilter` trait.
- **Request context propagation** — Key-value context (trace IDs, tenant IDs) flows automatically through grain-to-grain call chains, including cross-silo.
- **Backpressure** — `try_ask()` fails immediately if the mailbox is full. `mailbox_pressure()` reports utilization (0.0–1.0). `max_activations` caps total grains per silo.
- **Deadlock detection** — Circular grain call chains (A calls B calls A) are detected and return `GrainError::DeadlockDetected` instead of hanging.
- **Cancellation tokens** — Handlers can check `ctx.is_cancelled()` for cooperative shutdown during drain/rebalance.
- **Silo lifecycle hooks** — `on_startup` / `on_shutdown` callbacks on `SiloBuilder`.
- **Proc macros** — `#[grain]`, `#[message]`, `#[grain_handler]` eliminate boilerplate.

### Persistence
- **Automatic state persistence** — Grain state is loaded on activation and saved on deactivation via pluggable backends.
- **Configurable persistence strategy** — `WriteOnDeactivate` (default), `WriteThrough` (save after every message), `WriteBack(Duration)` (periodic save).
- **Transactional grains** — Automatic rollback on handler failure via `TransactionalGrainRef`.
- **State versioning / migration** — `VersionedGrain` with migration chains (`v0 -> v1 -> v2`) for schema evolution.
- **Event sourcing** — `JournaledGrain` appends events to a journal, replays on activation. Automatic snapshots.
- **Optimistic concurrency** — ETags on persisted state detect concurrent writes (`EtagMismatch` error).
- **Backends** — `InMemoryStateStore`, `FileStateStore`, `SqliteStateStore`.

### Timers and Reminders
- **Volatile timers** — Periodic messages into a grain's mailbox. Cancelled on deactivation or handle drop.
- **Durable reminders** — Persisted schedules that survive restarts. `InMemoryReminderStore` and `SqliteReminderStore`.

### Clustering
- **gRPC transport** — Silo-to-silo communication via tonic. Dual encoding: bincode (internal) + protobuf (external clients).
- **Consistent hashing** — Deterministic grain placement via FNV-1a hash ring with configurable virtual nodes.
- **SWIM failure detection** — Suspicion protocol with direct pings, indirect pings, configurable timeouts, and gossip piggybacking.
- **Automatic rebalancing** — Grains migrate gracefully on node join/leave (on_deactivate runs, state persists).
- **Distributed grain directory** — Cluster-wide activation lookup prevents duplicate activations during ring transitions.
- **Gateway forwarding** — Any silo can accept a grain call and route it to the correct owner transparently.
- **Placement strategies** — `HashBasedPlacement` (default), `PreferLocalPlacement`, `RandomPlacement`. Per-grain hints via `#[grain(placement = "prefer_local")]`.
- **Message versioning** — Versioned message types for safe rolling deploys across silos.
- **Retry policy** — Configurable exponential backoff on transient remote call failures. Application errors never retried.
- **TLS and authentication** — `ServerTlsConfig` / `ClientTlsConfig` for encrypted transport. Pluggable `ClusterAuth` trait with `SharedSecretAuth` included.
- **Service discovery** — `MembershipProvider` trait with `StaticSeedProvider` and `DnsMembershipProvider` (Kubernetes headless services).

### Observability
- **Metrics** — `MetricsFilter` records `calls_total`, `call_duration_seconds`, `errors_total` per grain type. `activations_active` gauge. Uses the `metrics` crate (backend-agnostic — wire in Prometheus, Datadog, etc.).
- **Structured tracing** — Every activation, deactivation, message dispatch, and failure logged via `tracing`.

### External Clients
- **Client SDK** — `orlando-client` crate for non-silo processes. Connects to any silo, discovers the cluster, routes via local hash ring. Typed (bincode) and untyped (protobuf) message support. Automatic retry with membership refresh on stale ring.

## Crate Layout

| Crate | Purpose |
|---|---|
| `orlando-core` | Grain/Message/GrainHandler traits, mailbox loop, filters, observers, streams, request context, cancellation |
| `orlando-runtime` | Silo, grain directory, activation management, metrics filter, lifecycle hooks |
| `orlando-macros` | `#[grain]`, `#[message]`, `#[grain_handler]` proc macros |
| `orlando-persistence` | Persistent/transactional/versioned/journaled grains, state stores, ETags |
| `orlando-timers` | Volatile timers and durable reminders |
| `orlando-cluster` | Multi-silo clustering, gRPC transport, SWIM, placement, TLS, auth, retry, discovery |
| `orlando-client` | External client SDK for non-silo processes |

## Quick Start

```rust
use orlando_core::GrainContext;
use orlando_macros::{grain, grain_handler, message};
use orlando_runtime::Silo;

#[derive(Default)]
struct CounterState { count: i64 }

#[grain(state = CounterState)]
struct Counter;

#[message(result = i64)]
struct Increment { amount: i64 }

#[message(result = i64)]
struct GetCount;

#[grain_handler(Counter)]
async fn handle_increment(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
    state.count += msg.amount;
    state.count
}

#[grain_handler(Counter)]
async fn handle_get(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
    state.count
}

#[tokio::main]
async fn main() {
    let silo = Silo::new();
    let counter = silo.get_ref::<Counter>("my-counter");

    counter.ask(Increment { amount: 5 }).await.unwrap();
    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 5);
}
```

## Persistence

```rust
use orlando_persistence::{PersistentSilo, PersistenceStrategy, SqliteStateStore};

let store = SqliteStateStore::new("sqlite:orlando.db").await?;
let silo = PersistentSilo::builder().store(store).build();

// Write-on-deactivate (default)
let counter = silo.persistent_get_ref::<PersistentCounter>("demo");

// Write-through (save after every message)
let counter = silo.persistent_get_ref_with_strategy::<PersistentCounter>(
    "demo",
    PersistenceStrategy::WriteThrough,
);
```

Available backends: `InMemoryStateStore`, `FileStateStore`, `SqliteStateStore`.

## Clustering

```rust
use orlando_cluster::{ClusterSilo, SharedSecretAuth, RetryPolicy};

let silo = ClusterSilo::builder()
    .host("127.0.0.1")
    .port(9001)
    .silo_id("silo-a")
    .register::<Counter, Increment>()
    .register::<Counter, GetCount>()
    .auth(Arc::new(SharedSecretAuth::new("my-cluster-secret")))
    .auth_token("my-cluster-secret")
    .retry_policy(RetryPolicy::with_retries(3))
    .build();

tokio::spawn(async move { silo.serve().await.unwrap() });
silo.join_cluster("127.0.0.1:9000").await?;

// Calls are transparently routed to the owning silo
let counter = silo.get_ref::<Counter>("my-counter");
counter.ask(Increment { amount: 1 }).await?;
```

## External Clients

```rust
use orlando_client::OrlandoClient;

let client = OrlandoClient::connect("127.0.0.1:9001").await?;
let counter = client.grain("Counter", "my-counter");

// Typed (Rust clients sharing message types)
let result: i64 = counter.ask(Increment { amount: 5 }).await?;

// Untyped (any language via protobuf)
let response_bytes = counter.ask_proto("Increment", payload_bytes).await?;
```

## Examples

```bash
cargo run -p orlando-runtime --example counter              # basic grain
cargo run -p orlando-runtime --example chat_room             # grain-to-grain calls
cargo run -p orlando-persistence --example persistent_counter # SQLite persistence
cargo run -p orlando-timers --example reminders              # durable reminders
cargo run -p orlando-cluster --example cluster               # two-silo cluster
```

## Orleans Feature Comparison

| Feature | Orleans | Orlando | Notes |
|---------|---------|---------|-------|
| Virtual actor model | Yes | Yes | Grains with identity-based addressing |
| Turn-based execution | Yes | Yes | Single-threaded mailbox loop |
| Reentrancy | At await points | Concurrent dispatch | Different model, same goal |
| Stateless workers | Yes | Yes | Pooled activations, round-robin |
| Persistent state | Yes | Yes | Pluggable backends, write-through/write-back |
| Transactional state | Yes | Single-grain | No distributed transactions |
| State versioning | Yes | Yes | Migration chains |
| Event sourcing | JournaledGrain | JournaledGrain | Append events, replay, snapshots |
| Timers | Yes | Yes | Volatile timers |
| Reminders | Yes | Yes | Durable, persist to SQLite |
| Observers / pub-sub | Yes | Yes | ObserverSet, fire-and-forget |
| Streaming | Yes | Yes | StreamProducer/StreamItem |
| Clustering | Yes | Yes | gRPC, consistent hashing, SWIM |
| Grain directory | Distributed | Cluster-wide lookup | Prevents duplicate activations |
| Failure detection | Yes | Yes | SWIM with suspicion, indirect pings |
| Placement strategies | Yes | Yes | Hash, prefer-local, random, per-grain hints |
| Gateway/forwarding | Yes | Yes | Any silo routes to owner |
| Call filters | Yes | Yes | Before/after interceptors |
| Request context | Yes | Yes | Cross-silo propagation |
| Deadlock detection | Yes | Yes | Call chain tracking |
| Metrics | Dashboard | metrics crate | No built-in dashboard |
| TLS | Yes | Yes | mTLS, server/client certs |
| Authentication | Yes | Yes | Pluggable trait, shared-secret included |
| Service discovery | Azure, K8s, Consul | DNS, static seeds | No cloud-specific providers yet |
| Retry policies | Yes | Yes | Exponential backoff, transient-only |
| Client SDK | Orleans.Client | orlando-client | Typed + protobuf |
| Multi-cluster | Yes | No | Single cluster only |
| Grain extensions | Yes | No | |
| Distributed transactions | Yes | No | Single-grain only |
| Dashboard UI | Yes | No | Use Prometheus + Grafana |

## Testing

```bash
cargo test --workspace                          # 101 tests
cargo clippy --workspace -- -D warnings         # lint check
```

## License

MIT
