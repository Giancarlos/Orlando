use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::mpsc;

use orlando_core::{GrainActivator, GrainHandler, GrainId, GrainRef, RequestContext, mailbox, reentrant_mailbox};

use crate::error::ClusterError;
use crate::network_message::{Encoding, NetworkMessage};

type DispatchFn = Arc<
    dyn Fn(
            String,
            Vec<u8>,
            Encoding,
            HashMap<String, String>,
            Arc<dyn GrainActivator>,
        ) -> Pin<Box<dyn Future<Output = Result<(Vec<u8>, Encoding), ClusterError>> + Send>>
        + Send
        + Sync,
>;

pub struct MessageRegistry {
    handlers: HashMap<(&'static str, &'static str), DispatchFn>,
    grain_types: HashMap<String, &'static str>,
    message_types: HashMap<String, &'static str>,
    /// Maps grain_type_name -> Rust type name, for collision detection.
    grain_rust_types: HashMap<String, &'static str>,
}

impl Default for MessageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            grain_types: HashMap::new(),
            message_types: HashMap::new(),
            grain_rust_types: HashMap::new(),
        }
    }

    /// Register a grain + message combination for remote dispatch.
    ///
    /// This captures the concrete types so the gRPC server can deserialize
    /// incoming requests, activate the grain, dispatch the message, and
    /// serialize the response — all without knowing `G` or `M` at call-site.
    pub fn register<G, M>(&mut self)
    where
        G: GrainHandler<M> + Sync,
        M: NetworkMessage,
        M::Result: Serialize + DeserializeOwned,
    {
        let grain_type: &'static str = G::grain_type_name();
        let message_type: &'static str = M::message_type_name();
        let rust_type: &'static str = std::any::type_name::<G>();

        // Detect grain type name collisions (two different Rust types with the same grain_type_name)
        if let Some(&existing_rust_type) = self.grain_rust_types.get(grain_type)
            && existing_rust_type != rust_type
        {
            panic!(
                "grain type name collision: \"{}\" is used by both {} and {}",
                grain_type, existing_rust_type, rust_type
            );
        }
        self.grain_rust_types
            .insert(grain_type.to_string(), rust_type);

        self.grain_types
            .insert(grain_type.to_string(), grain_type);
        self.message_types
            .insert(message_type.to_string(), message_type);

        let dispatch: DispatchFn = Arc::new(
            move |key: String,
                  payload: Vec<u8>,
                  encoding: Encoding,
                  request_context: HashMap<String, String>,
                  activator: Arc<dyn GrainActivator>| {
                Box::pin(async move {
                    // Deserialize based on encoding
                    let msg: M = match encoding {
                        Encoding::Bincode => {
                            let (msg, _) = bincode::serde::decode_from_slice(
                                &payload,
                                bincode::config::standard(),
                            )
                            .map_err(|e| ClusterError::Deserialization(e.to_string()))?;
                            msg
                        }
                        Encoding::Protobuf => M::decode_proto(&payload).ok_or_else(|| {
                            ClusterError::UnsupportedEncoding(
                                M::message_type_name().to_string(),
                                "protobuf not supported for this message type".to_string(),
                            )
                        })?,
                    };

                    let grain_id = GrainId {
                        type_name: grain_type,
                        key,
                    };

                    let activator_for_mailbox = activator.clone();
                    let sender = activator.get_or_insert(
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
                    );

                    let grain_ref = GrainRef::<G>::new(sender);
                    let req_ctx = RequestContext::with_values(request_context);
                    let result = req_ctx
                        .scope(grain_ref.ask(msg))
                        .await
                        .map_err(|e| ClusterError::HandlerError(e.to_string()))?;

                    // Serialize response in the same encoding
                    let response_bytes = match encoding {
                        Encoding::Bincode => {
                            bincode::serde::encode_to_vec(&result, bincode::config::standard())
                                .map_err(|e| ClusterError::Serialization(e.to_string()))?
                        }
                        Encoding::Protobuf => {
                            M::encode_result_proto(&result).ok_or_else(|| {
                                ClusterError::UnsupportedEncoding(
                                    M::message_type_name().to_string(),
                                    "protobuf not supported for result type".to_string(),
                                )
                            })?
                        }
                    };

                    Ok((response_bytes, encoding))
                })
            },
        );

        self.handlers.insert((grain_type, message_type), dispatch);
    }

    /// Dispatch an incoming remote call to the registered handler.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch(
        &self,
        grain_type: &str,
        grain_key: String,
        message_type: &str,
        payload: Vec<u8>,
        encoding: Encoding,
        request_context: HashMap<String, String>,
        activator: Arc<dyn GrainActivator>,
    ) -> Result<(Vec<u8>, Encoding), ClusterError> {
        let type_name = self
            .grain_types
            .get(grain_type)
            .ok_or_else(|| ClusterError::UnknownGrainType(grain_type.to_string()))?;

        let msg_name = self
            .message_types
            .get(message_type)
            .ok_or_else(|| ClusterError::UnknownMessageType(message_type.to_string()))?;

        let handler = self
            .handlers
            .get(&(*type_name, *msg_name))
            .ok_or_else(|| {
                ClusterError::UnknownMessageType(format!("{}::{}", grain_type, message_type))
            })?;

        handler(grain_key, payload, encoding, request_context, activator).await
    }
}
