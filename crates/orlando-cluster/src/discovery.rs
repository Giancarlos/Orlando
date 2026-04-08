use async_trait::async_trait;

use crate::error::ClusterError;
use crate::hash_ring::SiloAddress;

/// Pluggable cluster membership discovery.
///
/// Implementations provide the mechanism for silos to discover each other.
/// The default is `StaticSeedProvider` (hardcoded addresses). For Kubernetes,
/// use `DnsMembershipProvider` with a headless service.
#[async_trait]
pub trait MembershipProvider: Send + Sync + 'static {
    /// Discover current cluster members.
    async fn get_members(&self) -> Result<Vec<SiloAddress>, ClusterError>;
}

/// Static seed-based discovery. Connects to a fixed list of seed addresses.
/// This is the default — equivalent to the current `join_cluster(seed_addr)`.
///
/// Seeds are connection targets for joining, not full member descriptions.
/// The `silo_id` is left empty — the real silo_id is discovered during
/// the join handshake.
#[derive(Debug, Clone)]
pub struct StaticSeedProvider {
    seeds: Vec<String>,
}

impl StaticSeedProvider {
    pub fn new(seeds: Vec<String>) -> Self {
        Self { seeds }
    }

    pub fn single(seed: impl Into<String>) -> Self {
        Self {
            seeds: vec![seed.into()],
        }
    }
}

#[async_trait]
impl MembershipProvider for StaticSeedProvider {
    async fn get_members(&self) -> Result<Vec<SiloAddress>, ClusterError> {
        // Static seeds are connection targets only — silo_id is discovered
        // during the join handshake, so we use the endpoint as a placeholder.
        Ok(self
            .seeds
            .iter()
            .filter_map(|s| {
                let parts: Vec<&str> = s.rsplitn(2, ':').collect();
                if parts.len() == 2 {
                    let port = parts[0].parse::<u16>().ok()?;
                    let host = parts[1].to_string();
                    Some(SiloAddress {
                        host,
                        port,
                        // Placeholder — real silo_id comes from the Join response.
                        // discover_and_join() uses endpoint() for connection, not silo_id.
                        silo_id: String::new(),
                    })
                } else {
                    None
                }
            })
            .collect())
    }
}

/// DNS-based discovery for Kubernetes headless services.
///
/// Resolves a DNS hostname to discover all pod IPs. Works with Kubernetes
/// headless services where the DNS record returns all pod addresses.
///
/// ```ignore
/// // In Kubernetes, create a headless service:
/// // apiVersion: v1
/// // kind: Service
/// // metadata:
/// //   name: orlando-silo
/// // spec:
/// //   clusterIP: None
/// //   selector:
/// //     app: orlando
/// //   ports:
/// //     - port: 5001
///
/// let provider = DnsMembershipProvider::new("orlando-silo.default.svc.cluster.local", 5001);
/// let silo = ClusterSilo::builder()
///     .membership_provider(Arc::new(provider))
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct DnsMembershipProvider {
    service_name: String,
    port: u16,
}

impl DnsMembershipProvider {
    pub fn new(service_name: impl Into<String>, port: u16) -> Self {
        Self {
            service_name: service_name.into(),
            port,
        }
    }
}

#[async_trait]
impl MembershipProvider for DnsMembershipProvider {
    async fn get_members(&self) -> Result<Vec<SiloAddress>, ClusterError> {
        let lookup = format!("{}:{}", self.service_name, self.port);
        let addrs = tokio::net::lookup_host(&lookup)
            .await
            .map_err(|e| ClusterError::Transport(format!("DNS lookup failed for {}: {}", self.service_name, e)))?;

        Ok(addrs
            .map(|addr| SiloAddress {
                host: addr.ip().to_string(),
                port: self.port,
                silo_id: format!("{}:{}", addr.ip(), self.port),
            })
            .collect())
    }
}
