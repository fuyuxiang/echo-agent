//! Signed deployment-config envelope: the wire contract between the
//! cli-chat-proxy signer and the client verifier. Shared so a field rename
//! breaks at compile time on both sides instead of silently failing verification.

use serde::{Deserialize, Serialize};

/// The payload format version the server currently signs. Informational for
/// now: no verifier gates on it (every version verifies the same); enforce a
/// minimum only once the fleet has rotated past older generations.
/// `0` = pre-versioned, `1` = first versioned payload, `2` = per-fetch `nonce`.
pub const SIGNED_PAYLOAD_VERSION: u32 = 2;

/// Domain-separation tags inside the signed bytes: both message types share one
/// signing key, so each verifier requires its own tag (no cross-substitution).
pub const MANAGED_POLICY_TYP: &str = "echoagent.managed_policy.v1";
pub const MANAGED_IDENTITY_TYP: &str = "echoagent.managed_identity.v1";

/// Client echoes its persisted envelope `nonce` on this header for the server probe.
pub const MANAGED_CONFIG_NONCE_ECHO_HEADER: &str = "x-echo-agent-managed-config-nonce";

/// Shape of a server-minted nonce (16 random bytes as hex): what `fresh_nonce`
/// produces and the only shape the client echoes (hex is HTTP-header-safe).
/// Shared so a mint change cannot silently disable the echo: the proxy pins
/// its mint against this, the client gates its echo on it.
pub fn is_server_nonce_shape(nonce: &str) -> bool {
    nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The exact bytes the server signs: the served policy, the principal it is
/// bound to, and an expiry. Serialized once on the server and shipped verbatim
/// as `signed_payload`, so the client verifies the received bytes directly
/// instead of re-canonicalizing (no cross-language serialization drift).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedPayload {
    /// Domain-separation tag; the verifier requires [`MANAGED_POLICY_TYP`].
    /// `default` so untagged JSON parses — verification still rejects it.
    #[serde(default)]
    pub typ: String,
    /// Payload format version ([`SIGNED_PAYLOAD_VERSION`]); `default` 0 so
    /// pre-versioned sidecars parse and verify unchanged.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub deployment_id: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub managed_config: Option<String>,
    #[serde(default)]
    pub requirements: Option<String>,
    /// Strict (fail-closed) opt-in, carried in the SIGNED bytes so a local actor can't
    /// flip enforcement. `default` false so an older/unsigned payload stays lenient.
    #[serde(default)]
    pub fail_closed: bool,
    /// Unix seconds after which the signature is no longer trusted.
    pub expires_at: u64,
    /// Per-response nonce in the signed bytes (echoed on [`MANAGED_CONFIG_NONCE_ECHO_HEADER`]).
    /// `default` empty keeps pre-nonce sidecars verifiable.
    #[serde(default)]
    pub nonce: String,
    /// Identifies the signing key, so a rotation can be distinguished.
    pub key_id: String,
}

/// Server-signed claim that a principal is managed (+ fail-closed), persisted by
/// the client as its OWN sidecar — so deleting the policy sidecar alone cannot
/// downgrade the load-time gate to the forgeable marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedIdentityClaim {
    /// Domain-separation tag — the verifier requires [`MANAGED_IDENTITY_TYP`].
    #[serde(default)]
    pub typ: String,
    /// The managed principal (deployment or team id) this claim is bound to.
    pub principal: String,
    /// Strict opt-in, same server source as the policy's; `default` false so an
    /// older/partial claim never imposes.
    #[serde(default)]
    pub fail_closed: bool,
    /// Unix seconds after which the claim is no longer trusted.
    pub expires_at: u64,
    /// Signing key id (same rotation set as the policy envelope).
    pub key_id: String,
}

/// One signed envelope carried alongside the legacy policy fields in the
/// deployment-config response (additive: old clients ignore it). Also the
/// shape the client persists as its on-disk signature sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureEnvelope {
    /// The exact JSON string that was signed (a serialized [`SignedPayload`]).
    pub signed_payload: String,
    /// Base64 (standard) Ed25519 signature over `signed_payload`'s UTF-8 bytes.
    pub signature: String,
    /// Untrusted (outside the signed bytes): a hint for picking among multiple
    /// envelopes, never for selecting the verifying key — only the signed
    /// payload's `key_id` is authoritative.
    #[serde(default)]
    pub key_id: String,
}

