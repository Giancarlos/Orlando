use serde::{Serialize, de::DeserializeOwned};

use crate::store::PersistenceError;

/// Selects the serialization format for grain state.
///
/// Configure per-store via `PersistentSiloBuilder::named_store_with_serializer()`.
///
/// - `Bincode` (default) — compact binary, fast, not human-readable
/// - `Json` — human-readable, queryable in Postgres (`jsonb`), larger
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SerializerFormat {
    /// Compact binary format. Fast and small, but not human-readable.
    #[default]
    Bincode,
    /// JSON format. Human-readable, queryable with Postgres `jsonb`, larger.
    Json,
}

impl SerializerFormat {
    /// Serialize a value to bytes using this format.
    pub fn serialize<S: Serialize>(&self, state: &S) -> Result<Vec<u8>, PersistenceError> {
        match self {
            Self::Bincode => bincode::serde::encode_to_vec(state, bincode::config::standard())
                .map_err(|e| PersistenceError::Serialization(e.to_string())),
            Self::Json => serde_json::to_vec(state)
                .map_err(|e| PersistenceError::Serialization(e.to_string())),
        }
    }

    /// Deserialize bytes back into a value using this format.
    pub fn deserialize<S: DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<S, PersistenceError> {
        match self {
            Self::Bincode => {
                let (state, _) =
                    bincode::serde::decode_from_slice(bytes, bincode::config::standard())
                        .map_err(|e| PersistenceError::Deserialization(e.to_string()))?;
                Ok(state)
            }
            Self::Json => serde_json::from_slice(bytes)
                .map_err(|e| PersistenceError::Deserialization(e.to_string())),
        }
    }
}
