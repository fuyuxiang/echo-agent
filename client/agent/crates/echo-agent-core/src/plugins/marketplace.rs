//! Marketplace plugin discovery.
//!
//! Sources:
//! - `extraKnownMarketplaces` in `.claude/settings.json` (project-level)
//! - `~/.claude/plugins/known_marketplaces.json` (user-level registry)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ResolvedMarketplace {
    pub name: String,
    pub path: PathBuf,
    pub plugin_dirs: Vec<PathBuf>,
}

/// Resolve marketplaces and their enabled plugins from `extraKnownMarketplaces`
/// and `enabledPlugins` in `.claude/settings.json`. Local directory sources only.
pub fn resolve(git_root: &Path) -> Vec<ResolvedMarketplace> {
    let settings_path = git_root.join(".claude").join("settings.json");
    let json: serde_json::Value = match std::fs::read_to_string(&settings_path) {
        Ok(c) => match serde_json::from_str(&c) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "malformed .claude/settings.json");
                return vec![];
            }
        },
        Err(_) => return vec![],
    };

    let enabled = enabled_plugin_names(&json);
    if enabled.is_empty() {
        return vec![];
    }

    let Some(marketplaces) = json
        .get("extraKnownMarketplaces")
        .and_then(|v| v.as_object())
    else {
        return vec![];
    };

    let mut result = Vec::new();
    for (name, config) in marketplaces {
        let Some(rel_path) = config
            .get("source")
            .and_then(|s| s.get("path"))
            .and_then(|p| p.as_str())
        else {
            continue;
        };

        let marketplace_path = git_root.join(rel_path);
        let plugins_dir = marketplace_path.join("plugins");
        let Ok(entries) = std::fs::read_dir(&plugins_dir) else {
            continue;
        };

        let mut plugin_dirs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let plugin_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if enabled.contains(plugin_name) {
                tracing::info!(marketplace = %name, plugin = plugin_name, "marketplace plugin");
                plugin_dirs.push(path);
            }
        }

        result.push(ResolvedMarketplace {
            name: name.clone(),
            path: marketplace_path,
            plugin_dirs,
        });
    }
    result
}

/// Enabled plugin names from `enabledPlugins` (`"name@marketplace"` keys).
fn enabled_plugin_names(json: &serde_json::Value) -> HashSet<String> {
    json.get("enabledPlugins")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter(|(_, v)| v.as_bool().unwrap_or(false))
                .filter_map(|(k, _)| k.split('@').next().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `enabledPlugins` from a settings JSON value into enabled/disabled lists.
///
/// The `enabledPlugins` object has keys like `"name@marketplace"` with boolean values.
/// Keys with `true` are returned in the first vec (enabled), `false` in the second (disabled).
/// The `@marketplace` suffix is stripped — only the plugin name is returned.
pub fn parse_enabled_disabled_plugins(json: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let Some(obj) = json.get("enabledPlugins").and_then(|v| v.as_object()) else {
        return (vec![], vec![]);
    };
    // Deduplicate by plugin name: the same name may appear under different
    // marketplace keys (e.g. "foo@market1": true, "foo@market2": false).
    // If any entry for a name is `false`, the plugin is disabled (safe default).
    let mut state: HashMap<String, bool> = HashMap::new();
    for (key, val) in obj {
        let name = key.split('@').next().unwrap_or(key).to_string();
        if name.is_empty() {
            continue;
        }
        let Some(value) = val.as_bool() else {
            continue;
        };
        let entry = state.entry(name).or_insert(value);
        // disabled (false) wins on conflict
        if !value {
            *entry = false;
        }
    }
    let mut enabled = Vec::new();
    let mut disabled = Vec::new();
    for (name, is_enabled) in state {
        if is_enabled {
            enabled.push(name);
        } else {
            disabled.push(name);
        }
    }
    (enabled, disabled)
}

/// Load and parse `enabledPlugins` from a `.claude/settings.json` file path.
///
/// Returns `(enabled, disabled)` plugin name lists.
/// Returns empty vecs if the file is missing or malformed.
pub fn load_enabled_disabled_plugins(path: &Path) -> (Vec<String>, Vec<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (vec![], vec![]),
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return (vec![], vec![]),
    };
    parse_enabled_disabled_plugins(&json)
}

// ── Compat known_marketplaces.json ────────────────────────────────────

/// Entry in `~/.claude/plugins/known_marketplaces.json`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnownMarketplaceEntry {
    install_location: PathBuf,
}

/// Resolve user-level marketplaces from `known_marketplaces.json`.
///
/// Returns marketplace entries with their local `installLocation` paths.
/// Plugin dirs are filtered to names present in user-level
/// `~/.claude/settings{.local}.json` `enabledPlugins` with any value (a
/// `false` entry is an installed-but-disabled plugin whose state we
/// mirror), so never-installed catalog plugins are not discovered.
pub fn resolve_known_marketplaces() -> Vec<ResolvedMarketplace> {
    let Some(home) = echo_agent_config::discovery_home_dir() else {
        return vec![];
    };
    resolve_known_marketplaces_in(&home.join(".claude"))
}

