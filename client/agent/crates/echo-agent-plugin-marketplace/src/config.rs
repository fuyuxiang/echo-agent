//! Parse marketplace sources from `~/.echo-agent/config.toml`.
//!
//! Expected format:
//! ```toml
//! [[marketplace.sources]]
//! name = "EchoAgent Official"
//! git = "https://github.com/fuyuxiang/echo-agent-desktop.git"
//!
//! [[marketplace.sources]]
//! name = "Local Dev"
//! path = "~/dev/my-plugins"
//! ```

use std::path::PathBuf;

use serde::Deserialize;

use crate::types::{MarketplaceSource, SourceKind};

/// Raw TOML source entry.
#[derive(Debug, serde::Deserialize)]
struct RawSource {
    name: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    git: Option<String>,
    #[serde(default)]
    branch: Option<String>,
}

/// Whether remote plugin installs/updates must pin a full commit sha.
///
/// `[marketplace] require_sha = true` in config.toml, or
/// `ECHO_AGENT_MARKETPLACE_REQUIRE_SHA=1`. Tighten-only: either source can enable,
/// neither can override the other off. Defaults off so existing unpinned
/// catalogs keep installing.
pub fn load_require_sha(config: &toml::Value) -> bool {
    env_require_sha()
        || config
            .get("marketplace")
            .and_then(|m| m.get("require_sha"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

pub fn env_require_sha() -> bool {
    echo_agent_config::env_bool("ECHO_AGENT_MARKETPLACE_REQUIRE_SHA").unwrap_or(false)
}

/// Reads `[marketplace].sources` array. Returns empty vec if not configured.
pub fn load_sources(config: &toml::Value) -> Vec<MarketplaceSource> {
    let Some(marketplace) = config.get("marketplace") else {
        return Vec::new();
    };
    let Some(sources_val) = marketplace.get("sources") else {
        return Vec::new();
    };

    let raw_sources: Vec<RawSource> = match serde_json::to_value(sources_val)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
    {
        Some(s) => s,
        None => {
            // Try direct toml deserialization.
            match sources_val.clone().try_into::<Vec<RawSource>>() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("failed to parse marketplace.sources: {e}");
                    return Vec::new();
                }
            }
        }
    };

    raw_sources
        .into_iter()
        .filter_map(|raw| {
            let kind = if let Some(git_url) = raw.git {
                SourceKind::Git {
                    url: git_url,
                    branch: raw.branch,
                }
            } else if let Some(path_str) = raw.path {
                // Expand ~ to home directory.
                let expanded = if let Some(rest) = path_str.strip_prefix('~') {
                    echo_agent_config::discovery_home_dir()
                        .map(|h| {
                            h.join(rest.strip_prefix('/').unwrap_or(rest))
                                .to_string_lossy()
                                .to_string()
                        })
                        .unwrap_or(path_str.clone())
                } else {
                    path_str
                };
                SourceKind::Local {
                    path: PathBuf::from(expanded),
                }
            } else {
                tracing::warn!(
                    "marketplace source '{}' has neither 'path' nor 'git'",
                    raw.name
                );
                return None;
            };
            Some(MarketplaceSource {
                name: raw.name,
                kind,
            })
        })
        .collect()
}

/// Source descriptor from settings JSON.
///
/// Discriminated by the inner `"source"` field:
/// - `{ "source": "git", "url": "..." }`
/// - `{ "source": "github", "repo": "owner/repo" }`
/// - `{ "source": "local", "path": "..." }`
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "source", rename_all = "lowercase")]
enum SettingsSource {
    Git { url: String },
    Github { repo: String },
    Local { path: String },
}

/// A single entry under `extraKnownMarketplaces` or `known_marketplaces.json`.
#[derive(Debug, serde::Deserialize)]
struct SettingsEntry {
    source: SettingsSource,
}

