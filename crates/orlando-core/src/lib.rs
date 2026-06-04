#![warn(missing_docs)]
//! `orlando-core` — the core virtual-actor primitives for Orlando.
//!
//! Defines the [`Grain`]/[`GrainHandler`]/[`Message`] traits, the grain mailbox
//! loop and its lifecycle [`ActivationState`] FSM, cheap cloneable [`GrainRef`]
//! handles, grain identity ([`GrainId`]), the [`GrainContext`] passed to
//! handlers, and supporting types (filters, observers, streams, request
//! context). The runtime and clustering crates build on these.

/// Default capacity for grain mailbox channels.
pub const MAILBOX_CAPACITY: usize = 256;

mod activation_state;
mod cluster_id;
mod envelope;
mod error;
mod extensions;
mod filter;
mod grain;
mod observer;
mod grain_context;
mod grain_id;
mod grain_ref;
/// The single-message-at-a-time grain mailbox loop, driven by the activation FSM.
pub mod mailbox;
mod message;
/// Mailbox loop for `#[grain(reentrant)]` grains: concurrent dispatch with
/// state access serialized by an async mutex.
pub mod reentrant_mailbox;
pub mod replication;
mod replicated_grain;
mod request_context;
mod stream;
/// Test helpers (e.g. `FakeActivator`) for unit-testing grains without a silo.
pub mod testing;
mod worker_ref;

pub use activation_state::{ActivationEvent, ActivationState};
pub use cluster_id::ClusterId;
pub use envelope::{Envelope, HandleFn, build_ask_envelope, recv_ask_response};
pub use error::GrainError;
pub use extensions::Extensions;
pub use filter::{FilterChain, GrainCallFilter, GrainCallInfo};
pub use grain::{Grain, GrainHandler, StatelessWorker};
pub use grain_context::{ActivationFactory, CancellationToken, GrainActivator, GrainContext, PoolFactory};
pub use grain_id::GrainId;
pub use grain_ref::GrainRef;
pub use message::{Message, ReadOnlyMessage};
pub use observer::{ObserverSet, SubscriptionId};
pub use replicated_grain::ReplicatedGrain;
pub use replication::{ReplicationEntry, ReplicationEntryType, ReplicationMode};
pub use request_context::RequestContext;
pub use stream::{StreamItem, StreamProducer};
pub use worker_ref::WorkerGrainRef;
