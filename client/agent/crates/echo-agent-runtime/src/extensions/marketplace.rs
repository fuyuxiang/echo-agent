//! `echo.agent/marketplace/*` extension handlers.
//!
//! Provides marketplace browsing and install endpoints for the pager modal.
//! Delegates to `echo-agent-plugin-marketplace` crate for scanning and install logic.

use agent_client_protocol as acp;
use echo_agent_hooks_plugins_types::{
    MarketplaceAction, MarketplaceActionRequest, MarketplaceListResponse, MarketplacePluginEntry,
    MarketplaceScanResult,
};

use crate::agent::MvpAgent;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

fn load_mutable_marketplace_registry() -> Result<
    echo_agent_core::plugins::install_registry::InstallRegistry,
    echo_agent_hooks_plugins_types::ActionOutcome,
> {
    use echo_agent_core::plugins::install_registry::InstallRegistry;
    let mut registry = InstallRegistry::try_load_from(InstallRegistry::resolve_install_dir())
        .map_err(|error| echo_agent_hooks_plugins_types::ActionOutcome {
            status: echo_agent_hooks_plugins_types::OutcomeStatus::InternalError,
            message: format!("Failed to read install registry: {error}"),
            requires_reload: false,
            requires_restart: false,
        })?;
    crate::plugin::freeze_revoked_registry_provenance(&mut registry).map_err(|error| {
        echo_agent_hooks_plugins_types::ActionOutcome {
            status: echo_agent_hooks_plugins_types::OutcomeStatus::InternalError,
            message: format!("Failed to preserve revoked source ownership: {error}"),
            requires_reload: false,
            requires_restart: false,
        }
    })?;
    Ok(registry)
}

