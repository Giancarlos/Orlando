# Orlando — Project Status

**Date:** 2026-03-30
**Branch:** `main` at `561c4ef`

---

## Implementation Phases

All four planned phases are complete.

### Phase 1 — Single Silo (MVP) ✅

| Item | Status |
|------|--------|
| `GrainId`, `Message` trait, `Envelope` type | Done |
| `Grain` trait + `GrainHandler<M>` per message type | Done |
| `GrainRef<G>`: cloneable sender handle + `ask(msg).await` | Done |
| `GrainDirectory`: `DashMap<GrainId, GrainActivation>` — activate on miss | Done |
| Mailbox loop with `tokio::time::timeout` idle detection → deactivation | Done |
| `GrainContext` exposing `GrainFactory` for handler-to-grain calls | Done |
| `Silo::builder()` entry point (`Silo::new()`) | Done |
| Example: CounterGrain | Done (`crates/orlando-runtime/examples/counter.rs`) |

### Phase 2 — Persistence ✅

| Item | Status |
|------|--------|
| `PersistentGrain` supertrait with load/save | Done |
| State restored on activate, saved on deactivate | Done |
| In-memory backend (tests) | Done (`InMemoryStateStore`) |
| File-based backend (dev) | Done (`FileStateStore`) |
| SQLite backend via `sqlx` | Done (`SqliteStateStore`) |

### Phase 3 — Timers & Reminders ✅

| Item | Status |
|------|--------|
| Volatile timer (`tokio::time::interval`, cancelled on deactivate) | Done |
| Durable reminder (persisted, fires after restart) | Done |
| In-memory reminder store | Done (`InMemoryReminderStore`) |
| SQLite reminder store | Done (`SqliteReminderStore`) |

### Phase 4 — Clustering ✅

| Item | Status |
|------|--------|
| Silo-to-silo gRPC transport (tonic) | Done |
| Consistent-hashing grain placement (`HashRing`) | Done |
| Gossip membership (`NotifyJoin`/`NotifyLeave` RPCs) | Done |
| Failure detection (ping-based, configurable) | Done |
| Grain rebalancing on membership change | Done |
| Connection pool for gRPC channels | Done |
| Graceful shutdown (`serve_with_shutdown` + watch channel) | Done |

---

## Crate Layout

```
crates/
├── orlando-core/          Core types: GrainId, GrainRef, Message, Grain, GrainContext, Envelope
├── orlando-runtime/       Silo, GrainDirectory, activation loop, idle GC
├── orlando-persistence/   PersistentGrain, StateStore (in-memory, file, SQLite)
├── orlando-timers/        Timer (volatile), Reminder (durable), ReminderService
├── orlando-macros/        #[grain], #[message], #[grain_handler] proc macros
└── orlando-cluster/       ClusterSilo, HashRing, gRPC transport, gossip, failure detector
```

## Public API Surface by Crate

### orlando-core
- `Grain` — trait defining state type and lifecycle hooks
- `GrainHandler<M>` — trait for handling a specific message type
- `Message` — trait declaring a message's reply type
- `GrainId` — identity: `(type_name: &'static str, key: String)`
- `GrainRef<G>` — cloneable handle with `.ask(msg).await`
- `GrainContext` — passed to handlers; provides identity + cross-grain calls
- `Envelope` — `(Box<dyn Any + Send>, oneshot::Sender)`
- `GrainActivator` — trait for activating grains by id
- `GrainError` — error type
- `testing` module — test utilities

### orlando-runtime
- `Silo` / `SiloBuilder` — single-node runtime entry point
- `GrainDirectory` — `DashMap<GrainId, Activation>`
- `Activation` — a single grain activation (task + sender)

### orlando-persistence
- `PersistentGrain` — marker supertrait for auto-persisted grains
- `PersistentSilo` / `PersistentSiloBuilder` — silo with integrated persistence
- `StateStore` — pluggable backend trait
- `InMemoryStateStore`, `FileStateStore`, `SqliteStateStore` — backends
- `PersistenceError` — error type

### orlando-timers
- `register_timer::<G>(ctx, name, period)` → `TimerHandle`
- `TimerTick` / `TimerHandle` — volatile timer types
- `ReminderService` — polling service for durable reminders
- `ReminderTick` / `ReminderRegistration` — reminder types
- `ReminderStore` — pluggable backend trait
- `InMemoryReminderStore`, `SqliteReminderStore` — backends
- `ReminderError` — error type

### orlando-macros
- `#[grain(state = T, idle_timeout_secs = N)]` — generates `Grain` impl
- `#[message(result = T, network)]` — generates `Message` (+ optional `NetworkMessage`) impl
- `#[grain_handler(GrainType)]` — generates `GrainHandler` impl from a bare function

