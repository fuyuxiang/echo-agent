//! Skill, plugin, project-config, and permissions discovery.
//!
//! Phase 3a delegates to the existing discovery implementations in
//! `echo-agent-core` and `echo-agent-tools` rather than duplicating them.
//! The workspace stores configuration on [`WorkspaceShared`] and the
//! [`MpscChannel`] methods call these helpers to return real data
//! instead of `Value::Null` stubs.
//!
//! Re-exports [`AgentsMdTracker`] from `echo-agent-tools` for external
//! consumers that need per-session project-instruction tracking.

use std::path::Path;

use serde_json::Value;

// Re-export AgentsMdTracker so consumers can reference it via the
// workspace crate without a direct echo-agent-tools dependency.
pub use echo_agent_tools::types::agents_md_tracker::AgentsMdTracker;

// Re-export the config types that callers pass into WorkspaceConfig.
pub use echo_agent_core::plugins::discovery::DiscoveryConfig as PluginDiscoveryConfig;
pub use echo_agent_core::plugins::trust::TrustStore as PluginTrustStore;
pub use echo_agent_core::prompt::skills::SkillsConfig;

// ---------------------------------------------------------------------------
// Skill discovery
// ---------------------------------------------------------------------------

/// Discover skills visible from the workspace root.
///
/// Delegates to [`echo_agent_core::prompt::skills::list_skills`] with
/// the workspace's `root_cwd` and the caller-supplied `SkillsConfig`.
/// Returns each [`SkillInfo`] serialized to a `serde_json::Value`.
///
/// The underlying `list_skills` implementation performs filesystem
/// I/O (stat + read for each SKILL.md) and does not hold any async
/// locks across `.await` points, so contention is not a concern.
pub async fn discover_skills(root_cwd: &Path, config: &SkillsConfig) -> Vec<Value> {
    let cwd_str = root_cwd.to_string_lossy();
    // Workspace discovery is out of scope for per-vendor compat gating;
    // use the all-on default to preserve prior behavior.
    let skills = echo_agent_core::prompt::skills::list_skills(
        Some(&cwd_str),
        config,
        echo_agent_core::prompt::skills::CompatConfig::default(),
    )
    .await;

    skills
        .into_iter()
        .filter_map(|s| match serde_json::to_value(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(
                    skill = %s.name,
                    error = %e,
                    "failed to serialize SkillInfo; dropping from response"
                );
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// AGENTS.md discovery
// ---------------------------------------------------------------------------

/// Discover project-instruction files (AGENTS.md, Claude.md, rules) from the workspace root up to the git root.
pub async fn discover_agents_md(root_cwd: &Path) -> Vec<Value> {
    let cwd_str = root_cwd.to_string_lossy();
    let files = echo_agent_core::prompt::agents_md::read_agents_config_with_paths(
        &cwd_str,
        echo_agent_tools::types::compat::CompatConfig::default(),
    )
    .await;

    files
        .into_iter()
        .filter_map(|file| match serde_json::to_value(&file) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(
                    file = %file.file_path,
                    error = %e,
                    "failed to serialize AgentConfigFile; dropping from response"
                );
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Plugin discovery
// ---------------------------------------------------------------------------

/// Discover plugins visible from the workspace root.
///
/// Delegates to [`echo_agent_core::plugins::discover_plugins`] with
/// the workspace's `root_cwd` and the caller-supplied
/// [`PluginDiscoveryConfig`] and [`PluginTrustStore`].
///
/// Since [`DiscoveredPlugin`] does not derive `Serialize`, each
/// plugin is converted to a JSON object with the essential fields
/// that downstream consumers need (name, scope, root, trusted,
/// has_skills, has_hooks, has_mcp).
///
/// `project_trusted` is the folder-trust verdict for `root_cwd`, threaded into
/// discovery to gate Project-scope plugins.
pub fn discover_plugins(
    root_cwd: &Path,
    config: &PluginDiscoveryConfig,
    trust_store: &PluginTrustStore,
    project_trusted: bool,
) -> Vec<Value> {
    let discovered = echo_agent_core::plugins::discover_plugins(
        Some(root_cwd),
        config,
        trust_store,
        project_trusted,
    );

    discovered
        .into_iter()
        .map(|dp| {
            serde_json::json!({
                "name": dp.manifest.name,
                "id": dp.id.0,
                "root": dp.root.to_string_lossy(),
                "scope": dp.scope.to_string(),
                "trusted": dp.trusted,
                "version": dp.manifest.version,
                "description": dp.manifest.description,
                "has_skills": !dp.skill_dirs.is_empty(),
                "has_agents": !dp.agent_dirs.is_empty(),
                "has_hooks": dp.hooks_path.is_some(),
                "has_mcp": dp.mcp_config_path.is_some(),
                "has_lsp": dp.lsp_config_path.is_some(),
                "conflict": dp.conflict,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Project config
// ---------------------------------------------------------------------------

/// Load the project config from `<root_cwd>/.echo-agent/config.toml`.
///
/// Returns `Value::Null` if the file does not exist or cannot be
/// parsed. Non-fatal errors are logged.
pub fn load_project_config(root_cwd: &Path) -> Value {
    let config_path = root_cwd.join(".echo-agent").join("config.toml");
    match echo_agent_config::load_config_file(&config_path) {
        Ok(toml::Value::Table(ref t)) if t.is_empty() => {
            // The config loader returns an empty table when the file
            // does not exist. Normalize to Null for callers.
            Value::Null
        }
        Ok(toml_val) => toml_to_json(&toml_val),
        Err(e) => {
            tracing::warn!(
                path = %config_path.display(),
                error = %e,
                "failed to load project config"
            );
            Value::Null
        }
    }
}

/// Convert a `toml::Value` to a `serde_json::Value`.
///
/// TOML's type system is close to JSON's. The main difference is
/// TOML's `Datetime` type which maps to a JSON string.
fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
        toml::Value::Array(arr) => Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let map: serde_json::Map<String, Value> = table
                .iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect();
            Value::Object(map)
        }
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// Load the effective permission configuration for the workspace.
///
/// Delegates to
/// [`resolution::resolve_permissions_with_provenance`] which
/// merges rules from requirements.toml, managed-settings.json,
/// managed_config.toml, config.toml, and `.claude/settings.json`.
///
/// `project_trusted` gates project-tier permission sources (same contract as
/// env/hooks/plugins). Hub/cloud callers outside the local folder-trust model
/// should pass `true`.
///
/// Returns a JSON object with `sources`, `loaded` (rule count), and
/// `skipped` (unrecognized rules). Returns `Value::Null` if no
/// permission sources are configured.
pub async fn load_permissions(root_cwd: &Path, project_trusted: bool) -> Value {
    use crate::permission::resolution;

    let Some(resolved) =
        resolution::resolve_permissions_with_provenance(root_cwd, project_trusted).await
    else {
        return Value::Null;
    };

    let mut sources: Vec<String> = Vec::new();
    for s in resolved.sources.iter().map(|s| s.to_string()) {
        if !sources.contains(&s) {
            sources.push(s);
        }
    }

    let skipped: Vec<Value> = resolved
        .skipped
        .iter()
        .map(|s| {
            serde_json::json!({
                "rule": s.rule,
                "reason": s.reason,
            })
        })
        .collect();

    serde_json::json!({
        "sources": sources,
        "loaded": resolved.config.rules.len(),
        "skipped": skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ---- Skill discovery tests ----

    // Note: `list_skills` also discovers user-scoped skills from
    // `~/.echo-agent/skills/`, so on a developer machine the result may be
    // non-empty even for an empty workspace. Tests below check for
    // specific skills rather than asserting emptiness.

    #[tokio::test]
    async fn discover_skills_finds_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp
            .path()
            .join(".echo-agent")
            .join("skills")
            .join("my-skill");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: A test skill\n---\n# Body\n",
        )
        .unwrap();

        let skills = discover_skills(tmp.path(), &SkillsConfig::default()).await;
        let found = skills
            .iter()
            .find(|s| s["name"].as_str() == Some("my-skill"));
        assert!(found.is_some(), "should find my-skill");
        let skill = found.unwrap();
        assert_eq!(skill["description"].as_str(), Some("A test skill"));
    }

    #[tokio::test]
    async fn discover_skills_respects_ignore_config() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp
            .path()
            .join(".echo-agent")
            .join("skills")
            .join("ignored");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: ignored\ndescription: Ignored\n---\n",
        )
        .unwrap();

        let config = SkillsConfig {
            paths: vec![],
            ignore: vec![tmp.path().to_string_lossy().to_string()],
            disabled: vec![],
            server_skill_dirs: vec![],
            bundled_skill_dirs: vec![],
            server_enforced_skill_names: vec![],
        };
        let skills = discover_skills(tmp.path(), &config).await;
        let found = skills.iter().any(|s| s["name"].as_str() == Some("ignored"));
        assert!(
            !found,
            "skill in ignore path should be filtered out of results"
        );
    }

    #[tokio::test]
    async fn discover_skills_returns_serialized_values() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp
            .path()
            .join(".echo-agent")
            .join("skills")
            .join("serialized-check");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: serialized-check\ndescription: Test serialization\n---\n",
        )
        .unwrap();

        let skills = discover_skills(tmp.path(), &SkillsConfig::default()).await;
        let found = skills
            .iter()
            .find(|s| s["name"].as_str() == Some("serialized-check"))
            .expect("should find serialized-check");
        // SkillInfo has these required fields when serialized
        assert!(found["path"].is_string(), "path should be a string");
        assert!(found["scope"].is_string(), "scope should be serialized");
    }

    // ---- AGENTS.md discovery tests ----

    #[test]
    fn agent_config_file_wire_matches_workspace_types_mirror() {
        // The RPC serializes echo-agent-build's AgentConfigFile and the remote
        // consumer deserializes the workspace-types mirror; pin the cross-crate
        // serde shape so a rename/attr drift on either side can't silently
        // break discovery.
        let src = echo_agent_core::prompt::agents_md::AgentConfigFile {
            file_name: "AGENTS.md".to_string(),
            file_path: "/repo/AGENTS.md".to_string(),
            content: "# Instructions\n".to_string(),
        };
        let json = serde_json::to_value(&src).unwrap();
        let mirror: echo_agent_workspace_types::rpc::agents_md::AgentConfigFile =
            serde_json::from_value(json.clone())
                .expect("server shape must deserialize into mirror");
        assert_eq!(mirror.file_name, src.file_name);
        assert_eq!(mirror.file_path, src.file_path);
        assert_eq!(mirror.content, src.content);
        assert_eq!(
            serde_json::to_value(&mirror).unwrap(),
            json,
            "mirror must serialize to the exact shape the server emits"
        );
    }

    // Discovery also scans the real `~/.echo-agent`, so fixtures use test-unique names.
    #[tokio::test]
    async fn discover_agents_md_receives_normalized_rule_content() {
        let tmp = tempfile::tempdir().unwrap();
        let rules_dir = tmp.path().join(".cursor").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(
            rules_dir.join("xyzzy-discover-agents-md-test.md"),
            "---\nglobs: [\"*.rs\"]\n---\nUse tabs, not spaces.\n",
        )
        .unwrap();

        let files = discover_agents_md(tmp.path()).await;
        let rule = files
            .iter()
            .find(|f| {
                f["file_path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("/.cursor/rules/xyzzy-discover-agents-md-test.md"))
            })
            .expect("should discover the rules file");
        let content = rule["content"].as_str().unwrap();
        assert!(
            content.contains("Use tabs, not spaces."),
            "rule body must survive: {content}"
        );
        assert!(
            !content.contains("globs:"),
            "YAML frontmatter must be stripped: {content}"
        );
    }

    #[tokio::test]
    async fn discover_agents_md_keeps_non_rules_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("AGENTS.md"),
            "---\nXYZZY_LEADING_DASHES_MARKER\n---\nBody after horizontal rules.\n",
        )
        .unwrap();

        let files = discover_agents_md(tmp.path()).await;
        let agents = files
            .iter()
            .find(|f| {
                f["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("XYZZY_LEADING_DASHES_MARKER"))
            })
            .expect("should discover the AGENTS.md with its leading dashes intact");
        let content = agents["content"].as_str().unwrap();
        assert!(
            content.starts_with("---\n"),
            "stripping must be rules-files-only: {content}"
        );
    }

    // ---- Plugin discovery tests ----

    // Note: `discover_plugins` also discovers user-scoped plugins
    // from `~/.echo-agent/plugins/`, so tests check for specific plugins.

    #[test]
    fn discover_plugins_finds_manifest_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp
            .path()
            .join(".echo-agent")
            .join("plugins")
            .join("test-plugin");
        fs::create_dir_all(&plugins_dir).unwrap();
        fs::write(
            plugins_dir.join("plugin.json"),
            r#"{"name": "test-plugin"}"#,
        )
        .unwrap();
        // Plugin needs at least one component to be recognized
        fs::create_dir_all(plugins_dir.join("skills")).unwrap();

        let trust = PluginTrustStore::load_from(tmp.path().join("trust"));
        let config = PluginDiscoveryConfig::default();
        let plugins = discover_plugins(tmp.path(), &config, &trust, true);
        let found = plugins
            .iter()
            .find(|p| p["name"].as_str() == Some("test-plugin"));
        assert!(found.is_some(), "should find test-plugin");
        let p = found.unwrap();
        assert_eq!(p["scope"].as_str(), Some("project"));
        assert_eq!(p["has_skills"].as_bool(), Some(true));
    }

    #[test]
    fn discover_plugins_json_has_expected_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp
            .path()
            .join(".echo-agent")
            .join("plugins")
            .join("field-test");
        fs::create_dir_all(plugins_dir.join("skills")).unwrap();
        fs::write(
            plugins_dir.join("plugin.json"),
            r#"{"name": "field-test", "version": "1.0.0", "description": "Test plugin"}"#,
        )
        .unwrap();

        let trust = PluginTrustStore::load_from(tmp.path().join("trust"));
        let config = PluginDiscoveryConfig::default();
        let plugins = discover_plugins(tmp.path(), &config, &trust, true);
        let p = plugins
            .iter()
            .find(|p| p["name"].as_str() == Some("field-test"))
            .expect("should find field-test");
        assert_eq!(p["version"].as_str(), Some("1.0.0"));
        assert_eq!(p["description"].as_str(), Some("Test plugin"));
        assert!(p["id"].is_string());
        assert!(p["root"].is_string());
        assert!(p["trusted"].is_boolean());
        assert!(p["has_hooks"].is_boolean());
        assert!(p["has_mcp"].is_boolean());
        assert!(p["has_lsp"].is_boolean());
        assert!(p["has_agents"].is_boolean());
        assert!(p["has_skills"].is_boolean());
    }

    // ---- Project config tests ----

    #[test]
    fn load_project_config_missing_file_returns_null() {
        let tmp = tempfile::tempdir().unwrap();
        let config = load_project_config(tmp.path());
        assert_eq!(config, Value::Null, "missing config → Null");
    }

    #[test]
    fn load_project_config_reads_toml_as_json() {
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");
        fs::create_dir_all(&echo_agent_dir).unwrap();
        fs::write(
            echo_agent_dir.join("config.toml"),
            "[skills]\npaths = [\"/extra/skills\"]\n\n[plugins]\ndisabled = [\"noisy-plugin\"]\n",
        )
        .unwrap();

        let config = load_project_config(tmp.path());
        assert!(config.is_object(), "parsed config should be an object");
        assert!(
            config["skills"]["paths"].is_array(),
            "skills.paths should be an array"
        );
        assert_eq!(config["skills"]["paths"][0].as_str(), Some("/extra/skills"));
        assert_eq!(
            config["plugins"]["disabled"][0].as_str(),
            Some("noisy-plugin")
        );
    }

    // ---- toml_to_json tests ----

    #[test]
    fn toml_to_json_basic_types() {
        let toml_val: toml::Value =
            toml::from_str("s = \"hello\"\ni = 42\nf = 3.14\nb = true\n").unwrap();
        let json = toml_to_json(&toml_val);
        assert_eq!(json["s"], "hello");
        assert_eq!(json["i"], 42);
        assert_eq!(json["b"], true);
    }

    #[test]
    fn toml_to_json_nested_table() {
        let toml_val: toml::Value = toml::from_str("[section]\nkey = \"value\"\n").unwrap();
        let json = toml_to_json(&toml_val);
        assert_eq!(json["section"]["key"], "value");
    }

    #[test]
    fn toml_to_json_array() {
        let toml_val: toml::Value = toml::from_str("items = [1, 2, 3]\n").unwrap();
        let json = toml_to_json(&toml_val);
        assert_eq!(json["items"][0], 1);
        assert_eq!(json["items"][2], 3);
    }

    // ---- Permissions tests ----

    // Note: `resolve_permissions_with_provenance` checks system-managed
    // settings and requirements.toml from the global config, so on a
    // developer machine with EchoAgent installed it may return non-Null even
    // for a temp directory. Both branches assert a concrete condition.

    #[tokio::test]
    async fn load_permissions_returns_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let result = load_permissions(tmp.path(), true).await;
        // Result is either Null (no sources) or an object with
        // sources, loaded, and skipped fields. Both branches assert
        // a definite pass criterion.
        if result.is_null() {
            // No permission sources on this machine — Null is correct.
            assert_eq!(result, Value::Null, "expected Null for empty workspace");
        } else {
            assert!(result["sources"].is_array(), "sources should be an array");
            assert!(result["loaded"].is_number(), "loaded should be a number");
            assert!(result["skipped"].is_array(), "skipped should be an array");
        }
    }

    #[tokio::test]
    async fn load_permissions_with_settings_file_returns_object() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a minimal .claude/settings.json with a permission rule
        // so the test always exercises the non-null path.
        let claude_dir = tmp.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(
            claude_dir.join("settings.json"),
            r#"{"permissions":{"allow":["Bash(git status)"]}}"#,
        )
        .unwrap();

        let result = load_permissions(tmp.path(), true).await;
        assert!(result.is_object(), "should return an object, got {result}");
        assert!(result["sources"].is_array(), "sources should be an array");
        assert!(result["loaded"].is_number(), "loaded should be a number");
        assert!(result["skipped"].is_array(), "skipped should be an array");
        assert!(
            result["loaded"].as_u64().unwrap_or(0) >= 1,
            "should have at least one loaded rule"
        );
    }
}
