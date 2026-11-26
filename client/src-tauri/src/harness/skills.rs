//! Skills panel — drives EchoAgent's `echo.agent/skills/*` extension methods.
//!
//! EchoAgent discovers skills by recursively scanning `~/.echo-agent/skills/`,
//! `<cwd>/.echo-agent/skills/`, and a few bundled/plugin dirs (see
//! `echo-agent-tools/src/implementations/skills/discovery.rs`). Each skill is a
//! directory containing a `SKILL.md` with YAML frontmatter. EchoAgent exposes the
//! full CRUD surface over ACP — we call those methods here rather than reading
//! the filesystem ourselves, because EchoAgent holds the canonical enabled/disabled
//! state in `~/.echo-agent/config.toml` (`[skills] disabled`, `[skills] paths`) and
//! reloads on file changes.

#[cfg(test)]
use agent_client_protocol as acp;
use echo_agent_tools::implementations::skills::capability::{
    ConnectorState, SkillCapabilityReport,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tauri::State;

use crate::ext::{call_ext, raw_params};
use crate::harness::state::HarnessState;

/// Disk transactions are successful only after each resident actor has applied
/// the change. With no initialized runtime, the next session reads it from disk.
pub(crate) async fn refresh_live_skills(state: &HarnessState) -> Result<(), String> {
    let tx = state.tx.lock().unwrap().clone();
    if let Some(tx) = tx {
        refresh_skills_with_tx(&tx).await?;
    }
    Ok(())
}

async fn refresh_skills_with_tx(tx: &echo_agent_acp::AcpAgentTx) -> Result<(), String> {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        call_ext::<serde_json::Value>(
            tx,
            "echo.agent/internal/reload_skills",
            raw_params(&serde_json::json!({})),
        ),
    )
    .await
    .map_err(|_| "技能配置已保存，但现有会话刷新超时；请重试刷新".to_string())?;
    response.map_err(|error| format!("技能配置已保存，但现有会话刷新失败；请重试刷新：{error}"))?;
    Ok(())
}

fn process_cwd() -> String {
    std::env::current_dir()
        .ok()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .to_string_lossy()
        .into_owned()
}

fn required_cwd(cwd: Option<String>) -> String {
    cwd.filter(|value| !value.trim().is_empty())
        .unwrap_or_else(process_cwd)
}

fn configured_source_for_skill(path: &str, configured_paths: &[String]) -> Option<String> {
    configured_paths
        .iter()
        .filter(|configured| Path::new(path).starts_with(Path::new(configured)))
        .max_by_key(|configured| Path::new(configured).components().count())
        .cloned()
}

fn connector_state_from_host(
    enabled: bool,
    auth_required: bool,
    setup_required: bool,
    status: Option<&str>,
) -> ConnectorState {
    if !enabled || auth_required || setup_required {
        return ConnectorState::ConfigurationRequired;
    }
    // Agent-level catalog entries intentionally have no live session status.
    // They are installed/configured and may be started by the next session.
    // An explicit non-ready live status must still fail closed.
    match status {
        None | Some("ready") => ConnectorState::Ready,
        Some(_) => ConnectorState::ConfigurationRequired,
    }
}

fn insert_connector_state(
    states: &mut HashMap<String, ConnectorState>,
    name: &str,
    state: ConnectorState,
) {
    let normalized = name.to_ascii_lowercase();
    let mut insert = |key: &str| {
        states
            .entry(key.to_owned())
            .and_modify(|current| {
                // A requirement is satisfiable when any provider for the same
                // connector ID is ready.
                if state == ConnectorState::Ready {
                    *current = ConnectorState::Ready;
                }
            })
            .or_insert(state);
    };
    insert(&normalized);
    if let Some(connector_id) = normalized.strip_prefix("managed_gateway:") {
        insert(connector_id);
    }
}

fn command_cwd(
    state: &HarnessState,
    access: &crate::shell_fs::FilesystemAccess,
    cwd: Option<String>,
) -> Result<String, String> {
    if let Some(cwd) = cwd.filter(|value| !value.trim().is_empty()) {
        return access
            .require_workspace(&cwd)
            .map(|path| path.to_string_lossy().into_owned());
    }
    Ok(required_cwd(
        state
            .cwd
            .lock()
            .unwrap()
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    ))
}