/// Extract marketplace entries from a JSON object map (name -> config).
fn extract_marketplace_entries(
    marketplaces: &serde_json::Map<String, serde_json::Value>,
    seen_urls: &mut std::collections::HashSet<String>,
    sources: &mut Vec<MarketplaceSource>,
) {
    for (name, config) in marketplaces {
        let entry: SettingsEntry = match SettingsEntry::deserialize(config) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let kind = match entry.source {
            SettingsSource::Git { url } => {
                if !seen_urls.insert(url.clone()) {
                    continue;
                }
                SourceKind::Git { url, branch: None }
            }
            SettingsSource::Github { repo } => {
                let url = format!("https://github.com/{repo}.git");
                if !seen_urls.insert(url.clone()) {
                    continue;
                }
                SourceKind::Git { url, branch: None }
            }
            SettingsSource::Local { path: path_str } => {
                let expanded = if let Some(rest) = path_str.strip_prefix('~') {
                    echo_agent_config::discovery_home_dir()
                        .map(|h| {
                            h.join(rest.strip_prefix('/').unwrap_or(rest))
                                .to_string_lossy()
                                .to_string()
                        })
                        .unwrap_or(path_str)
                } else {
                    path_str
                };
                SourceKind::Local {
                    path: PathBuf::from(expanded),
                }
            }
        };
        sources.push(MarketplaceSource {
            name: name.clone(),
            kind,
        });
    }
}
/// Loads additional marketplace sources from `settings.json` (`extraKnownMarketplaces`)
/// and `known_marketplaces.json` files under `~/.echo-agent/` and `~/.claude/`.
pub fn load_extra_sources_from_settings(existing: &[MarketplaceSource]) -> Vec<MarketplaceSource> {
    let roots: Vec<PathBuf> = [
        echo_agent_config::user_echo_agent_home(),
        echo_agent_config::discovery_home_dir().map(|h| h.join(".claude")),
    ]
    .into_iter()
    .flatten()
    .collect();
    load_extra_sources_from_settings_in(existing, &roots)
}

