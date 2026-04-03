use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tokio::sync::broadcast;

use crate::connection_pool::ConnectionPool;
use crate::failure_detector::{FailureDetectorConfig, MembershipChange};
use crate::hash_ring::{HashRing, SiloAddress};
use crate::proto::{
    GossipEntry, PingReqRequest, PingRequest, SiloAddress as ProtoSiloAddress,
    gossip_entry::UpdateType,
};

// --- Types ---

/// Status of a member in the SWIM protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberStatus {
    Alive,
    Suspect {
        since: Instant,
        reported_by: String,
    },
}

/// A cluster member tracked by the SWIM protocol.
#[derive(Clone, Debug)]
pub struct SwimMember {
    pub addr: SiloAddress,
    pub status: MemberStatus,
    pub incarnation: u64,
}

/// A gossip update to be disseminated via piggybacking on protocol messages.
#[derive(Clone, Debug)]
pub enum GossipUpdate {
    Alive { addr: SiloAddress, incarnation: u64 },
    Suspect { addr: SiloAddress, incarnation: u64, reported_by: String },
    Dead { addr: SiloAddress, incarnation: u64 },
    Join { addr: SiloAddress },
}

// --- SwimState ---

/// Shared SWIM protocol state, accessed by both the protocol loop
/// and the membership gRPC service.
pub struct SwimState {
    pub members: HashMap<String, SwimMember>,
    pub local_incarnation: u64,
    pub local_addr: SiloAddress,
    pending_gossip: VecDeque<GossipUpdate>,
}

impl SwimState {
    pub fn new(local_addr: SiloAddress) -> Self {
        Self {
            members: HashMap::new(),
            local_incarnation: 0,
            local_addr,
            pending_gossip: VecDeque::new(),
        }
    }

    /// Drain up to `n` gossip updates for piggybacking.
    pub fn drain_gossip(&mut self, n: usize) -> Vec<GossipUpdate> {
        let count = n.min(self.pending_gossip.len());
        self.pending_gossip.drain(..count).collect()
    }

    /// Enqueue a gossip update for dissemination.
    pub fn enqueue_gossip(&mut self, update: GossipUpdate) {
        self.pending_gossip.push_back(update);
    }

    /// Process incoming gossip updates against our member table.
    /// Updates ring and broadcasts membership changes as needed.
    pub fn apply_gossip(
        &mut self,
        updates: &[GossipUpdate],
        ring: &mut HashRing,
        change_tx: &broadcast::Sender<MembershipChange>,
        pool: &ConnectionPool,
    ) {
        for update in updates {
            match update {
                GossipUpdate::Alive { addr, incarnation } => {
                    if addr.silo_id == self.local_addr.silo_id {
                        continue;
                    }
                    if let Some(member) = self.members.get_mut(&addr.silo_id)
                        && *incarnation > member.incarnation
                    {
                        member.incarnation = *incarnation;
                        member.status = MemberStatus::Alive;
                        tracing::debug!(silo_id = %addr.silo_id, "suspicion cleared via alive gossip");
                    }
                }
                GossipUpdate::Suspect { addr, incarnation, reported_by } => {
                    if addr.silo_id == self.local_addr.silo_id
                        && *incarnation >= self.local_incarnation
                    {
                        self.local_incarnation = incarnation + 1;
                        self.enqueue_gossip(GossipUpdate::Alive {
                            addr: self.local_addr.clone(),
                            incarnation: self.local_incarnation,
                        });
                        tracing::info!(incarnation = self.local_incarnation, "refuted suspicion about self");
                        continue;
                    }
                    if addr.silo_id == self.local_addr.silo_id {
                        continue;
                    }
                    if let Some(member) = self.members.get_mut(&addr.silo_id)
                        && *incarnation >= member.incarnation
                        && member.status == MemberStatus::Alive
                    {
                        member.status = MemberStatus::Suspect {
                            since: Instant::now(),
                            reported_by: reported_by.clone(),
                        };
                        member.incarnation = *incarnation;
                        tracing::info!(silo_id = %addr.silo_id, "member suspected via gossip");
                    }
                }
                GossipUpdate::Dead { addr, incarnation } => {
                    if addr.silo_id == self.local_addr.silo_id {
                        continue;
                    }
                    if let Some(member) = self.members.get(&addr.silo_id)
                        && *incarnation >= member.incarnation
                    {
                        ring.remove(addr);
                        pool.remove(&addr.endpoint());
                        self.members.remove(&addr.silo_id);
                        let _ = change_tx.send(MembershipChange::SiloLeft(addr.clone()));
                        tracing::warn!(silo_id = %addr.silo_id, "member dead via gossip");
                    }
                }
                GossipUpdate::Join { addr } => {
                    if addr.silo_id == self.local_addr.silo_id {
                        continue;
                    }
                    if !self.members.contains_key(&addr.silo_id) {
                        ring.add(addr.clone());
                        self.members.insert(addr.silo_id.clone(), SwimMember {
                            addr: addr.clone(),
                            status: MemberStatus::Alive,
                            incarnation: 0,
                        });
                        let _ = change_tx.send(MembershipChange::SiloJoined(addr.clone()));
                        tracing::info!(silo_id = %addr.silo_id, "member joined via gossip");
                    }
                }
            }
        }
    }

