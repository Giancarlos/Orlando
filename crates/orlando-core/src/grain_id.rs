use std::fmt;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GrainId {
    pub type_name: &'static str,
    pub key: String,
}

impl fmt::Display for GrainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.type_name, self.key)
    }
}
