use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::auth::{EchoAgentAuth, read_auth_json};

use super::watcher::ConfigChangeEvent;

/// Typed, `Send`-safe messages for the agent to apply inside its `LocalSet`.
#[derive(Debug)]
pub enum ConfigUpdate {
    /// New auth credentials from disk.
    Auth(Box<EchoAgentAuth>),
    /// Auth scope was removed (user logged out).
    AuthCleared,
    /// A **broadcast** MCP reload — applies to every active session
    /// regardless of cwd. Fires for two cases:
    ///
    /// 1. The global `[mcp_servers]` table in `~/.echo-agent/config.toml`
    ///    changed.
    /// 2. The user's home-level `~/.claude.json` changed.
    ///    `load_claude_json_mcp_servers_as_configs` reads this file
    ///    for every session, so the reload cannot be narrowed by cwd.
    ///
    /// Project-scoped changes (`<cwd>/.echo-agent/config.toml`,
    /// `<cwd>/.mcp.json`, project-level `<cwd>/.claude.json`) emit
    /// [`Self::ProjectMcpServersChanged`] instead so the reload can
    /// be narrowed to matching cwds.
    ///
    /// Deliberately kept as a unit variant.
    /// Adding a payload here would force pattern-match updates across
    /// (`<cwd>/.echo-agent/config.toml`, `<cwd>/.mcp.json`, or
    /// `mvp_agent`, `app`, `session/handle`, etc.
    McpServersChanged,
    /// A **project-scoped** MCP config file changed
    /// `<cwd>/.claude.json`). Agent should reload MCP only for
    /// sessions whose cwd matches `cwd` (or sits beneath it).
    ///
    /// Strictly additive to [`Self::McpServersChanged`] — the unit
    /// variant continues to fire for global-config edits. The two
    /// cases are split so per-project reloads don't
    /// echoagent process sharing the home dir). The agent should consult the cache
    /// thrash unrelated sessions.
    ProjectMcpServersChanged {
        /// The project root whose `.echo-agent/`, `.mcp.json`, or
        /// `.claude.json` file was edited. Sessions whose cwd equals
        /// this path — or is a descendant of it — are the reload
        /// targets.
        cwd: PathBuf,
    },
    /// Updated memory config (boxed to avoid large enum variant).
    Memory(Box<crate::config::MemoryConfig>),
    /// Updated skills discovery config.
    Skills(echo_agent_core::prompt::skills::SkillsConfig),
    /// Updated `[compat]` vendor-compatibility config. Applied on the
    /// next agent (re)build, which re-resolves `compat_resolved`.
    Compat(Box<echo_agent_tools::types::compat::CompatConfigToml>),
    /// The `[model.*]` entries in config.toml changed. Agent should re-resolve
    /// its model list (BYOK models added/removed, default or surprise changed).
    ModelsChanged,
    /// `~/.echo-agent/models_cache.json` was rewritten on disk (possibly by another
    /// via `ModelsManager::reload_from_disk_cache`, which content-dedupes
    /// self-writes (`persist` / `renew_ttl`) before applying. No payload —
    /// validation (TTL, version, auth method) requires `ModelsManager` state
    /// drop redundant `ProjectMcpServersChanged` dispatches on
    /// the reloader doesn't have.
    ModelsCacheChanged,
    /// Updated UI settings — agent broadcasts `echo.agent/config_changed` to IPC clients.
    Ui {
        theme: Option<String>,
        yolo: bool,
        fork_secondary_model: Option<String>,
    },
}

/// Runs on `tokio::spawn` (`Send`). Receives raw [`ConfigChangeEvent`]s from
/// the file watcher, diffs against last-known state, and sends [`ConfigUpdate`]
/// messages to the agent via an `mpsc` channel.
pub(crate) struct ConfigReloader {
    last_auth_key_hash: u64,
    last_global_config: toml::Value,
    last_effective_config: toml::Value,
    /// Per-cwd content hash of the project MCP config files, used to
    /// to diff (the dedup lives in `ModelsManager::reload_from_disk_cache`),
    /// mtime-only touches (see `hash_project_mcp_config`).
    last_project_mcp_hashes: HashMap<PathBuf, u64>,
    echo_agent_home: PathBuf,
    auth_scope: String,
    remote_settings: Option<crate::util::config::RemoteSettings>,
    config_update_tx: mpsc::UnboundedSender<ConfigUpdate>,
    memory_enabled_override: Option<bool>,
}

