use std::sync::{Arc, RwLock};

use tokio::sync::broadcast;
use tonic::{Request, Response, Status};

use crate::connection_pool::ConnectionPool;
use crate::failure_detector::MembershipChange;
use crate::hash_ring::{HashRing, SiloAddress};
use crate::swim::{
    GossipUpdate, MemberStatus, SwimMember, SwimState, from_proto_addr, gossip_to_proto,
    proto_to_gossip, to_proto_addr,
};
use crate::proto::membership_server::Membership;
use crate::proto::{
    GetMembersRequest, GetMembersResponse, JoinRequest, JoinResponse, NotifyJoinRequest,
    NotifyJoinResponse, NotifyLeaveRequest, NotifyLeaveResponse, PingReqRequest, PingReqResponse,
    PingRequest, PingResponse,
};

pub struct MembershipService {
    ring: Arc<RwLock<HashRing>>,
    local_addr: SiloAddress,
    change_tx: broadcast::Sender<MembershipChange>,
    pool: Arc<ConnectionPool>,
    swim_state: Arc<tokio::sync::Mutex<SwimState>>,
    gossip_fanout: usize,
}

impl std::fmt::Debug for MembershipService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MembershipService")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

impl MembershipService {
    pub fn new(
        ring: Arc<RwLock<HashRing>>,
        local_addr: SiloAddress,
        change_tx: broadcast::Sender<MembershipChange>,
        pool: Arc<ConnectionPool>,
        swim_state: Arc<tokio::sync::Mutex<SwimState>>,
        gossip_fanout: usize,
    ) -> Self {
        Self {
            ring,
            local_addr,
            change_tx,
            pool,
            swim_state,
            gossip_fanout,
        }
    }
}

#[tonic::async_trait]
impl Membership for MembershipService {
    async fn join(
        &self,
        request: Request<JoinRequest>,
    ) -> Result<Response<JoinResponse>, Status> {
        let req = request.into_inner();
        let joiner = req
            .joiner
            .ok_or_else(|| Status::invalid_argument("missing joiner"))?;

        let silo = from_proto_addr(joiner);

        tracing::info!(silo_id = %silo.silo_id, "silo joining cluster");

        // Add to ring
        {
            let mut ring = self.ring.write().map_err(|e| Status::internal(e.to_string()))?;
            ring.add(silo.clone());
        }

        // Add to SWIM state
        {
            let mut state = self.swim_state.lock().await;
            if !state.members.contains_key(&silo.silo_id) {
                state.members.insert(silo.silo_id.clone(), SwimMember {
                    addr: silo.clone(),
                    status: MemberStatus::Alive,
                    incarnation: 0,
                });
                state.enqueue_gossip(GossipUpdate::Join { addr: silo.clone() });
            }
        }

        let members = {
            let ring = self.ring.read().map_err(|e| Status::internal(e.to_string()))?;
            ring.members().iter().map(to_proto_addr).collect()
        };

        let _ = self.change_tx.send(MembershipChange::SiloJoined(silo));

        Ok(Response::new(JoinResponse { members }))
    }

    async fn notify_join(
        &self,
        request: Request<NotifyJoinRequest>,
    ) -> Result<Response<NotifyJoinResponse>, Status> {
        let req = request.into_inner();
        let silo_addr = req
            .silo
            .ok_or_else(|| Status::invalid_argument("missing silo"))?;

        let silo = from_proto_addr(silo_addr);

        // Check if already known
        {
            let ring = self.ring.read().map_err(|e| Status::internal(e.to_string()))?;
            if ring.members().iter().any(|m| m.silo_id == silo.silo_id) {
                return Ok(Response::new(NotifyJoinResponse {}));
            }
        }

        tracing::info!(silo_id = %silo.silo_id, "learned about new silo via gossip");

        {
            let mut ring = self.ring.write().map_err(|e| Status::internal(e.to_string()))?;
            ring.add(silo.clone());
        }

        // Add to SWIM state
        {
            let mut state = self.swim_state.lock().await;
            state.members.insert(silo.silo_id.clone(), SwimMember {
                addr: silo.clone(),
                status: MemberStatus::Alive,
                incarnation: 0,
            });
        }

        let _ = self.change_tx.send(MembershipChange::SiloJoined(silo));

        Ok(Response::new(NotifyJoinResponse {}))
    }

