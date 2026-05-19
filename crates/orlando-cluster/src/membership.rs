use std::sync::{Arc, Mutex};
use std::time::Instant;

use arc_swap::ArcSwap;

use tokio::sync::broadcast;
use tonic::{Request, Response, Status};

use orlando_core::GrainActivator;

use crate::auth::ClusterAuth;
use crate::connection_pool::ConnectionPool;
use crate::failure_detector::MembershipChange;
use crate::hash_ring::{HashRing, SiloAddress};
use crate::message_registry::MessageRegistry;
use crate::swim::{
    GossipUpdate, MemberStatus, SwimMember, SwimState, from_proto_addr, gossip_to_proto,
    proto_to_gossip, to_proto_addr,
};
use crate::proto::membership_server::Membership;
use crate::proto::{
    GetMembersRequest, GetMembersResponse, JoinRequest, JoinResponse, LookupGrainRequest,
    LookupGrainResponse, NotifyJoinRequest, NotifyJoinResponse, NotifyLeaveRequest,
    NotifyLeaveResponse, PingReqRequest, PingReqResponse, PingRequest, PingResponse,
};

/// Simple per-second rate limiter for membership operations.
struct RateLimiter {
    state: Mutex<(Instant, u32)>,
    max_per_second: u32,
}

impl RateLimiter {
    fn new(max_per_second: u32) -> Self {
        Self {
            state: Mutex::new((Instant::now(), 0)),
            max_per_second,
        }
    }

    #[allow(clippy::result_large_err)]
    fn check(&self) -> Result<(), Status> {
        let mut guard = self.state.lock().unwrap();
        let now = Instant::now();
        if now.duration_since(guard.0).as_secs() >= 1 {
            *guard = (now, 1);
            Ok(())
        } else if guard.1 < self.max_per_second {
            guard.1 += 1;
            Ok(())
        } else {
            Err(Status::resource_exhausted("membership rate limit exceeded"))
        }
    }
}

pub struct MembershipService {
    ring: Arc<ArcSwap<HashRing>>,
    local_addr: SiloAddress,
    change_tx: broadcast::Sender<MembershipChange>,
    pool: Arc<ConnectionPool>,
    swim_state: Arc<tokio::sync::Mutex<SwimState>>,
    gossip_fanout: usize,
    auth: Option<Arc<dyn ClusterAuth>>,
    join_limiter: RateLimiter,
    activator: Arc<dyn GrainActivator>,
    registry: Arc<MessageRegistry>,
}

impl std::fmt::Debug for MembershipService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MembershipService")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

impl MembershipService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ring: Arc<ArcSwap<HashRing>>,
        local_addr: SiloAddress,
        change_tx: broadcast::Sender<MembershipChange>,
        pool: Arc<ConnectionPool>,
        swim_state: Arc<tokio::sync::Mutex<SwimState>>,
        gossip_fanout: usize,
        auth: Option<Arc<dyn ClusterAuth>>,
        activator: Arc<dyn GrainActivator>,
        registry: Arc<MessageRegistry>,
    ) -> Self {
        Self {
            ring,
            local_addr,
            change_tx,
            pool,
            swim_state,
            gossip_fanout,
            auth,
            join_limiter: RateLimiter::new(10), // max 10 join/notify_join per second
            activator,
            registry,
        }
    }
}

#[tonic::async_trait]
impl Membership for MembershipService {
    async fn join(
        &self,
        request: Request<JoinRequest>,
    ) -> Result<Response<JoinResponse>, Status> {
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        self.join_limiter.check()?;
        let req = request.into_inner();
        let joiner = req
            .joiner
            .ok_or_else(|| Status::invalid_argument("missing joiner"))?;

        let silo = from_proto_addr(joiner);

        tracing::info!(silo_id = %silo.silo_id, "silo joining cluster");

        // Add to ring and SWIM state
        {
            let mut state = self.swim_state.lock().await;
            let mut new_ring = (**self.ring.load()).clone();
            new_ring.add(silo.clone());
            self.ring.store(Arc::new(new_ring));
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
            let ring = self.ring.load();
            ring.members().iter().map(to_proto_addr).collect()
        };

        let _ = self.change_tx.send(MembershipChange::SiloJoined(silo));

        Ok(Response::new(JoinResponse { members }))
    }

