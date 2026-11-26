//! JSON types for cli-chat-proxy's `/v1/team/{team_id}/managed-config` routes;
//! the documents written here are served to CLIs by `/v1/deployment/config`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize a present field into `Some(_)` even when its value is `null`.
/// Plain `Option<Option<T>>` cannot distinguish the two: serde maps an explicit
/// `null` to `None`, the same as an absent field. Paired with `#[serde(default)]`
/// this gives the three states a partial update needs.
fn present_or_null<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamManagedConfig {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<String>,
    pub fail_closed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

/// A partial update. Each document field is a double option, which is what lets
/// JSON say all three things:
///
/// - absent (`None`) — leave the stored document alone
/// - `null` (`Some(None)`) — clear it
/// - a string (`Some(Some(_))`) — replace it
///
/// An omitted field can therefore never erase a document, which is how an
/// editor that does not round-trip `requirements` stops being able to wipe a
/// team's enforced policy. A body that names neither document is a 400.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)] // a typoed key must 400, not silently leave a field alone
pub struct SetTeamManagedConfigRequest {
    #[serde(
        default,
        deserialize_with = "present_or_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub managed_config: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present_or_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub requirements: Option<Option<String>>,
    /// Guard: the write fails 412 unless the stored `updated_at` equals this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)] // a typoed guard key must 400, not silently unguard
pub struct DeleteTeamManagedConfigRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_updated_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults() {
        let config = TeamManagedConfig {
            configured: true,
            managed_config: Some("[cli]\n".into()),
            requirements: Some("fail_closed = true\n".into()),
            fail_closed: true,
            updated_at: Some(
                DateTime::parse_from_rfc3339("2026-01-02T03:04:05.123456Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("2026-01-02T03:04:05.123456Z"), "{json}");
        assert_eq!(
            serde_json::from_str::<TeamManagedConfig>(&json).unwrap(),
            config
        );

        let unconfigured: TeamManagedConfig =
            serde_json::from_str(r#"{"configured":false,"fail_closed":false}"#).unwrap();
        assert_eq!(unconfigured, TeamManagedConfig::default());

        let set: SetTeamManagedConfigRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(set, SetTeamManagedConfigRequest::default());
        let del: DeleteTeamManagedConfigRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(del.expected_updated_at, None);
    }

    /// The three states a partial update needs. Plain `Option<Option<String>>`
    /// silently collapses `null` onto absent, which would make "clear this
    /// document" unexpressible — hence `present_or_null`.
    #[test]
    fn set_request_distinguishes_absent_null_and_value() {
        let absent: SetTeamManagedConfigRequest =
            serde_json::from_str(r#"{"requirements":"fail_closed = true\n"}"#).unwrap();
        assert_eq!(absent.managed_config, None, "absent must not mean clear");
        assert_eq!(
            absent.requirements,
            Some(Some("fail_closed = true\n".to_owned()))
        );

        let cleared: SetTeamManagedConfigRequest =
            serde_json::from_str(r#"{"managed_config":null}"#).unwrap();
        assert_eq!(cleared.managed_config, Some(None), "null must mean clear");
        assert_eq!(cleared.requirements, None);

        // Round-trips: absent stays absent, `null` stays `null`.
        assert_eq!(
            serde_json::to_string(&cleared).unwrap(),
            r#"{"managed_config":null}"#
        );
        assert_eq!(
            serde_json::from_str::<SetTeamManagedConfigRequest>(
                &serde_json::to_string(&cleared).unwrap()
            )
            .unwrap(),
            cleared
        );

        // A typoed key is still a 400 rather than a silent no-op.
        assert!(
            serde_json::from_str::<SetTeamManagedConfigRequest>(r#"{"managed_confgi":"x"}"#)
                .is_err()
        );
    }
}
