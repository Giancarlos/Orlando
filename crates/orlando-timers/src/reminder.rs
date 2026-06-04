use std::time::{Duration, SystemTime};

use orlando_core::{GrainId, Message};

/// Message delivered to a grain when a reminder fires.
/// The grain must implement `GrainHandler<ReminderTick>` to receive these.
#[derive(Debug)]
pub struct ReminderTick {
    /// Name of the reminder that fired.
    pub name: String,
}

impl Message for ReminderTick {
    type Result = ();
}

/// A persisted reminder registration.
#[derive(Debug, Clone)]
pub struct ReminderRegistration {
    /// The grain the reminder fires on.
    pub grain_id: GrainId,
    /// Reminder name, unique per grain.
    pub name: String,
    /// Interval between fires.
    pub period: Duration,
    /// Next scheduled fire time.
    pub due_at: SystemTime,
}
