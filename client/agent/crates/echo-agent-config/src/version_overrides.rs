//! Version-aware config layering. A `[[version_overrides]]` array carries
//! semver-gated patches deep-merged in ascending `minimum_version` order.
//!
//! ```toml
//! [[version_overrides]]
//! minimum_version = "1.7.0"
//! [version_overrides.features]
//! logging = true
//!
//! [[version_overrides]]
//! minimum_version = "1.8.0"
//! maximum_version = "1.9.999"
//! [version_overrides.features.telemetry]
//! enabled = true
//! ```

use semver::Version;
use serde::Deserialize;

use crate::config_override::{PATCH_STRIP_KEYS, apply_patches, take_patch_array};

pub const VERSION_OVERRIDES_KEY: &str = "version_overrides";

#[derive(Debug, Clone, Deserialize)]
pub struct VersionOverrideMeta {
    #[serde(default)]
    pub minimum_version: Option<String>,
    #[serde(default)]
    pub maximum_version: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum VersionOverrideError {
    #[error("version_overrides: failed to deserialize: {0}")]
    Deserialize(#[from] toml::de::Error),
    #[error("version_overrides[{index}].minimum_version = {value:?} is not valid semver: {source}")]
    InvalidMinimumVersion {
        index: usize,
        value: String,
        #[source]
        source: semver::Error,
    },
    #[error("version_overrides[{index}].maximum_version = {value:?} is not valid semver: {source}")]
    InvalidMaximumVersion {
        index: usize,
        value: String,
        #[source]
        source: semver::Error,
    },
}

impl VersionOverrideError {
    /// A log safe summary: entry index, field, and error category only. Never
    /// the raw user supplied value or the offending source line, either of
    /// which can carry a secret. Prefer this over the `Display` impl (which
    /// echoes the value for local `Result` inspection) anywhere the message
    /// reaches logs. Mirrors the redaction rule in [`crate::loader::toml_error_detail`].
    pub fn redacted(&self) -> String {
        match self {
            Self::Deserialize(_) => {
                "version_overrides: failed to deserialize (details omitted)".to_owned()
            }
            Self::InvalidMinimumVersion { index, .. } => {
                format!("version_overrides[{index}].minimum_version is not valid semver")
            }
            Self::InvalidMaximumVersion { index, .. } => {
                format!("version_overrides[{index}].maximum_version is not valid semver")
            }
        }
    }
}

/// Strips `version_overrides` (always) and deep-merges each matching
/// patch in ascending `minimum_version` order.
pub fn apply_version_overrides(
    config: &mut toml::Value,
    version: &Version,
) -> Result<(), VersionOverrideError> {
    let entries = take_patch_array::<VersionOverrideMeta>(config, VERSION_OVERRIDES_KEY)?;

    // Parse all bounds upfront so an invalid entry fails before any merge.
    // Missing minimum_version => Version::new(0, 0, 0) (no lower bound).
    let mut parsed: Vec<(Version, Option<Version>, toml::Table)> =
        Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let min_v = match &entry.meta.minimum_version {
            Some(s) => Version::parse(s.trim()).map_err(|source| {
                VersionOverrideError::InvalidMinimumVersion {
                    index,
                    value: s.clone(),
                    source,
                }
            })?,
            None => Version::new(0, 0, 0),
        };
        let max_v = match &entry.meta.maximum_version {
            Some(max_str) => Some(Version::parse(max_str.trim()).map_err(|source| {
                VersionOverrideError::InvalidMaximumVersion {
                    index,
                    value: max_str.clone(),
                    source,
                }
            })?),
            None => None,
        };
        parsed.push((min_v, max_v, entry.patch));
    }

    // Stable sort -- ties on minimum_version keep declared order so later
    // entries win.
    parsed.sort_by(|a, b| a.0.cmp(&b.0));

    let patches = parsed.into_iter().filter_map(|(min_v, max_v, patch)| {
        if version < &min_v {
            return None;
        }
        if let Some(ref m) = max_v
            && version > m
        {
            return None;
        }
        Some(patch)
    });
    apply_patches(config, patches, PATCH_STRIP_KEYS);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).expect("valid toml")
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    /// Helper asserts the section is stripped on every call, so the
    /// "stripped even on no match" contract is covered across all 8 cases.
    #[test]
    fn version_match_boundaries() {
        fn applies(min: Option<&str>, max: Option<&str>, cli: &str) -> bool {
            let line = |k: &str, val: Option<&str>| {
                val.map(|s| format!("\n            {k} = \"{s}\""))
                    .unwrap_or_default()
            };
            let mut cfg = parse(&format!(
                r#"
                x = 0

                [[version_overrides]]{}{}
                x = 1
                "#,
                line("minimum_version", min),
                line("maximum_version", max),
            ));
            apply_version_overrides(&mut cfg, &v(cli)).unwrap();
            assert!(
                cfg.get(VERSION_OVERRIDES_KEY).is_none(),
                "section must be stripped"
            );
            cfg["x"].as_integer() == Some(1)
        }
        assert!(applies(Some("1.7.0"), None, "1.7.0")); // min inclusive
        assert!(applies(Some("1.0.0"), Some("1.7.0"), "1.7.0")); // max inclusive
        assert!(!applies(Some("1.7.0"), None, "1.6.0")); // below min
        assert!(!applies(Some("1.0.0"), Some("1.5.0"), "2.0.0")); // above max
        assert!(applies(Some("1.7.0"), None, "99.0.0")); // unbounded above
        assert!(applies(None, Some("2.0.0"), "1.5.0")); // max-only, within
        assert!(!applies(None, Some("2.0.0"), "2.0.1")); // max-only, above
        assert!(applies(None, None, "1.0.0")); // unbounded both = always
    }

