use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

use orlando_core::{Envelope, Grain, GrainError, GrainHandler, GrainRef, ReadOnlyMessage, RequestContext};

use crate::connection_pool::ConnectionPool;
use crate::network_message::NetworkMessage;
use crate::proto::InvokeRequest;
use crate::retry::RetryPolicy;
use crate::store_wrapper::ReplicaStore;

pub struct ClusterGrainRef<G: Grain> {
    inner: RefInner,
    retry_policy: RetryPolicy,
    /// Optional replica store for serving stale reads on secondaries.
    replica_store: Option<Arc<ReplicaStore>>,
    /// Max staleness for read-only messages (0 = always forward).
    max_staleness: Duration,
    _marker: PhantomData<G>,
}

enum RefInner {
    Local(tokio::sync::mpsc::Sender<Envelope>),
    Remote {
        endpoint: String,
        grain_type: &'static str,
        grain_key: String,
        pool: Arc<ConnectionPool>,
    },
}

impl Clone for RefInner {
    fn clone(&self) -> Self {
        match self {
            Self::Local(sender) => Self::Local(sender.clone()),
            Self::Remote {
                endpoint,
                grain_type,
                grain_key,
                pool,
            } => Self::Remote {
                endpoint: endpoint.clone(),
                grain_type,
                grain_key: grain_key.clone(),
                pool: pool.clone(),
            },
        }
    }
}

impl<G: Grain> Clone for ClusterGrainRef<G> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            retry_policy: self.retry_policy.clone(),
            replica_store: self.replica_store.clone(),
            max_staleness: self.max_staleness,
            _marker: PhantomData,
        }
    }
}

impl<G: Grain> ClusterGrainRef<G> {
    pub(crate) fn local(sender: tokio::sync::mpsc::Sender<Envelope>) -> Self {
        Self {
            inner: RefInner::Local(sender),
            retry_policy: RetryPolicy::default(),
            replica_store: None,
            max_staleness: Duration::ZERO,
            _marker: PhantomData,
        }
    }

    pub(crate) fn remote(
        endpoint: String,
        grain_type: &'static str,
        grain_key: String,
        pool: Arc<ConnectionPool>,
    ) -> Self {
        Self {
            inner: RefInner::Remote {
                endpoint,
                grain_type,
                grain_key,
                pool,
            },
            retry_policy: RetryPolicy::default(),
            replica_store: None,
            max_staleness: Duration::ZERO,
            _marker: PhantomData,
        }
    }

    /// Override the retry policy for remote calls on this grain reference.
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Attach a replica store for serving read-only messages from local state
    /// on secondary clusters.
    pub fn with_replica_store(mut self, store: Arc<ReplicaStore>, max_staleness: Duration) -> Self {
        self.replica_store = Some(store);
        self.max_staleness = max_staleness;
        self
    }

