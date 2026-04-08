use std::marker::PhantomData;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};

use orlando_core::{Envelope, Grain, GrainError, GrainHandler, GrainRef, RequestContext};

use crate::connection_pool::ConnectionPool;
use crate::network_message::NetworkMessage;
use crate::proto::InvokeRequest;
use crate::retry::RetryPolicy;

pub struct ClusterGrainRef<G: Grain> {
    inner: RefInner,
    retry_policy: RetryPolicy,
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
            _marker: PhantomData,
        }
    }
}

impl<G: Grain> ClusterGrainRef<G> {
    pub(crate) fn local(sender: tokio::sync::mpsc::Sender<Envelope>) -> Self {
        Self {
            inner: RefInner::Local(sender),
            retry_policy: RetryPolicy::default(),
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
            _marker: PhantomData,
        }
    }

    /// Override the retry policy for remote calls on this grain reference.
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
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
