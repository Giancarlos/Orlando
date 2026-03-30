use std::sync::Arc;

use dashmap::DashMap;

use crate::error::ClusterError;
use crate::proto::grain_transport_client::GrainTransportClient;
use crate::proto::membership_client::MembershipClient;

pub struct ConnectionPool {
    transports: DashMap<String, GrainTransportClient<tonic::transport::Channel>>,
    memberships: DashMap<String, MembershipClient<tonic::transport::Channel>>,
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            transports: DashMap::new(),
            memberships: DashMap::new(),
        }
    }

    pub async fn get_transport(
        self: &Arc<Self>,
        endpoint: &str,
    ) -> Result<GrainTransportClient<tonic::transport::Channel>, ClusterError> {
        if let Some(client) = self.transports.get(endpoint) {
            return Ok(client.clone());
        }

        let client =
            GrainTransportClient::connect(format!("http://{}", endpoint))
                .await
                .map_err(|e| ClusterError::Transport(e.to_string()))?;

        self.transports
            .insert(endpoint.to_string(), client.clone());
        Ok(client)
    }

    pub async fn get_membership(
        self: &Arc<Self>,
        endpoint: &str,
    ) -> Result<MembershipClient<tonic::transport::Channel>, ClusterError> {
        if let Some(client) = self.memberships.get(endpoint) {
            return Ok(client.clone());
        }

        let client = MembershipClient::connect(format!("http://{}", endpoint))
            .await
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        self.memberships
            .insert(endpoint.to_string(), client.clone());
        Ok(client)
    }

    pub fn remove(&self, endpoint: &str) {
        self.transports.remove(endpoint);
        self.memberships.remove(endpoint);
    }
}
