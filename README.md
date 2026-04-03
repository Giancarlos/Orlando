# Orlando

A virtual actor framework in Rust, inspired by [Microsoft Orleans](https://github.com/dotnet/orleans).

## What is a virtual actor?

Traditional actors (Erlang, Akka) require you to manually create, manage, and destroy actor instances. Virtual actors flip this: **every actor conceptually always exists**. You never create or destroy one — you just talk to it by identity, and the runtime handles the rest.

- **Automatic lifecycle** — A grain (virtual actor) is activated the first time someone sends it a message. After sitting idle, the runtime deactivates it to free resources. If someone talks to it again later, it reactivates transparently.
- **Single-threaded by design** — Each grain processes exactly one message at a time. No mutexes, no data races, no locks. Your handler is a plain `async fn` that owns its state exclusively.
- **Location transparency** — Callers address grains by type + key (e.g. `Counter/"room-42"`), not by address. In a cluster, the runtime routes messages to the correct silo automatically.

This model was pioneered by [Microsoft Orleans](https://www.microsoft.com/en-us/research/project/orleans-virtual-actors/) for building distributed systems like Halo's backend services. Orlando brings the same programming model to Rust.

## Features

- **Single-silo and clustered** — Start with a single `Silo` for local development, scale to a `ClusterSilo` with gRPC transport and consistent-hashing placement.
- **Persistent state** — Grain state can be automatically saved on deactivation and restored on activation via pluggable backends (in-memory, file, SQLite).
- **Timers and reminders** — Volatile timers fire while a grain is active. Durable reminders survive restarts via a persistent store.
- **Proc macros** — `#[grain]`, `#[message]`, and `#[grain_handler]` eliminate boilerplate.
- **Clustering** — Gossip-based membership, SWIM-style failure detection, and automatic grain rebalancing on node join/leave.

## Core Concepts

| Concept | What it is |
|---|---|
| **Grain** | A virtual actor. Defines a `State` type and optional lifecycle hooks. |
| **Message** | A request sent to a grain. Declares the reply type it expects back. |
| **GrainHandler** | Implement once per message type your grain handles. One grain can handle many message types. |
| **GrainRef** | A cheap, cloneable handle to a grain. Call `.ask(msg).await` to send a message and get a reply. |
| **GrainContext** | Passed to handlers. Provides the grain's identity and `get_ref()` for calling other grains. |
| **Silo** | The runtime host. Owns the grain directory and activates grains on demand. |

## Crate Layout

| Crate | Purpose |
|---|---|
| `orlando-core` | Traits, types, and the mailbox loop |
| `orlando-runtime` | Silo, grain directory, activation management |
| `orlando-macros` | `#[grain]`, `#[message]`, `#[grain_handler]` proc macros |
| `orlando-persistence` | Persistent grain state with pluggable backends (in-memory, file, SQLite) |
| `orlando-timers` | Volatile timers and durable reminders |
| `orlando-cluster` | Multi-silo clustering with gRPC, consistent hashing, gossip, and failure detection |

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

Grain state can be automatically saved to a backend and restored on reactivation:

```rust
use orlando_persistence::{PersistentGrain, PersistentSilo, SqliteStateStore};

let store = SqliteStateStore::new("sqlite:orlando.db").await?;
let silo = PersistentSilo::builder().store(store).build();

let counter = silo.persistent_get_ref::<PersistentCounter>("demo");
counter.ask(Increment { amount: 10 }).await?;

// After idle deactivation, state is saved to SQLite.
// On next access, state is restored automatically.
```

Available backends: `InMemoryStateStore`, `FileStateStore`, `SqliteStateStore`.

## Clustering

Multiple silos form a cluster with automatic grain placement via consistent hashing:

```rust
use orlando_cluster::ClusterSilo;

let silo = ClusterSilo::builder()
    .host("127.0.0.1")
    .port(9001)
    .silo_id("silo-a")
    .register::<Counter, Increment>()
    .register::<Counter, GetCount>()
    .build();

tokio::spawn(async move { silo.serve().await.unwrap() });

// Join an existing cluster
silo.join_cluster("127.0.0.1:9000").await?;

// Grain calls are transparently routed to the owning silo
let counter = silo.get_ref::<Counter>("my-counter");
counter.ask(Increment { amount: 1 }).await?;
```

Messages must implement `NetworkMessage` (or use `#[message(result = T, network)]`) and derive `Serialize`/`Deserialize`.

## Examples

```bash
cargo run -p orlando-runtime --example counter              # basic grain
cargo run -p orlando-runtime --example chat_room             # grain-to-grain calls
cargo run -p orlando-persistence --example persistent_counter # SQLite persistence
cargo run -p orlando-timers --example reminders              # durable reminders
cargo run -p orlando-cluster --example cluster               # two-silo cluster
```

## License

MIT
