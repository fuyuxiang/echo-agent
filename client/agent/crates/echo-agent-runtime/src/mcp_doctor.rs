//! `echoagent mcp doctor` -- runtime health check for MCP servers.

use std::collections::HashMap;
use std::path::Path;

use echo_agent_tools::types::config_source::ConfigSource;
use serde::Serialize;

use crate::session::mcp_servers;

// ── Report types ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ConfigSourceStatus {
    pub path: String,
    pub status: ConfigSourceState,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConfigSourceState {
    Found { server_count: usize },
    NotFound,
    Skipped { reason: String },
}

#[derive(Debug, Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub transport: String,
    pub target: String,
    pub source: String,
    pub checks: Vec<Check>,
    pub healthy: bool,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub label: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Check {
    fn pass(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            passed: true,
            detail: Some(detail.into()),
            hint: None,
        }
    }

    fn fail(label: impl Into<String>, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            passed: false,
            detail: Some(detail.into()),
            hint: Some(hint.into()),
        }
    }

    fn fail_no_hint(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            passed: false,
            detail: Some(detail.into()),
            hint: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub sources: Vec<ConfigSourceStatus>,
    pub servers: Vec<McpServerStatus>,
    #[serde(skip)]
    pub all_server_names: Vec<String>,
    pub healthy_count: usize,
    pub failing_count: usize,
}

// ── Server discovery ────────────────────────────────────────────

struct DiscoveredServer {
    server: agent_client_protocol::McpServer,
    source: ConfigSource,
}

fn discover_servers(cwd: &Path) -> (Vec<ConfigSourceStatus>, Vec<DiscoveredServer>) {
    let trust_store = echo_agent_core::plugins::TrustStore::load();
    let mut plugins_cfg: crate::agent::config::PluginsConfig =
        crate::config::load_effective_config()
            .ok()
            .and_then(|t| t.get("plugins").and_then(|v| v.clone().try_into().ok()))
            .unwrap_or_default();
    plugins_cfg.merge_claude_enabled_plugins(Some(cwd));
    let mut plugin_config = plugins_cfg.to_discovery_config();
    // Route through the live folder-trust gate (matches actual hook/MCP/LSP
    // gating) so the doctor report shows an untrusted folder's project plugin
    // MCP as blocked; no session resolve has run for a one-shot doctor. Resolve
    // and record the verdict, then gate plugins on it.
    let project_trusted = crate::agent::folder_trust::resolve_and_record(cwd, None, false);
    let discovered_plugins = echo_agent_core::plugins::discover_plugins(
        Some(cwd),
        &plugin_config,
        &trust_store,
        project_trusted,
    );
    plugin_config.populate_plugin_lists(&discovered_plugins);
    let plugin_registry = echo_agent_core::plugins::PluginRegistry::from_discovered(
        discovered_plugins,
        &plugin_config.disabled,
        &plugin_config.enabled,
    );

    // mcp-doctor is a diagnostic tool; use default (all-on) compat to show everything.
    let sourced = crate::session::managed_mcp::merge_managed_mcp_servers_sourced(
        cwd,
        Some(&plugin_registry),
        &echo_agent_tools::types::compat::CompatConfig::default(),
    );

    let mut config_count = 0usize;
    let mut claude_count = 0usize;
    let mut mcp_json_count = 0usize;
    let mut plugin_counts: HashMap<String, usize> = HashMap::new();
    let mut servers = Vec::new();
    for (server, source) in sourced {
        match &source {
            ConfigSource::ConfigToml { .. } | ConfigSource::Project { .. } => config_count += 1,
            ConfigSource::ClaudeJson { .. } => claude_count += 1,
            ConfigSource::McpJson { .. } => mcp_json_count += 1,
            ConfigSource::Plugin { plugin_name, .. } => {
                *plugin_counts.entry(plugin_name.clone()).or_default() += 1;
            }
            _ => {}
        }
        servers.push(DiscoveredServer { server, source });
    }

    let mut sources = Vec::new();

    let echo_agent_home = echo_agent_tools::util::echo_agent_home::echo_agent_home();
    let user_config = echo_agent_home.join("config.toml");
    if user_config.is_file() {
        sources.push(ConfigSourceStatus {
            path: "~/.echo-agent/config.toml".to_string(),
            status: ConfigSourceState::Found {
                server_count: config_count,
            },
        });
    } else {
        sources.push(ConfigSourceStatus {
            path: "~/.echo-agent/config.toml".to_string(),
            status: ConfigSourceState::NotFound,
        });
    }

    for config_path in crate::config::find_project_configs(cwd) {
        if config_path.is_file() {
            sources.push(ConfigSourceStatus {
                path: config_path.display().to_string(),
                status: ConfigSourceState::Found { server_count: 0 },
            });
        }
    }

    for (name, count) in &plugin_counts {
        sources.push(ConfigSourceStatus {
            path: format!("plugin: {}", name),
            status: ConfigSourceState::Found {
                server_count: *count,
            },
        });
    }

    let claude_imported = crate::claude_import::is_claude_import_marked();
    if claude_imported {
        sources.push(ConfigSourceStatus {
            path: "~/.claude.json".to_string(),
            status: ConfigSourceState::Skipped {
                reason: "claude_compat imported = true".to_string(),
            },
        });
    } else if let Some(home) = echo_agent_config::discovery_home_dir() {
        let claude_path = home.join(".claude.json");
        if claude_path.is_file() {
            sources.push(ConfigSourceStatus {
                path: "~/.claude.json".to_string(),
                status: ConfigSourceState::Found {
                    server_count: claude_count,
                },
            });
        } else {
            sources.push(ConfigSourceStatus {
                path: "~/.claude.json".to_string(),
                status: ConfigSourceState::NotFound,
            });
        }
    } else {
        sources.push(ConfigSourceStatus {
            path: "~/.claude.json".to_string(),
            status: ConfigSourceState::NotFound,
        });
    }

    if claude_imported {
        sources.push(ConfigSourceStatus {
            path: ".mcp.json".to_string(),
            status: ConfigSourceState::Skipped {
                reason: "claude_compat imported = true".to_string(),
            },
        });
    } else {
        let mcp_json_files = crate::util::config::find_mcp_json_files(cwd);
        if mcp_json_files.is_empty() {
            sources.push(ConfigSourceStatus {
                path: ".mcp.json".to_string(),
                status: ConfigSourceState::NotFound,
            });
        } else {
            sources.push(ConfigSourceStatus {
                path: ".mcp.json".to_string(),
                status: ConfigSourceState::Found {
                    server_count: mcp_json_count,
                },
            });
        }
    }

    (sources, servers)
}

