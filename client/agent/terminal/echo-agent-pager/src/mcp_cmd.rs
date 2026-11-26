//! `echoagent mcp` — manage MCP server configurations from the command line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Subcommand, ValueEnum};
use echo_agent_runtime::util::config::{McpServerConfig, McpServerTransportConfig};

use crate::util::display_user_echo_agent_path;

const ADD_AFTER_HELP: &str = "\
Examples:
  # Add a stdio server (everything after -- is the server command)
  echoagent mcp add xcode -- xcrun mcpbridge

  # Add a stdio server with environment variables
  echoagent mcp add postgres -e DATABASE_URL=postgres://localhost/mydb -- npx -y @modelcontextprotocol/server-postgres

  # Add a remote HTTP server
  echoagent mcp add --transport http sentry https://mcp.sentry.dev/mcp

  # Add a remote server with an authentication header
  echoagent mcp add --transport http api https://mcp.example.com/mcp --header \"Authorization: Bearer YOUR_TOKEN\"

  # Add to the project config (./.echo-agent/config.toml) instead of ~/.echo-agent/config.toml
  echoagent mcp add --scope project github -- npx -y @modelcontextprotocol/server-github";

#[derive(Debug, clap::Args, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

/// Transport used to communicate with an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpTransport {
    /// Launch a local process and communicate over stdin/stdout
    Stdio,
    /// Connect to a remote server over streamable HTTP
    Http,
    /// Connect to a remote server over Server-Sent Events
    Sse,
}

/// Which config file an MCP server definition is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpScope {
    /// `~/.echo-agent/config.toml`, available in all your projects
    User,
    /// `./.echo-agent/config.toml`, shared with everyone working in this directory
    Project,
}

impl McpScope {
    fn label(self) -> &'static str {
        match self {
            McpScope::User => "user",
            McpScope::Project => "project",
        }
    }
}

#[derive(Debug, Subcommand, Clone)]
pub enum McpCommand {
    /// List configured MCP servers
    List {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Add or update an MCP server
    Add(AddArgs),
    /// Remove an MCP server
    Remove {
        /// Server name to remove
        name: String,

        /// Config to remove from. When omitted, all scopes are searched.
        #[arg(short = 's', long, value_enum)]
        scope: Option<McpScope>,
    },
    /// Enable an MCP server
    Enable {
        /// Server name
        name: String,
    },
    /// Disable an MCP server
    Disable {
        /// Server name
        name: String,
    },
    /// Diagnose MCP server configuration and connectivity
    Doctor {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Server name to check
        name: Option<String>,
    },
}

// Everything `mcp add` accepts, before validation; `resolve_add` turns it
// into a transport config.
#[derive(Debug, clap::Args, Clone)]
#[command(after_help = ADD_AFTER_HELP)]
pub struct AddArgs {
    /// Server name
    name: String,

    /// Command to launch (stdio) or URL to connect to (http, sse)
    #[arg(value_name = "COMMAND_OR_URL", group = "source")]
    command_or_url: Option<String>,

    /// Arguments passed to the server command. Place them after `--` so
    /// flags such as `-y` are passed to the server instead of echoagent.
    #[arg(value_name = "ARGS")]
    args: Vec<String>,

    /// Transport type. Defaults to stdio, or to http when the positional
    /// argument is an http(s):// URL.
    #[arg(short = 't', long, value_enum)]
    transport: Option<McpTransport>,

    /// Config to write to: user (~/.echo-agent/config.toml) or project (./.echo-agent/config.toml)
    #[arg(short = 's', long, value_enum, default_value = "user")]
    scope: McpScope,

    /// Environment variable for the server process (repeatable)
    #[arg(short = 'e', long = "env", value_name = "KEY=value")]
    env: Vec<String>,

    /// HTTP header for remote servers (repeatable)
    #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
    header: Vec<String>,