impl ConfigReloader {
    pub(crate) fn new(
        echo_agent_home: PathBuf,
        initial_auth_key_hash: u64,
        initial_config: toml::Value,
        auth_scope: String,
        remote_settings: Option<crate::util::config::RemoteSettings>,
        config_update_tx: mpsc::UnboundedSender<ConfigUpdate>,
        memory_enabled_override: Option<bool>,
    ) -> Self {
        let last_effective_config =
            crate::config::load_effective_config().unwrap_or_else(|_| initial_config.clone());
        Self {
            last_auth_key_hash: initial_auth_key_hash,
            last_global_config: initial_config,
            last_effective_config,
            last_project_mcp_hashes: HashMap::new(),
            echo_agent_home,
            auth_scope,
            remote_settings,
            config_update_tx,
            memory_enabled_override,
        }
    }

    /// Main loop. Batches all events from each debounce tick before processing.
    pub(crate) async fn run(
        mut self,
        mut events: mpsc::UnboundedReceiver<ConfigChangeEvent>,
        cancel: CancellationToken,
    ) {
        loop {
            let first = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                evt = events.recv() => match evt {
                    Some(e) => e,
                    None => break,
                },
            };

            // Drain additional events that arrived in the same tick
            let mut batch = vec![first];
            while let Ok(evt) = events.try_recv() {
                batch.push(evt);
            }

            let has_auth = batch
                .iter()
                .any(|e| matches!(e, ConfigChangeEvent::AuthChanged));
            let has_global_config = batch
                .iter()
                .any(|e| matches!(e, ConfigChangeEvent::GlobalConfigChanged));
            let has_project_config = batch
                .iter()
                .any(|e| matches!(e, ConfigChangeEvent::ProjectConfigChanged { .. }));
            // `~/.claude.json` is loaded by every
            // session (it does NOT live in a project root), so its
            // reload must broadcast through the legacy unit
            // `McpServersChanged` arm. Routing it through the per-
            // cwd variant would silently miss sessions outside `$HOME`.
            let has_home_claude_json = batch
                .iter()
                .any(|e| matches!(e, ConfigChangeEvent::HomeClaudeJsonChanged));
            let has_models_cache = batch
                .iter()
                .any(|e| matches!(e, ConfigChangeEvent::ModelsCacheChanged));
            let has_config = has_global_config || has_project_config;

            // Collect the unique cwds whose project
            // files changed so we can emit one
            // `ConfigUpdate::ProjectMcpServersChanged { cwd }` per
            // project root (rather than the legacy unit
            // `McpServersChanged` that swept every session).
            let project_cwds = collect_project_cwds(&batch);

            if has_auth {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.reload_auth()));
                match result {
                    Ok(Err(e)) => {
                        error!(error = %e, "auth hot-reload failed, keeping previous credentials");
                        // Whole-file deletion (NotFound) and corrupt JSON
                        // land here. The resulting memory/disk divergence
                        // must be visible in unified.jsonl.
                        let path = self.echo_agent_home.join("auth.json");
                        echo_agent_telemetry::unified_log::error(
                            "auth reload: auth.json unreadable, keeping previous credentials",
                            None,
                            Some(serde_json::json!({
                                "error": e.to_string(),
                                "path": path.display().to_string(),
                                "path_exists": path.exists(),
                            })),
                        );
                    }
                    Err(_) => {
                        error!("panic in auth reload handler, keeping previous credentials");
                    }
                    Ok(Ok(())) => {}
                }
            }

            if has_config {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.reload_config()));
                match result {
                    Ok(Err(e)) => {
                        error!(error = %e, "config hot-reload failed, keeping last-known-good");
                    }
                    Err(_) => {
                        error!("panic in config reload handler, keeping last-known-good");
                    }
                    Ok(Ok(())) => {}
                }
            }

            // NB: the legacy fall-through that emitted a unit
            // `McpServersChanged` for any project `.mcp.json` /
            // `.claude.json` change is replaced by the
            // per-cwd fan-out below — `collect_project_cwds` already
            // includes every `McpConfigChanged` path in `project_cwds`,
            // so a separate emit here would double-dispatch. Global
            // `[mcp_servers]` edits are dispatched inside `reload_config`.

            // Home-level `~/.claude.json` must
            // broadcast to every session through the unit variant —
            // sessions outside `$HOME` would otherwise be silently
            // skipped by the per-cwd `cwd_matches` filter.
            if has_home_claude_json {
                info!("~/.claude.json change detected — broadcasting MCP reload");
                let _ = self.config_update_tx.send(ConfigUpdate::McpServersChanged);
            }

            // Pass-through (no toml diff possible here): the
            // content-vs-in-memory dedup happens in
            // `ModelsManager::reload_from_disk_cache`.
            if has_models_cache {
                debug!("models_cache.json change detected — forwarding to agent");
                let _ = self.config_update_tx.send(ConfigUpdate::ModelsCacheChanged);
            }

            // Fan out one
            // `ProjectMcpServersChanged { cwd }` per affected project
            // root. The legacy unit `McpServersChanged` above stays
            // for global-config edits — both variants can fire in the
            // same tick (e.g. `~/.echo-agent/config.toml` AND
            // `<cwd>/.mcp.json` edited together).
            for cwd in project_cwds {
                // Skip the dispatch when the project config bytes are
                // unchanged (the watcher fires on mtime-only touches).
                // On any uncertainty we dispatch; see
                // `hash_project_mcp_config`.
                let new_hash = hash_project_mcp_config(&cwd);
                let unchanged = match (new_hash, self.last_project_mcp_hashes.get(&cwd)) {
                    (Some(new), Some(&prev)) => new == prev,
                    _ => false,
                };
                if unchanged {
                    debug!(
                        cwd = %cwd.display(),
                        "project MCP config event with unchanged content, skipping reload"
                    );
                    continue;
                }
                if let Some(h) = new_hash {
                    self.last_project_mcp_hashes.insert(cwd.clone(), h);
                }
                info!("project MCP config change detected");
                let _ = self
                    .config_update_tx
                    .send(ConfigUpdate::ProjectMcpServersChanged { cwd });
            }
        }
    }

    pub(crate) fn reload_auth(&mut self) -> anyhow::Result<()> {
        let auth_path = self.echo_agent_home.join("auth.json");
        let store = read_auth_json(&auth_path)?;

        match crate::auth::lookup_auth(&store, &self.auth_scope) {
            Some(auth) => {
                let new_hash = hash_auth_key(&auth.key);

                if new_hash == self.last_auth_key_hash {
                    debug!("auth.json changed but token key is identical, skipping");
                    return Ok(());
                }

                self.last_auth_key_hash = new_hash;
                let _ = self
                    .config_update_tx
                    .send(ConfigUpdate::Auth(Box::new(auth.clone())));
                info!("auth token change detected, sent update to agent");
            }
            None => {
                if self.last_auth_key_hash != 0 {
                    self.last_auth_key_hash = 0;
                    let _ = self.config_update_tx.send(ConfigUpdate::AuthCleared);
                    info!("auth scope removed from auth.json, sent clear to agent");
                    // AuthCleared makes the agent drop in-memory credentials;
                    // record what the reloader saw so "entry removed" is
                    // distinguishable from "file deleted" (the Err path).
                    echo_agent_telemetry::unified_log::warn(
                        "auth reload: scope entry gone, sending AuthCleared",
                        None,
                        Some(serde_json::json!({
                            "scope": &self.auth_scope,
                            "scopes_on_disk": store.keys().collect::<Vec<_>>(),
                        })),
                    );
                }
            }
        }
        Ok(())
    }

    fn reload_config(&mut self) -> anyhow::Result<()> {
        // `has_project_config` parameter dropped —
        // project-scoped reloads are dispatched via
        // `ProjectMcpServersChanged { cwd }` in the caller's
        // `collect_project_cwds` fan-out, so this function only
        // needs to diff the global toml.
        let new_global = match crate::config::load_from_disk() {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, "failed to parse config.toml, keeping last-known-good");
                return Ok(());
            }
        };

        // MCP servers — compare [mcp_servers] table in the **global**
        // config (`~/.echo-agent/config.toml`) via toml::Value. Project-
        // scoped changes (`<cwd>/.echo-agent/config.toml`,
        // `<cwd>/.mcp.json`) are dispatched separately via
        // `ConfigUpdate::ProjectMcpServersChanged { cwd }` (see
        // `collect_project_cwds`) so they don't sweep
        // unrelated sessions.
        let old_mcp_table = self.last_global_config.get("mcp_servers");
        let new_mcp_table = new_global.get("mcp_servers");
        let mcp_changed = old_mcp_table != new_mcp_table;
        if mcp_changed {
            info!("Global MCP server config change detected");
            let _ = self.config_update_tx.send(ConfigUpdate::McpServersChanged);
        }

        let mut accepted_effective = None;
        match crate::config::load_effective_config() {
            Ok(new_effective) => match changed_memory_config(
                &self.last_effective_config,
                &new_effective,
                self.memory_enabled_override,
                self.remote_settings.as_ref(),
            ) {
                Ok(new_mem) => {
                    if let Some(new_mem) = new_mem {
                        info!("memory config change detected");
                        let _ = self
                            .config_update_tx
                            .send(ConfigUpdate::Memory(Box::new(new_mem)));
                    }
                    accepted_effective = Some(new_effective);
                }
                Err(e) => {
                    error!(
                        error = %e,
                        "failed to parse the config, keeping the previous memory config"
                    );
                }
            },
            Err(e) => {
                error!(
                    error = %e,
                    "failed to load effective config, keeping previous memory config"
                );
            }
        }

        // Skills config
        let old_skills = parse_skills_config(&self.last_global_config);
        let new_skills = parse_skills_config(&new_global);
        if old_skills != new_skills {
            info!("skills config change detected");
            let _ = self.config_update_tx.send(ConfigUpdate::Skills(new_skills));
        }

        // Compat config ([compat] vendor toggles)
        let old_compat = parse_compat_config(&self.last_global_config);
        let new_compat = parse_compat_config(&new_global);
        if old_compat != new_compat {
            info!("compat config change detected");
            let _ = self
                .config_update_tx
                .send(ConfigUpdate::Compat(Box::new(new_compat)));
        }

        // Models — compare [model] (BYOK entries) and [models] (default, surprise) tables.
        // Use toml::Value comparison (covers all fields including nested model entries).
        let old_model_table = self.last_global_config.get("model");
        let new_model_table = new_global.get("model");
        let old_models_table = self.last_global_config.get("models");
        let new_models_table = new_global.get("models");
        if old_model_table != new_model_table || old_models_table != new_models_table {
            info!("model config change detected");
            let _ = self.config_update_tx.send(ConfigUpdate::ModelsChanged);
        }

        // UI fields (theme, yolo, fork_secondary_model)
        let old_ui = extract_ui_fields(&self.last_global_config);
        let new_ui = extract_ui_fields(&new_global);
        if old_ui != new_ui {
            info!("UI config change detected");
            let _ = self.config_update_tx.send(ConfigUpdate::Ui {
                theme: new_ui.0,
                yolo: new_ui.1,
                fork_secondary_model: new_ui.2,
            });
        }

        self.last_global_config = new_global;
        if let Some(new_effective) = accepted_effective {
            self.last_effective_config = new_effective;
        }
        Ok(())
    }
}

