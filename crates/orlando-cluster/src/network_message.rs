use orlando_core::Message;
use serde::{de::DeserializeOwned, Serialize};

/// Payload encoding for grain messages on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Rust-native binary encoding. Fast, compact, Rust-only.
    /// Used for silo-to-silo internal traffic.
    Bincode,
    /// Protocol Buffers encoding. Cross-language.
    /// Used by external (non-Rust) clients.
    Protobuf,
}

impl Encoding {
    pub fn from_proto(val: i32) -> Self {
        match val {
            1 => Self::Protobuf,
            _ => Self::Bincode,
        }
    }

    pub fn to_proto(self) -> i32 {
        match self {
            Self::Bincode => 0,
            Self::Protobuf => 1,
        }
    }
}

/// A message that can be sent across silo boundaries.
///
/// Extends `Message` with serialization support and a stable type name
/// used for registry-based dispatch on the receiving silo.
///
/// By default, messages are serialized with bincode (serde). To support
/// external (non-Rust) clients, implement the `encode_proto`/`decode_proto`
/// methods or use `#[message(result = T, network, proto)]`.
pub trait NetworkMessage: Message + Serialize + DeserializeOwned
where
    Self::Result: Serialize + DeserializeOwned,
{
    /// Stable name for this message type (used as the key in the message registry).
    fn message_type_name() -> &'static str;

    /// Whether this message supports protobuf encoding for external clients.
    fn supports_proto() -> bool {
        false
    }

    /// Encode this message as protobuf. Returns `None` if not supported.
    fn encode_proto(&self) -> Option<Vec<u8>> {
        None
    }

    /// Decode this message from protobuf bytes. Returns `None` if not supported.
    fn decode_proto(_bytes: &[u8]) -> Option<Self> {
        None
    }

    /// Encode the result type as protobuf. Returns `None` if not supported.
    fn encode_result_proto(_result: &Self::Result) -> Option<Vec<u8>> {
        None
    }

    /// Decode the result type from protobuf bytes. Returns `None` if not supported.
    fn decode_result_proto(_bytes: &[u8]) -> Option<Self::Result> {
        None
    }
}