    /// Legacy alias for the positional command argument
    #[arg(long, hide = true, group = "source")]
    command: Option<String>,
    /// Legacy companion to --command
    #[arg(long = "args", hide = true, num_args = 1.., requires = "command")]
    legacy_args: Vec<String>,
    /// Legacy alias for adding a remote server by URL
    #[arg(long, hide = true, group = "source")]
    url: Option<String>,
    /// Legacy transport type for --url servers
    #[arg(long = "type", hide = true)]
    transport_type: Option<String>,
}

pub async fn run(mcp_args: McpArgs) -> Result<()> {
    match mcp_args.command {
        McpCommand::List { json } => run_list(json),
        McpCommand::Add(args) => run_add(args).await,
        McpCommand::Remove { name, scope } => run_remove(&name, scope).await,
        McpCommand::Enable { name } => run_set_enabled(&name, true).await,
        McpCommand::Disable { name } => run_set_enabled(&name, false).await,
        McpCommand::Doctor { json, name } => run_doctor(json, name).await,
    }
}

fn run_list(json: bool) -> Result<()> {
    // Include project-scoped servers (nearest definition wins), matching what
    // a session started in this directory would load from config.toml files.
    let cwd = current_dir_or_exit();
    let servers = echo_agent_runtime::util::config::load_mcp_server_configs_with_project(&cwd);
    let disabled = echo_agent_runtime::util::config::disabled_mcp_server_names(&cwd);

    if json {
        let payload: serde_json::Value = servers
            .iter()
            .map(|(name, (config, scope))| {
                let mut entry = serde_json::to_value(config).unwrap_or_default();
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("name".into(), serde_json::Value::String(name.clone()));
                    obj.insert("scope".into(), serde_json::Value::String(scope.to_string()));
                    obj.insert(
                        "enabled".into(),
                        serde_json::Value::Bool(!disabled.contains(name)),
                    );
                }
                entry
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else if servers.is_empty() {
        println!("No MCP servers configured. Run `echoagent mcp add --help` to get started.");
    } else {
        for (name, (config, scope)) in &servers {
            let transport = match &config.transport {
                McpServerTransportConfig::Stdio { command, args, .. } => {
                    if args.is_empty() {
                        command.clone()
                    } else {
                        format!("{} {}", command, args.join(" "))
                    }
                }
                McpServerTransportConfig::StreamableHttp { url, .. } => url.clone(),
            };
            let status = if disabled.contains(name) {
                " (disabled)"
            } else {
                ""
            };
            let scope_note = if *scope == "project" {
                " (project)"
            } else {
                ""
            };
            println!("  {name}: {transport}{status}{scope_note}");
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ResolvedAdd {
    /// Transport the request resolved to; drives the summary wording.
    kind: McpTransport,
    transport: McpServerTransportConfig,
    warnings: Vec<String>,
}

async fn run_add(args: AddArgs) -> Result<()> {
    let resolved = resolve_add(&args)?;
    for warning in &resolved.warnings {
        eprintln!("{warning}");
    }

    let name = &args.name;
    let kind = match resolved.kind {
        McpTransport::Stdio => "stdio",
        McpTransport::Http => "HTTP",
        McpTransport::Sse => "SSE",
    };
    let summary = match &resolved.transport {
        McpServerTransportConfig::Stdio {
            command,
            args: cmd_args,
            ..
        } => {
            let mut rendered = command.clone();
            if !cmd_args.is_empty() {
                rendered.push(' ');
                rendered.push_str(&cmd_args.join(" "));
            }
            format!("{kind} MCP server '{name}' with command: {rendered}")
        }
        McpServerTransportConfig::StreamableHttp { url, .. } => {
            format!("{kind} MCP server '{name}' with URL: {url}")
        }
    };

    let config = McpServerConfig {
        desktop_instance_id: None,
        transport: resolved.transport,
        enabled: true,
        oauth: None,
        setup: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        tool_timeouts: None,
        expose_image_base64: None,
    };

    let path = scope_target(args.scope);
    echo_agent_runtime::util::config::save_mcp_server_config_at(&path, name, &config).await?;
    println!("Added {summary} to {} config", args.scope.label());
    println!("File modified: {}", scope_display(args.scope, &path));
    Ok(())
}

/// Validate an `mcp add` request and build the transport config.
///
/// An explicit transport flag fully determines how `command_or_url` is
/// interpreted. Without one, a bare positional http(s):// URL is inferred to
/// be an HTTP server; other URL-looking commands stay stdio with a warning.
fn resolve_add(args: &AddArgs) -> Result<ResolvedAdd> {
    validate_server_name(&args.name)?;

    // Extra args or --env mean the user is describing a command, and the
    // legacy --command/--url flags keep their own semantics, so only a bare
    // positional http(s):// URL triggers inference.
    let inferred_http = args.transport.is_none()
        && args.url.is_none()
        && args.args.is_empty()
        && args.env.is_empty()
        && args
            .command_or_url
            .as_deref()
            .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://"));

    let transport = match args.transport {
        Some(t) => t,
        // The legacy --url form defaults to HTTP and honors the legacy --type flag.
        None if args.url.is_some() => match args.transport_type.as_deref() {
            Some(t) if t.eq_ignore_ascii_case("sse") => McpTransport::Sse,
            _ => McpTransport::Http,
        },
        None if inferred_http => McpTransport::Http,
        None => McpTransport::Stdio,
    };
    let explicit_transport = args.transport.is_some();

    // Legacy-flag misroutes: --url always means a remote server, and --type
    // only modifies --url.
    if args.url.is_some() && transport == McpTransport::Stdio {
        bail!(
            "--url cannot be combined with --transport stdio. For a remote server, use --transport http or --transport sse."
        );
    }
    if args.transport_type.is_some() && args.url.is_none() {
        bail!("--type is only valid together with --url. Use --transport to choose the transport.");
    }

    let server_args = if args.command.is_some() {
        &args.legacy_args
    } else {
        &args.args
    };
    // Clap's "source" group guarantees at most one of these is set.
    let source = args
        .command_or_url
        .as_deref()
        .or(args.command.as_deref())
        .or(args.url.as_deref());

    match transport {
        McpTransport::Stdio => {
            let Some(command) = source else {
                bail!(
                    "A command is required for stdio servers. Usage: echoagent mcp add <name> -- <command> [args...]"
                );
            };
            if !args.header.is_empty() {
                bail!("--header can only be used with HTTP or SSE servers.");
            }
            // A KEY=value command means an env pair leaked out of -e, which
            // takes one pair per flag (the pre-parity --env was greedy).
            if looks_like_env_pair(command) {
                let pairs: Vec<String> = args
                    .env
                    .iter()
                    .map(String::as_str)
                    .chain([command])
                    .map(|pair| format!("-e {pair}"))
                    .collect();
                bail!(
                    "Invalid command '{command}': it looks like an environment variable. Pass each variable as its own flag: {}",
                    pairs.join(" ")
                );
            }
            let env = parse_env_vars(&args.env)?;

            let mut warnings = Vec::new();
            if !explicit_transport && looks_like_url(command) {
                // Suggest a command that passes URL validation even when the
                // original lacks a scheme (e.g. localhost:3000).
                let suggested_url =
                    if command.starts_with("http://") || command.starts_with("https://") {
                        command.to_string()
                    } else {
                        format!("http://{command}")
                    };
                warnings.push(format!(
                    "Warning: '{command}' looks like a URL, but it is being added as a stdio command because --transport was not specified.\nFor a remote server, use: echoagent mcp add --transport http {} {suggested_url}",
                    args.name
                ));
            }

            Ok(ResolvedAdd {
                kind: McpTransport::Stdio,
                transport: McpServerTransportConfig::Stdio {
                    command: command.to_string(),
                    args: server_args.clone(),
                    env: (!env.is_empty()).then_some(env),
                    cwd: None,
                },
                warnings,
            })
        }
        McpTransport::Http | McpTransport::Sse => {
            let label = if transport == McpTransport::Sse {
                "sse"
            } else {
                "http"
            };
            let Some(url) = source else {
                bail!(
                    "A URL is required for {label} servers. Usage: echoagent mcp add --transport {label} <name> <url>"
                );
            };
            if !url.starts_with("http://") && !url.starts_with("https://") {
                bail!("Invalid URL '{url}'. Server URLs must start with http:// or https://.");
            }
            if !server_args.is_empty() {
                bail!(
                    "Unexpected arguments after the URL: '{}'. HTTP and SSE servers take a single URL.",
                    server_args.join(" ")
                );
            }
            if !args.env.is_empty() {
                bail!("--env can only be used with stdio servers.");
            }
            let headers = parse_headers(&args.header)?;

            let mut warnings = Vec::new();
            if inferred_http {
                warnings.push(format!(
                    "No --transport given; '{url}' starts with http(s)://, adding as an HTTP server. Use --transport sse for an SSE server, or --transport stdio to force a stdio command."
                ));
            }

            Ok(ResolvedAdd {
                kind: transport,
                transport: McpServerTransportConfig::StreamableHttp {
                    url: url.to_string(),
                    transport_type: (transport == McpTransport::Sse).then(|| "sse".to_string()),
                    bearer_token_env_var: None,
                    headers: (!headers.is_empty()).then_some(headers),
                    oauth_client_id: None,
                    oauth_client_secret_env_var: None,
                    oauth_scopes: None,
                },
                warnings,
            })
        }
    }
}

fn validate_server_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "Invalid name '{name}'. Names can only contain letters, numbers, hyphens, and underscores."
        );
    }
    Ok(())
}

fn parse_env_vars(pairs: &[String]) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    for pair in pairs {
        match pair.split_once('=') {
            Some((key, value)) if !key.is_empty() => {
                env.insert(key.to_string(), value.to_string());
            }
            _ => bail!(
                "Invalid environment variable format: '{pair}'. Environment variables should be added as: -e KEY1=value1 -e KEY2=value2"
            ),
        }
    }
    Ok(env)
}

fn parse_headers(headers: &[String]) -> Result<HashMap<String, String>> {
    let mut parsed = HashMap::new();
    for header in headers {
        let Some((name, value)) = header.split_once(':') else {
            bail!("Invalid header format: '{header}'. Expected format: 'Name: value'");
        };
        let name = name.trim();
        if name.is_empty() {
            bail!("Invalid header: '{header}'. Header name cannot be empty.");
        }
        parsed.insert(name.to_string(), value.trim().to_string());
    }
    Ok(parsed)
}

fn looks_like_url(command: &str) -> bool {
    command.starts_with("http://")
        || command.starts_with("https://")
        || command.starts_with("localhost")
}

/// True for `KEY=value` shapes with a `[A-Za-z_][A-Za-z0-9_]*` key.
fn looks_like_env_pair(s: &str) -> bool {
    let Some((key, _)) = s.split_once('=') else {
        return false;
    };
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Current working directory, exiting loudly when it cannot be determined.
fn current_dir_or_exit() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("Cannot determine working directory: {e}");
        std::process::exit(1);
    })
}

