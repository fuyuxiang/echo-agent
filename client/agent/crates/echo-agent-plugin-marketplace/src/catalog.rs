//! Parse the CI-generated `plugin-index.json` component catalog.
//!
//! Directory precedence mirrors `index::load_index`:
//! `.echo-agent-plugin/plugin-index.json` (preferred), then
//! `.claude-plugin/plugin-index.json` — but only one filename is probed per
//! directory, and a present-but-unreadable/unparseable preferred catalog does
//! not fall back to the other directory (never serve possibly-stale data when
//! the authoritative file is broken). The catalog is presentation-layer
//! enrichment only: failures degrade to `None` and never fail a marketplace
//! listing.

use std::collections::HashMap;
use std::path::Path;

use echo_agent_hooks_plugins_types::PluginComponents;
use serde::Deserialize;

/// Catalog format version this client understands.
const SUPPORTED_VERSION: u64 = 1;

/// Top-level `plugin-index.json` catalog, keyed by index plugin name.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginCatalog {
    pub version: u64,
    #[serde(default)]
    pub plugins: HashMap<String, CatalogEntry>,
}

/// Per-plugin catalog entry.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    /// Commit the components were extracted from (required for URL-sourced
    /// entries; optional for in-repo plugins).
    #[serde(default)]
    pub sha: Option<String>,
    pub components: PluginComponents,
}

impl PluginCatalog {
    /// Components for an index entry, gated on the pinned SHA for
    /// URL-sourced entries: when `index_sha` is `Some`, the catalog entry
    /// must carry an equal `sha` or the components are treated as absent.
    pub fn components_for(
        &self,
        index_name: &str,
        index_sha: Option<&str>,
    ) -> Option<&PluginComponents> {
        let entry = self.plugins.get(index_name)?;
        if let Some(expected) = index_sha
            && entry.sha.as_deref() != Some(expected)
        {
            tracing::debug!(
                plugin = index_name,
                catalog_sha = entry.sha.as_deref().unwrap_or(""),
                index_sha = expected,
                "marketplace catalog sha mismatch; hiding components"
            );
            return None;
        }
        Some(&entry.components)
    }
}

