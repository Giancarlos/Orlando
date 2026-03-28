use std::sync::{Arc, RwLock};

use tonic::{Request, Response, Status};

use crate::hash_ring::{HashRing, SiloAddress};
use crate::proto::membership_server::Membership;
use crate::proto::{
    self, GetMembersRequest, GetMembersResponse, JoinRequest, JoinResponse, PingRequest,
    PingResponse,
};

pub struct MembershipService {
    ring: Arc<RwLock<HashRing>>,
    local_addr: SiloAddress,
}

impl MembershipService {
    pub fn new(ring: Arc<RwLock<HashRing>>, local_addr: SiloAddress) -> Self {
        Self { ring, local_addr }
    }
}

fn to_proto_addr(s: &SiloAddress) -> proto::SiloAddress {
    proto::SiloAddress {
        host: s.host.clone(),
        port: s.port as u32,
        silo_id: s.silo_id.clone(),
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

        let silo = SiloAddress {
            host: joiner.host,
            port: joiner.port as u16,
            silo_id: joiner.silo_id,
        };

        tracing::info!(silo_id = %silo.silo_id, "silo joining cluster");

        let mut ring = self
            .ring
            .write()
            .map_err(|e| Status::internal(e.to_string()))?;
        ring.add(silo);

        let members = ring.members().iter().map(to_proto_addr).collect();
        Ok(Response::new(JoinResponse { members }))
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
