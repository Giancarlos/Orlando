use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::{broadcast, mpsc, watch};

use orlando_core::{ClusterId, Grain, GrainActivator, GrainHandler, GrainId, mailbox, reentrant_mailbox};
use orlando_runtime::GrainDirectory;

use crate::cluster_gateway::ClusterGatewayService;
use crate::cluster_grain_ref::ClusterGrainRef;
use crate::connection_pool::ConnectionPool;
use crate::cross_cluster_directory::CrossClusterDirectory;
use crate::error::ClusterError;
use crate::failure_detector::{FailureDetector, FailureDetectorConfig, MembershipChange};
use crate::hash_ring::{HashRing, SiloAddress};
use crate::placement::{HashBasedPlacement, PlacementStrategy};
use crate::membership::MembershipService;
use crate::message_registry::MessageRegistry;
use crate::network_message::NetworkMessage;
use crate::proto::cluster_gateway_server::ClusterGatewayServer;
use crate::proto::grain_transport_server::GrainTransportServer;
use crate::proto::membership_server::MembershipServer;
use crate::proto::{JoinRequest, NotifyJoinRequest, SiloAddress as ProtoSiloAddress};
use crate::rebalancer::Rebalancer;
use crate::transport::GrainTransportService;

pub struct ClusterSilo {
    local_addr: SiloAddress,
    directory: Arc<GrainDirectory>,
    registry: Arc<MessageRegistry>,
    ring: Arc<RwLock<HashRing>>,
    pool: Arc<ConnectionPool>,
    change_tx: broadcast::Sender<MembershipChange>,
    failure_detector_config: FailureDetectorConfig,
    shutdown_tx: watch::Sender<bool>,
    swim_state: Arc<tokio::sync::Mutex<crate::swim::SwimState>>,
    placement: Arc<dyn PlacementStrategy>,
    // Cross-cluster (GSI) fields -- None when multi-cluster is not configured
    cross_cluster_dir: Option<Arc<dyn CrossClusterDirectory>>,
    local_cluster_id: Option<ClusterId>,
    peer_endpoints: Option<Arc<HashMap<ClusterId, String>>>,
}

impl ClusterSilo {
    pub fn builder() -> ClusterSiloBuilder {
        ClusterSiloBuilder::new()
    }

    pub fn directory(&self) -> &Arc<GrainDirectory> {
        &self.directory
    }

    pub fn local_addr(&self) -> &SiloAddress {
        &self.local_addr
    }

    pub fn pool(&self) -> &Arc<ConnectionPool> {
        &self.pool
    }

    /// Signal the gRPC server and SWIM task to shut down.
    /// Does NOT drain active grains — use `shutdown_and_drain()` for graceful shutdown.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Gracefully shut down: drain all active grains (triggering on_deactivate
    /// and state persistence), then stop the gRPC server and SWIM task.
    pub async fn shutdown_and_drain(&self) {
        tracing::info!("starting graceful shutdown");
        self.directory.drain().await;
        let _ = self.shutdown_tx.send(true);
        tracing::info!("graceful shutdown complete");
    }

    /// Get a cluster-aware grain reference.
    ///
    /// Uses the hash ring to determine whether the grain lives on this silo
    /// (local dispatch) or a remote silo (gRPC dispatch via connection pool).
    pub fn get_ref<G: Grain>(&self, key: impl Into<String>) -> ClusterGrainRef<G> {
        let key = key.into();
        let grain_type = G::grain_type_name();

        let ring = self.ring.read().expect("ring lock poisoned");
        let target = self.placement.place(grain_type, &key, &self.local_addr.silo_id, &ring);
        drop(ring);

        match target {
            Some(ref t) if t.silo_id == self.local_addr.silo_id => {
                let sender = self.local_activate::<G>(&key);
                ClusterGrainRef::local(sender)
            }
            Some(t) => {
                ClusterGrainRef::remote(
                    t.endpoint(),
                    grain_type,
                    key,
                    self.pool.clone(),
                )
            }
            None => {
                let sender = self.local_activate::<G>(&key);
                ClusterGrainRef::local(sender)
            }
        }
    }

    fn local_activate<G: Grain>(
        &self,
        key: &str,
    ) -> mpsc::Sender<orlando_core::Envelope> {
        let grain_id = GrainId {
            type_name: G::grain_type_name(),
            key: key.to_string(),
        };
        let activator: Arc<dyn GrainActivator> = self.directory.clone();
        let activator_for_mailbox = activator.clone();
        activator.get_or_insert(
            grain_id,
            Box::new(move |id| {
                let (tx, rx) = mpsc::channel(orlando_core::MAILBOX_CAPACITY);
                let task = if G::reentrant() {
                    tokio::spawn(async move {
                        reentrant_mailbox::run_reentrant_mailbox::<G>(
                            id, rx, activator_for_mailbox,
                        )
                        .await;
                    })
                } else {
                    tokio::spawn(async move {
                        mailbox::run_mailbox::<G>(id, rx, activator_for_mailbox).await;
                    })
                };
                (tx, task)
            }),
        )
    }