/// Unix seconds now (saturating to 0 on a pre-epoch clock).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `requirements.toml` key for strict (fail-closed) enforcement.
pub const FAIL_CLOSED_KEY: &str = "fail_closed";

/// Parse result for `fail_closed`. [`Invalid`] = key present but not a bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailClosedFlag {
    True,
    False,
    Invalid,
}

impl FailClosedFlag {
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::True)
    }
}

/// Shared `fail_closed` parse (signer + client). Bad TOML → False; non-bool key → Invalid.
pub fn fail_closed_flag_status(requirements: &str) -> FailClosedFlag {
    let Ok(v) = toml::from_str::<toml::Value>(requirements) else {
        return FailClosedFlag::False;
    };
    fail_closed_flag_status_from_value(&v)
}

/// [`fail_closed_flag_status`] for an already-parsed table.
pub fn fail_closed_flag_status_from_value(requirements: &toml::Value) -> FailClosedFlag {
    match requirements.get(FAIL_CLOSED_KEY) {
        None => FailClosedFlag::False,
        Some(val) => match val.as_bool() {
            Some(true) => FailClosedFlag::True,
            Some(false) => FailClosedFlag::False,
            None => FailClosedFlag::Invalid,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version field round-trips, and a pre-versioned payload (no `version`
    /// key) defaults to 0 — old sidecars keep parsing.
    #[test]
    fn signed_payload_version_round_trips_and_defaults() {
        let versioned = SignedPayload {
            typ: MANAGED_POLICY_TYP.to_owned(),
            version: SIGNED_PAYLOAD_VERSION,
            deployment_id: None,
            team_id: Some("team-007".into()),
            managed_config: None,
            requirements: None,
            fail_closed: false,
            expires_at: 4_000_000_000,
            nonce: "9f86d081884c7d6594a85abf0f0cf96b".into(),
            key_id: "v1".into(),
        };
        let json = serde_json::to_string(&versioned).unwrap();
        assert_eq!(
            serde_json::from_str::<SignedPayload>(&json).unwrap(),
            versioned
        );

        let legacy: SignedPayload =
            serde_json::from_str(r#"{"expires_at": 1, "key_id": "v1"}"#).unwrap();
        assert_eq!(legacy.version, 0, "pre-versioned payloads default to 0");
        assert_eq!(
            legacy.typ, "",
            "an untagged payload parses (verifiers reject it)"
        );
        assert_eq!(
            legacy.nonce, "",
            "pre-nonce payloads default to an empty nonce"
        );
        let shaped = format!("{:032x}", std::process::id() as u128);
        assert!(is_server_nonce_shape(&shaped));
        assert!(!is_server_nonce_shape(&format!("x{}", std::process::id())));
    }

    /// The claim round-trips; `fail_closed` is additive (absent → permissive).
    #[test]
    fn managed_identity_claim_round_trips_and_defaults() {
        let claim = ManagedIdentityClaim {
            typ: MANAGED_IDENTITY_TYP.to_owned(),
            principal: "team-007".into(),
            fail_closed: true,
            expires_at: 4_000_000_000,
            key_id: "v1".into(),
        };
        let json = serde_json::to_string(&claim).unwrap();
        assert_eq!(
            serde_json::from_str::<ManagedIdentityClaim>(&json).unwrap(),
            claim
        );

        let partial: ManagedIdentityClaim = serde_json::from_str(
            r#"{"typ":"echoagent.managed_identity.v1","principal":"team-007","expires_at":1,"key_id":"v1"}"#,
        )
        .unwrap();
        assert!(!partial.fail_closed, "a partial claim parses permissive");
    }

    #[test]
    fn fail_closed_flag_status_distinguishes_invalid() {
        assert_eq!(
            fail_closed_flag_status("fail_closed = true\n"),
            FailClosedFlag::True
        );
        assert_eq!(
            fail_closed_flag_status("fail_closed = false\n"),
            FailClosedFlag::False
        );
        assert_eq!(
            fail_closed_flag_status("[features]\n"),
            FailClosedFlag::False
        );
        assert_eq!(
            fail_closed_flag_status("fail_closed = \"true\"\n"),
            FailClosedFlag::Invalid
        );
        assert_eq!(
            fail_closed_flag_status("fail_closed = 1\n"),
            FailClosedFlag::Invalid
        );
        // Unparseable TOML: no key to warn about.
        assert_eq!(
            fail_closed_flag_status("not = = valid"),
            FailClosedFlag::False
        );
    }
}
