use super::*;

impl SessionActor {
    // ── Shared hook/plugin operation functions ────────────────────────

    /// Trust the current project via the unified folder-trust store. Now an
    /// alias of `--trust`: also allows repo-local MCP/LSP for this folder.
    pub(super) fn do_hooks_trust_project(cwd: &str) -> Result<std::path::PathBuf, String> {
        let root =
            echo_agent_workspace::session::git::find_git_root_from_path(std::path::Path::new(cwd))
                .map_err(|_| {
                    "Not in a git repository. Project hooks require a git worktree root."
                        .to_string()
                })?;
        crate::agent::folder_trust::grant_folder_trust(&root);
        Ok(root)
    }

    /// Untrust the current project in the unified folder-trust store.
    /// Returns (git_root, was_trusted).
    pub(super) fn do_hooks_untrust_project(
        cwd: &str,
    ) -> Result<(std::path::PathBuf, bool), String> {
        let root =
            echo_agent_workspace::session::git::find_git_root_from_path(std::path::Path::new(cwd))
                .map_err(|_| "Not in a git repository.".to_string())?;
        // revoke_folder_trust persists set_untrusted AND downgrades the decision
        // cache so the untrust takes effect on the next reload, not just restart.
        let was_trusted = crate::agent::folder_trust::revoke_folder_trust(&root);
        Ok((root, was_trusted))
    }

    /// Re-resolve the session-scoped MCP output cap (repo
    /// `[mcp] max_output_bytes`) for this session's cwd and update the
    /// toolset's `TruncationCfg` resource to match.
    ///
    /// Field-level update so any other `TruncationCfg` fields a host seeded
    /// are preserved; clears the cap (restoring the process-global fallback)
    /// when the project tier no longer wins — e.g. the key was removed, or
    /// **folder trust was revoked** (`resolve_max_mcp_output_bytes_for_cwd`
    /// is trust-gated, so calling this after a trust change keeps the seeded
    /// cap in lockstep with the gate).
    ///
    /// Called from the `UpdateMcpServers` handler (project-config hot reload)
    /// and from the hooks-modal Trust/Untrust actions.
    pub(super) async fn reseed_mcp_output_cap(&self) {
        let resolved = crate::util::config::resolve_max_mcp_output_bytes_for_cwd(
            std::path::Path::new(&self.session_info.cwd),
        );
        let bridge = std::sync::Arc::clone(self.agent.borrow().tool_bridge());
        let toolset = bridge.toolset();
        let mut resources = toolset.resources.lock().await;
        let existing = resources
            .get::<echo_agent_tools::types::resources::TruncationCfg>()
            .map(|c| c.0.clone());
        match (resolved, existing) {
            (resolved, Some(mut cfg)) => {
                if cfg.mcp_max_output_bytes != resolved {
                    cfg.mcp_max_output_bytes = resolved;
                    resources.insert(echo_agent_tools::types::resources::TruncationCfg(cfg));
                }
            }
            (Some(v), None) => {
                resources.insert(echo_agent_tools::types::resources::TruncationCfg(
                    echo_agent_tools::types::context::TruncationConfig {
                        mcp_max_output_bytes: Some(v),
                        ..Default::default()
                    },
                ));
            }
            (None, None) => {}
        }
    }

    /// Resolve a potentially relative path against the session cwd.
    fn resolve_path(cwd: &str, path: &str) -> std::path::PathBuf {
        let p = std::path::Path::new(path);
        if p.is_relative() {
            std::path::Path::new(cwd).join(p)
        } else {
            p.to_path_buf()
        }
    }

    // ── Hooks/plugins action handlers (pager modal) ──────────────────

