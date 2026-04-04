use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use prost::Message as ProstMessage;
use serde::{Deserialize, Serialize};

use orlando_cluster::{ClusterSilo, Encoding, NetworkMessage};
use orlando_core::{Grain, GrainContext, GrainHandler, Message};

// --- Grain ---

struct ProtoCounter;

#[derive(Default)]
struct ProtoCounterState {
    count: i64,
}

impl Grain for ProtoCounter {
    type State = ProtoCounterState;
}

// --- Bincode-only message (existing pattern) ---

#[derive(Serialize, Deserialize)]
struct BincodeIncrement {
    amount: i64,
}

impl Message for BincodeIncrement {
    type Result = i64;
}

impl NetworkMessage for BincodeIncrement {
    fn message_type_name() -> &'static str {
        "BincodeIncrement"
    }
}

// --- Protobuf-capable message ---

#[derive(Clone, Serialize, Deserialize, ProstMessage)]
struct ProtoIncrement {
    #[prost(int64, tag = "1")]
    amount: i64,
}

impl Message for ProtoIncrement {
    type Result = ProtoResult;
}

#[derive(Clone, Serialize, Deserialize, ProstMessage)]
struct ProtoResult {
    #[prost(int64, tag = "1")]
    count: i64,
}

impl NetworkMessage for ProtoIncrement {
    fn message_type_name() -> &'static str {
        "ProtoIncrement"
    }

    fn supports_proto() -> bool {
        true
    }

    fn encode_proto(&self) -> Option<Vec<u8>> {
        Some(ProstMessage::encode_to_vec(self))
    }

    fn decode_proto(bytes: &[u8]) -> Option<Self> {
        <Self as ProstMessage>::decode(bytes).ok()
    }

    fn encode_result_proto(result: &ProtoResult) -> Option<Vec<u8>> {
        Some(ProstMessage::encode_to_vec(result))
    }

    fn decode_result_proto(bytes: &[u8]) -> Option<ProtoResult> {
        <ProtoResult as ProstMessage>::decode(bytes).ok()
    }
}

// --- Handlers ---

