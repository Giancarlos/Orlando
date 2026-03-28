use std::sync::Arc;

use tonic::{Request, Response, Status};

use orlando_core::GrainActivator;

use crate::message_registry::MessageRegistry;
use crate::proto::grain_transport_server::GrainTransport;
use crate::proto::{InvokeRequest, InvokeResponse};

pub struct GrainTransportService {
    registry: Arc<MessageRegistry>,
    activator: Arc<dyn GrainActivator>,
}

impl GrainTransportService {
    pub fn new(registry: Arc<MessageRegistry>, activator: Arc<dyn GrainActivator>) -> Self {
        Self {
            registry,
            activator,
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

        match self
            .registry
            .dispatch(
                &req.grain_type,
                req.grain_key,
                &req.message_type,
                req.payload,
                self.activator.clone(),
            )
            .await
        {
            Ok(payload) => Ok(Response::new(InvokeResponse {
                payload,
                error: String::new(),
            })),
            Err(e) => Ok(Response::new(InvokeResponse {
                payload: Vec::new(),
                error: e.to_string(),
            })),
        }
    }
}
