use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use crate::error::ClusterError;
use crate::proto::grain_transport_client::GrainTransportClient;
use crate::proto::membership_client::MembershipClient;

/// Timeout for establishing new gRPC connections.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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

    async fn connect_channel(endpoint: &str) -> Result<tonic::transport::Channel, ClusterError> {
        let uri = format!("http://{}", endpoint);
        let channel = tonic::transport::Endpoint::from_shared(uri)
            .map_err(|e| ClusterError::Transport(e.to_string()))?
            .connect_timeout(CONNECT_TIMEOUT)
            .connect()
            .await
            .map_err(|e| ClusterError::Transport(e.to_string()))?;
        Ok(channel)
    }

    pub async fn get_transport(
        self: &Arc<Self>,
        endpoint: &str,
    ) -> Result<GrainTransportClient<tonic::transport::Channel>, ClusterError> {
        if let Some(client) = self.transports.get(endpoint) {
            return Ok(client.clone());
        }

        let channel = Self::connect_channel(endpoint).await?;
        let client = GrainTransportClient::new(channel);

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

        let channel = Self::connect_channel(endpoint).await?;
        let client = MembershipClient::new(channel);

        self.memberships
            .insert(endpoint.to_string(), client.clone());
        Ok(client)
    }

    pub fn remove(&self, endpoint: &str) {
        self.transports.remove(endpoint);
        self.memberships.remove(endpoint);
    }
}
