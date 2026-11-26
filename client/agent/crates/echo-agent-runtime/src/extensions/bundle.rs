//! ACP extension handlers for bundled subagent cache sync and status.
//!
//! These endpoints operate on the on-disk bundled cache only. Sync updates the
//! cache for future agent construction / future conversations; it does not live
//! reload the currently running `MvpAgent` instance.
use super::{ExtResult, parse_params, to_ext_response};
use crate::agent::MvpAgent;
use crate::bundle::{self, BundleManifest};
use crate::remote::{FetchedBundle, fetch_bundle};
use agent_client_protocol as acp;
use anyhow::Context;
use echo_agent_tools::implementations::skills::discovery::extract_first_paragraph;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
/// Default freshness window for the proactive bundle sync. Bypassed by `force`.
pub(crate) const BUNDLE_SYNC_TTL: Duration = Duration::from_secs(60 * 60);
/// Error message returned when no auth source is available for a bundle sync.
///
/// Hoisted to a constant so the user-facing wording stays in lockstep
/// across `sync_bundle`, `sync_bundle_to_root`, and any future call sites.
pub(crate) const NO_BUNDLE_CREDENTIALS_ERROR: &str =
    "bundle sync requires either an authenticated cli-chat-proxy session or a deployment key";
/// Whether the caller has any source of authentication that the
/// cli-chat-proxy `/v1/subagents/bundle` endpoint will accept.
///
/// Centralised so the auth gate predicate stays consistent across:
/// - `sync_bundle` (user-triggered ACP entrypoint)
/// - `sync_bundle_to_root` (defense-in-depth on the public function)
/// - `maybe_sync_bundle_to_root` (proactive wrapper, silent skip on miss)
/// - `MvpAgent::maybe_sync_bundle_in_background` (post-auth pre-spawn gate)
///
/// All four call sites previously inlined the same predicate; a future
/// auth-source addition (e.g., service-account token) only needs to land
/// here.
#[inline]
pub(crate) fn has_bundle_credentials(
    auth_manager: Option<&std::sync::Arc<crate::auth::AuthManager>>,
    deployment_key: Option<&str>,
) -> bool {
    auth_manager
        .as_ref()
        .is_some_and(|am| am.current_or_expired().is_some())
        || deployment_key.is_some()
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleSyncRequest {
    #[serde(default)]
    force: bool,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleSyncResult {
    pub updated: bool,
    pub version: String,
    pub personas_count: usize,
    pub roles_count: usize,
    pub agents_count: usize,
    pub skills_count: usize,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleStatusRequest {}
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BundleStatusResult {
    pub has_cache: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub personas: Vec<String>,
    pub roles: Vec<String>,
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub persona_details: Vec<PersonaDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub role_details: Vec<RoleDetail>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersonaDetail {
    pub name: String,
    pub description: Option<String>,
    pub has_inputs: bool,
    pub has_outputs: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RoleDetail {
    pub name: String,
    pub description: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryGetRequest {
    kind: String,
    name: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryGetResult {
    pub kind: String,
    pub name: String,
    pub content: String,
}
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/bundle/sync" => {
            let req: BundleSyncRequest = parse_params(args)?;
            to_ext_response(sync_bundle(agent, req).await)
        }
        "echo.agent/bundle/status" => {
            let _req: BundleStatusRequest = parse_params(args)?;
            to_ext_response(status_bundle())
        }
        "echo.agent/bundle/entry/get" => {
            let req: EntryGetRequest = parse_params(args)?;
            to_ext_response(get_entry(&req.kind, &req.name))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}
async fn sync_bundle(agent: &MvpAgent, req: BundleSyncRequest) -> anyhow::Result<BundleSyncResult> {
    let deployment_key = agent.deployment_key();
    if !has_bundle_credentials(Some(&agent.auth_manager), deployment_key.as_deref()) {
        anyhow::bail!(NO_BUNDLE_CREDENTIALS_ERROR);
    }
    sync_bundle_to_root(
        &bundle::bundled_root(),
        &agent.cli_chat_proxy_base_url(),
        Some(&agent.auth_manager),
        deployment_key.as_deref(),
        agent.alpha_test_key().as_deref(),
        req.force,
    )
    .await
}
/// `true` when `<root>/manifest.json` exists, was written within `ttl`, and
/// is parseable as a [`BundleManifest`].
///
/// The parse check guards against the silent-skip failure mode where the
/// mtime is recent (e.g., a partial/aborted write) but the manifest is
/// truncated or otherwise corrupt. A bare mtime check would let
/// `maybe_sync_bundle_to_root` proactively skip a re-sync, leaving callers
/// (`status_bundle_at`, `SubagentsConfig::resolve`) to fail later with an
/// empty or stale catalog. Treating an unparseable manifest as "not fresh"
/// forces a re-sync on the next post-auth event.
pub(crate) fn bundle_cache_is_fresh(root: &Path, ttl: Duration) -> bool {
    let manifest = root.join("manifest.json");
    let Ok(meta) = std::fs::metadata(&manifest) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let within_ttl = modified
        .elapsed()
        .map(|elapsed| elapsed < ttl)
        .unwrap_or(false);
    if !within_ttl {
        return false;
    }
    matches!(bundle::read_cached_manifest(root), Ok(Some(_)))
}
/// Proactive variant of [`sync_bundle_to_root`] that respects an auth gate
/// and a TTL guard.
///
/// Returns:
/// - `Ok(Some(result))` when a sync was performed.
/// - `Ok(None)` when the call was skipped (no credentials or cache fresh).
/// - `Err(_)` when sync was attempted but the network call or extract failed.
pub(crate) async fn maybe_sync_bundle_to_root(
    root: &Path,
    proxy_base_url: &str,
    auth_manager: Option<&std::sync::Arc<crate::auth::AuthManager>>,
    deployment_key: Option<&str>,
    alpha_test_key: Option<&str>,
    force: bool,
    ttl: Duration,
) -> anyhow::Result<Option<BundleSyncResult>> {
    if !has_bundle_credentials(auth_manager, deployment_key) {
        tracing::debug!("proactive bundle sync skipped: no auth and no deployment key");
        return Ok(None);
    }
    if !force && bundle_cache_is_fresh(root, ttl) {
        tracing::debug!(
            ttl_secs = ttl.as_secs(),
            "proactive bundle sync skipped: cache is fresh"
        );
        return Ok(None);
    }
    sync_bundle_to_root(
        root,
        proxy_base_url,
        auth_manager,
        deployment_key,
        alpha_test_key,
        force,
    )
    .await
    .map(Some)
}
pub(crate) async fn sync_bundle_to_root(
    root: &Path,
    proxy_base_url: &str,
    auth_manager: Option<&std::sync::Arc<crate::auth::AuthManager>>,
    deployment_key: Option<&str>,
    alpha_test_key: Option<&str>,
    _force: bool,
) -> anyhow::Result<BundleSyncResult> {
    if !has_bundle_credentials(auth_manager, deployment_key) {
        anyhow::bail!(NO_BUNDLE_CREDENTIALS_ERROR);
    }
    let fetched =
        fetch_bundle(proxy_base_url, auth_manager, deployment_key, alpha_test_key).await?;
    match fetched {
        FetchedBundle::Archive(bytes) => {
            let root_owned = root.to_path_buf();
            let manifest = tokio::task::spawn_blocking(move || {
                bundle::extract_bundle_archive(&root_owned, &bytes)
            })
            .await
            .context("bundle extract task panicked")??;
            let personas_count = bundle::count_entries_by_prefix(&manifest, "personas/");
            let roles_count = bundle::count_entries_by_prefix(&manifest, "roles/");
            let agents_count = bundle::count_entries_by_prefix(&manifest, "agents/");
            let skills_count = bundle::count_entries_by_prefix(&manifest, "skills/");
            Ok(BundleSyncResult {
                updated: true,
                version: manifest.version,
                personas_count,
                roles_count,
                agents_count,
                skills_count,
            })
        }
        FetchedBundle::Legacy(legacy_bundle) => {
            let version = legacy_bundle.version.clone();
            let personas_count = legacy_bundle.personas.len();
            let roles_count = legacy_bundle.roles.len();
            let agents_count = legacy_bundle.agents.len();
            let skills_count = legacy_bundle.skills.len();
            let root_owned = root.to_path_buf();
            tokio::task::spawn_blocking(move || {
                bundle::write_bundle_to_cache(&root_owned, &legacy_bundle)
            })
            .await
            .context("bundle write task panicked")??;
            Ok(BundleSyncResult {
                updated: true,
                version,
                personas_count,
                roles_count,
                agents_count,
                skills_count,
            })
        }
    }
}
fn get_entry(kind: &str, name: &str) -> anyhow::Result<EntryGetResult> {
    get_entry_at(&bundle::bundled_root(), kind, name)
}
fn validate_entry_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name == "."
    {
        anyhow::bail!("invalid entry name: {name}");
    }
    Ok(())
}
fn get_entry_at(root: &Path, kind: &str, name: &str) -> anyhow::Result<EntryGetResult> {
    validate_entry_name(name)?;
    let (dir_name, ext) = match kind {
        "persona" => ("personas", "toml"),
        "role" => ("roles", "toml"),
        "agent" => ("agents", "md"),
        _ => anyhow::bail!("unknown entry kind: {kind}"),
    };
    let path = root.join(dir_name).join(format!("{name}.{ext}"));
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("{kind} '{name}' not found in bundle cache"))?;
    Ok(EntryGetResult {
        kind: kind.to_owned(),
        name: name.to_owned(),
        content,
    })
}
fn status_bundle() -> anyhow::Result<BundleStatusResult> {
    status_bundle_at(&bundle::bundled_root())
}
fn status_bundle_at(root: &Path) -> anyhow::Result<BundleStatusResult> {
    let Some(manifest) = bundle::read_cached_manifest(root)? else {
        return Ok(BundleStatusResult {
            has_cache: false,
            version: None,
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        });
    };
    let personas = list_cached_entries(root, &manifest, "personas", "toml");
    let roles = list_cached_entries(root, &manifest, "roles", "toml");
    let agents = list_cached_entries(root, &manifest, "agents", "md");
    let skills = list_cached_skill_entries(root, &manifest);
    let persona_details = personas
        .iter()
        .filter_map(|name| persona_detail_from_toml(name, root))
        .collect();
    let role_details = roles
        .iter()
        .filter_map(|name| role_detail_from_toml(name, root))
        .collect();
    Ok(BundleStatusResult {
        has_cache: true,
        version: Some(manifest.version.clone()),
        personas,
        roles,
        agents,
        skills,
        persona_details,
        role_details,
    })
}
fn persona_detail_from_toml(name: &str, root: &Path) -> Option<PersonaDetail> {
    let path = root.join("personas").join(format!("{name}.toml"));
    let content = std::fs::read_to_string(&path).ok()?;
    let table: toml::Value = toml::from_str(&content).ok()?;
    let desc = table
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            table
                .get("instructions")
                .and_then(|v| v.as_str())
                .and_then(extract_first_paragraph)
        });
    let has_inputs = table
        .get("inputs")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let has_outputs = table
        .get("outputs")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    Some(PersonaDetail {
        name: name.to_owned(),
        description: desc,
        has_inputs,
        has_outputs,
    })
}
fn role_detail_from_toml(name: &str, root: &Path) -> Option<RoleDetail> {
    let path = root.join("roles").join(format!("{name}.toml"));
    let content = std::fs::read_to_string(&path).ok()?;
    let table: toml::Value = toml::from_str(&content).ok()?;
    let desc = table
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    Some(RoleDetail {
        name: name.to_owned(),
        description: desc,
    })
}
fn list_cached_entries(
    root: &Path,
    manifest: &BundleManifest,
    dir_name: &str,
    extension: &str,
) -> Vec<String> {
    let dir = root.join(dir_name);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let file_name = path.file_name()?.to_str()?;
            let relative_path = format!("{dir_name}/{file_name}");
            if !manifest.checksums.contains_key(&relative_path) {
                return None;
            }
            match path.extension().and_then(|ext| ext.to_str()) {
                Some(ext) if ext == extension => path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(ToOwned::to_owned),
                _ => None,
            }
        })
        .collect();
    names.sort();
    names
}
fn list_cached_skill_entries(root: &Path, manifest: &BundleManifest) -> Vec<String> {
    let prefix = "skills/";
    let mut names: Vec<String> = manifest
        .checksums
        .keys()
        .filter_map(|k| {
            let name = k.strip_prefix(prefix)?.strip_suffix("/SKILL.md")?;
            root.join(k).is_file().then(|| name.to_owned())
        })
        .collect();
    names.sort();
    names
}
#[allow(clippy::disallowed_methods)]
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use echo_agent_chat_proxy_types::SubagentBundle;
    use serial_test::serial;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    fn sample_bundle() -> SubagentBundle {
        let mut bundle = SubagentBundle::empty("bundle-v1");
        bundle.personas.insert(
            "researcher".to_string(),
            concat!(
                "instructions = \"You are a thorough researcher.\\nDig deep.\"\n",
                "[[inputs]]\nname = \"topic\"\n",
                "[[outputs]]\nname = \"report\"\n",
            )
            .to_string(),
        );
        bundle.roles.insert(
            "reviewer".to_string(),
            "description = \"Meticulous code reviewer\"\n".to_string(),
        );
        bundle
            .agents
            .insert("default".to_string(), "# agent\n".to_string());
        bundle
    }
    fn sample_bundle_with_skills() -> SubagentBundle {
        let mut bundle = sample_bundle();
        bundle
            .skills
            .insert("commit".to_string(), "# Commit skill\n".to_string());
        bundle
            .skills
            .insert("review".to_string(), "# Review skill\n".to_string());
        bundle
    }
    fn test_auth() -> crate::auth::EchoAgentAuth {
        crate::auth::EchoAgentAuth {
            key: "token".to_string(),
            auth_mode: crate::auth::AuthMode::Oidc,
            create_time: chrono::Utc::now(),
            user_id: "user-1".to_string(),
            email: Some("test@example.com".to_string()),
            first_name: None,
            last_name: None,
            profile_image_asset_id: None,
            principal_type: None,
            principal_id: None,
            team_id: None,
            team_name: None,
            team_role: None,
            organization_id: None,
            organization_name: None,
            organization_role: None,
            user_blocked_reason: None,
            team_blocked_reasons: vec![],
            coding_data_retention_opt_out: false,
            has_echo_agent_code_access: None,
            refresh_token: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            oidc_issuer: None,
            oidc_client_id: None,
        }
    }
    fn test_auth_manager() -> Arc<crate::auth::AuthManager> {
        let dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::auth::AuthManager::new(dir.path(), crate::auth::EchoAgentComConfig::default());
        mgr.hot_swap(test_auth());
        std::mem::forget(dir);
        Arc::new(mgr)
    }
    #[derive(Debug, Default, Clone)]
    struct SeenHeaders {
        authorization: Option<String>,
        token_auth: Option<String>,
        user_id: Option<String>,
        email: Option<String>,
        alpha_test_key: Option<String>,
    }
    #[derive(Clone)]
    struct BundleServerState {
        body: serde_json::Value,
        status_code: StatusCode,
        seen_headers: Arc<Mutex<Vec<SeenHeaders>>>,
    }
    async fn start_bundle_server(
        status_code: StatusCode,
        body: serde_json::Value,
    ) -> (
        String,
        Arc<Mutex<Vec<SeenHeaders>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let seen_headers = Arc::new(Mutex::new(Vec::new()));
        let state = BundleServerState {
            body,
            status_code,
            seen_headers: seen_headers.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let app = Router::new()
            .route(
                "/v1/subagents/bundle",
                get(
                    |State(state): State<BundleServerState>, headers: HeaderMap| async move {
                        state.seen_headers.lock().unwrap().push(SeenHeaders {
                            authorization: headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_owned),
                            token_auth: headers
                                .get("x-echo-agent-token-auth")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_owned),
                            user_id: headers
                                .get("x-userid")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_owned),
                            email: headers
                                .get("x-email")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_owned),
                            alpha_test_key: {
                                let _ = &headers;
                                None
                            },
                        });
                        (state.status_code, axum::Json(state.body))
                    },
                ),
            )
            .with_state(state);
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("{base}/v1"), seen_headers, handle)
    }
    #[test]
    #[serial]
    fn status_reports_no_cache_when_manifest_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let status = status_bundle_at(&root).unwrap();
        assert_eq!(
            status,
            BundleStatusResult {
                has_cache: false,
                version: None,
                personas: vec![],
                roles: vec![],
                agents: vec![],
                skills: vec![],
                persona_details: vec![],
                role_details: vec![],
            }
        );
    }
    #[test]
    #[serial]
    fn status_reports_cached_entries_from_manifest_and_disk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        std::fs::write(
            root.join("personas/local-only.toml"),
            "instructions = \"ignore\"",
        )
        .unwrap();
        let status = status_bundle_at(&root).unwrap();
        assert!(status.has_cache);
        assert_eq!(status.version.as_deref(), Some("bundle-v1"));
        assert_eq!(status.personas, vec!["researcher"]);
        assert_eq!(status.roles, vec!["reviewer"]);
        assert_eq!(status.agents, vec!["default"]);
        assert_eq!(status.skills, Vec::<String>::new());
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_success_writes_cache_and_returns_counts() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let bundle = sample_bundle();
        let (proxy_base_url, _seen_headers, server) = start_bundle_server(
            axum::http::StatusCode::OK,
            serde_json::to_value(&bundle).unwrap(),
        )
        .await;
        let am = test_auth_manager();
        let result = sync_bundle_to_root(&root, &proxy_base_url, Some(&am), None, None, false)
            .await
            .unwrap();
        assert_eq!(result.version, "bundle-v1");
        assert_eq!(result.personas_count, 1);
        assert_eq!(result.roles_count, 1);
        assert_eq!(result.agents_count, 1);
        assert_eq!(result.skills_count, 0);
        assert!(root.join("personas/researcher.toml").exists());
        assert!(root.join("roles/reviewer.toml").exists());
        assert!(root.join("agents/default.md").exists());
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_force_true_has_same_write_semantics() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let bundle = sample_bundle();
        let (proxy_base_url, _seen_headers, server) =
            start_bundle_server(StatusCode::OK, serde_json::to_value(&bundle).unwrap()).await;
        let am = test_auth_manager();
        let normal = sync_bundle_to_root(&root, &proxy_base_url, Some(&am), None, None, false)
            .await
            .unwrap();
        let forced = sync_bundle_to_root(&root, &proxy_base_url, Some(&am), None, None, true)
            .await
            .unwrap();
        assert_eq!(forced, normal);
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_http_failure_surfaces_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let (proxy_base_url, _seen_headers, server) = start_bundle_server(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error": "unauthorized"}),
        )
        .await;
        let am = test_auth_manager();
        let error = sync_bundle_to_root(&root, &proxy_base_url, Some(&am), None, None, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("401"));
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_uses_deployment_key_auth_mode() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let bundle = sample_bundle();
        let (proxy_base_url, seen_headers, server) =
            start_bundle_server(StatusCode::OK, serde_json::to_value(&bundle).unwrap()).await;
        let am = test_auth_manager();
        let result = sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&am),
            Some("deploy-key"),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(result.version, "bundle-v1");
        let headers = seen_headers.lock().unwrap();
        let headers = headers.last().unwrap();
        assert_eq!(headers.authorization.as_deref(), Some("Bearer deploy-key"));
        assert_eq!(headers.token_auth, None);
        assert_eq!(headers.user_id, None);
        assert_eq!(headers.email, None);
        assert_eq!(headers.alpha_test_key, None);
        server.abort();
    }
    #[test]
    #[serial]
    fn status_only_reports_bundled_cache_not_higher_priority_sources() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        let project_root = tmp.path().join("workspace");
        std::fs::create_dir_all(project_root.join(".echo-agent/personas")).unwrap();
        std::fs::create_dir_all(project_root.join(".echo-agent/roles")).unwrap();
        std::fs::write(
            project_root.join(".echo-agent/personas/researcher.toml"),
            "instructions = \"project persona\"\n",
        )
        .unwrap();
        std::fs::write(
            project_root.join(".echo-agent/roles/reviewer.toml"),
            "description = \"project role\"\n",
        )
        .unwrap();
        let base = crate::config::SubagentsConfig::resolve_base_with_sources(
            false,
            &toml::Value::Table(Default::default()),
            None,
            &root,
        );
        let (roles, personas) = crate::config::SubagentsConfig::effective_definition_maps(
            &base.roles,
            &base.personas,
            &project_root,
            true,
        );
        assert_eq!(
            personas
                .get("researcher")
                .and_then(|persona| persona.instructions.as_deref()),
            Some("project persona")
        );
        assert_eq!(
            roles.get("reviewer").map(|role| role.description.as_str()),
            Some("project role")
        );
        let status = status_bundle_at(&root).unwrap();
        assert_eq!(status.personas, vec!["researcher"]);
        assert_eq!(status.roles, vec!["reviewer"]);
        assert_eq!(status.agents, vec!["default"]);
        assert_eq!(status.skills, Vec::<String>::new());
    }
    #[test]
    #[serial]
    fn sync_requires_auth_or_deployment_key() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let error = futures::executor::block_on(sync_bundle_to_root(
            &root,
            "http://127.0.0.1:1/v1",
            None,
            None,
            None,
            false,
        ))
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("bundle sync requires either an authenticated cli-chat-proxy session or a deployment key"));
    }
    #[test]
    #[serial]
    fn get_entry_reads_persona_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        let result = get_entry_at(&root, "persona", "researcher").unwrap();
        assert_eq!(result.kind, "persona");
        assert_eq!(result.name, "researcher");
        assert!(result.content.contains("instructions"));
    }
    #[test]
    #[serial]
    fn get_entry_unknown_kind_returns_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let err = get_entry_at(&root, "widget", "foo").unwrap_err();
        assert!(err.to_string().contains("unknown entry kind: widget"));
    }
    #[test]
    #[serial]
    fn get_entry_missing_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        let err = get_entry_at(&root, "persona", "nonexistent").unwrap_err();
        assert!(err.to_string().contains("not found in bundle cache"));
    }
    #[test]
    fn get_entry_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        for bad_name in ["../../../etc/passwd", "foo/bar", "a\\b", "..", "."] {
            let err = get_entry_at(&root, "persona", bad_name).unwrap_err();
            assert!(
                err.to_string().contains("invalid entry name"),
                "expected rejection for {bad_name:?}, got: {err}"
            );
        }
    }
    #[test]
    fn get_entry_rejects_empty_name() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let err = get_entry_at(&root, "persona", "").unwrap_err();
        assert!(err.to_string().contains("invalid entry name"));
    }
    #[test]
    #[serial]
    fn status_includes_persona_and_role_details() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        let status = status_bundle_at(&root).unwrap();
        assert_eq!(status.persona_details.len(), 1);
        let pd = &status.persona_details[0];
        assert_eq!(pd.name, "researcher");
        assert_eq!(
            pd.description.as_deref(),
            Some("You are a thorough researcher. Dig deep.")
        );
        assert!(pd.has_inputs);
        assert!(pd.has_outputs);
        assert_eq!(status.role_details.len(), 1);
        let rd = &status.role_details[0];
        assert_eq!(rd.name, "reviewer");
        assert_eq!(rd.description, "Meticulous code reviewer");
    }
    #[test]
    #[serial]
    fn status_without_toml_files_returns_empty_details() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let mut bundle = SubagentBundle::empty("v1");
        bundle
            .personas
            .insert("ghost".to_string(), "instructions = \"x\"".to_string());
        bundle::write_bundle_to_cache(&root, &bundle).unwrap();
        std::fs::remove_file(root.join("personas/ghost.toml")).unwrap();
        let status = status_bundle_at(&root).unwrap();
        assert!(status.personas.is_empty());
        assert!(status.persona_details.is_empty());
    }
    #[test]
    fn malformed_toml_skipped_gracefully() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("cache");
        std::fs::create_dir_all(root.join("personas")).unwrap();
        std::fs::write(root.join("personas/bad.toml"), "{{{{not toml").unwrap();
        assert!(persona_detail_from_toml("bad", &root).is_none());
    }
    #[test]
    fn persona_detail_without_inputs_outputs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("personas")).unwrap();
        std::fs::write(
            root.join("personas/simple.toml"),
            "instructions = \"Just a simple persona\"",
        )
        .unwrap();
        let detail = persona_detail_from_toml("simple", root).unwrap();
        assert_eq!(detail.name, "simple");
        assert_eq!(detail.description.as_deref(), Some("Just a simple persona"));
        assert!(!detail.has_inputs);
        assert!(!detail.has_outputs);
    }
    #[test]
    fn role_detail_missing_description_defaults_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("roles")).unwrap();
        std::fs::write(root.join("roles/bare.toml"), "instructions = \"hi\"").unwrap();
        let detail = role_detail_from_toml("bare", root).unwrap();
        assert_eq!(detail.name, "bare");
        assert_eq!(detail.description, "");
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_with_skills_reports_skills_count() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let bundle = sample_bundle_with_skills();
        let (proxy_base_url, _seen_headers, server) =
            start_bundle_server(StatusCode::OK, serde_json::to_value(&bundle).unwrap()).await;
        let result = sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&test_auth_manager()),
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(result.skills_count, 2);
        assert_eq!(result.personas_count, 1);
        assert_eq!(result.roles_count, 1);
        assert_eq!(result.agents_count, 1);
        server.abort();
    }
    #[test]
    #[serial]
    fn status_lists_skill_names_from_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle_with_skills()).unwrap();
        let status = status_bundle_at(&root).unwrap();
        assert!(status.has_cache);
        assert_eq!(status.skills, vec!["commit", "review"]);
        assert_eq!(status.personas, vec!["researcher"]);
    }
    #[test]
    #[serial]
    fn status_skills_only_lists_files_present_on_disk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle_with_skills()).unwrap();
        std::fs::remove_file(root.join("skills/commit/SKILL.md")).unwrap();
        let status = status_bundle_at(&root).unwrap();
        assert_eq!(status.skills, vec!["review"]);
    }
    use crate::bundle::test_helpers::make_test_archive;
    #[derive(Clone)]
    struct ArchiveServerState {
        archive_bytes: Vec<u8>,
    }
    async fn start_archive_bundle_server(
        archive_bytes: Vec<u8>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let state = ArchiveServerState { archive_bytes };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let app = Router::new()
            .route(
                "/v1/bundle/archive",
                get(|State(state): State<ArchiveServerState>| async move {
                    (StatusCode::OK, state.archive_bytes)
                }),
            )
            .with_state(state);
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("{base}/v1"), handle)
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_with_archive_endpoint_extracts_and_reports_counts() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let archive = make_test_archive(&[
            ("bundle.json", br#"{"version":"archive-v1"}"#),
            (
                "subagents/personas/researcher.toml",
                b"instructions = \"hello\"",
            ),
            ("subagents/roles/reviewer.toml", b"description = \"review\""),
            ("skills/commit/SKILL.md", b"# Commit skill"),
        ]);
        let (proxy_base_url, server) = start_archive_bundle_server(archive).await;
        let result = sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&test_auth_manager()),
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(result.version, "archive-v1");
        assert_eq!(result.personas_count, 1);
        assert_eq!(result.roles_count, 1);
        assert_eq!(result.agents_count, 0);
        assert_eq!(result.skills_count, 1);
        assert!(root.join("personas/researcher.toml").exists());
        assert!(root.join("skills/commit/SKILL.md").exists());
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_falls_back_to_legacy_when_archive_unavailable() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let bundle = sample_bundle_with_skills();
        let (proxy_base_url, _seen_headers, server) =
            start_bundle_server(StatusCode::OK, serde_json::to_value(&bundle).unwrap()).await;
        let result = sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&test_auth_manager()),
            None,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(result.version, "bundle-v1");
        assert_eq!(result.personas_count, 1);
        assert_eq!(result.skills_count, 2);
        server.abort();
    }
    fn backdate_manifest(root: &std::path::Path, age: Duration) {
        let path = root.join("manifest.json");
        let stale_time = std::time::SystemTime::now() - age;
        let times = std::fs::FileTimes::new().set_modified(stale_time);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open manifest for backdate")
            .set_times(times)
            .expect("set manifest mtime");
    }
    #[tokio::test(flavor = "current_thread")]
    async fn maybe_sync_skips_without_auth_or_deployment_key() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let (proxy_base_url, seen_headers, server) = start_bundle_server(
            StatusCode::OK,
            serde_json::to_value(sample_bundle()).unwrap(),
        )
        .await;
        let result = maybe_sync_bundle_to_root(
            &root,
            &proxy_base_url,
            None,
            None,
            None,
            false,
            BUNDLE_SYNC_TTL,
        )
        .await
        .unwrap();
        assert!(result.is_none(), "expected sync skipped");
        assert!(
            seen_headers.lock().unwrap().is_empty(),
            "auth-gated sync must not hit the network"
        );
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn maybe_sync_skips_when_cache_is_fresh() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        let (proxy_base_url, seen_headers, server) = start_bundle_server(
            StatusCode::OK,
            serde_json::to_value(sample_bundle()).unwrap(),
        )
        .await;
        let result = maybe_sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&test_auth_manager()),
            None,
            None,
            false,
            BUNDLE_SYNC_TTL,
        )
        .await
        .unwrap();
        assert!(result.is_none(), "fresh cache should skip sync");
        assert!(
            seen_headers.lock().unwrap().is_empty(),
            "fresh cache must not hit the network"
        );
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn maybe_sync_runs_when_cache_is_stale() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        backdate_manifest(&root, BUNDLE_SYNC_TTL + Duration::from_secs(60));
        let (proxy_base_url, seen_headers, server) = start_bundle_server(
            StatusCode::OK,
            serde_json::to_value(sample_bundle()).unwrap(),
        )
        .await;
        let outcome = maybe_sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&test_auth_manager()),
            None,
            None,
            false,
            BUNDLE_SYNC_TTL,
        )
        .await
        .unwrap()
        .expect("stale cache should trigger a sync");
        assert_eq!(outcome.version, "bundle-v1");
        assert_eq!(seen_headers.lock().unwrap().len(), 1);
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn maybe_sync_force_bypasses_ttl() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        let (proxy_base_url, seen_headers, server) = start_bundle_server(
            StatusCode::OK,
            serde_json::to_value(sample_bundle()).unwrap(),
        )
        .await;
        let outcome = maybe_sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&test_auth_manager()),
            None,
            None,
            true,
            BUNDLE_SYNC_TTL,
        )
        .await
        .unwrap()
        .expect("force=true should always sync");
        assert_eq!(outcome.version, "bundle-v1");
        assert_eq!(seen_headers.lock().unwrap().len(), 1);
        server.abort();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn maybe_sync_runs_when_no_manifest_exists() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        let (proxy_base_url, seen_headers, server) = start_bundle_server(
            StatusCode::OK,
            serde_json::to_value(sample_bundle()).unwrap(),
        )
        .await;
        let outcome = maybe_sync_bundle_to_root(
            &root,
            &proxy_base_url,
            Some(&test_auth_manager()),
            None,
            None,
            false,
            BUNDLE_SYNC_TTL,
        )
        .await
        .unwrap()
        .expect("missing manifest should trigger a sync");
        assert_eq!(outcome.personas_count, 1);
        assert_eq!(seen_headers.lock().unwrap().len(), 1);
        server.abort();
    }
    #[test]
    fn bundle_cache_is_fresh_returns_false_without_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        assert!(!bundle_cache_is_fresh(&root, BUNDLE_SYNC_TTL));
    }
    #[test]
    fn bundle_cache_is_fresh_true_for_recent_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        assert!(bundle_cache_is_fresh(&root, BUNDLE_SYNC_TTL));
    }
    #[test]
    fn bundle_cache_is_fresh_false_for_stale_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        backdate_manifest(&root, BUNDLE_SYNC_TTL + Duration::from_secs(60));
        assert!(!bundle_cache_is_fresh(&root, BUNDLE_SYNC_TTL));
    }
    #[test]
    fn bundle_cache_is_fresh_false_for_corrupted_manifest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundled");
        bundle::write_bundle_to_cache(&root, &sample_bundle()).unwrap();
        std::fs::write(root.join("manifest.json"), "{not json}").unwrap();
        assert!(!bundle_cache_is_fresh(&root, BUNDLE_SYNC_TTL));
    }
}