fn load_filtered_marketplace_sources() -> Vec<echo_agent_plugin_marketplace::MarketplaceSource> {
    crate::plugin::load_filtered_marketplace_sources()
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/marketplace/list" => handle_list().await,
        "echo.agent/marketplace/action" => handle_action(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_list() -> ExtResult {
    let t0 = std::time::Instant::now();
    let sources = load_filtered_marketplace_sources();
    let source_names: Vec<String> = sources
        .iter()
        .map(|s| {
            let url = match &s.kind {
                echo_agent_plugin_marketplace::SourceKind::Git { url, .. } => url.as_str(),
                echo_agent_plugin_marketplace::SourceKind::Local { path } => {
                    path.to_str().unwrap_or("?")
                }
            };
            format!("{}={}", s.name, url)
        })
        .collect();
    echo_agent_telemetry::unified_log::info(
        "marketplace handle_list: sources loaded",
        None,
        Some(serde_json::json!({
            "source_count": sources.len(),
            "sources": source_names,
            "load_sources_ms": t0.elapsed().as_millis() as u64,
        })),
    );

    // Scan all sources in parallel using blocking tasks (git operations are sync).
    let scan_handles: Vec<_> = sources
        .iter()
        .map(|source| {
            let source = source.clone();
            tokio::task::spawn_blocking(move || scan_source(&source))
        })
        .collect();
    let mut results = Vec::with_capacity(scan_handles.len());
    for (i, handle) in scan_handles.into_iter().enumerate() {
        let (scan, catalog_loaded) = handle.await.unwrap_or_else(|e| {
            (
                MarketplaceScanResult {
                    source_name: sources[i].name.clone(),
                    source_kind: String::new(),
                    source_url_or_path: String::new(),
                    plugins: Vec::new(),
                    error: Some(format!("scan task failed: {e}")),
                },
                false,
            )
        });
        let components_present = scan
            .plugins
            .iter()
            .filter(|p| p.components.is_some())
            .count();
        echo_agent_telemetry::unified_log::info(
            "marketplace handle_list: source scanned",
            None,
            Some(serde_json::json!({
                "source_index": i,
                "source_name": sources[i].name,
                "scan_ms": 0, // individual timing not available in parallel mode
                "plugin_count": scan.plugins.len(),
                "catalog_loaded": catalog_loaded,
                "components_present": components_present,
                "components_absent": scan.plugins.len() - components_present,
                "error": scan.error,
            })),
        );
        results.push(scan);
    }

    echo_agent_telemetry::unified_log::info(
        "marketplace handle_list: complete",
        None,
        Some(serde_json::json!({
            "total_ms": t0.elapsed().as_millis() as u64,
        })),
    );

    let response = MarketplaceListResponse { sources: results };
    super::to_ext_response(Ok(response))
}

async fn handle_action(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: MarketplaceActionRequest = super::parse_params(args)?;
    let sid = acp::SessionId::new(req.session_id);

    let outcome = match req.action {
        MarketplaceAction::Refresh { source_url_or_path } => {
            // Force re-sync git caches (local sources are re-scanned on next
            // list). Runs on the blocking pool: git clone/fetch is sync and
            // can stall for up to its timeout — never run it on the LocalSet.
            let sources = load_filtered_marketplace_sources();
            let filter = source_url_or_path;
            match tokio::task::spawn_blocking(move || refresh_sources(&sources, filter.as_deref()))
                .await
            {
                Ok(outcome) => outcome,
                Err(e) => echo_agent_hooks_plugins_types::ActionOutcome {
                    status: echo_agent_hooks_plugins_types::OutcomeStatus::InternalError,
                    message: format!("Refresh task failed: {e}"),
                    requires_reload: false,
                    requires_restart: false,
                },
            }
        }
        MarketplaceAction::Install {
            source_url_or_path,
            plugin_relative_path,
        } => {
            let outcome = handle_install(&source_url_or_path, &plugin_relative_path).await;
            reload_after_plugin_change(agent, &sid, outcome).await
        }
        MarketplaceAction::Update {
            source_url_or_path,
            plugin_relative_path,
        } => {
            let outcome = handle_update(&source_url_or_path, &plugin_relative_path).await;
            reload_after_plugin_change(agent, &sid, outcome).await
        }
        MarketplaceAction::Uninstall {
            source_url_or_path,
            plugin_relative_path,
        } => {
            let outcome = handle_uninstall(&source_url_or_path, &plugin_relative_path).await;
            reload_after_plugin_change(agent, &sid, outcome).await
        }
        MarketplaceAction::AddSource { url } => {
            let outcome = handle_add_source(&url).await;
            reload_after_plugin_change(agent, &sid, outcome).await
        }
        MarketplaceAction::RemoveSource { source_url_or_path } => {
            let outcome = handle_remove_source(&source_url_or_path).await;
            reload_after_plugin_change(agent, &sid, outcome).await
        }
    };

    super::to_ext_response(Ok(outcome))
}

async fn reload_after_plugin_change(
    agent: &MvpAgent,
    sid: &acp::SessionId,
    outcome: echo_agent_hooks_plugins_types::ActionOutcome,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::PluginsAction;
    if !outcome.requires_reload {
        return outcome;
    }
    let reload = agent
        .execute_plugins_action(sid, PluginsAction::Reload)
        .await;
    complete_plugin_reload(outcome, reload)
}

fn complete_plugin_reload(
    mut outcome: echo_agent_hooks_plugins_types::ActionOutcome,
    reload: Option<echo_agent_hooks_plugins_types::ActionOutcome>,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    match reload {
        Some(reload) if reload.status == OutcomeStatus::Success => {
            outcome.requires_reload = false;
        }
        reload => {
            let reason = reload
                .map(|value| value.message)
                .unwrap_or_else(|| "session unavailable".into());
            outcome.status = OutcomeStatus::InternalError;
            outcome.message.push_str(&format!("\nThe change was saved, but live plugins could not reload: {reason}. Run /plugins reload or restart the application."));
            outcome.requires_restart = true;
        }
    }
    outcome
}

fn plugin_setup_outcome(
    message: String,
    warnings: &[String],
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
    ActionOutcome {
        status: if warnings.is_empty() {
            OutcomeStatus::Success
        } else {
            OutcomeStatus::InternalError
        },
        message: if warnings.is_empty() {
            message
        } else {
            format!(
                "{message}\nPlugin files were saved, but setup is incomplete: {}. Enable the plugin in Plugins or retry Update.",
                warnings.join("; ")
            )
        },
        requires_reload: true,
        requires_restart: false,
    }
}

fn refresh_sources(
    sources: &[echo_agent_plugin_marketplace::MarketplaceSource],
    source_url_or_path: Option<&str>,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    refresh_sources_with(sources, source_url_or_path, |url, branch| {
        let cache_root = echo_agent_plugin_marketplace::git::default_cache_root();
        echo_agent_plugin_marketplace::git::force_sync_source_cache(url, branch, &cache_root)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

fn refresh_sources_with(
    sources: &[echo_agent_plugin_marketplace::MarketplaceSource],
    source_url_or_path: Option<&str>,
    mut sync_git: impl FnMut(&str, Option<&str>) -> Result<(), String>,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut selected = 0;
    let mut refreshed = 0;
    let mut errors = Vec::new();
    for source in sources {
        if let Some(filter) = source_url_or_path {
            let identity = match &source.kind {
                echo_agent_plugin_marketplace::SourceKind::Local { path } => {
                    path.display().to_string()
                }
                echo_agent_plugin_marketplace::SourceKind::Git { url, .. } => url.clone(),
            };
            if identity != filter {
                continue;
            }
        }
        selected += 1;
        let result = match &source.kind {
            echo_agent_plugin_marketplace::SourceKind::Git { url, branch } => {
                sync_git(url, branch.as_deref())
            }
            echo_agent_plugin_marketplace::SourceKind::Local { path } => {
                std::fs::read_dir(path).map(|_| ()).map_err(|error| {
                    format!(
                        "Cannot read local marketplace directory {}: {error}",
                        path.display()
                    )
                })
            }
        };
        match result {
            Ok(()) => refreshed += 1,
            Err(error) => errors.push(format!("{}: {error}", source.name)),
        }
    }

    let (status, msg) = if selected == 0 && source_url_or_path.is_some() {
        (
            OutcomeStatus::NotFound,
            format!(
                "Marketplace source not found: {}",
                source_url_or_path.unwrap_or_default()
            ),
        )
    } else if errors.is_empty() {
        (
            OutcomeStatus::Success,
            format!("Refreshed {refreshed} source(s)."),
        )
    } else {
        (
            OutcomeStatus::InternalError,
            format!(
                "Refreshed {refreshed} of {selected} source(s); {} failed: {}. Check the source configuration and retry Refresh.",
                errors.len(),
                errors.join("; ")
            ),
        )
    };
    echo_agent_hooks_plugins_types::ActionOutcome {
        status,
        message: msg,
        requires_reload: false,
        requires_restart: false,
    }
}

async fn handle_update(
    source_url_or_path: &str,
    plugin_relative_path: &str,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
    use echo_agent_plugin_marketplace::installer;

    let sources = load_filtered_marketplace_sources();

    let source_identity = |s: &echo_agent_plugin_marketplace::MarketplaceSource| -> String {
        match &s.kind {
            echo_agent_plugin_marketplace::SourceKind::Local { path } => path.display().to_string(),
            echo_agent_plugin_marketplace::SourceKind::Git { url, .. } => url.clone(),
        }
    };

    let source = match sources
        .iter()
        .find(|s| source_identity(s) == source_url_or_path)
    {
        Some(s) => s,
        None => {
            return ActionOutcome {
                status: OutcomeStatus::NotFound,
                message: format!("Marketplace source not found: {source_url_or_path}"),
                requires_reload: false,
                requires_restart: false,
            };
        }
    };

    let plugin_path =
        match echo_agent_plugin_marketplace::MarketplaceRelativePath::parse(plugin_relative_path) {
            Ok(path) => path,
            Err(e) => {
                return ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: format!("Invalid plugin path: {e}"),
                    requires_reload: false,
                    requires_restart: false,
                };
            }
        };
    let plugin_relative_path = plugin_path.as_str();

    let marketplace_lease;
    let marketplace_root = match &source.kind {
        echo_agent_plugin_marketplace::SourceKind::Local { path } => {
            marketplace_lease = None;
            path.clone()
        }
        echo_agent_plugin_marketplace::SourceKind::Git { url, branch } => {
            let cache_root = echo_agent_plugin_marketplace::git::default_cache_root();
            match echo_agent_plugin_marketplace::git::sync_source_cache_with_mode(
                url,
                branch.as_deref(),
                &cache_root,
                echo_agent_plugin_marketplace::git::SyncMode::Force,
            ) {
                Ok(lease) => {
                    let path = lease.path.clone();
                    marketplace_lease = Some(lease);
                    path
                }
                Err(e) => {
                    return ActionOutcome {
                        status: OutcomeStatus::InternalError,
                        message: format!("Git sync failed: {e}"),
                        requires_reload: false,
                        requires_restart: false,
                    };
                }
            }
        }
    };

    let scan = echo_agent_plugin_marketplace::scan_marketplace(&marketplace_root);
    let entry = match scan
        .entries
        .into_iter()
        .find(|entry| entry.relative_path == plugin_relative_path)
    {
        Some(entry) => entry,
        None => {
            return ActionOutcome {
                status: OutcomeStatus::NotFound,
                message: format!("Marketplace plugin not found: {plugin_relative_path}"),
                requires_reload: false,
                requires_restart: false,
            };
        }
    };

    let provenance = echo_agent_core::plugins::install_registry::MarketplaceProvenance {
        source_url_or_path: source_url_or_path.to_string(),
        canonical_source_identity: Some(
            echo_agent_core::plugins::marketplace_scope::source_identity(source_url_or_path),
        ),
        source_display_name: source.name.clone(),
        plugin_subdir: plugin_relative_path.to_string(),
    };
    let mut registry = match load_mutable_marketplace_registry() {
        Ok(registry) => registry,
        Err(outcome) => return outcome,
    };
    let require_sha = crate::plugin::marketplace_require_sha();
    let update_result = installer::update_from_marketplace_entry_transactional(
        &marketplace_root,
        &entry,
        provenance,
        &mut registry,
        require_sha,
    );
    drop(marketplace_lease);

    match update_result {
        Ok(result) => {
            let (_, post_warnings) = crate::config::post_install_plugin(&result.repo_key);
            for w in &post_warnings {
                tracing::warn!("{w}");
            }
            plugin_setup_outcome(
                format!(
                    "Updated {} ({} -> {})",
                    result.repo_key,
                    result.old_version.as_deref().unwrap_or("?"),
                    result.new_version.as_deref().unwrap_or("?")
                ),
                &post_warnings,
            )
        }
        Err(echo_agent_core::plugins::install_registry::InstallError::PluginNotFound {
            ..
        }) => ActionOutcome {
            status: OutcomeStatus::NotFound,
            message: format!(
                "Plugin not installed from this marketplace source: {plugin_relative_path}"
            ),
            requires_reload: false,
            requires_restart: false,
        },
        Err(e) => ActionOutcome {
            status: OutcomeStatus::InternalError,
            message: format!("Update failed: {e}"),
            requires_reload: false,
            requires_restart: false,
        },
    }
}
async fn handle_install(
    source_url_or_path: &str,
    plugin_relative_path: &str,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
    use echo_agent_plugin_marketplace::installer;

    let sources = load_filtered_marketplace_sources();

    // Helper: get canonical URL/path for a source.
    let source_identity = |s: &echo_agent_plugin_marketplace::MarketplaceSource| -> String {
        match &s.kind {
            echo_agent_plugin_marketplace::SourceKind::Local { path } => path.display().to_string(),
            echo_agent_plugin_marketplace::SourceKind::Git { url, .. } => url.clone(),
        }
    };

    let source = match sources
        .iter()
        .find(|s| source_identity(s) == source_url_or_path)
    {
        Some(s) => s,
        None => {
            return ActionOutcome {
                status: OutcomeStatus::NotFound,
                message: format!("Marketplace source not found: {source_url_or_path}"),
                requires_reload: false,
                requires_restart: false,
            };
        }
    };

    // Check if this is a URL-sourced plugin by scanning the marketplace index.
    let (scan, _) = scan_source(source);
    let remote_entry = scan
        .plugins
        .iter()
        .find(|p| p.relative_path == plugin_relative_path)
        .and_then(|p| {
            p.remote_url.as_ref().map(|url| {
                (
                    url.clone(),
                    p.remote_ref.clone(),
                    p.remote_sha.clone(),
                    p.remote_subdir.clone(),
                )
            })
        });

    if let Some((remote_url, remote_ref, remote_sha, remote_subdir)) = remote_entry {
        // URL-sourced plugin: clone from remote git URL.
        let provenance = echo_agent_core::plugins::install_registry::MarketplaceProvenance {
            source_url_or_path: source_url_or_path.to_string(),
            canonical_source_identity: Some(
                echo_agent_core::plugins::marketplace_scope::source_identity(source_url_or_path),
            ),
            source_display_name: source.name.clone(),
            plugin_subdir: plugin_relative_path.to_string(),
        };
        let mut registry = match load_mutable_marketplace_registry() {
            Ok(registry) => registry,
            Err(outcome) => return outcome,
        };
        let require_sha = crate::plugin::marketplace_require_sha();
        match installer::install_from_remote_url(
            &remote_url,
            remote_ref.as_deref(),
            remote_sha.as_deref(),
            remote_subdir.as_deref(),
            plugin_relative_path,
            provenance,
            &mut registry,
            require_sha,
        ) {
            Ok(installer::MarketplaceInstallResult::Installed { repo_key }) => {
                // Auto-enable installed plugin so it's active after reload.
                let (_, post_warnings) = crate::config::post_install_plugin(&repo_key);
                for w in &post_warnings {
                    tracing::warn!("{w}");
                }
                plugin_setup_outcome(
                    format!(
                        "Installed from {}: {plugin_relative_path} (key: {repo_key})",
                        source.name,
                    ),
                    &post_warnings,
                )
            }
            Ok(installer::MarketplaceInstallResult::AlreadyInstalled { repo_key }) => {
                ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: format!(
                        "Already installed (key: {repo_key}). Use Update to reinstall."
                    ),
                    requires_reload: false,
                    requires_restart: false,
                }
            }
            Err(e) => ActionOutcome {
                status: OutcomeStatus::InternalError,
                message: format!("Install failed: {e}"),
                requires_reload: false,
                requires_restart: false,
            },
        }
    } else {
        // Local-sourced plugin: resolve from marketplace directory.
        let marketplace_lease;
        let marketplace_root = match &source.kind {
            echo_agent_plugin_marketplace::SourceKind::Local { path } => {
                marketplace_lease = None;
                path.clone()
            }
            echo_agent_plugin_marketplace::SourceKind::Git { url, branch } => {
                let cache_root = echo_agent_plugin_marketplace::git::default_cache_root();
                match echo_agent_plugin_marketplace::git::sync_source_cache_with_mode(
                    url,
                    branch.as_deref(),
                    &cache_root,
                    echo_agent_plugin_marketplace::git::SyncMode::UseTtl,
                ) {
                    Ok(lease) => {
                        let cached_path = lease.path.clone();
                        marketplace_lease = Some(lease);
                        cached_path
                    }
                    Err(e) => {
                        return ActionOutcome {
                            status: OutcomeStatus::InternalError,
                            message: format!("Git sync failed: {e}"),
                            requires_reload: false,
                            requires_restart: false,
                        };
                    }
                }
            }
        };

        let plugin_path = match echo_agent_plugin_marketplace::MarketplaceRelativePath::parse(
            plugin_relative_path,
        ) {
            Ok(path) => path,
            Err(e) => {
                return ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: format!("Invalid plugin path: {e}"),
                    requires_reload: false,
                    requires_restart: false,
                };
            }
        };
        let plugin_dir = match plugin_path.join_under(&marketplace_root) {
            Ok(path) => path,
            Err(e) => {
                return ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: format!("Invalid plugin path: {e}"),
                    requires_reload: false,
                    requires_restart: false,
                };
            }
        };
        if !plugin_dir.is_dir() {
            return ActionOutcome {
                status: OutcomeStatus::NotFound,
                message: format!("Plugin directory not found: {}", plugin_dir.display()),
                requires_reload: false,
                requires_restart: false,
            };
        };
        let plugin_relative_path = plugin_path.as_str();

        let provenance = echo_agent_core::plugins::install_registry::MarketplaceProvenance {
            source_url_or_path: source_url_or_path.to_string(),
            canonical_source_identity: Some(
                echo_agent_core::plugins::marketplace_scope::source_identity(source_url_or_path),
            ),
            source_display_name: source.name.clone(),
            plugin_subdir: plugin_relative_path.to_string(),
        };

        let mut registry = match load_mutable_marketplace_registry() {
            Ok(registry) => registry,
            Err(outcome) => return outcome,
        };
        let install_result = installer::install_from_marketplace(
            &marketplace_root,
            plugin_relative_path,
            provenance,
            &mut registry,
        );
        drop(marketplace_lease);
        match install_result {
            Ok(installer::MarketplaceInstallResult::Installed { repo_key }) => {
                // Auto-enable installed plugin so it's active after reload.
                let (_, post_warnings) = crate::config::post_install_plugin(&repo_key);
                for w in &post_warnings {
                    tracing::warn!("{w}");
                }
                plugin_setup_outcome(
                    format!(
                        "Installed from {}: {plugin_relative_path} (key: {repo_key})",
                        source.name,
                    ),
                    &post_warnings,
                )
            }
            Ok(installer::MarketplaceInstallResult::AlreadyInstalled { repo_key }) => {
                ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: format!(
                        "Already installed (key: {repo_key}). Use Update to reinstall."
                    ),
                    requires_reload: false,
                    requires_restart: false,
                }
            }
            Err(e) => ActionOutcome {
                status: OutcomeStatus::InternalError,
                message: format!("Install failed: {e}"),
                requires_reload: false,
                requires_restart: false,
            },
        }
    }
}

