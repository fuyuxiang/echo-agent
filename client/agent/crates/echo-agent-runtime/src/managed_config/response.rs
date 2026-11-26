//! The deployment-config fetch/response contract: the credential source and its
//! errors, response parsing, envelope picking, fetched-envelope verification, and
//! the apply outcome the sync orchestration consumes.

use echo_agent_config::signed_policy::now_unix;
use serde::Deserialize;

/// Which credential a config fetch used — tailors error messages and the
/// post-fetch confirmation (team vs deployment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ManagedConfigSource {
    DeploymentKey,
    TeamOauth,
}

impl ManagedConfigSource {
    pub(super) fn is_team(self) -> bool {
        matches!(self, Self::TeamOauth)
    }

    /// The 401/403 error tailored to the credential (don't tell a team user to
    /// check `ECHO_AGENT_DEPLOYMENT_KEY`).
    pub(super) fn auth_rejected_error(self) -> ManagedConfigError {
        if self.is_team() {
            ManagedConfigError::TeamAuthRejected
        } else {
            ManagedConfigError::DeploymentKeyRejected
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedConfigError {
    #[error("Can't reach the server. Check your network connection and try again.\n  ({0})")]
    Network(String),
    #[error(
        "Can't verify the server's security certificate. This usually means your organization's root certificates aren't installed. Install them, then try again.\n  ({0})"
    )]
    CertificateUntrusted(String),
    #[error(
        "The server's security certificate is invalid (for example expired or issued for a different hostname), so it can't be trusted. Installing a root certificate won't fix this; contact the server administrator.\n  ({0})"
    )]
    CertificateInvalid(String),
    #[error(
        "The connection to the server was interrupted or timed out before completing. This is usually temporary; please try again.\n  ({0})"
    )]
    ConnectionInterrupted(String),
    #[error(
        "The deployment key was rejected. Confirm that ECHO_AGENT_DEPLOYMENT_KEY is set correctly and hasn't expired."
    )]
    DeploymentKeyRejected,
    #[error(
        "Your team sign-in was rejected. It may have expired or lack access. Run `echoagent login` to sign in again."
    )]
    TeamAuthRejected,
    #[error("The server returned an unexpected error (HTTP {status}). Try again in a few minutes.")]
    ServerError { status: u16 },
    #[error("The server returned an unexpected response.\n  ({0})")]
    InvalidResponse(String),
    #[error(
        "The configuration request couldn't be completed due to a client-side error (not a server response). This is unexpected; please report it if it persists.\n  ({0})"
    )]
    RequestFailed(String),
    #[error(
        "The server's response could not be verified as authentic managed policy, so nothing was installed. Try again; if this persists, contact your administrator."
    )]
    SignatureRejected,
    #[error(
        "Can't save the configuration to ~/.echo-agent. Make sure the directory exists and is writable.\n  ({0})"
    )]
    DiskWrite(#[from] std::io::Error),
}

impl ManagedConfigError {
    /// Transient failure (network / connection interruption / server 5xx) where retrying may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Network(_) | Self::ConnectionInterrupted(_) | Self::ServerError { .. }
        )
    }

    /// Auth/eligibility rejection (no access or expired session) — not fixable by retrying.
    pub fn is_auth_rejection(&self) -> bool {
        matches!(self, Self::TeamAuthRejected | Self::DeploymentKeyRejected)
    }
}

#[derive(Deserialize, Default)]
pub(super) struct ManagedConfigResponse {
    #[serde(default)]
    pub(super) deployment_id: Option<String>,
    #[serde(default)]
    pub(super) team_id: Option<String>,
    pub(super) managed_config: Option<String>,
    pub(super) requirements: Option<String>,
    /// The signed envelopes (additive; absent from old servers), primary first —
    /// a rollover server dual-signs, each payload signed by ITS OWN key. The
    /// signed payload's policy is the trusted copy when verification is on.
    #[serde(default)]
    pub(super) signatures: Option<Vec<echo_agent_config::signed_policy::SignatureEnvelope>>,
    /// The is-managed claim envelopes (additive; absent from old servers), same
    /// rotation shape as `signatures`, persisted as their own sidecar.
    #[serde(default)]
    pub(super) managed_identity_signatures:
        Option<Vec<echo_agent_config::signed_policy::SignatureEnvelope>>,
}

impl ManagedConfigResponse {
    pub(super) fn config_exists(&self) -> bool {
        self.deployment_id.is_some() || self.team_id.is_some()
    }

    /// The signed envelope to verify, when the server included any: the first
    /// `signatures` entry whose key_id is in the embedded trusted set, else the
    /// first entry (so verification reports the real UnknownKeyId failure). The
    /// outer key_id only PICKS — verification re-selects the key from the signed bytes.
    pub(super) fn signature_sidecar(
        &self,
    ) -> Option<echo_agent_config::signed_policy::SignatureEnvelope> {
        pick_trusted_envelope(
            self.signatures.as_deref(),
            echo_agent_config::signed_policy::embedded_key_id_trusted,
        )
    }

    /// The claim envelope to verify — same picking rule as [`Self::signature_sidecar`].
    pub(super) fn managed_identity_sidecar(
        &self,
    ) -> Option<echo_agent_config::signed_policy::SignatureEnvelope> {
        pick_trusted_envelope(
            self.managed_identity_signatures.as_deref(),
            echo_agent_config::signed_policy::embedded_key_id_trusted,
        )
    }