    /// Join an existing cluster by contacting a seed silo.
    ///
    /// After the seed responds with its member list, this silo announces itself
    /// to every other member via `NotifyJoin` so the entire cluster learns about
    /// the new node (gossip propagation).
    pub async fn join_cluster(&self, seed_addr: &str) -> Result<(), ClusterError> {
        let mut client = self.pool.get_membership(seed_addr).await?;

        let self_proto = ProtoSiloAddress {
            host: self.local_addr.host.clone(),
            port: self.local_addr.port as u32,
            silo_id: self.local_addr.silo_id.clone(),
        };

        let response = client
            .join(JoinRequest {
                joiner: Some(self_proto.clone()),
            })
            .await
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        let members: Vec<SiloAddress> = response
            .into_inner()
            .members
            .into_iter()
            .map(|m| SiloAddress {
                host: m.host,
                port: m.port as u16,
                silo_id: m.silo_id,
            })
            .collect();

        // Add all members to our ring
        {
            let mut ring = self
                .ring
                .write()
                .map_err(|e| ClusterError::Transport(e.to_string()))?;

            for silo in &members {
                ring.add(silo.clone());
                let _ = self.change_tx.send(MembershipChange::SiloJoined(silo.clone()));
            }
        }

        // Add members to SWIM state
        {
            let mut swim = self.swim_state.lock().await;
            for silo in &members {
                if silo.silo_id != self.local_addr.silo_id {
                    swim.members.insert(
                        silo.silo_id.clone(),
                        crate::swim::SwimMember {
                            addr: silo.clone(),
                            status: crate::swim::MemberStatus::Alive,
                            incarnation: 0,
                        },
                    );
                }
            }
        }

        // Announce ourselves to every peer (except the seed, which already knows)
        for member in &members {
            if member.silo_id == self.local_addr.silo_id {
                continue;
            }
            if member.endpoint() == seed_addr {
                continue; // seed already processed our Join
            }

            let result = self.pool.get_membership(&member.endpoint()).await;
            if let Ok(mut peer) = result {
                let _ = peer
                    .notify_join(NotifyJoinRequest {
                        silo: Some(self_proto.clone()),
                    })
                    .await;
            }
        }

        Ok(())
    }

    /// Start serving gRPC (grain transport + membership + optional gateway) and
    /// background tasks (failure detector + rebalancer).
    pub async fn serve(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        let addr: SocketAddr = format!("{}:{}", self.local_addr.host, self.local_addr.port)
            .parse()?;

        let mut transport = GrainTransportService::new(
            self.registry.clone(),
            self.directory.clone() as Arc<dyn GrainActivator>,
            self.ring.clone(),
            self.pool.clone(),
            self.local_addr.silo_id.clone(),
        );

        // Wire in cross-cluster forwarding if multi-cluster is configured
        if let (Some(dir), Some(cid), Some(peers)) = (
            &self.cross_cluster_dir,
            &self.local_cluster_id,
            &self.peer_endpoints,
        ) {
            transport = transport.with_cross_cluster(dir.clone(), cid.clone(), peers.clone());
        }

        // Create failure detector using the shared SWIM state
        let detector = FailureDetector::with_state(
            self.failure_detector_config.clone(),
            self.ring.clone(),
            self.pool.clone(),
            self.change_tx.clone(),
            self.swim_state.clone(),
            self.shutdown_tx.subscribe(),
        );

        let swim_state = self.swim_state.clone();

        let membership = MembershipService::new(
            self.ring.clone(),
            self.local_addr.clone(),
            self.change_tx.clone(),
            self.pool.clone(),
            swim_state,
            self.failure_detector_config.gossip_fanout,
        );

        tokio::spawn(detector.run());

        // Spawn rebalancer
        let rebalancer = Rebalancer::new(
            self.ring.clone(),
            self.directory.clone(),
            self.local_addr.silo_id.clone(),
            self.change_tx.subscribe(),
        );
        tokio::spawn(rebalancer.run());

        tracing::info!(%addr, "cluster silo listening");

        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_signal = async move {
            let _ = shutdown_rx.changed().await;
        };

        let mut server = tonic::transport::Server::builder();

        // Add the optional ClusterGateway service for cross-cluster forwarding
        if let Some(ref cluster_id) = self.local_cluster_id {
            let gateway = ClusterGatewayService::new(
                self.registry.clone(),
                self.directory.clone() as Arc<dyn GrainActivator>,
                cluster_id.to_string(),
            );
            server
                .add_service(GrainTransportServer::new(transport))
                .add_service(MembershipServer::new(membership))
                .add_service(ClusterGatewayServer::new(gateway))
                .serve_with_shutdown(addr, shutdown_signal)
                .await?;
        } else {
            server
                .add_service(GrainTransportServer::new(transport))
                .add_service(MembershipServer::new(membership))
                .serve_with_shutdown(addr, shutdown_signal)
                .await?;
        }

        Ok(())
    }
}

