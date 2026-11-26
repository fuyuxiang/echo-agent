//! `echo.agent/plugins/*` extension handlers.
//!
//! Provides the plugins list endpoint for the pager's hooks/plugins modal.

use agent_client_protocol as acp;
use echo_agent_hooks_plugins_types::{
    HookStatus, McpStatus, PluginInfo, PluginOrigin, PluginScope, PluginsListResponse,
};
use serde::Deserialize;

use crate::agent::MvpAgent;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRequest {
    session_id: String,
}

/// Convert a `LoadedPlugin` to a `PluginInfo` DTO.
pub(crate) fn loaded_plugin_to_info(plugin: &echo_agent_core::plugins::LoadedPlugin) -> PluginInfo {
    use echo_agent_core::plugins::discovery::PluginScope as AgentScope;

    let scope = match plugin.scope {
        AgentScope::CliOverride => PluginScope::Cli,
        AgentScope::Project => PluginScope::Project,
        AgentScope::User => PluginScope::User,
        AgentScope::ConfigPath => PluginScope::Config,
    };

    let origin = origin_to_dto(&plugin.origin);

    let hook_status = if !plugin.has_hooks {
        HookStatus::None
    } else if !plugin.trusted {
        HookStatus::Blocked
    } else if plugin.has_inline_hooks_only {
        HookStatus::ActiveInline
    } else {
        HookStatus::Active
    };

    let mcp_status = if plugin.mcp_server_count == 0 {
        McpStatus::None
    } else if !plugin.trusted {
        McpStatus::Blocked
    } else if plugin.has_inline_mcp_only {
        McpStatus::ActiveInline
    } else {
        McpStatus::Active
    };

    PluginInfo {
        name: plugin.name.clone(),
        id: plugin.id.0.clone(),
        root: plugin.root.display().to_string(),
        scope,
        trusted: plugin.trusted,
        enabled: plugin.enabled,
        version: plugin.version.clone(),
        description: plugin.description.clone(),
        skill_count: plugin.skill_count,
        skill_names: plugin.skill_names.clone(),
        agent_count: plugin.agent_count,
        agent_names: plugin.agent_names.clone(),
        hook_status,
        hook_count: plugin.hook_count,
        mcp_server_count: plugin.mcp_server_count,
        mcp_status,
        marketplace_source: marketplace_source_label(&origin),
        origin: Some(origin),
        conflict: plugin.conflict.clone(),
    }
}

/// Map the agent-side origin to the wire DTO.
fn origin_to_dto(origin: &echo_agent_core::plugins::PluginOrigin) -> PluginOrigin {
    use echo_agent_core::plugins::PluginOrigin as AgentOrigin;
    match origin {
        AgentOrigin::CliOverride => PluginOrigin::CliOverride,
        AgentOrigin::ProjectEchoAgent => PluginOrigin::ProjectEchoAgent,
        AgentOrigin::ProjectClaude => PluginOrigin::ProjectClaude,
        AgentOrigin::UserEchoAgent => PluginOrigin::UserEchoAgent,
        AgentOrigin::UserClaude => PluginOrigin::UserClaude,
        AgentOrigin::ClaudeMarketplace { marketplace } => PluginOrigin::ClaudeMarketplace {
            marketplace: marketplace.clone(),
        },
        AgentOrigin::ClaudeInstalled { marketplace } => PluginOrigin::ClaudeInstalled {
            marketplace: marketplace.clone(),
        },
        AgentOrigin::MarketplaceInstall {
            source_name,
            git_url,
        } => PluginOrigin::MarketplaceInstall {
            source_name: source_name.clone(),
            git_url: git_url.clone(),
        },
        AgentOrigin::ConfigPath => PluginOrigin::ConfigPath,
    }
}