#[async_trait]
impl GrainHandler<BincodeIncrement> for ProtoCounter {
    async fn handle(
        state: &mut ProtoCounterState,
        msg: BincodeIncrement,
        _ctx: &GrainContext,
    ) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<ProtoIncrement> for ProtoCounter {
    async fn handle(
        state: &mut ProtoCounterState,
        msg: ProtoIncrement,
        _ctx: &GrainContext,
    ) -> ProtoResult {
        state.count += msg.amount;
        ProtoResult { count: state.count }
    }
}

// --- Tests ---

#[tokio::test]
async fn bincode_round_trip_unchanged() {
    // Existing silo-to-silo path (bincode) should work identically
    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("bincode-test")
        .register::<ProtoCounter, BincodeIncrement>()
        .build();

    let grain = silo.get_ref::<ProtoCounter>("c1");
    let result = grain.ask(BincodeIncrement { amount: 5 }).await.unwrap();
    assert_eq!(result, 5);
}

#[tokio::test]
async fn default_encoding_is_bincode() {
    // An InvokeRequest without encoding set should default to BINCODE (0)
    let encoding = Encoding::from_proto(0);
    assert_eq!(encoding, Encoding::Bincode);

    // Unknown values also default to bincode
    let encoding = Encoding::from_proto(99);
    assert_eq!(encoding, Encoding::Bincode);
}

#[tokio::test]
async fn protobuf_round_trip_via_grpc() {
    use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
    use orlando_cluster::proto::InvokeRequest;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let silo = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port)
            .silo_id("proto-test")
            .register::<ProtoCounter, ProtoIncrement>()
            .register::<ProtoCounter, BincodeIncrement>()
            .build(),
    );

    let silo_clone = silo.clone();
    let server = tokio::spawn(async move { silo_clone.serve().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a protobuf-encoded message as an external client would
    let mut client = GrainTransportClient::connect(format!("http://127.0.0.1:{}", port))
        .await
        .unwrap();

    let proto_msg = ProtoIncrement { amount: 42 };
    let payload = ProstMessage::encode_to_vec(&proto_msg);

    let response = client
        .invoke(InvokeRequest {
            grain_type: std::any::type_name::<ProtoCounter>().to_string(),
            grain_key: "proto-grain-1".to_string(),
            message_type: "ProtoIncrement".to_string(),
            payload,
            encoding: 1, // PROTOBUF
            request_context: std::collections::HashMap::new(),
        })
        .await
        .unwrap();

    let inner = response.into_inner();
    assert!(inner.error.is_empty(), "unexpected error: {}", inner.error);
    assert_eq!(inner.encoding, 1, "response should use protobuf encoding");

    // Decode the protobuf result
    let result = <ProtoResult as ProstMessage>::decode(inner.payload.as_slice()).unwrap();
    assert_eq!(result.count, 42);

    server.abort();
}

#[tokio::test]
async fn protobuf_to_bincode_only_message_returns_error() {
    use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
    use orlando_cluster::proto::InvokeRequest;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let silo = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port)
            .silo_id("proto-fail-test")
            .register::<ProtoCounter, BincodeIncrement>()
            .build(),
    );

    let silo_clone = silo.clone();
    let server = tokio::spawn(async move { silo_clone.serve().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = GrainTransportClient::connect(format!("http://127.0.0.1:{}", port))
        .await
        .unwrap();

    // Send a protobuf request to a bincode-only message type
    let response = client
        .invoke(InvokeRequest {
            grain_type: std::any::type_name::<ProtoCounter>().to_string(),
            grain_key: "fail-grain".to_string(),
            message_type: "BincodeIncrement".to_string(),
            payload: vec![1, 2, 3], // garbage protobuf bytes
            encoding: 1,            // PROTOBUF
            request_context: std::collections::HashMap::new(),
        })
        .await
        .unwrap();

    let inner = response.into_inner();
    assert!(
        inner.error.contains("unsupported encoding"),
        "expected unsupported encoding error, got: {}",
        inner.error
    );

    server.abort();
}

#[tokio::test]
async fn protobuf_multiple_calls_accumulate_state() {
    use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
    use orlando_cluster::proto::InvokeRequest;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let silo = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port)
            .silo_id("proto-multi")
            .register::<ProtoCounter, ProtoIncrement>()
            .build(),
    );

    let silo_clone = silo.clone();
    let server = tokio::spawn(async move { silo_clone.serve().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = GrainTransportClient::connect(format!("http://127.0.0.1:{}", port))
        .await
        .unwrap();

    // Send 3 increments to the same grain: +10, +20, +30
    for i in 1..=3 {
        let payload = ProstMessage::encode_to_vec(&ProtoIncrement { amount: i * 10 });
        let response = client
            .invoke(InvokeRequest {
                grain_type: std::any::type_name::<ProtoCounter>().to_string(),
                grain_key: "accumulate".to_string(),
                message_type: "ProtoIncrement".to_string(),
                payload,
                encoding: 1,
                    request_context: std::collections::HashMap::new(),
            })
            .await
            .unwrap();

        let inner = response.into_inner();
        assert!(inner.error.is_empty());
    }

    // Verify final state: 10 + 20 + 30 = 60
    let payload = ProstMessage::encode_to_vec(&ProtoIncrement { amount: 0 });
    let response = client
        .invoke(InvokeRequest {
            grain_type: std::any::type_name::<ProtoCounter>().to_string(),
            grain_key: "accumulate".to_string(),
            message_type: "ProtoIncrement".to_string(),
            payload,
            encoding: 1,
                    request_context: std::collections::HashMap::new(),
        })
        .await
        .unwrap();

    let result =
        <ProtoResult as ProstMessage>::decode(response.into_inner().payload.as_slice()).unwrap();
    assert_eq!(result.count, 60);

    server.abort();
}
