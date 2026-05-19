//! Lightweight TCP transport for silo-to-silo grain calls.
//!
//! Uses length-delimited frames over raw TCP with bincode serialization,
//! targeting 15-40us latency for internal grain dispatch. gRPC remains
//! the transport for external clients.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use orlando_core::GrainActivator;

use crate::connection_pool::ConnectionPool;
use crate::error::ClusterError;
use crate::hash_ring::HashRing;
use crate::message_registry::MessageRegistry;
use crate::network_message::Encoding;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Request sent over the TCP wire. Mirrors the gRPC `InvokeRequest` but avoids
/// protobuf overhead — pure bincode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpInvokeRequest {
    pub request_id: u64,
    pub grain_type: String,
    pub grain_key: String,
    pub message_type: String,
    pub payload: Vec<u8>,
    pub context: HashMap<String, String>,
}

/// Response sent over the TCP wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpInvokeResponse {
    pub request_id: u64,
    pub success: bool,
    pub payload: Vec<u8>,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// TCP transport server that listens for incoming silo-to-silo grain calls.
pub struct TcpTransportServer {
    registry: Arc<MessageRegistry>,
    activator: Arc<dyn GrainActivator>,
    ring: Arc<std::sync::RwLock<HashRing>>,
    pool: Arc<ConnectionPool>,
    local_silo_id: String,
}

impl fmt::Debug for TcpTransportServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpTransportServer")
            .field("local_silo_id", &self.local_silo_id)
            .finish_non_exhaustive()
    }
}

impl TcpTransportServer {
    pub fn new(
        registry: Arc<MessageRegistry>,
        activator: Arc<dyn GrainActivator>,
        ring: Arc<std::sync::RwLock<HashRing>>,
        pool: Arc<ConnectionPool>,
        local_silo_id: String,
    ) -> Self {
        Self {
            registry,
            activator,
            ring,
            pool,
            local_silo_id,
        }
    }

