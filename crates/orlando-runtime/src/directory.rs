use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use orlando_core::{ActivationFactory, Envelope, GrainActivator, GrainId};

use crate::activation::Activation;

#[derive(Debug)]
pub struct GrainDirectory {
    activations: DashMap<GrainId, Activation>,
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
    }

    fn grain_ids(&self) -> Vec<GrainId> {
        self.activations.iter().map(|e| e.key().clone()).collect()
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
}
