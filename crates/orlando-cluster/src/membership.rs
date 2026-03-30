use std::sync::{Arc, RwLock};

use tokio::sync::broadcast;
use tonic::{Request, Response, Status};

use crate::failure_detector::MembershipChange;
use crate::hash_ring::{HashRing, SiloAddress};
use crate::proto::membership_server::Membership;
use crate::proto::{
    self, GetMembersRequest, GetMembersResponse, JoinRequest, JoinResponse,
    NotifyJoinRequest, NotifyJoinResponse, NotifyLeaveRequest, NotifyLeaveResponse,
    PingRequest, PingResponse,
};

pub struct MembershipService {
    ring: Arc<RwLock<HashRing>>,
    local_addr: SiloAddress,
    change_tx: broadcast::Sender<MembershipChange>,
}

impl MembershipService {
    pub fn new(
        ring: Arc<RwLock<HashRing>>,
        local_addr: SiloAddress,
        change_tx: broadcast::Sender<MembershipChange>,
    ) -> Self {
        Self {
            ring,
            local_addr,
            change_tx,
        }
    }
}

fn to_proto_addr(s: &SiloAddress) -> proto::SiloAddress {
    proto::SiloAddress {
        host: s.host.clone(),
        port: s.port as u32,
        silo_id: s.silo_id.clone(),
    }
}

fn from_proto_addr(p: proto::SiloAddress) -> SiloAddress {
    SiloAddress {
        host: p.host,
        port: p.port as u16,
        silo_id: p.silo_id,
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

        let mut ring = self
            .ring
            .write()
            .map_err(|e| Status::internal(e.to_string()))?;
        ring.add(silo.clone());

        let members = ring.members().iter().map(to_proto_addr).collect();
        drop(ring);

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

        // Skip if we already know about this silo
        {
            let ring = self
                .ring
                .read()
                .map_err(|e| Status::internal(e.to_string()))?;
            if ring.members().iter().any(|m| m.silo_id == silo.silo_id) {
                return Ok(Response::new(NotifyJoinResponse {}));
            }
        }

        tracing::info!(silo_id = %silo.silo_id, "learned about new silo via gossip");

        let mut ring = self
            .ring
            .write()
            .map_err(|e| Status::internal(e.to_string()))?;
        ring.add(silo.clone());
        drop(ring);

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

        // Skip if we don't know about this silo (already removed or never seen)
        {
            let ring = self
                .ring
                .read()
                .map_err(|e| Status::internal(e.to_string()))?;
            if !ring.members().iter().any(|m| m.silo_id == silo.silo_id) {
                return Ok(Response::new(NotifyLeaveResponse {}));
            }
        }

        tracing::info!(silo_id = %silo.silo_id, "learned about dead silo via gossip");

        let mut ring = self
            .ring
            .write()
            .map_err(|e| Status::internal(e.to_string()))?;
        ring.remove(&silo);
        drop(ring);

        let _ = self.change_tx.send(MembershipChange::SiloLeft(silo));

        Ok(Response::new(NotifyLeaveResponse {}))
    }

    async fn ping(
        &self,
        _request: Request<PingRequest>,
    ) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            silo_id: self.local_addr.silo_id.clone(),
        }))
    }

    async fn get_members(
        &self,
        _request: Request<GetMembersRequest>,
    ) -> Result<Response<GetMembersResponse>, Status> {
        let ring = self
            .ring
            .read()
            .map_err(|e| Status::internal(e.to_string()))?;
        let members = ring.members().iter().map(to_proto_addr).collect();

        Ok(Response::new(GetMembersResponse { members }))
    }
}
