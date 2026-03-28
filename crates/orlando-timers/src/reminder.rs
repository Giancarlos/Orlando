use std::time::{Duration, SystemTime};

use orlando_core::{GrainId, Message};

/// Message delivered to a grain when a reminder fires.
/// The grain must implement `GrainHandler<ReminderTick>` to receive these.
#[derive(Debug)]
pub struct ReminderTick {
    pub name: String,
}

impl Message for ReminderTick {
    type Result = ();
}

/// A persisted reminder registration.
#[derive(Debug, Clone)]
pub struct ReminderRegistration {
    pub grain_id: GrainId,
    pub name: String,
    pub period: Duration,
    pub due_at: SystemTime,
}
