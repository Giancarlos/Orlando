# CLAUDE.md — orleans-rs

A from-scratch reimplementation of Microsoft Orleans (https://github.com/dotnet/orleans)
as a native Rust library. Grains are **virtual actors** — they conceptually always exist,
are activated on first call, and deactivated after idling. The defining invariant is that
**a grain processes exactly one message at a time**; there is no threading inside a grain
by design.

I want to create this myself piecemeal, with your guiding hand. Your goal is to help me to build this entire project out. Give me commands and sample code to piece together and links to documentation / documents that are relevant to the question. I may sometimes ask you to flat out write code, when I do, I want you to explain it after the fact.

---

## Core Design Principles

1. **No concurrency inside a grain.** A grain's state is owned exclusively by its mailbox
   loop. No `Mutex`, no `RwLock`, no `Arc` wrapping grain state.
2. **Grains are virtual.** Callers never create or destroy grains manually. The runtime
   activates a grain on first access and deactivates it after an idle timeout.
3. **Typed grain references.** A `GrainRef<G>` is a cheap cloneable handle to a grain's
   mailbox sender. It works the same whether the grain is local or (eventually) remote.
4. **Turn-based execution.** The mailbox loop dequeues and fully awaits one message before
   accepting the next. Handlers are plain `async fn` — no explicit locking needed.

---

## Architecture Overview

```
+------------------------------------------------------+
|                        Silo                          |
|                                                      |
|  +--------------+   +------------------------------+ |
|  | GrainFactory |-->|  GrainDirectory              | |
|  +--------------+   |  DashMap<GrainId, Activation>| |
|         |           +------------------------------+ |
|         |                        |                   |
|         v                        v                   |
|  +--------------+   +------------------------------+ |
|  | GrainRef<G>  |   |  GrainActivation<G>          | |
|  |  (proxy)     |-->|  mpsc::Sender<Envelope>      | |
|  +--------------+   |  tokio::task (mailbox loop)  | |
|                     |  state: G::State              | |
|                     +------------------------------+ |
+------------------------------------------------------+
```

---

## No-Threading Invariant — CRITICAL

Every grain activation runs a single `tokio` task. The mailbox loop shape is:

```rust
while let Some(envelope) = rx.recv().await {
    handler.handle(&mut state, envelope, &ctx).await;
    // next message only dequeued AFTER this returns
    last_active = Instant::now();
}
```

Rules that enforce this:
- Grain state is `!Sync` by construction — the compiler prevents sharing it.
- Handlers must never call `tokio::spawn` with a borrow of grain state.
- Handlers must never hold a lock across an `.await`.
- The `GrainContext` exposes a `GrainFactory` so handlers can call other grains
  (via their `GrainRef`) without touching their own state concurrently.

---

## Grain Identity

```rust
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GrainId {
    pub type_name: &'static str,   // "CounterGrain"
    pub key: String,               // "room-42", "user-7", etc.
}
```

---

## Core Traits

```rust
// The grain implementation — defines state and lifecycle hooks
#[async_trait]
pub trait Grain: Send + 'static {
    type State: Default + Send + 'static;

    async fn on_activate(_state: &mut Self::State, _ctx: &GrainContext) {}
    async fn on_deactivate(_state: &mut Self::State, _ctx: &GrainContext) {}

    fn idle_timeout() -> Duration {
        Duration::from_secs(300)
    }
}

// One impl per message type the grain handles
#[async_trait]
pub trait GrainHandler<M: Message>: Grain {
    async fn handle(state: &mut Self::State, msg: M, ctx: &GrainContext) -> M::Result;
}

// All messages declare their reply type
pub trait Message: Send + 'static {
    type Result: Send + 'static;
}
```

---

## Message Envelope (ask pattern)

```rust
pub struct Envelope {
    pub msg: Box<dyn Any + Send>,
    pub reply: oneshot::Sender<Box<dyn Any + Send>>,
}
```

Callers send a `(msg, oneshot::Sender)` pair. The mailbox loop calls the handler and
sends the result back over the oneshot. `GrainRef::ask(msg).await` wraps this
transparently so call sites look like normal async function calls.

---

## Crate Layout

```
orleans-rs/
├── crates/
│   ├── orleans-core/         # GrainId, GrainRef, Message, Grain trait, GrainContext
│   ├── orleans-runtime/      # Silo, GrainDirectory, activation loop, idle GC
│   ├── orleans-persistence/  # PersistentGrain trait + in-memory + pluggable backends
│   ├── orleans-timers/       # Timer (volatile) + Reminder (durable, persisted)
│   └── orleans-macros/       # #[grain] derive, #[grain_handler] attribute macro
├── examples/
│   ├── counter_grain/
│   └── chat_room/
└── CLAUDE.md
```

---

## Dependencies

```toml
[workspace.dependencies]

# Async runtime — grain mailbox loop, timers, oneshot reply channels
# https://crates.io/crates/tokio
tokio = { version = "1", features = ["full"] }

# Lock-free concurrent HashMap: GrainId -> GrainActivation (GrainDirectory)
# https://crates.io/crates/dashmap
dashmap = "6"

# Required until native async-in-traits lands in our MSRV
# https://crates.io/crates/async-trait
async-trait = "0.1"

# Grain and silo identity
# https://crates.io/crates/uuid
uuid = { version = "1", features = ["v4"] }

# Grain state persistence and (Phase 4) cross-silo message encoding
# https://crates.io/crates/serde
serde = { version = "1", features = ["derive"] }
# https://crates.io/crates/bincode
bincode = "2"

# Structured tracing: grain activation/deactivation events, message latency
# https://crates.io/crates/tracing
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# thiserror for library error types, anyhow for examples/binaries
# https://crates.io/crates/thiserror
thiserror = "2"
# https://crates.io/crates/anyhow
anyhow = "1"

# Phase 4 — gRPC transport for cross-silo grain calls and directory sync
# https://crates.io/crates/tonic
tonic = "0.12"
# https://crates.io/crates/prost
prost = "0.13"
```

---

## Implementation Phases

### Phase 1 — Single Silo (MVP)
- [ ] `GrainId`, `Message` trait, `Envelope` type
- [ ] `Grain` trait + `GrainHandler<M>` per message type
- [ ] `GrainRef<G>`: cloneable sender handle + `ask(msg).await` sugar
- [ ] `GrainDirectory`: `DashMap<GrainId, GrainActivation>` — activate on miss
- [ ] Mailbox loop with `tokio::time::timeout` idle detection -> deactivation
- [ ] `GrainContext` exposing `GrainFactory` for handler-to-grain calls
- [ ] `Silo::builder()` entry point
- [ ] Example: `CounterGrain` (Increment / GetCount messages)

### Phase 2 — Persistence
- [ ] `PersistentGrain` supertrait: `load_state() -> State`, `save_state(&State)`
- [ ] State restored during `on_activate`, saved during `on_deactivate`
- [ ] In-memory backend (tests), file-based backend (dev)
- [ ] SQLite backend via `sqlx`

### Phase 3 — Timers and Reminders
- [ ] Timer (volatile): `tokio::time::interval` task that injects a message into the
      grain's own mailbox; cancelled when grain deactivates
- [ ] Reminder (durable): persisted via the persistence layer; re-registered on
      activation; fires even after a silo restart

### Phase 4 — Clustering
- [ ] Silo-to-silo tonic (gRPC) transport
- [ ] Distributed `GrainDirectory` with consistent-hashing placement
- [ ] Gossip-based membership / failure detection (SWIM-style)
- [ ] Grain handoff / rebalancing on node join/leave

---

## Coding Conventions

- No `unwrap()` in library code — use `?` and typed errors with `thiserror`.
- No shared mutable state for grains — `!Sync` is enforced by the type system; lean into it.
- Use `async-trait` now; migrate to native async traits when the MSRV supports them.
- Every grain activation owns a `tokio::task::JoinHandle` — the task IS the grain.
- All public types implement `Debug`.
- Modules within each crate are flat; avoid nesting beyond two levels.
- Instrument every activation, deactivation, and message receipt with `tracing::debug!`.

---

## Testing Strategy

- Unit-test handlers in isolation: construct `State::default()`, call handler directly.
- Integration-test through `Silo`: activate -> call -> assert state -> idle -> deactivated.
- Use `#[tokio::test]` for all async tests.
- Use `tokio::time::pause()` + `tokio::time::advance()` for deterministic timer tests
  without wall-clock delays.
- A `FakeGrainContext` in `orleans-core` enables pure unit tests without a live silo.

---

## Notable Prior Art (read before implementing)

- ractor         https://crates.io/crates/ractor
  Erlang gen_server-inspired actor framework; separate state type mirrors grain model well.

- Coerce-rs      https://github.com/LeonHartley/Coerce-rs
  Closest existing Rust project to Orleans; has distributed sharding, Redis persistence,
  and Kubernetes discovery. Good reference for placement logic.

- kameo          https://github.com/tqwewe/kameo
  Fault-tolerant async actors with a persistence add-on; clean ask/tell API shape.

- edacious       https://jaysonlennon.dev/project-edacious
  Minimal Rust virtual actor experiment written specifically as an Orleans-style demo.
  Small enough to read in an afternoon.

---

## Non-Goals (v1)

- No distributed transactions — compose sagas at the application layer.
- No stream processing built-in.
- No compatibility with the .NET Orleans wire protocol.
- No WASM grain hosting.

<!-- rtk-instructions v2 -->
# RTK (Rust Token Killer) - Token-Optimized Commands

## Golden Rule


**Always prefix commands with `rtk`**. If RTK has a dedicated filter, it uses it. If not, it passes through unchanged. This means RTK is always safe to use.

**Important**: Even in command chains with `&&`, use `rtk`:
```bash
# ❌ Wrong
git add . && git commit -m "msg" && git push

# ✅ Correct
rtk git add . && rtk git commit -m "msg" && rtk git push
```

## RTK Commands by Workflow

### Build & Compile (80-90% savings)
```bash
rtk cargo build         # Cargo build output
rtk cargo check         # Cargo check output
rtk cargo clippy        # Clippy warnings grouped by file (80%)
rtk tsc                 # TypeScript errors grouped by file/code (83%)
rtk lint                # ESLint/Biome violations grouped (84%)
rtk prettier --check    # Files needing format only (70%)
rtk next build          # Next.js build with route metrics (87%)
```

### Test (90-99% savings)
```bash
rtk cargo test          # Cargo test failures only (90%)
rtk vitest run          # Vitest failures only (99.5%)
rtk playwright test     # Playwright failures only (94%)
rtk test <cmd>          # Generic test wrapper - failures only
```

### Git (59-80% savings)
```bash
rtk git status          # Compact status
rtk git log             # Compact log (works with all git flags)
rtk git diff            # Compact diff (80%)
rtk git show            # Compact show (80%)
rtk git add             # Ultra-compact confirmations (59%)
rtk git commit          # Ultra-compact confirmations (59%)
rtk git push            # Ultra-compact confirmations
rtk git pull            # Ultra-compact confirmations
rtk git branch          # Compact branch list
rtk git fetch           # Compact fetch
rtk git stash           # Compact stash
rtk git worktree        # Compact worktree
```

Note: Git passthrough works for ALL subcommands, even those not explicitly listed.

### GitHub (26-87% savings)
```bash
rtk gh pr view <num>    # Compact PR view (87%)
rtk gh pr checks        # Compact PR checks (79%)
rtk gh run list         # Compact workflow runs (82%)
rtk gh issue list       # Compact issue list (80%)
rtk gh api              # Compact API responses (26%)
```

### JavaScript/TypeScript Tooling (70-90% savings)
```bash
rtk pnpm list           # Compact dependency tree (70%)
rtk pnpm outdated       # Compact outdated packages (80%)
rtk pnpm install        # Compact install output (90%)
rtk npm run <script>    # Compact npm script output
rtk npx <cmd>           # Compact npx command output
rtk prisma              # Prisma without ASCII art (88%)
```

### Files & Search (60-75% savings)
```bash
rtk ls <path>           # Tree format, compact (65%)
rtk read <file>         # Code reading with filtering (60%)
rtk grep <pattern>      # Search grouped by file (75%)
rtk find <pattern>      # Find grouped by directory (70%)
```

### Analysis & Debug (70-90% savings)
```bash
rtk err <cmd>           # Filter errors only from any command
rtk log <file>          # Deduplicated logs with counts
rtk json <file>         # JSON structure without values
rtk deps                # Dependency overview
rtk env                 # Environment variables compact
rtk summary <cmd>       # Smart summary of command output
rtk diff                # Ultra-compact diffs
```

### Infrastructure (85% savings)
```bash
rtk docker ps           # Compact container list
rtk docker images       # Compact image list
rtk docker logs <c>     # Deduplicated logs
rtk kubectl get         # Compact resource list
rtk kubectl logs        # Deduplicated pod logs
```

### Network (65-70% savings)
```bash
rtk curl <url>          # Compact HTTP responses (70%)
rtk wget <url>          # Compact download output (65%)
```

### Meta Commands
```bash
rtk gain                # View token savings statistics
rtk gain --history      # View command history with savings
rtk discover            # Analyze Claude Code sessions for missed RTK usage
rtk proxy <cmd>         # Run command without filtering (for debugging)
rtk init                # Add RTK instructions to CLAUDE.md
rtk init --global       # Add RTK to ~/.claude/CLAUDE.md
```

## Token Savings Overview

| Category | Commands | Typical Savings |
|----------|----------|-----------------|
| Tests | vitest, playwright, cargo test | 90-99% |
| Build | next, tsc, lint, prettier | 70-87% |
| Git | status, log, diff, add, commit | 59-80% |
| GitHub | gh pr, gh run, gh issue | 26-87% |
| Package Managers | pnpm, npm, npx | 70-90% |
| Files | ls, read, grep, find | 60-75% |
| Infrastructure | docker, kubectl | 85% |
| Network | curl, wget | 65-70% |

Overall average: **60-90% token reduction** on common development operations.
<!-- /rtk-instructions -->