    /// Mark a member as suspect and enqueue gossip.
    pub fn suspect_member(&mut self, silo_id: &str) {
        let gossip = if let Some(member) = self.members.get_mut(silo_id) {
            if member.status == MemberStatus::Alive {
                let reported_by = self.local_addr.silo_id.clone();
                let addr = member.addr.clone();
                let incarnation = member.incarnation;
                member.status = MemberStatus::Suspect {
                    since: Instant::now(),
                    reported_by: reported_by.clone(),
                };
                tracing::info!(silo_id = %silo_id, "member suspected after failed pings");
                Some(GossipUpdate::Suspect { addr, incarnation, reported_by })
            } else {
                None
            }
        } else {
            None
        };
        if let Some(update) = gossip {
            self.enqueue_gossip(update);
        }
    }

    /// Declare a member dead — remove from ring, pool, enqueue gossip.
    pub fn declare_dead(
        &mut self,
        silo_id: &str,
        ring: &mut HashRing,
        pool: &ConnectionPool,
        change_tx: &broadcast::Sender<MembershipChange>,
    ) {
        if let Some(member) = self.members.remove(silo_id) {
            ring.remove(&member.addr);
            pool.remove(&member.addr.endpoint());
            self.enqueue_gossip(GossipUpdate::Dead {
                addr: member.addr.clone(),
                incarnation: member.incarnation,
            });
            let _ = change_tx.send(MembershipChange::SiloLeft(member.addr));
            tracing::warn!(silo_id = %silo_id, "member declared dead");
        }
    }

    /// All tracked member silo IDs (alive or suspect).
    pub fn live_member_ids(&self) -> Vec<String> {
        self.members.keys().cloned().collect()
    }
}

impl std::fmt::Debug for SwimState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwimState")
            .field("local_addr", &self.local_addr)
            .field("local_incarnation", &self.local_incarnation)
            .field("member_count", &self.members.len())
            .field("pending_gossip", &self.pending_gossip.len())
            .finish()
    }
}

// --- Proto conversion helpers ---

pub(crate) fn gossip_to_proto(updates: &[GossipUpdate]) -> Vec<GossipEntry> {
    updates.iter().map(|u| match u {
        GossipUpdate::Alive { addr, incarnation } => GossipEntry {
            update_type: UpdateType::Alive as i32,
            silo: Some(to_proto_addr(addr)),
            incarnation: *incarnation,
            reported_by: String::new(),
        },
        GossipUpdate::Suspect { addr, incarnation, reported_by } => GossipEntry {
            update_type: UpdateType::Suspect as i32,
            silo: Some(to_proto_addr(addr)),
            incarnation: *incarnation,
            reported_by: reported_by.clone(),
        },
        GossipUpdate::Dead { addr, incarnation } => GossipEntry {
            update_type: UpdateType::Dead as i32,
            silo: Some(to_proto_addr(addr)),
            incarnation: *incarnation,
            reported_by: String::new(),
        },
        GossipUpdate::Join { addr } => GossipEntry {
            update_type: UpdateType::Join as i32,
            silo: Some(to_proto_addr(addr)),
            incarnation: 0,
            reported_by: String::new(),
        },
    }).collect()
}

pub(crate) fn proto_to_gossip(entries: &[GossipEntry]) -> Vec<GossipUpdate> {
    entries.iter().filter_map(|e| {
        let addr = from_proto_addr(e.silo.clone()?);
        let update_type = UpdateType::try_from(e.update_type).ok()?;
        Some(match update_type {
            UpdateType::Alive => GossipUpdate::Alive { addr, incarnation: e.incarnation },
            UpdateType::Suspect => GossipUpdate::Suspect {
                addr, incarnation: e.incarnation, reported_by: e.reported_by.clone(),
            },
            UpdateType::Dead => GossipUpdate::Dead { addr, incarnation: e.incarnation },
            UpdateType::Join => GossipUpdate::Join { addr },
        })
    }).collect()
}

pub(crate) fn to_proto_addr(s: &SiloAddress) -> ProtoSiloAddress {
    ProtoSiloAddress { host: s.host.clone(), port: s.port as u32, silo_id: s.silo_id.clone() }
}

pub(crate) fn from_proto_addr(p: ProtoSiloAddress) -> SiloAddress {
    SiloAddress { host: p.host, port: p.port as u16, silo_id: p.silo_id }
}

// --- SWIM protocol loop ---

