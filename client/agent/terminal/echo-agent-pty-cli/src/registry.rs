//! Named session registry stored at ~/.local/state/echo-agent-pty/sessions/.
#![allow(clippy::disallowed_methods)] // talks to the local pty daemon; TLS policy N/A

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Information about a registered session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub port: u16,
    pub pid: Option<u32>,
    pub command: Vec<String>,
    pub cwd: String,
    pub started_at: String,
}

/// Get the session registry directory.
fn registry_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("ECHO_AGENT_PTY_SESSION_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let state_dir = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .context("cannot determine state directory")?;
    Ok(state_dir.join("echo-agent-pty").join("sessions"))
}

/// Register a named session.
pub fn register_session(name: &str, info: &SessionInfo) -> Result<()> {
    let dir = registry_dir()?;
    fs::create_dir_all(&dir).context("failed to create session registry directory")?;

    let path = dir.join(format!("{name}.json"));
    let json = serde_json::to_string_pretty(info)?;

    // Atomic write: write to temp file, then rename.
    let tmp = dir.join(format!(".{name}.json.tmp"));
    fs::write(&tmp, &json).context("failed to write session file")?;
    fs::rename(&tmp, &path).context("failed to rename session file")?;

    Ok(())
}

/// Look up a named session.
pub fn lookup_session(name: &str) -> Result<SessionInfo> {
    let dir = registry_dir()?;
    let path = dir.join(format!("{name}.json"));

    if !path.exists() {
        bail!("session '{name}' not found");
    }

    let json = fs::read_to_string(&path).context("failed to read session file")?;
    let info: SessionInfo = serde_json::from_str(&json).context("failed to parse session file")?;

    Ok(info)
}

/// Remove a named session.
pub fn unregister_session(name: &str) -> Result<()> {
    let dir = registry_dir()?;
    let path = dir.join(format!("{name}.json"));
    if path.exists() {
        fs::remove_file(&path).context("failed to remove session file")?;
    }
    Ok(())
}

/// Check whether a registered session's echo-agent-pty server is reachable.
///
/// Probes `GET /query/status` and requires a 200 with the echo-agent-pty status body
/// shape — a bare TCP connect (or bare 200) would misread an unrelated
/// process on a recycled port as live. Not PID-based because the recorded
/// PID is the child, which may exit while a `--linger` server is still up.
pub async fn server_alive(port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    match client
        .get(format!("http://127.0.0.1:{port}/query/status"))
        .send()
        .await
    {
        // Require the status body shape, not just a 200, so wildcard-200 servers read dead.
        Ok(resp) if resp.status() == reqwest::StatusCode::OK => resp
            .json::<serde_json::Value>()
            .await
            .is_ok_and(|v| v.get("size").is_some()),
        _ => false,
    }
}

/// List all registered sessions.
pub fn list_sessions() -> Result<Vec<(String, SessionInfo)>> {
    let dir = registry_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for entry in fs::read_dir(&dir).context("failed to read session directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if name.starts_with('.') {
                continue; // skip temp files
            }
            if let Ok(json) = fs::read_to_string(&path)
                && let Ok(info) = serde_json::from_str::<SessionInfo>(&json)
            {
                sessions.push((name, info));
            }
        }
    }

    Ok(sessions)
}