### orlando-cluster
- `ClusterSilo` / `ClusterSiloBuilder` — multi-node silo with gRPC
- `ClusterGrainRef` — grain ref with local/remote dispatch
- `HashRing` — consistent hashing for grain placement
- `MembershipService` — join/gossip/member-list management
- `FailureDetector` / `FailureDetectorConfig` — ping-based liveness
- `Rebalancer` — deactivates misplaced grains after ring changes
- `ConnectionPool` — shared gRPC channel cache
- `MessageRegistry` — maps message types for network serialization
- `NetworkMessage` — trait for cluster-serializable messages
- `GrainTransportService` — gRPC service impl for remote grain calls
- `proto` module — generated protobuf types

---

## Tests

| Crate | File | Tests | Description |
|-------|------|-------|-------------|
| orlando-runtime | `tests/counter_grain.rs` | 4 | Increment, separate grains, same-key dedup, grain-to-grain calls |
| orlando-persistence | `tests/persistent_counter.rs` | 5 | In-memory state survival, separate grains, SQLite persistence (×2) |
| orlando-timers | `tests/timer_test.rs` | 3 | Timer firing, cancellation, multiple timers |
| orlando-timers | `tests/reminder_test.rs` | 4 | Reminder delivery, lazy activation, unregister, SQLite durability |
| orlando-cluster | `tests/cluster_test.rs` | 7 | Local dispatch, cross-silo, membership, failure detection, rebalancing, gossip |
| **Total** | **5 files** | **23** | 3 ignored (require multi-process setup) |

All 23 tests pass. 0 clippy warnings.

## Examples

| Crate | Example | What It Demonstrates |
|-------|---------|----------------------|
| orlando-runtime | `counter.rs` | Basic grain with proc macros |
| orlando-runtime | `chat_room.rs` | Multi-grain interaction via `GrainContext` |
| orlando-persistence | `persistent_counter.rs` | State surviving deactivation/reactivation with SQLite |
| orlando-timers | `reminders.rs` | Durable reminders with SQLite reminder store |
| orlando-cluster | `cluster.rs` | Two-silo cluster with cross-silo grain calls |

---

## Workspace Dependencies

| Dependency | Version | Purpose |
|------------|---------|---------|
| tokio | 1 (full) | Async runtime, timers, channels |
| dashmap | 6 | Lock-free concurrent grain directory |
| async-trait | 0.1 | Async fn in traits |
| uuid | 1 (v4) | Silo identity |
| serde | 1 (derive) | Serialization |
| bincode | 2 (serde) | Binary encoding for persistence + network |
| tracing | 0.1 | Structured logging |
| tracing-subscriber | 0.3 (env-filter) | Log output |
| thiserror | 2 | Typed error macros |
| anyhow | 1 | Flexible errors in examples |
| sqlx | 0.8 (runtime-tokio, sqlite) | SQLite persistence |
| tonic | 0.12 | gRPC framework |
| prost | 0.13 | Protobuf codegen |

---

## Proto Schema (`orlando.proto`)

**Services:**

- **GrainTransport** — `Invoke(InvokeRequest) → InvokeResponse`
- **Membership** — `Join`, `NotifyJoin`, `NotifyLeave`, `Ping`, `GetMembers`

**Key message types:** `SiloAddress`, `InvokeRequest/Response`, `JoinRequest/Response`, `NotifyJoinRequest/Response`, `NotifyLeaveRequest/Response`, `PingRequest/Response`, `GetMembersRequest/Response`

---

## What's Not Built (Non-Goals per CLAUDE.md)

- No distributed transactions (compose sagas at application layer)
- No stream processing
- No .NET Orleans wire compatibility
- No WASM grain hosting

## Potential Future Work

- **Full SWIM protocol** — current gossip is simplified (direct broadcast on join/leave); a proper protocol would add suspicion states, protocol periods, and indirect pings
- **Reentrancy** — Orleans supports reentrant grains that can interleave awaits; Orlando currently has strict turn-based execution only
- **Grain observers / subscriptions** — pub/sub pattern for grains to subscribe to events from other grains
- **Stateless worker grains** — grains that can run multiple activations concurrently for throughput
- **Dashboard / metrics** — expose activation counts, message latency, ring state via HTTP
- **Kubernetes discovery** — auto-discover cluster members via K8s API instead of seed addresses
- **Redis / Postgres backends** — additional persistence and reminder store implementations
- **Grain versioning / migration** — handle state schema changes across deployments
- **Streaming** — grain-to-grain async streams (Orleans has this as "Streams")
- **Transaction support** — at minimum, ACID state writes within a single grain
- **Documentation** — rustdoc on all public types, a book-style guide
