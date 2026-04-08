use tonic::Status;

/// Pluggable authentication for cluster gRPC endpoints.
///
/// Implement this to validate incoming requests. The metadata map contains
/// gRPC headers — check for tokens, API keys, or client certificates.
///
/// For silo-to-silo traffic, mTLS (configured via `ClusterSiloBuilder::tls()`)
/// is the recommended approach. This trait adds an additional application-level
/// check on top of transport-level security.
pub trait ClusterAuth: Send + Sync + 'static {
    /// Validate an incoming request. Return `Err(Status)` to reject.
    #[allow(clippy::result_large_err)]
    fn authenticate(&self, metadata: &tonic::metadata::MetadataMap) -> Result<(), Status>;
}

/// Shared-secret authentication. Validates that incoming requests carry
/// a specific token in the `authorization` gRPC metadata header.
pub struct SharedSecretAuth {
    expected: String,
}

impl SharedSecretAuth {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            expected: token.into(),
        }
    }
}

impl ClusterAuth for SharedSecretAuth {
    fn authenticate(&self, metadata: &tonic::metadata::MetadataMap) -> Result<(), Status> {
        match metadata.get("authorization") {
            Some(value) => {
                let value_str = value
                    .to_str()
                    .map_err(|_| Status::unauthenticated("invalid authorization header encoding"))?;
                if constant_time_eq(value_str.as_bytes(), self.expected.as_bytes()) {
                    Ok(())
                } else {
                    Err(Status::unauthenticated("invalid authorization token"))
                }
            }
            None => Err(Status::unauthenticated("missing authorization header")),
        }
    }
}

/// Constant-time byte comparison to prevent timing attacks on token validation.
/// Returns true only if both slices have the same length and identical contents.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

impl std::fmt::Debug for SharedSecretAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedSecretAuth")
            .field("token", &"[REDACTED]")
            .finish()
    }
}
