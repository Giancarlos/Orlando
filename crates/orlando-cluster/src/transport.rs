use std::sync::{Arc, RwLock};

use tonic::{Request, Response, Status};

use orlando_core::GrainActivator;

use crate::connection_pool::ConnectionPool;
use crate::hash_ring::HashRing;
use crate::message_registry::MessageRegistry;
use crate::network_message::Encoding;
use crate::proto::grain_transport_server::GrainTransport;
use crate::proto::{InvokeRequest, InvokeResponse};

pub struct GrainTransportService {
    registry: Arc<MessageRegistry>,
    activator: Arc<dyn GrainActivator>,
    ring: Arc<RwLock<HashRing>>,
    pool: Arc<ConnectionPool>,
    local_silo_id: String,
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
        }
    }

    /// Check the hash ring to determine which silo owns a grain.
    /// Returns None if this silo owns it (or ring is empty), Some(endpoint) if remote.
    fn find_owner(&self, grain_type: &str, grain_key: &str) -> Option<String> {
        let ring_key = format!("{}/{}", grain_type, grain_key);
        let ring = self.ring.read().expect("ring lock poisoned");
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
        let req = request.into_inner();
        let encoding = Encoding::from_proto(req.encoding);

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
