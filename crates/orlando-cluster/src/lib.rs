mod cluster_gateway;
mod cluster_grain_ref;
mod cluster_silo;
mod connection_pool;
mod cross_cluster_directory;
mod error;
mod failure_detector;
mod hash_ring;
mod membership;
mod message_registry;
mod network_message;
mod placement;
mod rebalancer;
pub(crate) mod swim;
mod transport;

pub mod proto {
    tonic::include_proto!("orlando.cluster");
}

pub use cluster_gateway::ClusterGatewayService;
pub use cluster_grain_ref::ClusterGrainRef;
pub use cluster_silo::{ClusterSilo, ClusterSiloBuilder};
pub use connection_pool::ConnectionPool;
pub use cross_cluster_directory::{
    CrossClusterDirectory, DirectoryError, GrainOwnership, InMemoryCrossClusterDirectory,
};
pub use error::ClusterError;
pub use failure_detector::{FailureDetector, FailureDetectorConfig, MembershipChange};
pub use hash_ring::{HashRing, SiloAddress};
pub use membership::MembershipService;
pub use message_registry::MessageRegistry;
pub use network_message::{Encoding, NetworkMessage};
pub use orlando_core::ClusterId;
pub use placement::{HashBasedPlacement, PlacementStrategy, PreferLocalPlacement, RandomPlacement};
pub use rebalancer::Rebalancer;
pub use transport::GrainTransportService;
