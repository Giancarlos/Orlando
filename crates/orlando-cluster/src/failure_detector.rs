use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::broadcast;

use crate::connection_pool::ConnectionPool;
use crate::hash_ring::{HashRing, SiloAddress};
use crate::swim::{self, SwimState};

/// A membership change event broadcast to subscribers (e.g., the rebalancer).
#[derive(Clone, Debug)]
pub enum MembershipChange {
    SiloJoined(SiloAddress),
    SiloLeft(SiloAddress),
}

/// Configuration for the SWIM-based failure detector.
///
/// Replaces the old simple ping-count model with the full SWIM protocol:
/// protocol periods, indirect pings, suspicion with timeout, and gossip
/// piggybacking.
#[derive(Clone, Debug)]
pub struct FailureDetectorConfig {
    /// How often the protocol runs a probe cycle.
    pub protocol_period: Duration,
    /// Timeout for a direct ping response.
    pub ping_timeout: Duration,
    /// Number of random peers to ask for an indirect ping when a direct ping fails.
    pub ping_req_count: usize,
    /// How long a member stays in "suspect" state before being declared dead.
    pub suspect_timeout: Duration,
    /// Maximum number of gossip updates piggybacked per protocol message.
    pub gossip_fanout: usize,
}

impl Default for FailureDetectorConfig {
    fn default() -> Self {
        Self {
            protocol_period: Duration::from_secs(2),
            ping_timeout: Duration::from_secs(1),
            ping_req_count: 3,
            suspect_timeout: Duration::from_secs(5),
            gossip_fanout: 6,
        }
    }
}

impl FailureDetectorConfig {
    /// Migrate from the pre-SWIM config fields.
    ///
    /// Maps old field names to the SWIM equivalents:
    /// - `ping_interval` -> `protocol_period`
    /// - `ping_timeout` -> `ping_timeout` (unchanged)
    /// - `max_missed_pings` -> `suspect_timeout` (computed as `ping_interval * max_missed_pings`)
    ///
    /// New SWIM-specific fields (`ping_req_count`, `gossip_fanout`) use defaults.
    pub fn from_legacy(
        ping_interval: Duration,
        ping_timeout: Duration,
        max_missed_pings: u32,
    ) -> Self {
        Self {
            protocol_period: ping_interval,
            ping_timeout,
            suspect_timeout: ping_interval * max_missed_pings,
            ..Default::default()
        }
    }
}

/// SWIM-based failure detector. Runs as a background task that periodically
/// probes random cluster members using direct pings, indirect pings, and
/// a suspicion mechanism before declaring members dead.
pub struct FailureDetector {
    config: FailureDetectorConfig,
    ring: Arc<RwLock<HashRing>>,
    pool: Arc<ConnectionPool>,
    change_tx: broadcast::Sender<MembershipChange>,
    swim_state: Arc<tokio::sync::Mutex<SwimState>>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl FailureDetector {
    pub fn new(
        config: FailureDetectorConfig,
        ring: Arc<RwLock<HashRing>>,
        pool: Arc<ConnectionPool>,
        local_addr: SiloAddress,
        change_tx: broadcast::Sender<MembershipChange>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        let swim_state = Arc::new(tokio::sync::Mutex::new(SwimState::new(local_addr)));
        Self {
            config,
            ring,
            pool,
            change_tx,
            swim_state,
            shutdown_rx,
        }
    }

    /// Create with an existing shared SWIM state.
    pub fn with_state(
        config: FailureDetectorConfig,
        ring: Arc<RwLock<HashRing>>,
        pool: Arc<ConnectionPool>,
        change_tx: broadcast::Sender<MembershipChange>,
        swim_state: Arc<tokio::sync::Mutex<SwimState>>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            config,
            ring,
            pool,
            change_tx,
            swim_state,
            shutdown_rx,
        }
    }

    /// Get a reference to the shared SWIM state for use by the membership service.
    pub fn swim_state(&self) -> Arc<tokio::sync::Mutex<SwimState>> {
        self.swim_state.clone()
    }

    /// Run the SWIM protocol loop (consumes self).
    pub async fn run(self) {
        swim::run_swim_protocol(
            self.config,
            self.swim_state,
            self.ring,
            self.pool,
            self.change_tx,
            self.shutdown_rx,
        )
        .await;
    }
}
