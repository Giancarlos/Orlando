use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use arc_swap::ArcSwap;

use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::{broadcast, mpsc, watch};

use orlando_core::{ClusterId, Grain, GrainActivator, GrainHandler, GrainId, mailbox, reentrant_mailbox};
use orlando_runtime::GrainDirectory;

use crate::failover::{FailoverConfig, FailoverManager};
use crate::multi_cluster::{ClusterHealth, MultiClusterConfig};

use crate::cluster_gateway::ClusterGatewayService;
use crate::cluster_grain_ref::ClusterGrainRef;
use crate::connection_pool::ConnectionPool;
use crate::cross_cluster_directory::CrossClusterDirectory;
use crate::error::ClusterError;
use crate::retry::RetryPolicy;
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
    ring: Arc<ArcSwap<HashRing>>,
    pool: Arc<ConnectionPool>,
    change_tx: broadcast::Sender<MembershipChange>,
    failure_detector_config: FailureDetectorConfig,
    shutdown_tx: watch::Sender<bool>,
    swim_state: Arc<tokio::sync::Mutex<crate::swim::SwimState>>,
    placement: Arc<dyn PlacementStrategy>,
    tls_identity: Option<tonic::transport::Identity>,
    tls_ca: Option<tonic::transport::Certificate>,
    auth: Option<Arc<dyn crate::auth::ClusterAuth>>,
    retry_policy: RetryPolicy,
    multi_cluster: Option<MultiClusterConfig>,
    // Cross-cluster (GSI) fields -- None when multi-cluster is not configured
    cross_cluster_dir: Option<Arc<dyn CrossClusterDirectory>>,
    local_cluster_id: Option<ClusterId>,
    peer_endpoints: Option<Arc<HashMap<ClusterId, String>>>,
    failover_config: FailoverConfig,
    health_port: Option<u16>,
    store_probe: Option<crate::health_server::StoreProbe>,
    #[cfg(feature = "tcp-transport")]
    tcp_port: Option<u16>,
    #[cfg(feature = "tcp-transport")]
    tcp_pool: Arc<crate::tcp_transport::TcpConnectionPool>,
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

    /// Get the cluster identity if multi-cluster is configured.
    pub fn cluster_id(&self) -> Option<&ClusterId> {
        self.multi_cluster.as_ref().map(|c| &c.cluster_id)
    }

    /// Signal the gRPC server and SWIM task to shut down.
    /// Does NOT drain active grains — use `shutdown_and_drain()` for graceful shutdown.
    /// Get the TCP connection pool (only available with `tcp-transport` feature).
    #[cfg(feature = "tcp-transport")]
    pub fn tcp_pool(&self) -> &Arc<crate::tcp_transport::TcpConnectionPool> {
        &self.tcp_pool
    }

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

        let ring = self.ring.load();

        // Use per-grain placement hint if set, otherwise silo-level strategy
        let target = match G::placement_hint() {
            Some("prefer_local") => {
                crate::placement::PreferLocalPlacement.place(
                    grain_type, &key, &self.local_addr.silo_id, &ring,
                )
            }
            Some("random") => {
                crate::placement::RandomPlacement.place(
                    grain_type, &key, &self.local_addr.silo_id, &ring,
                )
            }
            Some("hash") | None => {
                self.placement.place(grain_type, &key, &self.local_addr.silo_id, &ring)
            }
            Some(unknown) => {
                tracing::warn!(grain_type, hint = unknown, "unknown placement hint, using silo default");
                self.placement.place(grain_type, &key, &self.local_addr.silo_id, &ring)
            }
        };

        let grain_ref = match target {
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
        };

        grain_ref.with_retry_policy(self.retry_policy.clone())
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
            Box::new(move |id, cancellation| {
                let (tx, rx) = mpsc::channel(orlando_core::MAILBOX_CAPACITY);
                let task = if G::reentrant() {
                    tokio::spawn(async move {
                        reentrant_mailbox::run_reentrant_mailbox::<G>(
                            id, rx, activator_for_mailbox, cancellation,
                        )
                        .await;
                    })
                } else {
                    tokio::spawn(async move {
                        mailbox::run_mailbox::<G>(id, rx, activator_for_mailbox, cancellation).await;
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

        // Atomic transition: hold the swim_state mutex across both the ring
        // ArcSwap store and the SwimState members inserts.
        {
            let mut swim = self.swim_state.lock().await;
            let mut new_ring = (**self.ring.load()).clone();
            for silo in &members {
                new_ring.add(silo.clone());
                let _ = self.change_tx.send(MembershipChange::SiloJoined(silo.clone()));
            }
            self.ring.store(Arc::new(new_ring));
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

    /// Discover and join a cluster using a membership provider.
    ///
    /// Resolves members via the provider, then joins through the first
    /// reachable seed. Equivalent to calling `join_cluster()` with the
    /// provider's discovered address.
    pub async fn discover_and_join(
        &self,
        provider: &dyn crate::discovery::MembershipProvider,
    ) -> Result<(), ClusterError> {
        let members = provider.get_members().await?;
        for member in &members {
            let endpoint = member.endpoint();
            if endpoint == self.local_addr.endpoint() {
                continue; // skip self
            }
            match self.join_cluster(&endpoint).await {
                Ok(()) => {
                    tracing::info!(seed = %endpoint, "joined cluster via discovery");
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(seed = %endpoint, error = %e, "failed to join via seed, trying next");
                }
            }
        }
        Err(ClusterError::Transport(
            "no reachable seeds found via membership provider".to_string(),
        ))
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
            self.auth.clone(),
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
            self.auth.clone(),
            self.directory.clone() as Arc<dyn GrainActivator>,
            self.registry.clone(),
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

        // Spawn HTTP health server if configured
        if let Some(health_port) = self.health_port {
            let health_addr: SocketAddr =
                format!("{}:{}", self.local_addr.host, health_port).parse()?;
            let ring = self.ring.clone();
            let probe = self.store_probe.clone();
            let shutdown_rx = self.shutdown_tx.subscribe();
            tokio::spawn(async move {
                crate::health_server::run_health_server(health_addr, ring, probe, shutdown_rx)
                    .await;
            });
            tracing::info!(addr = %health_addr, "health server started");
        }

        // Spawn multi-cluster health checker + failover manager if configured
        if let Some(mc_config) = &self.multi_cluster {
            let health = Arc::new(ClusterHealth::new(
                mc_config.clone(),
                self.pool.clone(),
                self.shutdown_tx.subscribe(),
            ));
            // The health checker needs to be cloned before being moved into spawn
            let health_runner = ClusterHealth::new(
                mc_config.clone(),
                self.pool.clone(),
                self.shutdown_tx.subscribe(),
            );
            tokio::spawn(health_runner.run());
            tracing::info!(cluster_id = %mc_config.cluster_id, "multi-cluster health checker started");

            // Spawn failover manager if we have a cross-cluster directory
            if let (Some(dir), Some(cid)) = (&self.cross_cluster_dir, &self.local_cluster_id) {
                let failover = FailoverManager::new(
                    cid.clone(),
                    self.failover_config.clone(),
                    health,
                    dir.clone(),
                    self.shutdown_tx.subscribe(),
                );
                tokio::spawn(failover.run());
                tracing::info!("failover manager started");
            }
        }

        tracing::info!(%addr, "cluster silo listening");

        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_signal = async move {
            let _ = shutdown_rx.changed().await;
        };

        let mut server_builder = tonic::transport::Server::builder();
        if let Some(ref identity) = self.tls_identity {
            let mut tls_config = tonic::transport::ServerTlsConfig::new()
                .identity(identity.clone());
            if let Some(ref ca) = self.tls_ca {
                tls_config = tls_config.client_ca_root(ca.clone());
            }
            server_builder = server_builder
                .tls_config(tls_config)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
        }

        // Add the optional ClusterGateway service for cross-cluster forwarding
        if let Some(ref cluster_id) = self.local_cluster_id {
            let gateway = ClusterGatewayService::new(
                self.registry.clone(),
                self.directory.clone() as Arc<dyn GrainActivator>,
                cluster_id.to_string(),
            );
            server_builder
                .add_service(GrainTransportServer::new(transport))
                .add_service(MembershipServer::new(membership))
                .add_service(ClusterGatewayServer::new(gateway))
                .serve_with_shutdown(addr, shutdown_signal)
                .await?;
        } else {
            server_builder
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
    tls_identity: Option<tonic::transport::Identity>,
    tls_ca: Option<tonic::transport::Certificate>,
    auth: Option<Arc<dyn crate::auth::ClusterAuth>>,
    auth_token: Option<String>,
    retry_policy: RetryPolicy,
    multi_cluster: Option<MultiClusterConfig>,
    cross_cluster_dir: Option<Arc<dyn CrossClusterDirectory>>,
    local_cluster_id: Option<ClusterId>,
    peer_endpoints: Option<HashMap<ClusterId, String>>,
    failover_config: FailoverConfig,
    health_port: Option<u16>,
    store_probe: Option<crate::health_server::StoreProbe>,
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
            tls_identity: None,
            tls_ca: None,
            auth: None,
            auth_token: None,
            retry_policy: RetryPolicy::default(),
            multi_cluster: None,
            cross_cluster_dir: None,
            local_cluster_id: None,
            peer_endpoints: None,
            failover_config: FailoverConfig::default(),
            health_port: None,
            store_probe: None,
        }
    }

    /// Bind an HTTP server on `host:port` exposing `/healthz` (liveness) and
    /// `/readyz` (readiness) endpoints for orchestrator probes. The host
    /// matches the gRPC bind host. When unset, no health server runs.
    pub fn health_port(mut self, port: u16) -> Self {
        self.health_port = Some(port);
        self
    }

    /// Optional dependency probe checked by `/readyz`. Use this to verify
    /// the backing store (Postgres, Redis, etc.) is reachable. Returning
    /// `Err` causes `/readyz` to respond with 503.
    pub fn store_probe(mut self, probe: crate::health_server::StoreProbe) -> Self {
        self.store_probe = Some(probe);
        self
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

    /// Configure TLS with a server certificate and private key (PEM-encoded).
    pub fn tls(mut self, cert_pem: impl AsRef<[u8]>, key_pem: impl AsRef<[u8]>) -> Self {
        self.tls_identity = Some(tonic::transport::Identity::from_pem(cert_pem, key_pem));
        self
    }

    /// Configure a CA certificate for verifying peer certificates (mTLS).
    pub fn tls_ca(mut self, ca_pem: impl AsRef<[u8]>) -> Self {
        self.tls_ca = Some(tonic::transport::Certificate::from_pem(ca_pem));
        self
    }

    /// Set the authentication handler for validating incoming requests.
    pub fn auth(mut self, auth: Arc<dyn crate::auth::ClusterAuth>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Set a token to attach to all outgoing silo-to-silo requests.
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Set the retry policy applied to remote grain calls. Defaults to 2 retries
    /// with exponential backoff (100ms base, 2s cap).
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Configure multi-cluster support with peer cluster endpoints.
    pub fn multi_cluster(mut self, config: MultiClusterConfig) -> Self {
        self.multi_cluster = Some(config);
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

    /// Configure failover behavior (grace period, check interval).
    /// Only relevant when multi-cluster is enabled.
    pub fn failover_config(mut self, config: FailoverConfig) -> Self {
        self.failover_config = config;
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
        #[cfg(not(test))]
        {
            let no_auth = self.auth.is_none() && self.auth_token.is_none();
            let no_tls = self.tls_identity.is_none() && self.tls_ca.is_none();
            if no_auth {
                tracing::warn!(
                    "ClusterSilo built without authentication — silo-to-silo RPC is \
                     unauthenticated. Any reachable node can join the cluster, invoke \
                     grains, and read state. For production, call .auth(SharedSecretAuth::new(..)) \
                     or .auth_token(..) on the builder."
                );
            }
            if no_tls {
                tracing::warn!(
                    "ClusterSilo built without TLS — silo-to-silo RPC traffic is plaintext \
                     on the wire. For production, configure .tls_identity(..) and .tls_ca(..)."
                );
            }
        }

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

        let (change_tx, _) = broadcast::channel(256);
        let (shutdown_tx, _) = watch::channel(false);

        let swim_state = Arc::new(tokio::sync::Mutex::new(
            crate::swim::SwimState::new(local_addr.clone()),
        ));

        let placement = self
            .placement
            .unwrap_or_else(|| Arc::new(HashBasedPlacement));

        // Build ConnectionPool with TLS and/or auth token
        let pool = match (&self.tls_ca, &self.auth_token) {
            (Some(ca), Some(token)) => {
                let mut client_tls = tonic::transport::ClientTlsConfig::new()
                    .ca_certificate(ca.clone());
                if let Some(ref identity) = self.tls_identity {
                    client_tls = client_tls.identity(identity.clone());
                }
                ConnectionPool::with_tls_and_auth(client_tls, token.clone())
            }
            (Some(ca), None) => {
                let mut client_tls = tonic::transport::ClientTlsConfig::new()
                    .ca_certificate(ca.clone());
                if let Some(ref identity) = self.tls_identity {
                    client_tls = client_tls.identity(identity.clone());
                }
                ConnectionPool::with_tls(client_tls)
            }
            (None, Some(token)) => ConnectionPool::with_auth(token.clone()),
            (None, None) => ConnectionPool::new(),
        };

        ClusterSilo {
            local_addr,
            directory: Arc::new(GrainDirectory::new()),
            registry: Arc::new(self.registry),
            ring: Arc::new(ArcSwap::from_pointee(ring)),
            pool: Arc::new(pool),
            change_tx,
            failure_detector_config: self.failure_detector_config,
            shutdown_tx,
            swim_state,
            placement,
            tls_identity: self.tls_identity,
            tls_ca: self.tls_ca,
            auth: self.auth,
            retry_policy: self.retry_policy,
            multi_cluster: self.multi_cluster,
            cross_cluster_dir: self.cross_cluster_dir,
            local_cluster_id: self.local_cluster_id,
            peer_endpoints: self.peer_endpoints.map(Arc::new),
            failover_config: self.failover_config,
            health_port: self.health_port,
            store_probe: self.store_probe,
            #[cfg(feature = "tcp-transport")]
            tcp_port: None,
            #[cfg(feature = "tcp-transport")]
            tcp_pool: Arc::new(crate::tcp_transport::TcpConnectionPool::new()),
        }
    }
}
