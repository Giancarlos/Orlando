use std::sync::Arc;

use arc_swap::ArcSwap;

use tonic::{Request, Response, Status};

use orlando_core::GrainActivator;

use crate::auth::ClusterAuth;
use crate::connection_pool::ConnectionPool;
use crate::hash_ring::HashRing;
use crate::message_registry::MessageRegistry;
use crate::network_message::Encoding;
use crate::proto::grain_transport_server::GrainTransport;
use crate::proto::{InvokeRequest, InvokeResponse};

/// Maximum number of forwarding hops before a request is rejected.
/// Prevents infinite forwarding loops when the ring is stale on multiple silos.
const MAX_HOPS: u32 = 3;
const HOP_COUNT_KEY: &str = "__orlando_hop_count";

/// Maximum payload size accepted for deserialization (64 MB).
const MAX_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;

pub struct GrainTransportService {
    registry: Arc<MessageRegistry>,
    activator: Arc<dyn GrainActivator>,
    ring: Arc<ArcSwap<HashRing>>,
    pool: Arc<ConnectionPool>,
    local_silo_id: String,
    auth: Option<Arc<dyn ClusterAuth>>,
}

impl GrainTransportService {
    pub fn new(
        registry: Arc<MessageRegistry>,
        activator: Arc<dyn GrainActivator>,
        ring: Arc<ArcSwap<HashRing>>,
        pool: Arc<ConnectionPool>,
        local_silo_id: String,
        auth: Option<Arc<dyn ClusterAuth>>,
    ) -> Self {
        Self {
            registry,
            activator,
            ring,
            pool,
            local_silo_id,
            auth,
        }
    }

    /// Check all other silos for an active grain. Returns the endpoint of the owner if found.
    async fn lookup_grain_cluster_wide(
        &self,
        grain_type: &str,
        grain_key: &str,
    ) -> Option<String> {
        let members: Vec<crate::hash_ring::SiloAddress> = {
            let ring = self.ring.load();
            ring.members()
                .into_iter()
                .filter(|m| m.silo_id != self.local_silo_id)
                .collect()
        };

        if members.is_empty() {
            return None;
        }

        // Parallel lookup — fire all RPCs concurrently, return on first hit
        let mut tasks = tokio::task::JoinSet::new();
        for member in members {
            let pool = self.pool.clone();
            let gt = grain_type.to_string();
            let gk = grain_key.to_string();
            tasks.spawn(async move {
                let mut client = pool.get_membership(&member.endpoint()).await.ok()?;
                let resp = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    client.lookup_grain(crate::proto::LookupGrainRequest {
                        grain_type: gt,
                        grain_key: gk,
                    }),
                )
                .await
                .ok()?
                .ok()?;
                let inner = resp.into_inner();
                if inner.active {
                    Some(inner.endpoint)
                } else {
                    None
                }
            });
        }

        while let Some(result) = tasks.join_next().await {
            if let Ok(Some(endpoint)) = result {
                tasks.abort_all();
                return Some(endpoint);
            }
        }

        None
    }

    /// Check the hash ring to determine which silo owns a grain.
    /// Returns None if this silo owns it (or ring is empty), Some(endpoint) if remote.
    fn find_owner(&self, grain_type: &str, grain_key: &str) -> Option<String> {
        let ring_key = format!("{}/{}", grain_type, grain_key);
        let ring = self.ring.load();
        match ring.get(&ring_key) {
            Some(target) if target.silo_id != self.local_silo_id => {
                Some(target.endpoint())
            }
            _ => None,
        }
    }
}

