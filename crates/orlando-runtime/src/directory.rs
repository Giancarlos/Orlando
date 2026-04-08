use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use orlando_core::{ActivationFactory, CancellationToken, Envelope, GrainActivator, GrainId, PoolFactory};

use crate::activation::Activation;
use crate::worker_pool::WorkerPool;

#[derive(Debug)]
pub struct GrainDirectory {
    activations: DashMap<GrainId, Activation>,
    worker_pools: DashMap<GrainId, WorkerPool>,
    draining: AtomicBool,
    /// Maximum number of grain activations allowed (0 = no limit).
    max_activations: AtomicUsize,
}

impl Default for GrainDirectory {
    fn default() -> Self {
        Self::new()
    }
}

impl GrainDirectory {
    pub fn new() -> Self {
        Self {
            activations: DashMap::new(),
            worker_pools: DashMap::new(),
            draining: AtomicBool::new(false),
            max_activations: AtomicUsize::new(0),
        }
    }

    /// Set the maximum number of grain activations on this silo.
    /// 0 means no limit (default).
    /// When the limit is reached, new grain activations return a closed channel,
    /// causing the caller's `ask()` to fail with `MailboxClosed`.
    pub fn set_max_activations(&self, limit: usize) {
        self.max_activations.store(limit, Ordering::Relaxed);
    }

    /// Get the current activation count.
    pub fn activation_count(&self) -> usize {
        self.activations.len()
    }

    pub fn remove(&self, id: &GrainId) -> Option<Activation> {
        self.activations.remove(id).map(|(_, a)| {
            metrics::gauge!("orlando.grain.activations_active",
                "grain_type" => id.type_name.to_string(),
            )
            .decrement(1.0);
            a
        })
    }

    /// Gracefully drain all active grains.
    ///
    /// Blocks new activations, then drops all directory-held sender clones so
    /// mailbox loops see a closed channel and run `on_deactivate` naturally.
    /// Waits up to 5 seconds for tasks to finish; any that remain are aborted.
    pub async fn drain(&self) {
        self.draining.store(true, Ordering::SeqCst);

        // Remove all activations and drop the directory's sender clones.
        // If no outstanding GrainRef clones exist, this closes the channel,
        // letting the mailbox exit naturally and run on_deactivate.
        let keys: Vec<GrainId> = self.activations.iter().map(|e| e.key().clone()).collect();
        let mut tasks: Vec<(GrainId, JoinHandle<()>)> = Vec::new();
        for key in &keys {
            if let Some((_, activation)) = self.activations.remove(key) {
                metrics::gauge!("orlando.grain.activations_active",
                    "grain_type" => key.type_name.to_string(),
                )
                .decrement(1.0);
                activation.cancellation.cancel(); // signal handlers to exit early
                drop(activation.sender); // release directory's clone
                tasks.push((key.clone(), activation.task));
            }
        }

        // Grace period: wait for natural deactivation (on_deactivate runs)
        let grace = Duration::from_secs(5);
        let deadline = tokio::time::Instant::now() + grace;
        for (id, mut task) in tasks {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!(%id, "grain did not deactivate within grace period, aborting");
                task.abort();
                let _ = task.await;
                continue;
            }
            match tokio::time::timeout(remaining, &mut task).await {
                Ok(_) => {} // task completed naturally with on_deactivate
                Err(_) => {
                    tracing::warn!(%id, "grain did not deactivate within grace period, aborting");
                    task.abort();
                    let _ = task.await;
                }
            }
        }

        // Also drain worker pools
        let pool_keys: Vec<GrainId> = self.worker_pools.iter().map(|e| e.key().clone()).collect();
        for key in pool_keys {
            if let Some((_, pool)) = self.worker_pools.remove(&key) {
                drop(pool.senders);
                for task in pool.tasks {
                    task.abort();
                    let _ = task.await;
                }
            }
        }

        tracing::info!("all grains drained");
    }
}

impl GrainActivator for GrainDirectory {
    fn get_sender(&self, grain_id: &GrainId) -> Option<mpsc::Sender<Envelope>> {
        self.activations.get(grain_id).map(|a| a.sender.clone())
    }

    fn register(&self, grain_id: GrainId, sender: mpsc::Sender<Envelope>, task: JoinHandle<()>) {
        let activation = Activation {
            grain_id: grain_id.clone(),
            sender,
            task,
            cancellation: CancellationToken::new(),
        };
        self.activations.insert(grain_id, activation);
    }

