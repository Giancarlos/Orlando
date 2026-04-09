use async_trait::async_trait;

use crate::error::ClusterError;
use crate::hash_ring::SiloAddress;

/// Trait for discovering cluster members.
///
/// Implementations provide a list of known silos that can be used as
/// seed nodes when joining a cluster, or as a full membership list
/// depending on the backend (static config, DNS, Consul, etc.).
#[async_trait]
pub trait MembershipProvider: Send + Sync + 'static {
    async fn get_members(&self) -> Result<Vec<SiloAddress>, ClusterError>;
}

/// Static seed-based discovery from a fixed list of addresses.
///
/// Useful for development, testing, or environments where silo
/// addresses are known ahead of time (e.g., from config files).
///
/// ```ignore
/// let provider = StaticSeedProvider::new(vec![
///     "127.0.0.1:5001",
///     "127.0.0.1:5002",
/// ]);
/// ```
#[derive(Debug, Clone)]
pub struct StaticSeedProvider {
    seeds: Vec<SiloAddress>,
}

impl StaticSeedProvider {
    pub fn new(addresses: Vec<impl AsRef<str>>) -> Self {
        let seeds = addresses
            .iter()
            .filter_map(|addr| {
                let s = addr.as_ref();
                let (host, port_str) = s.rsplit_once(':')?;
                let port = port_str.parse::<u16>().ok()?;
                Some(SiloAddress {
                    host: host.to_string(),
                    port,
                    silo_id: s.to_string(),
                })
            })
            .collect();

        Self { seeds }
    }
}

#[async_trait]
impl MembershipProvider for StaticSeedProvider {
    async fn get_members(&self) -> Result<Vec<SiloAddress>, ClusterError> {
        Ok(self.seeds.clone())
    }
}

/// DNS-based service discovery.
///
/// Resolves a DNS hostname to discover silo addresses. All resolved
/// IPs use the same configured port. Suitable for environments where
/// DNS SRV records or round-robin A records are managed externally
/// (e.g., Kubernetes headless services).
///
/// ```ignore
/// let provider = DnsMembershipProvider::new("orlando-silos.default.svc.cluster.local", 5001);
/// ```
#[derive(Debug, Clone)]
pub struct DnsMembershipProvider {
    hostname: String,
    port: u16,
}

impl DnsMembershipProvider {
    pub fn new(hostname: impl Into<String>, port: u16) -> Self {
        Self {
            hostname: hostname.into(),
            port,
        }
    }
}

#[async_trait]
impl MembershipProvider for DnsMembershipProvider {
    async fn get_members(&self) -> Result<Vec<SiloAddress>, ClusterError> {
        let resolved = tokio::net::lookup_host(format!("{}:{}", self.hostname, self.port))
            .await
            .map_err(|e| ClusterError::Transport(format!("DNS lookup failed: {}", e)))?;

        let members = resolved
            .map(|addr| SiloAddress {
                host: addr.ip().to_string(),
                port: addr.port(),
                silo_id: format!("{}:{}", addr.ip(), addr.port()),
            })
            .collect();

        Ok(members)
    }
}

/// Consul-based service discovery.
///
/// Discovers silos via the Consul service catalog, querying only
/// healthy service instances. Works with Consul's health checking
/// to automatically exclude failed nodes.
///
/// Enable with the `consul` feature flag.
///
/// ```ignore
/// let provider = ConsulMembershipProvider::new(
///     "http://localhost:8500",
///     "orlando-silo",
/// );
/// ```
#[cfg(feature = "consul")]
#[derive(Debug, Clone)]
pub struct ConsulMembershipProvider {
    consul_url: String,
    service_name: String,
}

#[cfg(feature = "consul")]
impl ConsulMembershipProvider {
    pub fn new(consul_url: impl Into<String>, service_name: impl Into<String>) -> Self {
        Self {
            consul_url: consul_url.into(),
            service_name: service_name.into(),
        }
    }
}

#[cfg(feature = "consul")]
#[async_trait]
impl MembershipProvider for ConsulMembershipProvider {
    async fn get_members(&self) -> Result<Vec<SiloAddress>, ClusterError> {
        let url = format!(
            "{}/v1/health/service/{}?passing=true",
            self.consul_url.trim_end_matches('/'),
            self.service_name,
        );

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| ClusterError::Transport(format!("Consul request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(ClusterError::Transport(format!(
                "Consul returned status {}",
                response.status()
            )));
        }

        // Consul /v1/health/service returns an array of service entries.
        // Each entry has Node.Address, Service.Address, Service.Port, Service.ID.
        // Service.Address may be empty; fall back to Node.Address.
        let entries: Vec<serde_json::Value> = response
            .json()
            .await
            .map_err(|e| ClusterError::Transport(format!("Consul response parse failed: {}", e)))?;

        let members = entries
            .iter()
            .filter_map(|entry| {
                let address = entry["Service"]["Address"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| entry["Node"]["Address"].as_str())?;
                let port = entry["Service"]["Port"].as_u64()? as u16;
                let service_id = entry["Service"]["ID"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();

                Some(SiloAddress {
                    host: address.to_string(),
                    port,
                    silo_id: if service_id.is_empty() {
                        format!("{}:{}", address, port)
                    } else {
                        service_id
                    },
                })
            })
            .collect();

        Ok(members)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_seed_provider_parses_addresses() {
        let provider = StaticSeedProvider::new(vec!["127.0.0.1:5001", "10.0.0.2:5002"]);
        let members = provider.get_members().await.unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].host, "127.0.0.1");
        assert_eq!(members[0].port, 5001);
        assert_eq!(members[1].host, "10.0.0.2");
        assert_eq!(members[1].port, 5002);
    }

    #[tokio::test]
    async fn static_seed_provider_skips_invalid() {
        let provider = StaticSeedProvider::new(vec!["valid:1234", "no-port", "also:bad"]);
        let members = provider.get_members().await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].host, "valid");
        assert_eq!(members[0].port, 1234);
    }

    #[tokio::test]
    async fn static_seed_provider_empty() {
        let provider = StaticSeedProvider::new(Vec::<&str>::new());
        let members = provider.get_members().await.unwrap();
        assert!(members.is_empty());
    }
}