/// Derive the legacy `marketplace_source` label (older-pager compat) from the
/// origin: marketplace display name, or a `git: owner/repo` label for direct
/// git installs.
fn marketplace_source_label(origin: &PluginOrigin) -> Option<String> {
    match origin {
        PluginOrigin::MarketplaceInstall {
            source_name: Some(name),
            ..
        } => Some(name.clone()),
        PluginOrigin::MarketplaceInstall {
            source_name: None,
            git_url: Some(url),
        } => {
            // Derive short name from URL: "https://github.com/obra/superpowers.git" → "obra/superpowers"
            let label = url
                .trim_end_matches(".git")
                .rsplit("://")
                .next()
                .and_then(|s| {
                    s.strip_prefix("github.com/")
                        .or_else(|| s.strip_prefix("gitlab.com/"))
                        .or(Some(s))
                })
                .unwrap_or(url);
            Some(format!("git: {label}"))
        }
        _ => None,
    }
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/plugins/list" => {
            let req: ListRequest = super::parse_params(args)?;

            // A known session answers from its own registry, which includes
            // `_meta.pluginDirs` plugins. Only an unknown session (a pull
            // before any session exists) falls back to the shared snapshot.
            let sid = acp::SessionId::new(req.session_id);
            let registry = match agent.session_handle_waiting_for_load(&sid).await {
                Some(handle) => handle.plugins_list().await,
                None => agent.plugin_registry_snapshot(),
            };
            let response = match registry {
                Some(registry) => {
                    let plugins = registry
                        .list()
                        .iter()
                        .map(|p| loaded_plugin_to_info(p))
                        .collect();
                    PluginsListResponse { plugins }
                }
                None => PluginsListResponse {
                    plugins: Vec::new(),
                },
            };
            super::to_ext_response(Ok::<_, anyhow::Error>(response))
        }
        "echo.agent/plugins/action" => {
            let req: echo_agent_hooks_plugins_types::PluginsActionRequest =
                super::parse_params(args)?;
            let sid = acp::SessionId::new(req.session_id);

            let result = agent
                .execute_plugins_action(&sid, req.action)
                .await
                .ok_or_else(|| anyhow::anyhow!("session not found"));
            super::to_ext_response(result)
        }
        "echo.agent/plugins/notify-updates" => {
            // Broadcast a PluginUpdatesInstalled notification to the session.
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct NotifyUpdatesRequest {
                session_id: String,
                updates: Vec<(String, String, String)>, // (name, old_ver, new_ver)
            }
            let req: NotifyUpdatesRequest = super::parse_params(args)?;
            let sid = acp::SessionId::new(req.session_id);
            if let Some(handle) = agent.get_session_handle(&sid) {
                handle.notify_plugin_updates(req.updates).await;
            }
            super::to_ext_response(Ok::<_, anyhow::Error>(serde_json::json!({ "ok": true })))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_core::plugins::PluginOrigin as AgentOrigin;
    use echo_agent_core::plugins::discovery::{PluginId, PluginScope as AgentScope};

    fn make_loaded_plugin(origin: AgentOrigin) -> echo_agent_core::plugins::LoadedPlugin {
        let root = std::path::PathBuf::from("/tmp/test-plugin");
        echo_agent_core::plugins::LoadedPlugin {
            name: "test-plugin".to_string(),
            id: PluginId::new(AgentScope::User, &root, "test-plugin"),
            root: root.clone(),
            canonical_root: root,
            scope: AgentScope::User,
            origin,
            trusted: true,
            enabled: true,
            version: Some("1.0.0".to_string()),
            description: None,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            skill_count: 0,
            agent_count: 0,
            skill_names: vec![],
            agent_names: vec![],
            has_hooks: false,
            hook_count: 0,
            has_inline_hooks_only: false,
            mcp_server_count: 0,
            has_inline_mcp_only: false,
            lsp_server_count: 0,
            has_inline_lsp_only: false,
            inline_hooks: None,
            inline_mcp_servers: None,
            inline_lsp_servers: None,
            conflict: None,
        }
    }

    #[test]
    fn info_carries_origin_and_marketplace_display_name() {
        let plugin = make_loaded_plugin(AgentOrigin::MarketplaceInstall {
            source_name: Some("EchoAgent Official".to_string()),
            git_url: Some("https://example.com/mp.git".to_string()),
        });
        let info = loaded_plugin_to_info(&plugin);
        assert_eq!(
            info.origin,
            Some(PluginOrigin::MarketplaceInstall {
                source_name: Some("EchoAgent Official".to_string()),
                git_url: Some("https://example.com/mp.git".to_string()),
            })
        );
        assert_eq!(
            info.marketplace_source.as_deref(),
            Some("EchoAgent Official")
        );
    }

    #[test]
    fn direct_git_install_gets_git_label() {
        let plugin = make_loaded_plugin(AgentOrigin::MarketplaceInstall {
            source_name: None,
            git_url: Some("https://github.com/obra/superpowers.git".to_string()),
        });
        let info = loaded_plugin_to_info(&plugin);
        assert_eq!(
            info.marketplace_source.as_deref(),
            Some("git: obra/superpowers")
        );
    }

    #[test]
    fn direct_local_install_has_no_marketplace_source() {
        let plugin = make_loaded_plugin(AgentOrigin::MarketplaceInstall {
            source_name: None,
            git_url: None,
        });
        let info = loaded_plugin_to_info(&plugin);
        assert_eq!(info.marketplace_source, None);
        assert_eq!(
            info.origin,
            Some(PluginOrigin::MarketplaceInstall {
                source_name: None,
                git_url: None,
            })
        );
    }

    #[test]
    fn claude_origins_map_to_dto_without_marketplace_source() {
        for (agent_origin, expected) in [
            (
                AgentOrigin::ClaudeMarketplace {
                    marketplace: "mp".to_string(),
                },
                PluginOrigin::ClaudeMarketplace {
                    marketplace: "mp".to_string(),
                },
            ),
            (
                AgentOrigin::ClaudeInstalled {
                    marketplace: Some("mp".to_string()),
                },
                PluginOrigin::ClaudeInstalled {
                    marketplace: Some("mp".to_string()),
                },
            ),
            (AgentOrigin::UserClaude, PluginOrigin::UserClaude),
            (AgentOrigin::ProjectClaude, PluginOrigin::ProjectClaude),
        ] {
            let info = loaded_plugin_to_info(&make_loaded_plugin(agent_origin));
            assert_eq!(info.origin, Some(expected));
            assert_eq!(info.marketplace_source, None);
        }
    }
}
