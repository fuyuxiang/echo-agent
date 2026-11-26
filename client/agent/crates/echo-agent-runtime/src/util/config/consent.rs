//! Local record of which consent notices this machine has answered.
//!
//! The server is authoritative once the upstream record exists; until then this is what stops the
//! notice re-asking on every launch.

use anyhow::Result;

use super::persist::update_config;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ConsentConfig {
    /// One entry per notice id, so two notices at once cannot overwrite each other's answers.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub answers: std::collections::BTreeMap<String, ConsentAnswer>,
}

/// One notice's answer, under `[consent.answers.<notice id>]`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ConsentAnswer {
    /// Highest version answered for this notice on this machine.
    #[serde(default)]
    pub version: i32,
    /// Account the answer belongs to, so a second user on this machine is still asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,

    /// `true` once the server accepted the record; the local answer is what gates the prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub acked: bool,
}

/// Version is monotonic within a notice, so a stale replay cannot lower the record.
pub async fn set_consent_answer(
    account: Option<String>,
    notice_id: String,
    version: i32,
    acked: bool,
) -> Result<()> {
    update_config(|cfg| {
        let entry = cfg.consent.answers.entry(notice_id).or_default();

        // A different account restarts the count rather than inheriting the previous version.
        let recorded = if entry.account == account {
            entry.version
        } else {
            0
        };
        entry.account = account;

        // The ack belongs to a version, so a replay cannot mark a higher one acked.
        if version > recorded {
            entry.version = version;
            entry.acked = acked;
        } else if version == recorded {
            // The local write and the server ack race for the same version, and the local one
            // carries `false`. Losing that race must not retract an ack that already landed.
            entry.acked |= acked;
        }
    })
    .await
}

#[cfg(test)]
#[path = "consent_tests.rs"]
mod tests;