    fn remove(&self, grain_id: &GrainId) {
        if self.activations.remove(grain_id).is_some() {
            metrics::gauge!("orlando.grain.activations_active",
                "grain_type" => grain_id.type_name.to_string(),
            )
            .decrement(1.0);
        }
        // Also remove dead worker pools for this grain ID
        if let Some(pool) = self.worker_pools.get(grain_id) {
            let all_closed = pool.senders.iter().all(|s| s.is_closed());
            if all_closed {
                drop(pool);
                self.worker_pools.remove(grain_id);
            }
        }
    }

    fn grain_ids(&self) -> Vec<GrainId> {
        let mut seen: HashSet<GrainId> = self.activations.iter().map(|e| e.key().clone()).collect();
        for entry in self.worker_pools.iter() {
            seen.insert(entry.key().clone());
        }
        seen.into_iter().collect()
    }

    fn get_or_insert(
        &self,
        grain_id: GrainId,
        create: ActivationFactory,
    ) -> mpsc::Sender<Envelope> {
        // During drain, don't create new activations — return existing or a closed channel
        if self.draining.load(Ordering::SeqCst) {
            if let Some(sender) = self.get_sender(&grain_id)
                && !sender.is_closed()
            {
                return sender;
            }
            // Return a closed channel — the caller's send will fail with MailboxClosed
            let (tx, _rx) = mpsc::channel(1);
            drop(_rx);
            return tx;
        }

        // Enforce activation limit (0 = no limit)
        let max = self.max_activations.load(Ordering::Relaxed);
        if max > 0 && self.activations.len() >= max {
            // If this grain already exists, allow access to it
            if let Some(sender) = self.get_sender(&grain_id)
                && !sender.is_closed()
            {
                return sender;
            }
            // Over limit and grain doesn't exist — return closed channel
            tracing::warn!(
                grain_id = %grain_id,
                limit = max,
                "silo activation limit reached, rejecting new grain"
            );
            let (tx, _rx) = mpsc::channel(1);
            drop(_rx);
            return tx;
        }

        // Atomic: only one thread can win the entry for a given grain_id.
        let entry = self.activations.entry(grain_id.clone());
        match entry {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                if e.get().sender.is_closed() {
                    // Stale entry — grain deactivated but cleanup raced with this lookup.
                    // Replace with a fresh activation. No net gauge change since the stale
                    // one was already counted and this replaces it 1-for-1.
                    let (sender, task) = create(grain_id.clone());
                    let activation = Activation {
                        grain_id,
                        sender: sender.clone(),
                        task,
                        cancellation: CancellationToken::new(),
                    };
                    e.replace_entry(activation);
                    sender
                } else {
                    e.get().sender.clone()
                }
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                let (sender, task) = create(grain_id.clone());
                metrics::gauge!("orlando.grain.activations_active",
                    "grain_type" => grain_id.type_name.to_string(),
                )
                .increment(1.0);
                let activation = Activation {
                    grain_id,
                    sender: sender.clone(),
                    task,
                    cancellation: CancellationToken::new(),
                };
                e.insert(activation);
                sender
            }
        }
    }

    fn get_or_insert_pool(
        &self,
        grain_id: GrainId,
        create: PoolFactory,
        pool_size: usize,
    ) -> Vec<mpsc::Sender<Envelope>> {
        let entry = self.worker_pools.entry(grain_id.clone());
        match entry {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                // Check if any senders are still open
                let pool = e.get();
                let any_alive = pool.senders.iter().any(|s| !s.is_closed());
                if any_alive {
                    return pool.senders.clone();
                }
                // All workers dead — replace with fresh pool
                let mut senders = Vec::with_capacity(pool_size);
                let mut tasks = Vec::with_capacity(pool_size);
                for _ in 0..pool_size {
                    let (sender, task) = create(grain_id.clone());
                    senders.push(sender);
                    tasks.push(task);
                }
                let result = senders.clone();
                e.replace_entry(WorkerPool {
                    grain_id,
                    senders,
                    tasks,
                });
                result
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                let mut senders = Vec::with_capacity(pool_size);
                let mut tasks = Vec::with_capacity(pool_size);
                for _ in 0..pool_size {
                    let (sender, task) = create(grain_id.clone());
                    senders.push(sender);
                    tasks.push(task);
                }
                let result = senders.clone();
                e.insert(WorkerPool {
                    grain_id,
                    senders,
                    tasks,
                });
                result
            }
        }
    }
}
