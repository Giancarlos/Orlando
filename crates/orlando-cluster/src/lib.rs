mod cluster_grain_ref;
mod cluster_silo;
mod error;
mod hash_ring;
mod membership;
mod message_registry;
mod network_message;
mod transport;

pub mod proto {
    tonic::include_proto!("orlando.cluster");
}

pub use cluster_grain_ref::ClusterGrainRef;
pub use cluster_silo::{ClusterSilo, ClusterSiloBuilder};
pub use error::ClusterError;
pub use hash_ring::{HashRing, SiloAddress};
pub use membership::MembershipService;
pub use message_registry::MessageRegistry;
pub use network_message::NetworkMessage;
pub use transport::GrainTransportService;