// ── Check functions ─────────────────────────────────────────────

fn resolve_command(command: &str) -> Option<String> {
    let path = std::path::Path::new(command);
    if path.is_absolute() {
        return path.exists().then(|| command.to_string());
    }

    #[cfg(unix)]
    let which_cmd = "which";
    #[cfg(windows)]
    let which_cmd = "where";

    let mut cmd = std::process::Command::new(which_cmd);
    cmd.arg(command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    echo_agent_tools::util::detach_std_command(&mut cmd);
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .map(|s| s.trim().to_string())
        })
}

fn check_command_exists(command: &str) -> Check {
    match resolve_command(command) {
        Some(resolved) => Check::pass("command found", resolved),
        None => Check::fail(
            "command not found",
            command,
            "verify the binary exists and is in PATH",
        ),
    }
}

async fn check_server_start(
    acp_server: agent_client_protocol::McpServer,
    cwd: &Path,
) -> Result<(mcp_servers::McpClient, Check), Check> {
    let start = std::time::Instant::now();
    let noop = echo_agent_session_events::EventWriter::noop();
    let ctx = mcp_servers::McpSpawnCtx::session_less(&noop);
    match mcp_servers::start_mcp_server(acp_server, Some(cwd), None, None, &ctx).await {
        Ok(client) => {
            let elapsed = start.elapsed();
            Ok((
                client,
                Check::pass("server started", format!("{:.1}s", elapsed.as_secs_f64())),
            ))
        }
        Err(e) => Err(format_mcp_error("server failed to start", &e)),
    }
}

async fn check_handshake(
    client: &mcp_servers::McpClient,
) -> Result<(mcp_servers::McpService, Check), Check> {
    match client.ensure_initialized().await {
        Ok(service) => {
            let protocol = service
                .peer_info()
                .map(|info| format!("protocol {}", info.protocol_version))
                .unwrap_or_else(|| "protocol unknown".to_string());
            Ok((service, Check::pass("handshake OK", protocol)))
        }
        Err(e) => Err(format_mcp_error("handshake failed", &e)),
    }
}

async fn check_tools_list(service: &mcp_servers::McpService) -> Check {
    use echo_agent_mcp::rmcp::model::PaginatedRequestParams;
    match service
        .list_tools(Some(PaginatedRequestParams::default()))
        .await
    {
        Ok(result) => {
            let count = result.tools.len();
            if count == 0 {
                Check::fail(
                    "0 tools discovered",
                    "server returned an empty tool list",
                    "check server config",
                )
            } else {
                Check::pass(format!("{} tools discovered", count), "")
            }
        }
        Err(e) => Check::fail("tools/list failed", e.to_string(), "check server logs"),
    }
}