async fn handle_uninstall(
    source_url_or_path: &str,
    plugin_relative_path: &str,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
    use echo_agent_plugin_marketplace::installer;

    let mut registry = match load_mutable_marketplace_registry() {
        Ok(registry) => registry,
        Err(outcome) => return outcome,
    };

    // Find the installed entry by marketplace provenance.
    let found = installer::find_installed_marketplace_plugin(
        &registry,
        source_url_or_path,
        plugin_relative_path,
    );

    let repo_key = match found {
        Some((key, _version)) => key,
        None => {
            return ActionOutcome {
                status: OutcomeStatus::NotFound,
                message: format!("Plugin not installed from marketplace: {plugin_relative_path}"),
                requires_reload: false,
                requires_restart: false,
            };
        }
    };

    // Delete the installed plugin directory.
    let plugin_dir = registry.install_dir().join(&repo_key);
    if plugin_dir.is_dir()
        && let Err(e) = std::fs::remove_dir_all(&plugin_dir)
    {
        return ActionOutcome {
            status: OutcomeStatus::InternalError,
            message: format!("Failed to remove plugin directory: {e}"),
            requires_reload: false,
            requires_restart: false,
        };
    }

    // Remove from registry and save.
    registry.remove(&repo_key);
    if let Err(e) = registry.save() {
        return ActionOutcome {
            status: OutcomeStatus::InternalError,
            message: format!("Failed to save registry: {e}"),
            requires_reload: false,
            requires_restart: false,
        };
    }

    ActionOutcome {
        status: OutcomeStatus::Success,
        message: format!("Uninstalled {plugin_relative_path} (key: {repo_key})"),
        requires_reload: true,
        requires_restart: false,
    }
}

fn scan_source(
    source: &echo_agent_plugin_marketplace::MarketplaceSource,
) -> (MarketplaceScanResult, bool) {
    let lease;
    let (source_kind, source_url_or_path, root) = match &source.kind {
        echo_agent_plugin_marketplace::SourceKind::Local { path } => {
            lease = None;
            (
                "local".to_string(),
                path.display().to_string(),
                Some(path.clone()),
            )
        }
        echo_agent_plugin_marketplace::SourceKind::Git { url, branch } => {
            let cache_root = echo_agent_plugin_marketplace::git::default_cache_root();
            let t_git = std::time::Instant::now();
            match echo_agent_plugin_marketplace::git::sync_source_cache_with_mode(
                url,
                branch.as_deref(),
                &cache_root,
                echo_agent_plugin_marketplace::git::SyncMode::UseTtl,
            ) {
                Ok(cache_lease) => {
                    let cached_path = cache_lease.path.clone();
                    lease = Some(cache_lease);
                    echo_agent_telemetry::unified_log::info(
                        "scan_source: git sync done",
                        None,
                        Some(serde_json::json!({
                            "url": url,
                            "git_sync_ms": t_git.elapsed().as_millis() as u64,
                        })),
                    );
                    ("git".to_string(), url.clone(), Some(cached_path))
                }
                Err(e) => {
                    return (
                        MarketplaceScanResult {
                            source_name: source.name.clone(),
                            source_kind: "git".to_string(),
                            source_url_or_path: url.clone(),
                            plugins: Vec::new(),
                            error: Some(format!("Git sync failed: {e}")),
                        },
                        false,
                    );
                }
            }
        }
    };

    let root = match root {
        Some(r) if r.is_dir() => r,
        _ => {
            return (
                MarketplaceScanResult {
                    source_name: source.name.clone(),
                    source_kind,
                    source_url_or_path: source_url_or_path.clone(),
                    plugins: Vec::new(),
                    error: Some(format!("Directory not found: {source_url_or_path}")),
                },
                false,
            );
        }
    };

    let scan = echo_agent_plugin_marketplace::scan_marketplace(&root);
    let catalog_loaded = scan.catalog_loaded;
    let discovered = scan.entries;
    drop(lease);

    // Cross-reference with install registry.
    let registry = crate::plugin::marketplace_lookup_registry(
        &echo_agent_core::plugins::install_registry::InstallRegistry::load(),
    );
    let plugins = discovered
        .into_iter()
        .map(|p| {
            let (install_status, installed_version) =
                match echo_agent_plugin_marketplace::installer::find_installed_marketplace_plugin(
                    &registry,
                    &source_url_or_path,
                    &p.relative_path,
                ) {
                    Some((_key, ver)) => {
                        if p.version.is_none()
                            || p.version.as_deref() == Some(ver.as_str())
                            || ver.is_empty()
                        {
                            ("installed".to_string(), Some(ver))
                        } else {
                            ("update_available".to_string(), Some(ver))
                        }
                    }
                    None => ("not_installed".to_string(), None),
                };

            to_plugin_entry(p, install_status, installed_version)
        })
        .collect();

    (
        MarketplaceScanResult {
            source_name: source.name.clone(),
            source_kind,
            source_url_or_path,
            plugins,
            error: None,
        },
        catalog_loaded,
    )
}