/// Like [`resolve_known_marketplaces`] but reads from an explicit `~/.claude`
/// root, so tests stay isolated from the developer's real home dir.
pub fn resolve_known_marketplaces_in(claude_dir: &Path) -> Vec<ResolvedMarketplace> {
    let json_path = claude_dir.join("plugins").join("known_marketplaces.json");
    let content = match std::fs::read_to_string(&json_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let registry: HashMap<String, KnownMarketplaceEntry> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse known_marketplaces.json");
            return vec![];
        }
    };

    let installed = installed_plugin_keys(claude_dir);

    registry
        .into_iter()
        .filter_map(|(name, entry)| {
            let path = entry.install_location;
            if !path.is_dir() {
                return None;
            }
            // Collect plugin subdirectories from plugins/ and external_plugins/
            let mut plugin_dirs = Vec::new();
            for subdir in &["plugins", "external_plugins"] {
                let dir = path.join(subdir);
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if !p.is_dir() {
                            continue;
                        }
                        let plugin_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        let is_installed = match installed.get(plugin_name) {
                            Some(None) => true,
                            Some(Some(marketplaces)) => marketplaces.contains(name.as_str()),
                            None => false,
                        };
                        if is_installed {
                            plugin_dirs.push(p);
                        }
                    }
                }
            }
            Some(ResolvedMarketplace {
                name,
                path,
                plugin_dirs,
            })
        })
        .collect()
}