fn format_mcp_error(label: &str, err: &mcp_servers::McpError) -> Check {
    use mcp_servers::McpError;
    match err {
        McpError::Timeout { timeout_secs, .. } => Check::fail(
            "server timed out",
            format!("no response within {}s", timeout_secs),
            "try increasing startup_timeout_sec in config.toml",
        ),
        McpError::SpawnFailed { source, .. } => Check::fail(
            "spawn failed",
            source.to_string(),
            "check command and permissions",
        ),
        McpError::HandshakeFailed { source, .. } => {
            Check::fail("handshake failed", source.to_string(), "check server logs")
        }
        _ => Check::fail(label, err.to_string(), "check server logs"),
    }
}

// ── Per-server orchestration ────────────────────────────────────

fn describe_server(server: &agent_client_protocol::McpServer) -> (String, String) {
    (
        mcp_servers::mcp_transport_str(server).to_string(),
        mcp_servers::mcp_target_str(server),
    )
}

async fn check_server(
    server: agent_client_protocol::McpServer,
    source_label: &str,
    cwd: &Path,
) -> McpServerStatus {
    let name = mcp_servers::mcp_server_name(&server).to_string();
    let (transport, target) = describe_server(&server);

    let mut checks = Vec::new();

    if let agent_client_protocol::McpServer::Stdio(agent_client_protocol::McpServerStdio {
        ref command,
        ..
    }) = server
    {
        let check = check_command_exists(&command.to_string_lossy());
        let ok = check.passed;
        checks.push(check);
        if !ok {
            return McpServerStatus {
                name,
                transport,
                target,
                source: source_label.to_string(),
                checks,
                healthy: false,
            };
        }
    }

    match check_server_start(server, cwd).await {
        Err(check) => {
            checks.push(check);
        }
        Ok((client, check)) => {
            checks.push(check);
            match check_handshake(&client).await {
                Err(check) => {
                    checks.push(check);
                }
                Ok((service, check)) => {
                    checks.push(check);
                    checks.push(check_tools_list(&service).await);
                }
            }
            // Client drops here, killing child process via kill_on_drop.
        }
    }

    let healthy = checks.iter().all(|c| c.passed);
    McpServerStatus {
        name,
        transport,
        target,
        source: source_label.to_string(),
        checks,
        healthy,
    }
}

// ── Entry point ─────────────────────────────────────────────────

pub async fn run_doctor(cwd: &Path, name_filter: Option<&str>) -> DoctorReport {
    let (mut sources, discovered) = discover_servers(cwd);

    let allowlist = &echo_agent_workspace::permission::resolution::managed_settings().mcp_allowlist;
    if allowlist.is_restricted() {
        let path = allowlist
            .source_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "managed-settings.json".to_string());
        sources.push(ConfigSourceStatus {
            path: format!("server allowlist ({})", path),
            status: ConfigSourceState::Found {
                server_count: allowlist.entries.len() + allowlist.deny_entries.len(),
            },
        });
    }

    let all_server_names: Vec<String> = discovered
        .iter()
        .map(|d| mcp_servers::mcp_server_name(&d.server).to_string())
        .collect();

    let to_probe: Vec<DiscoveredServer> = if let Some(filter) = name_filter {
        discovered
            .into_iter()
            .filter(|d| mcp_servers::mcp_server_name(&d.server) == filter)
            .collect()
    } else {
        discovered
    };

    let disabled_names = crate::util::config::disabled_mcp_server_names(cwd);

    // Folder-trust gate: `echoagent mcp doctor` actually STARTS each server
    // (`check_server_start`), so in an untrusted clone it would spawn the repo's
    // project-scoped servers. Resolve the doctor cwd once (no prompt), then skip
    // (do not start) any project-scoped server when untrusted. Reuses the same
    // name primitive as the session/agent-pool gates.
    //
    // `remote = None` is intentional: standalone `echoagent mcp doctor` has no loaded
    // `RemoteSettings`, so a remote-only org `folder_trust_enabled = false`
    // opt-out isn't seen here — gating conservatively (treating the feature as
    // enabled) is the deliberate fail-secure direction. Local env/user/managed
    // config disable is still honored by `feature_enabled`.
    crate::agent::folder_trust::resolve_and_record(cwd, None, false);
    let untrusted_project: std::collections::HashSet<String> =
        if crate::agent::folder_trust::project_scope_allowed(cwd) {
            std::collections::HashSet::new()
        } else {
            crate::agent::folder_trust::project_scoped_mcp_names(cwd)
        };

    const PROBE_CONCURRENCY: usize = 8;

    use futures::StreamExt;
    let results: Vec<McpServerStatus> = futures::stream::iter(to_probe)
        .map(|d| {
            let label = d.source.display_label();
            let name = mcp_servers::mcp_server_name(&d.server).to_string();
            let block_detail = (!allowlist.is_server_allowed(&d.server)).then(|| {
                crate::session::managed_mcp::McpDisabledReason::for_blocked_server(
                    allowlist, &d.server,
                )
                .to_string()
            });
            let disabled = disabled_names.contains(&name);
            let untrusted = untrusted_project.contains(&name);
            async move {
                let skip_reason = if untrusted {
                    Some(Check::fail(
                        "folder untrusted",
                        "repo-local (project-scoped) server not started for an untrusted folder",
                        "re-run with --trust to allow repo-local servers",
                    ))
                } else if disabled {
                    Some(Check::fail(
                        "disabled in config",
                        "server is disabled in config.toml",
                        "set enabled = true or remove from disabled_mcp_servers",
                    ))
                } else {
                    block_detail
                        .map(|detail| Check::fail_no_hint("blocked by organization policy", detail))
                };
                if let Some(check) = skip_reason {
                    let (transport, target) = describe_server(&d.server);
                    return McpServerStatus {
                        name,
                        transport,
                        target,
                        source: label,
                        checks: vec![check],
                        healthy: false,
                    };
                }
                check_server(d.server, &label, cwd).await
            }
        })
        .buffer_unordered(PROBE_CONCURRENCY)
        .collect()
        .await;
    let healthy_count = results.iter().filter(|s| s.healthy).count();
    let failing_count = results.len() - healthy_count;

    DoctorReport {
        sources,
        servers: results,
        all_server_names,
        healthy_count,
        failing_count,
    }
}