    async fn notify_leave(
        &self,
        request: Request<NotifyLeaveRequest>,
    ) -> Result<Response<NotifyLeaveResponse>, Status> {
        let req = request.into_inner();
        let silo_addr = req
            .silo
            .ok_or_else(|| Status::invalid_argument("missing silo"))?;

        let silo = from_proto_addr(silo_addr);

        {
            let ring = self.ring.read().map_err(|e| Status::internal(e.to_string()))?;
            if !ring.members().iter().any(|m| m.silo_id == silo.silo_id) {
                return Ok(Response::new(NotifyLeaveResponse {}));
            }
        }

        tracing::info!(silo_id = %silo.silo_id, "learned about dead silo via gossip");

        {
            let mut ring = self.ring.write().map_err(|e| Status::internal(e.to_string()))?;
            ring.remove(&silo);
        }

        // Remove from SWIM state
        {
            let mut state = self.swim_state.lock().await;
            state.members.remove(&silo.silo_id);
        }

        let _ = self.change_tx.send(MembershipChange::SiloLeft(silo));

        Ok(Response::new(NotifyLeaveResponse {}))
    }

    async fn ping(
        &self,
        request: Request<PingRequest>,
    ) -> Result<Response<PingResponse>, Status> {
        let req = request.into_inner();

        // Process incoming gossip
        let incoming = proto_to_gossip(&req.gossip);
        if !incoming.is_empty() {
            let mut state = self.swim_state.lock().await;
            let mut ring = self.ring.write().map_err(|e| Status::internal(e.to_string()))?;
            state.apply_gossip(&incoming, &mut ring, &self.change_tx, &self.pool);
        }

        // Reply with our own gossip
        let outgoing = {
            let mut state = self.swim_state.lock().await;
            state.drain_gossip(self.gossip_fanout)
        };

        Ok(Response::new(PingResponse {
            silo_id: self.local_addr.silo_id.clone(),
            gossip: gossip_to_proto(&outgoing),
        }))
    }

    /// Indirect ping: ping a target on behalf of a requester.
    async fn ping_req(
        &self,
        request: Request<PingReqRequest>,
    ) -> Result<Response<PingReqResponse>, Status> {
        let req = request.into_inner();

        // Process incoming gossip
        let incoming = proto_to_gossip(&req.gossip);
        if !incoming.is_empty() {
            let mut state = self.swim_state.lock().await;
            let mut ring = self.ring.write().map_err(|e| Status::internal(e.to_string()))?;
            state.apply_gossip(&incoming, &mut ring, &self.change_tx, &self.pool);
        }

        let target_addr = req
            .target
            .ok_or_else(|| Status::invalid_argument("missing target"))?;

        let target = from_proto_addr(target_addr);
        let target_alive = self.try_ping_target(&target).await;

        let outgoing = {
            let mut state = self.swim_state.lock().await;
            state.drain_gossip(self.gossip_fanout)
        };

        Ok(Response::new(PingReqResponse {
            target_alive,
            gossip: gossip_to_proto(&outgoing),
        }))
    }

    async fn get_members(
        &self,
        _request: Request<GetMembersRequest>,
    ) -> Result<Response<GetMembersResponse>, Status> {
        let ring = self.ring.read().map_err(|e| Status::internal(e.to_string()))?;
        let members = ring.members().iter().map(to_proto_addr).collect();

        Ok(Response::new(GetMembersResponse { members }))
    }
}

impl MembershipService {
    /// Attempt a direct ping to a target (for PingReq handling).
    async fn try_ping_target(&self, target: &SiloAddress) -> bool {
        let mut client = match self.pool.get_membership(&target.endpoint()).await {
            Ok(c) => c,
            Err(_) => return false,
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.ping(PingRequest {
                silo_id: self.local_addr.silo_id.clone(),
                gossip: Vec::new(),
            }),
        )
        .await;

        matches!(result, Ok(Ok(_)))
    }
}
