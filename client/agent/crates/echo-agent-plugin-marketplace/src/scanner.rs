//! Marketplace plugin discovery.
//!
//! Supports two modes:
//! 1. **Indexed:** if a catalog index file exists (see `index::load_index` for
//!    the lookup order — `.echo-agent-plugin/marketplace.json` is preferred), use it.
//! 2. **Filesystem fallback:** walk `plugins/*/` and resolve manifests directly.

use std::path::Path;

use crate::catalog;
use crate::index;
use crate::types::{MarketplaceEntry, MarketplaceScan};

/// Scan a marketplace directory for plugins, reporting whether a
/// `plugin-index.json` component catalog was loaded.
///
/// Tries indexed mode first, falls back to filesystem scanning. The component
/// catalog is only consulted in indexed mode: its keys are defined as index
/// names, so the filesystem fallback ignores it.
pub fn scan_marketplace(root: &Path) -> MarketplaceScan {
    match index::load_index(root) {
        Ok(Some(idx)) => {
            tracing::debug!(
                "using marketplace index: {} ({} plugins)",
                idx.name,
                idx.plugins.len()
            );
            let plugin_catalog = catalog::load_catalog(root);
            let mut plugins = Vec::new();
            for entry in &idx.plugins {
                // URL-sourced entries: build entry from index metadata only
                // (the actual repo is cloned at install time, not scan time).
                if let Some((url, git_ref)) = entry.remote_url() {
                    let discovered = MarketplaceEntry {
                        name: entry.name.clone(),
                        version: entry.version.clone(),
                        description: entry.description.clone(),
                        category: entry.category.clone(),
                        author: entry.author.as_ref().map(|a| a.name.clone()),
                        tags: entry.tags.clone(),
                        keywords: entry.keywords.clone(),
                        domains: entry.domains.clone(),
                        homepage: entry.homepage.clone(),
                        relative_path: entry.name.clone(),
                        skill_count: 0,
                        has_hooks: false,
                        has_agents: false,
                        has_mcp: false,
                        remote_url: Some(url.to_string()),
                        remote_ref: git_ref.map(|s| s.to_string()),
                        remote_sha: entry.remote_sha().map(|s| s.to_string()),
                        remote_subdir: entry.remote_subdir().map(|s| s.to_string()),
                        components: entry.remote_sha().and_then(|sha| {
                            plugin_catalog
                                .as_ref()
                                .and_then(|c| c.components_for(&entry.name, Some(sha)).cloned())
                        }),
                    };
                    plugins.push(discovered);
                    continue;
                }

                let rel_path = match entry.resolved_marketplace_path() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            "marketplace index entry '{}' has invalid source path: {}",
                            entry.name,
                            e
                        );
                        continue;
                    }
                };
                let plugin_dir = match rel_path.join_under(root) {
                    Ok(path) => path,
                    Err(e) => {
                        tracing::warn!(
                            "marketplace index entry '{}' source path escapes marketplace root: {}",
                            entry.name,
                            e
                        );
                        continue;
                    }
                };
                if !plugin_dir.is_dir() {
                    tracing::warn!(
                        "marketplace index entry '{}' points to non-existent dir: {}",
                        entry.name,
                        plugin_dir.display()
                    );
                    continue;
                }
                let mut discovered = scan_single_plugin(&plugin_dir, rel_path.as_str());
                // Enrich from index metadata.
                if discovered.description.is_none() {
                    discovered.description = entry.description.clone();
                }
                discovered.category = entry.category.clone();
                discovered.tags = entry.tags.clone();
                discovered.keywords = entry.keywords.clone();
                discovered.domains = entry.domains.clone();
                discovered.homepage = entry.homepage.clone();
                if discovered.author.is_none() {
                    discovered.author = entry.author.as_ref().map(|a| a.name.clone());
                }
                discovered.components = plugin_catalog
                    .as_ref()
                    .and_then(|c| c.components_for(&entry.name, None).cloned());
                plugins.push(discovered);
            }
            MarketplaceScan {
                entries: plugins,
                catalog_loaded: plugin_catalog.is_some(),
            }
        }
        Ok(None) => {
            // No index — filesystem fallback.
            MarketplaceScan {
                entries: scan_filesystem(root),
                catalog_loaded: false,
            }
        }
        Err(e) => {
            // Invalid index — warn and fall back.
            tracing::warn!("marketplace index invalid, falling back to scan: {e}");
            MarketplaceScan {
                entries: scan_filesystem(root),
                catalog_loaded: false,
            }
        }
    }
}