pub struct ClusterSiloBuilder {
    host: String,
    port: u16,
    silo_id: Option<String>,
    registry: MessageRegistry,
    virtual_nodes: u32,
    failure_detector_config: FailureDetectorConfig,
    placement: Option<Arc<dyn PlacementStrategy>>,
    cross_cluster_dir: Option<Arc<dyn CrossClusterDirectory>>,
    local_cluster_id: Option<ClusterId>,
    peer_endpoints: Option<HashMap<ClusterId, String>>,
}

impl ClusterSiloBuilder {
    fn new() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 0,
            silo_id: None,
            registry: MessageRegistry::new(),
            virtual_nodes: 150,
            failure_detector_config: FailureDetectorConfig::default(),
            placement: None,
            cross_cluster_dir: None,
            local_cluster_id: None,
            peer_endpoints: None,
        }
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn silo_id(mut self, id: impl Into<String>) -> Self {
        self.silo_id = Some(id.into());
        self
    }

    pub fn virtual_nodes(mut self, n: u32) -> Self {
        self.virtual_nodes = n;
        self
    }

    pub fn failure_detector_config(mut self, config: FailureDetectorConfig) -> Self {
        self.failure_detector_config = config;
        self
    }

    /// Set the grain placement strategy. Defaults to `HashBasedPlacement`.
    pub fn placement(mut self, strategy: Arc<dyn PlacementStrategy>) -> Self {
        self.placement = Some(strategy);
        self
    }

    /// Set the cross-cluster directory for GSI (Global Single Instance) support.
    pub fn cross_cluster_directory(mut self, dir: Arc<dyn CrossClusterDirectory>) -> Self {
        self.cross_cluster_dir = Some(dir);
        self
    }

    /// Set the local cluster ID for multi-cluster deployments.
    pub fn cluster_id(mut self, id: impl Into<ClusterId>) -> Self {
        self.local_cluster_id = Some(id.into());
        self
    }

    /// Add a peer cluster endpoint for cross-cluster forwarding.
    pub fn peer_cluster(mut self, cluster_id: impl Into<ClusterId>, endpoint: impl Into<String>) -> Self {
        self.peer_endpoints
            .get_or_insert_with(HashMap::new)
            .insert(cluster_id.into(), endpoint.into());
        self
    }

    /// Register a grain + message type for remote dispatch on this silo.
    pub fn register<G, M>(mut self) -> Self
    where
        G: GrainHandler<M> + Sync,
        M: NetworkMessage,
        M::Result: Serialize + DeserializeOwned,
    {
        self.registry.register::<G, M>();
        self
    }

    pub fn build(self) -> ClusterSilo {
        let silo_id = self
            .silo_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let local_addr = SiloAddress {
            host: self.host,
            port: self.port,
            silo_id,
        };

        let mut ring = HashRing::new(self.virtual_nodes);
        ring.add(local_addr.clone());

        let (change_tx, _) = broadcast::channel(64);
        let (shutdown_tx, _) = watch::channel(false);

        let swim_state = Arc::new(tokio::sync::Mutex::new(
            crate::swim::SwimState::new(local_addr.clone()),
        ));

        let placement = self
            .placement
            .unwrap_or_else(|| Arc::new(HashBasedPlacement));

        ClusterSilo {
            local_addr,
            directory: Arc::new(GrainDirectory::new()),
            registry: Arc::new(self.registry),
            ring: Arc::new(RwLock::new(ring)),
            pool: Arc::new(ConnectionPool::new()),
            change_tx,
            failure_detector_config: self.failure_detector_config,
            shutdown_tx,
            swim_state,
            placement,
            cross_cluster_dir: self.cross_cluster_dir,
            local_cluster_id: self.local_cluster_id,
            peer_endpoints: self.peer_endpoints.map(Arc::new),
        }
    }
}
