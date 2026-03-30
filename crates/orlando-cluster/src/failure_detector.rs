use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::broadcast;

use crate::connection_pool::ConnectionPool;
use crate::hash_ring::{HashRing, SiloAddress};
use crate::proto::{NotifyLeaveRequest, PingRequest, SiloAddress as ProtoSiloAddress};

#[derive(Clone, Debug)]
pub enum MembershipChange {
    SiloJoined(SiloAddress),
    SiloLeft(SiloAddress),
}

#[derive(Clone, Debug)]
pub struct FailureDetectorConfig {
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    pub max_missed_pings: u32,
}

impl Default for FailureDetectorConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(2),
            ping_timeout: Duration::from_secs(1),
            max_missed_pings: 3,
        }
    }
}

pub struct FailureDetector {
    config: FailureDetectorConfig,
    ring: Arc<RwLock<HashRing>>,
    pool: Arc<ConnectionPool>,
    local_silo_id: String,
    change_tx: broadcast::Sender<MembershipChange>,
}

impl FailureDetector {
    pub fn new(
        config: FailureDetectorConfig,
        ring: Arc<RwLock<HashRing>>,
        pool: Arc<ConnectionPool>,
        local_silo_id: String,
        change_tx: broadcast::Sender<MembershipChange>,
    ) -> Self {
        Self {
            config,
            ring,
            pool,
            local_silo_id,
            change_tx,
        }
    }

    pub async fn run(self) {
        let mut missed: HashMap<String, u32> = HashMap::new();

        loop {
            tokio::time::sleep(self.config.ping_interval).await;

            let members = {
                let ring = self.ring.read().expect("ring lock poisoned");
                ring.members()
            };

            for member in &members {
                if member.silo_id == self.local_silo_id {
                    continue;
                }

                let alive = self.ping_silo(member).await;

                if alive {
                    missed.remove(&member.silo_id);
                } else {
                    let count = missed.entry(member.silo_id.clone()).or_insert(0);
                    *count += 1;

                    if *count >= self.config.max_missed_pings {
                        tracing::warn!(
                            silo_id = %member.silo_id,
                            missed = *count,
                            "silo declared dead, removing from cluster"
                        );

                        {
                            let mut ring = self.ring.write().expect("ring lock poisoned");
                            ring.remove(member);
                        }

                        self.pool.remove(&member.endpoint());
                        missed.remove(&member.silo_id);

                        let _ = self.change_tx.send(MembershipChange::SiloLeft(member.clone()));

                        // Gossip: notify remaining peers about the dead silo
                        self.notify_peers_of_leave(member, &members).await;
                    }
                }
            }
        }
    }

    async fn notify_peers_of_leave(&self, dead: &SiloAddress, members: &[SiloAddress]) {
        let proto_dead = ProtoSiloAddress {
            host: dead.host.clone(),
            port: dead.port as u32,
            silo_id: dead.silo_id.clone(),
        };

        for peer in members {
            if peer.silo_id == self.local_silo_id || peer.silo_id == dead.silo_id {
                continue;
            }
            if let Ok(mut client) = self.pool.get_membership(&peer.endpoint()).await {
                let _ = client
                    .notify_leave(NotifyLeaveRequest {
                        silo: Some(proto_dead.clone()),
                    })
                    .await;
            }
        }
    }

    async fn ping_silo(&self, silo: &SiloAddress) -> bool {
        let client = self.pool.get_membership(&silo.endpoint()).await;
        let mut client = match client {
            Ok(c) => c,
            Err(_) => return false,
        };

        let result = tokio::time::timeout(
            self.config.ping_timeout,
            client.ping(PingRequest {
                silo_id: self.local_silo_id.clone(),
            }),
        )
        .await;

        matches!(result, Ok(Ok(_)))
    }
}
