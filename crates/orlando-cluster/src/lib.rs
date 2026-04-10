mod auth;
mod cluster_gateway;
mod cluster_grain_ref;
mod cluster_silo;
mod connection_pool;
mod cross_cluster_directory;
pub mod discovery;
mod error;
mod failure_detector;
mod hash_ring;
mod membership;
mod message_registry;
mod multi_cluster;
mod network_message;
mod placement;
mod rebalancer;
pub(crate) mod swim;
mod retry;
mod transport;

pub mod proto {
    tonic::include_proto!("orlando.cluster");
}

pub use auth::{ClusterAuth, SharedSecretAuth};
pub use cluster_gateway::ClusterGatewayService;
pub use cluster_grain_ref::ClusterGrainRef;
pub use cluster_silo::{ClusterSilo, ClusterSiloBuilder};
pub use orlando_core::ClusterId;
pub use connection_pool::ConnectionPool;
pub use cross_cluster_directory::{
    CrossClusterDirectory, DirectoryError, GrainOwnership, InMemoryCrossClusterDirectory,
};
pub use error::ClusterError;
pub use failure_detector::{FailureDetector, FailureDetectorConfig, MembershipChange};
pub use hash_ring::{HashRing, SiloAddress};
pub use membership::MembershipService;
pub use message_registry::MessageRegistry;
pub use multi_cluster::{ClusterHealth, MultiClusterConfig, PeerStatus};
pub use network_message::{Encoding, NetworkMessage};
pub use placement::{HashBasedPlacement, PlacementStrategy, PreferLocalPlacement, RandomPlacement};
pub use discovery::{DnsMembershipProvider, MembershipProvider, StaticSeedProvider};
#[cfg(feature = "consul")]
pub use discovery::ConsulMembershipProvider;
pub use rebalancer::Rebalancer;
pub use retry::RetryPolicy;
pub use transport::GrainTransportService;
