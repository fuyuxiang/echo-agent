use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

use crate::util::config as cli_config;
use echo_agent_core::prompt::skills::{
    CompatConfig, SkillInfo, SkillsConfig, list_skills_with_plugins,
};

use super::ExtResult;

/// Generic params for methods that only need an optional `cwd`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CwdParams {
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsAddRequest {
    /// Path to add (directory or SKILL.md file). Supports `~` expansion.
    pub path: String,
    /// Working directory for skill discovery context.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsAddResponse {
    /// Number of skills discovered at the added path.
    pub added_count: usize,
    /// Total number of skills loaded across all sources.
    pub total: usize,
    /// The path that was added to config.
    pub path: String,
    /// Full updated skill list after reload.
    pub skills: Vec<SkillInfo>,
    /// Human-readable message.
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsRemoveRequest {
    /// Path to remove from config paths.
    pub path: String,
    /// Working directory for skill discovery context.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsRemoveResponse {
    /// The path that was removed.
    pub path: String,
    /// Full updated skill list after reload.
    pub skills: Vec<SkillInfo>,
    /// Human-readable message.
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsResetResponse {
    /// Full updated skill list after reload.
    pub skills: Vec<SkillInfo>,
    /// Human-readable message.
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillsToggleRequest {
    /// Skill name to toggle.
    pub name: String,
    /// Whether to enable (`true`) or disable (`false`) the skill.
    pub enabled: bool,
    /// Working directory for skill discovery context.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListRequest {
    /// Working directory for skill discovery context.
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowsListRequest {
    session_id: acp::SessionId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListResponse {
    /// All discovered skills.
    pub skills: Vec<SkillInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsConfigResponse {
    /// Configured paths from `[skills].paths`.
    pub paths: Vec<String>,
    /// Ignored paths from `[skills].ignore`.
    pub ignore: Vec<String>,
    /// Total loaded skill count.
    pub total_skills: usize,
    /// Human-readable summary.
    pub message: String,
    /// Full updated skill list.
    pub skills: Vec<SkillInfo>,
}

/// Reload skills using the current config for the given working directory.
#[tracing::instrument(skip_all, fields(cwd))]
async fn reload_skills(
    cwd: &str,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: CompatConfig,
) -> Vec<SkillInfo> {
    let config = cli_config::load_config().await.skills;
    let discovery = list_skills_with_plugins(Some(cwd), &config, plugin_registry, compat);
    match tokio::time::timeout(std::time::Duration::from_secs(5), discovery).await {
        Ok(skills) => skills,
        Err(_) => {
            tracing::warn!("Skills reload timed out");
            vec![]
        }
    }
}

/// Count how many skills have paths starting with the given prefix.
fn count_skills_from(skills: &[SkillInfo], dir: &std::path::Path) -> usize {
    let prefix = dir.to_str().unwrap_or("");
    skills.iter().filter(|s| s.path.starts_with(prefix)).count()
}

/// Resolve a skill path to an absolute path.
///
/// Handles `~` expansion and relative path resolution against `cwd`.
/// Falls back to the original string if canonicalization fails.
fn resolve_skill_path(raw: &str, cwd: &str) -> String {
    use std::path::PathBuf;

    // Expand ~ to $HOME
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        echo_agent_config::discovery_home_dir()
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else if raw == "~" {
        echo_agent_config::discovery_home_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    };

    // If already absolute, canonicalize to resolve `..` etc.
    // If relative, join with cwd first.
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        PathBuf::from(cwd).join(&expanded)
    };

    // canonicalize resolves symlinks and `..` — fall back to the joined path if it fails
    // (e.g. path doesn't exist yet)
    dunce::canonicalize(&absolute)
        .unwrap_or(absolute)
        .to_string_lossy()
        .to_string()
}

/// Collect auto-discovered skill source directories and their counts.
fn discover_auto_sources(cwd: &str, skills: &[SkillInfo]) -> Vec<(String, usize)> {
    let cwd_path = std::path::PathBuf::from(cwd);
    let echo_agent_home = echo_agent_tools::util::echo_agent_home::echo_agent_home();
    let git_root = git2::Repository::discover(&cwd_path)
        .ok()
        .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()));

    // Once the user has imported, stop scanning hardcoded
    // .claude/skills/ paths. Equivalent locations should be opted in via
    // [paths] extra_skill_dirs in config.toml (written by /import-claude).
    let imported = crate::claude_import::is_claude_import_marked();
    let local_dir_names: &[&str] = if imported {
        &[".echo-agent", ".agents"]
    } else {
        &[".echo-agent", ".agents", ".claude"]
    };

    let mut sources: Vec<(String, usize)> = Vec::new();
    let subdirs = ["skills", "commands"];

    let mut try_add_source = |dir: std::path::PathBuf, seen: Option<&[std::path::PathBuf]>| {
        if dir.is_dir() && !seen.is_some_and(|s| s.contains(&dir)) {
            let count = count_skills_from(skills, &dir);
            if count > 0 {
                sources.push((dir.to_string_lossy().to_string(), count));
            }
        }
    };

    let mut local_dirs: Vec<std::path::PathBuf> = Vec::new();
    for dir_name in local_dir_names {
        for subdir in &subdirs {
            let dir = cwd_path.join(dir_name).join(subdir);
            try_add_source(dir.clone(), None);
            local_dirs.push(dir);
        }
    }

    if let Some(ref root) = git_root {
        for dir_name in local_dir_names {
            for subdir in &subdirs {
                try_add_source(root.join(dir_name).join(subdir), Some(&local_dirs));
            }
        }
    }

    for subdir in &subdirs {
        try_add_source(echo_agent_home.join(subdir), None);
    }

    let home = echo_agent_config::discovery_home_dir();
    if let Some(ref h) = home {
        let home_path = std::path::PathBuf::from(h);
        for subdir in &subdirs {
            try_add_source(home_path.join(".agents").join(subdir), None);
        }
        if !imported {
            for subdir in &subdirs {
                try_add_source(home_path.join(".claude").join(subdir), None);
            }
        }
    }

    // [paths] extra_skill_dirs from config.toml. These supplement the built-in
    // scan locations. Used both standalone and as the migration target after
    // /import-claude when the runtime .claude/skills/ scan is disabled.
    for dir in extra_skill_dirs_from_config() {
        let path = crate::util::expand_home(&dir);
        if path.is_dir()
            && !sources
                .iter()
                .any(|(s, _)| s.as_str() == path.to_string_lossy().as_ref())
        {
            sources.push((
                path.to_string_lossy().to_string(),
                count_skills_from(skills, &path),
            ));
        }
    }

    sources
}

/// Read `[paths] extra_skill_dirs` from the effective config. Returns empty
/// on any read/parse failure so misconfiguration never breaks listing.
fn extra_skill_dirs_from_config() -> Vec<String> {
    let Ok(root) = crate::config::load_effective_config() else {
        return Vec::new();
    };
    root.get("paths")
        .and_then(|v| v.get("extra_skill_dirs"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(
    agent: &crate::agent::mvp_agent::MvpAgent,
    args: &acp::ExtRequest,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: CompatConfig,
) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/skills/add" => {
            let req: SkillsAddRequest = serde_json::from_str(args.params.get())?;
            let cwd = req.cwd.as_deref().unwrap_or(".");

            // Resolve to absolute path so config entries work from any cwd.
            let resolved = resolve_skill_path(&req.path, cwd);

            let p = resolved.clone();
            if let Err(e) = cli_config::update_config(|cfg| {
                cfg.skills.ignore.retain(|i| {
                    !(i == &p || p.starts_with(i.as_str()) || i.starts_with(p.as_str()))
                });
                if !cfg.skills.paths.contains(&p) {
                    cfg.skills.paths.push(p);
                }
            })
            .await
            {
                echo_agent_telemetry::session_ctx::log_event(
                    echo_agent_telemetry::events::SkillAdded {
                        added_count: 0,
                        total_skills: 0,
                        success: false,
                    },
                );
                return super::to_ext_response(Err::<SkillsAddResponse, _>(anyhow::anyhow!(
                    "Failed to save config: {e}"
                )));
            }

            let skills = reload_skills(cwd, plugin_registry, compat).await;
            let added_count = skills
                .iter()
                .filter(|s| s.path.starts_with(&resolved))
                .count();
            let total = skills.len();
            let message = format!(
                "Added path {}. {} new skill{} found ({} total).",
                resolved,
                added_count,
                if added_count == 1 { "" } else { "s" },
                total,
            );

            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::SkillAdded {
                    added_count: added_count as u32,
                    total_skills: total as u32,
                    success: true,
                },
            );
            super::to_ext_response(Ok(SkillsAddResponse {
                added_count,
                total,
                path: resolved,
                skills,
                message,
            }))
        }

        "echo.agent/skills/remove" => {
            let req: SkillsRemoveRequest = serde_json::from_str(args.params.get())?;
            let cwd = req.cwd.as_deref().unwrap_or(".");

            // Resolve so relative/tilde paths match what was saved by add.
            let resolved = resolve_skill_path(&req.path, cwd);

            let p = resolved.clone();
            if let Err(e) = cli_config::update_config(|cfg| {
                cfg.skills.paths.retain(|i| i != &p);
            })
            .await
            {
                echo_agent_telemetry::session_ctx::log_event(
                    echo_agent_telemetry::events::SkillRemoved { success: false },
                );
                return super::to_ext_response(Err::<SkillsRemoveResponse, _>(anyhow::anyhow!(
                    "Failed to save config: {e}"
                )));
            }

            let skills = reload_skills(cwd, plugin_registry, compat).await;
            let total = skills.len();
            let message = format!(
                "Removed path {}. {} skill{} remaining.",
                resolved,
                total,
                if total == 1 { "" } else { "s" },
            );

            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::SkillRemoved { success: true },
            );
            super::to_ext_response(Ok(SkillsRemoveResponse {
                path: resolved,
                skills,
                message,
            }))
        }

        "echo.agent/skills/reset" => {
            let params: CwdParams =
                serde_json::from_str(args.params.get()).unwrap_or(CwdParams { cwd: None });
            let cwd = params.cwd.as_deref().unwrap_or(".");

            if let Err(e) = cli_config::update_config(|cfg| {
                cfg.skills = SkillsConfig::default();
            })
            .await
            {
                return super::to_ext_response(Err::<SkillsResetResponse, _>(anyhow::anyhow!(
                    "Failed to save config: {e}"
                )));
            }

            let skills = reload_skills(cwd, plugin_registry, compat).await;
            let message = "Custom skills config reset".to_string();

            super::to_ext_response(Ok(SkillsResetResponse { skills, message }))
        }

        "echo.agent/skills/list" => {
            let req: SkillsListRequest = serde_json::from_str(args.params.get())?;
            let skills = reload_skills(&req.cwd, plugin_registry, compat).await;
            super::to_ext_response(Ok(SkillsListResponse { skills }))
        }

        "echo.agent/workflows/list" => {
            let req: WorkflowsListRequest = serde_json::from_str(args.params.get())?;
            let Some(handle) = agent.session_handle_waiting_for_load(&req.session_id).await else {
                return super::to_ext_response(Err::<serde_json::Value, _>(anyhow::anyhow!(
                    "unknown session id: {}",
                    req.session_id.0
                )));
            };
            let (launches_enabled, _management_available) = handle.workflow_catalog_state().await;
            let workflows = if launches_enabled {
                crate::session::workflow::registry::list_workflows(Some(
                    handle.tool_context.cwd.as_path(),
                ))
            } else {
                Vec::new()
            };
            super::to_ext_response(Ok(serde_json::json!({ "workflows": workflows })))
        }

        "echo.agent/skills/config" => {
            let params: CwdParams =
                serde_json::from_str(args.params.get()).unwrap_or(CwdParams { cwd: None });
            let cwd = params.cwd.as_deref().unwrap_or(".");

            let config = cli_config::load_config().await.skills;
            let paths = config.paths.clone();
            let ignore = config.ignore.clone();

            let skills = reload_skills(cwd, plugin_registry, compat).await;
            let total_skills = skills.len();

            let auto_sources = discover_auto_sources(cwd, &skills);

            let mut msg = String::new();

            msg.push_str("Skill discovery sources:\n");
            for (source, count) in &auto_sources {
                msg.push_str(&format!(
                    "  • {}  ({} skill{})\n",
                    source,
                    count,
                    if *count == 1 { "" } else { "s" }
                ));
            }
            if auto_sources.is_empty() {
                msg.push_str("  (no auto-discovered directories found)\n");
            }

            if !paths.is_empty() {
                msg.push_str("\nCustom paths:\n");
                for p in &paths {
                    let count = skills
                        .iter()
                        .filter(|s| s.path.starts_with(p.as_str()))
                        .count();
                    msg.push_str(&format!(
                        "  • {}  ({} skill{})\n",
                        p,
                        count,
                        if count == 1 { "" } else { "s" }
                    ));
                }
            }

            if !ignore.is_empty() {
                msg.push_str("\nIgnored:\n");
                for p in &ignore {
                    msg.push_str(&format!("  • {}\n", p));
                }
            }

            msg.push_str(&format!("\nTotal skills loaded: {}", total_skills));

            super::to_ext_response(Ok(SkillsConfigResponse {
                paths,
                ignore,
                total_skills,
                message: msg,
                skills,
            }))
        }

        "echo.agent/skills/toggle" => {
            let req: SkillsToggleRequest = serde_json::from_str(args.params.get())?;
            let cwd = req.cwd.as_deref().unwrap_or(".");

            // Validate the skill name exists before modifying config.
            let current_skills = reload_skills(cwd, plugin_registry, compat).await;
            if !current_skills.iter().any(|s| s.name == req.name) {
                return super::to_ext_response(Err::<SkillsListResponse, _>(anyhow::anyhow!(
                    "Skill '{}' not found",
                    req.name
                )));
            }

            let name = req.name.clone();
            let enabled = req.enabled;
            if let Err(e) = cli_config::update_config(|cfg| {
                if enabled {
                    cfg.skills.disabled.retain(|d| d != &name);
                } else if !cfg.skills.disabled.contains(&name) {
                    cfg.skills.disabled.push(name.clone());
                }
            })
            .await
            {
                return super::to_ext_response(Err::<SkillsListResponse, _>(anyhow::anyhow!(
                    "Failed to save config: {e}"
                )));
            }

            // Re-apply disabled marking against the already-loaded skills
            // to reflect the config change without a second full discovery.
            let config = cli_config::load_config().await.skills;
            let disabled_set: std::collections::HashSet<&str> =
                config.disabled.iter().map(|s| s.as_str()).collect();
            let skills: Vec<SkillInfo> = current_skills
                .into_iter()
                .map(|mut s| {
                    s.enabled = !disabled_set.contains(s.name.as_str());
                    s
                })
                .collect();
            super::to_ext_response(Ok(SkillsListResponse { skills }))
        }

        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_request_with_cwd() {
        let json = r#"{"path": "/home/user/skills", "cwd": "/project"}"#;
        let req: SkillsAddRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.path, "/home/user/skills");
        assert_eq!(req.cwd, Some("/project".to_string()));
    }

    #[test]
    fn test_add_request_without_cwd() {
        let json = r#"{"path": "~/my-skills"}"#;
        let req: SkillsAddRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.path, "~/my-skills");
        assert_eq!(req.cwd, None);
    }

    #[test]
    fn test_remove_request() {
        let json = r#"{"path": "/home/user/skills", "cwd": "/project"}"#;
        let req: SkillsRemoveRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.path, "/home/user/skills");
        assert_eq!(req.cwd, Some("/project".to_string()));
    }

    #[test]
    fn test_list_request() {
        let json = r#"{"cwd": "/project"}"#;
        let req: SkillsListRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.cwd, "/project");
    }

    #[test]
    fn test_add_response_camel_case() {
        let resp = SkillsAddResponse {
            added_count: 3,
            total: 10,
            path: "/test".to_string(),
            skills: vec![],
            message: "ok".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["addedCount"], 3);
        assert_eq!(json["total"], 10);
        assert_eq!(json["path"], "/test");
    }

    #[test]
    fn test_resolve_absolute_path_unchanged() {
        let resolved = resolve_skill_path("/absolute/path/to/skills", "/some/cwd");
        // Canonicalize will fail (path doesn't exist), so we get the joined absolute path
        assert_eq!(resolved, "/absolute/path/to/skills");
    }

    #[test]
    fn test_resolve_relative_path_against_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();

        let resolved = resolve_skill_path("sub", &tmp.path().to_string_lossy());
        assert_eq!(
            resolved,
            dunce::canonicalize(&sub).unwrap().to_string_lossy()
        );
    }

    #[test]
    fn test_resolve_dotdot_path() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&cwd).unwrap();

        let resolved = resolve_skill_path("../..", &cwd.to_string_lossy());
        assert_eq!(
            resolved,
            dunce::canonicalize(tmp.path()).unwrap().to_string_lossy()
        );
    }

    /// Hermetic tilde expansion: pin HOME to a temp dir so remote sandboxes
    /// (missing HOME, symlink-resolved homes, pre-existing ~/my-skills) cannot
    /// make `starts_with($HOME)` fail spuriously. Serial because env mutation
    /// is process-global.
    #[test]
    #[serial_test::serial]
    fn test_resolve_tilde_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        // SAFETY: serial test; restored in the same scope below.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("USERPROFILE");
        }
        let resolved = resolve_skill_path("~/my-skills", "/ignored");
        match prev_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        match prev_userprofile {
            Some(v) => unsafe { std::env::set_var("USERPROFILE", v) },
            None => unsafe { std::env::remove_var("USERPROFILE") },
        }
        let expected = home.join("my-skills");
        assert_eq!(
            std::path::PathBuf::from(&resolved),
            expected,
            "resolved={resolved}"
        );
    }

    #[test]
    fn test_config_response_camel_case() {
        let resp = SkillsConfigResponse {
            paths: vec!["/a".into()],
            ignore: vec![],
            total_skills: 5,
            message: "ok".to_string(),
            skills: vec![],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["totalSkills"], 5);
        assert!(json["paths"].is_array());
    }
}
