//! Auth credentials and pool-dedup principal keys.
//!
//! [`AuthCredential`] models the credential the client attaches at
//! handshake time. Two variants are supported:
//!
//! - [`AuthCredential::Bearer`] for the simple "Authorization: Bearer
//!   …" path (e.g. JWT-against-OAuth2 deployments).
//! - [`AuthCredential::Headers`] for callers that already hold a
//!   pre-built header bundle (e.g. signed identity headers generated
//!   by an upstream proxy or test harness).
//!
//! [`PrincipalKey`] is the stable hashable projection of an
//! `AuthCredential`; the pool keys connections by
//! `(url, principal_key)` so two [`crate::ToolServer`] builds with the
//! same credential reuse one socket while distinct credentials open
//! distinct sockets. The server derives `user_id` from the credential at
//! upgrade time and returns it in the hello ack — the SDK never needs
//! to carry `user_id` alongside the credential.
//!
//! ## Pool dedup and credential refresh
//!
//! Both variants include the secret material in the `PrincipalKey`
//! fingerprint. This is deliberate: distinct secrets imply distinct
//! credentials, so two callers with different tokens open distinct
//! sockets. The trade-off is that a caller that rotates its bearer JWT
//! every N minutes will open a new socket on each rotation.
//! Long-running tool servers should reuse the SAME [`AuthCredential`]
//! instance across builds and refresh the credential out-of-band rather
//! than hand a fresh JWT to every build.

use std::collections::BTreeMap;
use std::fmt;

use http::HeaderName;
use http::header::AUTHORIZATION;

use crate::error::ClientError;

/// Credential carried into the WebSocket upgrade.
///
/// Clones are cheap (the secret material is at most a small number of
/// owned strings). The server derives `user_id` from the credential at
/// upgrade time and returns it in the [`echo_agent_tool_protocol::HelloAckMsg`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum AuthCredential {
    /// Bearer token attached as the `Authorization: Bearer …` header.
    Bearer { token: String },
    /// Pre-built header bundle. Used when the auth flow lives outside
    /// the SDK (e.g. an upstream proxy that already produced signed
    /// identity headers). Header order is canonicalised for stable
    /// hashing via [`BTreeMap`]; names are lowercased and validated
    /// as `HeaderName` at construction time so an invalid name
    /// surfaces as [`ClientError::InvalidConfig`] instead of being
    /// silently dropped at upgrade time.
    Headers { headers: BTreeMap<String, String> },
}

impl AuthCredential {
    /// Convenience constructor for the bearer-token shape.
    pub fn bearer(token: impl Into<String>) -> Self {
        Self::Bearer {
            token: token.into(),
        }
    }

    /// Convenience constructor for the raw-header bundle shape.
    ///
    /// Names are canonicalised to lowercase and validated as
    /// [`HeaderName`] at construction so an invalid header (e.g. one
    /// containing a newline injection attempt) returns
    /// [`ClientError::InvalidConfig`] rather than being silently
    /// filtered out at upgrade time.
    pub fn headers<I, K, V>(headers: I) -> Result<Self, ClientError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for (raw_name, raw_value) in headers {
            let name = raw_name.as_ref().to_ascii_lowercase();
            HeaderName::from_bytes(name.as_bytes()).map_err(|err| {
                ClientError::InvalidConfig(format!("invalid header name {name:?}: {err}"))
            })?;
            map.insert(name, raw_value.into());
        }
        Ok(Self::Headers { headers: map })
    }

    /// Stable hashable projection used as the pool dedup key.
    ///
    /// Distinct credentials hash equal iff they carry the same secret
    /// material. See the module-level "Pool dedup and credential
    /// refresh" section for the implications when bearer tokens are
    /// rotated.
    pub fn principal_key(&self) -> PrincipalKey {
        match self {
            Self::Bearer { token } => PrincipalKey {
                fingerprint: format!("bearer:{token}"),
            },
            Self::Headers { headers } => {
                // Concatenate canonicalised name=value pairs so the
                // fingerprint is order-independent.
                let mut joined = String::with_capacity(headers.len() * 32);
                for (name, value) in headers {
                    joined.push_str(name);
                    joined.push('=');
                    joined.push_str(value);
                    joined.push('\n');
                }
                PrincipalKey {
                    fingerprint: format!("headers:{joined}"),
                }
            }
        }
    }

    /// Headers to attach to the WebSocket upgrade request.
    ///
    /// `Headers` variant entries are infallible at this point — names
    /// were validated by [`Self::headers`].
    pub fn upgrade_headers(&self) -> Vec<(HeaderName, String)> {
        match self {
            Self::Bearer { token, .. } => {
                vec![(AUTHORIZATION, format!("Bearer {token}"))]
            }
            Self::Headers { headers, .. } => headers
                .iter()
                .filter_map(|(name, value)| {
                    HeaderName::from_bytes(name.as_bytes())
                        .ok()
                        .map(|n| (n, value.clone()))
                })
                .collect(),
        }
    }
}