    /// Handle a hooks management action from the pager modal.
    pub(super) async fn handle_hooks_action(
        self: &Arc<Self>,
        action: echo_agent_hooks_plugins_types::HooksAction,
    ) -> echo_agent_hooks_plugins_types::ActionOutcome {
        use echo_agent_hooks_plugins_types::{ActionOutcome, HooksAction, OutcomeStatus};

        match action {
            HooksAction::Reload => {
                let reload_msg = self.reload_hooks_impl().await;
                ActionOutcome {
                    status: OutcomeStatus::Success,
                    message: format!("Hooks reloaded.\n{reload_msg}"),
                    requires_reload: false,
                    requires_restart: false,
                }
            }
            HooksAction::Trust => match Self::do_hooks_trust_project(&self.session_info.cwd) {
                Err(e) => ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: e,
                    requires_reload: false,
                    requires_restart: false,
                },
                Ok(root) => {
                    let reload_msg = self.reload_hooks_impl().await;
                    // Trust change flips the project-config gate: re-seed the
                    // repo-level MCP output cap so it applies without waiting
                    // for a config edit.
                    self.reseed_mcp_output_cap().await;
                    ActionOutcome {
                        status: OutcomeStatus::Success,
                        message: format!("Trusted: {}.\n{reload_msg}", root.display()),
                        requires_reload: false,
                        requires_restart: false,
                    }
                }
            },
            HooksAction::Untrust => match Self::do_hooks_untrust_project(&self.session_info.cwd) {
                Err(e) => ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: e,
                    requires_reload: false,
                    requires_restart: false,
                },
                Ok((root, false)) => ActionOutcome {
                    status: OutcomeStatus::NotFound,
                    message: format!("Not currently trusted: {}", root.display()),
                    requires_reload: false,
                    requires_restart: false,
                },
                Ok((root, true)) => {
                    let reload_msg = self.reload_hooks_impl().await;
                    // Revoked trust must immediately drop a previously seeded
                    // repo-level MCP output cap (the resolver is trust-gated,
                    // so this clears it) — not linger until the next config
                    // reload.
                    self.reseed_mcp_output_cap().await;
                    ActionOutcome {
                        status: OutcomeStatus::Success,
                        message: format!("Untrusted: {}.\n{reload_msg}", root.display()),
                        requires_reload: false,
                        requires_restart: false,
                    }
                }
            },
            HooksAction::Add { path } => {
                if path.is_empty() {
                    return ActionOutcome {
                        status: OutcomeStatus::ValidationError,
                        message: "Path is required.".into(),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }
                // CWE-427: add_hooks_path() validates path is under ~/.echo-agent/.
                match crate::config::add_hooks_path(&path) {
                    Ok(()) => ActionOutcome {
                        status: OutcomeStatus::Success,
                        message: {
                            let reload_msg = self.reload_hooks_impl().await;
                            format!("Added hook path: {path}.\n{reload_msg}")
                        },
                        requires_reload: false,
                        requires_restart: false,
                    },
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::ValidationError,
                        message: format!("Failed to add hook path: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            HooksAction::Remove { path } => {
                if path.is_empty() {
                    return ActionOutcome {
                        status: OutcomeStatus::ValidationError,
                        message: "Path is required.".into(),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }
                match crate::config::remove_hooks_path(&path) {
                    Ok(()) => ActionOutcome {
                        status: OutcomeStatus::Success,
                        message: {
                            let reload_msg = self.reload_hooks_impl().await;
                            format!("Removed hook path: {path}.\n{reload_msg}")
                        },
                        requires_reload: false,
                        requires_restart: false,
                    },
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to remove hook path: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            HooksAction::Disable { hook_name } => {
                match echo_agent_hooks::trust::disable_hook(&hook_name) {
                    Ok(()) => ActionOutcome {
                        status: OutcomeStatus::Success,
                        message: format!("Disabled hook: {hook_name}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to disable hook: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            HooksAction::Enable { hook_name } => {
                match echo_agent_hooks::trust::enable_hook(&hook_name) {
                    Ok(true) => ActionOutcome {
                        status: OutcomeStatus::Success,
                        message: format!("Enabled hook: {hook_name}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                    Ok(false) => ActionOutcome {
                        status: OutcomeStatus::NotFound,
                        message: format!("Hook was not disabled: {hook_name}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to enable hook: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            HooksAction::ToggleSource {
                hook_names,
                disable,
            } => {
                let mut toggled = 0usize;
                for name in &hook_names {
                    let ok = if disable {
                        echo_agent_hooks::trust::disable_hook(name).is_ok()
                    } else {
                        echo_agent_hooks::trust::enable_hook(name).is_ok()
                    };
                    if ok {
                        toggled += 1;
                    }
                }
                let action = if disable { "Disabled" } else { "Enabled" };
                ActionOutcome {
                    status: OutcomeStatus::Success,
                    message: format!("{action} {toggled}/{} hooks", hook_names.len()),
                    requires_reload: false,
                    requires_restart: false,
                }
            }
        }
    }

    /// Handle a plugins management action from the pager modal.
    pub(super) async fn handle_plugins_action(
        self: &Arc<Self>,
        action: echo_agent_hooks_plugins_types::PluginsAction,
    ) -> echo_agent_hooks_plugins_types::ActionOutcome {
        use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus, PluginsAction};

        match action {
            PluginsAction::Reload => match &self.plugin_registry_handle {
                Some(handle) => {
                    // Explicit user reload: force a full local-install re-copy.
                    let msg = self.reload_plugins_impl(handle, true).await;
                    ActionOutcome {
                        status: OutcomeStatus::Success,
                        message: msg,
                        requires_reload: false,
                        requires_restart: false,
                    }
                }
                None => ActionOutcome {
                    status: OutcomeStatus::Unsupported,
                    message: "No plugin registry handle available.".into(),
                    requires_reload: false,
                    requires_restart: false,
                },
            },
            PluginsAction::Install { source } => {
                if source.is_empty() {
                    return ActionOutcome {
                        status: OutcomeStatus::ValidationError,
                        message: "Source is required (git URL or local path).".into(),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }
                let cwd = std::path::Path::new(&self.session_info.cwd);
                let install_source =
                    echo_agent_core::plugins::git_install::parse_install_source(&source, cwd);
                let registry = echo_agent_core::plugins::InstallRegistry::load();
                match echo_agent_core::plugins::git_install::install_from_source(
                    &install_source,
                    &registry,
                    crate::plugin::marketplace_require_sha(),
                ) {
                    Ok(result) => {
                        let repo = echo_agent_core::plugins::git_install::build_installed_repo(
                            &result,
                            &install_source,
                        );
                        let mut registry = registry;
                        registry.insert(result.repo_key.clone(), repo);
                        if let Err(e) = registry.save() {
                            tracing::warn!("Failed to save install registry: {e}");
                        }
                        let (names, post_warnings) =
                            crate::config::post_install_plugin(&result.repo_key);
                        let count = names.len();
                        let mut msg = format!(
                            "Installed {count} plugin(s) from {source}: {}",
                            names.join(", ")
                        );
                        for w in &post_warnings {
                            msg.push_str(&format!(" (warning: {w})"));
                        }
                        ActionOutcome {
                            status: OutcomeStatus::Success,
                            message: msg,
                            requires_reload: true,
                            requires_restart: false,
                        }
                    }
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to install plugin: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            PluginsAction::Uninstall {
                plugin_id,
                confirmed,
            } => {
                if plugin_id.is_empty() {
                    return ActionOutcome {
                        status: OutcomeStatus::ValidationError,
                        message: "Plugin ID is required.".into(),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }
                // Stable identity and the exact loaded root select the package.
                // A display name can occur in several unrelated repositories.
                let plugin_name = plugin_id.rsplit('/').next().unwrap_or(&plugin_id);
                let mut registry = echo_agent_core::plugins::InstallRegistry::load();
                let snapshot = self.plugin_registry.borrow().clone();
                let repo_key = snapshot
                    .as_ref()
                    .and_then(|plugins| {
                        plugins
                            .list()
                            .into_iter()
                            .find(|plugin| plugin.id.0 == plugin_id)
                    })
                    .and_then(|plugin| {
                        registry.find_repo_key_by_plugin_root(&plugin.root, &plugin.canonical_root)
                    })
                    .map(str::to_owned);
                match repo_key
                    .as_deref()
                    .and_then(|key| registry.get_repo(key).map(|repo| (key, repo)))
                {
                    None => ActionOutcome {
                        status: OutcomeStatus::NotFound,
                        message: format!("Plugin \"{plugin_name}\" not found in install registry."),
                        requires_reload: false,
                        requires_restart: false,
                    },
                    Some((repo_key, repo)) => {
                        let repo_key = repo_key.to_string();
                        let repo_path = repo.path.clone();
                        let plugin_names: Vec<String> = repo.plugins.keys().cloned().collect();
                        let count = plugin_names.len();

                        // Check multi-plugin repo — return ConfirmationRequired.
                        if count > 1 && !confirmed {
                            return ActionOutcome {
                                status: OutcomeStatus::ConfirmationRequired,
                                message: format!(
                                    "Repo \"{repo_key}\" contains {count} plugin(s): {}. Uninstalling will remove all of them.",
                                    plugin_names.join(", ")
                                ),
                                requires_reload: false,
                                requires_restart: false,
                            };
                        }

                        // Proceed with removal.
                        if let Err(e) =
                            echo_agent_core::plugins::git_install::remove_repo_path(&repo_path)
                        {
                            return ActionOutcome {
                                status: OutcomeStatus::InternalError,
                                message: format!("卸载失败，安装记录已保留，可重试：{e}"),
                                requires_reload: false,
                                requires_restart: false,
                            };
                        }
                        registry.remove(&repo_key);
                        if let Err(e) = registry.save() {
                            return ActionOutcome {
                                status: OutcomeStatus::InternalError,
                                message: format!(
                                    "插件文件已移除，但安装记录更新失败，请重试卸载：{e}"
                                ),
                                requires_reload: true,
                                requires_restart: false,
                            };
                        }
                        ActionOutcome {
                            status: OutcomeStatus::Success,
                            message: format!(
                                "Uninstalled repo \"{repo_key}\" ({count} plugin(s): {})",
                                plugin_names.join(", ")
                            ),
                            requires_reload: true,
                            requires_restart: false,
                        }
                    }
                }
            }
            PluginsAction::Update { plugin_id } => {
                let registry = echo_agent_core::plugins::InstallRegistry::load();
                let all_repos = registry.list();
                if all_repos.is_empty() {
                    return ActionOutcome {
                        status: OutcomeStatus::NotFound,
                        message: "No installed plugins to update.".into(),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }

                let repos_to_update: Vec<(
                    String,
                    echo_agent_core::plugins::install_registry::InstalledRepo,
                )> = if let Some(ref id) = plugin_id {
                    let name = id.rsplit('/').next().unwrap_or(id);
                    match registry.find_plugin(name) {
                        Some((key, repo, _plugin)) => vec![(key.to_string(), repo.clone())],
                        None => {
                            return ActionOutcome {
                                status: OutcomeStatus::NotFound,
                                message: format!("Plugin \"{name}\" not found."),
                                requires_reload: false,
                                requires_restart: false,
                            };
                        }
                    }
                } else {
                    all_repos
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v.clone()))
                        .collect()
                };

                let mut messages = Vec::new();
                let mut any_updated = false;
                for (key, repo) in &repos_to_update {
                    match echo_agent_core::plugins::git_install::update_repo(
                        key,
                        repo,
                        crate::plugin::marketplace_require_sha(),
                    ) {
                        Ok(status) => {
                            use echo_agent_core::plugins::git_install::UpdateStatus;
                            match status {
                                UpdateStatus::Updated(result) => {
                                    if result.changed {
                                        any_updated = true;
                                        messages.push(format!("{key}: updated"));
                                    } else {
                                        messages.push(format!("{key}: already up to date"));
                                    }
                                }
                                UpdateStatus::Pinned { ref_name } => {
                                    messages.push(format!("{key}: pinned to {ref_name}"));
                                }
                                UpdateStatus::LiveLocal => {
                                    messages.push(format!("{key}: local symlink (already live)"));
                                }
                            }
                        }
                        Err(e) => {
                            messages.push(format!("{key}: update failed: {e}"));
                        }
                    }
                }
                if let Err(e) = registry.save() {
                    tracing::warn!("Failed to save install registry after update: {e}");
                }
                ActionOutcome {
                    status: OutcomeStatus::Success,
                    message: messages.join("\n"),
                    requires_reload: any_updated,
                    requires_restart: false,
                }
            }
            PluginsAction::Add { path } => {
                if path.is_empty() {
                    return ActionOutcome {
                        status: OutcomeStatus::ValidationError,
                        message: "Path is required.".into(),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }
                let resolved = Self::resolve_path(&self.session_info.cwd, &path);
                let path_str = resolved.display().to_string();
                match crate::config::add_plugin_path(&path_str) {
                    Ok(()) => {
                        let mut msg = format!("Added plugin path: {path_str}");
                        if let Some(ref handle) = self.plugin_registry_handle {
                            let reload_msg = self.reload_plugins_impl(handle, false).await;
                            msg.push('\n');
                            msg.push_str(&reload_msg);
                        }
                        ActionOutcome {
                            status: OutcomeStatus::Success,
                            message: msg,
                            requires_reload: false,
                            requires_restart: false,
                        }
                    }
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to add plugin path: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            PluginsAction::Enable { plugin_id } => {
                // Add to enabled list (for project plugins) and remove from disabled list.
                let r1 = crate::config::add_enabled_plugin(&plugin_id);
                let r2 = crate::config::remove_disabled_plugin(&plugin_id);
                match r1.and(r2) {
                    Ok(()) => {
                        if let Some(ref handle) = self.plugin_registry_handle {
                            let reload_msg = self.reload_plugins_impl(handle, false).await;
                            ActionOutcome {
                                status: OutcomeStatus::Success,
                                message: format!("Enabled: {plugin_id}.\n{reload_msg}"),
                                requires_reload: false,
                                requires_restart: false,
                            }
                        } else {
                            ActionOutcome {
                                status: OutcomeStatus::Success,
                                message: format!("Enabled: {plugin_id}. Restart to apply."),
                                requires_reload: false,
                                requires_restart: true,
                            }
                        }
                    }
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to enable plugin: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            PluginsAction::Disable { plugin_id } => {
                // Add to disabled list and remove from enabled list.
                let r1 = crate::config::add_disabled_plugin(&plugin_id);
                let r2 = crate::config::remove_enabled_plugin(&plugin_id);
                match r1.and(r2) {
                    Ok(()) => {
                        if let Some(ref handle) = self.plugin_registry_handle {
                            let reload_msg = self.reload_plugins_impl(handle, false).await;
                            ActionOutcome {
                                status: OutcomeStatus::Success,
                                message: format!("Disabled: {plugin_id}.\n{reload_msg}"),
                                requires_reload: false,
                                requires_restart: false,
                            }
                        } else {
                            ActionOutcome {
                                status: OutcomeStatus::Success,
                                message: format!("Disabled: {plugin_id}. Restart to apply."),
                                requires_reload: false,
                                requires_restart: true,
                            }
                        }
                    }
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to disable plugin: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
            PluginsAction::Remove { path } => {
                if path.is_empty() {
                    return ActionOutcome {
                        status: OutcomeStatus::ValidationError,
                        message: "Path is required.".into(),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }
                let resolved = Self::resolve_path(&self.session_info.cwd, &path);
                let path_str = resolved.display().to_string();
                match crate::config::remove_plugin_path(&path) {
                    Ok(()) => {
                        let mut msg = format!("Removed plugin path: {path_str}");
                        if let Some(ref handle) = self.plugin_registry_handle {
                            let reload_msg = self.reload_plugins_impl(handle, false).await;
                            msg.push('\n');
                            msg.push_str(&reload_msg);
                        }
                        ActionOutcome {
                            status: OutcomeStatus::Success,
                            message: msg,
                            requires_reload: false,
                            requires_restart: false,
                        }
                    }
                    Err(e) => ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Failed to remove plugin path: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    },
                }
            }
        }
    }

    /// Reload hooks mid-session. Re-discovers global and project hooks,
    /// re-evaluates project trust, and re-appends plugin-contributed hooks.
    /// `pub(super)` so the `SessionCommand::ReloadHooks` arm in `run_session`
    /// (the parent module) can invoke it after an interactive folder-trust
    /// grant — same visibility as `apply_plugin_registry_snapshot` below.
    pub(super) async fn reload_hooks_impl(self: &std::sync::Arc<Self>) -> String {
        let git_root = echo_agent_workspace::session::git::find_git_root_from_path(
            std::path::Path::new(&self.session_info.cwd),
        )
        .ok();
        // Reconcile folder-trust so a mid-session /hooks-trust (or --trust) grant
        // is honored on reload, then gate project hook sources on the verdict.
        let cwd = std::path::Path::new(&self.session_info.cwd);
        let is_trusted = crate::agent::folder_trust::resolve_and_record(cwd, None, false);
        // Single load entry point so all vendors (compat and native) and custom
        // hook-paths are handled consistently with the session-startup sites.
        let (mut registry, errors) = crate::util::hooks::discover_hooks(
            git_root.as_deref(),
            &self.rebuild_spec.compat,
            is_trusted,
        );
        for err in &errors {
            tracing::warn!("hook reload error: {err}");
        }
        *self.hook_load_errors.borrow_mut() = errors.iter().map(|e| e.to_string()).collect();
        // Re-append plugin hooks from current plugin registry.
        // Clone the Arc out of the RefCell so the borrow is dropped immediately.
        let plugin_registry_snapshot = self.plugin_registry.borrow().clone();
        if let Some(ref pr) = plugin_registry_snapshot {
            for plugin in pr.active_plugins() {
                if let Some(ref hooks_path) = plugin.hooks_path {
                    let (specs, warnings) =
                        echo_agent_core::plugins::hooks_adapter::parse_plugin_hooks(
                            hooks_path,
                            &plugin.name,
                            &plugin.root_str(),
                            &plugin.data_dir_str(),
                        );
                    for w in &warnings {
                        tracing::warn!("{w}");
                    }
                    registry.append_specs(specs);
                }
                if let Some(ref inline_value) = plugin.inline_hooks {
                    let (specs, warnings) =
                        echo_agent_core::plugins::hooks_adapter::parse_plugin_hooks_from_value(
                            inline_value,
                            &plugin.name,
                            &plugin.root_str(),
                            &plugin.data_dir_str(),
                        );
                    for w in &warnings {
                        tracing::warn!("{w}");
                    }
                    registry.append_specs(specs);
                }
            }
        }
        let hook_count = registry.len();
        {
            let mut reg = self.hook_registry.borrow_mut();
            if registry.is_empty() {
                *reg = None;
            } else {
                *reg = Some(std::sync::Arc::new(registry));
            }
        }
        tracing::info!(hook_count, "hooks reloaded mid-session");

        // Notify pager about hooks change.
        // Extract all RefCell borrows into locals before the .await so
        // no Ref guard is alive across the suspension point.
        {
            use crate::extensions::hooks::hook_spec_to_info;
            let hooks = {
                let reg = self.hook_registry.borrow();
                match &*reg {
                    Some(registry) => registry
                        .all_hooks()
                        .iter()
                        .map(|s| hook_spec_to_info(s))
                        .collect(),
                    None => Vec::new(),
                }
            };
            let load_errors = self.hook_load_errors.borrow().clone();
            let project_trusted = is_trusted;
            self.send_echo_agent_notification(EchoAgentSessionUpdate::HooksChanged {
                hooks,
                project_trusted,
                load_errors,
            })
            .await;
        }
        format!("Hooks reloaded: {hook_count} hook(s) loaded.")
    }

    /// Shared plugin reload logic used by enable/disable/add/remove and the
    /// explicit `/plugins reload` command.
    ///
    /// Re-reads plugin config from disk, rebuilds the registry, reloads hooks,
    /// and returns a human-readable status message. `force` is `true` only for the
    /// explicit `/plugins reload` (full local-install re-copy); incidental toggles
    /// pass `false` for the cheap skip-unchanged path.
    pub(super) async fn reload_plugins_impl(
        self: &Arc<Self>,
        handle: &echo_agent_core::plugins::SharedPluginRegistryHandle,
        force: bool,
    ) -> String {
        let session_cwd = std::path::Path::new(&self.session_info.cwd);

        let sid = self.session_info.id.0.as_ref();
        echo_agent_telemetry::unified_log::info("reload_plugins_impl: start", Some(sid), None);

        // Folder-trust gates repo-local project plugins (hooks/MCP). Resolve and
        // record the verdict for this cwd BEFORE the plugins-config read below,
        // whose project-paths merge reads the gate — same site ordering as
        // commands/list and the fan-out, so no gate read ever precedes the
        // site's own resolve. Pure verdict: the session-start hook load already
        // printed the folder-untrusted notice, so don't print a second.
        let project_trusted =
            crate::agent::folder_trust::resolve_and_record(session_cwd, None, false);

        let t0 = std::time::Instant::now();
        // Resolve effective [plugins] config (global + ancestor project
        // configs + compat merge). Shared with commands/list and the eager
        // fan-out so all paths discover the same plugins for this cwd.
        let plugins_cfg = crate::config::resolve_effective_plugins_config(session_cwd);
        let config_read_ms = t0.elapsed().as_millis();

        let t2 = std::time::Instant::now();
        let discovery_config = plugins_cfg.to_discovery_config();
        let count = handle.reload(Some(session_cwd), &discovery_config, project_trusted, force);
        let discover_ms = t2.elapsed().as_millis();

        echo_agent_telemetry::unified_log::info(
            "reload_plugins_impl: discovery done",
            Some(sid),
            Some(serde_json::json!({
                "config_read_ms": config_read_ms as u64,
                "discover_ms": discover_ms as u64,
                "total_ms": t0.elapsed().as_millis() as u64,
                "plugin_count": count,
            })),
        );

        // Adopt the freshly-rebuilt snapshot into this session (hooks, MCP,
        // skills, client slash-command catalog). Sessions with `_meta.pluginDirs`
        // rebuild their own view instead — the shared snapshot never carries them.
        let session_dirs = self.session_plugin_dirs();
        let new_registry_snapshot = if session_dirs.is_empty() {
            handle.snapshot()
        } else {
            handle.build_for_cwd(
                session_cwd,
                &discovery_config,
                &session_dirs,
                project_trusted,
            )
        };
        let (hooks_reloaded, mcp_changed, skill_count) = self
            .apply_plugin_registry_snapshot(new_registry_snapshot)
            .await;

        let mcp_status = if mcp_changed {
            "MCP refreshed"
        } else {
            "MCP unchanged"
        };
        format!(
            "Plugin registry rebuilt: {count} plugin(s), {hooks_reloaded} hook(s) reloaded, \
             {mcp_status}, {skill_count} skill(s) refreshed."
        )
    }

    /// This session's `_meta.pluginDirs`, recovered from the registry it was
    /// built with; empty when the session has none.
    pub(crate) fn session_plugin_dirs(&self) -> Vec<std::path::PathBuf> {
        self.plugin_registry
            .borrow()
            .as_ref()
            .map(|r| r.session_plugin_dirs().to_vec())
            .unwrap_or_default()
    }

    /// Re-merge this session's `_meta.pluginDirs` into a registry rebuilt by a
    /// process-wide fan-out (which knows nothing about per-session dirs).
    pub(crate) fn preserve_session_plugin_dirs(
        &self,
        incoming: Option<std::sync::Arc<echo_agent_core::plugins::PluginRegistry>>,
    ) -> Option<std::sync::Arc<echo_agent_core::plugins::PluginRegistry>> {
        let dirs = self.session_plugin_dirs();
        if dirs.is_empty() {
            return incoming;
        }
        let Some(handle) = self.plugin_registry_handle.as_ref() else {
            return incoming;
        };
        let session_cwd = std::path::Path::new(&self.session_info.cwd);
        let disk_cfg =
            crate::config::resolve_effective_plugins_config(session_cwd).to_discovery_config();
        // Pure verdict read: the session's spawn resolve already recorded this
        // cwd with the real remote.
        let project_trusted = crate::agent::folder_trust::project_scope_allowed(session_cwd);
        handle.build_for_cwd(session_cwd, &disk_cfg, &dirs, project_trusted)
    }

    /// Apply a pre-built plugin registry snapshot to this session: swap the
    /// per-session registry, reload plugin hooks, re-merge plugin MCP servers,
    /// re-scan skills, and notify the client. Shared by `reload_plugins_impl`
    /// (the originating session) and the `ReloadPlugins` command (the agent's
    /// eager fan-out to other live sessions when plugins change elsewhere).
    /// Returns `(hooks_reloaded, mcp_changed, skill_count)`.
    pub(super) async fn apply_plugin_registry_snapshot(
        self: &Arc<Self>,
        new_registry_snapshot: Option<std::sync::Arc<echo_agent_core::plugins::PluginRegistry>>,
    ) -> (usize, bool, usize) {
        let sid = self.session_info.id.0.as_ref();
        let session_cwd = std::path::Path::new(&self.session_info.cwd);

        // Update session's plugin registry snapshot
        *self.plugin_registry.borrow_mut() = new_registry_snapshot.clone();

        // Reload hooks in the current session
        let t_hooks = std::time::Instant::now();
        let mut hooks_reloaded = 0usize;
        if let Some(ref new_registry) = new_registry_snapshot {
            let mut new_specs = Vec::new();
            for plugin in new_registry.active_plugins() {
                // File-based hooks
                if let Some(ref hooks_path) = plugin.hooks_path {
                    let (specs, warnings) =
                        echo_agent_core::plugins::hooks_adapter::parse_plugin_hooks(
                            hooks_path,
                            &plugin.name,
                            &plugin.root_str(),
                            &plugin.data_dir_str(),
                        );
                    for w in &warnings {
                        tracing::warn!("{w}");
                    }
                    new_specs.extend(specs);
                }
                // Inline hooks
                if let Some(ref inline_value) = plugin.inline_hooks {
                    let (specs, warnings) =
                        echo_agent_core::plugins::hooks_adapter::parse_plugin_hooks_from_value(
                            inline_value,
                            &plugin.name,
                            &plugin.root_str(),
                            &plugin.data_dir_str(),
                        );
                    for w in &warnings {
                        tracing::warn!("{w}");
                    }
                    new_specs.extend(specs);
                }
            }
            hooks_reloaded = new_specs.len();
            {
                let mut reg = self.hook_registry.borrow_mut();
                if let Some(ref mut arc_reg) = *reg {
                    let hook_reg = Arc::make_mut(arc_reg);
                    hook_reg.remove_by_prefix("plugin/");
                    hook_reg.append_specs(new_specs);
                } else if !new_specs.is_empty() {
                    // No registry yet: bootstrap config-layer and file hooks (as
                    // reload_hooks_impl does), not empty sources, so a plugin-first
                    // snapshot doesn't drop config hooks.
                    let git_root =
                        echo_agent_workspace::session::git::find_git_root_from_path(session_cwd)
                            .ok();
                    let is_trusted =
                        crate::agent::folder_trust::resolve_and_record(session_cwd, None, false);
                    let (mut new_reg, _errs) = crate::util::hooks::discover_hooks(
                        git_root.as_deref(),
                        &self.rebuild_spec.compat,
                        is_trusted,
                    );
                    new_reg.append_specs(new_specs);
                    *reg = Some(Arc::new(new_reg));
                }
            }
        }

        echo_agent_telemetry::unified_log::info(
            "reload_plugins_impl: hooks done",
            Some(sid),
            Some(serde_json::json!({
                "hooks_reload_ms": t_hooks.elapsed().as_millis() as u64,
                "hooks_reloaded": hooks_reloaded,
            })),
        );

        // Always re-merge plugin-contributed MCP servers and apply via an
        // order-insensitive diff: unchanged servers stay connected, and only
        // added/changed/removed ones are re-initialized. Merging
        // unconditionally (no "plugins have MCP" guard) lets a removed
        // plugin's server tear down cleanly; the diff keeps it a no-op when
        // the effective set is unchanged, avoiding the spurious full teardown
        // the order-sensitive `update_configs` would cause (merge order is
        // non-deterministic). Mirrors the `UpdateMcpServers` command handler.
        let t_mcp = std::time::Instant::now();
        let new_mcp_servers = crate::session::managed_mcp::merge_managed_mcp_servers(
            self.initial_client_mcp_servers.clone(),
            session_cwd,
            new_registry_snapshot.as_deref(),
            &self.rebuild_spec.compat,
        );
        let new_mcp_servers = crate::session::managed_mcp::resolve_agent_mcp_servers(
            new_mcp_servers,
            self.agent.borrow().definition(),
            session_cwd,
        );
        let (mcp_diff, dispatch_event_tx) = {
            let mut mcp_state = self.mcp_state.lock().await;
            let diff = mcp_state.update_configs_diff(new_mcp_servers);
            let tx = mcp_state.client_event_tx();
            (diff, tx)
        };
        let mcp_changed = if let Some(diff) = mcp_diff {
            if (!diff.added.is_empty() || !diff.removed.is_empty())
                && let Some(tx) = &dispatch_event_tx
            {
                let _ = tx.send(echo_agent_mcp::servers::McpClientEvent::ConfigDiff {
                    added: diff.added.clone(),
                    removed: diff.removed.clone(),
                });
            }
            for name in &diff.removed {
                let prefix = format!(
                    "{}{}",
                    name,
                    crate::session::mcp_servers::MCP_TOOL_NAME_DELIMITER
                );
                let removed_count = self
                    .agent
                    .borrow()
                    .tool_bridge()
                    .unregister_tools_by_prefix(&prefix);
                tracing::info!(
                    server = name.as_str(),
                    tools_removed = removed_count,
                    "Unregistered tools for removed MCP server (plugin reload)"
                );
            }
            self.ensure_mcp_tools_initialized().await;
            true
        } else {
            false
        };

        echo_agent_telemetry::unified_log::info(
            "reload_plugins_impl: MCP done",
            Some(sid),
            Some(serde_json::json!({
                "mcp_merge_ms": t_mcp.elapsed().as_millis() as u64,
                "mcp_changed": mcp_changed,
            })),
        );

        // Refresh skills: re-scan from disk using the (already-updated) plugin registry.
        let t_skills = std::time::Instant::now();
        let skill_count = self.reload_skills_from_disk().await;
        echo_agent_telemetry::unified_log::info(
            "reload_plugins_impl: skills done",
            Some(sid),
            Some(serde_json::json!({
                "skills_ms": t_skills.elapsed().as_millis() as u64,
                "skill_count": skill_count,
            })),
        );

        // Notify pager about registry changes so the modal auto-refreshes.
        // Extract all RefCell borrows into locals before the .await so
        // no Ref guard is alive across the suspension point (prevents
        // BorrowMutError panics when send_echo_agent_notification dispatches
        // Notification hooks that also borrow these RefCells).
        let t_notify = std::time::Instant::now();
        {
            use crate::extensions::hooks::hook_spec_to_info;

            let hooks = {
                let reg = self.hook_registry.borrow();
                match &*reg {
                    Some(registry) => registry
                        .all_hooks()
                        .iter()
                        .map(|s| hook_spec_to_info(s))
                        .collect(),
                    None => Vec::new(),
                }
            };
            let load_errors = self.hook_load_errors.borrow().clone();
            // Report the folder-trust verdict so the flag matches the gated registry.
            let project_trusted = crate::agent::folder_trust::project_scope_allowed(
                std::path::Path::new(&self.session_info.cwd),
            );
            self.send_echo_agent_notification(EchoAgentSessionUpdate::HooksChanged {
                hooks,
                project_trusted,
                load_errors,
            })
            .await;

            use crate::extensions::plugins::loaded_plugin_to_info;
            let plugins = {
                let reg = self.plugin_registry.borrow();
                match &*reg {
                    Some(registry) => registry
                        .list()
                        .iter()
                        .map(|p| loaded_plugin_to_info(p))
                        .collect(),
                    None => Vec::new(),
                }
            };
            self.send_echo_agent_notification(EchoAgentSessionUpdate::PluginsChanged { plugins })
                .await;
        }

        echo_agent_telemetry::unified_log::info(
            "apply_plugin_registry_snapshot: complete",
            Some(sid),
            Some(serde_json::json!({
                "notify_ms": t_notify.elapsed().as_millis() as u64,
                "hooks_reloaded": hooks_reloaded,
                "mcp_changed": mcp_changed,
                "skill_count": skill_count,
            })),
        );

        (hooks_reloaded, mcp_changed, skill_count)
    }
}