    /// Run the TCP listener until the shutdown signal fires.
    pub async fn serve(
        self: Arc<Self>,
        addr: SocketAddr,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), ClusterError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| ClusterError::Transport(format!("tcp bind failed: {}", e)))?;

        tracing::info!(%addr, "tcp transport listening");

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, peer)) => {
                            tracing::debug!(%peer, "tcp transport: accepted connection");
                            let server = self.clone();
                            tokio::spawn(async move {
                                if let Err(e) = server.handle_connection(stream).await {
                                    tracing::debug!(error = %e, "tcp connection handler error");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "tcp accept error");
                        }
                    }
                }
                _ = shutdown.changed() => {
                    tracing::info!("tcp transport shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn handle_connection(self: Arc<Self>, stream: TcpStream) -> Result<(), ClusterError> {
        stream
            .set_nodelay(true)
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        let mut framed = Framed::new(stream, build_codec());

        while let Some(frame) = framed.next().await {
            let frame = frame.map_err(|e| ClusterError::Transport(e.to_string()))?;

            let (request, _): (TcpInvokeRequest, _) =
                bincode::serde::decode_from_slice(&frame, bincode::config::standard())
                    .map_err(|e| ClusterError::Deserialization(e.to_string()))?;

            let server = self.clone();

            // Dispatch inline (not spawned) to keep ordering per connection.
            // Callers wanting concurrency use multiple connections or request IDs
            // via the multiplexed client.
            let response = server.dispatch(request).await;

            let encoded = bincode::serde::encode_to_vec(&response, bincode::config::standard())
                .map_err(|e| ClusterError::Serialization(e.to_string()))?;

            framed
                .send(Bytes::from(encoded))
                .await
                .map_err(|e| ClusterError::Transport(e.to_string()))?;
        }

        Ok(())
    }

    async fn dispatch(&self, request: TcpInvokeRequest) -> TcpInvokeResponse {
        // Check if grain belongs on a different silo (gateway forwarding)
        let forward_endpoint = {
            let ring_key = format!("{}/{}", request.grain_type, request.grain_key);
            let ring = self.ring.read().expect("ring lock poisoned");
            match ring.get(&ring_key) {
                Some(target) if target.silo_id != self.local_silo_id => {
                    Some(target.endpoint())
                }
                _ => None,
            }
        };

        if let Some(endpoint) = forward_endpoint {
            tracing::debug!(
                grain_type = %request.grain_type,
                grain_key = %request.grain_key,
                target = %endpoint,
                "tcp: forwarding grain call to owner silo via grpc"
            );

            // Forward via gRPC (TCP forwarding would require knowing peer TCP ports)
            let client = self.pool.get_transport(&endpoint).await;
            match client {
                Ok(mut c) => {
                    let grpc_req = crate::proto::InvokeRequest {
                        grain_type: request.grain_type,
                        grain_key: request.grain_key,
                        message_type: request.message_type,
                        payload: request.payload,
                        encoding: 0, // bincode
                        request_context: request.context,
                        message_version: 0,
                    };
                    match c.invoke(grpc_req).await {
                        Ok(resp) => {
                            let inner = resp.into_inner();
                            let success = inner.error.is_empty();
                            TcpInvokeResponse {
                                request_id: request.request_id,
                                success,
                                payload: inner.payload,
                                error: if success {
                                    None
                                } else {
                                    Some(inner.error)
                                },
                            }
                        }
                        Err(e) => TcpInvokeResponse {
                            request_id: request.request_id,
                            success: false,
                            payload: Vec::new(),
                            error: Some(e.to_string()),
                        },
                    }
                }
                Err(e) => TcpInvokeResponse {
                    request_id: request.request_id,
                    success: false,
                    payload: Vec::new(),
                    error: Some(e.to_string()),
                },
            }
        } else {
            // Local dispatch
            match self
                .registry
                .dispatch(
                    &request.grain_type,
                    request.grain_key,
                    &request.message_type,
                    0, // message_version
                    request.payload,
                    Encoding::Bincode,
                    request.context,
                    self.activator.clone(),
                )
                .await
            {
                Ok((payload, _encoding)) => TcpInvokeResponse {
                    request_id: request.request_id,
                    success: true,
                    payload,
                    error: None,
                },
                Err(e) => TcpInvokeResponse {
                    request_id: request.request_id,
                    success: false,
                    payload: Vec::new(),
                    error: Some(e.to_string()),
                },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A single TCP connection to a peer silo that supports sequential request/response.
struct TcpTransportConnection {
    framed: Framed<TcpStream, LengthDelimitedCodec>,
}

impl TcpTransportConnection {
    async fn connect(addr: &str) -> Result<Self, ClusterError> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| ClusterError::Transport(format!("tcp connect to {}: {}", addr, e)))?;

        stream
            .set_nodelay(true)
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        Ok(Self {
            framed: Framed::new(stream, build_codec()),
        })
    }

    async fn invoke(
        &mut self,
        request: TcpInvokeRequest,
    ) -> Result<TcpInvokeResponse, ClusterError> {
        let encoded = bincode::serde::encode_to_vec(&request, bincode::config::standard())
            .map_err(|e| ClusterError::Serialization(e.to_string()))?;

        self.framed
            .send(Bytes::from(encoded))
            .await
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        let frame = self
            .framed
            .next()
            .await
            .ok_or_else(|| ClusterError::Transport("tcp connection closed".to_string()))?
            .map_err(|e| ClusterError::Transport(e.to_string()))?;

        let (response, _): (TcpInvokeResponse, _) =
            bincode::serde::decode_from_slice(&frame, bincode::config::standard())
                .map_err(|e| ClusterError::Deserialization(e.to_string()))?;

        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// Client (public API wrapping a connection)
// ---------------------------------------------------------------------------

/// TCP transport client for sending grain invoke requests to a peer silo.
pub struct TcpTransportClient {
    /// The peer endpoint (host:port).
    endpoint: String,
    /// The underlying connection, wrapped in a mutex for sequential access.
    /// For concurrency, use `TcpConnectionPool` which manages multiple clients.
    conn: Mutex<Option<TcpTransportConnection>>,
    next_request_id: AtomicU64,
}

impl fmt::Debug for TcpTransportClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpTransportClient")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl TcpTransportClient {
    /// Create a new client targeting the given endpoint. Connection is lazy.
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            conn: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Send an invoke request and return the response.
    pub async fn invoke(
        &self,
        grain_type: String,
        grain_key: String,
        message_type: String,
        payload: Vec<u8>,
        context: HashMap<String, String>,
    ) -> Result<TcpInvokeResponse, ClusterError> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);

        let request = TcpInvokeRequest {
            request_id,
            grain_type,
            grain_key,
            message_type,
            payload,
            context,
        };

        let mut guard = self.conn.lock().await;

        // Lazy connect
        if guard.is_none() {
            *guard = Some(TcpTransportConnection::connect(&self.endpoint).await?);
        }

        let conn = guard.as_mut().expect("connection just established");
        match conn.invoke(request.clone()).await {
            Ok(resp) => Ok(resp),
            Err(_) => {
                // Reconnect once on failure
                tracing::debug!(endpoint = %self.endpoint, "tcp: reconnecting after error");
                let mut new_conn =
                    TcpTransportConnection::connect(&self.endpoint).await?;
                let resp = new_conn.invoke(request).await?;
                *guard = Some(new_conn);
                Ok(resp)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Connection pool
// ---------------------------------------------------------------------------

/// Manages TCP connections to peer silos. Each endpoint gets a lazily-created
/// `TcpTransportClient`.
#[derive(Debug, Default)]
pub struct TcpConnectionPool {
    clients: DashMap<String, Arc<TcpTransportClient>>,
}

impl TcpConnectionPool {
    pub fn new() -> Self {
        Self {
            clients: DashMap::new(),
        }
    }

    /// Get or create a TCP client for the given endpoint.
    pub fn get_client(&self, endpoint: &str) -> Arc<TcpTransportClient> {
        if let Some(client) = self.clients.get(endpoint) {
            return client.clone();
        }

        let client = Arc::new(TcpTransportClient::new(endpoint.to_string()));
        self.clients
            .insert(endpoint.to_string(), client.clone());
        client
    }

    /// Remove a client for the given endpoint (e.g. on silo departure).
    pub fn remove(&self, endpoint: &str) {
        self.clients.remove(endpoint);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .max_frame_length(16 * 1024 * 1024) // 16 MiB max frame
        .new_codec()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use orlando_core::GrainActivator;
    use crate::hash_ring::HashRing;
    use crate::message_registry::MessageRegistry;

    /// Round-trip test: start a TCP server, connect a client, send an invoke,
    /// and get a response. The registry has no handlers registered, so we
    /// expect an error response (unknown grain type) — but the wire protocol
    /// round-trip is exercised.
    #[tokio::test]
    async fn tcp_round_trip_unknown_grain() {
        let registry = Arc::new(MessageRegistry::new());
        let ring = Arc::new(std::sync::RwLock::new(HashRing::new(10)));
        let pool = Arc::new(ConnectionPool::new());

        // Use the GrainDirectory as the activator (same as production)
        let directory = Arc::new(orlando_runtime::GrainDirectory::new());
        let activator: Arc<dyn GrainActivator> = directory;

        let server = Arc::new(TcpTransportServer::new(
            registry,
            activator,
            ring,
            pool,
            "test-silo".to_string(),
        ));

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // Bind to an OS-assigned port
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        drop(listener); // release so the server can bind

        let server_handle = {
            let server = server.clone();
            tokio::spawn(async move {
                let _ = server.serve(addr, shutdown_rx).await;
            })
        };

        // Give the server a moment to start listening
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Client side
        let client = TcpTransportClient::new(addr.to_string());
        let response = client
            .invoke(
                "NonExistentGrain".to_string(),
                "key-1".to_string(),
                "SomeMessage".to_string(),
                vec![1, 2, 3],
                HashMap::new(),
            )
            .await
            .expect("invoke should succeed at transport level");

        assert!(!response.success, "should fail because grain type is unknown");
        assert!(
            response.error.as_deref().unwrap_or("").contains("unknown grain type"),
            "error should mention unknown grain type, got: {:?}",
            response.error
        );

        // Shut down
        let _ = shutdown_tx.send(true);
        let _ = server_handle.await;
    }

    /// Test that TcpConnectionPool returns the same client for repeated lookups.
    #[test]
    fn tcp_pool_returns_same_client() {
        let pool = TcpConnectionPool::new();
        let c1 = pool.get_client("127.0.0.1:9999");
        let c2 = pool.get_client("127.0.0.1:9999");
        assert!(Arc::ptr_eq(&c1, &c2));
    }

    /// Test that TcpConnectionPool returns different clients for different endpoints.
    #[test]
    fn tcp_pool_different_endpoints() {
        let pool = TcpConnectionPool::new();
        let c1 = pool.get_client("127.0.0.1:9999");
        let c2 = pool.get_client("127.0.0.1:8888");
        assert!(!Arc::ptr_eq(&c1, &c2));
    }
}
