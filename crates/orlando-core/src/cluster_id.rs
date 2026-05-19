use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifies a cluster within a multi-cluster (GSI) deployment.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClusterId(pub String);

impl ClusterId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClusterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ClusterId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ClusterId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
