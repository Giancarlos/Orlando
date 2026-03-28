use orlando_core::Message;
use serde::{de::DeserializeOwned, Serialize};

/// A message that can be sent across silo boundaries.
///
/// Extends `Message` with serialization support and a stable type name
/// used for registry-based dispatch on the receiving silo.
pub trait NetworkMessage: Message + Serialize + DeserializeOwned
where
    Self::Result: Serialize + DeserializeOwned,
{
    /// Stable name for this message type (used as the key in the message registry).
    fn message_type_name() -> &'static str;
}