// ── Human-readable output ───────────────────────────────────────

pub fn print_report(report: &DoctorReport) {
    println!();
    println!("MCP Doctor");
    println!();

    println!("  Config sources");
    for source in &report.sources {
        let status = match &source.status {
            ConfigSourceState::Found { server_count } => {
                format!(
                    "{} server{}",
                    server_count,
                    if *server_count == 1 { "" } else { "s" }
                )
            }
            ConfigSourceState::NotFound => "not found".to_string(),
            ConfigSourceState::Skipped { reason } => format!("skipped ({})", reason),
        };
        println!("    {:<40} {}", source.path, status);
    }
    println!();

    if report.servers.is_empty() {
        println!("  No MCP servers configured.");
        println!("  Run `echoagent mcp add --help` to get started.");
        println!();
        return;
    }

    for server in &report.servers {
        println!(
            "  {} ({}: {})",
            server.name, server.transport, server.target
        );
        for check in &server.checks {
            let icon = if check.passed { "\u{2713}" } else { "\u{2717}" };
            let detail = check.detail.as_deref().unwrap_or("");
            if detail.is_empty() {
                println!("    {} {}", icon, check.label);
            } else {
                println!("    {} {} ({})", icon, check.label, detail);
            }
            if let Some(hint) = &check.hint {
                println!("    \u{2192} {}", hint);
            }
        }
        println!();
    }

    println!(
        "Found {} healthy, {} failing.{}",
        report.healthy_count,
        report.failing_count,
        if report.failing_count > 0 {
            " Run `echoagent mcp doctor --json` for full diagnostics."
        } else {
            ""
        }
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_servers::McpError;

    #[test]
    fn timeout_gets_specific_hint() {
        let err = McpError::Timeout {
            server: "test".into(),
            timeout_secs: 5,
        };
        let check = format_mcp_error("ignored", &err);
        assert_eq!(check.label, "server timed out");
        assert!(
            check
                .hint
                .as_deref()
                .unwrap()
                .contains("startup_timeout_sec")
        );
    }

    #[test]
    fn non_timeout_uses_caller_label() {
        let check = format_mcp_error("handshake failed", &McpError::ClientError("boom".into()));
        assert_eq!(check.label, "handshake failed");
        assert_eq!(check.detail.as_deref(), Some("MCP client error: boom"));
    }

    #[test]
    fn spawn_failed_shows_io_error() {
        let err = McpError::SpawnFailed {
            server: "test".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "No such file or directory"),
        };
        let check = format_mcp_error("ignored", &err);
        assert_eq!(check.label, "spawn failed");
        assert!(check.detail.as_deref().unwrap().contains("No such file"));
    }
}
