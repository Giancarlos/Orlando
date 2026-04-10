use std::sync::Arc;

use tonic::{Request, Response, Status};

use orlando_core::GrainActivator;

use crate::message_registry::MessageRegistry;
use crate::network_message::Encoding;
use crate::proto::cluster_gateway_server::ClusterGateway;
use crate::proto::{
    ClusterPingRequest, ClusterPingResponse, ForwardInvokeRequest, ForwardInvokeResponse,
};

/// gRPC service handling cross-cluster grain forwarding.
///
/// When a grain is accessed from a cluster that doesn't own it,
/// the request is forwarded to the owning cluster via this service.
pub struct ClusterGatewayService {
    registry: Arc<MessageRegistry>,
    activator: Arc<dyn GrainActivator>,
    local_cluster_id: String,
}

impl ClusterGatewayService {
    pub fn new(
        registry: Arc<MessageRegistry>,
        activator: Arc<dyn GrainActivator>,
        local_cluster_id: String,
    ) -> Self {
        Self {
            registry,
            activator,
            local_cluster_id,
        }
    }
}

#[tonic::async_trait]
impl ClusterGateway for ClusterGatewayService {
    async fn forward_invoke(
        &self,
        request: Request<ForwardInvokeRequest>,
    ) -> Result<Response<ForwardInvokeResponse>, Status> {
        let req = request.into_inner();
        let encoding = Encoding::from_proto(req.encoding);

        tracing::debug!(
            grain_type = %req.grain_type,
            grain_key = %req.grain_key,
            source_cluster = %req.source_cluster_id,
            "handling cross-cluster forward invoke"
        );

        // Dispatch locally: this grain belongs to us
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
            Ok((payload, enc)) => Ok(Response::new(ForwardInvokeResponse {
                payload,
                error: String::new(),
                encoding: enc.to_proto(),
            })),
            Err(e) => Ok(Response::new(ForwardInvokeResponse {
                payload: Vec::new(),
                error: e.to_string(),
                encoding: encoding.to_proto(),
            })),
        }
    }

    async fn cluster_ping(
        &self,
        _request: Request<ClusterPingRequest>,
    ) -> Result<Response<ClusterPingResponse>, Status> {
        Ok(Response::new(ClusterPingResponse {
            cluster_id: self.local_cluster_id.clone(),
            silo_count: 1,
            active_grains: 0,
        }))
    }
}
