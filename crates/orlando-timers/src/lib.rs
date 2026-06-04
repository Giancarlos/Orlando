#![warn(missing_docs)]
//! `orlando-timers` — volatile timers and durable reminders for grains.
//!
//! **Timers** ([`register_timer`], [`TimerHandle`], [`TimerTick`]) fire periodic
//! messages into a grain's own mailbox and are cancelled when the grain
//! deactivates. **Reminders** ([`ReminderService`], [`ReminderStore`],
//! [`ReminderRegistration`], [`ReminderTick`]) are persisted via a
//! [`ReminderStore`] backend ([`InMemoryReminderStore`], [`SqliteReminderStore`])
//! so they survive silo restarts and re-fire on reactivation.

mod reminder;
mod reminder_service;
mod reminder_store;
mod sqlite_reminder_store;
mod timer;

pub use reminder::{ReminderRegistration, ReminderTick};
pub use reminder_service::ReminderService;
pub use reminder_store::{InMemoryReminderStore, ReminderError, ReminderStore};
pub use sqlite_reminder_store::SqliteReminderStore;
pub use timer::{TimerHandle, TimerTick, register_timer};
