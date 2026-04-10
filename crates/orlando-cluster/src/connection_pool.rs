use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use crate::error::ClusterError;
use crate::proto::cluster_gateway_client::ClusterGatewayClient;
use crate::proto::grain_transport_client::GrainTransportClient;
use crate::proto::membership_client::MembershipClient;

/// Timeout for establishing new gRPC connections.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ConnectionPool {
    transports: DashMap<String, GrainTransportClient<tonic::transport::Channel>>,
    memberships: DashMap<String, MembershipClient<tonic::transport::Channel>>,
    gateways: DashMap<String, ClusterGatewayClient<tonic::transport::Channel>>,
    tls_config: Option<tonic::transport::ClientTlsConfig>,
    auth_token: Option<String>,
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
            gateways: DashMap::new(),
            tls_config: None,
            auth_token: None,
        }
    }

    pub fn with_tls(tls: tonic::transport::ClientTlsConfig) -> Self {
        Self {
            tls_config: Some(tls),
            ..Self::new()
        }
    }

    pub fn with_tls_and_auth(tls: tonic::transport::ClientTlsConfig, token: String) -> Self {
        Self {
            tls_config: Some(tls),
            auth_token: Some(token),
            ..Self::new()
        }
    }

    pub fn with_auth(token: String) -> Self {
        Self {
            auth_token: Some(token),
            ..Self::new()
        }
    }

    /// Wrap a value in a `tonic::Request` with the auth token attached (if configured).
    pub fn authorized_request<T>(&self, inner: T) -> tonic::Request<T> {
        let mut request = tonic::Request::new(inner);
        if let Some(ref token) = self.auth_token
            && let Ok(value) = token.parse()
        {
            request.metadata_mut().insert("authorization", value);
        }
        request
    }

    async fn connect_channel(&self, endpoint: &str) -> Result<tonic::transport::Channel, ClusterError> {
        let uri = if self.tls_config.is_some() {
            format!("https://{}", endpoint)
        } else {
            format!("http://{}", endpoint)
        };
        let mut ep = tonic::transport::Endpoint::from_shared(uri)
            .map_err(|e| ClusterError::Transport(e.to_string()))?
            .connect_timeout(CONNECT_TIMEOUT);

        if let Some(ref tls) = self.tls_config {
            ep = ep
                .tls_config(tls.clone())
                .map_err(|e| ClusterError::Transport(e.to_string()))?;
        }

        ep.connect()
            .await
            .map_err(|e| ClusterError::Transport(e.to_string()))
    }

    pub async fn get_transport(
        self: &Arc<Self>,
        endpoint: &str,
    ) -> Result<GrainTransportClient<tonic::transport::Channel>, ClusterError> {
        if let Some(client) = self.transports.get(endpoint) {
            return Ok(client.clone());
        }

        let channel = self.connect_channel(endpoint).await?;
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

        let channel = self.connect_channel(endpoint).await?;
        let client = MembershipClient::new(channel);

        self.memberships
            .insert(endpoint.to_string(), client.clone());
        Ok(client)
    }

    pub async fn get_gateway(
        self: &Arc<Self>,
        endpoint: &str,
    ) -> Result<ClusterGatewayClient<tonic::transport::Channel>, ClusterError> {
        if let Some(client) = self.gateways.get(endpoint) {
            return Ok(client.clone());
        }

        let channel = self.connect_channel(endpoint).await?;
        let client = ClusterGatewayClient::new(channel);

        self.gateways
            .insert(endpoint.to_string(), client.clone());
        Ok(client)
    }

    /// Remove cached connections for an endpoint.
    ///
    /// Called automatically when SWIM declares a member dead. Callers should
    /// also call this after persistent connection errors to force reconnection.
    ///
    /// Note: tonic `Channel` handles reconnection internally for transient
    /// failures — this method is for permanent removals (dead silos) or
    /// forcing a fresh connection after repeated errors.
    pub fn remove(&self, endpoint: &str) {
        self.transports.remove(endpoint);
        self.memberships.remove(endpoint);
        self.gateways.remove(endpoint);
    }
}