/// One discovered skill. Mirrors the relevant fields of EchoAgent's `SkillInfo`
/// (`echo-agent-tools/src/implementations/skills/types.rs:40`). Unknown/missing
/// fields fall back to defaults — the shape is stable across EchoAgent versions but
/// we stay defensive.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    // Runtime SkillInfo itself uses snake_case even though the extension
    // response wrapper uses camelCase. Accept both wire generations.
    #[serde(default, alias = "display_name")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Where the skill was discovered: "local" | "repo" | "user" | "server"
    /// | "bundled" | "plugin". See `SkillScope`.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, alias = "user_invocable")]
    pub user_invocable: Option<bool>,
    /// Filesystem path to the skill directory (when available).
    #[serde(default)]
    pub path: Option<String>,
    /// True for packages copied into and owned by EchoAgent's local installer.
    #[serde(default)]
    pub managed: bool,
    /// True for a signed package synchronized from echo-agent-server. Kept
    /// separate from `managed`, which means the local package installer owns
    /// the directory and may safely uninstall it.
    #[serde(default)]
    pub org_managed: bool,
    #[serde(default)]
    pub org_skill_id: Option<String>,
    #[serde(default)]
    pub org_version_id: Option<String>,
    #[serde(default)]
    pub org_scope_kind: Option<String>,
    #[serde(default)]
    pub org_mandatory: Option<bool>,
    #[serde(default)]
    pub org_allow_personal_override: Option<bool>,
    /// Installed package version when declared by its install manifest.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
    #[serde(default, alias = "when_to_use")]
    pub when_to_use: Option<String>,
    /// The exact `[skills].paths` entry that owns this skill, when any.
    /// Removing a discovered SKILL.md path directly would not remove its parent
    /// registration, so the UI must use this field.
    #[serde(default)]
    pub configured_path: Option<String>,
    /// Machine-readable runtime/dependency/connector readiness from the
    /// optional echo.skill.json package contract.
    #[serde(default)]
    pub capability: Option<SkillCapabilityReport>,
}

/// Generic list shape returned by `echo.agent/skills/list` and `echo.agent/skills/config`:
/// EchoAgent returns either a bare array or `{ skills: [...] }`. We accept both.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SkillsListResponse {
    Array(Vec<SkillInfo>),
    Wrapped {
        skills: Vec<SkillInfo>,
        #[serde(default)]
        paths: Vec<String>,
    },
}

impl SkillsListResponse {
    fn into_parts(self) -> (Vec<SkillInfo>, Vec<String>) {
        match self {
            SkillsListResponse::Array(v) => (v, Vec::new()),
            SkillsListResponse::Wrapped { skills, paths } => (skills, paths),
        }
    }
}

/// List all skills EchoAgent has discovered. `cwd` is optional (used by EchoAgent to
/// resolve project-scoped skills).
#[tauri::command]
pub async fn skills_list(
    state: State<'_, HarnessState>,
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    cwd: Option<String>,
) -> Result<Vec<SkillInfo>, String> {
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let cwd = command_cwd(&state, &access, cwd)?;
    let skills = skills_list_with_tx(&tx, Some(cwd)).await?;
    for skill in &skills {
        if let Some(path) = skill.path.as_deref() {
            if let Err(error) = access.record_trusted_package_source(Path::new(path)) {
                tracing::warn!(%error, %path, "ignored invalid Runtime Skill package source");
            }
        }
        if let Some(path) = skill.configured_path.as_deref() {
            if let Err(error) = access.record_configured_skill_source(path) {
                tracing::warn!(%error, %path, "ignored invalid configured Skill source");
            }
        }
    }
    Ok(skills)
}

pub(crate) async fn skills_config_with_tx(
    tx: &echo_agent_acp::AcpAgentTx,
    cwd: Option<String>,
) -> Result<(Vec<SkillInfo>, Vec<String>), String> {
    // Current EchoAgent builds require `cwd` and reject both null and a missing
    // field. Older builds also accept the explicit string, so always provide a
    // concrete fallback for calls made outside an active project/session.
    let params: Arc<RawValue> = raw_params(&serde_json::json!({
        "cwd": required_cwd(cwd),
    }));
    // Prefer `echo.agent/skills/config` (richer: includes paths/ignore config), but
    // fall back to `echo.agent/skills/list` if the method is unavailable on this
    // EchoAgent build.
    let res: Result<SkillsListResponse, _> =
        call_ext(tx, "echo.agent/skills/config", params.clone()).await;
    Ok(match res {
        Ok(v) => v.into_parts(),
        Err(_) => {
            let v: SkillsListResponse = call_ext(tx, "echo.agent/skills/list", params)
                .await
                .map_err(|e| e.to_string())?;
            v.into_parts()
        }
    })
}