    #[test]
    fn later_matching_override_wins_on_same_key() {
        let mut cfg = parse(
            r#"
            [features.telemetry]
            enabled = false

            [[version_overrides]]
            minimum_version = "1.7.0"
            [version_overrides.features.telemetry]
            enabled = true
            sample_rate = 0.1

            [[version_overrides]]
            minimum_version = "1.8.0"
            [version_overrides.features.telemetry]
            sample_rate = 0.5
            "#,
        );
        apply_version_overrides(&mut cfg, &v("1.8.0")).unwrap();
        let t = &cfg["features"]["telemetry"];
        assert_eq!(t["enabled"].as_bool(), Some(true));
        assert_eq!(t["sample_rate"].as_float(), Some(0.5));
    }

    #[test]
    fn invalid_semver_in_bounds_is_hard_error() {
        let mut cfg = parse(
            r#"
            [[version_overrides]]
            minimum_version = "not-a-version"
            x = 1
            "#,
        );
        let err = apply_version_overrides(&mut cfg, &v("1.0.0")).unwrap_err();
        assert!(matches!(
            err,
            VersionOverrideError::InvalidMinimumVersion { .. }
        ));
        // Section is consumed even on error.
        assert!(cfg.get(VERSION_OVERRIDES_KEY).is_none());

        let mut cfg = parse(
            r#"
            [[version_overrides]]
            minimum_version = "1.0.0"
            maximum_version = "garbage"
            x = 1
            "#,
        );
        let err = apply_version_overrides(&mut cfg, &v("1.0.0")).unwrap_err();
        assert!(matches!(
            err,
            VersionOverrideError::InvalidMaximumVersion { .. }
        ));
    }

    #[test]
    fn redacted_summary_omits_the_raw_bound_value() {
        let mut cfg = parse(
            r#"
            [[version_overrides]]
            minimum_version = "sk-secret-token"
            x = 1
            "#,
        );
        let err = apply_version_overrides(&mut cfg, &v("1.0.0")).unwrap_err();
        assert_eq!(
            err.redacted(),
            "version_overrides[0].minimum_version is not valid semver"
        );
    }
}
