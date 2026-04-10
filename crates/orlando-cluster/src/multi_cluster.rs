use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::sync::watch;

use orlando_core::ClusterId;

use crate::connection_pool::ConnectionPool;

/// Configuration for a multi-cluster deployment.
#[derive(Debug, Clone)]
pub struct MultiClusterConfig {
    /// This cluster's identity.
    pub cluster_id: ClusterId,
    /// Gateway endpoints of peer clusters (cluster_id -> "host:port").
    pub peers: HashMap<ClusterId, String>,
    /// How often to health-check peer clusters.
    pub health_check_interval: Duration,
}

impl MultiClusterConfig {
    pub fn new(cluster_id: impl Into<String>) -> Self {
        Self {
            cluster_id: ClusterId::new(cluster_id),
            peers: HashMap::new(),
            health_check_interval: Duration::from_secs(10),
        }
    }

    pub fn peer(mut self, cluster_id: impl Into<String>, endpoint: impl Into<String>) -> Self {
        self.peers
            .insert(ClusterId::new(cluster_id), endpoint.into());
        self
    }

    pub fn health_check_interval(mut self, interval: Duration) -> Self {
        self.health_check_interval = interval;
        self
    }
}

/// Status of a peer cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerStatus {
    Healthy,
    Unreachable,
    Unknown,
}

/// Tracks health of peer clusters via periodic gRPC probes.
pub struct ClusterHealth {
    config: MultiClusterConfig,
    pool: Arc<ConnectionPool>,
    statuses: Arc<ArcSwap<HashMap<ClusterId, PeerStatus>>>,
    shutdown_rx: watch::Receiver<bool>,
}

impl ClusterHealth {
    pub fn new(
        config: MultiClusterConfig,
        pool: Arc<ConnectionPool>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        let initial: HashMap<ClusterId, PeerStatus> = config
            .peers
            .keys()
            .map(|id| (id.clone(), PeerStatus::Unknown))
            .collect();

        Self {
            config,
            pool,
            statuses: Arc::new(ArcSwap::from_pointee(initial)),
            shutdown_rx,
        }
    }

    /// Get the current status of a peer cluster.
    pub fn status(&self, cluster_id: &ClusterId) -> PeerStatus {
        self.statuses
            .load()
            .get(cluster_id)
            .cloned()
            .unwrap_or(PeerStatus::Unknown)
    }

    /// Get a snapshot of all peer statuses.
    pub fn all_statuses(&self) -> Arc<HashMap<ClusterId, PeerStatus>> {
        self.statuses.load_full()
    }

    /// Get the endpoint for a peer cluster.
    pub fn peer_endpoint(&self, cluster_id: &ClusterId) -> Option<String> {
        self.config.peers.get(cluster_id).cloned()
    }

    /// Run the health check loop (call via `tokio::spawn`).
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.config.health_check_interval) => {}
                _ = self.shutdown_rx.changed() => {
                    tracing::debug!("cluster health checker shutting down");
                    return;
                }
            }

            let mut new_statuses = HashMap::new();
            for (cluster_id, endpoint) in &self.config.peers {
                let status = match self.pool.get_membership(endpoint).await {
                    Ok(mut client) => {
                        match tokio::time::timeout(
                            Duration::from_secs(5),
                            client.get_members(crate::proto::GetMembersRequest {}),
                        )
                        .await
                        {
                            Ok(Ok(_)) => PeerStatus::Healthy,
                            _ => PeerStatus::Unreachable,
                        }
                    }
                    Err(_) => PeerStatus::Unreachable,
                };

                if status != self.status(cluster_id) {
                    tracing::info!(
                        cluster = %cluster_id,
                        ?status,
                        "peer cluster status changed"
                    );
                }
                new_statuses.insert(cluster_id.clone(), status);
            }
            self.statuses.store(Arc::new(new_statuses));
        }
    }
}