impl fmt::Debug for AuthCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never log the secret; surface only the variant.
        match self {
            Self::Bearer { .. } => f
                .debug_struct("AuthCredential::Bearer")
                .finish_non_exhaustive(),
            Self::Headers { headers } => f
                .debug_struct("AuthCredential::Headers")
                .field("header_count", &headers.len())
                .finish_non_exhaustive(),
        }
    }
}

/// Stable hashable projection of an [`AuthCredential`] used as the
/// pool dedup key alongside the connect URL. Two connections with the
/// same token fingerprint will get the same server-assigned `user_id`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PrincipalKey {
    fingerprint: String,
}

impl PrincipalKey {
    /// Stable non-secret fingerprint (e.g. OIDC issuer+client); never tokens.
    pub fn opaque(fingerprint: impl Into<String>) -> Self {
        Self {
            fingerprint: fingerprint.into(),
        }
    }
}

impl fmt::Debug for PrincipalKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrincipalKey").finish_non_exhaustive()
    }
}

/// Owner identity surfaced by an [`AuthProvider`] alongside its credential.
///
/// Mirrors the OAuth principal fields the provider parsed from its auth source.
/// It is kept separate from [`AuthCredential`] on purpose: identity must NOT
/// participate in pool-dedup hashing (that keys only on the secret), and the
/// credential's `Eq`/`Hash` derives must stay token-only. Consumers (e.g. the
/// workspace) map this onto their own identity record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthIdentity {
    /// Stable user identifier (owner of the bearer token).
    pub user_id: String,
    /// OAuth `principal_type` wire string (`"User"` / `"Team"`), when known.
    pub principal_type: Option<String>,
    /// Team id when `principal_type == "Team"`; otherwise `None`.
    pub principal_id: Option<String>,
}

/// Credential provider called on every connect/reconnect.
pub trait AuthProvider: Send + Sync + std::fmt::Debug {
    fn current(&self) -> AuthCredential;

    /// Stable pool-dedup key, decoupled from the per-connect credential.
    ///
    /// Defaults to the current credential's key (existing behavior). A provider
    /// that re-mints a rotating secret on every [`Self::current`] call (e.g. a
    /// refresh-before-use bearer) MUST override this to key only on stable
    /// identity, otherwise each rotation fragments the connection pool.
    fn principal_key(&self) -> PrincipalKey {
        self.current().principal_key()
    }

    /// Owner identity behind the credential, when the provider can surface it.
    ///
    /// Defaults to `None` for providers that only carry a bearer token (e.g. a
    /// bare [`AuthCredential`]). Providers that parse OAuth principal fields
    /// (e.g. OIDC) override this so downstream consumers can attribute
    /// requests without a second auth-source read.
    fn identity(&self) -> Option<AuthIdentity> {
        None
    }
}

pub type SharedAuthProvider = std::sync::Arc<dyn AuthProvider>;

impl AuthProvider for AuthCredential {
    fn current(&self) -> AuthCredential {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_header_name_rejected_at_construction() {
        let cred = AuthCredential::headers([("authorization\nx-injected", "value")]);
        match cred {
            Err(ClientError::InvalidConfig(msg)) => {
                assert!(msg.contains("invalid header name"), "got {msg}")
            }
            other => panic!("expected InvalidConfig; got {other:?}"),
        }
    }

    #[test]
    fn valid_headers_accepted() {
        let cred = AuthCredential::headers([("authorization", "Bearer token")]).expect("valid");
        assert_eq!(cred.upgrade_headers().len(), 1);
    }
}