fn changed_memory_config(
    old_effective: &toml::Value,
    new_effective: &toml::Value,
    memory_enabled_override: Option<bool>,
    remote_settings: Option<&crate::util::config::RemoteSettings>,
) -> Result<Option<crate::config::MemoryConfig>, String> {
    let old_config = crate::agent::config::Config::new_from_toml_cfg(old_effective)?;
    let new_config = crate::agent::config::Config::new_from_toml_cfg(new_effective)?;
    let old_mem = old_config.resolve_memory(memory_enabled_override, remote_settings);
    let new_mem = new_config.resolve_memory(memory_enabled_override, remote_settings);
    Ok((old_mem != new_mem).then_some(new_mem))
}

/// Derive the unique project cwds whose files were touched in this
/// debounce window. Used to fan out one
/// [`ConfigUpdate::ProjectMcpServersChanged`] per project root rather
/// than one legacy `McpServersChanged` that reloads every active
/// session.
///
/// Path-to-cwd mapping:
///
/// | `ConfigChangeEvent`        | path shape              | cwd               |
/// |----------------------------|-------------------------|-------------------|
/// | `ProjectConfigChanged`     | `<cwd>/.echo-agent/config.toml` | `<cwd>`           |
/// | `McpConfigChanged`         | `<cwd>/.mcp.json`         | `<cwd>`           |
/// | `McpConfigChanged`         | `<cwd>/.claude.json`      | `<cwd>`           |
///
/// Order-preserving de-dup (a `Vec` rather than a `HashSet`) so the
/// downstream emit order is deterministic in tests.
fn collect_project_cwds(batch: &[ConfigChangeEvent]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for evt in batch {
        let cwd = match evt {
            ConfigChangeEvent::ProjectConfigChanged { path } => {
                // <cwd>/.echo-agent/config.toml → <cwd>
                path.parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_path_buf())
            }
            ConfigChangeEvent::McpConfigChanged { path } => {
                // <cwd>/.mcp.json or <cwd>/.claude.json → <cwd>
                path.parent().map(|p| p.to_path_buf())
            }
            _ => None,
        };
        if let Some(cwd) = cwd
            && !out.contains(&cwd)
        {
            out.push(cwd);
        }
    }
    out
}

