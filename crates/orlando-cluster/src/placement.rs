use crate::hash_ring::{HashRing, SiloAddress};

/// Strategy for determining which silo should host a grain activation.
pub trait PlacementStrategy: Send + Sync + 'static {
    /// Given a grain type and key, return which silo should own it.
    /// Returns `None` if no silo is available (empty cluster).
    fn place(
        &self,
        grain_type: &str,
        grain_key: &str,
        local_silo_id: &str,
        ring: &HashRing,
    ) -> Option<SiloAddress>;
}

/// Default strategy: consistent hashing via the hash ring.
/// This is what Orleans uses for most grains.
#[derive(Debug)]
pub struct HashBasedPlacement;

impl PlacementStrategy for HashBasedPlacement {
    fn place(
        &self,
        grain_type: &str,
        grain_key: &str,
        _local_silo_id: &str,
        ring: &HashRing,
    ) -> Option<SiloAddress> {
        let ring_key = format!("{}/{}", grain_type, grain_key);
        ring.get(&ring_key).cloned()
    }
}

/// Prefer-local placement: always activate grains on the silo that receives
/// the first call. Useful for cache grains or compute-local workloads.
#[derive(Debug)]
pub struct PreferLocalPlacement;

impl PlacementStrategy for PreferLocalPlacement {
    fn place(
        &self,
        _grain_type: &str,
        _grain_key: &str,
        local_silo_id: &str,
        ring: &HashRing,
    ) -> Option<SiloAddress> {
        ring.members()
            .into_iter()
            .find(|s| s.silo_id == local_silo_id)
    }
}

/// Random placement: distribute grains randomly across the cluster.
/// Useful for stateless compute grains where you want even load distribution
/// without the determinism of consistent hashing.
#[derive(Debug)]
pub struct RandomPlacement;

impl PlacementStrategy for RandomPlacement {
    fn place(
        &self,
        _grain_type: &str,
        _grain_key: &str,
        _local_silo_id: &str,
        ring: &HashRing,
    ) -> Option<SiloAddress> {
        let members = ring.members();
        if members.is_empty() {
            return None;
        }
        let idx = fastrand::usize(..members.len());
        Some(members[idx].clone())
    }
}