/// Resolve the config file path for a scope.
fn scope_target(scope: McpScope) -> PathBuf {
    match scope {
        McpScope::User => echo_agent_runtime::util::config::user_config_path(),
        McpScope::Project => {
            echo_agent_runtime::util::config::project_config_path(&current_dir_or_exit())
        }
    }
}

/// Display form of a scope's config file path.
fn scope_display(scope: McpScope, path: &Path) -> String {
    match scope {
        McpScope::User => display_user_echo_agent_path("config.toml"),
        McpScope::Project => path.display().to_string(),
    }
}

/// Why `mcp remove` could not resolve a single config file to delete from.
#[derive(Debug, PartialEq)]
enum RemoveError {
    /// The name is not defined in any searched scope.
    NotFound,
    /// The name is defined in both scopes, so the user must pick one.
    Ambiguous { project_path: PathBuf },
}

/// Pick the config file `mcp remove` deletes from, given which scopes define
/// the name. Pure so the scope x presence matrix is unit-testable; printing
/// and exit codes stay in `run_remove`.
fn select_remove_site(
    user_defined: bool,
    project_site: Option<PathBuf>,
    scope: Option<McpScope>,
) -> Result<(McpScope, PathBuf), RemoveError> {
    use echo_agent_runtime::util::config::user_config_path;

    match scope {
        Some(McpScope::User) => user_defined
            .then(|| (McpScope::User, user_config_path()))
            .ok_or(RemoveError::NotFound),
        Some(McpScope::Project) => project_site
            .map(|path| (McpScope::Project, path))
            .ok_or(RemoveError::NotFound),
        None => match (user_defined, project_site) {
            (true, Some(project_path)) => Err(RemoveError::Ambiguous { project_path }),
            (true, None) => Ok((McpScope::User, user_config_path())),
            (false, Some(path)) => Ok((McpScope::Project, path)),
            (false, None) => Err(RemoveError::NotFound),
        },
    }
}