/// Run the SWIM protocol loop as a background task.
pub async fn run_swim_protocol(
    config: FailureDetectorConfig,
    swim_state: Arc<tokio::sync::Mutex<SwimState>>,
    ring: Arc<RwLock<HashRing>>,
    pool: Arc<ConnectionPool>,
    change_tx: broadcast::Sender<MembershipChange>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.protocol_period) => {}
            _ = shutdown_rx.changed() => {
                tracing::debug!("SWIM protocol shutting down");
                return;
            }
        }

        // Pick a random member to probe
        let target = {
            let state = swim_state.lock().await;
            let ids = state.live_member_ids();
            tracing::debug!(local = %state.local_addr.silo_id, member_count = ids.len(), "SWIM protocol period");
            if ids.is_empty() {
                continue;
            }
            let idx = fastrand::usize(..ids.len());
            state.members.get(&ids[idx]).map(|m| m.addr.clone())
        };

        let Some(target) = target else {
            continue;
        };
        // Drain gossip for piggybacking
        let gossip = {
            let mut state = swim_state.lock().await;
            state.drain_gossip(config.gossip_fanout)
        };

        // Step 1: Direct ping
        let direct_ok = direct_ping(&target, &config, &pool, &gossip, &swim_state, &ring, &change_tx).await;

        if direct_ok {
            // Mark alive
            let mut state = swim_state.lock().await;
            if let Some(member) = state.members.get_mut(&target.silo_id) {
                member.status = MemberStatus::Alive;
            }
        } else {
            // Step 2: Indirect pings via K random peers
            let peers = {
                let state = swim_state.lock().await;
                let mut candidates: Vec<SiloAddress> = state.members.values()
                    .filter(|m| m.addr.silo_id != target.silo_id)
                    .map(|m| m.addr.clone())
                    .collect();
                fastrand::shuffle(&mut candidates);
                candidates.truncate(config.ping_req_count);
                candidates
            };

            let mut indirect_ok = false;
            for peer in &peers {
                if indirect_ping(peer, &target, &config, &pool).await {
                    indirect_ok = true;
                    break;
                }
            }

            if indirect_ok {
                let mut state = swim_state.lock().await;
                if let Some(member) = state.members.get_mut(&target.silo_id) {
                    member.status = MemberStatus::Alive;
                }
            } else {
                // Step 3: Suspect the member
                let mut state = swim_state.lock().await;
                state.suspect_member(&target.silo_id);
            }
        }

        // Expire timed-out suspects
        expire_suspects(&swim_state, &ring, &pool, &change_tx, &config).await;
    }
}

/// Send a direct ping with piggybacked gossip.
async fn direct_ping(
    target: &SiloAddress,
    config: &FailureDetectorConfig,
    pool: &Arc<ConnectionPool>,
    gossip: &[GossipUpdate],
    swim_state: &Arc<tokio::sync::Mutex<SwimState>>,
    ring: &Arc<RwLock<HashRing>>,
    change_tx: &broadcast::Sender<MembershipChange>,
) -> bool {
    let mut client = match pool.get_membership(&target.endpoint()).await {
        Ok(c) => c,
        Err(_) => return false,
    };

    let local_silo_id = swim_state.lock().await.local_addr.silo_id.clone();

    let result = tokio::time::timeout(
        config.ping_timeout,
        client.ping(PingRequest {
            silo_id: local_silo_id,
            gossip: gossip_to_proto(gossip),
        }),
    ).await;

    match result {
        Ok(Ok(response)) => {
            let resp = response.into_inner();
            let incoming = proto_to_gossip(&resp.gossip);
            if !incoming.is_empty() {
                let mut state = swim_state.lock().await;
                let mut ring_guard = ring.write().expect("ring lock poisoned");
                state.apply_gossip(&incoming, &mut ring_guard, change_tx, pool);
            }
            true
        }
        Ok(Err(_)) | Err(_) => false,
    }
}

/// Ask a peer to ping a target on our behalf (indirect ping).
async fn indirect_ping(
    peer: &SiloAddress,
    target: &SiloAddress,
    config: &FailureDetectorConfig,
    pool: &Arc<ConnectionPool>,
) -> bool {
    let mut client = match pool.get_membership(&peer.endpoint()).await {
        Ok(c) => c,
        Err(_) => return false,
    };

    let result = tokio::time::timeout(
        config.ping_timeout * 2,
        client.ping_req(PingReqRequest {
            target: Some(to_proto_addr(target)),
            requester_silo_id: String::new(),
            gossip: Vec::new(),
        }),
    ).await;

    match result {
        Ok(Ok(resp)) => resp.into_inner().target_alive,
        _ => false,
    }
}

/// Declare dead any suspects who have exceeded the suspect timeout.
async fn expire_suspects(
    swim_state: &Arc<tokio::sync::Mutex<SwimState>>,
    ring: &Arc<RwLock<HashRing>>,
    pool: &Arc<ConnectionPool>,
    change_tx: &broadcast::Sender<MembershipChange>,
    config: &FailureDetectorConfig,
) {
    let now = Instant::now();
    let expired: Vec<String> = {
        let state = swim_state.lock().await;
        state.members.iter()
            .filter_map(|(id, m)| match &m.status {
                MemberStatus::Suspect { since, .. } => {
                    if now.duration_since(*since) > config.suspect_timeout {
                        Some(id.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect()
    };

    if !expired.is_empty() {
        let mut state = swim_state.lock().await;
        let mut ring_guard = ring.write().expect("ring lock poisoned");
        for silo_id in &expired {
            state.declare_dead(silo_id, &mut ring_guard, pool, change_tx);
        }
    }
}