/// `enabledPlugins` keys from `<claude_dir>/settings.local.json` and
/// `<claude_dir>/settings.json`, keyed by plugin name. `None` = a bare key
/// (matches any marketplace); `Some(set)` = only those marketplaces.
/// Entries with any boolean value count; non-boolean values are skipped.
fn installed_plugin_keys(claude_dir: &Path) -> HashMap<String, Option<HashSet<String>>> {
    let mut keys: HashMap<String, Option<HashSet<String>>> = HashMap::new();
    for settings_name in ["settings.local.json", "settings.json"] {
        let path = claude_dir.join(settings_name);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "malformed settings.json");
                continue;
            }
        };
        let Some(obj) = json.get("enabledPlugins").and_then(|v| v.as_object()) else {
            continue;
        };
        for (key, value) in obj {
            if !value.is_boolean() {
                continue;
            }
            let mut parts = key.splitn(2, '@');
            let Some(plugin_name) = parts.next().filter(|n| !n.is_empty()) else {
                continue;
            };
            match parts.next() {
                Some(marketplace) => {
                    if let Some(marketplaces) = keys
                        .entry(plugin_name.to_string())
                        .or_insert_with(|| Some(HashSet::new()))
                    {
                        marketplaces.insert(marketplace.to_string());
                    }
                }
                None => {
                    keys.insert(plugin_name.to_string(), None);
                }
            }
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_enabled_disabled_both() {
        let json = serde_json::json!({
            "enabledPlugins": {
                "alpha@marketplace": true,
                "beta@marketplace": false,
                "gamma@other": true
            }
        });
        let (enabled, disabled) = parse_enabled_disabled_plugins(&json);
        assert!(enabled.contains(&"alpha".to_string()));
        assert!(enabled.contains(&"gamma".to_string()));
        assert_eq!(enabled.len(), 2);
        assert_eq!(disabled.len(), 1);
        assert!(disabled.contains(&"beta".to_string()));
    }

    #[test]
    fn parse_enabled_disabled_empty() {
        let json = serde_json::json!({});
        let (enabled, disabled) = parse_enabled_disabled_plugins(&json);
        assert!(enabled.is_empty());
        assert!(disabled.is_empty());
    }

    #[test]
    fn parse_enabled_disabled_no_at_sign() {
        let json = serde_json::json!({
            "enabledPlugins": {
                "plain-name": true,
                "other-name": false
            }
        });
        let (enabled, disabled) = parse_enabled_disabled_plugins(&json);
        assert_eq!(enabled.len(), 1);
        assert!(enabled.contains(&"plain-name".to_string()));
        assert_eq!(disabled.len(), 1);
        assert!(disabled.contains(&"other-name".to_string()));
    }

    #[test]
    fn parse_enabled_disabled_skips_non_bool() {
        let json = serde_json::json!({
            "enabledPlugins": {
                "good@m": true,
                "bad@m": "yes",
                "ugly@m": 42
            }
        });
        let (enabled, disabled) = parse_enabled_disabled_plugins(&json);
        assert_eq!(enabled.len(), 1);
        assert!(enabled.contains(&"good".to_string()));
        assert!(disabled.is_empty());
    }

    #[test]
    fn load_enabled_disabled_missing_file() {
        let (enabled, disabled) =
            load_enabled_disabled_plugins(Path::new("/nonexistent/settings.json"));
        assert!(enabled.is_empty());
        assert!(disabled.is_empty());
    }

    #[test]
    fn load_enabled_disabled_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"enabledPlugins": {"foo@m": true, "bar@m": false}}"#,
        )
        .unwrap();
        let (enabled, disabled) = load_enabled_disabled_plugins(&path);
        assert_eq!(enabled.len(), 1);
        assert!(enabled.contains(&"foo".to_string()));
        assert_eq!(disabled.len(), 1);
        assert!(disabled.contains(&"bar".to_string()));
    }

    /// Build a `~/.claude`-style dir with one known marketplace named `mp`
    /// containing `plugins/{alpha,beta}` and `external_plugins/gamma`, plus a
    /// `settings.json` with the given content (skipped when `None`).
    fn make_known_marketplace(
        tmp: &Path,
        settings_json: Option<&str>,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let claude_dir = tmp.join(".claude");
        let mp_dir = tmp.join("mp-repo");
        for plugin in ["plugins/alpha", "plugins/beta", "external_plugins/gamma"] {
            std::fs::create_dir_all(mp_dir.join(plugin)).unwrap();
        }
        std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
        let known = serde_json::json!({
            "mp": { "installLocation": mp_dir.to_string_lossy() }
        });
        std::fs::write(
            claude_dir.join("plugins").join("known_marketplaces.json"),
            serde_json::to_string(&known).unwrap(),
        )
        .unwrap();
        if let Some(settings) = settings_json {
            std::fs::write(claude_dir.join("settings.json"), settings).unwrap();
        }
        (claude_dir, mp_dir)
    }

    fn plugin_dir_names(marketplaces: &[ResolvedMarketplace]) -> Vec<String> {
        let mut names: Vec<String> = marketplaces
            .iter()
            .flat_map(|m| &m.plugin_dirs)
            .filter_map(|d| d.file_name().and_then(|n| n.to_str()).map(String::from))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn known_marketplaces_filtered_to_enabled_plugins_including_false() {
        let tmp = tempfile::tempdir().unwrap();
        // `gamma@mp: false` = installed-but-disabled: still discovered.
        // `beta` is not listed at all: a never-installed catalog entry.
        let (claude_dir, mp_dir) = make_known_marketplace(
            tmp.path(),
            Some(r#"{"enabledPlugins": {"alpha@mp": true, "gamma@mp": false}}"#),
        );

        let resolved = resolve_known_marketplaces_in(&claude_dir);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "mp");
        assert_eq!(resolved[0].path, mp_dir);
        assert_eq!(
            plugin_dir_names(&resolved),
            vec!["alpha".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn known_marketplaces_key_with_other_marketplace_does_not_match() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude_dir, _) = make_known_marketplace(
            tmp.path(),
            Some(r#"{"enabledPlugins": {"alpha@other": true}}"#),
        );

        let resolved = resolve_known_marketplaces_in(&claude_dir);
        assert_eq!(resolved.len(), 1);
        assert!(plugin_dir_names(&resolved).is_empty());
    }

    #[test]
    fn known_marketplaces_unqualified_key_matches_any_marketplace() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude_dir, _) =
            make_known_marketplace(tmp.path(), Some(r#"{"enabledPlugins": {"alpha": true}}"#));

        let resolved = resolve_known_marketplaces_in(&claude_dir);
        assert_eq!(plugin_dir_names(&resolved), vec!["alpha".to_string()]);
    }

    #[test]
    fn known_marketplaces_no_settings_yields_no_plugin_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude_dir, _) = make_known_marketplace(tmp.path(), None);

        let resolved = resolve_known_marketplaces_in(&claude_dir);
        assert_eq!(resolved.len(), 1, "marketplace entry itself is kept");
        assert!(plugin_dir_names(&resolved).is_empty());
    }

    #[test]
    fn known_marketplaces_reads_settings_local_json_too() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude_dir, _) = make_known_marketplace(
            tmp.path(),
            Some(r#"{"enabledPlugins": {"alpha@mp": true}}"#),
        );
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{"enabledPlugins": {"gamma@mp": false}}"#,
        )
        .unwrap();

        let resolved = resolve_known_marketplaces_in(&claude_dir);
        assert_eq!(
            plugin_dir_names(&resolved),
            vec!["alpha".to_string(), "gamma".to_string()],
            "keys from settings.local.json and settings.json must both count"
        );
    }

    #[test]
    fn known_marketplaces_non_bool_enabled_value_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let (claude_dir, _) = make_known_marketplace(
            tmp.path(),
            Some(r#"{"enabledPlugins": {"alpha@mp": "yes", "beta@mp": true}}"#),
        );

        let resolved = resolve_known_marketplaces_in(&claude_dir);
        assert_eq!(plugin_dir_names(&resolved), vec!["beta".to_string()]);
    }

    #[test]
    fn parse_enabled_disabled_conflict_disabled_wins() {
        // Same plugin name from different marketplaces with conflicting values:
        // disabled (false) should win.
        let json = serde_json::json!({
            "enabledPlugins": {
                "conflict@market1": true,
                "conflict@market2": false
            }
        });
        let (enabled, disabled) = parse_enabled_disabled_plugins(&json);
        assert!(enabled.is_empty());
        assert_eq!(disabled.len(), 1);
        assert!(disabled.contains(&"conflict".to_string()));
    }
}