    async fn notify_join(
        &self,
        request: Request<NotifyJoinRequest>,
    ) -> Result<Response<NotifyJoinResponse>, Status> {
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        self.join_limiter.check()?;
        let req = request.into_inner();
        let silo_addr = req
            .silo
            .ok_or_else(|| Status::invalid_argument("missing silo"))?;

        let silo = from_proto_addr(silo_addr);

        // Check if already known
        {
            let ring = self.ring.load();
            if ring.members().iter().any(|m| m.silo_id == silo.silo_id) {
                return Ok(Response::new(NotifyJoinResponse {}));
            }
        }

        tracing::info!(silo_id = %silo.silo_id, "learned about new silo via gossip");

        // Atomic transition: hold the swim_state mutex across both the ring
        // ArcSwap store and the SwimState members insert so concurrent readers
        // never see the ring out of sync with the membership table.
        {
            let mut state = self.swim_state.lock().await;
            let mut new_ring = (**self.ring.load()).clone();
            new_ring.add(silo.clone());
            self.ring.store(Arc::new(new_ring));
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
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        let req = request.into_inner();
        let silo_addr = req
            .silo
            .ok_or_else(|| Status::invalid_argument("missing silo"))?;

        let silo = from_proto_addr(silo_addr);

        {
            let ring = self.ring.load();
            if !ring.members().iter().any(|m| m.silo_id == silo.silo_id) {
                return Ok(Response::new(NotifyLeaveResponse {}));
            }
        }

        tracing::info!(silo_id = %silo.silo_id, "learned about dead silo via gossip");

        // Atomic transition: hold the swim_state mutex across both the ring
        // ArcSwap store and the SwimState members remove.
        {
            let mut state = self.swim_state.lock().await;
            let mut new_ring = (**self.ring.load()).clone();
            new_ring.remove(&silo);
            self.ring.store(Arc::new(new_ring));
            state.members.remove(&silo.silo_id);
        }

        let _ = self.change_tx.send(MembershipChange::SiloLeft(silo));

        Ok(Response::new(NotifyLeaveResponse {}))
    }

    async fn ping(
        &self,
        request: Request<PingRequest>,
    ) -> Result<Response<PingResponse>, Status> {
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        let req = request.into_inner();

        // Process incoming gossip
        let incoming = proto_to_gossip(&req.gossip);
        if !incoming.is_empty() {
            let mut state = self.swim_state.lock().await;
            state.apply_gossip(&incoming, &self.ring, &self.change_tx, &self.pool);
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
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        let req = request.into_inner();

        // Process incoming gossip
        let incoming = proto_to_gossip(&req.gossip);
        if !incoming.is_empty() {
            let mut state = self.swim_state.lock().await;
            state.apply_gossip(&incoming, &self.ring, &self.change_tx, &self.pool);
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
        request: Request<GetMembersRequest>,
    ) -> Result<Response<GetMembersResponse>, Status> {
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        let ring = self.ring.load();
        let members = ring.members().iter().map(to_proto_addr).collect();

        Ok(Response::new(GetMembersResponse { members }))
    }

    async fn lookup_grain(
        &self,
        request: Request<LookupGrainRequest>,
    ) -> Result<Response<LookupGrainResponse>, Status> {
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        let req = request.into_inner();

        // Convert the grain type string to &'static str for GrainId construction
        let active = if let Some(type_name) = self.registry.grain_type_str(&req.grain_type) {
            let grain_id = orlando_core::GrainId {
                type_name,
                key: req.grain_key.clone(),
            };
            self.activator
                .get_sender(&grain_id)
                .map_or(false, |s| !s.is_closed())
        } else {
            false
        };

        Ok(Response::new(LookupGrainResponse {
            active,
            silo_id: if active {
                self.local_addr.silo_id.clone()
            } else {
                String::new()
            },
            endpoint: if active {
                self.local_addr.endpoint()
            } else {
                String::new()
            },
        }))
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
