//! External client for calling Orlando grains from non-silo processes.
//!
//! ```ignore
//! use orlando_client::OrlandoClient;
//!
//! let client = OrlandoClient::connect("127.0.0.1:5001").await?;
//! let counter = client.grain("Counter", "my-counter");
//!
//! // Typed (Rust clients sharing message types):
//! let result: i64 = counter.ask(Increment { amount: 5 }).await?;
//!
//! // Untyped (any language via protobuf):
//! let response_bytes = counter.ask_proto("Increment", payload_bytes).await?;
//! ```

mod error;

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Serialize, de::DeserializeOwned};

use orlando_cluster::{
    ConnectionPool, Encoding, HashRing, NetworkMessage, SiloAddress,
};
use orlando_cluster::proto::{
    GetMembersRequest, InvokeRequest,
};

pub use error::ClientError;

/// Client for calling Orlando grains from outside the cluster.
///
/// Connects to any silo, discovers the full cluster membership, and
/// routes grain calls to the correct silo using the same consistent
/// hashing as the cluster itself.
pub struct OrlandoClient {
    pool: Arc<ConnectionPool>,
    ring: ArcSwap<HashRing>,
}

impl OrlandoClient {
    /// Connect to an Orlando cluster via any silo endpoint.
    ///
    /// Fetches the current member list and builds a local hash ring
    /// for routing grain calls to the correct silo.
    pub async fn connect(endpoint: &str) -> Result<Self, ClientError> {
        let pool = Arc::new(ConnectionPool::new());
        let mut client = pool
            .get_membership(endpoint)
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        let response = client
            .get_members(GetMembersRequest {})
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

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

        if members.is_empty() {
            return Err(ClientError::Connection(
                "cluster returned no members".to_string(),
            ));
        }

        let mut ring = HashRing::new(150);
        for member in &members {
            ring.add(member.clone());
        }

        tracing::info!(
            members = members.len(),
            "connected to Orlando cluster"
        );

        Ok(Self {
            pool,
            ring: ArcSwap::from_pointee(ring),
        })
    }

    /// Connect with TLS.
    pub async fn connect_tls(
        endpoint: &str,
        tls: tonic::transport::ClientTlsConfig,
    ) -> Result<Self, ClientError> {
        let pool = Arc::new(ConnectionPool::with_tls(tls));
        let mut client = pool
            .get_membership(endpoint)
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        let response = client
            .get_members(GetMembersRequest {})
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

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

        if members.is_empty() {
            return Err(ClientError::Connection(
                "cluster returned no members".to_string(),
            ));
        }

        let mut ring = HashRing::new(150);
        for member in &members {
            ring.add(member.clone());
        }

        Ok(Self {
            pool,
            ring: ArcSwap::from_pointee(ring),
        })
    }

    /// Refresh the cluster membership. Call this periodically or after
    /// connection errors to pick up membership changes.
    pub async fn refresh_membership(&self) -> Result<(), ClientError> {
        let members = {
            let ring = self.ring.load();
            ring.members()
        };

        for member in &members {
            let result = async {
                let mut client = self
                    .pool
                    .get_membership(&member.endpoint())
                    .await
                    .map_err(|e| ClientError::Connection(e.to_string()))?;
                let response = client
                    .get_members(GetMembersRequest {})
                    .await
                    .map_err(|e| ClientError::Connection(e.to_string()))?;
                Ok::<_, ClientError>(response)
            }
            .await;

            if let Ok(response) = result {
                let new_members: Vec<SiloAddress> = response
                    .into_inner()
                    .members
                    .into_iter()
                    .map(|m| SiloAddress {
                        host: m.host,
                        port: m.port as u16,
                        silo_id: m.silo_id,
                    })
                    .collect();

                let mut new_ring = HashRing::new(150);
                for m in &new_members {
                    new_ring.add(m.clone());
                }
                self.ring.store(Arc::new(new_ring));
                tracing::debug!(members = new_members.len(), "membership refreshed");
                return Ok(());
            }
        }

        Err(ClientError::Connection(
            "no reachable silos for membership refresh".to_string(),
        ))
    }