/// Internal form used by automations for execution-time capability checks.
pub async fn skills_list_with_tx(
    tx: &echo_agent_acp::AcpAgentTx,
    cwd: Option<String>,
) -> Result<Vec<SkillInfo>, String> {
    let (skills, configured_paths) = skills_config_with_tx(tx, cwd).await?;
    let organization_metadata = crate::org::managed_skills_metadata();
    // Connector readiness belongs to the host product because credentials stay
    // inside connector implementations. A Skill receives status, never secrets.
    let connector_states = crate::mcp::mcp_list_with_tx(tx, None)
        .await
        .ok()
        .map(|servers| {
            let mut states = HashMap::new();
            for server in servers {
                let state = connector_state_from_host(
                    server.enabled,
                    server.auth_required,
                    server.setup_required,
                    server.status.as_deref(),
                );
                insert_connector_state(&mut states, &server.name, state);
            }
            states
        });
    Ok(skills
        .into_iter()
        .map(|mut skill| {
            if let Some(path) = skill.path.clone() {
                if let Some((_, meta)) = organization_metadata
                    .iter()
                    .find(|(root, _)| Path::new(&path).starts_with(root))
                {
                    skill.org_managed = true;
                    skill.org_skill_id = Some(meta.skill_id.clone());
                    skill.org_version_id = Some(meta.version_id.clone());
                    skill.org_scope_kind = Some(meta.scope_kind.clone());
                    skill.org_mandatory = Some(meta.mandatory);
                    skill.org_allow_personal_override = Some(meta.allow_personal_override);
                    skill.version = Some(meta.version.clone());
                }
                skill.managed = crate::skill_installer::is_managed_skill(&path);
                if skill.managed {
                    skill.version = crate::skill_installer::managed_skill_version(&path);
                    skill.path = crate::skill_installer::managed_skill_directory(&path);
                } else {
                    skill.configured_path = configured_source_for_skill(&path, &configured_paths);
                }
                skill.capability = Some(crate::skill_installer::inspect_installed_capability(
                    &path,
                    connector_states.as_ref(),
                ));
            }
            skill
        })
        .collect())
}

/// Add a skill path (directory or file) to `[skills].paths` and rescan.
#[tauri::command]
pub async fn skills_add(
    state: State<'_, HarnessState>,
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    path: String,
    cwd: Option<String>,
) -> Result<(), String> {
    crate::harness::policy::require_feature("skills")?;
    crate::harness::policy::require_skill_upload()?;
    let path = access
        .require_authorized_package_source(Path::new(&path))?
        .to_string_lossy()
        .into_owned();
    crate::skill_installer::validate_registered_source(&path)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let cwd = command_cwd(&state, &access, cwd)?;
    let params = raw_params(&serde_json::json!({ "path": path, "cwd": cwd }));
    let _: serde_json::Value = call_ext(&tx, "echo.agent/skills/add", params)
        .await
        .map_err(|e| e.to_string())?;
    access.record_configured_skill_source(&path)?;
    refresh_live_skills(&state).await
}

/// Remove a skill path from `[skills].paths`.
#[tauri::command]
pub async fn skills_remove(
    state: State<'_, HarnessState>,
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    path: String,
    cwd: Option<String>,
) -> Result<(), String> {
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let path = access.require_configured_skill_source(&path)?;
    let cwd = command_cwd(&state, &access, cwd)?;
    let params = raw_params(&serde_json::json!({ "path": path, "cwd": cwd }));
    let _: serde_json::Value = call_ext(&tx, "echo.agent/skills/remove", params)
        .await
        .map_err(|e| e.to_string())?;
    refresh_live_skills(&state).await
}

