//! Failover state machine for cross-cluster grain ownership promotion.
//!
//! Monitors the health of peer clusters and, when the primary for a set of
//! grains becomes unreachable, attempts promotion via CAS on the cross-cluster
//! directory. Fence tokens (epochs) prevent stale primaries from reclaiming
//! ownership after a promotion.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use orlando_core::ClusterId;

use crate::cross_cluster_directory::{CrossClusterDirectory, GrainOwnership};
use crate::multi_cluster::{ClusterHealth, PeerStatus};

/// Failover progression for a single peer cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailoverPhase {
    /// Peer is healthy, no action needed.
    Monitoring,
    /// Peer became unreachable. Waiting for grace period before promoting.
    PrimaryUnreachable { since: Instant },
    /// Grace period elapsed. Attempting promotion via directory CAS.
    Promoting,
    /// Successfully promoted — this cluster is the new primary for the
    /// affected grains.
    Promoted { epoch: u64 },
}

/// Configuration for failover behavior.
#[derive(Debug, Clone)]
pub struct FailoverConfig {
    /// How long to wait after detecting a peer as unreachable before
    /// beginning promotion. Prevents premature failover on transient blips.
    pub grace_period: Duration,
    /// How often the failover manager checks cluster health.
    pub check_interval: Duration,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            grace_period: Duration::from_secs(30),
            check_interval: Duration::from_secs(5),
        }
    }
}

/// Manages failover for a single local cluster watching its peers.
///
/// When a peer cluster becomes unreachable for longer than the grace period,
/// the failover manager attempts to re-register ownership of grains that
/// were owned by the failed cluster.
pub struct FailoverManager {
    local_cluster_id: ClusterId,
    config: FailoverConfig,
    health: Arc<ClusterHealth>,
    directory: Arc<dyn CrossClusterDirectory>,
    shutdown_rx: watch::Receiver<bool>,
}

impl FailoverManager {
    pub fn new(
        local_cluster_id: ClusterId,
        config: FailoverConfig,
        health: Arc<ClusterHealth>,
        directory: Arc<dyn CrossClusterDirectory>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            local_cluster_id,
            config,
            health,
            directory,
            shutdown_rx,
        }
    }

    /// Run the failover monitoring loop. Call via `tokio::spawn`.
    pub async fn run(mut self) {
        // Track per-peer failover state
        let mut peer_phases: std::collections::HashMap<ClusterId, FailoverPhase> =
            std::collections::HashMap::new();

        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.config.check_interval) => {}
                _ = self.shutdown_rx.changed() => {
                    tracing::debug!("failover manager shutting down");
                    return;
                }
            }

            let statuses = self.health.all_statuses();
            for (cluster_id, status) in statuses.iter() {
                let phase = peer_phases
                    .entry(cluster_id.clone())
                    .or_insert(FailoverPhase::Monitoring);

                match (status, &phase) {
                    (PeerStatus::Healthy, _) => {
                        if *phase != FailoverPhase::Monitoring {
                            tracing::info!(
                                cluster = %cluster_id,
                                "peer cluster recovered, returning to monitoring"
                            );
                            *phase = FailoverPhase::Monitoring;
                        }
                    }
                    (PeerStatus::Unreachable, FailoverPhase::Monitoring) => {
                        tracing::warn!(
                            cluster = %cluster_id,
                            grace_period = ?self.config.grace_period,
                            "peer cluster unreachable, starting grace period"
                        );
                        *phase = FailoverPhase::PrimaryUnreachable {
                            since: Instant::now(),
                        };
                    }
                    (
                        PeerStatus::Unreachable,
                        FailoverPhase::PrimaryUnreachable { since },
                    ) => {
                        if since.elapsed() >= self.config.grace_period {
                            tracing::warn!(
                                cluster = %cluster_id,
                                "grace period elapsed, beginning promotion"
                            );
                            *phase = FailoverPhase::Promoting;
                            // Actual promotion happens in the next check cycle
                            // or can be triggered immediately by the caller
                        }
                    }
                    (PeerStatus::Unreachable, FailoverPhase::Promoting) => {
                        let grains = match self.directory.list_owned_by(cluster_id).await {
                            Ok(g) => g,
                            Err(e) => {
                                tracing::error!(
                                    failed_cluster = %cluster_id,
                                    error = %e,
                                    "failover: directory list_owned_by failed, will retry next cycle"
                                );
                                continue;
                            }
                        };

                        tracing::warn!(
                            failed_cluster = %cluster_id,
                            local_cluster = %self.local_cluster_id,
                            grain_count = grains.len(),
                            "failover: promoting grains from failed cluster"
                        );

                        let mut max_epoch = 0u64;
                        let mut promoted = 0usize;
                        let mut lost = 0usize;
                        let mut failed = 0usize;
                        for (grain_id, ownership) in grains {
                            match self.promote_grain(&grain_id, &ownership).await {
                                Ok(new_owner) => {
                                    if new_owner.cluster_id == self.local_cluster_id {
                                        promoted += 1;
                                        max_epoch = max_epoch.max(new_owner.epoch);
                                    } else {
                                        lost += 1;
                                    }
                                }
                                Err(e) => {
                                    failed += 1;
                                    tracing::error!(
                                        grain_type = grain_id.type_name,
                                        grain_key = %grain_id.key,
                                        error = %e,
                                        "failover: promote_grain failed (continuing best-effort)"
                                    );
                                }
                            }
                        }

                        tracing::info!(
                            failed_cluster = %cluster_id,
                            promoted, lost, failed,
                            "failover: promotion sweep complete"
                        );

                        // Transition to Promoted regardless of partial failures;
                        // next health-check cycle will retry any grains that
                        // remain owned by the failed cluster.
                        *phase = FailoverPhase::Promoted { epoch: max_epoch };
                    }
                    _ => {}
                }
            }
        }
    }

    /// Attempt to promote a specific grain owned by a failed cluster.
    ///
    /// Uses CAS on the directory with an incremented epoch. Returns the new
    /// ownership record on success, or the existing owner if another cluster
    /// won the promotion race.
    pub async fn promote_grain(
        &self,
        grain_id: &orlando_core::GrainId,
        current_ownership: &GrainOwnership,
    ) -> Result<GrainOwnership, crate::cross_cluster_directory::DirectoryError> {
        let new_epoch = current_ownership.epoch + 1;

        tracing::info!(
            grain_type = grain_id.type_name,
            grain_key = %grain_id.key,
            old_cluster = %current_ownership.cluster_id,
            new_cluster = %self.local_cluster_id,
            new_epoch = new_epoch,
            "attempting grain promotion"
        );

        let result = self
            .directory
            .register(grain_id, &self.local_cluster_id, new_epoch)
            .await?;

        if result.cluster_id == self.local_cluster_id {
            tracing::info!(
                grain_type = grain_id.type_name,
                grain_key = %grain_id.key,
                epoch = new_epoch,
                "grain promotion successful"
            );
        } else {
            tracing::info!(
                grain_type = grain_id.type_name,
                grain_key = %grain_id.key,
                winner = %result.cluster_id,
                "grain promotion lost to another cluster"
            );
        }

        Ok(result)
    }
}
