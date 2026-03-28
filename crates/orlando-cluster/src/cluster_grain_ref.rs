use std::marker::PhantomData;

use serde::{de::DeserializeOwned, Serialize};

use orlando_core::{Envelope, Grain, GrainError, GrainHandler, GrainRef};

use crate::network_message::NetworkMessage;
use crate::proto::grain_transport_client::GrainTransportClient;
use crate::proto::InvokeRequest;

pub struct ClusterGrainRef<G: Grain> {
    inner: RefInner,
    _marker: PhantomData<G>,
}

enum RefInner {
    Local(tokio::sync::mpsc::Sender<Envelope>),
    Remote {
        endpoint: String,
        grain_type: &'static str,
        grain_key: String,
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
            } => Self::Remote {
                endpoint: endpoint.clone(),
                grain_type,
                grain_key: grain_key.clone(),
            },
        }
    }
}

impl<G: Grain> Clone for ClusterGrainRef<G> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

impl<G: Grain> ClusterGrainRef<G> {
    pub(crate) fn local(sender: tokio::sync::mpsc::Sender<Envelope>) -> Self {
        Self {
            inner: RefInner::Local(sender),
            _marker: PhantomData,
        }
    }

    pub(crate) fn remote(
        endpoint: String,
        grain_type: &'static str,
        grain_key: String,
    ) -> Self {
        Self {
            inner: RefInner::Remote {
                endpoint,
                grain_type,
                grain_key,
            },
            _marker: PhantomData,
        }
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
            } => {
                let payload =
                    bincode::serde::encode_to_vec(&msg, bincode::config::standard())
                        .map_err(|e| GrainError::RemoteCallFailed(e.to_string()))?;

                let mut client =
                    GrainTransportClient::connect(format!("http://{}", endpoint))
                        .await
                        .map_err(|e| GrainError::RemoteCallFailed(e.to_string()))?;

                let response = client
                    .invoke(InvokeRequest {
                        grain_type: grain_type.to_string(),
                        grain_key: grain_key.clone(),
                        message_type: M::message_type_name().to_string(),
                        payload,
                    })
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
    }
}