/// Where a name still resolves after a delete: project sites shadow user
/// scope, so the nearest surviving definition wins.
fn surviving_definition(
    user_defined: bool,
    project_site: Option<PathBuf>,
) -> Option<(McpScope, PathBuf)> {
    project_site
        .map(|path| (McpScope::Project, path))
        .or_else(|| {
            user_defined.then(|| {
                (
                    McpScope::User,
                    echo_agent_runtime::util::config::user_config_path(),
                )
            })
        })
}

/// TOML / disabled list / compat JSON / plugin names. Gateway connectors are
/// rejected earlier (colon names).
fn mcp_server_is_known(name: &str, cwd: &Path) -> bool {
    echo_agent_runtime::util::config::cli_known_mcp_server_names(cwd).contains(name)
}

fn is_gateway_cli_toggle_name(name: &str) -> bool {
    name.starts_with("managed_gateway:") || name.contains(':')
}

fn available_mcp_server_names(cwd: &Path) -> Vec<String> {
    let mut names: Vec<String> = echo_agent_runtime::util::config::cli_known_mcp_server_names(cwd)
        .into_iter()
        .collect();
    names.sort();
    names
}

async fn run_set_enabled(name: &str, enabled: bool) -> Result<()> {
    // Do not use validate_server_name (add-only: [A-Za-z0-9_-]). Enable/disable
    // also targets compat/plugin names that may contain dots or other keys.
    if name.is_empty() {
        bail!("Server name cannot be empty.");
    }
    if is_gateway_cli_toggle_name(name) {
        eprintln!(
            "Gateway connectors (e.g. managed_gateway:…) cannot be toggled via CLI; use Space in /mcps."
        );
        std::process::exit(1);
    }
    let cwd = current_dir_or_exit();

    if !mcp_server_is_known(name, &cwd) {
        eprintln!("No MCP server named '{name}'.");
        let available = available_mcp_server_names(&cwd);
        if !available.is_empty() {
            eprintln!("Available servers: {}", available.join(", "));
        } else {
            eprintln!("No MCP servers configured. Run `echoagent mcp add --help` to get started.");
        }
        std::process::exit(1);
    }

    let was_disabled =
        echo_agent_runtime::util::config::disabled_mcp_server_names(&cwd).contains(name);

    let modified =
        echo_agent_runtime::util::config::save_mcp_server_enabled_in(name, enabled, &cwd).await?;

    let now_disabled =
        echo_agent_runtime::util::config::disabled_mcp_server_names(&cwd).contains(name);
    let now_enabled = !now_disabled;

    if enabled && now_disabled {
        eprintln!(
            "Warning: '{name}' is still disabled after enable (check project-scoped config)."
        );
        std::process::exit(1);
    }
    if !enabled && now_enabled {
        eprintln!("Warning: '{name}' is still enabled after disable.");
        std::process::exit(1);
    }

    if was_disabled == now_disabled {
        let state = if now_enabled { "enabled" } else { "disabled" };
        println!("MCP server '{name}' is already {state}.");
    } else if now_enabled {
        println!("Enabled MCP server '{name}'.");
    } else {
        println!("Disabled MCP server '{name}'.");
    }

    let user_config = echo_agent_runtime::util::config::user_config_path();
    for path in &modified {
        if path == &user_config {
            println!(
                "File modified: {}",
                display_user_echo_agent_path("config.toml")
            );
        } else {
            println!("File modified: {}", path.display());
        }
    }
    Ok(())
}

