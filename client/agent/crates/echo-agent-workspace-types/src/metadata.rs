//! Per-call metadata header map.
//!
//! [`Metadata`] is a string-keyed map intended to mirror gRPC metadata 1:1
//! when the call goes over the wire. It carries per-call context like
//! session ids, trace context, and deadlines.
//!
//! `Extensions` (the typed in-process map) is
//! deliberately **not** defined here -- it is in-process only and not
//! serialized, so it lives in the runtime `echo-agent-workspace` crate
//! alongside the transport implementations.
//!
//! # Implementation notes
//!
//! - **`META_GRPC_TIMEOUT`**: an earlier name for this constant was
//!   `META_DEADLINE_MS`, but the underlying gRPC `grpc-timeout` header
//!   is unit-suffixed (e.g. `"30S"` for 30 seconds, `"100m"` for 100 ms)
//!   per the [gRPC HTTP/2 spec][grpc-spec], not a bare millisecond
//!   count. The renamed constant is more honest about what callers will
//!   find in the value -- the `_MS` suffix would otherwise mislead
//!   anyone parsing the header to expect a plain integer.
//!
//! - **`Metadata` backing store**: the obvious choice is
//!   `HashMap<String, String>`. We use `BTreeMap<String, String>` so
//!   serialization order is deterministic, which matters for snapshot
//!   tests and any wire-bytes hashing. The on-wire JSON shape is
//!   identical (a JSON object).
//!
//! [grpc-spec]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Standard metadata key for the active session id.
pub const META_SESSION_ID: &str = "x-workspace-session-id";

/// Standard metadata key for the W3C trace parent (`traceparent`).
pub const META_TRACEPARENT: &str = "traceparent";

/// Standard metadata key for the W3C trace state (`tracestate`).
pub const META_TRACESTATE: &str = "tracestate";

/// Standard metadata key for the originating client identity.
pub const META_CLIENT_ID: &str = "x-workspace-client-id";

/// Standard metadata key for the prompt index of the current turn.
pub const META_PROMPT_INDEX: &str = "x-workspace-prompt-index";

/// Standard metadata key for the gRPC call deadline.
///
/// Note: the gRPC `grpc-timeout` header carries a unit-suffixed string
/// per the [gRPC HTTP/2 spec][grpc-spec], not a bare millisecond count.
/// Examples: `"100m"` (100 ms), `"30S"` (30 s), `"2H"` (2 h). Callers
/// reading this key are responsible for parsing the unit suffix --
/// the constant only names the header, it does not impose a unit.
///
/// [grpc-spec]: https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#requests
pub const META_GRPC_TIMEOUT: &str = "grpc-timeout";

/// All standard metadata keys defined by this crate, in declaration order.
///
/// Useful for tests that need to assert uniqueness or for callers that
/// want to scrub well-known keys from a metadata map.
pub const STANDARD_META_KEYS: &[&str] = &[
    META_SESSION_ID,
    META_TRACEPARENT,
    META_TRACESTATE,
    META_CLIENT_ID,
    META_PROMPT_INDEX,
    META_GRPC_TIMEOUT,
];

/// String-keyed metadata headers.
///
/// Backed by a `BTreeMap` so serialization order is deterministic, which
/// matters for snapshot tests and for stable wire-bytes hashing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Metadata(pub BTreeMap<String, String>);

impl Metadata {
    /// Create an empty metadata map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a header. Returns the previous value if any.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.0.insert(key.into(), value.into())
    }

    /// Look up a header by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Whether `key` is present in the map.
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Remove a header. Returns the removed value if any.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.0.remove(key)
    }

    /// Iterate over the map's `(key, value)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Iterate over the map's keys in sorted order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Iterate over the map's values in key order.
    pub fn values(&self) -> impl Iterator<Item = &str> {
        self.0.values().map(String::as_str)
    }

    /// Number of entries in the map.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K, V> FromIterator<(K, V)> for Metadata
where
    K: Into<String>,
    V: Into<String>,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

/// Allows `for (k, v) in &metadata { ... }` without going through
/// [`Metadata::iter`] explicitly.
impl<'a> IntoIterator for &'a Metadata {
    type Item = (&'a str, &'a str);
    type IntoIter = std::iter::Map<
        std::collections::btree_map::Iter<'a, String, String>,
        fn((&'a String, &'a String)) -> (&'a str, &'a str),
    >;

    fn into_iter(self) -> Self::IntoIter {
        // Using a `fn` pointer (rather than a closure) keeps the
        // associated type nameable; the closure form would require an
        // unnameable opaque type.
        self.0
            .iter()
            .map(|(k, v): (&'a String, &'a String)| -> (&'a str, &'a str) {
                (k.as_str(), v.as_str())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn standard_meta_keys_are_unique() {
        let set: HashSet<&&str> = STANDARD_META_KEYS.iter().collect();
        assert_eq!(
            set.len(),
            STANDARD_META_KEYS.len(),
            "duplicate standard metadata key"
        );
    }

    #[test]
    fn standard_meta_keys_have_expected_values() {
        // Sanity: lock the wire constants down so a typo is a test failure.
        assert_eq!(META_SESSION_ID, "x-workspace-session-id");
        assert_eq!(META_TRACEPARENT, "traceparent");
        assert_eq!(META_TRACESTATE, "tracestate");
        assert_eq!(META_CLIENT_ID, "x-workspace-client-id");
        assert_eq!(META_PROMPT_INDEX, "x-workspace-prompt-index");
        assert_eq!(META_GRPC_TIMEOUT, "grpc-timeout");
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let m: Metadata = [(META_SESSION_ID, "sess-1"), (META_TRACEPARENT, "00-...")]
            .into_iter()
            .collect();
        let json = serde_json::to_string(&m).unwrap();
        let back: Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn metadata_iteration_is_sorted() {
        let mut m = Metadata::new();
        m.insert("z", "1");
        m.insert("a", "2");
        m.insert("m", "3");
        let keys: Vec<&str> = m.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["a", "m", "z"]);
    }

    #[test]
    fn metadata_supports_for_loop_via_into_iterator() {
        let mut m = Metadata::new();
        m.insert("a", "1");
        m.insert("b", "2");
        let mut collected = Vec::new();
        for (k, v) in &m {
            collected.push((k.to_owned(), v.to_owned()));
        }
        assert_eq!(
            collected,
            vec![
                ("a".to_owned(), "1".to_owned()),
                ("b".to_owned(), "2".to_owned())
            ]
        );
    }

    #[test]
    fn metadata_keys_values_helpers() {
        let mut m = Metadata::new();
        m.insert("a", "1");
        m.insert("b", "2");
        let keys: Vec<&str> = m.keys().collect();
        let values: Vec<&str> = m.values().collect();
        assert_eq!(keys, ["a", "b"]);
        assert_eq!(values, ["1", "2"]);
        assert!(m.contains_key("a"));
        assert!(!m.contains_key("z"));
    }
}
