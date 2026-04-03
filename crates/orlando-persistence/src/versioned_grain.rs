use serde::{Serialize, de::DeserializeOwned};

use crate::persistent_grain::PersistentGrain;
use crate::store::PersistenceError;

/// Trait for grains with versioned state that supports schema migration.
///
/// When a grain's state schema changes across deployments, old persisted
/// state needs to be migrated to the new schema. `VersionedGrain` provides
/// a version number and a migration chain that runs automatically on load.
///
/// ```ignore
/// impl VersionedGrain for MyGrain {
///     fn state_version() -> u32 { 2 }
///
///     fn migrate(from_version: u32, bytes: Vec<u8>) -> Result<Vec<u8>, PersistenceError> {
///         match from_version {
///             0 => migrate_state::<OldStateV0, OldStateV1>(bytes, |old| OldStateV1 { ... }),
///             1 => migrate_state::<OldStateV1, CurrentState>(bytes, |old| CurrentState { ... }),
///             _ => Err(PersistenceError::Deserialization("unknown version".into())),
///         }
///     }
/// }
/// ```
pub trait VersionedGrain: PersistentGrain
where
    Self::State: Serialize + DeserializeOwned,
{
    /// The current (latest) version of this grain's state schema.
    /// Must be >= 1. Version 0 is reserved for unversioned legacy state.
    fn state_version() -> u32;

    /// Migrate state bytes from `from_version` to `from_version + 1`.
    /// Called repeatedly in a chain: v0 → v1 → v2 → ... → current.
    ///
    /// `from_version == 0` means the raw bytes are unversioned legacy format
    /// (plain bincode of the original struct).
    fn migrate(from_version: u32, bytes: Vec<u8>) -> Result<Vec<u8>, PersistenceError>;
}

/// Helper function for writing migration steps.
///
/// Deserializes `bytes` as `Old` (bincode), applies `transform` to produce `New`,
/// then serializes `New` back to bincode bytes.
///
/// ```ignore
/// fn migrate(from_version: u32, bytes: Vec<u8>) -> Result<Vec<u8>, PersistenceError> {
///     match from_version {
///         0 => migrate_state::<OldState, NewState>(bytes, |old| NewState {
///             name: old.name,
///             email: String::new(), // new field with default
///         }),
///         _ => Err(PersistenceError::Deserialization("unknown version".into())),
///     }
/// }
/// ```
pub fn migrate_state<Old, New>(
    bytes: Vec<u8>,
    transform: impl FnOnce(Old) -> New,
) -> Result<Vec<u8>, PersistenceError>
where
    Old: DeserializeOwned,
    New: Serialize,
{
    let (old, _) = bincode::serde::decode_from_slice::<Old, _>(&bytes, bincode::config::standard())
        .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;

    let new = transform(old);

    bincode::serde::encode_to_vec(&new, bincode::config::standard())
        .map_err(|e| PersistenceError::Serialization(e.to_string()))
}