/// Filesystem fallback: walk `plugins/*/` and discover each.
fn scan_filesystem(root: &Path) -> Vec<MarketplaceEntry> {
    let plugins_dir = root.join("plugins");
    if !plugins_dir.is_dir() {
        return Vec::new();
    }

    let mut entries: Vec<_> = match std::fs::read_dir(&plugins_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            tracing::warn!("failed to read plugins dir: {e}");
            return Vec::new();
        }
    };
    entries.sort_by_key(|e| e.file_name());

    let mut plugins = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let rel_path = format!("plugins/{name}");
        plugins.push(scan_single_plugin(&path, &rel_path));
    }
    plugins
}

/// Scan a single plugin directory for metadata and components.
fn scan_single_plugin(plugin_dir: &Path, relative_path: &str) -> MarketplaceEntry {
    // Load manifest using runtime conventions.
    let manifest_result = echo_agent_core::plugins::manifest::load_manifest(plugin_dir);
    let manifest = match &manifest_result {
        Ok(echo_agent_core::plugins::manifest::ManifestLoadResult::Found(m)) => Some(m.as_ref()),
        _ => None,
    };
    let dir_name = plugin_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let name = manifest
        .map(|m| m.name.clone())
        .unwrap_or_else(|| dir_name.clone());
    let version = manifest.and_then(|m| m.version.clone());
    let description = manifest.and_then(|m| m.description.clone());
    let author = manifest.and_then(|m| m.author.as_ref().and_then(|a| a.name.clone()));

    // Count components using manifest conventions with defaults.
    let (skill_count, has_hooks, has_agents, has_mcp) = if let Some(m) = manifest {
        let skill_dirs = m.skill_dirs(plugin_dir);
        let sc = skill_dirs
            .iter()
            .filter(|d| d.is_dir())
            .flat_map(|d| std::fs::read_dir(d).ok())
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("SKILL.md").exists())
            .count();
        let hk = m.hooks_path(plugin_dir).is_some_and(|p| p.exists());
        let ag = m.agent_dirs(plugin_dir).iter().any(|d| {
            d.is_dir()
                && std::fs::read_dir(d)
                    .ok()
                    .is_some_and(|mut rd| rd.next().is_some())
        });
        let mc = m.mcp_config_path(plugin_dir).is_some_and(|p| p.exists());
        (sc, hk, ag, mc)
    } else {
        // No manifest — check defaults.
        let skills_dir = plugin_dir.join("skills");
        let sc = if skills_dir.is_dir() {
            std::fs::read_dir(&skills_dir)
                .ok()
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().join("SKILL.md").exists())
                        .count()
                })
                .unwrap_or(0)
        } else {
            0
        };
        let hk = plugin_dir.join("hooks").join("hooks.json").exists();
        let ag = plugin_dir.join("agents").is_dir();
        let mc = plugin_dir.join(".mcp.json").exists();
        (sc, hk, ag, mc)
    };

    MarketplaceEntry {
        name,
        version,
        description,
        category: None,
        author,
        tags: Vec::new(),
        keywords: Vec::new(),
        domains: Vec::new(),
        homepage: None,
        relative_path: relative_path.to_string(),
        skill_count,
        has_hooks,
        has_agents,
        has_mcp,
        remote_url: None,
        remote_ref: None,
        remote_sha: None,
        remote_subdir: None,
        components: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a minimal plugin directory with a manifest.
    fn make_plugin(dir: &Path, name: &str, version: &str) {
        let plugin_dir = dir.join("plugins").join(name);
        let claude_dir = plugin_dir.join(".claude-plugin");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}","description":"Test {name}"}}"#),
        )
        .unwrap();
        // Add a skill.
        let skill_dir = plugin_dir.join("skills").join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# My Skill").unwrap();
    }

    #[test]
    fn filesystem_scan_discovers_plugins() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin(dir.path(), "plugin-a", "1.0.0");
        make_plugin(dir.path(), "plugin-b", "2.0.0");

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].name, "plugin-a");
        assert_eq!(plugins[0].version.as_deref(), Some("1.0.0"));
        assert_eq!(plugins[0].skill_count, 1);
        assert!(plugins[0].keywords.is_empty());
        assert_eq!(plugins[1].name, "plugin-b");
    }

    #[test]
    fn indexed_scan_uses_index() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin(dir.path(), "indexed-plugin", "1.0.0");

        // Create marketplace index.
        let claude_dir = dir.path().join(".claude-plugin");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("marketplace.json"),
            r#"{
                "name": "test-marketplace",
                "plugins": [{
                    "name": "indexed-plugin",
                    "description": "From index",
                    "category": "development",
                    "source": { "type": "local", "path": "./plugins/indexed-plugin" },
                    "tags": ["test"],
                    "keywords": ["editor", "code"]
                }]
            }"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "indexed-plugin");
        assert_eq!(plugins[0].category.as_deref(), Some("development"));
        assert_eq!(plugins[0].tags, vec!["test"]);
        assert_eq!(plugins[0].keywords, vec!["editor", "code"]);
        // Version comes from per-plugin manifest, not index.
        assert_eq!(plugins[0].version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn url_sourced_entry_carries_keywords() {
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_dir = dir.path().join(".echo-agent-plugin");
        std::fs::create_dir_all(&echo_agent_dir).unwrap();
        std::fs::write(
            echo_agent_dir.join("marketplace.json"),
            r#"{
                "name": "kw-marketplace",
                "plugins": [{
                    "name": "remote-plugin",
                    "source": { "source": "url", "url": "https://github.com/acme/remote-plugin.git" },
                    "homepage": "https://acme.example.com",
                    "tags": ["t1"],
                    "keywords": ["acme", "remote tool"],
                    "domains": ["acme.example.com"]
                }]
            }"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "remote-plugin");
        assert_eq!(
            plugins[0].remote_url.as_deref(),
            Some("https://github.com/acme/remote-plugin.git")
        );
        assert_eq!(plugins[0].tags, vec!["t1"]);
        assert_eq!(plugins[0].keywords, vec!["acme", "remote tool"]);
        assert_eq!(plugins[0].domains, vec!["acme.example.com"]);
        assert!(plugins[0].remote_subdir.is_none());
    }

    #[test]
    fn url_sourced_entry_with_path_sets_remote_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_dir = dir.path().join(".echo-agent-plugin");
        std::fs::create_dir_all(&echo_agent_dir).unwrap();
        std::fs::write(
            echo_agent_dir.join("marketplace.json"),
            r#"{
                "name": "acme-marketplace",
                "plugins": [{
                    "name": "acme",
                    "source": {
                        "source": "url",
                        "url": "https://github.com/acme/agent-skills.git",
                        "sha": "61f1903bed7b322c9745f6ba67095bc006de7e63",
                        "path": "plugins/acme"
                    }
                }]
            }"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "acme");
        assert_eq!(
            plugins[0].remote_url.as_deref(),
            Some("https://github.com/acme/agent-skills.git")
        );
        assert_eq!(plugins[0].remote_subdir.as_deref(), Some("plugins/acme"));
    }

    #[test]
    fn indexed_scan_rejects_traversal_path() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        make_plugin(outside.path(), "escaped-plugin", "1.0.0");

        let claude_dir = dir.path().join(".claude-plugin");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("marketplace.json"),
            r#"{
                "name": "test-marketplace",
                "plugins": [{
                    "name": "escaped-plugin",
                    "source": { "type": "local", "path": "../escaped-plugin" }
                }]
            }"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert!(plugins.is_empty());
    }

    #[test]
    fn indexed_scan_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        make_plugin(outside.path(), "escaped-plugin", "1.0.0");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("plugins").join("escaped-plugin"),
            dir.path().join("escaped"),
        )
        .unwrap();

        let claude_dir = dir.path().join(".claude-plugin");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("marketplace.json"),
            r#"{
                "name": "test-marketplace",
                "plugins": [{
                    "name": "escaped-plugin",
                    "source": { "type": "local", "path": "escaped" }
                }]
            }"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert!(plugins.is_empty());
    }

    #[test]
    fn echo_agent_plugin_dir_index_drives_scan_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin(dir.path(), "echo-agent-plugin", "1.0.0");

        let echo_agent_dir = dir.path().join(".echo-agent-plugin");
        std::fs::create_dir_all(&echo_agent_dir).unwrap();
        std::fs::write(
            echo_agent_dir.join("marketplace.json"),
            r#"{
                "name": "echo-agent-marketplace",
                "plugins": [{
                    "name": "echo-agent-plugin",
                    "description": "From the .echo-agent-plugin index",
                    "category": "design",
                    "source": { "type": "local", "path": "./plugins/echo-agent-plugin" },
                    "tags": ["echoagent"]
                }]
            }"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "echo-agent-plugin");
        assert_eq!(plugins[0].category.as_deref(), Some("design"));
        assert_eq!(plugins[0].tags, vec!["echoagent"]);
        assert!(plugins[0].keywords.is_empty());
    }

    #[test]
    fn invalid_index_falls_back_to_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin(dir.path(), "fallback-plugin", "1.0.0");

        let claude_dir = dir.path().join(".claude-plugin");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("marketplace.json"), "not valid json").unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "fallback-plugin");
    }

    #[test]
    fn empty_marketplace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
        let plugins = scan_marketplace(dir.path()).entries;
        assert!(plugins.is_empty());
    }

    #[test]
    fn no_plugins_dir() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = scan_marketplace(dir.path()).entries;
        assert!(plugins.is_empty());
    }

    #[test]
    fn plugin_with_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("hooked");
        std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
        std::fs::write(
            plugin_dir.join("hooks").join("hooks.json"),
            r#"{"hooks":{}}"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].has_hooks);
        assert_eq!(plugins[0].name, "hooked");
    }

    fn write_echo_agent_file(dir: &Path, file: &str, content: &str) {
        let echo_agent_dir = dir.join(".echo-agent-plugin");
        std::fs::create_dir_all(&echo_agent_dir).unwrap();
        std::fs::write(echo_agent_dir.join(file), content).unwrap();
    }

    #[test]
    fn catalog_attaches_components_to_indexed_local_entry() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin(dir.path(), "plugin-a", "1.0.0");
        write_echo_agent_file(
            dir.path(),
            "marketplace.json",
            r#"{
                "name": "m",
                "plugins": [
                    { "name": "plugin-a", "source": { "type": "local", "path": "./plugins/plugin-a" } }
                ]
            }"#,
        );
        write_echo_agent_file(
            dir.path(),
            "plugin-index.json",
            r#"{
                "version": 1,
                "plugins": {
                    "plugin-a": {
                        "components": {
                            "skills": [ { "name": "my-skill", "description": "Does things" } ],
                            "commands": [ { "name": "/go" } ]
                        }
                    }
                }
            }"#,
        );

        let scan = scan_marketplace(dir.path());
        assert!(scan.catalog_loaded);
        assert_eq!(scan.entries.len(), 1);
        let components = scan.entries[0].components.as_ref().unwrap();
        assert_eq!(components.skills[0].name, "my-skill");
        assert_eq!(
            components.skills[0].description.as_deref(),
            Some("Does things")
        );
        assert_eq!(components.commands[0].name, "/go");
        // Legacy scan fields still populated alongside catalog data.
        assert_eq!(scan.entries[0].skill_count, 1);
    }

    #[test]
    fn catalog_lookup_keyed_by_index_name_not_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        // Manifest name "plugin-a" diverges from index name "index-name".
        make_plugin(dir.path(), "plugin-a", "1.0.0");
        write_echo_agent_file(
            dir.path(),
            "marketplace.json",
            r#"{
                "name": "m",
                "plugins": [
                    { "name": "index-name", "source": { "type": "local", "path": "./plugins/plugin-a" } }
                ]
            }"#,
        );
        write_echo_agent_file(
            dir.path(),
            "plugin-index.json",
            r#"{
                "version": 1,
                "plugins": {
                    "index-name": { "components": { "skills": [ { "name": "indexed-skill" } ] } },
                    "plugin-a": { "components": { "skills": [ { "name": "wrong-skill" } ] } }
                }
            }"#,
        );

        let scan = scan_marketplace(dir.path());
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].name, "plugin-a");
        let components = scan.entries[0].components.as_ref().unwrap();
        assert_eq!(components.skills[0].name, "indexed-skill");
    }

    fn url_marketplace_index(sha_field: &str) -> String {
        format!(
            r#"{{
                "name": "m",
                "plugins": [{{
                    "name": "remote-plugin",
                    "source": {{ "source": "url", "url": "https://example.com/r.git"{sha_field} }}
                }}]
            }}"#
        )
    }

    const URL_CATALOG: &str = r#"{
        "version": 1,
        "plugins": {
            "remote-plugin": {
                "sha": "61f1903bed7b322c9745f6ba67095bc006de7e63",
                "components": { "skills": [ { "name": "remote-skill" } ] }
            }
        }
    }"#;

    #[test]
    fn url_entry_gets_components_when_sha_matches() {
        let dir = tempfile::tempdir().unwrap();
        write_echo_agent_file(
            dir.path(),
            "marketplace.json",
            &url_marketplace_index(r#", "sha": "61f1903bed7b322c9745f6ba67095bc006de7e63""#),
        );
        write_echo_agent_file(dir.path(), "plugin-index.json", URL_CATALOG);

        let scan = scan_marketplace(dir.path());
        assert!(scan.catalog_loaded);
        let components = scan.entries[0].components.as_ref().unwrap();
        assert_eq!(components.skills[0].name, "remote-skill");
    }

    #[test]
    fn url_entry_components_hidden_on_sha_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        write_echo_agent_file(
            dir.path(),
            "marketplace.json",
            &url_marketplace_index(r#", "sha": "0000000000000000000000000000000000000000""#),
        );
        write_echo_agent_file(dir.path(), "plugin-index.json", URL_CATALOG);

        let scan = scan_marketplace(dir.path());
        assert!(scan.catalog_loaded);
        assert!(scan.entries[0].components.is_none());
    }

    #[test]
    fn url_entry_without_pinned_sha_gets_no_components() {
        let dir = tempfile::tempdir().unwrap();
        write_echo_agent_file(dir.path(), "marketplace.json", &url_marketplace_index(""));
        write_echo_agent_file(dir.path(), "plugin-index.json", URL_CATALOG);

        let scan = scan_marketplace(dir.path());
        assert!(scan.catalog_loaded);
        assert!(scan.entries[0].components.is_none());
    }

    #[test]
    fn malformed_catalog_degrades_to_no_components() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin(dir.path(), "plugin-a", "1.0.0");
        write_echo_agent_file(
            dir.path(),
            "marketplace.json",
            r#"{
                "name": "m",
                "plugins": [
                    { "name": "plugin-a", "source": { "type": "local", "path": "./plugins/plugin-a" } }
                ]
            }"#,
        );
        write_echo_agent_file(dir.path(), "plugin-index.json", "not json");

        let scan = scan_marketplace(dir.path());
        assert!(!scan.catalog_loaded);
        assert_eq!(scan.entries.len(), 1);
        assert!(scan.entries[0].components.is_none());
    }

    #[test]
    fn filesystem_fallback_ignores_catalog() {
        let dir = tempfile::tempdir().unwrap();
        make_plugin(dir.path(), "plugin-a", "1.0.0");
        write_echo_agent_file(
            dir.path(),
            "plugin-index.json",
            r#"{
                "version": 1,
                "plugins": { "plugin-a": { "components": { "skills": [ { "name": "s" } ] } } }
            }"#,
        );

        let scan = scan_marketplace(dir.path());
        assert!(!scan.catalog_loaded);
        assert_eq!(scan.entries.len(), 1);
        assert!(scan.entries[0].components.is_none());
        assert_eq!(scan.entries[0].skill_count, 1);
    }

    #[test]
    fn root_plugin_json_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("root-manifest");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name":"root-manifest","version":"2.0.0","description":"Root manifest"}"#,
        )
        .unwrap();

        let plugins = scan_marketplace(dir.path()).entries;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "root-manifest");
        assert_eq!(plugins[0].version.as_deref(), Some("2.0.0"));
    }
}
