//! Auth dependency-inversion seam shared between `echo-agent-file-utils`
//! (the holder) and `echo-agent-runtime` (the implementer). Keeps shell types
//! out of data-collector's import graph while still letting refresh-aware
//! token resolution drive HTTP requests.

pub mod auth_provider;
pub mod bearer_fragment;
#[cfg(feature = "middleware")]
pub mod retry_middleware;
pub mod visibility;

pub use auth_provider::{AuthCredentialProvider, CredentialSnapshot, StaticAuthCredentialProvider};
pub use bearer_fragment::{BEARER_SUFFIX_LEN, bearer_suffix};
#[cfg(feature = "middleware")]
pub use retry_middleware::{AuthRetryMiddleware, StampedBearerSuffix, execute_with_stamp};
pub use visibility::HttpAuth;