    /// Non-empty served content, recorded in the marker so staleness can later detect a deleted file.
    pub(super) fn has_managed_config(&self) -> bool {
        self.managed_config
            .as_deref()
            .is_some_and(|s| !s.is_empty())
    }

    pub(super) fn has_requirements(&self) -> bool {
        self.requirements.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// Served `fail_closed` from the payload (not disk). Non-bool → warn once, treat as false.
    pub(super) fn requirements_fail_closed(&self) -> bool {
        let Some(req) = self.requirements.as_deref() else {
            return false;
        };
        use echo_agent_chat_proxy_types::{FailClosedFlag, fail_closed_flag_status};
        let status = fail_closed_flag_status(req);
        if matches!(status, FailClosedFlag::Invalid) {
            static WARN_ONCE: std::sync::Once = std::sync::Once::new();
            WARN_ONCE.call_once(|| {
                tracing::warn!(
                    "served requirements fail_closed is present but not a boolean \
                     (e.g. fail_closed = \"true\"); treating as false - use fail_closed = true"
                );
            });
        }
        status.is_enabled()
    }
}

/// Result of applying a fetched managed-config response.
pub(super) enum ApplyOutcome {
    /// Locked, persisted policy, recorded marker. `wrote` = ≥1 artifact written or removed.
    Applied { wrote: bool },
    /// Nothing persisted/marked: lock held by another process, or credential vanished mid-fetch.
    Skipped,
    /// Envelope failed verification — nothing persisted or marked.
    SignatureRejected,
}

impl ApplyOutcome {
    pub(super) fn wrote(&self) -> bool {
        matches!(self, Self::Applied { wrote: true })
    }

    pub(super) fn skipped(&self) -> bool {
        matches!(self, Self::Skipped)
    }

    pub(super) fn signature_rejected(&self) -> bool {
        matches!(self, Self::SignatureRejected)
    }
}

/// Pick the envelope whose (hint-only) key_id is trusted, else the first.
fn pick_trusted_envelope(
    envelopes: Option<&[echo_agent_config::signed_policy::SignatureEnvelope]>,
    key_id_trusted: impl Fn(&str) -> bool,
) -> Option<echo_agent_config::signed_policy::SignatureEnvelope> {
    let envelopes = envelopes?;
    envelopes
        .iter()
        .find(|e| key_id_trusted(&e.key_id))
        .or_else(|| envelopes.first())
        .cloned()
}

/// A fetched envelope that passed verification: the sidecar to persist, plus its
/// parsed (now-trusted) payload.
pub(super) struct VerifiedEnvelope {
    pub(super) sidecar: echo_agent_config::signed_policy::SignatureEnvelope,
    pub(super) payload: echo_agent_config::signed_policy::SignedPayload,
}

/// Verify the signed envelope without persisting anything. The legacy body fields must
/// equal the signed copy, so what lands on disk is exactly what was signed (and the
/// load-time gate can re-verify it). The error is a plain message: the caller only logs it.
pub(super) fn verify_signed_envelope(
    body: &ManagedConfigResponse,
    active_team_id: Option<&str>,
) -> Result<VerifiedEnvelope, String> {
    use echo_agent_config::signed_policy;
    let sidecar = body.signature_sidecar().ok_or_else(|| {
        "managed policy is required but the server returned no signature".to_owned()
    })?;
    // Unclamped wall clock: a fresh envelope must heal an inflated floor, not be refused by it.
    let payload = signed_policy::verify_fetched(&sidecar, active_team_id, now_unix())
        .map_err(|e| e.to_string())?;
    if body.managed_config != payload.managed_config || body.requirements != payload.requirements {
        return Err("served policy does not match the signed payload".to_owned());
    }
    Ok(VerifiedEnvelope { sidecar, payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Picking (shared by the policy and claim carriers): the first trusted-key_id
    /// entry wins; no trusted entry → the first entry (picking must not invent
    /// absence); no array (old/unsigned server) → None.
    #[test]
    fn pick_trusted_envelope_prefers_trusted_then_falls_back() {
        use echo_agent_config::signed_policy::SignatureEnvelope;
        let envelope = |kid: &str| SignatureEnvelope {
            signed_payload: format!("payload-{kid}"),
            signature: format!("sig-{kid}"),
            key_id: kid.to_owned(),
        };
        let envelopes = vec![envelope("v1"), envelope("v2")];

        // A rotated client trusting only v2 picks the v2 envelope from the array.
        let picked = pick_trusted_envelope(Some(&envelopes), |id| id == "v2").unwrap();
        assert_eq!(picked.key_id, "v2");
        assert_eq!(picked.signed_payload, "payload-v2");

        // Trusting v1 picks the primary entry (first in the array).
        let picked = pick_trusted_envelope(Some(&envelopes), |id| id == "v1").unwrap();
        assert_eq!(picked.key_id, "v1");

        // No trusted id → the first entry, so verification reports UnknownKeyId.
        let picked = pick_trusted_envelope(Some(&envelopes), |_| false).unwrap();
        assert_eq!(picked.key_id, "v1");

        // Nothing signed at all → None.
        assert!(pick_trusted_envelope(None, |_| true).is_none());
    }
}
