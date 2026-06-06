use std::fmt;

/// Identity of a grain: its type name plus an application-chosen key.
/// `(type_name, key)` uniquely addresses one virtual actor (e.g. `Counter`/`"room-42"`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GrainId {
    /// The grain type's stable name (see `Grain::grain_type_name`).
    pub type_name: &'static str,
    /// The application-chosen key identifying this instance within the type.
    pub key: String,
}

impl fmt::Display for GrainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.type_name, self.key)
    }
}
