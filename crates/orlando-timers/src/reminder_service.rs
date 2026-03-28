use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use orlando_core::{Envelope, GrainActivator, GrainHandler, GrainId};

use crate::reminder::{ReminderRegistration, ReminderTick};
use crate::reminder_store::{ReminderError, ReminderStore};

/// Type-erased dispatch function: given a grain_id and reminder name,
/// activates the grain if needed and sends a ReminderTick.
type DispatchFn = Arc<
    dyn Fn(GrainId, String, Arc<dyn GrainActivator>) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Background service that polls for due reminders and dispatches them.
pub struct ReminderService {
    store: Arc<dyn ReminderStore>,
    activator: Arc<dyn GrainActivator>,
    dispatchers: Mutex<HashMap<String, DispatchFn>>,
    poll_interval: Duration,
}

impl ReminderService {
    pub fn new(
        store: Arc<dyn ReminderStore>,
        activator: Arc<dyn GrainActivator>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            activator,
            dispatchers: Mutex::new(HashMap::new()),
            poll_interval: Duration::from_millis(500),
        })
    }

    pub fn with_poll_interval(
        store: Arc<dyn ReminderStore>,
        activator: Arc<dyn GrainActivator>,
        poll_interval: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            activator,
            dispatchers: Mutex::new(HashMap::new()),
            poll_interval,
        })
    }

    /// Register a grain type so the service can activate it and deliver ReminderTicks.
    /// Call this at silo startup for every grain type that uses reminders.
    pub fn register_grain_type<G>(&self)
    where
        G: GrainHandler<ReminderTick>,
    {
        let dispatch: DispatchFn = Arc::new(|grain_id, name, activator| {
            Box::pin(async move {
                let activator_for_mailbox = activator.clone();
                let sender = activator.get_or_insert(
                    grain_id.clone(),
                    Box::new(move |id| {
                        let (tx, rx) = mpsc::channel(256);
                        let task = tokio::spawn(async move {
                            orlando_core::mailbox::run_mailbox::<G>(id, rx, activator_for_mailbox)
                                .await;
                        });
                        (tx, task)
                    }),
                );

                let envelope = Envelope::new(Box::new(move |state_any, ctx| {
                    let state = state_any
                        .downcast_mut::<G::State>()
                        .expect("grain state type mismatch");
                    let tick = ReminderTick { name };
                    Box::pin(async move {
                        <G as GrainHandler<ReminderTick>>::handle(state, tick, ctx).await;
                    })
                }));

                if sender.send(envelope).await.is_err() {
                    tracing::warn!(%grain_id, "failed to deliver reminder: mailbox closed");
                }
            })
        });

        self.dispatchers
            .lock()
            .unwrap()
            .insert(std::any::type_name::<G>().to_string(), dispatch);
    }

    /// Register a reminder for a grain. Persists the schedule to the store.
    pub async fn register_reminder(
        &self,
        grain_id: &GrainId,
        name: impl Into<String>,
        period: Duration,
    ) -> Result<(), ReminderError> {
        let name = name.into();
        let reg = ReminderRegistration {
            grain_id: grain_id.clone(),
            name,
            period,
            due_at: SystemTime::now() + period,
        };
        self.store.save(&reg).await
    }

    /// Unregister a reminder.
    pub async fn unregister_reminder(
        &self,
        grain_id: &GrainId,
        name: &str,
    ) -> Result<(), ReminderError> {
        self.store.delete(grain_id, name).await
    }

    /// Start the background poll loop. Returns a JoinHandle for the spawned task.
    pub fn start(self: &Arc<Self>) -> JoinHandle<()> {
        let svc = self.clone();
        tokio::spawn(async move {
            svc.poll_loop().await;
        })
    }

    async fn poll_loop(&self) {
        let mut interval = tokio::time::interval(self.poll_interval);

        loop {
            interval.tick().await;

            let now = SystemTime::now();
            let due = match self.store.load_due(now).await {
                Ok(due) => due,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load due reminders");
                    continue;
                }
            };

            for reg in due {
                let dispatch = {
                    let dispatchers = self.dispatchers.lock().unwrap();
                    dispatchers.get(reg.grain_id.type_name).cloned()
                };

                if let Some(dispatch) = dispatch {
                    let activator = self.activator.clone();
                    let grain_id = reg.grain_id.clone();
                    let name = reg.name.clone();

                    tokio::spawn(async move {
                        dispatch(grain_id, name, activator).await;
                    });
                } else {
                    tracing::warn!(
                        grain_type = reg.grain_id.type_name,
                        "no dispatcher registered for grain type, skipping reminder"
                    );
                }

                // Advance due_at to next period
                let next_due = now + reg.period;
                if let Err(e) = self
                    .store
                    .update_due_at(&reg.grain_id, &reg.name, next_due)
                    .await
                {
                    tracing::warn!(error = %e, "failed to update reminder due_at");
                }
            }
        }
    }
}

impl std::fmt::Debug for ReminderService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReminderService")
            .field("poll_interval", &self.poll_interval)
            .finish()
    }
}
