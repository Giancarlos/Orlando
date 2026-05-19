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
    /// Maps message_type_name -> highest supported version for that message type.
    message_versions: HashMap<String, u32>,
    /// Data residency constraints: grain_type_name -> allowed cluster IDs.
    /// If the slice is empty, the grain can be activated in any cluster.
    allowed_clusters: HashMap<String, &'static [&'static str]>,
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
            message_versions: HashMap::new(),
            allowed_clusters: HashMap::new(),
        }
    }

    /// Look up the `&'static str` for a registered grain type name.
    pub fn grain_type_str(&self, grain_type: &str) -> Option<&'static str> {
        self.grain_types.get(grain_type).copied()
    }

    /// Get data residency constraints for a grain type.
    /// Returns `None` if the grain has no restrictions (activate anywhere).
    /// Returns `Some(clusters)` if the grain is pinned to specific clusters.
    pub fn allowed_clusters(&self, grain_type: &str) -> Option<&'static [&'static str]> {
        self.allowed_clusters.get(grain_type).copied()
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
        self.message_versions
            .insert(message_type.to_string(), M::message_version());

        // Record data residency constraints if present
        if let Some(clusters) = G::allowed_clusters() {
            self.allowed_clusters
                .insert(grain_type.to_string(), clusters);
        }

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

    /// Resolve a grain type string to its static str reference.
    /// Falls back to a leaked string if the grain type is not registered.
    pub fn resolve_grain_type(&self, grain_type: &str) -> &'static str {
        if let Some(&s) = self.grain_types.get(grain_type) {
            return s;
        }
        // Fallback: leak the string so we get a &'static str.
        // This only happens for unknown grain types (which will fail dispatch anyway).
        Box::leak(grain_type.to_string().into_boxed_str())
    }

    /// Dispatch an incoming remote call to the registered handler.
    ///
    /// `message_version` is the version the sender encoded the message with.
    /// If it is newer than the version this silo supports, dispatch returns
    /// `UnsupportedMessageVersion` so rolling deploys degrade gracefully.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch(
        &self,
        grain_type: &str,
        grain_key: String,
        message_type: &str,
        message_version: u32,
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

        // Reject messages whose version is newer than what this silo supports.
        // Older versions (message_version <= supported) are accepted for backward compat.
        if let Some(&supported_version) = self.message_versions.get(message_type)
            && message_version > supported_version
        {
            return Err(ClusterError::UnsupportedMessageVersion(
                message_type.to_string(),
                message_version,
                supported_version,
            ));
        }

        let handler = self
            .handlers
            .get(&(*type_name, *msg_name))
            .ok_or_else(|| {
                ClusterError::UnknownMessageType(format!("{}::{}", grain_type, message_type))
            })?;

        handler(grain_key, payload, encoding, request_context, activator).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orlando_core::{Envelope, Grain, GrainHandler, GrainId, Message};
    use serde::{Deserialize, Serialize};
    use tokio::task::JoinHandle;

    // -- Minimal test grain + message --

    struct TestGrain;

    #[async_trait::async_trait]
    impl Grain for TestGrain {
        type State = ();

        fn grain_type_name() -> &'static str {
            "TestGrain"
        }
    }

    #[derive(Serialize, Deserialize)]
    struct TestMsg;

    impl Message for TestMsg {
        type Result = ();
    }

    impl crate::network_message::NetworkMessage for TestMsg {
        fn message_type_name() -> &'static str {
            "TestMsg"
        }
        // default version = 0
    }

    #[async_trait::async_trait]
    impl GrainHandler<TestMsg> for TestGrain {
        async fn handle(
            _state: &mut Self::State,
            _msg: TestMsg,
            _ctx: &orlando_core::GrainContext,
        ) -> <TestMsg as Message>::Result {
        }
    }

    // A v2 message to test version rejection
    #[derive(Serialize, Deserialize)]
    struct TestMsgV2;

    impl Message for TestMsgV2 {
        type Result = ();
    }

    impl crate::network_message::NetworkMessage for TestMsgV2 {
        fn message_type_name() -> &'static str {
            "TestMsgV2"
        }
        fn message_version() -> u32 {
            2
        }
    }

    #[async_trait::async_trait]
    impl GrainHandler<TestMsgV2> for TestGrain {
        async fn handle(
            _state: &mut Self::State,
            _msg: TestMsgV2,
            _ctx: &orlando_core::GrainContext,
        ) -> <TestMsgV2 as Message>::Result {
        }
    }

    struct FakeActivator;

    impl GrainActivator for FakeActivator {
        fn get_sender(&self, _id: &GrainId) -> Option<tokio::sync::mpsc::Sender<Envelope>> {
            None
        }
        fn register(
            &self,
            _id: GrainId,
            _sender: tokio::sync::mpsc::Sender<Envelope>,
            _task: JoinHandle<()>,
        ) {
        }
        fn remove(&self, _id: &GrainId) {}
    }

    #[tokio::test]
    async fn default_version_zero_dispatch_succeeds() {
        let mut registry = MessageRegistry::new();
        registry.register::<TestGrain, TestMsg>();

        let payload =
            bincode::serde::encode_to_vec(&TestMsg, bincode::config::standard()).unwrap();

        // version 0 matches the default — should not be rejected by the version check
        let result = registry
            .dispatch(
                "TestGrain",
                "key-1".to_string(),
                "TestMsg",
                0,
                payload,
                Encoding::Bincode,
                HashMap::new(),
                Arc::new(FakeActivator),
            )
            .await;

        // The dispatch will succeed or fail inside the handler (activation),
        // but it must NOT fail with UnsupportedMessageVersion.
        match &result {
            Err(ClusterError::UnsupportedMessageVersion(..)) => {
                panic!("version 0 should not be rejected")
            }
            _ => {} // any other outcome is fine for this test
        }
    }

    #[tokio::test]
    async fn newer_version_than_supported_returns_error() {
        let mut registry = MessageRegistry::new();
        registry.register::<TestGrain, TestMsgV2>();

        let payload =
            bincode::serde::encode_to_vec(&TestMsgV2, bincode::config::standard()).unwrap();

        // Claim version 5 but the silo only supports v2
        let result = registry
            .dispatch(
                "TestGrain",
                "key-1".to_string(),
                "TestMsgV2",
                5,
                payload,
                Encoding::Bincode,
                HashMap::new(),
                Arc::new(FakeActivator),
            )
            .await;

        match result {
            Err(ClusterError::UnsupportedMessageVersion(name, got, supported)) => {
                assert_eq!(name, "TestMsgV2");
                assert_eq!(got, 5);
                assert_eq!(supported, 2);
            }
            other => panic!("expected UnsupportedMessageVersion, got {:?}", other.err()),
        }
    }

    #[tokio::test]
    async fn older_version_than_supported_is_accepted() {
        let mut registry = MessageRegistry::new();
        registry.register::<TestGrain, TestMsgV2>();

        let payload =
            bincode::serde::encode_to_vec(&TestMsgV2, bincode::config::standard()).unwrap();

        // Claim version 1 but the silo supports v2 — backward compat
        let result = registry
            .dispatch(
                "TestGrain",
                "key-1".to_string(),
                "TestMsgV2",
                1,
                payload,
                Encoding::Bincode,
                HashMap::new(),
                Arc::new(FakeActivator),
            )
            .await;

        match &result {
            Err(ClusterError::UnsupportedMessageVersion(..)) => {
                panic!("older version should be accepted for backward compatibility")
            }
            _ => {} // any other outcome is fine
        }
    }
}
