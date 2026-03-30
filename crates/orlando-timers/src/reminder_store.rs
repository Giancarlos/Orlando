use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use thiserror::Error;

use orlando_core::GrainId;

use crate::reminder::ReminderRegistration;

#[derive(Debug, Error)]
pub enum ReminderError {
    #[error("reminder store error: {0}")]
    Store(String),
}

/// Pluggable backend for persisting reminder registrations.
#[async_trait]
pub trait ReminderStore: Send + Sync + 'static {
    async fn save(&self, reg: &ReminderRegistration) -> Result<(), ReminderError>;
    async fn delete(&self, grain_id: &GrainId, name: &str) -> Result<(), ReminderError>;
    async fn load_due(&self, now: SystemTime) -> Result<Vec<ReminderRegistration>, ReminderError>;
    async fn update_due_at(
        &self,
        grain_id: &GrainId,
        name: &str,
        due_at: SystemTime,
    ) -> Result<(), ReminderError>;
}

/// In-memory reminder store for testing. Data is lost on drop.
#[derive(Debug)]
pub struct InMemoryReminderStore {
    entries: Mutex<HashMap<(GrainId, String), ReminderRegistration>>,
}

impl Default for InMemoryReminderStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryReminderStore {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ReminderStore for InMemoryReminderStore {
    async fn save(&self, reg: &ReminderRegistration) -> Result<(), ReminderError> {
        let key = (reg.grain_id.clone(), reg.name.clone());
        self.entries.lock().unwrap().insert(key, reg.clone());
        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId, name: &str) -> Result<(), ReminderError> {
        let key = (grain_id.clone(), name.to_string());
        self.entries.lock().unwrap().remove(&key);
        Ok(())
    }

    async fn load_due(&self, now: SystemTime) -> Result<Vec<ReminderRegistration>, ReminderError> {
        let entries = self.entries.lock().unwrap();
        let due = entries
            .values()
            .filter(|reg| reg.due_at <= now)
            .cloned()
            .collect();
        Ok(due)
    }

    async fn update_due_at(
        &self,
        grain_id: &GrainId,
        name: &str,
        due_at: SystemTime,
    ) -> Result<(), ReminderError> {
        let key = (grain_id.clone(), name.to_string());
        if let Some(entry) = self.entries.lock().unwrap().get_mut(&key) {
            entry.due_at = due_at;
        }
        Ok(())
    }
}