#[tonic::async_trait]
impl GrainTransport for GrainTransportService {
    async fn invoke(
        &self,
        request: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        if let Some(ref auth) = self.auth {
            auth.authenticate(request.metadata())?;
        }
        let req = request.into_inner();
        let encoding = Encoding::from_proto(req.encoding);

        // Reject oversized payloads before deserialization
        if req.payload.len() > MAX_PAYLOAD_SIZE {
            return Err(Status::invalid_argument(format!(
                "payload size {} exceeds maximum of {} bytes",
                req.payload.len(),
                MAX_PAYLOAD_SIZE,
            )));
        }

        // Gateway forwarding: if this grain belongs on a different silo, forward the request
        if let Some(endpoint) = self.find_owner(&req.grain_type, &req.grain_key) {
            // Check hop count to prevent infinite forwarding loops
            let hop_count: u32 = req
                .request_context
                .get(HOP_COUNT_KEY)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);

            if hop_count >= MAX_HOPS {
                return Err(Status::internal(format!(
                    "maximum forwarding hops ({}) exceeded — possible routing loop",
                    MAX_HOPS,
                )));
            }

            tracing::debug!(
                grain_type = %req.grain_type,
                grain_key = %req.grain_key,
                target = %endpoint,
                hop = hop_count + 1,
                "forwarding grain call to owner silo"
            );

            let mut client = self
                .pool
                .get_transport(&endpoint)
                .await
                .map_err(|e| Status::unavailable(e.to_string()))?;

            // Increment hop count and forward
            let mut fwd_context = req.request_context.clone();
            fwd_context.insert(HOP_COUNT_KEY.to_string(), (hop_count + 1).to_string());

            let response = client
                .invoke(self.pool.authorized_request(InvokeRequest {
                    grain_type: req.grain_type,
                    grain_key: req.grain_key,
                    message_type: req.message_type,
                    payload: req.payload,
                    encoding: req.encoding,
                    request_context: fwd_context,
                    message_version: req.message_version,
                }))
                .await
                .map_err(|e| Status::internal(e.to_string()))?;

            return Ok(response);
        }

        // Before local dispatch: check if grain is already active locally
        let already_local = if let Some(type_name) = self.registry.grain_type_str(&req.grain_type) {
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

        // If not active locally, check cluster-wide to prevent duplicate activation
        if !already_local
            && let Some(endpoint) =
                self.lookup_grain_cluster_wide(&req.grain_type, &req.grain_key)
                    .await
        {
            // Parse hop count (shared with the gateway forwarding path)
            let hop_count: u32 = req
                .request_context
                .get(HOP_COUNT_KEY)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);

            if hop_count >= MAX_HOPS {
                return Err(Status::internal(format!(
                    "maximum forwarding hops ({}) exceeded during directory lookup",
                    MAX_HOPS,
                )));
            }

            tracing::debug!(
                grain_type = %req.grain_type,
                grain_key = %req.grain_key,
                target = %endpoint,
                hop = hop_count + 1,
                "grain active on another silo, forwarding to prevent duplicate activation"
            );

            let mut client = self
                .pool
                .get_transport(&endpoint)
                .await
                .map_err(|e| Status::unavailable(e.to_string()))?;

            let mut fwd_context = req.request_context.clone();
            fwd_context.insert(HOP_COUNT_KEY.to_string(), (hop_count + 1).to_string());

            let response = client
                .invoke(self.pool.authorized_request(InvokeRequest {
                    grain_type: req.grain_type,
                    grain_key: req.grain_key,
                    message_type: req.message_type,
                    payload: req.payload,
                    encoding: req.encoding,
                    request_context: fwd_context,
                    message_version: req.message_version,
                }))
                .await
                .map_err(|e| Status::internal(e.to_string()))?;

            return Ok(response);
        }

        // Local dispatch — pass request context through to the grain handler
        match self
            .registry
            .dispatch(
                &req.grain_type,
                req.grain_key,
                &req.message_type,
                req.message_version,
                req.payload,
                encoding,
                req.request_context,
                self.activator.clone(),
            )
            .await
        {
            Ok((payload, response_encoding)) => Ok(Response::new(InvokeResponse {
                payload,
                error: String::new(),
                encoding: response_encoding.to_proto(),
            })),
            Err(e) => Ok(Response::new(InvokeResponse {
                payload: Vec::new(),
                error: e.to_string(),
                encoding: encoding.to_proto(),
            })),
        }
    }
}