fn to_plugin_entry(
    p: echo_agent_plugin_marketplace::MarketplaceEntry,
    install_status: String,
    installed_version: Option<String>,
) -> MarketplacePluginEntry {
    MarketplacePluginEntry {
        name: p.name,
        version: p.version,
        description: p.description,
        category: p.category,
        author: p.author,
        tags: p.tags,
        keywords: p.keywords,
        domains: p.domains,
        homepage: p.homepage,
        relative_path: p.relative_path,
        skill_count: p.skill_count,
        has_hooks: p.has_hooks,
        has_agents: p.has_agents,
        has_mcp: p.has_mcp,
        install_status,
        installed_version,
        components: p.components,
        remote_url: p.remote_url,
        remote_ref: p.remote_ref,
        remote_sha: p.remote_sha,
        remote_subdir: p.remote_subdir,
    }
}

/// Add a new git or local-path marketplace source to `~/.echo-agent/config.toml`.
async fn handle_add_source(url: &str) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use crate::plugin::{self, MarketplaceAddInput};
    use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};

    let url = url.trim();
    if url.is_empty() {
        return ActionOutcome {
            status: OutcomeStatus::ValidationError,
            message: "URL cannot be empty.".into(),
            requires_reload: false,
            requires_restart: false,
        };
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let input = plugin::classify_marketplace_add_input(url, &cwd);

    // Fail fast on missing local paths: without this, a path input would be
    // stored as a git URL and only error after network clone attempts.
    if let MarketplaceAddInput::LocalPath(path) = &input
        && !path.is_dir()
    {
        return ActionOutcome {
            status: OutcomeStatus::ValidationError,
            message: format!(
                "Local marketplace path not found (or is not a directory): {}",
                path.display()
            ),
            requires_reload: false,
            requires_restart: false,
        };
    }

    let identity = match &input {
        MarketplaceAddInput::GitUrl(u) => u.clone(),
        MarketplaceAddInput::LocalPath(p) => p.display().to_string(),
    };

    // Local paths never match the git-URL allowlist, so a restricted
    // strictKnownMarketplaces policy blocks them — intentionally fail-closed.
    let allowlist =
        &echo_agent_workspace::permission::resolution::managed_settings().marketplace_allowlist;
    if allowlist.is_restricted() && !allowlist.is_url_allowed(&identity) {
        return ActionOutcome {
            status: OutcomeStatus::ValidationError,
            message: format!("Marketplace source blocked: {}", allowlist.block_reason()),
            requires_reload: false,
            requires_restart: false,
        };
    }

    let config = crate::config::load_effective_config()
        .ok()
        .unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let existing = echo_agent_plugin_marketplace::load_sources(&config);
    let already_configured = match &input {
        MarketplaceAddInput::GitUrl(git_url) => {
            let normalized = git_url.trim_end_matches(".git");
            existing.iter().any(|s| {
                matches!(&s.kind, echo_agent_plugin_marketplace::SourceKind::Git { url: u, .. }
                    if u.trim_end_matches(".git") == normalized)
            })
        }
        MarketplaceAddInput::LocalPath(path) => existing.iter().any(|s| {
            matches!(&s.kind, echo_agent_plugin_marketplace::SourceKind::Local { path: p }
                if p == path)
        }),
    };
    if already_configured
        && !crate::plugin::marketplace_disabled_sources()
            .contains(&echo_agent_core::plugins::marketplace_scope::source_identity(&identity))
    {
        return ActionOutcome {
            status: OutcomeStatus::ValidationError,
            message: format!("Marketplace source already configured: {identity}"),
            requires_reload: false,
            requires_restart: false,
        };
    }

    // Reject URLs that aren't reachable git repos (e.g. MCP endpoints pasted
    // into the wrong tab) before persisting. The probe blocks on a git
    // subprocess, so run it off the LocalSet.
    if let MarketplaceAddInput::GitUrl(git_url) = &input {
        let probe_url = git_url.clone();
        let probe = tokio::task::spawn_blocking(move || {
            echo_agent_plugin_marketplace::git::probe_git_remote(&probe_url)
        })
        .await;
        match probe {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return ActionOutcome {
                    status: OutcomeStatus::ValidationError,
                    message: format!(
                        "{e}. Not a reachable git repository — to add it anyway (e.g. a \
                         VPN-gated host), run: echoagent plugin marketplace add {url} --force"
                    ),
                    requires_reload: false,
                    requires_restart: false,
                };
            }
            Err(e) => {
                return ActionOutcome {
                    status: OutcomeStatus::InternalError,
                    message: format!("Probe task failed: {e}"),
                    requires_reload: false,
                    requires_restart: false,
                };
            }
        }
    }

    let is_official = matches!(&input, MarketplaceAddInput::GitUrl(u)
        if echo_agent_plugin_marketplace::is_official_source_url(u));
    let name = if is_official {
        echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.to_string()
    } else {
        match &input {
            MarketplaceAddInput::GitUrl(u) => plugin::name_from_url(u),
            MarketplaceAddInput::LocalPath(p) => plugin::name_from_path(p),
        }
    };

    // Run the write under SAVE_LOCK + flock, off the reactor.
    let config_path = echo_agent_config::echo_agent_home().join("config.toml");
    let echo_agent_home = echo_agent_config::echo_agent_home();
    let _save_guard = crate::util::config::lock_config_writes().await;
    let write = {
        let name = name.clone();
        tokio::task::spawn_blocking(move || {
            let _flock = acquire_init_lock(&echo_agent_home)?;
            plugin::preserve_revoked_marketplace_installs_before_add()?;
            add_marketplace_source(&config_path, &name, &input, is_official)
        })
        .await
    };
    match write {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return ActionOutcome {
                status: OutcomeStatus::InternalError,
                message: format!("Failed to write config: {e}"),
                requires_reload: false,
                requires_restart: false,
            };
        }
        Err(e) => {
            return ActionOutcome {
                status: OutcomeStatus::InternalError,
                message: format!("Config write task failed: {e}"),
                requires_reload: false,
                requires_restart: false,
            };
        }
    }

    ActionOutcome {
        status: OutcomeStatus::Success,
        message: format!("Added marketplace source: {name} ({identity})"),
        requires_reload: true,
        requires_restart: false,
    }
}

