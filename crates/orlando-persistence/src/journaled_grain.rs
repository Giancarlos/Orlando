use async_trait::async_trait;
use orlando_core::{Grain, GrainContext, Message};
use serde::{Serialize, de::DeserializeOwned};

/// A grain that uses event sourcing for state management.
///
/// Instead of mutating state directly, handlers return events. Events are
/// appended to a journal and applied to the state via `apply()`. On
/// activation, the journal is replayed to reconstruct the state.
#[async_trait]
pub trait JournaledGrain: Grain
where
    Self::State: Serialize + DeserializeOwned,
{
    /// The event type produced by handlers.
    type Event: Serialize + DeserializeOwned + Send + 'static;

    /// Apply an event to the state (pure function, no side effects).
    fn apply(state: &mut Self::State, event: &Self::Event);

    /// Number of events between automatic snapshots.
    /// Set to 0 to disable snapshots. Default: 100.
    fn snapshot_interval() -> u64 {
        100
    }
}

/// Handler trait for journaled grains. Returns events instead of mutating state.
#[async_trait]
pub trait JournaledHandler<M: Message>: JournaledGrain
where
    Self::State: Serialize + DeserializeOwned,
{
    /// Handle a message and return (result, events).
    /// Events will be persisted and applied to state after this returns.
    /// Note: state is read-only — mutations happen via the returned events.
    async fn handle(
        state: &Self::State,
        msg: M,
        ctx: &GrainContext,
    ) -> (M::Result, Vec<Self::Event>);
}
