use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::mpsc;

use orlando_core::{Grain, GrainActivator, GrainHandler, GrainId, mailbox};
use orlando_runtime::GrainDirectory;

use crate::cluster_grain_ref::ClusterGrainRef;
use crate::error::ClusterError;
use crate::hash_ring::{HashRing, SiloAddress};
use crate::membership::MembershipService;
use crate::message_registry::MessageRegistry;
use crate::network_message::NetworkMessage;
use crate::proto::grain_transport_server::GrainTransportServer;
use crate::proto::membership_client::MembershipClient;
use crate::proto::membership_server::MembershipServer;
use crate::proto::{JoinRequest, SiloAddress as ProtoSiloAddress};
use crate::transport::GrainTransportService;

pub struct ClusterSilo {
    local_addr: SiloAddress,
    directory: Arc<GrainDirectory>,
    registry: Arc<MessageRegistry>,
    ring: Arc<RwLock<HashRing>>,
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

    /// Get a cluster-aware grain reference.
    ///
    /// Uses the hash ring to determine whether the grain lives on this silo
    /// (local dispatch) or a remote silo (gRPC dispatch).
    pub fn get_ref<G: Grain>(&self, key: impl Into<String>) -> ClusterGrainRef<G> {
        let key = key.into();
        let grain_key = format!("{}/{}", std::any::type_name::<G>(), key);

        let ring = self.ring.read().expect("ring lock poisoned");
        match ring.get(&grain_key) {
            Some(target) if target.silo_id == self.local_addr.silo_id => {
                drop(ring);
                let sender = self.local_activate::<G>(&key);
                ClusterGrainRef::local(sender)
            }
            Some(target) => {
                let endpoint = target.endpoint();
                drop(ring);
                ClusterGrainRef::remote(endpoint, std::any::type_name::<G>(), key)
            }
            None => {
                drop(ring);
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
            type_name: std::any::type_name::<G>(),
            key: key.to_string(),
        };
        let activator: Arc<dyn GrainActivator> = self.directory.clone();
        let activator_for_mailbox = activator.clone();
        activator.get_or_insert(
            grain_id,
            Box::new(move |id| {
                let (tx, rx) = mpsc::channel(256);
                let task = tokio::spawn(async move {
                    mailbox::run_mailbox::<G>(id, rx, activator_for_mailbox).await;
                });
                (tx, task)
            }),
        )
    }

    /// Join an existing cluster by contacting a seed silo.
    pub async fn join_cluster(&self, seed_addr: &str) -> Result<(), ClusterError> {
        let mut client = MembershipClient::connect(format!("http://{}", seed_addr))
            .await
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        let response = client
            .join(JoinRequest {
                joiner: Some(ProtoSiloAddress {
                    host: self.local_addr.host.clone(),
                    port: self.local_addr.port as u32,
                    silo_id: self.local_addr.silo_id.clone(),
                }),
            })
            .await
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        let mut ring = self
            .ring
            .write()
            .map_err(|e| ClusterError::Transport(e.to_string()))?;
        for member in response.into_inner().members {
            ring.add(SiloAddress {
                host: member.host,
                port: member.port as u16,
                silo_id: member.silo_id,
            });
        }

        Ok(())
    }

    /// Start serving gRPC (grain transport + membership).
    pub async fn serve(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        let addr: SocketAddr = format!("{}:{}", self.local_addr.host, self.local_addr.port)
            .parse()?;

        let transport = GrainTransportService::new(
            self.registry.clone(),
            self.directory.clone() as Arc<dyn GrainActivator>,
        );

        let membership =
            MembershipService::new(self.ring.clone(), self.local_addr.clone());

        tracing::info!(%addr, "cluster silo listening");

        tonic::transport::Server::builder()
            .add_service(GrainTransportServer::new(transport))
            .add_service(MembershipServer::new(membership))
            .serve(addr)
            .await?;

        Ok(())
    }
}

pub struct ClusterSiloBuilder {
    host: String,
    port: u16,
    silo_id: Option<String>,
    registry: MessageRegistry,
    virtual_nodes: u32,
}

impl ClusterSiloBuilder {
    fn new() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 0,
            silo_id: None,
            registry: MessageRegistry::new(),
            virtual_nodes: 150,
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

        ClusterSilo {
            local_addr,
            directory: Arc::new(GrainDirectory::new()),
            registry: Arc::new(self.registry),
            ring: Arc::new(RwLock::new(ring)),
        }
    }
}