/// Append a `[[marketplace.sources]]` entry (and optionally set the official
/// flag) in one atomic `toml_edit` write, so a crash can't leave a source
/// without its flag. Idempotent on normalized git URL / local path; preserves
/// comments.
pub(crate) fn add_marketplace_source(
    config_path: &std::path::Path,
    name: &str,
    source: &crate::plugin::MarketplaceAddInput,
    set_official_flag: bool,
) -> std::io::Result<()> {
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = crate::util::config::read_to_string_or_empty(config_path)?;
    let mut doc = existing.parse::<toml_edit::DocumentMut>().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid TOML: {e}"),
        )
    })?;
    let identity = match source {
        crate::plugin::MarketplaceAddInput::GitUrl(url) => url.clone(),
        crate::plugin::MarketplaceAddInput::LocalPath(path) => path.to_string_lossy().into_owned(),
    };
    crate::plugin::clear_marketplace_source_revocation(&mut doc, &identity)?;

    let marketplace_item = doc
        .entry("marketplace")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let marketplace = marketplace_item.as_table_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "[marketplace] is not a table",
        )
    })?;

    let sources_item = marketplace
        .entry("sources")
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let sources = sources_item.as_array_of_tables_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "[[marketplace.sources]] is not an array of tables",
        )
    })?;

    // Skip if the normalized URL / path already exists: the pre-lock dup check
    // in handle_add_source can let two serialized adds reach here.
    use crate::plugin::MarketplaceAddInput;
    let already_present = match source {
        MarketplaceAddInput::GitUrl(git_url) => {
            let normalized = echo_agent_core::plugins::marketplace_scope::source_identity(git_url);
            sources.iter().any(|t| {
                t.get("git").and_then(|v| v.as_str()).is_some_and(|u| {
                    echo_agent_core::plugins::marketplace_scope::source_identity(u) == normalized
                })
            })
        }
        MarketplaceAddInput::LocalPath(path) => {
            let path_str = path.display().to_string();
            sources.iter().any(|t| {
                t.get("path")
                    .and_then(|v| v.as_str())
                    .is_some_and(|p| p == path_str)
            })
        }
    };
    if !already_present {
        let mut entry = toml_edit::Table::new();
        entry["name"] = toml_edit::value(name.to_string());
        match source {
            MarketplaceAddInput::GitUrl(git_url) => {
                entry["git"] = toml_edit::value(git_url.to_string());
            }
            MarketplaceAddInput::LocalPath(path) => {
                entry["path"] = toml_edit::value(path.display().to_string());
            }
        }
        sources.push(entry);
    }

    if set_official_flag {
        marketplace["official_marketplace_auto_installed"] = toml_edit::value(true);
    }

    crate::util::config::atomic_write_string(config_path, &doc.to_string())
}

/// Remove a marketplace source from `~/.echo-agent/config.toml` and uninstall all
/// plugins that were installed from it.
async fn handle_remove_source(
    source_url_or_path: &str,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    let src = source_url_or_path.to_string();
    // Lock + run the blocking FS work off the reactor.
    let _save_guard = crate::util::config::lock_config_writes().await;
    match tokio::task::spawn_blocking(move || remove_source_locked(&src)).await {
        Ok(outcome) => outcome,
        Err(e) => echo_agent_hooks_plugins_types::ActionOutcome {
            status: echo_agent_hooks_plugins_types::OutcomeStatus::InternalError,
            message: format!("Config write task failed: {e}"),
            requires_reload: false,
            requires_restart: false,
        },
    }
}

/// Sync body of [`handle_remove_source`], run on a blocking thread under the
/// flock for the whole read-modify-write so a concurrent auto-register can't
/// re-add the source mid-removal.
fn remove_source_locked(source_url_or_path: &str) -> echo_agent_hooks_plugins_types::ActionOutcome {
    use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
    match crate::plugin::remove_marketplace_source(source_url_or_path) {
        Ok(uninstalled) => ActionOutcome {
            status: OutcomeStatus::Success,
            message: if uninstalled.is_empty() {
                format!(
                    "Revoked marketplace source in this Echo profile: {source_url_or_path}. External application files are unchanged."
                )
            } else {
                format!(
                    "Revoked marketplace source and uninstalled {} managed plugin(s): {}",
                    uninstalled.len(),
                    uninstalled.join(", ")
                )
            },
            requires_reload: true,
            requires_restart: false,
        },
        Err(error) => {
            let identity =
                echo_agent_core::plugins::marketplace_scope::source_identity(source_url_or_path);
            let persisted = crate::plugin::marketplace_disabled_sources().contains(&identity);
            ActionOutcome {
                status: if error.kind() == std::io::ErrorKind::NotFound {
                    OutcomeStatus::NotFound
                } else {
                    OutcomeStatus::InternalError
                },
                message: if persisted {
                    format!(
                        "The source was revoked, but managed file cleanup failed: {error}. Retry removal to clean the remaining managed files, or re-add the source to manage them."
                    )
                } else {
                    format!("Failed to remove marketplace source: {error}")
                },
                requires_reload: persisted,
                requires_restart: false,
            }
        }
    }
}

fn set_marketplace_bool_flag_in_toml(content: &str, key: &str) -> std::io::Result<String> {
    let mut doc = content.parse::<toml_edit::DocumentMut>().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid TOML: {e}"),
        )
    })?;

    let marketplace = doc
        .entry("marketplace")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "[marketplace] is not a table",
            )
        })?;
    marketplace[key] = toml_edit::value(true);

    Ok(doc.to_string())
}

fn set_marketplace_bool_flag(config_path: &std::path::Path, key: &str) -> std::io::Result<()> {
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = crate::util::config::read_to_string_or_empty(config_path)?;
    let updated = set_marketplace_bool_flag_in_toml(&existing, key)?;
    crate::util::config::atomic_write_string(config_path, &updated)
}

fn read_marketplace_bool_flag(config_path: &std::path::Path, key: &str) -> bool {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    parsed
        .get("marketplace")
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
fn set_official_flag_in_toml(content: &str) -> std::io::Result<String> {
    set_marketplace_bool_flag_in_toml(content, "official_marketplace_auto_installed")
}

fn set_official_marketplace_auto_installed(config_path: &std::path::Path) -> std::io::Result<()> {
    set_marketplace_bool_flag(config_path, "official_marketplace_auto_installed")
}

fn read_official_marketplace_auto_installed(config_path: &std::path::Path) -> bool {
    read_marketplace_bool_flag(config_path, "official_marketplace_auto_installed")
}

/// Acquire the shared Runtime configuration transaction lock. The helper uses
/// the historical `.config-init.lock` path, so marketplace initialization,
/// MCP mutations, settings saves, and desktop-side writes all serialize on the
/// same advisory lock across threads and processes.
fn acquire_init_lock(
    echo_agent_home: &std::path::Path,
) -> std::io::Result<crate::util::config::ConfigTransactionGuard> {
    crate::util::config::acquire_config_transaction_lock_at(&echo_agent_home.join("config.toml"))
}

fn is_default_skills_plugin_subdir(plugin_subdir: &str) -> bool {
    plugin_subdir == "default-skills"
}

fn default_skills_repo_keys<'a>(
    repos: impl IntoIterator<
        Item = (
            &'a str,
            &'a echo_agent_core::plugins::install_registry::InstalledRepo,
        ),
    >,
) -> Vec<&'a str> {
    repos
        .into_iter()
        .filter_map(|(key, repo)| {
            repo.marketplace
                .as_ref()
                .filter(|mp| is_default_skills_plugin_subdir(&mp.plugin_subdir))
                .map(|_| key)
        })
        .collect()
}

fn set_default_skills_installs_purged(config_path: &std::path::Path) -> std::io::Result<()> {
    set_marketplace_bool_flag(config_path, "default_skills_installs_purged")
}

fn read_default_skills_installs_purged(config_path: &std::path::Path) -> bool {
    read_marketplace_bool_flag(config_path, "default_skills_installs_purged")
}

/// One-shot purge of legacy marketplace `default-skills` installs.
///
/// Gated by sticky `default_skills_installs_purged` in config.toml. Best-effort:
/// errors are logged and never block startup.
pub(crate) fn purge_default_skills_installs(echo_agent_home: &std::path::Path) {
    purge_default_skills_installs_impl(echo_agent_home, || {
        echo_agent_core::plugins::install_registry::InstallRegistry::try_load_from(
            echo_agent_core::plugins::install_registry::InstallRegistry::resolve_install_dir(),
        )
    });
}