async fn run_remove(name: &str, requested_scope: Option<McpScope>) -> Result<()> {
    use echo_agent_runtime::util::config::{
        delete_mcp_server_config_at, mcp_server_defined_at, user_config_path,
    };

    let cwd = current_dir_or_exit();

    // Project configs from cwd up to the repo root, nearest first.
    let find_project_site = || {
        echo_agent_runtime::config::find_project_configs(&cwd)
            .into_iter()
            .rev()
            .find(|path| mcp_server_defined_at(path, name))
    };

    let user_defined = mcp_server_defined_at(&user_config_path(), name);
    let (scope, path) = match select_remove_site(user_defined, find_project_site(), requested_scope)
    {
        Ok(site) => site,
        Err(RemoveError::NotFound) => {
            let searched = requested_scope.map_or("user or project", McpScope::label);
            eprintln!("No MCP server named '{name}' in {searched} config");
            std::process::exit(1);
        }
        Err(RemoveError::Ambiguous { project_path }) => {
            eprintln!("MCP server '{name}' exists in multiple scopes:");
            eprintln!("  user: {}", display_user_echo_agent_path("config.toml"));
            eprintln!("  project: {}", project_path.display());
            eprintln!(
                "Specify which one to remove, e.g.: echoagent mcp remove {name} --scope project"
            );
            std::process::exit(1);
        }
    };

    let existed = delete_mcp_server_config_at(&path, name).await?;
    if !existed {
        // Race guard: the entry vanished between the existence check and the delete.
        eprintln!("No MCP server named '{name}' in {} config", scope.label());
        std::process::exit(1);
    }

    println!("Removed MCP server '{name}' from {} config", scope.label());
    println!("File modified: {}", scope_display(scope, &path));

    // A scoped delete can leave the name defined in the other scope or an
    // ancestor .echo-agent/config.toml, where it still resolves for sessions.
    let still_user_defined = mcp_server_defined_at(&user_config_path(), name);
    if let Some((survivor_scope, remaining)) =
        surviving_definition(still_user_defined, find_project_site())
    {
        eprintln!(
            "note: '{name}' is still defined in {}",
            scope_display(survivor_scope, &remaining)
        );
    }

    Ok(())
}