/// Enable or disable a skill by name (writes `[skills] disabled`).
#[tauri::command]
pub async fn skills_toggle(
    state: State<'_, HarnessState>,
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    name: String,
    enabled: bool,
    cwd: Option<String>,
) -> Result<(), String> {
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let cwd = command_cwd(&state, &access, cwd)?;
    let params = raw_params(&serde_json::json!({ "name": name, "enabled": enabled, "cwd": cwd }));
    let _: serde_json::Value = call_ext(&tx, "echo.agent/skills/toggle", params)
        .await
        .map_err(|e| e.to_string())?;
    refresh_live_skills(&state).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_snake_case_skill_metadata_is_preserved() {
        let response: SkillsListResponse = serde_json::from_value(serde_json::json!({
            "paths": ["/tmp/skills"],
            "skills": [{
                "name": "deploy",
                "display_name": "Deploy Service",
                "description": "Deploy safely",
                "scope": "user",
                "enabled": true,
                "user_invocable": false,
                "path": "/tmp/skills/deploy/SKILL.md",
                "author": "team",
                "license": "Apache-2.0",
                "compatibility": "Requires git",
                "when_to_use": "release requests"
            }]
        }))
        .unwrap();
        let (skills, paths) = response.into_parts();
        assert_eq!(paths, vec!["/tmp/skills"]);
        assert_eq!(skills[0].display_name.as_deref(), Some("Deploy Service"));
        assert_eq!(skills[0].user_invocable, Some(false));
        assert_eq!(skills[0].when_to_use.as_deref(), Some("release requests"));
        assert!(skills[0].capability.is_none());
    }

    #[test]
    fn required_cwd_never_returns_an_empty_value() {
        assert_eq!(required_cwd(Some("/tmp/project".into())), "/tmp/project");
        assert!(!required_cwd(None).trim().is_empty());
        assert!(!required_cwd(Some("  ".into())).trim().is_empty());
    }

    #[test]
    fn configured_source_matches_path_components_not_neighbor_prefixes() {
        let paths = vec!["/tmp/skills/review".into()];
        assert_eq!(
            configured_source_for_skill("/tmp/skills/review/SKILL.md", &paths),
            Some("/tmp/skills/review".into())
        );
        assert_eq!(
            configured_source_for_skill("/tmp/skills/reviewer/SKILL.md", &paths),
            None
        );
        let paths = vec!["/tmp/skills".into(), "/tmp/skills/review".into()];
        assert_eq!(
            configured_source_for_skill("/tmp/skills/review/SKILL.md", &paths),
            Some("/tmp/skills/review".into())
        );
    }

    #[test]
    fn connector_preflight_distinguishes_configured_and_unavailable() {
        assert_eq!(
            connector_state_from_host(true, false, false, None),
            ConnectorState::Ready
        );
        assert_eq!(
            connector_state_from_host(true, false, false, Some("ready")),
            ConnectorState::Ready
        );
        for state in [
            connector_state_from_host(false, false, false, None),
            connector_state_from_host(true, true, false, Some("ready")),
            connector_state_from_host(true, false, true, Some("setuprequired")),
            connector_state_from_host(true, false, false, Some("initializing")),
            connector_state_from_host(true, false, false, Some("unavailable")),
        ] {
            assert_eq!(state, ConnectorState::ConfigurationRequired);
        }
    }

    #[test]
    fn managed_gateway_connectors_are_addressable_by_manifest_id() {
        let mut states = HashMap::new();
        insert_connector_state(&mut states, "managed_gateway:notion", ConnectorState::Ready);
        assert_eq!(
            states.get("managed_gateway:notion"),
            Some(&ConnectorState::Ready)
        );
        assert_eq!(states.get("notion"), Some(&ConnectorState::Ready));
    }

    #[test]
    fn enveloped_runtime_skill_list_decodes() {
        let response = acp::ExtResponse::new(crate::ext::raw_params(&serde_json::json!({
            "result": {
                "skills": [{
                    "name": "demo",
                    "description": "Demo skill",
                    "path": "/tmp/skills/demo/SKILL.md",
                    "scope": "user",
                    "enabled": true
                }]
            }
        })));

        let parsed: SkillsListResponse = crate::ext::parse_ext_response(&response).unwrap();
        let (skills, paths) = parsed.into_parts();
        assert_eq!(skills[0].name, "demo");
        assert!(skills[0].enabled);
        assert!(paths.is_empty());
    }
}

#[cfg(test)]
#[path = "skills_runtime_test.rs"]
mod runtime_tests;