/// Load `plugin-index.json` from a marketplace root, or `None` when absent,
/// malformed, or of an unsupported version. A missing file falls through to
/// the next candidate directory; a broken one does not (see module docs).
pub fn load_catalog(marketplace_root: &Path) -> Option<PluginCatalog> {
    let candidates = [
        marketplace_root
            .join(".echo-agent-plugin")
            .join("plugin-index.json"),
        marketplace_root
            .join(".claude-plugin")
            .join("plugin-index.json"),
    ];
    for path in &candidates {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!("failed to read {}: {e}", path.display());
                return None;
            }
        };
        let mut catalog: PluginCatalog = match serde_json::from_str(&content) {
            Ok(catalog) => catalog,
            Err(e) => {
                tracing::warn!("failed to parse {}: {e}", path.display());
                return None;
            }
        };
        if catalog.version != SUPPORTED_VERSION {
            tracing::warn!(
                "unsupported plugin catalog version {} in {}",
                catalog.version,
                path.display()
            );
            return None;
        }
        for entry in catalog.plugins.values_mut() {
            entry.components.sanitize();
        }
        return Some(catalog);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_catalog(dir: &Path, subdir: &str, content: &str) {
        let d = dir.join(subdir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("plugin-index.json"), content).unwrap();
    }

    const BASIC: &str = r#"{
        "version": 1,
        "plugins": {
            "superpowers": {
                "sha": "61f1903bed7b322c9745f6ba67095bc006de7e63",
                "components": {
                    "skills": [
                        { "name": "brainstorming", "description": "Structured ideation" }
                    ],
                    "commands": [ { "name": "/brainstorm" } ],
                    "hooks": [ { "name": "PreToolUse", "description": "Bash" } ]
                }
            }
        }
    }"#;

    #[test]
    fn load_catalog_parses_echo_agent_plugin_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(dir.path(), ".echo-agent-plugin", BASIC);
        let catalog = load_catalog(dir.path()).unwrap();
        let components = catalog.components_for("superpowers", None).unwrap();
        assert_eq!(components.skills.len(), 1);
        assert_eq!(components.skills[0].name, "brainstorming");
        assert_eq!(
            components.skills[0].description.as_deref(),
            Some("Structured ideation")
        );
        assert_eq!(components.commands[0].name, "/brainstorm");
        assert_eq!(components.hooks[0].name, "PreToolUse");
        assert!(components.agents.is_empty());
    }

    #[test]
    fn load_catalog_falls_back_to_claude_plugin_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(dir.path(), ".claude-plugin", BASIC);
        assert!(load_catalog(dir.path()).is_some());
    }

    #[test]
    fn load_catalog_prefers_echo_agent_dir_over_claude_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(dir.path(), ".echo-agent-plugin", BASIC);
        write_catalog(
            dir.path(),
            ".claude-plugin",
            r#"{"version": 1, "plugins": {"other": {"components": {}}}}"#,
        );
        let catalog = load_catalog(dir.path()).unwrap();
        assert!(catalog.plugins.contains_key("superpowers"));
        assert!(!catalog.plugins.contains_key("other"));
    }

    #[test]
    fn load_catalog_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_catalog(dir.path()).is_none());
    }

    #[test]
    fn load_catalog_malformed_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(dir.path(), ".echo-agent-plugin", "not json");
        assert!(load_catalog(dir.path()).is_none());
    }

    #[test]
    fn load_catalog_broken_preferred_does_not_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(dir.path(), ".echo-agent-plugin", "not json");
        write_catalog(dir.path(), ".claude-plugin", BASIC);
        assert!(load_catalog(dir.path()).is_none());
    }

    #[test]
    fn load_catalog_unsupported_version_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(
            dir.path(),
            ".echo-agent-plugin",
            r#"{"version": 2, "plugins": {}}"#,
        );
        assert!(load_catalog(dir.path()).is_none());
    }

    #[test]
    fn load_catalog_ignores_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(
            dir.path(),
            ".echo-agent-plugin",
            r#"{
                "$schema": "https://echo-agent.invalid/echoagent/plugin-index.schema.json",
                "version": 1,
                "generatedAt": "2026-06-09T12:00:00Z",
                "plugins": {
                    "p": { "components": { "skills": [{"name": "s", "extra": 1}] }, "future": true }
                }
            }"#,
        );
        let catalog = load_catalog(dir.path()).unwrap();
        assert_eq!(
            catalog.components_for("p", None).unwrap().skills[0].name,
            "s"
        );
    }

    #[test]
    fn load_catalog_sanitizes_entries() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(
            dir.path(),
            ".echo-agent-plugin",
            r#"{
                "version": 1,
                "plugins": {
                    "p": { "components": { "skills": [{"name": "a\u001b[31mb", "description": "x\u0007y"}] } }
                }
            }"#,
        );
        let catalog = load_catalog(dir.path()).unwrap();
        let components = catalog.components_for("p", None).unwrap();
        assert_eq!(components.skills[0].name, "a[31mb");
        assert_eq!(components.skills[0].description.as_deref(), Some("xy"));
    }

    #[test]
    fn components_for_gates_on_sha() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(dir.path(), ".echo-agent-plugin", BASIC);
        let catalog = load_catalog(dir.path()).unwrap();
        let pinned = "61f1903bed7b322c9745f6ba67095bc006de7e63";
        assert!(
            catalog
                .components_for("superpowers", Some(pinned))
                .is_some()
        );
        assert!(
            catalog
                .components_for("superpowers", Some("deadbeef"))
                .is_none()
        );
        assert!(catalog.components_for("unknown", None).is_none());
    }

    #[test]
    fn components_for_requires_catalog_sha_when_index_pinned() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(
            dir.path(),
            ".echo-agent-plugin",
            r#"{"version": 1, "plugins": {"p": {"components": {"skills": [{"name": "s"}]}}}}"#,
        );
        let catalog = load_catalog(dir.path()).unwrap();
        assert!(catalog.components_for("p", Some("abc123")).is_none());
        assert!(catalog.components_for("p", None).is_some());
    }
}