    /// Send a message and wait for the result.
    ///
    /// If the grain lives on this silo, dispatches locally (same as `GrainRef::ask`).
    /// If the grain lives on a remote silo, serializes the message, sends it
    /// via gRPC, and deserializes the response.
    pub async fn ask<M>(&self, msg: M) -> Result<M::Result, GrainError>
    where
        M: NetworkMessage,
        G: GrainHandler<M>,
        M::Result: Serialize + DeserializeOwned,
    {
        match &self.inner {
            RefInner::Local(sender) => {
                let grain_ref = GrainRef::<G>::new(sender.clone());
                grain_ref.ask(msg).await
            }
            RefInner::Remote {
                endpoint,
                grain_type,
                grain_key,
                pool,
            } => {
                // Serialize once — bytes can be reused across retries since
                // M: Message doesn't require Clone.
                let payload =
                    bincode::serde::encode_to_vec(&msg, bincode::config::standard())
                        .map_err(|e| GrainError::RemoteCallFailed(e.to_string()))?;

                let mut last_error = None;

                for attempt in 0..=self.retry_policy.max_retries {
                    if attempt > 0 {
                        let delay = self.retry_policy.delay_for_attempt(attempt - 1);
                        tokio::time::sleep(delay).await;
                        tracing::debug!(
                            grain_type,
                            grain_key = grain_key.as_str(),
                            attempt,
                            "retrying remote grain call"
                        );
                    }

                    let result = Self::remote_invoke::<M>(
                        pool,
                        endpoint,
                        grain_type,
                        grain_key,
                        &payload,
                    )
                    .await;

                    match result {
                        Ok(value) => return Ok(value),
                        Err(e)
                            if attempt < self.retry_policy.max_retries
                                && RetryPolicy::is_retryable(&e) =>
                        {
                            last_error = Some(e);
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }

                Err(last_error.unwrap_or(GrainError::RemoteCallFailed(
                    "max retries exceeded".to_string(),
                )))
            }
        }
    }

    /// Send a read-only message. If a fresh-enough replica exists locally,
    /// the result is deserialized from the replica store without a network hop.
    /// Otherwise falls back to `ask()` (forwarding to the primary).
    ///
    /// Only usable with messages that implement `ReadOnlyMessage`.
    pub async fn read_only_ask<M>(&self, msg: M) -> Result<M::Result, GrainError>
    where
        M: NetworkMessage + ReadOnlyMessage,
        G: GrainHandler<M>,
        M::Result: Serialize + DeserializeOwned,
    {
        // Try the replica store first (only for remote refs with a store attached)
        if let (
            Some(store),
            RefInner::Remote {
                grain_type,
                grain_key,
                ..
            },
        ) = (&self.replica_store, &self.inner)
        {
            let staleness_millis = self.max_staleness.as_millis() as i64;
            if staleness_millis > 0 && store.is_fresh(grain_type, grain_key, staleness_millis) {
                if let Some(entry) = store.get(grain_type, grain_key) {
                    // Deserialize the full grain state — but we actually need the
                    // message result, not the grain state. For read-only messages
                    // served from replicas, we need a convention: the replica payload
                    // IS the serialized grain state. The caller can only use this
                    // path for messages whose Result can be derived from state.
                    //
                    // For now, we expose the raw replica payload as M::Result. This
                    // works when M::Result IS the grain state (e.g., GetState -> State).
                    // For messages that compute a derived value, fall through to ask().
                    match bincode::serde::decode_from_slice::<M::Result, _>(
                        &entry.payload,
                        bincode::config::standard(),
                    ) {
                        Ok((result, _)) => {
                            tracing::debug!(
                                grain_type,
                                grain_key = grain_key.as_str(),
                                sequence = entry.sequence,
                                "served read-only message from local replica"
                            );
                            return Ok(result);
                        }
                        Err(_) => {
                            // Deserialization failed — state format doesn't match
                            // M::Result. Fall through to primary.
                        }
                    }
                }
            }
        }

        // Fall back to standard ask (forwards to primary)
        self.ask(msg).await
    }

    /// Execute a single remote invoke attempt with pre-serialized payload bytes.
    async fn remote_invoke<M>(
        pool: &Arc<ConnectionPool>,
        endpoint: &str,
        grain_type: &str,
        grain_key: &str,
        payload: &[u8],
    ) -> Result<M::Result, GrainError>
    where
        M: NetworkMessage,
        M::Result: Serialize + DeserializeOwned,
    {
        let mut client = pool
            .get_transport(endpoint)
            .await
            .map_err(|e| GrainError::RemoteCallFailed(e.to_string()))?;

        let response = client
            .invoke(pool.authorized_request(InvokeRequest {
                grain_type: grain_type.to_string(),
                grain_key: grain_key.to_string(),
                message_type: M::message_type_name().to_string(),
                payload: payload.to_vec(),
                encoding: 0, // Encoding::Bincode — silo-to-silo always uses bincode
                request_context: RequestContext::current().to_map(),
                message_version: M::message_version(),
            }))
            .await
            .map_err(|e| GrainError::RemoteCallFailed(e.to_string()))?;

        let inner = response.into_inner();
        if !inner.error.is_empty() {
            return Err(GrainError::RemoteCallFailed(inner.error));
        }

        let (result, _): (M::Result, _) = bincode::serde::decode_from_slice(
            &inner.payload,
            bincode::config::standard(),
        )
        .map_err(|e| GrainError::RemoteCallFailed(e.to_string()))?;

        Ok(result)
    }
}