fn purge_default_skills_installs_impl(
    echo_agent_home: &std::path::Path,
    load_registry: impl FnOnce() -> Result<
        echo_agent_core::plugins::install_registry::InstallRegistry,
        echo_agent_core::plugins::install_registry::InstallError,
    >,
) {
    let config_path = echo_agent_home.join("config.toml");

    if read_default_skills_installs_purged(&config_path) {
        return;
    }

    let _lock = match acquire_init_lock(echo_agent_home) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %echo_agent_home.join(".config-init.lock").display(),
                "skipping default-skills purge: failed to acquire init lock"
            );
            return;
        }
    };

    if read_default_skills_installs_purged(&config_path) {
        return;
    }

    let mut registry = match load_registry() {
        Ok(reg) => reg,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "skipping default-skills purge: failed to load install registry"
            );
            return;
        }
    };
    let keys: Vec<String> = default_skills_repo_keys(registry.list())
        .into_iter()
        .map(|k| k.to_string())
        .collect();

    for key in &keys {
        let path = registry
            .get_repo(key)
            .map(|r| r.path.clone())
            .unwrap_or_else(|| registry.install_dir().join(key));
        if path.exists()
            && let Err(e) = std::fs::remove_dir_all(&path)
        {
            let _ = std::fs::remove_file(&path);
            if path.exists() {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    repo_key = %key,
                    "failed to remove default-skills install dir"
                );
            }
        }
        registry.remove(key);
    }

    if !keys.is_empty() {
        if let Err(e) = registry.save() {
            tracing::warn!(error = %e, "failed to save registry after default-skills purge");
            return;
        }
        tracing::info!(
            count = keys.len(),
            "purged legacy default-skills marketplace installs"
        );
    }

    if let Err(e) = set_default_skills_installs_purged(&config_path) {
        tracing::warn!(
            error = %e,
            path = %config_path.display(),
            "failed to set default_skills_installs_purged flag"
        );
    }
}

/// Auto-register the official EchoAgent marketplace source on first run.
///
/// Gated by the caller (`init_process`); see
/// `Config::resolve_official_marketplace_auto_register`. No-op once
/// `official_marketplace_auto_installed` is set. Under a process-wide flock it
/// adds the source (or just sets the flag if it's already present in config.toml
/// or a JSON store). Best-effort: errors are logged and never block startup.
pub(crate) fn ensure_official_marketplace_source(echo_agent_home: &std::path::Path) {
    let config_path = echo_agent_home.join("config.toml");

    if read_official_marketplace_auto_installed(&config_path) {
        return;
    }

    let _lock = match acquire_init_lock(echo_agent_home) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %echo_agent_home.join(".config-init.lock").display(),
                "skipping official marketplace auto-register: failed to acquire init lock"
            );
            return;
        }
    };

    // Re-check under the lock: another process may have registered meanwhile.
    if read_official_marketplace_auto_installed(&config_path) {
        return;
    }

    let raw = match crate::util::config::read_to_string_or_empty(&config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "skipping official marketplace auto-register: cannot read config.toml");
            return;
        }
    };
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "skipping official marketplace auto-register: invalid config.toml");
            return;
        }
    };

    // "Already present" = official URL in config.toml sources OR a JSON store
    // (settings.json / known_marketplaces.json) under echo_agent_home. Scoped to
    // echo_agent_home only (not ~/.claude) to keep tests hermetic; a user with the URL
    // solely in ~/.claude gets one duplicate entry that the UI dedupes by URL.
    let toml_sources = echo_agent_plugin_marketplace::load_sources(&parsed);
    let json_sources = echo_agent_plugin_marketplace::load_extra_sources_from_settings_in(
        &toml_sources,
        std::slice::from_ref(&echo_agent_home.to_path_buf()),
    );
    let already_present = toml_sources.iter().chain(json_sources.iter()).any(|s| {
        matches!(&s.kind, echo_agent_plugin_marketplace::SourceKind::Git { url, .. }
            if echo_agent_plugin_marketplace::is_official_source_url(url))
    });

    let write_result = if already_present {
        // Already present: just set the flag.
        set_official_marketplace_auto_installed(&config_path)
    } else {
        add_marketplace_source(
            &config_path,
            echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME,
            &crate::plugin::MarketplaceAddInput::GitUrl(
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.to_string(),
            ),
            true,
        )
    };

    match write_result {
        Ok(()) if !already_present => {
            tracing::info!(
                url = echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL,
                "auto-registered official EchoAgent marketplace source"
            );
        }
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(error = %e, "failed to auto-register official marketplace source");
        }
    }
}

#[cfg(test)]
mod official_source_tests {
    use super::*;

