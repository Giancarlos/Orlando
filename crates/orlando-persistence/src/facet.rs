use std::collections::HashMap;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};

use orlando_core::GrainId;

use crate::serializer::SerializerFormat;
use crate::store::{PersistenceError, StateStore};

/// Describes a single state facet for a grain — a named, independently-persisted
/// state object backed by a specific store and serializer.
#[derive(Debug, Clone)]
pub struct FacetDescriptor {
    /// Name of this facet (e.g., "profile", "preferences").
    /// Used as a key suffix when persisting: `{grain_key}/__facet/{name}`.
    pub name: String,
    /// Named store to persist this facet to (e.g., "postgres", "redis").
    pub storage: String,
}

/// A resolved facet entry pairing a descriptor with its store and serializer.
#[derive(Clone)]
pub(crate) struct ResolvedFacet {
    pub name: String,
    pub store: Arc<dyn StateStore>,
    pub serializer: SerializerFormat,
}

impl std::fmt::Debug for ResolvedFacet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedFacet")
            .field("name", &self.name)
            .field("serializer", &self.serializer)
            .finish()
    }
}

/// Context for accessing named state facets within a grain handler.
///
/// Each facet is an independently-persisted state object that can live on a
/// different store and use a different serializer than the grain's primary state.
///
/// Facets are auto-loaded on grain activation and auto-saved on deactivation.
/// Use `load()` / `save()` / `clear()` for explicit mid-handler operations.
///
/// ```ignore
/// // In a handler:
/// let profile: Profile = facets.load("profile").await?.unwrap_or_default();
/// profile.name = "Alice".to_string();
/// facets.save("profile", &profile).await?;
/// ```
#[derive(Clone)]
pub struct FacetContext {
    grain_id: GrainId,
    facets: Arc<HashMap<String, ResolvedFacet>>,
}

impl FacetContext {
    pub(crate) fn new(
        grain_id: GrainId,
        facets: HashMap<String, ResolvedFacet>,
    ) -> Self {
        Self {
            grain_id,
            facets: Arc::new(facets),
        }
    }

    /// Derive the storage key for a facet.
    fn facet_grain_id(&self, name: &str) -> GrainId {
        GrainId {
            type_name: self.grain_id.type_name,
            key: format!("{}/__facet/{}", self.grain_id.key, name),
        }
    }

    fn resolve(&self, name: &str) -> Result<&ResolvedFacet, PersistenceError> {
        self.facets.get(name).ok_or_else(|| {
            PersistenceError::Deserialization(format!("unknown facet: {name}"))
        })
    }

    /// Load a facet's state from its store.
    ///
    /// Returns `Ok(None)` if no state is persisted for this facet.
    pub async fn load<S: DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<S>, PersistenceError> {
        let facet = self.resolve(name)?;
        let fid = self.facet_grain_id(name);
        let Some(bytes) = facet.store.load(&fid).await? else {
            return Ok(None);
        };
        let state = facet.serializer.deserialize(&bytes)?;
        tracing::debug!(grain_id = %self.grain_id, facet = name, "facet loaded");
        Ok(Some(state))
    }

    /// Save a facet's state to its store.
    pub async fn save<S: Serialize>(
        &self,
        name: &str,
        state: &S,
    ) -> Result<(), PersistenceError> {
        let facet = self.resolve(name)?;
        let fid = self.facet_grain_id(name);
        let bytes = facet.serializer.serialize(state)?;
        facet.store.save(&fid, &bytes).await?;
        tracing::debug!(grain_id = %self.grain_id, facet = name, "facet saved");
        Ok(())
    }

    /// Delete a facet's persisted state.
    pub async fn clear(&self, name: &str) -> Result<(), PersistenceError> {
        let facet = self.resolve(name)?;
        let fid = self.facet_grain_id(name);
        facet.store.delete(&fid).await?;
        tracing::debug!(grain_id = %self.grain_id, facet = name, "facet cleared");
        Ok(())
    }

    /// List the names of all registered facets.
    pub fn facet_names(&self) -> Vec<&str> {
        self.facets.keys().map(|s| s.as_str()).collect()
    }
}

impl std::fmt::Debug for FacetContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FacetContext")
            .field("grain_id", &self.grain_id)
            .field("facets", &self.facets.keys().collect::<Vec<_>>())
            .finish()
    }
}
