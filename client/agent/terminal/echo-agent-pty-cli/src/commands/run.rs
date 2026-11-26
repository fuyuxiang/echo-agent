//! `echo-agent-pty run` — spawn a PTY session and start the HTTP server.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::net::TcpListener;

use echo_agent_pty::pty::PtyConfig;
use echo_agent_pty::server;
use echo_agent_pty::session::{PtySession, SessionConfig};

use crate::registry;

/// Run the `echo-agent-pty run` command.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    command: Vec<String>,
    width: u16,
    height: u16,
    cwd: Option<PathBuf>,
    env_vars: Vec<String>,
    port: u16,
    name: Option<String>,
    force: bool,
    timeout: Option<u64>,
    linger: bool,
    quiet: bool,
) -> Result<()> {
    // Refuse to take over a name whose server is still reachable unless --force; stale entries are replaced.
    if let Some(ref session_name) = name
        && !force
        && let Ok(existing) = registry::lookup_session(session_name)
        && registry::server_alive(existing.port).await
    {
        bail!(
            "session '{session_name}' is already running on port {} (use --force to replace it)",
            existing.port
        );
    }

    // Parse env vars.
    let mut env = HashMap::new();
    for var in &env_vars {
        if let Some((k, v)) = var.split_once('=') {
            env.insert(k.to_string(), v.to_string());
        }
    }

    let cwd_str = cwd
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".into());

    let config = SessionConfig {
        pty: PtyConfig {
            command: command.clone(),
            cols: width,
            rows: height,
            cwd,
            env,
        },
        timeout,
        linger,
    };

    // Start the session.
    let session = PtySession::start(config).await?;
    let pid = session.status_basic().1;

    // Build the HTTP server.
    let router = server::build_router(session);

    // Bind to the requested port.
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)
        .await
        .context("failed to bind TCP listener")?;
    let actual_addr = listener.local_addr()?;
    let actual_port = actual_addr.port();

    // Register named session.
    if let Some(ref session_name) = name {
        let info = registry::SessionInfo {
            port: actual_port,
            pid,
            command: command.clone(),
            cwd: cwd_str,
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        registry::register_session(session_name, &info)?;
    }

    if !quiet {
        eprintln!("Command: {}", command.join(" "));
        if let Some(p) = pid {
            eprintln!("PID: {p}");
        }
        eprintln!("Server listening on port: {actual_port}");
    } else {
        println!("{actual_port}");
    }

    // Serve until shutdown.
    let shutdown_result = axum::serve(listener, router)
        .await
        .context("HTTP server error");

    // Clean up only a registration that still points at this server; a --force takeover may have replaced it.
    if let Some(ref session_name) = name
        && let Ok(info) = registry::lookup_session(session_name)
        && info.port == actual_port
    {
        let _ = registry::unregister_session(session_name);
    }

    shutdown_result
}