    fn read_sources(
        config_path: &std::path::Path,
    ) -> Vec<echo_agent_plugin_marketplace::MarketplaceSource> {
        let raw = std::fs::read_to_string(config_path).unwrap_or_default();
        let parsed: toml::Value =
            toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(Default::default()));
        echo_agent_plugin_marketplace::load_sources(&parsed)
    }

    fn read_flag(config_path: &std::path::Path) -> bool {
        let raw = std::fs::read_to_string(config_path).unwrap_or_default();
        let parsed: toml::Value =
            toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(Default::default()));
        parsed
            .get("marketplace")
            .and_then(|m| m.get("official_marketplace_auto_installed"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    #[test]
    fn set_official_flag_in_toml_preserves_other_content() {
        let content = "[ui]\ntheme = \"dark\"\n";
        let out = set_official_flag_in_toml(content).unwrap();
        assert!(out.contains("theme = \"dark\""), "{out}");
        assert!(
            out.contains("official_marketplace_auto_installed = true"),
            "{out}"
        );
    }

    #[test]
    fn add_marketplace_source_local_path_writes_path_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        let dir = tmp.path().join("my-plugins");
        std::fs::create_dir_all(&dir).unwrap();

        let input = crate::plugin::MarketplaceAddInput::LocalPath(dir.clone());
        add_marketplace_source(&config_path, "my-plugins", &input, false).unwrap();
        // Idempotent on the same path.
        add_marketplace_source(&config_path, "my-plugins", &input, false).unwrap();

        let sources = read_sources(&config_path);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "my-plugins");
        assert!(matches!(
            &sources[0].kind,
            echo_agent_plugin_marketplace::SourceKind::Local { path } if path == &dir
        ));
        // The path must not be mangled into a git URL.
        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(!raw.contains("git ="), "{raw}");
    }

    #[test]
    fn scoped_marketplace_revocation_readd_clears_only_matching_tombstone_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "# Keep user comment\n[marketplace]\ndisabled_sources = [\"https://example.invalid/selected\", \"https://example.invalid/other\"]\n").unwrap();
        let input = crate::plugin::MarketplaceAddInput::GitUrl(
            "https://example.invalid/selected.git".into(),
        );
        add_marketplace_source(&config_path, "selected", &input, false).unwrap();
        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(raw.starts_with("# Keep user comment"));
        let config: toml::Value = toml::from_str(&raw).unwrap();
        assert_eq!(
            config["marketplace"]["disabled_sources"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            config["marketplace"]["disabled_sources"][0].as_str(),
            Some("https://example.invalid/other")
        );
        assert_eq!(
            config["marketplace"]["sources"][0]["git"].as_str(),
            Some("https://example.invalid/selected.git")
        );
    }

    #[test]
    fn scoped_marketplace_reload_failure_preserves_partial_change_and_recovery_feedback() {
        use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
        let applied = || ActionOutcome {
            status: OutcomeStatus::Success,
            message: "Installed files".into(),
            requires_reload: true,
            requires_restart: false,
        };
        for reload in [
            None,
            Some(ActionOutcome {
                status: OutcomeStatus::InternalError,
                message: "reload rejected".into(),
                requires_reload: false,
                requires_restart: false,
            }),
        ] {
            let result = complete_plugin_reload(applied(), reload);
            assert_eq!(
                result.status,
                OutcomeStatus::InternalError,
                "A missing/rejected live reload must not certify installation activation"
            );
            assert!(result.requires_reload && result.requires_restart);
            assert!(
                result.message.contains("Installed files")
                    && result.message.contains("Run /plugins reload or restart")
            );
        }
        let result = complete_plugin_reload(
            applied(),
            Some(ActionOutcome {
                status: OutcomeStatus::Success,
                message: "reloaded".into(),
                requires_reload: false,
                requires_restart: false,
            }),
        );
        assert_eq!(result.status, OutcomeStatus::Success);
        assert!(!result.requires_reload && !result.requires_restart);
    }

    #[test]
    fn scoped_marketplace_setup_failure_is_not_masked_by_successful_reload() {
        use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
        let applied = plugin_setup_outcome(
            "Installed files".into(),
            &["Could not enable plugin: disk write denied".into()],
        );
        assert_eq!(applied.status, OutcomeStatus::InternalError);
        assert!(applied.requires_reload && applied.message.contains("disk write denied"));
        let reloaded = complete_plugin_reload(
            applied,
            Some(ActionOutcome {
                status: OutcomeStatus::Success,
                message: "reloaded".into(),
                requires_reload: false,
                requires_restart: false,
            }),
        );
        assert_eq!(reloaded.status, OutcomeStatus::InternalError);
        assert!(
            reloaded
                .message
                .contains("Enable the plugin in Plugins or retry Update")
        );
    }

    #[test]
    fn removing_last_nonofficial_source_preserves_flag_and_blocks_readd() {
        // Regression: removing the last (non-official) source must not wipe the
        // sticky flag, or a gated startup would re-add the removed official source.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let config_path = home.join("config.toml");

        std::fs::write(
            &config_path,
            "[marketplace]\nofficial_marketplace_auto_installed = true\n\n\
             [[marketplace.sources]]\nname = \"custom\"\ngit = \"https://example.com/custom.git\"\n",
        )
        .unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let new_content = crate::plugin::remove_toml_marketplace_block(
            &content,
            "https://example.com/custom.git",
        )
        .expect("custom source should be removed");
        std::fs::write(&config_path, &new_content).unwrap();

        assert!(
            read_flag(&config_path),
            "flag must be preserved after removing the last source: {new_content}"
        );

        ensure_official_marketplace_source(home);
        let sources = read_sources(&config_path);
        assert!(
            !sources.iter().any(|s| matches!(&s.kind,
                echo_agent_plugin_marketplace::SourceKind::Git { url, .. }
                    if echo_agent_plugin_marketplace::is_official_source_url(url))),
            "official source must not be re-added after removal"
        );
    }

    #[test]
    fn first_run_creates_source_and_sets_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        ensure_official_marketplace_source(home);

        let config_path = home.join("config.toml");
        assert!(config_path.exists(), "config.toml should be created");

        let sources = read_sources(&config_path);
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].name,
            echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME
        );
        assert!(matches!(
            &sources[0].kind,
            echo_agent_plugin_marketplace::SourceKind::Git { url, .. }
                if url == echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL
        ));
        assert!(read_flag(&config_path));
    }

    #[test]
    fn second_run_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        ensure_official_marketplace_source(home);
        let after_first = std::fs::read_to_string(home.join("config.toml")).unwrap();

        ensure_official_marketplace_source(home);
        let after_second = std::fs::read_to_string(home.join("config.toml")).unwrap();

        assert_eq!(
            after_first, after_second,
            "second run must not modify config"
        );
    }

    #[test]
    fn removed_source_stays_removed_across_restarts() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let config_path = home.join("config.toml");

        ensure_official_marketplace_source(home);
        assert_eq!(read_sources(&config_path).len(), 1);

        // Simulate removal: drop the source block, keep the flag.
        std::fs::write(
            &config_path,
            "[marketplace]\nofficial_marketplace_auto_installed = true\n",
        )
        .unwrap();

        ensure_official_marketplace_source(home);

        assert!(
            read_sources(&config_path).is_empty(),
            "official source must not be re-added after removal"
        );
        assert!(read_flag(&config_path));
    }

    #[test]
    fn pre_existing_official_source_just_sets_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let config_path = home.join("config.toml");

        // User added the official source manually (flag not set).
        std::fs::write(
            &config_path,
            format!(
                "[[marketplace.sources]]\nname = \"{}\"\ngit = \"{}\"\n",
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME,
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL,
            ),
        )
        .unwrap();

        ensure_official_marketplace_source(home);

        let sources = read_sources(&config_path);
        assert_eq!(sources.len(), 1, "must not duplicate existing source");
        assert!(read_flag(&config_path));
    }

    #[test]
    fn pre_existing_official_source_in_known_marketplaces_skips_append() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let plugins_dir = home.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::write(
            plugins_dir.join("known_marketplaces.json"),
            format!(
                r#"{{"echo-agent-official":{{"source":{{"source":"git","url":"{}"}}}}}}"#,
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL,
            ),
        )
        .unwrap();

        ensure_official_marketplace_source(home);

        let config_path = home.join("config.toml");
        assert!(
            read_sources(&config_path).is_empty(),
            "must not append to config.toml when the source is already present in known_marketplaces.json"
        );
        assert!(
            read_flag(&config_path),
            "must set the auto-installed flag so subsequent restarts skip the append check"
        );
    }

    #[test]
    fn pre_existing_official_source_in_extra_known_marketplaces_skips_append() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(
            home.join("settings.json"),
            format!(
                r#"{{"extraKnownMarketplaces":{{"echo-agent-official":{{"source":{{"source":"git","url":"{}"}}}}}}}}"#,
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL,
            ),
        )
        .unwrap();

        ensure_official_marketplace_source(home);

        let config_path = home.join("config.toml");
        assert!(
            read_sources(&config_path).is_empty(),
            "must not append to config.toml when source is in extraKnownMarketplaces"
        );
        assert!(read_flag(&config_path));
    }

    #[test]
    fn pre_existing_official_source_with_branch_just_sets_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let config_path = home.join("config.toml");

        // Official source pinned to a non-main branch: dedup must match URL alone.
        std::fs::write(
            &config_path,
            format!(
                "[[marketplace.sources]]\nname = \"{}\"\ngit = \"{}\"\nbranch = \"some-branch\"\n",
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME,
                echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL,
            ),
        )
        .unwrap();

        ensure_official_marketplace_source(home);

        let sources = read_sources(&config_path);
        assert_eq!(sources.len(), 1, "must not duplicate existing source");
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("branch = \"some-branch\""),
            "branch override must survive registration"
        );
        assert!(read_flag(&config_path));
    }

    #[test]
    fn preserves_existing_user_sources_and_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let config_path = home.join("config.toml");
        std::fs::write(
            &config_path,
            "# my custom marketplaces\n[[marketplace.sources]]\nname = \"Local\"\npath = \"/tmp/mine\"\n",
        )
        .unwrap();

        ensure_official_marketplace_source(home);

        let after = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            after.contains("# my custom marketplaces"),
            "comments preserved"
        );
        assert!(after.contains("Local"), "existing source preserved");
        let sources = read_sources(&config_path);
        assert_eq!(sources.len(), 2);
        assert!(read_flag(&config_path));
    }
}

#[cfg(test)]
mod default_skills_purge_tests {
    use super::*;
    use echo_agent_core::plugins::install_registry::{
        InstallKind, InstallRegistry, InstalledRepo, MarketplaceProvenance, RepoPlugin,
    };

    fn repo_at(path: &std::path::Path, plugin_subdir: Option<&str>) -> InstalledRepo {
        InstalledRepo {
            kind: InstallKind::Local {
                source_path: path.to_path_buf(),
                subdir: None,
            },
            installed_at: String::new(),
            updated_at: String::new(),
            path: path.to_path_buf(),
            plugins: std::collections::HashMap::from([(
                "p".into(),
                RepoPlugin {
                    subdir: None,
                    version: None,
                },
            )]),
            marketplace: plugin_subdir.map(|subdir| MarketplaceProvenance {
                source_url_or_path: "https://example.com/market.git".into(),
                canonical_source_identity: None,
                source_display_name: "Test".into(),
                plugin_subdir: subdir.into(),
            }),
        }
    }

    #[test]
    fn match_is_exact_plugin_subdir_only() {
        assert!(is_default_skills_plugin_subdir("default-skills"));
        assert!(!is_default_skills_plugin_subdir("plugins/default-skills"));
        assert!(!is_default_skills_plugin_subdir("default-skills/extra"));
        assert!(!is_default_skills_plugin_subdir("defaults-skills"));
        assert!(!is_default_skills_plugin_subdir(""));
    }

    #[test]
    fn collects_only_default_skills_repo_keys() {
        let default_skills = repo_at(std::path::Path::new("/tmp/ds"), Some("default-skills"));
        let other = repo_at(std::path::Path::new("/tmp/office"), Some("plugins/office"));
        let no_marketplace = repo_at(std::path::Path::new("/tmp/local"), None);

        let keys = default_skills_repo_keys([
            ("ds-aaaa", &default_skills),
            ("office-bbbb", &other),
            ("local-cccc", &no_marketplace),
        ]);
        assert_eq!(keys, vec!["ds-aaaa"]);
    }

    #[test]
    fn purged_flag_toml_preserves_other_content() {
        let content =
            "[ui]\ntheme = \"dark\"\n[marketplace]\nofficial_marketplace_auto_installed = true\n";
        let out =
            set_marketplace_bool_flag_in_toml(content, "default_skills_installs_purged").unwrap();
        assert!(out.contains("theme = \"dark\""), "{out}");
        assert!(
            out.contains("official_marketplace_auto_installed = true"),
            "{out}"
        );
        assert!(
            out.contains("default_skills_installs_purged = true"),
            "{out}"
        );
    }

