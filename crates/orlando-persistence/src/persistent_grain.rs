use orlando_core::Grain;
use serde::{Serialize, de::DeserializeOwned};

/// Marker trait for grains whose state is automatically persisted.
/// State is loaded from the store on activation and saved on deactivation.
///
/// To make a grain persistent, implement both `Grain` and `PersistentGrain`,
/// and ensure your `State` type derives `Serialize` and `Deserialize`.
pub trait PersistentGrain: Grain
where
    Self::State: Serialize + DeserializeOwned,
{
}
