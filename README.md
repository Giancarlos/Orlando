# Orlando

A virtual actor framework in Rust, inspired by [Microsoft Orleans](https://github.com/dotnet/orleans).

## What is a virtual actor?

Traditional actors (Erlang, Akka) require you to manually create, manage, and destroy actor instances. Virtual actors flip this: **every actor conceptually always exists**. You never create or destroy one — you just talk to it by identity, and the runtime handles the rest.

- **Automatic lifecycle** — A grain (another term for a virtual actor) is activated the first time someone sends it a message. After sitting idle, the runtime deactivates it to free resources. If someone talks to it again later, it reactivates transparently.
- **Single-threaded by design** — Each grain processes exactly one message at a time. No mutexes, no data races, no locks. Your handler is a plain `async fn` that owns its state exclusively.
- **Location transparency** — Callers address grains by type + key (e.g. `CounterGrain/"room-42"`), not by address. The caller doesn't know or care whether the grain is local or remote.

This model was pioneered by [Microsoft Orleans](https://www.microsoft.com/en-us/research/project/orleans-virtual-actors/) for building distributed systems like Halo's backend services. Orlando brings the same programming model to Rust.

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
| `orlando-persistence` | *(planned)* Persistent grain state with pluggable backends |
| `orlando-timers` | *(planned)* Volatile timers and durable reminders |
| `orlando-macros` | *(planned)* Derive macros to reduce boilerplate |

## Example

```rust
use async_trait::async_trait;
use orlando_core::{Grain, GrainContext, GrainHandler, Message};
use orlando_runtime::Silo;

#[derive(Default)]
struct CounterState { count: i64 }

struct CounterGrain;

#[async_trait]
impl Grain for CounterGrain {
    type State = CounterState;
}

struct Increment { amount: i64 }
impl Message for Increment { type Result = (); }

struct GetCount;
impl Message for GetCount { type Result = i64; }

#[async_trait]
impl GrainHandler<Increment> for CounterGrain {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> () {
        state.count += msg.amount;
    }
}

#[async_trait]
impl GrainHandler<GetCount> for CounterGrain {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

#[tokio::main]
async fn main() {
    let silo = Silo::new();
    let counter = silo.get_ref::<CounterGrain>("my-counter");

    counter.ask(Increment { amount: 5 }).await.unwrap();
    let count = counter.ask(GetCount).await.unwrap();
    assert_eq!(count, 5);
}
```

## License

MIT