    #[test]
    fn read_purged_flag_false_when_missing_or_wrong_type() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        assert!(!read_default_skills_installs_purged(&path));

        std::fs::write(
            &path,
            "[marketplace]\ndefault_skills_installs_purged = \"yes\"\n",
        )
        .unwrap();
        assert!(!read_default_skills_installs_purged(&path));

        std::fs::write(
            &path,
            "[marketplace]\ndefault_skills_installs_purged = true\n",
        )
        .unwrap();
        assert!(read_default_skills_installs_purged(&path));
    }

    #[test]
    fn purge_sets_flag_when_nothing_to_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let install_dir = home.join("installed-plugins");
        purge_default_skills_installs_impl(home, || {
            Ok(InstallRegistry::empty(install_dir.clone()))
        });
        let config_path = home.join("config.toml");
        assert!(read_default_skills_installs_purged(&config_path));

        let after_first = std::fs::read_to_string(&config_path).unwrap();
        purge_default_skills_installs_impl(home, || Ok(InstallRegistry::empty(install_dir)));
        let after_second = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn purge_skips_flag_when_registry_load_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let install_dir = home.join("installed-plugins");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("registry.json"), "{not-json").unwrap();

        purge_default_skills_installs_impl(home, || {
            InstallRegistry::try_load_from(install_dir.clone())
        });

        assert!(!read_default_skills_installs_purged(
            &home.join("config.toml")
        ));
    }

    #[test]
    fn purge_removes_default_skills_retains_others_and_sets_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let install_dir = home.join("installed-plugins");
        std::fs::create_dir_all(&install_dir).unwrap();

        let ds_path = install_dir.join("ds-aaaa");
        std::fs::create_dir_all(&ds_path).unwrap();
        std::fs::write(ds_path.join("marker"), "ds").unwrap();

        let other_path = install_dir.join("office-bbbb");
        std::fs::create_dir_all(&other_path).unwrap();
        std::fs::write(other_path.join("marker"), "office").unwrap();

        let mut registry = InstallRegistry::empty(install_dir.clone());
        registry.insert("ds-aaaa".into(), repo_at(&ds_path, Some("default-skills")));
        registry.insert(
            "office-bbbb".into(),
            repo_at(&other_path, Some("plugins/office")),
        );
        registry.save().unwrap();

        let install_dir_for_load = install_dir.clone();
        purge_default_skills_installs_impl(home, move || {
            InstallRegistry::try_load_from(install_dir_for_load)
        });

        assert!(
            !ds_path.exists(),
            "default-skills install dir must be removed"
        );
        assert!(other_path.exists(), "non-matching install must be retained");

        let reloaded = InstallRegistry::load_from(install_dir.clone());
        assert!(reloaded.get_repo("ds-aaaa").is_none());
        assert!(reloaded.get_repo("office-bbbb").is_some());

        let config_path = home.join("config.toml");
        assert!(read_default_skills_installs_purged(&config_path));

        let after_first = std::fs::read_to_string(&config_path).unwrap();
        let install_dir_for_reload = install_dir;
        purge_default_skills_installs_impl(home, move || {
            InstallRegistry::try_load_from(install_dir_for_reload)
        });
        let after_second = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after_first, after_second);
        assert!(other_path.exists());
    }
}

#[cfg(test)]
mod refresh_feedback_tests {
    use super::*;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    use echo_agent_plugin_marketplace::{MarketplaceSource, SourceKind};

    #[test]
    fn scoped_marketplace_refresh_unknown_target_does_not_report_success() {
        let result = refresh_sources(&[], Some("https://example.invalid/missing.git"));
        assert_eq!(result.status, OutcomeStatus::NotFound);
        assert!(!result.requires_reload && !result.requires_restart);
    }

    #[test]
    fn scoped_marketplace_refresh_missing_local_directory_does_not_report_success() {
        let root = tempfile::tempdir().unwrap();
        let source = MarketplaceSource {
            name: "deleted owned catalog".into(),
            kind: SourceKind::Local {
                path: root.path().join("deleted"),
            },
        };
        let result = refresh_sources(&[source], None);
        assert_eq!(result.status, OutcomeStatus::InternalError);
        assert!(result.message.contains("deleted owned catalog"));
    }

    #[test]
    fn scoped_marketplace_refresh_existing_local_directory_succeeds_without_git() {
        let root = tempfile::tempdir().unwrap();
        let source = MarketplaceSource {
            name: "owned catalog".into(),
            kind: SourceKind::Local {
                path: root.path().to_owned(),
            },
        };
        let result = refresh_sources_with(&[source], None, |_, _| panic!("Unexpected Git access"));
        assert_eq!(result.status, OutcomeStatus::Success);
        assert_eq!(result.message, "Refreshed 1 source(s).");
    }

    #[test]
    fn scoped_marketplace_refresh_partial_git_failure_reports_real_counts_and_retry() {
        let root = tempfile::tempdir().unwrap();
        let sources = [
            MarketplaceSource {
                name: "local".into(),
                kind: SourceKind::Local {
                    path: root.path().to_owned(),
                },
            },
            MarketplaceSource {
                name: "offline".into(),
                kind: SourceKind::Git {
                    url: "https://example.invalid/owned.git".into(),
                    branch: Some("main".into()),
                },
            },
        ];
        let result = refresh_sources_with(&sources, None, |url, branch| {
            assert_eq!(url, "https://example.invalid/owned.git");
            assert_eq!(branch, Some("main"));
            Err("Connection refused".into())
        });
        assert_eq!(result.status, OutcomeStatus::InternalError);
        assert!(result.message.contains("Refreshed 1 of 2"));
        assert!(result.message.contains("offline: Connection refused"));
        assert!(result.message.contains("retry Refresh"));
        assert!(!result.requires_reload && !result.requires_restart);
    }

    #[test]
    fn scoped_marketplace_refresh_filter_does_not_access_unselected_sources() {
        let sources = [MarketplaceSource {
            name: "unrelated".into(),
            kind: SourceKind::Git {
                url: "https://example.invalid/unrelated.git".into(),
                branch: None,
            },
        }];
        let result = refresh_sources_with(
            &sources,
            Some("https://example.invalid/missing.git"),
            |_, _| panic!("Unselected source was accessed"),
        );
        assert_eq!(result.status, OutcomeStatus::NotFound);
    }

    #[test]
    fn scoped_marketplace_refresh_empty_catalog_remains_a_successful_noop() {
        let result = refresh_sources_with(&[], None, |_, _| panic!("Unexpected Git access"));
        assert_eq!(result.status, OutcomeStatus::Success);
        assert_eq!(result.message, "Refreshed 0 source(s).");
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;

    #[test]
    fn to_plugin_entry_carries_homepage_and_keywords() {
        let entry = echo_agent_plugin_marketplace::MarketplaceEntry {
            name: "demo".into(),
            version: Some("1.0.0".into()),
            description: Some("demo".into()),
            category: Some("dev".into()),
            author: Some("echoagent".into()),
            tags: vec!["cli".into()],
            keywords: vec!["search".into(), "rank".into()],
            domains: vec!["example.com".into()],
            homepage: Some("https://example.com/demo".into()),
            relative_path: "plugins/demo".into(),
            skill_count: 2,
            has_hooks: true,
            has_agents: false,
            has_mcp: false,
            remote_url: None,
            remote_ref: None,
            remote_sha: None,
            remote_subdir: Some("plugins/acme".into()),
            components: Some(echo_agent_hooks_plugins_types::PluginComponents {
                skills: vec![echo_agent_hooks_plugins_types::ComponentItem::new(
                    "code-review",
                    Some("Review staged changes".to_string()),
                )],
                ..Default::default()
            }),
        };

        let dto = to_plugin_entry(entry, "not_installed".to_string(), None);

        assert_eq!(dto.homepage.as_deref(), Some("https://example.com/demo"));
        assert_eq!(dto.keywords, vec!["search".to_string(), "rank".to_string()]);
        assert_eq!(dto.domains, vec!["example.com".to_string()]);
        assert_eq!(dto.tags, vec!["cli".to_string()]);
        assert_eq!(dto.install_status, "not_installed");
        assert_eq!(dto.remote_subdir.as_deref(), Some("plugins/acme"));
        let components = dto.components.expect("components passed through");
        assert_eq!(components.skills.len(), 1);
        assert_eq!(components.skills[0].name, "code-review");
        assert_eq!(
            components.skills[0].description.as_deref(),
            Some("Review staged changes")
        );
    }
}