    /// Get a handle to a specific grain for sending messages.
    pub fn grain<'a>(&'a self, grain_type: &str, grain_key: &str) -> GrainHandle<'a> {
        GrainHandle {
            client: self,
            grain_type: grain_type.to_string(),
            grain_key: grain_key.to_string(),
        }
    }

    /// Send a raw invoke request to the cluster. Used internally by `GrainHandle`.
    ///
    /// Retries once with a membership refresh if the target silo is unreachable,
    /// in case the ring is stale from a membership change.
    async fn invoke(
        &self,
        grain_type: &str,
        grain_key: &str,
        message_type: &str,
        payload: Vec<u8>,
        encoding: i32,
    ) -> Result<Vec<u8>, ClientError> {
        let ring_key = format!("{}/{}", grain_type, grain_key);

        for attempt in 0..2u32 {
            let ring = self.ring.load();
            let target = ring
                .get(&ring_key)
                .ok_or(ClientError::NoSiloAvailable)?;
            let endpoint = target.endpoint();
            drop(ring);

            let client_result = self
                .pool
                .get_transport(&endpoint)
                .await;

            let mut client = match client_result {
                Ok(c) => c,
                Err(e) => {
                    if attempt == 0 {
                        tracing::debug!(endpoint = %endpoint, "target unreachable, refreshing membership");
                        let _ = self.refresh_membership().await;
                        continue;
                    }
                    return Err(ClientError::Connection(e.to_string()));
                }
            };

            let result = client
                .invoke(InvokeRequest {
                    grain_type: grain_type.to_string(),
                    grain_key: grain_key.to_string(),
                    message_type: message_type.to_string(),
                    payload: payload.clone(),
                    encoding,
                    request_context: HashMap::new(),
                    message_version: 0,
                })
                .await;

            match result {
                Ok(response) => {
                    let inner = response.into_inner();
                    if !inner.error.is_empty() {
                        return Err(ClientError::GrainError(inner.error));
                    }
                    return Ok(inner.payload);
                }
                Err(e) => {
                    if attempt == 0 {
                        tracing::debug!(endpoint = %endpoint, error = %e, "call failed, refreshing membership and retrying");
                        self.pool.remove(&endpoint);
                        let _ = self.refresh_membership().await;
                        continue;
                    }
                    return Err(ClientError::Transport(e.to_string()));
                }
            }
        }

        Err(ClientError::NoSiloAvailable)
    }
}

/// A handle to a specific grain, used to send messages.
pub struct GrainHandle<'a> {
    client: &'a OrlandoClient,
    grain_type: String,
    grain_key: String,
}

impl GrainHandle<'_> {
    /// Send a typed message using bincode encoding (Rust-to-Rust).
    ///
    /// The message type must implement `NetworkMessage` (shared between
    /// client and server via a common crate).
    pub async fn ask<M>(&self, msg: M) -> Result<M::Result, ClientError>
    where
        M: NetworkMessage,
        M::Result: Serialize + DeserializeOwned,
    {
        let payload = bincode::serde::encode_to_vec(&msg, bincode::config::standard())
            .map_err(|e| ClientError::Serialization(e.to_string()))?;

        let response = self
            .client
            .invoke(
                &self.grain_type,
                &self.grain_key,
                M::message_type_name(),
                payload,
                Encoding::Bincode.to_proto(),
            )
            .await?;

        let (result, _) =
            bincode::serde::decode_from_slice(&response, bincode::config::standard())
                .map_err(|e| ClientError::Deserialization(e.to_string()))?;

        Ok(result)
    }

    /// Send a protobuf-encoded message (cross-language clients).
    ///
    /// Takes raw protobuf bytes and returns raw protobuf bytes.
    /// The caller is responsible for encoding/decoding the protobuf messages.
    pub async fn ask_proto(
        &self,
        message_type: &str,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, ClientError> {
        self.client
            .invoke(
                &self.grain_type,
                &self.grain_key,
                message_type,
                payload,
                Encoding::Protobuf.to_proto(),
            )
            .await
    }
}

impl std::fmt::Debug for OrlandoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrlandoClient").finish()
    }
}