async fn run_doctor(json: bool, name: Option<String>) -> Result<()> {
    let cwd = current_dir_or_exit();
    let report = echo_agent_runtime::mcp_doctor::run_doctor(&cwd, name.as_deref()).await;

    if let Some(ref filter) = name
        && report.servers.is_empty()
    {
        eprintln!("MCP server '{}' not found.", filter);
        if !report.all_server_names.is_empty() {
            eprintln!("Available servers: {}", report.all_server_names.join(", "));
        }
        std::process::exit(1);
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        echo_agent_runtime::mcp_doctor::print_report(&report);
    }

    if report.failing_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Command, PagerArgs};
    use clap::Parser as _;

    fn parse_add(argv: &[&str]) -> AddArgs {
        let args = PagerArgs::try_parse_from(argv).expect("args should parse");
        match args.command {
            Some(Command::Mcp(McpArgs {
                command: McpCommand::Add(add),
            })) => add,
            other => panic!("expected mcp add, got {other:?}"),
        }
    }

    #[test]
    fn add_accepts_trailing_command_after_double_dash() {
        // The invocation from the original report: a stdio server whose
        // command follows `--`, with an explicit transport.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "stdio",
            "xcode",
            "--",
            "xcrun",
            "mcpbridge",
        ]);
        assert_eq!(add.name, "xcode");
        assert_eq!(add.transport, Some(McpTransport::Stdio));
        assert_eq!(add.scope, McpScope::User);

        let resolved = resolve_add(&add).expect("resolves to stdio");
        match resolved.transport {
            McpServerTransportConfig::Stdio { command, args, .. } => {
                assert_eq!(command, "xcrun");
                assert_eq!(args, vec!["mcpbridge".to_string()]);
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }
    }

    #[test]
    fn add_passes_hyphen_flags_and_repeated_env_to_server() {
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "fs",
            "-e",
            "FOO=bar",
            "-e",
            "BAZ=qux=quux",
            "--",
            "npx",
            "-y",
            "@modelcontextprotocol/server-filesystem",
            "/allowed/dir",
        ]);

        let resolved = resolve_add(&add).expect("resolves to stdio");
        match resolved.transport {
            McpServerTransportConfig::Stdio {
                command, args, env, ..
            } => {
                assert_eq!(command, "npx");
                assert_eq!(args[0], "-y");
                let env = env.expect("env should be set");
                assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
                // Values may themselves contain '='.
                assert_eq!(env.get("BAZ").map(String::as_str), Some("qux=quux"));
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }
    }

    #[test]
    fn add_hyphen_flag_without_double_dash_is_rejected() {
        let err = PagerArgs::try_parse_from(["echoagent", "mcp", "add", "fs", "npx", "-y"])
            .expect_err("hyphen args must be escaped with --");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn add_http_with_headers() {
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "http",
            "api",
            "https://mcp.example.com/mcp",
            "--header",
            "Authorization: Bearer tok",
        ]);

        let resolved = resolve_add(&add).expect("resolves to http");
        // Explicit --transport http must not fire the inference info line.
        assert!(resolved.warnings.is_empty());
        match resolved.transport {
            McpServerTransportConfig::StreamableHttp {
                url,
                transport_type,
                headers,
                ..
            } => {
                assert_eq!(url, "https://mcp.example.com/mcp");
                assert_eq!(transport_type, None);
                let headers = headers.expect("headers should be set");
                assert_eq!(
                    headers.get("Authorization").map(String::as_str),
                    Some("Bearer tok")
                );
            }
            other => panic!("expected http transport, got {other:?}"),
        }
    }

    #[test]
    fn add_sse_sets_transport_type() {
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "sse",
            "linear",
            "https://mcp.linear.app/sse",
        ]);
        let resolved = resolve_add(&add).expect("resolves to sse");
        assert_eq!(resolved.kind, McpTransport::Sse);
        match resolved.transport {
            McpServerTransportConfig::StreamableHttp { transport_type, .. } => {
                assert_eq!(transport_type.as_deref(), Some("sse"));
            }
            other => panic!("expected sse transport, got {other:?}"),
        }
    }

    #[test]
    fn add_http_transport_with_non_url_command_is_rejected() {
        // Previously this silently stored `xcrun` as an HTTP URL.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "http",
            "xcode",
            "--",
            "xcrun",
            "mcpbridge",
        ]);
        let err = resolve_add(&add).expect_err("non-URL with http transport must fail");
        assert!(err.to_string().contains("Invalid URL"), "got: {err}");
    }

    #[test]
    fn add_explicit_stdio_keeps_url_looking_command_as_stdio() {
        // Previously URL sniffing overrode an explicit stdio transport.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "stdio",
            "proxy",
            "--",
            "https://example.com/fetcher",
        ]);
        let resolved = resolve_add(&add).expect("explicit stdio must stay stdio");
        assert!(resolved.warnings.is_empty());
        assert!(matches!(
            resolved.transport,
            McpServerTransportConfig::Stdio { .. }
        ));
    }

    #[test]
    fn add_infers_http_for_bare_positional_url() {
        for url in ["https://mcp.example.com/mcp", "http://mcp.example.com/mcp"] {
            let add = parse_add(&["echoagent", "mcp", "add", "api", url]);
            let resolved = resolve_add(&add).expect("bare http(s) URL infers http");
            assert_eq!(resolved.kind, McpTransport::Http);
            match resolved.transport {
                McpServerTransportConfig::StreamableHttp {
                    url: stored,
                    transport_type,
                    ..
                } => {
                    assert_eq!(stored, url);
                    assert_eq!(transport_type, None);
                }
                other => panic!("expected http transport, got {other:?}"),
            }
            assert_eq!(resolved.warnings.len(), 1);
            assert!(
                resolved.warnings[0].contains("No --transport given"),
                "got: {}",
                resolved.warnings[0]
            );
        }
    }

    #[test]
    fn add_infers_http_with_headers() {
        // Previously this bailed: stdio was assumed and --header is remote-only.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "api",
            "https://mcp.example.com/mcp",
            "--header",
            "Authorization: Bearer tok",
        ]);
        let resolved = resolve_add(&add).expect("URL with headers infers http");
        assert_eq!(resolved.kind, McpTransport::Http);
        match resolved.transport {
            McpServerTransportConfig::StreamableHttp { headers, .. } => {
                let headers = headers.expect("headers should be set");
                assert_eq!(
                    headers.get("Authorization").map(String::as_str),
                    Some("Bearer tok")
                );
            }
            other => panic!("expected http transport, got {other:?}"),
        }
    }

    #[test]
    fn add_default_transport_warns_on_url_looking_command() {
        // Scheme-less URL-looking commands are not inferred; they get
        // http:// prepended so the suggested command passes URL validation.
        let add = parse_add(&["echoagent", "mcp", "add", "local", "localhost:3000"]);
        let resolved = resolve_add(&add).expect("localhost command warns");
        assert!(matches!(
            resolved.transport,
            McpServerTransportConfig::Stdio { .. }
        ));
        assert_eq!(resolved.warnings.len(), 1);
        assert!(
            resolved.warnings[0].contains("--transport http local http://localhost:3000"),
            "got: {}",
            resolved.warnings[0]
        );

        // Extra args or --env mean a command; URLs stay stdio with a warning.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "api",
            "https://mcp.example.com/mcp",
            "--",
            "extra",
        ]);
        let resolved = resolve_add(&add).expect("URL with args stays stdio");
        assert!(matches!(
            resolved.transport,
            McpServerTransportConfig::Stdio { .. }
        ));
        assert_eq!(resolved.warnings.len(), 1);
        assert!(resolved.warnings[0].contains("--transport http"));

        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "api",
            "-e",
            "K=v",
            "https://mcp.example.com/mcp",
        ]);
        let resolved = resolve_add(&add).expect("URL with env stays stdio");
        assert!(matches!(
            resolved.transport,
            McpServerTransportConfig::Stdio { .. }
        ));
        assert_eq!(resolved.warnings.len(), 1);
    }

    #[test]
    fn add_scope_project_parses_and_invalid_scope_is_rejected() {
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "-s",
            "project",
            "fs",
            "--",
            "npx",
            "pkg",
        ]);
        assert_eq!(add.scope, McpScope::Project);

        let err = PagerArgs::try_parse_from([
            "echoagent",
            "mcp",
            "add",
            "-s",
            "local",
            "fs",
            "--",
            "npx",
            "pkg",
        ])
        .expect_err("local is not a echoagent scope");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn add_legacy_flag_forms_still_parse() {
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "oldfs",
            "--command",
            "npx",
            "--args",
            "@foo/bar",
            "/path",
        ]);
        let resolved = resolve_add(&add).expect("legacy stdio form resolves");
        match resolved.transport {
            McpServerTransportConfig::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["@foo/bar".to_string(), "/path".to_string()]);
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }

        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "remote",
            "--url",
            "https://mcp.example.com/sse",
            "--type",
            "sse",
        ]);
        let resolved = resolve_add(&add).expect("legacy url form resolves");
        // Legacy --url defaults to HTTP on its own path, not via inference.
        assert!(resolved.warnings.is_empty());
        match resolved.transport {
            McpServerTransportConfig::StreamableHttp {
                url,
                transport_type,
                ..
            } => {
                assert_eq!(url, "https://mcp.example.com/sse");
                assert_eq!(transport_type.as_deref(), Some("sse"));
            }
            other => panic!("expected http transport, got {other:?}"),
        }
    }

    #[test]
    fn add_legacy_command_conflicts_with_positional() {
        let err = PagerArgs::try_parse_from([
            "echoagent",
            "mcp",
            "add",
            "fs",
            "npx",
            "--command",
            "other-npx",
        ])
        .expect_err("--command and a positional command are mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn add_legacy_multi_value_env_is_rejected() {
        // Pre-parity --env was greedy (`--env A=1 B=2`); with --command the
        // stray pair now lands in the positional and trips the source group.
        let err = PagerArgs::try_parse_from([
            "echoagent",
            "mcp",
            "add",
            "github",
            "--command",
            "npx",
            "--args",
            "@foo/bar",
            "--env",
            "A=1",
            "B=2",
        ])
        .expect_err("greedy --env must no longer parse");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);

        // Without --command the stray pair used to be silently written as the
        // command; resolve_add must reject it with migration guidance.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "pg",
            "--env",
            "A=1",
            "B=2",
            "--",
            "npx",
            "-y",
            "server",
        ]);
        let err = resolve_add(&add).expect_err("env-shaped command must fail");
        assert!(err.to_string().contains("-e A=1 -e B=2"), "got: {err}");
    }

    #[test]
    fn add_legacy_url_and_type_misuse_is_rejected() {
        // --url with an explicit stdio transport used to silently store the
        // URL as a stdio command.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "foo",
            "--url",
            "https://mcp.example.com/mcp",
            "-t",
            "stdio",
        ]);
        let err = resolve_add(&add).expect_err("--url with stdio transport must fail");
        assert!(err.to_string().contains("--url"), "got: {err}");

        // --type without --url used to be silently ignored.
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "bar",
            "https://x.example/sse",
            "--type",
            "sse",
        ]);
        let err = resolve_add(&add).expect_err("--type without --url must fail");
        assert!(err.to_string().contains("--transport"), "got: {err}");
    }

    #[test]
    fn add_validates_name_env_and_headers() {
        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "fs",
            "-e",
            "NOT_A_PAIR",
            "--",
            "npx",
        ]);
        let err = resolve_add(&add).expect_err("malformed env must fail");
        assert!(
            err.to_string()
                .contains("Invalid environment variable format"),
            "got: {err}"
        );

        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "http",
            "api",
            "https://mcp.example.com",
            "-H",
            "NoColonHere",
        ]);
        let err = resolve_add(&add).expect_err("malformed header must fail");
        assert!(
            err.to_string().contains("Invalid header format"),
            "got: {err}"
        );

        let add = parse_add(&["echoagent", "mcp", "add", "bad name!", "--", "npx"]);
        let err = resolve_add(&add).expect_err("invalid name must fail");
        assert!(err.to_string().contains("Invalid name"), "got: {err}");
    }

    #[test]
    fn add_rejects_mismatched_options_per_transport() {
        let add = parse_add(&["echoagent", "mcp", "add", "fs", "-H", "X: y", "--", "npx"]);
        let err = resolve_add(&add).expect_err("--header is remote-only");
        assert!(err.to_string().contains("--header"), "got: {err}");

        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "http",
            "api",
            "https://mcp.example.com",
            "-e",
            "K=v",
        ]);
        let err = resolve_add(&add).expect_err("--env is stdio-only");
        assert!(err.to_string().contains("--env"), "got: {err}");

        let add = parse_add(&[
            "echoagent",
            "mcp",
            "add",
            "--transport",
            "http",
            "api",
            "--",
            "https://mcp.example.com",
            "extra",
        ]);
        let err = resolve_add(&add).expect_err("extra args after a URL must fail");
        assert!(
            err.to_string().contains("Unexpected arguments"),
            "got: {err}"
        );
    }

    #[test]
    fn add_requires_a_source_for_each_transport() {
        let add = parse_add(&["echoagent", "mcp", "add", "fs"]);
        let err = resolve_add(&add).expect_err("stdio without a command must fail");
        assert!(
            err.to_string().contains("command is required"),
            "got: {err}"
        );

        let add = parse_add(&["echoagent", "mcp", "add", "--transport", "http", "api"]);
        let err = resolve_add(&add).expect_err("http without a URL must fail");
        assert!(err.to_string().contains("URL is required"), "got: {err}");
    }

    #[test]
    fn remove_accepts_optional_scope() {
        let args = PagerArgs::try_parse_from(["echoagent", "mcp", "remove", "fs", "-s", "project"])
            .expect("remove with scope parses");
        match args.command {
            Some(Command::Mcp(McpArgs {
                command: McpCommand::Remove { name, scope },
            })) => {
                assert_eq!(name, "fs");
                assert_eq!(scope, Some(McpScope::Project));
            }
            other => panic!("expected mcp remove, got {other:?}"),
        }
    }

    #[test]
    fn gateway_cli_toggle_names_are_rejected() {
        assert!(is_gateway_cli_toggle_name("managed_gateway:linear"));
        assert!(is_gateway_cli_toggle_name("other:colon"));
        assert!(!is_gateway_cli_toggle_name("echo_agent_com_slack"));
        assert!(!is_gateway_cli_toggle_name("user-slack"));
    }

    #[test]
    fn echo_agent_com_known_only_with_toml_definition() {
        // Unique name: `echo_agent_home()` is process-wide OnceLock, so ECHO_AGENT_HOME
        // EnvGuard is a no-op if another test already resolved it. A leftover
        // `echo_agent_com_*` in the real ~/.echo-agent disabled list would fail an orphan
        // assertion on a well-known name.
        let name = format!("echo_agent_com_orphan_{}", uuid::Uuid::new_v4().as_simple());

        let orphan = tempfile::tempdir().unwrap();
        git2::Repository::init(orphan.path()).unwrap();
        assert!(
            !mcp_server_is_known(&name, orphan.path()),
            "orphan echo_agent_com_* must not be known by prefix"
        );

        let defined = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(defined.path().join(".echo-agent")).unwrap();
        std::fs::write(
            defined.path().join(".echo-agent").join("config.toml"),
            format!(
                r#"
[mcp_servers.{name}]
url = "https://mcp.example.test/sse"
"#
            ),
        )
        .unwrap();
        git2::Repository::init(defined.path()).unwrap();
        assert!(
            mcp_server_is_known(&name, defined.path()),
            "TOML-defined {name} must be known"
        );
    }

    #[test]
    fn enable_and_disable_parse_name() {
        let args = PagerArgs::try_parse_from(["echoagent", "mcp", "enable", "user-grafana"])
            .expect("enable should parse");
        match args.command {
            Some(Command::Mcp(McpArgs {
                command: McpCommand::Enable { name },
            })) => assert_eq!(name, "user-grafana"),
            other => panic!("expected mcp enable, got {other:?}"),
        }

        let args = PagerArgs::try_parse_from(["echoagent", "mcp", "disable", "user-slack"])
            .expect("disable should parse");
        match args.command {
            Some(Command::Mcp(McpArgs {
                command: McpCommand::Disable { name },
            })) => assert_eq!(name, "user-slack"),
            other => panic!("expected mcp disable, got {other:?}"),
        }
    }

    #[test]
    fn enable_disable_require_name() {
        let err = PagerArgs::try_parse_from(["echoagent", "mcp", "enable"])
            .expect_err("enable without name must fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        let err = PagerArgs::try_parse_from(["echoagent", "mcp", "disable"])
            .expect_err("disable without name must fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn select_remove_site_covers_scope_presence_matrix() {
        let user = echo_agent_runtime::util::config::user_config_path();
        let project = PathBuf::from("/repo/.echo-agent/config.toml");

        // No scope: single hits resolve, both scopes is ambiguous, neither is
        // not found.
        assert_eq!(
            select_remove_site(true, None, None),
            Ok((McpScope::User, user.clone()))
        );
        assert_eq!(
            select_remove_site(false, Some(project.clone()), None),
            Ok((McpScope::Project, project.clone()))
        );
        assert_eq!(
            select_remove_site(true, Some(project.clone()), None),
            Err(RemoveError::Ambiguous {
                project_path: project.clone()
            })
        );
        assert_eq!(
            select_remove_site(false, None, None),
            Err(RemoveError::NotFound)
        );

        // Explicit scope: only that scope is consulted.
        assert_eq!(
            select_remove_site(true, Some(project.clone()), Some(McpScope::User)),
            Ok((McpScope::User, user))
        );
        assert_eq!(
            select_remove_site(false, Some(project.clone()), Some(McpScope::Project)),
            Ok((McpScope::Project, project.clone()))
        );
        assert_eq!(
            select_remove_site(false, Some(project), Some(McpScope::User)),
            Err(RemoveError::NotFound)
        );
        assert_eq!(
            select_remove_site(true, None, Some(McpScope::Project)),
            Err(RemoveError::NotFound)
        );
    }

    #[test]
    fn surviving_definition_prefers_project_then_user() {
        let user = echo_agent_runtime::util::config::user_config_path();
        let project = PathBuf::from("/repo/.echo-agent/config.toml");

        // The mirror of the remove note: a user-scope delete with a project
        // survivor (and vice versa) must still report the remaining site.
        assert_eq!(
            surviving_definition(false, Some(project.clone())),
            Some((McpScope::Project, project.clone()))
        );
        assert_eq!(
            surviving_definition(true, None),
            Some((McpScope::User, user))
        );
        // Project shadows user when both survive; nothing left is silent.
        assert_eq!(
            surviving_definition(true, Some(project.clone())),
            Some((McpScope::Project, project))
        );
        assert_eq!(surviving_definition(false, None), None);
    }
}