/// Content hash of the cwd-dependent MCP config files a
/// `ProjectMcpServersChanged { cwd }` reload re-reads. Walks ancestors
/// up to the git root exactly as the loaders do (`find_project_configs`
/// for `.echo-agent/config.toml`, `find_mcp_json_files` for `.mcp.json`) so
/// the hash can't drift from the set the merge actually reads, plus
/// `<cwd>/.claude.json` (watched at the project root). A stable hash
/// means the reload would be a no-op. Home-level sources
/// (`~/.echo-agent/config.toml`, `~/.claude.json`, `~/.cursor/mcp.json`)
/// change through their own events.
///
/// Returns `None` on a non-`NotFound` read error so the caller
/// dispatches rather than risk suppressing a real edit.
fn hash_project_mcp_config(cwd: &Path) -> Option<u64> {
    let mut paths = crate::config::find_project_configs(cwd);
    paths.extend(crate::util::config::find_mcp_json_files(cwd));
    paths.push(cwd.join(".claude.json"));

    let mut hasher = DefaultHasher::new();
    paths.len().hash(&mut hasher);
    for f in &paths {
        f.to_string_lossy().hash(&mut hasher);
        match std::fs::read(f) {
            Ok(bytes) => {
                1u8.hash(&mut hasher); // present
                bytes.hash(&mut hasher);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                0u8.hash(&mut hasher); // absent
            }
            Err(_) => return None, // can't read confidently → dispatch
        }
    }
    Some(hasher.finish())
}

