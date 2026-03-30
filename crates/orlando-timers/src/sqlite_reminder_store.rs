use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sqlx::SqlitePool;

use orlando_core::GrainId;

use crate::reminder::ReminderRegistration;
use crate::reminder_store::{ReminderError, ReminderStore};

/// SQLite-backed reminder store for durable reminders that survive restarts.
///
/// Stores reminders in a single table keyed by (type_name, grain_key, name).
/// Times are stored as milliseconds since the UNIX epoch.
#[derive(Debug, Clone)]
pub struct SqliteReminderStore {
    pool: SqlitePool,
}

impl SqliteReminderStore {
    /// Create a new store and ensure the schema exists.
    /// `url` is a SQLite connection string, e.g. `"sqlite://reminders.db"` or `"sqlite::memory:"`.
    pub async fn new(url: &str) -> Result<Self, ReminderError> {
        let pool = SqlitePool::connect(url)
            .await
            .map_err(|e| ReminderError::Store(e.to_string()))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS reminders (
                type_name  TEXT    NOT NULL,
                grain_key  TEXT    NOT NULL,
                name       TEXT    NOT NULL,
                period_ms  INTEGER NOT NULL,
                due_at_ms  INTEGER NOT NULL,
                PRIMARY KEY (type_name, grain_key, name)
            )",
        )
        .execute(&pool)
        .await
        .map_err(|e| ReminderError::Store(e.to_string()))?;

        Ok(Self { pool })
    }
}

fn system_time_to_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn ms_to_system_time(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms as u64)
}

/// Interns type_name strings loaded from SQLite into `&'static str`.
/// The set of grain types is bounded, so memory usage is negligible.
fn intern_type_name(name: &str) -> &'static str {
    static CACHE: LazyLock<Mutex<HashMap<String, &'static str>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    let mut cache = CACHE.lock().unwrap();
    if let Some(&cached) = cache.get(name) {
        return cached;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    cache.insert(name.to_string(), leaked);
    leaked
}

#[async_trait]
impl ReminderStore for SqliteReminderStore {
    async fn save(&self, reg: &ReminderRegistration) -> Result<(), ReminderError> {
        sqlx::query(
            "INSERT INTO reminders (type_name, grain_key, name, period_ms, due_at_ms)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (type_name, grain_key, name)
             DO UPDATE SET period_ms = excluded.period_ms, due_at_ms = excluded.due_at_ms",
        )
        .bind(reg.grain_id.type_name)
        .bind(&reg.grain_id.key)
        .bind(&reg.name)
        .bind(reg.period.as_millis() as i64)
        .bind(system_time_to_ms(reg.due_at))
        .execute(&self.pool)
        .await
        .map_err(|e| ReminderError::Store(e.to_string()))?;

        Ok(())
    }

    async fn delete(&self, grain_id: &GrainId, name: &str) -> Result<(), ReminderError> {
        sqlx::query(
            "DELETE FROM reminders WHERE type_name = ? AND grain_key = ? AND name = ?",
        )
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(|e| ReminderError::Store(e.to_string()))?;

        Ok(())
    }

    async fn load_due(
        &self,
        now: SystemTime,
    ) -> Result<Vec<ReminderRegistration>, ReminderError> {
        let now_ms = system_time_to_ms(now);

        let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
            "SELECT type_name, grain_key, name, period_ms, due_at_ms
             FROM reminders WHERE due_at_ms <= ?",
        )
        .bind(now_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ReminderError::Store(e.to_string()))?;

        let regs = rows
            .into_iter()
            .map(|(type_name, grain_key, name, period_ms, due_at_ms)| {
                ReminderRegistration {
                    grain_id: GrainId {
                        type_name: intern_type_name(&type_name),
                        key: grain_key,
                    },
                    name,
                    period: Duration::from_millis(period_ms as u64),
                    due_at: ms_to_system_time(due_at_ms),
                }
            })
            .collect();

        Ok(regs)
    }

    async fn update_due_at(
        &self,
        grain_id: &GrainId,
        name: &str,
        due_at: SystemTime,
    ) -> Result<(), ReminderError> {
        sqlx::query(
            "UPDATE reminders SET due_at_ms = ?
             WHERE type_name = ? AND grain_key = ? AND name = ?",
        )
        .bind(system_time_to_ms(due_at))
        .bind(grain_id.type_name)
        .bind(&grain_id.key)
        .bind(name)
        .execute(&self.pool)
        .await
        .map_err(|e| ReminderError::Store(e.to_string()))?;

        Ok(())
    }
}
