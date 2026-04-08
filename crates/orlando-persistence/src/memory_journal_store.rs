use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use orlando_core::GrainId;

use crate::journal_store::{JournalEntry, JournalStore};
use crate::store::PersistenceError;

/// Per-grain journal data: events and optional snapshot.
struct GrainJournal {
    events: Vec<JournalEntry>,
    snapshot: Option<(u64, Vec<u8>)>,
}

/// In-memory journal store for testing.
/// Data is lost when the store is dropped.
#[derive(Debug)]
pub struct InMemoryJournalStore {
    // Debug is derived but the inner type isn't Debug-printable in detail;
    // that's fine for a test store.
    data: Mutex<HashMap<GrainId, GrainJournal>>,
}

impl std::fmt::Debug for GrainJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrainJournal")
            .field("event_count", &self.events.len())
            .field("has_snapshot", &self.snapshot.is_some())
            .finish()
    }
}

impl Default for InMemoryJournalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryJournalStore {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl JournalStore for InMemoryJournalStore {
    async fn append(
        &self,
        grain_id: &GrainId,
        events: &[Vec<u8>],
    ) -> Result<u64, PersistenceError> {
        let mut data = self.data.lock().expect("journal store lock poisoned");
        let journal = data.entry(grain_id.clone()).or_insert_with(|| GrainJournal {
            events: Vec::new(),
            snapshot: None,
        });

        let mut seq = journal
            .events
            .last()
            .map(|e| e.sequence)
            .unwrap_or(0);

        for event_bytes in events {
            seq += 1;
            journal.events.push(JournalEntry {
                sequence: seq,
                event_bytes: event_bytes.clone(),
            });
        }

        Ok(seq)
    }

    async fn load_events(
        &self,
        grain_id: &GrainId,
    ) -> Result<Vec<JournalEntry>, PersistenceError> {
        let data = self.data.lock().expect("journal store lock poisoned");
        Ok(data
            .get(grain_id)
            .map(|j| j.events.clone())
            .unwrap_or_default())
    }

    async fn load_events_after(
        &self,
        grain_id: &GrainId,
        after_sequence: u64,
    ) -> Result<Vec<JournalEntry>, PersistenceError> {
        let data = self.data.lock().expect("journal store lock poisoned");
        Ok(data
            .get(grain_id)
            .map(|j| {
                j.events
                    .iter()
                    .filter(|e| e.sequence > after_sequence)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn save_snapshot(
        &self,
        grain_id: &GrainId,
        sequence: u64,
        state_bytes: &[u8],
    ) -> Result<(), PersistenceError> {
        let mut data = self.data.lock().expect("journal store lock poisoned");
        let journal = data.entry(grain_id.clone()).or_insert_with(|| GrainJournal {
            events: Vec::new(),
            snapshot: None,
        });
        journal.snapshot = Some((sequence, state_bytes.to_vec()));
        Ok(())
    }

    async fn load_snapshot(
        &self,
        grain_id: &GrainId,
    ) -> Result<Option<(u64, Vec<u8>)>, PersistenceError> {
        let data = self.data.lock().expect("journal store lock poisoned");
        Ok(data.get(grain_id).and_then(|j| j.snapshot.clone()))
    }
}
