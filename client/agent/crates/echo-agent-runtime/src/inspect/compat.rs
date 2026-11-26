//! Vendor-compat resolution for `echoagent inspect`.
//!
//! Resolves the local env/config/default stack into a diagnostic report.

use echo_agent_tools::types::compat::{COMPAT_CELLS, CompatCell, CompatConfig};
use serde::Serialize;

/// Derive the vendor origin from a file path. Returns `Some("cursor")` or
/// `Some("claude")` when the path passes through a vendor config directory;
/// `None` for native `.echo-agent`/`.agents` paths.
pub(super) fn derive_vendor(path: &str) -> Option<&'static str> {
    if path.contains("/.cursor/") || path.contains("\\.cursor\\") || path.ends_with("/.cursor") {
        Some("cursor")
    } else if path.contains("/.claude/")
        || path.contains("\\.claude\\")
        || path.ends_with("/.claude")
        || path.contains("/.claude.json")
    {
        Some("claude")
    } else {
        None
    }
}

pub(super) fn instruction_compat_status(
    vendor: &Option<String>,
    file_type: &str,
    compat: &ExternalCompatReport,
) -> Option<CompatEntryStatus> {
    let surface = if file_type == "rules" {
        "rules"
    } else {
        "agents"
    };
    vendor_compat_status(vendor, surface, compat)
}

pub(super) fn vendor_compat_status(
    vendor: &Option<String>,
    surface: &str,
    compat: &ExternalCompatReport,
) -> Option<CompatEntryStatus> {
    let vendor = vendor.as_deref()?;
    if !matches!(vendor, "cursor" | "claude") {
        return None;
    }
    compat.status(vendor, surface)
}

