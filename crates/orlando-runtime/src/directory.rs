use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use orlando_core::{ActivationFactory, Envelope, GrainActivator, GrainId, PoolFactory};

use crate::activation::Activation;
use crate::worker_pool::WorkerPool;

#[derive(Debug)]
pub struct GrainDirectory {
    activations: DashMap<GrainId, Activation>,
    worker_pools: DashMap<GrainId, WorkerPool>,
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
        }
    }

    pub fn remove(&self, id: &GrainId) -> Option<Activation> {
        self.activations.remove(id).map(|(_, a)| a)
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
        };
        self.activations.insert(grain_id, activation);
    }

    fn remove(&self, grain_id: &GrainId) {
        self.activations.remove(grain_id);
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
        let mut ids: Vec<GrainId> = self.activations.iter().map(|e| e.key().clone()).collect();
        for entry in self.worker_pools.iter() {
            if !ids.contains(entry.key()) {
                ids.push(entry.key().clone());
            }
        }
        ids
    }

    fn get_or_insert(
        &self,
        grain_id: GrainId,
        create: ActivationFactory,
    ) -> mpsc::Sender<Envelope> {
        // Atomic: only one thread can win the entry for a given grain_id.
        let entry = self.activations.entry(grain_id.clone());
        match entry {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                if e.get().sender.is_closed() {
                    // Stale entry — grain deactivated but cleanup raced with this lookup.
                    // Replace with a fresh activation.
                    let (sender, task) = create(grain_id.clone());
                    let activation = Activation {
                        grain_id,
                        sender: sender.clone(),
                        task,
                    };
                    e.replace_entry(activation);
                    sender
                } else {
                    e.get().sender.clone()
                }
            }
            dashmap::mapref::entry::Entry::Vacant(e) => {
                let (sender, task) = create(grain_id.clone());
                let activation = Activation {
                    grain_id,
                    sender: sender.clone(),
                    task,
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
