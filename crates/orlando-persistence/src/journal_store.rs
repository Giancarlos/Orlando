use async_trait::async_trait;
use orlando_core::GrainId;

use crate::store::PersistenceError;

/// A stored event in the journal.
#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub sequence: u64,
    pub event_bytes: Vec<u8>,
}

/// Pluggable backend for event journal persistence.
#[async_trait]
pub trait JournalStore: Send + Sync + 'static {
    /// Append events to the journal for a grain. Returns the new highest sequence number.
    async fn append(
        &self,
        grain_id: &GrainId,
        events: &[Vec<u8>],
    ) -> Result<u64, PersistenceError>;

    /// Load all events for a grain, ordered by sequence number.
    async fn load_events(
        &self,
        grain_id: &GrainId,
    ) -> Result<Vec<JournalEntry>, PersistenceError>;

    /// Load events after a given sequence number (for incremental replay after snapshot).
    async fn load_events_after(
        &self,
        grain_id: &GrainId,
        after_sequence: u64,
    ) -> Result<Vec<JournalEntry>, PersistenceError>;

    /// Save a state snapshot at a given sequence number.
    async fn save_snapshot(
        &self,
        grain_id: &GrainId,
        sequence: u64,
        state_bytes: &[u8],
    ) -> Result<(), PersistenceError>;

    /// Load the latest snapshot (if any). Returns `(sequence, state_bytes)`.
    async fn load_snapshot(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(u64, Vec<u8>)>, PersistenceError>;
}