/// Format a vendor tag for human output (e.g. " [cursor]"), empty for native.
pub(super) fn vendor_tag(vendor: &Option<String>) -> String {
    match vendor {
        Some(v) => format!(" [{}]", v),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CompatEntryStatus {
    Enabled,
    Disabled,
}

/// Which resolution layer determined a vendor-compat cell's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CompatSource {
    Env,
    Config,
    ConfigError,
    Default,
}

impl std::fmt::Display for CompatSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Env => "env",
            Self::Config => "config",
            Self::ConfigError => "config error; fail closed",
            Self::Default => "default",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalCompatEntry {
    pub vendor: String,
    pub surface: String,
    pub enabled: bool,
    pub source: CompatSource,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalCompatReport {
    pub remote_settings_loaded: bool,
    pub cells: Vec<ExternalCompatEntry>,
}

impl ExternalCompatReport {
    fn status(&self, vendor: &str, surface: &str) -> Option<CompatEntryStatus> {
        self.cells
            .iter()
            .find(|cell| cell.vendor == vendor && cell.surface == surface)
            .map(|cell| {
                if cell.enabled {
                    CompatEntryStatus::Enabled
                } else {
                    CompatEntryStatus::Disabled
                }
            })
    }
}

pub(super) fn resolve_inspect_compat(
    effective_config: Result<&toml::Value, ()>,
) -> ExternalCompatReport {
    resolve_inspect_compat_with_env(effective_config, |cell| {
        echo_agent_config::env_bool(cell.env_var())
    })
}

pub(super) fn resolve_inspect_compat_with_env(
    effective_config: Result<&toml::Value, ()>,
    env_value: impl Fn(CompatCell) -> Option<bool>,
) -> ExternalCompatReport {
    let defaults = CompatConfig::default();
    let cells = COMPAT_CELLS
        .into_iter()
        .filter(|cell| cell.is_runtime_supported())
        .map(|cell| {
            let config = crate::agent::config::compat_config_cell(effective_config, cell);
            resolve_compat_entry(cell, env_value(cell), config, defaults.value(cell))
        })
        .collect();

    ExternalCompatReport {
        remote_settings_loaded: false,
        cells,
    }
}

fn resolve_compat_entry(
    cell: CompatCell,
    env: Option<bool>,
    config: Result<Option<bool>, crate::agent::config::CompatConfigCellError>,
    default: bool,
) -> ExternalCompatEntry {
    let (config, config_error) = match config {
        Ok(value) => (value, false),
        Err(_) => (Some(false), true),
    };
    let resolved = crate::agent::config::resolve_compat_cell_with_env(env, config, None, default);
    let source = if env.is_some() {
        CompatSource::Env
    } else if config.is_some() {
        if config_error {
            CompatSource::ConfigError
        } else {
            CompatSource::Config
        }
    } else {
        CompatSource::Default
    };

    ExternalCompatEntry {
        vendor: cell.vendor().as_str().to_owned(),
        surface: cell.surface().as_str().to_owned(),
        enabled: resolved.value,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_without_env(effective_config: Result<&toml::Value, ()>) -> ExternalCompatReport {
        resolve_inspect_compat_with_env(effective_config, |_| None)
    }

    fn entry<'a>(
        report: &'a ExternalCompatReport,
        vendor: &str,
        surface: &str,
    ) -> &'a ExternalCompatEntry {
        report
            .cells
            .iter()
            .find(|entry| entry.vendor == vendor && entry.surface == surface)
            .unwrap()
    }

    #[test]
    fn empty_config_reports_defaults_and_remote_not_loaded() {
        let effective_config = toml::Value::Table(toml::map::Map::new());
        let report = resolve_without_env(Ok(&effective_config));

        assert!(!report.remote_settings_loaded);
        assert_eq!(report.cells.len(), 13);
        assert!(
            report
                .cells
                .iter()
                .all(|cell| cell.enabled && cell.source == CompatSource::Default)
        );
        assert_eq!(
            report
                .cells
                .iter()
                .filter(|entry| entry.vendor == "codex")
                .map(|entry| entry.surface.as_str())
                .collect::<Vec<_>>(),
            vec!["sessions"]
        );
        let session = entry(&report, "codex", "sessions");
        assert_eq!(session.enabled, CompatConfig::default().codex.sessions);
        assert_eq!(session.source, CompatSource::Default);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["remoteSettingsLoaded"], false);
        assert_eq!(
            serde_json::to_value(session).unwrap(),
            serde_json::json!({
                "vendor": "codex",
                "surface": "sessions",
                "enabled": true,
                "source": "default"
            })
        );
    }

    #[test]
    fn inspect_compat_uses_env_config_default_precedence() {
        let effective_config: toml::Value =
            toml::from_str("[compat.cursor]\nskills = false\nrules = false\n").unwrap();
        let report = resolve_inspect_compat_with_env(Ok(&effective_config), |cell| {
            (cell.env_var() == "ECHO_AGENT_CURSOR_SKILLS_ENABLED").then_some(true)
        });

        let skills = entry(&report, "cursor", "skills");
        assert!(skills.enabled);
        assert_eq!(skills.source, CompatSource::Env);
        let rules = entry(&report, "cursor", "rules");
        assert!(!rules.enabled);
        assert_eq!(rules.source, CompatSource::Config);
        let agents = entry(&report, "cursor", "agents");
        assert!(agents.enabled);
        assert_eq!(agents.source, CompatSource::Default);
    }

    #[test]
    fn config_load_failure_fails_closed_unless_env_overrides() {
        let report = resolve_inspect_compat_with_env(Err(()), |cell| {
            (cell.env_var() == "ECHO_AGENT_CURSOR_SKILLS_ENABLED").then_some(true)
        });

        let skills = entry(&report, "cursor", "skills");
        assert!(skills.enabled);
        assert_eq!(skills.source, CompatSource::Env);
        let rules = entry(&report, "cursor", "rules");
        assert!(!rules.enabled);
        assert_eq!(rules.source, CompatSource::ConfigError);
        assert_eq!(
            serde_json::to_value(rules).unwrap(),
            serde_json::json!({
                "vendor": "cursor",
                "surface": "rules",
                "enabled": false,
                "source": "configError"
            })
        );
    }

    #[test]
    fn malformed_cell_fails_closed_without_erasing_valid_cells() {
        let effective_config: toml::Value = toml::from_str(
            r#"
[compat.cursor]
skills = false
rules = "malformed"
agents = true
[compat.claude]
hooks = false
"#,
        )
        .unwrap();
        let report = resolve_without_env(Ok(&effective_config));

        let skills = entry(&report, "cursor", "skills");
        assert!(!skills.enabled);
        assert_eq!(skills.source, CompatSource::Config);
        let rules = entry(&report, "cursor", "rules");
        assert!(!rules.enabled);
        assert_eq!(rules.source, CompatSource::ConfigError);
        let agents = entry(&report, "cursor", "agents");
        assert!(agents.enabled);
        assert_eq!(agents.source, CompatSource::Config);
        let hooks = entry(&report, "claude", "hooks");
        assert!(!hooks.enabled);
        assert_eq!(hooks.source, CompatSource::Config);
    }
}
