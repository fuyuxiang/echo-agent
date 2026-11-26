//! Pure policy modeling for future child website egress.
//!
//! These types are not selected by sandbox profiles or enforced by the current
//! runtime. Constructing a policy does not grant or restrict network access.

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

/// Version of the compact JSON produced by [`NetworkPolicySnapshot`].
pub const NETWORK_POLICY_SNAPSHOT_VERSION: u32 = 1;

/// Requested child-network behavior for future enforcement backends.
///
/// This is not currently selected or enforced by the sandbox runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "policy", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChildNetworkPolicy {
    Unrestricted,
    Blocked,
    Websites(WebsitePolicy),
}

impl ChildNetworkPolicy {
    pub fn from_restrict_network(restrict_network: bool) -> Self {
        if restrict_network {
            Self::Blocked
        } else {
            Self::Unrestricted
        }
    }
}

/// Result of exact-origin website policy evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WebsiteAction {
    Allow,
    Deny,
}

/// Exact HTTP(S) origin with an IDNA ASCII hostname and effective nonzero port.
///
/// Equality never includes subdomains, redirects, paths, or another scheme or
/// port.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WebsiteOrigin {
    scheme: String,
    hostname: String,
    port: u16,
}

impl WebsiteOrigin {
    /// Parses only `http://authority`, `https://authority`, or either with `/`.
    pub fn parse(value: &str) -> Result<Self, WebsiteOriginError> {
        validate_raw_origin(value)?;
        let url = Url::parse(value).map_err(|error| WebsiteOriginError::InvalidOrigin {
            value: value.to_owned(),
            reason: error.to_string(),
        })?;
        let scheme = url.scheme();
        let hostname = match url.host() {
            Some(Host::Domain(hostname)) => hostname.strip_suffix('.').unwrap_or(hostname),
            Some(Host::Ipv4(_) | Host::Ipv6(_)) => return Err(WebsiteOriginError::IpLiteral),
            None => return Err(WebsiteOriginError::InvalidSyntax),
        };
        let hostname = hostname.to_ascii_lowercase();
        if !valid_dns_hostname(&hostname) || hostname.parse::<IpAddr>().is_ok() {
            return Err(WebsiteOriginError::InvalidHost(hostname));
        }
        let port = url
            .port_or_known_default()
            .ok_or(WebsiteOriginError::InvalidSyntax)?;
        if port == 0 {
            return Err(WebsiteOriginError::PortZero);
        }

        Ok(Self {
            scheme: scheme.to_owned(),
            hostname,
            port,
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

fn validate_raw_origin(value: &str) -> Result<(), WebsiteOriginError> {
    if value.bytes().any(|byte| byte <= b' ' || byte == 0x7f) {
        return Err(WebsiteOriginError::InvalidSyntax);
    }
    if value.contains('\\') {
        return Err(WebsiteOriginError::InvalidSyntax);
    }
    let authority = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .ok_or(WebsiteOriginError::InvalidSyntax)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
    {
        return Err(WebsiteOriginError::InvalidSyntax);
    }
    if authority.contains('@') {
        return Err(WebsiteOriginError::Userinfo);
    }
    if authority.contains('*') {
        return Err(WebsiteOriginError::Wildcard);
    }
    Ok(())
}

fn valid_dns_hostname(hostname: &str) -> bool {
    !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

impl fmt::Display for WebsiteOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}:{}", self.scheme, self.hostname, self.port)
    }
}

impl FromStr for WebsiteOrigin {
    type Err = WebsiteOriginError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for WebsiteOrigin {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for WebsiteOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Immutable exact-origin rules with deny precedence over allow and default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebsitePolicy {
    default: WebsiteAction,
    allow: BTreeSet<WebsiteOrigin>,
    deny: BTreeSet<WebsiteOrigin>,
}

impl WebsitePolicy {
    pub fn new(
        default: WebsiteAction,
        allow: impl IntoIterator<Item = WebsiteOrigin>,
        deny: impl IntoIterator<Item = WebsiteOrigin>,
    ) -> Self {
        Self {
            default,
            allow: allow.into_iter().collect(),
            deny: deny.into_iter().collect(),
        }
    }

    pub fn default_action(&self) -> WebsiteAction {
        self.default
    }

    pub fn allow(&self) -> &BTreeSet<WebsiteOrigin> {
        &self.allow
    }

    pub fn deny(&self) -> &BTreeSet<WebsiteOrigin> {
        &self.deny
    }

    /// Evaluates deny exact match, then allow exact match, then the default.
    pub fn evaluate(&self, origin: &WebsiteOrigin) -> WebsiteAction {
        if self.deny.contains(origin) {
            WebsiteAction::Deny
        } else if self.allow.contains(origin) {
            WebsiteAction::Allow
        } else {
            self.default
        }
    }
}

/// Versioned deterministic JSON and SHA-256 identity for later persistence.
///
/// The snapshot is not currently written to sessions or used for enforcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkPolicySnapshot {
    version: u32,
    policy: ChildNetworkPolicy,
}

impl NetworkPolicySnapshot {
    pub fn new(policy: ChildNetworkPolicy) -> Self {
        Self {
            version: NETWORK_POLICY_SNAPSHOT_VERSION,
            policy,
        }
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn policy(&self) -> &ChildNetworkPolicy {
        &self.policy
    }

    pub fn into_policy(self) -> ChildNetworkPolicy {
        self.policy
    }

    /// Serializes the stable compact JSON representation for this version.
    pub fn canonical_json(&self) -> Result<String, NetworkPolicySnapshotError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Returns SHA-256 hex over [`Self::canonical_json`].
    pub fn sha256(&self) -> Result<String, NetworkPolicySnapshotError> {
        Ok(format!(
            "{:x}",
            Sha256::digest(self.canonical_json()?.as_bytes())
        ))
    }

    pub fn validate_sha256(&self, expected: &str) -> Result<bool, NetworkPolicySnapshotError> {
        Ok(self.sha256()?.eq_ignore_ascii_case(expected))
    }

    /// Decodes the version envelope before interpreting its policy payload.
    pub fn from_canonical_json(value: &str) -> Result<Self, NetworkPolicySnapshotError> {
        #[derive(Deserialize)]
        struct RawSnapshot {
            version: u32,
            policy: serde_json::Value,
        }

        let raw: RawSnapshot = serde_json::from_str(value)?;
        if raw.version != NETWORK_POLICY_SNAPSHOT_VERSION {
            return Err(NetworkPolicySnapshotError::UnsupportedVersion(raw.version));
        }
        Ok(Self {
            version: raw.version,
            policy: serde_json::from_value(raw.policy)?,
        })
    }
}

/// Validation failures for strict raw exact-origin syntax.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum WebsiteOriginError {
    #[error("website origin must use exact http(s)://authority syntax with optional '/'")]
    InvalidSyntax,
    #[error("invalid website origin '{value}': {reason}")]
    InvalidOrigin { value: String, reason: String },
    #[error("website origin must not contain userinfo")]
    Userinfo,
    #[error("website origin must not contain wildcards")]
    Wildcard,
    #[error("website origin must not use an IP literal")]
    IpLiteral,
    #[error("invalid website origin hostname '{0}'")]
    InvalidHost(String),
    #[error("website origin port must be nonzero")]
    PortZero,
}

/// Snapshot encoding, decoding, and version failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NetworkPolicySnapshotError {
    #[error("invalid network policy snapshot: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unsupported network policy snapshot version {0}")]
    UnsupportedVersion(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(value: &str) -> WebsiteOrigin {
        WebsiteOrigin::parse(value).unwrap()
    }

    #[test]
    fn normalizes_default_ports_case_trailing_dot_and_idna() {
        let http = origin("http://Example.COM");
        assert_eq!(http, origin("http://example.com:80/"));
        assert_eq!(http.scheme(), "http");
        assert_eq!(http.port(), 80);

        let https = origin("https://example.com.");
        assert_eq!(https, origin("https://EXAMPLE.com:443"));
        assert_eq!(https.port(), 443);
        assert_eq!(
            origin("https://bücher.example").hostname(),
            "xn--bcher-kva.example"
        );
        assert_eq!(origin("https://example.com:8443").port(), 8443);
    }

    #[test]
    fn rejects_non_origin_inputs() {
        let cases = [
            ("ftp://example.com", "exact http(s)"),
            ("https://127.0.0.1", "IP literal"),
            ("https://[::1]", "IP literal"),
            ("https://user@example.com", "userinfo"),
            ("https://@example.com", "userinfo"),
            ("https://:@example.com", "userinfo"),
            ("https://example.com/path", "exact http(s)"),
            ("https://example.com/?query=1", "exact http(s)"),
            ("https://example.com/#fragment", "exact http(s)"),
            ("https://*.example.com", "wildcards"),
            ("https://example.*", "wildcards"),
            ("https://example.com:0", "nonzero"),
            (
                "https://bad_host.example",
                "invalid website origin hostname",
            ),
            ("https://", "exact http(s)"),
        ];

        for (value, expected) in cases {
            let error = WebsiteOrigin::parse(value).unwrap_err().to_string();
            assert!(error.contains(expected), "{value}: {error}");
        }
    }

    #[test]
    fn rejects_url_parser_repairs_and_ignored_characters() {
        for value in [
            "https:example.com",
            "https:/example.com",
            "https:///example.com",
            "https:\\example.com",
            "https://example.com/..",
            " https://example.com",
            "https://example.com ",
            "https://exam\tple.com",
            "https://example.com\n",
        ] {
            assert_eq!(
                WebsiteOrigin::parse(value),
                Err(WebsiteOriginError::InvalidSyntax),
                "{value:?}"
            );
        }
    }

    #[test]
    fn evaluates_exact_origin_with_deny_precedence() {
        let exact = origin("https://example.com");
        let allowed = origin("https://allowed.example");
        let policy = WebsitePolicy::new(
            WebsiteAction::Deny,
            [exact.clone(), allowed.clone()],
            [exact.clone()],
        );

        assert_eq!(policy.evaluate(&exact), WebsiteAction::Deny);
        assert_eq!(policy.evaluate(&allowed), WebsiteAction::Allow);
        for different in [
            "http://example.com",
            "https://sub.example.com",
            "https://example.com:8443",
        ] {
            assert_eq!(policy.evaluate(&origin(different)), WebsiteAction::Deny);
        }
    }

    #[test]
    fn default_action_applies_after_exact_rules() {
        let allowed = origin("https://allowed.example");
        let denied = origin("https://denied.example");
        let policy = WebsitePolicy::new(WebsiteAction::Deny, [allowed.clone()], [denied.clone()]);

        assert_eq!(policy.evaluate(&allowed), WebsiteAction::Allow);
        assert_eq!(policy.evaluate(&denied), WebsiteAction::Deny);
        assert_eq!(
            policy.evaluate(&origin("https://other.example")),
            WebsiteAction::Deny
        );
    }

    #[test]
    fn snapshot_deduplicates_sorts_and_hashes_independent_of_input_order() {
        let a = origin("https://a.example");
        let b = origin("https://b.example");
        let first = WebsitePolicy::new(
            WebsiteAction::Deny,
            [b.clone(), a.clone(), b.clone()],
            [b.clone(), a.clone()],
        );
        let second = WebsitePolicy::new(
            WebsiteAction::Deny,
            [a.clone(), b.clone()],
            [a.clone(), b.clone(), a.clone()],
        );
        let first = NetworkPolicySnapshot::new(ChildNetworkPolicy::Websites(first));
        let second = NetworkPolicySnapshot::new(ChildNetworkPolicy::Websites(second));

        assert_eq!(first, second);
        let ChildNetworkPolicy::Websites(policy) = first.policy() else {
            panic!("expected website policy")
        };
        assert_eq!(policy.allow().iter().collect::<Vec<_>>(), vec![&a, &b]);
        assert_eq!(first.sha256().unwrap(), second.sha256().unwrap());
        assert!(first.validate_sha256(&first.sha256().unwrap()).unwrap());
        assert!(!first.validate_sha256(&"0".repeat(64)).unwrap());
    }

    #[test]
    fn snapshot_roundtrip_preserves_policy_and_hash() {
        let policy = ChildNetworkPolicy::Websites(WebsitePolicy::new(
            WebsiteAction::Deny,
            [origin("https://allowed.example")],
            [origin("http://denied.example:8080")],
        ));
        let snapshot = NetworkPolicySnapshot::new(policy.clone());
        let json = snapshot.canonical_json().unwrap();
        let expected = r#"{"version":1,"policy":{"mode":"websites","policy":{"default":"deny","allow":["https://allowed.example:443"],"deny":["http://denied.example:8080"]}}}"#;
        assert_eq!(json, expected);
        assert_eq!(
            snapshot.sha256().unwrap(),
            "1b076f4854a41891304774143110ef54eb9936160d3d0ea3db91ca08f1e06f84"
        );

        let decoded = NetworkPolicySnapshot::from_canonical_json(expected).unwrap();
        assert_eq!(decoded.version(), NETWORK_POLICY_SNAPSHOT_VERSION);
        assert_eq!(decoded.policy(), &policy);
        assert_eq!(decoded.clone().into_policy(), policy);
        assert_eq!(decoded.sha256().unwrap(), snapshot.sha256().unwrap());
        let wrong_version = r#"{"version":2,"policy":{"mode":"future_mode"}}"#;
        assert!(matches!(
            NetworkPolicySnapshot::from_canonical_json(wrong_version),
            Err(NetworkPolicySnapshotError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn legacy_restriction_maps_without_selecting_websites() {
        assert_eq!(
            ChildNetworkPolicy::from_restrict_network(false),
            ChildNetworkPolicy::Unrestricted
        );
        assert_eq!(
            ChildNetworkPolicy::from_restrict_network(true),
            ChildNetworkPolicy::Blocked
        );
    }
}