pub(crate) fn hash_auth_key(key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// Extract the `[skills]` table from an effective config.
///
/// Consumers: the reload dispatch above (change detection →
/// `ConfigUpdate::Skills`) and `echoagent inspect` (via the `crate::config`
/// re-export), so both honor the same paths/ignore/disabled as a live
/// session. Session spawn parses the same table separately through the typed
/// `Config.skills` (agent/config.rs) — keep these in sync rather than adding
/// a fourth parse path.
pub(crate) fn parse_skills_config(
    config: &toml::Value,
) -> echo_agent_core::prompt::skills::SkillsConfig {
    config
        .get("skills")
        .and_then(|v| v.clone().try_into().ok())
        .unwrap_or_default()
}

fn parse_compat_config(config: &toml::Value) -> echo_agent_tools::types::compat::CompatConfigToml {
    config
        .get("compat")
        .and_then(|v| v.clone().try_into().ok())
        .unwrap_or_default()
}

fn extract_ui_fields(config: &toml::Value) -> (Option<String>, bool, Option<String>) {
    let ui = config.get("ui").and_then(|v| v.as_table());
    let theme = ui
        .and_then(|u| u.get("theme"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let yolo = ui
        .and_then(|u| u.get("yolo"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let fork = ui
        .and_then(|u| u.get("fork_secondary_model"))
        .and_then(|v| v.as_str())
        .map(String::from);
    (theme, yolo, fork)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::EchoAgentAuth;
    use std::collections::BTreeMap;

    fn make_auth(key: &str) -> EchoAgentAuth {
        EchoAgentAuth {
            key: key.to_string(),
            email: Some("test@test.com".to_string()),
            ..EchoAgentAuth::test_default()
        }
    }

    #[tokio::test]
    async fn reloader_skips_unchanged_auth() {
        let tmp = tempfile::TempDir::new().unwrap();
        let auth = make_auth("same-key");
        let mut store = BTreeMap::new();
        let scope = "https://test.example.com".to_string();
        store.insert(scope.clone(), auth);
        let json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(tmp.path().join("auth.json"), &json).unwrap();

        let initial_hash = hash_auth_key("same-key");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let mut reloader = ConfigReloader::new(
            tmp.path().to_path_buf(),
            initial_hash,
            empty_config,
            scope,
            None,
            tx,
            None,
        );

        reloader.reload_auth().unwrap();
        assert!(
            rx.try_recv().is_err(),
            "should not send update when key is unchanged"
        );
    }

    #[tokio::test]
    async fn reloader_detects_new_auth_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let auth = make_auth("new-key");
        let mut store = BTreeMap::new();
        let scope = "https://test.example.com".to_string();
        store.insert(scope.clone(), auth);
        let json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(tmp.path().join("auth.json"), &json).unwrap();

        let old_hash = hash_auth_key("old-key");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let mut reloader = ConfigReloader::new(
            tmp.path().to_path_buf(),
            old_hash,
            empty_config,
            scope,
            None,
            tx,
            None,
        );

        reloader.reload_auth().unwrap();
        let update = rx.try_recv().expect("should send Auth update");
        assert!(
            matches!(update, ConfigUpdate::Auth(a) if a.key == "new-key"), // a is Box<EchoAgentAuth>, Deref coercion
            "should contain new key"
        );
    }

    #[tokio::test]
    async fn reloader_detects_auth_cleared() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Write auth.json with a DIFFERENT scope — our scope is missing
        let auth = make_auth("other-key");
        let mut store = BTreeMap::new();
        store.insert("https://other.example.com".to_string(), auth);
        let json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(tmp.path().join("auth.json"), &json).unwrap();

        let old_hash = hash_auth_key("had-a-key");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let mut reloader = ConfigReloader::new(
            tmp.path().to_path_buf(),
            old_hash,
            empty_config,
            "https://test.example.com".to_string(),
            None,
            tx,
            None,
        );

        reloader.reload_auth().unwrap();
        let update = rx.try_recv().expect("should send AuthCleared");
        assert!(matches!(update, ConfigUpdate::AuthCleared));
    }

    #[tokio::test]
    async fn reloader_handles_malformed_auth_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("auth.json"), "not valid json{{{").unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let mut reloader = ConfigReloader::new(
            tmp.path().to_path_buf(),
            0,
            empty_config,
            "https://test.example.com".to_string(),
            None,
            tx,
            None,
        );

        let result = reloader.reload_auth();
        assert!(result.is_err(), "malformed JSON should return error");
        assert!(
            rx.try_recv().is_err(),
            "should not send update on parse failure"
        );
    }

    #[tokio::test]
    async fn reloader_handles_missing_auth_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No auth.json written

        let (tx, mut rx) = mpsc::unbounded_channel();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let mut reloader = ConfigReloader::new(
            tmp.path().to_path_buf(),
            0,
            empty_config,
            "https://test.example.com".to_string(),
            None,
            tx,
            None,
        );

        let result = reloader.reload_auth();
        assert!(result.is_err(), "missing file should return error");
        assert!(
            rx.try_recv().is_err(),
            "should not send update on missing file"
        );
    }

    /// `ModelsCacheChanged` is a pure pass-through: the reloader has no toml
    /// so the event must surface as `ConfigUpdate::ModelsCacheChanged`
    /// (walked to the git root by the loaders), not just files directly
    /// without touching auth or config state.
    #[tokio::test]
    async fn reloader_forwards_models_cache_changed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let reloader = ConfigReloader::new(
            tmp.path().to_path_buf(),
            0,
            empty_config,
            "https://test.example.com".to_string(),
            None,
            tx,
            None,
        );

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(reloader.run(event_rx, cancel.clone()));

        event_tx
            .send(ConfigChangeEvent::ModelsCacheChanged)
            .unwrap();

        let update = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive an update within 2s")
            .expect("update channel should remain open");
        assert!(matches!(update, ConfigUpdate::ModelsCacheChanged));

        cancel.cancel();
        let _ = handle.await;
    }

    /// A project event with unchanged bytes must not re-dispatch a
    /// reload; the first event and a later real edit must both dispatch.
    #[tokio::test]
    async fn reloader_dedupes_unchanged_project_mcp_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let cwd = tmp.path().to_path_buf();
        let mcp_json = cwd.join(".mcp.json");
        std::fs::write(&mcp_json, r#"{"mcpServers":{}}"#).unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let reloader = ConfigReloader::new(
            tmp.path().to_path_buf(),
            0,
            empty_config,
            "https://test.example.com".to_string(),
            None,
            tx,
            None,
        );

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(reloader.run(event_rx, cancel.clone()));

        let evt = || ConfigChangeEvent::McpConfigChanged {
            path: mcp_json.clone(),
        };

        // First event → dispatch (no prior hash for this cwd).
        event_tx.send(evt()).unwrap();
        let update = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("first event should dispatch within 2s")
            .expect("channel open");
        assert!(
            matches!(update, ConfigUpdate::ProjectMcpServersChanged { cwd: ref c } if *c == cwd),
            "first project event must dispatch"
        );

        // Second event, identical bytes → must be suppressed.
        event_tx.send(evt()).unwrap();
        let res = tokio::time::timeout(std::time::Duration::from_millis(400), rx.recv()).await;
        assert!(
            res.is_err(),
            "unchanged project config must not re-dispatch a reload"
        );

        // Real content change → dispatch again.
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"x":{"url":"http://localhost"}}}"#,
        )
        .unwrap();
        event_tx.send(evt()).unwrap();
        let update = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("changed content should dispatch within 2s")
            .expect("channel open");
        assert!(
            matches!(update, ConfigUpdate::ProjectMcpServersChanged { cwd: ref c } if *c == cwd),
            "changed project config must dispatch"
        );

        cancel.cancel();
        let _ = handle.await;
    }

    /// `hash_project_mcp_config` is stable for identical content and
    /// changes on create/edit.
    #[test]
    fn hash_project_mcp_config_detects_create_and_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let cwd = tmp.path();

        let empty = hash_project_mcp_config(cwd).expect("readable");
        assert_eq!(empty, hash_project_mcp_config(cwd).expect("stable"));

        std::fs::write(cwd.join(".mcp.json"), "a").unwrap();
        let created = hash_project_mcp_config(cwd).expect("readable");
        assert_ne!(empty, created, "creating a config file changes the hash");

        std::fs::write(cwd.join(".mcp.json"), "b").unwrap();
        let changed = hash_project_mcp_config(cwd).expect("readable");
        assert_ne!(created, changed, "editing content changes the hash");
    }

    /// The hash must reflect ancestor `.echo-agent/config.toml` and `.mcp.json`
    /// under `cwd` — otherwise an ancestor edit would be wrongly
    /// must be a distinct variant from the unit `McpServersChanged`
    /// suppressed.
    #[test]
    fn hash_project_mcp_config_covers_ancestors() {
        let tmp = tempfile::TempDir::new().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let child = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&child).unwrap();

        let h0 = hash_project_mcp_config(&child).expect("readable");

        std::fs::write(tmp.path().join(".mcp.json"), "a").unwrap();
        let h1 = hash_project_mcp_config(&child).expect("readable");
        assert_ne!(h0, h1, "ancestor .mcp.json create must change the hash");

        std::fs::write(tmp.path().join(".mcp.json"), "b").unwrap();
        let h2 = hash_project_mcp_config(&child).expect("readable");
        assert_ne!(h1, h2, "ancestor .mcp.json edit must change the hash");

        std::fs::create_dir_all(tmp.path().join(".echo-agent")).unwrap();
        std::fs::write(tmp.path().join(".echo-agent").join("config.toml"), "x = 1").unwrap();
        let h3 = hash_project_mcp_config(&child).expect("readable");
        assert_ne!(
            h2, h3,
            "ancestor .echo-agent/config.toml create must change the hash"
        );
    }

    #[test]
    fn parse_skills_config_empty() {
        let config = toml::Value::Table(toml::map::Map::new());
        let skills = parse_skills_config(&config);
        assert_eq!(
            skills,
            echo_agent_core::prompt::skills::SkillsConfig::default()
        );
    }

    #[test]
    fn parse_skills_config_with_paths() {
        let config: toml::Value = toml::from_str(
            r#"
[skills]
paths = ["/home/user/.echo-agent/skills"]
ignore = ["/tmp"]
"#,
        )
        .unwrap();
        let skills = parse_skills_config(&config);
        assert_eq!(
            skills.paths,
            vec!["/home/user/.echo-agent/skills".to_string()]
        );
        assert_eq!(skills.ignore, vec!["/tmp".to_string()]);
    }

    #[test]
    fn memory_config_diff_detects_enabled_change() {
        let empty = toml::Value::Table(toml::map::Map::new());
        let enabled: toml::Value = toml::from_str("[memory]\nenabled = true").unwrap();

        let old = crate::agent::config::Config::new_from_toml_cfg(&empty)
            .unwrap()
            .resolve_memory(None, None);
        let new = crate::agent::config::Config::new_from_toml_cfg(&enabled)
            .unwrap()
            .resolve_memory(None, None);
        assert_ne!(old, new, "should detect enabled field change");
    }

    #[test]
    fn memory_config_diff_detects_search_param_change() {
        let a: toml::Value = toml::from_str("[memory.search]\nmax_results = 6").unwrap();
        let b: toml::Value = toml::from_str("[memory.search]\nmax_results = 10").unwrap();

        let old = crate::agent::config::Config::new_from_toml_cfg(&a)
            .unwrap()
            .resolve_memory(None, None);
        let new = crate::agent::config::Config::new_from_toml_cfg(&b)
            .unwrap()
            .resolve_memory(None, None);
        assert_ne!(old, new, "should detect search param change");
    }

    #[test]
    fn memory_config_diff_rejects_invalid_typed_memory_without_advancing() {
        let old: toml::Value = toml::from_str("[memory.search]\nmax_results = 6").unwrap();
        let invalid: toml::Value =
            toml::from_str("[memory.search]\nmax_results = \"many\"").unwrap();
        assert!(changed_memory_config(&old, &invalid, None, None).is_err());
    }

    #[test]
    fn extract_ui_fields_empty() {
        let config = toml::Value::Table(toml::map::Map::new());
        let (theme, yolo, fork) = extract_ui_fields(&config);
        assert_eq!(theme, None);
        assert!(!yolo);
        assert_eq!(fork, None);
    }

    #[test]
    fn extract_ui_fields_with_values() {
        let config: toml::Value = toml::from_str(
            r#"
[ui]
theme = "dark"
yolo = true
fork_secondary_model = "echo-agent-4.5"
"#,
        )
        .unwrap();
        let (theme, yolo, fork) = extract_ui_fields(&config);
        assert_eq!(theme.as_deref(), Some("dark"));
        assert!(yolo);
        assert_eq!(fork.as_deref(), Some("echo-agent-4.5"));
    }

    #[test]
    fn extract_ui_fields_diff_detects_theme_change() {
        let a: toml::Value = toml::from_str("[ui]\ntheme = \"light\"").unwrap();
        let b: toml::Value = toml::from_str("[ui]\ntheme = \"dark\"").unwrap();
        assert_ne!(extract_ui_fields(&a), extract_ui_fields(&b));
    }

    #[test]
    fn extract_ui_fields_diff_detects_yolo_change() {
        let a: toml::Value = toml::from_str("[ui]\nyolo = false").unwrap();
        let b: toml::Value = toml::from_str("[ui]\nyolo = true").unwrap();
        assert_ne!(extract_ui_fields(&a), extract_ui_fields(&b));
    }

    #[test]
    fn models_changed_detects_new_byok_model() {
        let a = toml::Value::Table(toml::map::Map::new());
        let b: toml::Value = toml::from_str(
            r#"
[model.my-custom]
model = "echo-agent-4.5"
base_url = "https://api.example.com/v1"
"#,
        )
        .unwrap();
        assert_ne!(a.get("model"), b.get("model"));
    }

    #[test]
    fn models_changed_detects_default_change() {
        let a: toml::Value =
            toml::from_str("[models]\ndefault = \"echo-agent-code-fast-1\"").unwrap();
        let b: toml::Value =
            toml::from_str("[models]\ndefault = \"echo-agent-code-slow-1\"").unwrap();
        assert_ne!(a.get("models"), b.get("models"));
    }

    #[test]
    fn mcp_servers_changed_detects_new_server() {
        let a = toml::Value::Table(toml::map::Map::new());
        let b: toml::Value = toml::from_str(
            r#"
[mcp_servers.test]
command = "/bin/test"
"#,
        )
        .unwrap();
        assert_ne!(a.get("mcp_servers"), b.get("mcp_servers"));
    }

    /// `ConfigUpdate::ProjectMcpServersChanged { cwd }`
    /// so the two paths route through different match arms in
    /// `app.rs`. Guards against an accidental merge that would force
    /// fan-out — it must NOT contribute a cwd to
    /// per-cwd reloads through the legacy sweep-all-sessions arm.
    #[test]
    fn project_variant_dispatches_separately() {
        let cwd = PathBuf::from("/tmp/proj-x");
        let global: ConfigUpdate = ConfigUpdate::McpServersChanged;
        let project = ConfigUpdate::ProjectMcpServersChanged { cwd: cwd.clone() };

        // Each variant must be matched by its own arm — fall-through
        // would indicate a single arm handling both.
        let mut routed_global = false;
        let mut routed_project = None;
        for u in [global, project] {
            match u {
                ConfigUpdate::McpServersChanged => routed_global = true,
                ConfigUpdate::ProjectMcpServersChanged { cwd } => routed_project = Some(cwd),
                _ => panic!("unexpected variant"),
            }
        }
        assert!(
            routed_global,
            "global variant must route through its own arm"
        );
        assert_eq!(routed_project.as_deref(), Some(cwd.as_path()));
    }

    /// `HomeClaudeJsonChanged` is **not** part of the per-cwd
    /// `collect_project_cwds` (otherwise sessions outside `$HOME`
    /// would be silently skipped). The reloader broadcasts it via
    /// the unit `McpServersChanged` variant; this test locks that
    /// `ProjectConfigChanged` (`<cwd>/.echo-agent/config.toml`) and
    /// invariant at the helper layer.
    #[test]
    fn collect_project_cwds_excludes_home_claude_json() {
        let batch = vec![
            ConfigChangeEvent::HomeClaudeJsonChanged,
            ConfigChangeEvent::ProjectConfigChanged {
                path: PathBuf::from("/repo/x/.echo-agent/config.toml"),
            },
        ];
        let cwds = collect_project_cwds(&batch);
        // Only the project entry contributes; the home-level `.claude.json`
        // entry is silently dropped because it routes through the
        // broadcast arm instead.
        assert_eq!(cwds, vec![PathBuf::from("/repo/x")]);
    }

    /// `collect_project_cwds` extracts `<cwd>` from
    /// `McpConfigChanged` (`<cwd>/.mcp.json`), de-duplicates while
    /// `McpConfigChanged` (`<cwd>/.mcp.json`), de-duplicates while
    /// preserving order.
    #[test]
    fn collect_project_cwds_dedupes_and_extracts() {
        let batch = vec![
            ConfigChangeEvent::ProjectConfigChanged {
                path: PathBuf::from("/repo/a/.echo-agent/config.toml"),
            },
            ConfigChangeEvent::McpConfigChanged {
                path: PathBuf::from("/repo/a/.mcp.json"),
            },
            ConfigChangeEvent::ProjectConfigChanged {
                path: PathBuf::from("/repo/b/.echo-agent/config.toml"),
            },
        ];
        let cwds = collect_project_cwds(&batch);
        assert_eq!(
            cwds,
            vec![PathBuf::from("/repo/a"), PathBuf::from("/repo/b")]
        );
    }

    #[test]
    fn mcp_servers_unchanged_same_config() {
        let cfg: toml::Value = toml::from_str(
            r#"
[mcp_servers.test]
command = "/bin/test"
"#,
        )
        .unwrap();
        assert_eq!(cfg.get("mcp_servers"), cfg.get("mcp_servers"));
    }
}
