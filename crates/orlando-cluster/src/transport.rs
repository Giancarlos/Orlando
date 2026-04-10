use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tonic::{Request, Response, Status};

use orlando_core::{ClusterId, GrainActivator, GrainId};

use crate::connection_pool::ConnectionPool;
use crate::cross_cluster_directory::CrossClusterDirectory;
use crate::hash_ring::HashRing;
use crate::message_registry::MessageRegistry;
use crate::network_message::Encoding;
use crate::proto::grain_transport_server::GrainTransport;
use crate::proto::{ForwardInvokeRequest, InvokeRequest, InvokeResponse};

pub struct GrainTransportService {
    registry: Arc<MessageRegistry>,
    activator: Arc<dyn GrainActivator>,
    ring: Arc<RwLock<HashRing>>,
    pool: Arc<ConnectionPool>,
    local_silo_id: String,
    // Cross-cluster (GSI) fields -- None when multi-cluster is not configured
    cross_cluster_dir: Option<Arc<dyn CrossClusterDirectory>>,
    local_cluster_id: Option<ClusterId>,
    peer_endpoints: Option<Arc<HashMap<ClusterId, String>>>,
}

impl GrainTransportService {
    pub fn new(
        registry: Arc<MessageRegistry>,
        activator: Arc<dyn GrainActivator>,
        ring: Arc<RwLock<HashRing>>,
        pool: Arc<ConnectionPool>,
        local_silo_id: String,
    ) -> Self {
        Self {
            registry,
            activator,
            ring,
            pool,
            local_silo_id,
            cross_cluster_dir: None,
            local_cluster_id: None,
            peer_endpoints: None,
        }
    }

    /// Enable cross-cluster forwarding (GSI).
    pub fn with_cross_cluster(
        mut self,
        dir: Arc<dyn CrossClusterDirectory>,
        local_cluster_id: ClusterId,
        peer_endpoints: Arc<HashMap<ClusterId, String>>,
    ) -> Self {
        self.cross_cluster_dir = Some(dir);
        self.local_cluster_id = Some(local_cluster_id);
        self.peer_endpoints = Some(peer_endpoints);
        self
    }

    /// Check the hash ring to determine which silo owns a grain.
    /// Returns None if this silo owns it (or ring is empty), Some(endpoint) if remote.
    fn find_owner(&self, grain_type: &str, grain_key: &str) -> Option<String> {
        let ring_key = format!("{}/{}", grain_type, grain_key);
        let ring = self.ring.read().expect("ring lock poisoned");
        match ring.get(&ring_key) {
            Some(target) if target.silo_id != self.local_silo_id => Some(target.endpoint()),
            _ => None,
        }
    }

    /// Check if a grain is owned by another cluster and forward if so.
    /// Returns Some(response) if forwarded, None if we should handle locally.
    async fn check_cross_cluster(
        &self,
        req: &InvokeRequest,
    ) -> Result<Option<Response<InvokeResponse>>, Status> {
        let (dir, local_cid, peers) = match (
            &self.cross_cluster_dir,
            &self.local_cluster_id,
            &self.peer_endpoints,
        ) {
            (Some(d), Some(c), Some(p)) => (d, c, p),
            _ => return Ok(None),
        };

        // We need a &'static str for type_name. Use the registry to resolve it.
        // For cross-cluster purposes we use the string form of the grain identity.
        let grain_id = GrainId {
            type_name: self.registry.resolve_grain_type(&req.grain_type),
            key: req.grain_key.clone(),
        };

        match dir.lookup(&grain_id).await {
            Ok(Some(ownership)) => {
                if ownership.cluster_id == *local_cid {
                    // We own it, proceed with local dispatch
                    return Ok(None);
                }
                // Forward to the owning cluster
                if let Some(endpoint) = peers.get(&ownership.cluster_id) {
                    tracing::debug!(
                        grain_type = %req.grain_type,
                        grain_key = %req.grain_key,
                        target_cluster = %ownership.cluster_id,
                        "forwarding grain call to owning cluster"
                    );
                    let response = self.forward_to_cluster(endpoint, req, local_cid).await?;
                    return Ok(Some(response));
                }
                // Unknown peer cluster, fall through to local
                Ok(None)
            }
            Ok(None) => {
                // Not registered anywhere. Register ourselves as the owner.
                let _ = dir.register(&grain_id, local_cid).await;
                Ok(None)
            }
            Err(e) => {
                tracing::warn!(error = %e, "cross-cluster directory lookup failed, falling back to local");
                Ok(None)
            }
        }
    }

    /// Forward a grain call to another cluster's gateway service.
    async fn forward_to_cluster(
        &self,
        endpoint: &str,
        req: &InvokeRequest,
        local_cluster_id: &ClusterId,
    ) -> Result<Response<InvokeResponse>, Status> {
        let mut client = self
            .pool
            .get_gateway(endpoint)
            .await
            .map_err(|e| Status::unavailable(format!("cross-cluster connection failed: {}", e)))?;

        let forward_req = ForwardInvokeRequest {
            grain_type: req.grain_type.clone(),
            grain_key: req.grain_key.clone(),
            message_type: req.message_type.clone(),
            payload: req.payload.clone(),
            encoding: req.encoding,
            request_context: req.request_context.clone(),
            source_cluster_id: local_cluster_id.to_string(),
        };

        let response = client
            .forward_invoke(forward_req)
            .await
            .map_err(|e| Status::internal(format!("cross-cluster forward failed: {}", e)))?;

        let inner = response.into_inner();
        Ok(Response::new(InvokeResponse {
            payload: inner.payload,
            error: inner.error,
            encoding: inner.encoding,
        }))
    }
}

#[tonic::async_trait]
impl GrainTransport for GrainTransportService {
    async fn invoke(
        &self,
        request: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        let req = request.into_inner();
        let encoding = Encoding::from_proto(req.encoding);

        // Cross-cluster check (GSI): if another cluster owns this grain, forward there
        if let Some(response) = self.check_cross_cluster(&req).await? {
            return Ok(response);
        }

        // Gateway forwarding: if this grain belongs on a different silo, forward the request
        if let Some(endpoint) = self.find_owner(&req.grain_type, &req.grain_key) {
            tracing::debug!(
                grain_type = %req.grain_type,
                grain_key = %req.grain_key,
                target = %endpoint,
                "forwarding grain call to owner silo"
            );

            let mut client = self
                .pool
                .get_transport(&endpoint)
                .await
                .map_err(|e| Status::unavailable(e.to_string()))?;

            // Forward the request as-is (including request_context) to the owning silo
            let response = client
                .invoke(InvokeRequest {
                    grain_type: req.grain_type,
                    grain_key: req.grain_key,
                    message_type: req.message_type,
                    payload: req.payload,
                    encoding: req.encoding,
                    request_context: req.request_context,
                })
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