/// Like [`load_extra_sources_from_settings`] but reads from explicit `roots`
/// instead of `~/.echo-agent`/`~/.claude`. Each root is checked for
/// `settings.local.json`, `settings.json` (`extraKnownMarketplaces` key), and
/// `plugins/known_marketplaces.json`. Lets callers (e.g. first-run auto-register
/// tests) stay isolated from the developer's real home dir.
pub fn load_extra_sources_from_settings_in(
    existing: &[MarketplaceSource],
    roots: &[PathBuf],
) -> Vec<MarketplaceSource> {
    let mut sources = Vec::new();
    // Seed seen_urls with URLs already in config.toml sources to avoid duplicates.
    let mut seen_urls: std::collections::HashSet<String> = existing
        .iter()
        .filter_map(|s| match &s.kind {
            SourceKind::Git { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();

    // Order matters: all settings files across roots, then all
    // known_marketplaces.json across roots — preserves the first-wins URL dedup
    // in extract_marketplace_entries. Don't reorder without auditing UI impact.
    for root in roots {
        for settings_name in ["settings.local.json", "settings.json"] {
            let path = root.join(settings_name);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let json: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "malformed settings.json");
                    continue;
                }
            };
            let Some(marketplaces) = json
                .get("extraKnownMarketplaces")
                .and_then(|v| v.as_object())
            else {
                continue;
            };
            extract_marketplace_entries(marketplaces, &mut seen_urls, &mut sources);
        }
    }

    for root in roots {
        let known = root.join("plugins").join("known_marketplaces.json");
        let content = match std::fs::read_to_string(&known) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %known.display(), error = %e, "malformed known_marketplaces.json");
                continue;
            }
        };
        // known_marketplaces.json is a top-level object with the same shape as
        // extraKnownMarketplaces (map of name → { source, ... }).
        let Some(marketplaces) = json.as_object() else {
            continue;
        };
        extract_marketplace_entries(marketplaces, &mut seen_urls, &mut sources);
    }

    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test that touches the process-global
    /// `ECHO_AGENT_MARKETPLACE_REQUIRE_SHA`, so they cannot race each other.
    static REQUIRE_SHA_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_local_source() {
        let config: toml::Value = toml::from_str(
            r#"
            [[marketplace.sources]]
            name = "Local Dev"
            path = "/home/user/plugins"
            "#,
        )
        .unwrap();
        let sources = load_sources(&config);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "Local Dev");
        assert!(
            matches!(&sources[0].kind, SourceKind::Local { path } if path == &PathBuf::from("/home/user/plugins"))
        );
    }

    #[test]
    fn parse_git_source() {
        let config: toml::Value = toml::from_str(
            r#"
            [[marketplace.sources]]
            name = "EchoAgent Official"
            git = "https://github.com/fuyuxiang/echo-agent-desktop.git"
            branch = "main"
            "#,
        )
        .unwrap();
        let sources = load_sources(&config);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "EchoAgent Official");
        assert!(matches!(
            &sources[0].kind,
            SourceKind::Git { url, branch }
                if url == "https://github.com/fuyuxiang/echo-agent-desktop.git"
                    && branch.as_deref() == Some("main")
        ));
    }

    #[test]
    fn parse_mixed_sources() {
        let config: toml::Value = toml::from_str(
            r#"
            [[marketplace.sources]]
            name = "Local"
            path = "/tmp/plugins"

            [[marketplace.sources]]
            name = "Remote"
            git = "https://example.com/plugins.git"
            "#,
        )
        .unwrap();
        let sources = load_sources(&config);
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn empty_config_returns_empty() {
        let config: toml::Value = toml::from_str("").unwrap();
        assert!(load_sources(&config).is_empty());
    }

    /// Drives the shipped composition: config alone, env alone, and the
    /// tighten-only rule (falsy env cannot relax config-set true).
    #[test]
    fn require_sha_policy_composition() {
        // Process-global env: serialize against any other env-touching test.
        let _guard = REQUIRE_SHA_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let empty: toml::Value = toml::from_str("").unwrap();
        let enabled: toml::Value = toml::from_str("[marketplace]\nrequire_sha = true\n").unwrap();

        // SAFETY: single-threaded within the lock; restored before release.
        unsafe { std::env::remove_var("ECHO_AGENT_MARKETPLACE_REQUIRE_SHA") };
        assert!(!load_require_sha(&empty), "absent everywhere → off");
        assert!(load_require_sha(&enabled), "config alone can enable");

        unsafe { std::env::set_var("ECHO_AGENT_MARKETPLACE_REQUIRE_SHA", "1") };
        assert!(load_require_sha(&empty), "env alone can enable");

        unsafe { std::env::set_var("ECHO_AGENT_MARKETPLACE_REQUIRE_SHA", "0") };
        assert!(
            load_require_sha(&enabled),
            "a falsy env must not relax config-set policy (tighten-only)"
        );

        unsafe { std::env::remove_var("ECHO_AGENT_MARKETPLACE_REQUIRE_SHA") };
    }

    #[test]
    fn overlay_cannot_loosen_require_sha() {
        let _guard = REQUIRE_SHA_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECHO_AGENT_MARKETPLACE_REQUIRE_SHA") };

        let layers = echo_agent_config::ConfigLayers {
            user: toml::from_str("[marketplace]\nrequire_sha = true\n").unwrap(),
            env_overlay: Some(toml::from_str("[marketplace]\nrequire_sha = false\n").unwrap()),
            ..Default::default()
        };
        assert!(load_require_sha(
            &layers.effective_config_base_without_overlay()
        ));
        assert!(!load_require_sha(&layers.effective_config_base()));
    }

    #[test]
    fn missing_sources_key_returns_empty() {
        let config: toml::Value = toml::from_str("[marketplace]\n").unwrap();
        assert!(load_sources(&config).is_empty());
    }

    #[test]
    fn source_without_path_or_git_skipped() {
        let config: toml::Value = toml::from_str(
            r#"
            [[marketplace.sources]]
            name = "Bad"
            "#,
        )
        .unwrap();
        assert!(load_sources(&config).is_empty());
    }

    #[test]
    fn extract_github_source() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "my-marketplace": {
                    "source": {
                        "source": "github",
                        "repo": "anthropics/claude-plugins-official"
                    },
                    "installLocation": "/tmp/test",
                    "lastUpdated": "2026-04-10T00:00:00Z",
                    "autoUpdate": true
                }
            }"#,
        )
        .unwrap();
        let marketplaces = json.as_object().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut sources = Vec::new();
        extract_marketplace_entries(marketplaces, &mut seen, &mut sources);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "my-marketplace");
        assert!(
            matches!(&sources[0].kind, SourceKind::Git { url, .. } if url == "https://github.com/anthropics/claude-plugins-official.git")
        );
    }

    #[test]
    fn extract_git_source_with_url() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "my-git-marketplace": {
                    "source": {
                        "source": "git",
                        "url": "git@github.com:org/repo.git"
                    }
                }
            }"#,
        )
        .unwrap();
        let marketplaces = json.as_object().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut sources = Vec::new();
        extract_marketplace_entries(marketplaces, &mut seen, &mut sources);
        assert_eq!(sources.len(), 1);
        assert!(
            matches!(&sources[0].kind, SourceKind::Git { url, .. } if url == "git@github.com:org/repo.git")
        );
    }

    #[test]
    fn extract_deduplicates_urls() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "a": {
                    "source": { "source": "github", "repo": "org/repo" }
                },
                "b": {
                    "source": { "source": "github", "repo": "org/repo" }
                }
            }"#,
        )
        .unwrap();
        let marketplaces = json.as_object().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut sources = Vec::new();
        extract_marketplace_entries(marketplaces, &mut seen, &mut sources);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn extract_skips_entry_without_source() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "no-source": {
                    "installLocation": "/tmp/test"
                }
            }"#,
        )
        .unwrap();
        let marketplaces = json.as_object().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut sources = Vec::new();
        extract_marketplace_entries(marketplaces, &mut seen, &mut sources);
        assert!(sources.is_empty());
    }
}
