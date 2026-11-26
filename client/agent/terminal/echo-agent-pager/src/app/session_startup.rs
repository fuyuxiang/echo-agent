//! Canonical session-selection CLI intent.
//!
//! Built once from CLI flags and consumed by interactive resolve, the event
//! loop, and headless mode so resume / new-with-id / fork are not re-derived
//! in three places.
use super::cli::PagerArgs;
use std::path::{Path, PathBuf};
/// Session-create intent deferred until [`AppView::session_startup_allowed`].
///
/// Replaces the prior matrix of `startup_load_session` + cwd + `startup_fork`
/// tuple + ad-hoc preferred-only replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredSessionStartup {
    /// Strict resume (`-r` / `-c` / picker load).
    Load {
        session_id: String,
        session_cwd: Option<PathBuf>,
        /// Conversation-entry bit (`source == "conversation"`), not sticky `--chat`.
        chat_kind: bool,
    },
    /// Client-chosen id (`--session-id`), also mirrored into `preferred_session_id`.
    NewWithId { session_id: String },
    /// Startup `--fork-session` after parent resolve.
    Fork {
        parent_session_id: String,
        parent_cwd: Option<PathBuf>,
        new_session_id: Option<String>,
    },
    /// Fresh plain EchoAgent session whose first prompt resumes a foreign tool session.
    ForeignResume {
        tool: echo_agent_foreign_sessions::ForeignSessionTool,
        native_id: String,
    },
}
/// One owner for every action deferred behind auth/folder-trust startup gates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeferredStartupActions {
    pub session: Option<DeferredSessionStartup>,
    pub preferred_session_id: Option<String>,
    pub worktree: bool,
    pub worktree_label: Option<String>,
    pub worktree_ref: Option<String>,
    pub new_session: bool,
    pub prompt: Option<String>,
    pub open_dashboard: bool,
    pub pending_chat: bool,
    /// Welcome history local-disk bypass persisted across the startup gate.
    #[cfg(feature = "local-workspace")]
    pub history_load_as_build: bool,
}
impl DeferredStartupActions {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
    pub fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}
/// Build `echo.agent/session/fork` params shared by TUI effects and headless.
///
/// `new_cwd` is the write namespace for the child (parent session cwd when
/// cross-cwd); preflight must use the same path via [`effective_fork_new_cwd`].
pub fn fork_session_params(
    parent_session_id: &str,
    parent_cwd: &Path,
    new_session_id: Option<&str>,
    parent_is_worktree: bool,
) -> serde_json::Value {
    let parent_cwd_str = parent_cwd.to_string_lossy().into_owned();
    let source_cwd = echo_agent_runtime::session::resolve_local_session_any_cwd(parent_session_id)
        .unwrap_or_else(|| parent_cwd_str.clone());
    let mut payload = serde_json::json!({
        "sourceSessionId": parent_session_id,
        "sourceCwd": source_cwd,
        "newCwd": parent_cwd_str.clone(),
        "sessionKind": "fork",
    });
    if let Some(nid) = new_session_id {
        payload["newSessionId"] = serde_json::Value::String(nid.to_string());
    }
    if parent_is_worktree {
        payload["sourceWorkspaceDir"] = serde_json::Value::String(parent_cwd_str);
    }
    payload
}
/// Whether a persisted session (or its cwd) is worktree-backed.
/// Mirrors in-session `/fork` reading `agent.session.is_worktree`.
pub fn parent_session_is_worktree(session_id: &str, cwd: &Path) -> bool {
    let cwd_str = cwd.to_string_lossy();
    let sessions_root =
        echo_agent_runtime::util::echo_agent_home::echo_agent_home().join("sessions");
    let encoded = echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd_str);
    let summary_path = sessions_root
        .join(encoded)
        .join(session_id)
        .join("summary.json");
    if let Ok(bytes) = std::fs::read(&summary_path)
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        if v.get("session_kind").and_then(|k| k.as_str()) == Some("worktree") {
            return true;
        }
        if v.get("source_workspace_dir")
            .and_then(|k| k.as_str())
            .is_some_and(|s| !s.is_empty())
        {
            return true;
        }
        if v.get("worktree_label")
            .and_then(|k| k.as_str())
            .is_some_and(|s| !s.is_empty())
        {
            return true;
        }
    }
    if let Some(info) = crate::git_info::compute_cwd_git_info(cwd) {
        return info.is_worktree;
    }
    for dir in cwd.ancestors() {
        let git = dir.join(".git");
        if git.is_file() {
            return true;
        }
        if git.is_dir() {
            return std::fs::read_to_string(git.join("echo-agent-worktree-source"))
                .is_ok_and(|s| !s.trim().is_empty());
        }
    }
    false
}
/// Parse `newSessionId` from an `echo.agent/session/fork` ACP response body.
pub fn fork_response_new_session_id(resp_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(resp_json).unwrap_or_default();
    if v.get("error").is_some_and(|e| !e.is_null()) {
        return None;
    }
    v.get("newSessionId")
        .and_then(|x| x.as_str())
        .or_else(|| {
            v.get("result")
                .and_then(|r| r.get("newSessionId"))
                .and_then(|x| x.as_str())
        })
        .map(|s| s.to_string())
}
/// Error string from a fork response, if present.
pub fn fork_response_error(resp_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(resp_json).ok()?;
    v.get("error")
        .filter(|e| !e.is_null())
        .map(|e| e.to_string())
}
/// Pure interpretation of session-selection CLI flags (no I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStartupIntent {
    /// Fresh session; agent picks the ID.
    NewAuto,
    /// Fresh session with a client-chosen ID (must not exist under cwd).
    NewWithId { session_id: String },
    /// Load an existing session (strict — never create).
    Resume {
        /// `None` means resolve most-recent for cwd at materialize time.
        session_id: Option<String>,
        most_recent_for_cwd: bool,
    },
    /// Resolve source like resume, then fork; optional forced ID for the child.
    ForkFrom {
        source_session_id: Option<String>,
        most_recent_for_cwd: bool,
        new_session_id: Option<String>,
    },
}
/// Flag combinations that clap allows but we reject at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFlagError {
    /// `--session-id` with resume/continue/load without `--fork-session`.
    SessionIdRequiresFork,
    /// `--fork-session` without resume/continue/load.
    ForkRequiresResumeOrContinue,
    /// `--fork-session` with `--worktree` (not supported yet).
    ForkWithWorktree,
}
impl std::fmt::Display for StartupFlagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionIdRequiresFork => {
                write!(
                    f,
                    "Error: --session-id can only be used with --continue or --resume if --fork-session is also specified."
                )
            }
            Self::ForkRequiresResumeOrContinue => {
                write!(f, "Error: --fork-session requires --resume or --continue.")
            }
            Self::ForkWithWorktree => {
                write!(
                    f,
                    "Error: --fork-session cannot be combined with --worktree."
                )
            }
        }
    }
}
impl std::error::Error for StartupFlagError {}
/// Inputs shared by interactive CLI and headless (no clap dependency).
#[derive(Debug, Clone, Copy)]
pub struct SessionStartupFlags<'a> {
    pub session_id: Option<&'a str>,
    /// Explicit resume id from `-r` / `--resume` (not the empty most-recent sentinel).
    pub resume_session_id: Option<&'a str>,
    /// `--resume` with no value (most recent for cwd).
    pub resume_most_recent: bool,
    pub continue_last_session: bool,
    pub fork_session: bool,
    /// True when `--worktree` is set (any label, including empty default).
    pub has_worktree: bool,
}
/// Classify session-selection flags into a single intent (no I/O).
pub fn session_startup_intent_from_flags(
    f: SessionStartupFlags<'_>,
) -> Result<SessionStartupIntent, StartupFlagError> {
    let has_resume_id = f.resume_session_id.is_some();
    let most_recent = f.resume_most_recent || f.continue_last_session;
    let has_resume_or_continue = has_resume_id || most_recent;
    if f.fork_session && f.has_worktree {
        return Err(StartupFlagError::ForkWithWorktree);
    }
    if f.fork_session && !has_resume_or_continue {
        return Err(StartupFlagError::ForkRequiresResumeOrContinue);
    }
    if let Some(sid) = f.session_id {
        if has_resume_or_continue && !f.fork_session {
            return Err(StartupFlagError::SessionIdRequiresFork);
        }
        if f.fork_session {
            return Ok(SessionStartupIntent::ForkFrom {
                source_session_id: f.resume_session_id.map(|s| s.to_owned()),
                most_recent_for_cwd: most_recent && !has_resume_id,
                new_session_id: Some(sid.to_owned()),
            });
        }
        return Ok(SessionStartupIntent::NewWithId {
            session_id: sid.to_owned(),
        });
    }
    if f.fork_session {
        return Ok(SessionStartupIntent::ForkFrom {
            source_session_id: f.resume_session_id.map(|s| s.to_owned()),
            most_recent_for_cwd: most_recent && !has_resume_id,
            new_session_id: None,
        });
    }
    if let Some(id) = f.resume_session_id {
        return Ok(SessionStartupIntent::Resume {
            session_id: Some(id.to_owned()),
            most_recent_for_cwd: false,
        });
    }
    if most_recent {
        return Ok(SessionStartupIntent::Resume {
            session_id: None,
            most_recent_for_cwd: true,
        });
    }
    Ok(SessionStartupIntent::NewAuto)
}
impl PagerArgs {
    /// Classify session-selection flags into a single intent (no I/O).
    pub fn session_startup_intent(&self) -> Result<SessionStartupIntent, StartupFlagError> {
        session_startup_intent_from_flags(SessionStartupFlags {
            session_id: self.session_id.as_deref(),
            resume_session_id: self.session_to_resume(),
            resume_most_recent: self.resume_most_recent(),
            continue_last_session: self.continue_last_session,
            fork_session: self.fork_session,
            has_worktree: self.worktree.is_some(),
        })
    }
}
/// User-facing refusal when process-wide `--chat` would open a local Build disk row.
pub const CHAT_MODE_LOCAL_BUILD_REFUSAL: &str = "cannot open a local Build session while --chat is active; \
resume a conversation or start a new chat (/chat)";
/// User-facing error when `--chat` is combined with leader mode.
pub const CHAT_MODE_LEADER_CONFLICT: &str = "gateway chat mode (--chat) cannot run with leader mode; \
pass --no-leader or disable [cli] use_leader in config";
/// Startup guard used by TUI `run` (and unit-tested): sticky `--chat` + leader is invalid.
#[inline]
pub fn chat_mode_conflicts_with_leader(chat: bool, use_leader: bool) -> bool {
    chat && use_leader
}
/// User-facing error for `--fork-session` + `--chat` (forking is a Build disk
/// concept; chat sessions have no local copy to fork).
pub const CHAT_MODE_FORK_CONFLICT: &str = "--fork-session is not supported with --chat";
/// User-facing error for `--restore-code` + `--chat` (code restore is a
/// Build/worktree concept; chat sessions carry no codebase).
pub const CHAT_MODE_RESTORE_CODE_CONFLICT: &str = "--restore-code is not supported with --chat";
/// Flag validation: Build-lifecycle flags that cannot combine with `--chat`.
/// Always `None` when `chat_mode` is false, so call sites need no `cfg`.
pub fn chat_mode_flag_conflict(
    chat_mode: bool,
    fork_session: bool,
    restore_code: bool,
) -> Option<&'static str> {
    if !chat_mode {
        return None;
    }
    if fork_session {
        return Some(CHAT_MODE_FORK_CONFLICT);
    }
    if restore_code {
        return Some(CHAT_MODE_RESTORE_CODE_CONFLICT);
    }
    None
}
/// Env: enable local workspace without CLI flags (`1`). Mode defaults to `own`
/// unless `ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE` / attach server id is set.
#[cfg(feature = "local-workspace")]
pub const ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ENV: &str = "ECHO_AGENT_CHAT_LOCAL_WORKSPACE";
#[cfg(feature = "local-workspace")]
pub const ECHO_AGENT_CHAT_LOCAL_WORKSPACE_CWD_ENV: &str = "ECHO_AGENT_CHAT_LOCAL_WORKSPACE_CWD";
#[cfg(feature = "local-workspace")]
pub const ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE_ENV: &str = "ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE";
#[cfg(feature = "local-workspace")]
pub const ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID_ENV: &str =
    "ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID";
#[cfg(feature = "local-workspace")]
pub const ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME_ENV: &str =
    "ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME";
/// Skip interactive first-run confirm (still prints the banner).
#[cfg(feature = "local-workspace")]
pub const ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK_ENV: &str = "ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK";
/// Startup banner / first-run copy.
#[cfg(feature = "local-workspace")]
pub const LOCAL_WORKSPACE_BANNER: &str =
    "Local workspace runs tools on this machine (FS confined to <cwd>).";
#[cfg(feature = "local-workspace")]
pub const LOCAL_WORKSPACE_ATTACH_NEEDS_SERVER_ID: &str = "local-workspace attach requires --local-workspace-attach=<server_id> \
     (or ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID)";
#[cfg(feature = "local-workspace")]
pub const LOCAL_WORKSPACE_REQUIRES_CHAT: &str = "local-workspace flags/env require --chat";
#[cfg(feature = "local-workspace")]
pub const LOCAL_WORKSPACE_HOME_DENIED: &str =
    "local-workspace cwd may not be / or $HOME unless ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME=1";
#[cfg(feature = "local-workspace")]
pub const LOCAL_WORKSPACE_HITL_HINT: &str = "Permission prompts for local workspace tools apply to your machine. \
     Local workspace replaces the chat sandbox.";
#[cfg(feature = "local-workspace")]
pub const LOCAL_WORKSPACE_ACK_REQUIRED: &str = "local-workspace requires interactive confirm, ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK=1, or an ack file";
/// Declared advertised tool ids for attach FS-only check (comma-separated).
#[cfg(feature = "local-workspace")]
pub const ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS_ENV: &str =
    "ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS";
#[cfg(feature = "local-workspace")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalWorkspaceMode {
    Own,
    Attach,
}
#[cfg(feature = "local-workspace")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspaceConfig {
    pub mode: LocalWorkspaceMode,
    pub cwd: Option<std::path::PathBuf>,
    pub server_id: Option<String>,
}
#[cfg(feature = "local-workspace")]
static ACTIVE_LOCAL_WORKSPACE: std::sync::Mutex<Option<LocalWorkspaceConfig>> =
    std::sync::Mutex::new(None);
#[cfg(feature = "local-workspace")]
pub fn set_active_local_workspace(cfg: Option<LocalWorkspaceConfig>) -> anyhow::Result<()> {
    let mut guard = ACTIVE_LOCAL_WORKSPACE.lock().map_err(|_| {
        anyhow::anyhow!("local-workspace intent mutex poisoned; refuse attach (fail closed)")
    })?;
    tracing::info!(
        target: crate::views::welcome::workspace_mode::WORKSPACE_MODE_LOG,
        event = if cfg.is_some() {
            "process_stamp_set"
        } else {
            "process_stamp_cleared"
        },
        mode = cfg.as_ref().map(|c| format!("{:?}", c.mode)),
        server_id = cfg.as_ref().and_then(|c| c.server_id.as_deref()),
        cwd = cfg.as_ref().and_then(|c| c.cwd.as_ref().map(|p| p.display().to_string())),
        "local-workspace process-wide intent stamp"
    );
    *guard = cfg;
    Ok(())
}
#[cfg(feature = "local-workspace")]
pub fn active_local_workspace() -> anyhow::Result<Option<LocalWorkspaceConfig>> {
    ACTIVE_LOCAL_WORKSPACE
        .lock()
        .map(|g| g.clone())
        .map_err(|_| {
            anyhow::anyhow!("local-workspace intent mutex poisoned; refuse attach (fail closed)")
        })
}
#[cfg(not(feature = "local-workspace"))]
pub fn active_local_workspace() -> anyhow::Result<Option<()>> {
    Ok(None)
}
#[cfg(feature = "local-workspace")]
fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}
#[cfg(feature = "local-workspace")]
fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
/// Resolve CLI > env local-workspace intent (own or attach).
///
/// Returns `Ok(None)` when local workspace is not requested.
#[cfg(feature = "local-workspace")]
pub fn resolve_local_workspace_config(
    chat: bool,
    cli_own: Option<Option<&std::path::Path>>,
    cli_attach: Option<&str>,
    cli_cwd: Option<&std::path::Path>,
) -> anyhow::Result<Option<LocalWorkspaceConfig>> {
    let env_enable = env_truthy(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ENV);
    let env_mode = env_nonempty(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE_ENV);
    let env_server_id = env_nonempty(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID_ENV);
    let env_cwd =
        env_nonempty(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_CWD_ENV).map(std::path::PathBuf::from);
    let cli_attach = cli_attach.map(str::trim).filter(|s| !s.is_empty());
    let cli_requested = cli_own.is_some() || cli_attach.is_some();
    let env_requested = env_enable || env_mode.is_some() || env_server_id.is_some();
    if !cli_requested && !env_requested {
        return Ok(None);
    }
    if !chat {
        anyhow::bail!("{LOCAL_WORKSPACE_REQUIRES_CHAT}");
    }
    let mode = if cli_attach.is_some() {
        LocalWorkspaceMode::Attach
    } else if cli_own.is_some() {
        LocalWorkspaceMode::Own
    } else if let Some(ref m) = env_mode {
        match m.as_str() {
            "attach" => LocalWorkspaceMode::Attach,
            "own" => LocalWorkspaceMode::Own,
            other => {
                anyhow::bail!(
                    "invalid {ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE_ENV}={other:?}; expected own|attach"
                )
            }
        }
    } else if env_server_id.is_some() {
        LocalWorkspaceMode::Attach
    } else {
        LocalWorkspaceMode::Own
    };
    let cwd = cli_cwd
        .map(std::path::Path::to_path_buf)
        .or_else(|| cli_own.and_then(|inner| inner.map(std::path::Path::to_path_buf)))
        .or(env_cwd)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let cwd = if cwd.is_absolute() {
        cwd
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(cwd)
    };
    let cwd = validate_local_workspace_cwd(&cwd)?;
    match mode {
        LocalWorkspaceMode::Own => Ok(Some(LocalWorkspaceConfig {
            mode,
            cwd: Some(cwd),
            server_id: None,
        })),
        LocalWorkspaceMode::Attach => {
            let server_id = cli_attach
                .map(str::to_owned)
                .or(env_server_id)
                .filter(|s| !s.is_empty());
            let Some(server_id) = server_id else {
                anyhow::bail!("{LOCAL_WORKSPACE_ATTACH_NEEDS_SERVER_ID}");
            };
            ensure_attach_fs_only_toolset(&server_id)?;
            Ok(Some(LocalWorkspaceConfig {
                mode,
                cwd: Some(cwd),
                server_id: Some(server_id),
            }))
        }
    }
}
/// Canonicalize `path` and enforce the `/` + `$HOME` denylist.
///
/// Returns the canonical directory so callers stamp/persist what was actually
/// checked (symlinks / `..` must not diverge from validation).
#[cfg(feature = "local-workspace")]
pub fn validate_local_workspace_cwd(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(path)
    };
    let canon = abs.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "local workspace cwd must exist and be canonicalizable: {}: {e}",
            abs.display()
        )
    })?;
    if !canon.is_dir() {
        anyhow::bail!(
            "local workspace cwd must be an existing directory: {}",
            canon.display()
        );
    }
    if env_truthy(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME_ENV) {
        return Ok(canon);
    }
    if canon == std::path::Path::new("/") {
        anyhow::bail!("{LOCAL_WORKSPACE_HOME_DENIED}");
    }
    if let Some(home_path) = dirs::home_dir().or_else(|| std::env::var_os("HOME").map(Into::into)) {
        let home_canon = home_path.canonicalize().unwrap_or(home_path);
        if canon == home_canon {
            anyhow::bail!("{LOCAL_WORKSPACE_HOME_DENIED}");
        }
    }
    Ok(canon)
}
/// Banner + first-run confirm for local-workspace own/attach.
///
/// Skip confirm only with `ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK=1` or a prior ack file.
/// Non-TTY without ACK refuses (fail closed).
#[cfg(feature = "local-workspace")]
pub fn emit_local_workspace_startup_ux(cfg: &LocalWorkspaceConfig) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    emit_local_workspace_startup_ux_with(cfg, std::io::stdin().is_terminal())
}
/// Testable UX gate: `stdin_is_terminal` is injected.
#[cfg(feature = "local-workspace")]
pub fn emit_local_workspace_startup_ux_with(
    cfg: &LocalWorkspaceConfig,
    stdin_is_terminal: bool,
) -> anyhow::Result<()> {
    let cwd_display = cfg
        .cwd
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<session cwd>".to_string());
    let banner = LOCAL_WORKSPACE_BANNER.replace("<cwd>", &cwd_display);
    eprintln!("{banner}");
    eprintln!("{LOCAL_WORKSPACE_HITL_HINT}");
    if local_workspace_ack_satisfied() {
        return Ok(());
    }
    if !stdin_is_terminal {
        anyhow::bail!("{LOCAL_WORKSPACE_ACK_REQUIRED}");
    }
    eprint!("Continue with local workspace on this machine? [y/N] ");
    use std::io::Write;
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let ok = matches!(line.trim(), "y" | "Y" | "yes" | "YES");
    if !ok {
        anyhow::bail!("local workspace cancelled");
    }
    write_local_workspace_ack();
    Ok(())
}
/// True when ACK env or ack file already authorizes local workspace.
#[cfg(feature = "local-workspace")]
pub fn local_workspace_ack_satisfied() -> bool {
    if env_truthy(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK_ENV) {
        return true;
    }
    local_workspace_ack_path().is_some_and(|p| p.is_file())
}
/// Persist the first-run local-workspace ACK file (best-effort).
#[cfg(feature = "local-workspace")]
pub fn write_local_workspace_ack() {
    if let Some(path) = local_workspace_ack_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, "1\n");
    }
}
/// Fail closed unless advertised tools are FS-only.
///
/// Until diag exposes a real tool catalog, attach trusts operator attestation
/// via `ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS` (comma-separated ids).
/// Unset / empty → refuse.
#[cfg(feature = "local-workspace")]
pub fn ensure_attach_fs_only_toolset(_server_id: &str) -> anyhow::Result<()> {
    let advertised = probe_advertised_tool_ids();
    let refs: Option<Vec<&str>> = advertised
        .as_ref()
        .map(|ids| ids.iter().map(String::as_str).collect());
    crate::app::effects::reject_non_fs_only_advertised_tools(refs.as_deref())
        .map_err(|e| anyhow::anyhow!("{e}"))
}
/// Operator-attested advertised tool ids for attach (env only; no fake diag probe).
#[cfg(feature = "local-workspace")]
pub fn probe_advertised_tool_ids() -> Option<Vec<String>> {
    let raw = env_nonempty(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS_ENV)?;
    let ids: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(ids)
}
#[cfg(feature = "local-workspace")]
fn local_workspace_ack_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("ECHO_AGENT_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            dirs::home_dir()
                .or_else(|| std::env::var_os("HOME").map(Into::into))
                .map(|h| h.join(".echo-agent"))
        })?;
    Some(home.join("local_workspace_ack"))
}
/// Conservative shape check for a chat-mode `--resume <id>` passthrough.
///
/// The id skips disk/GCS resolution and flows to the gateway, but it is also
/// path-joined by the local cwd-collision check — so reject path separators,
/// dots, and anything outside the conversation-id alphabet before it leaves
/// materialization. Existence is still validated by the gateway at load.
pub fn valid_conversation_id_shape(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}
/// True when `session_id` resolves under the **cwd-scoped** local Build sessions
/// tree. Deliberately does **not** use `resolve_local_session_any_cwd`: a gateway
/// conversation id that collides with a Build session under another cwd must not
/// false-refuse CLI resume / non-entry loads under `--chat`.
pub fn local_build_session_on_disk(session_id: &str, cwd: &Path) -> bool {
    let cwd_str = cwd.to_string_lossy();
    echo_agent_runtime::session::resolve_local_session(session_id, &cwd_str).is_some()
}
/// Pure policy: process-wide `--chat` refuses a local Build disk row unless the
/// caller marked an explicit conversation entry (picker `source == "conversation"`).
pub fn chat_mode_refuses_local_build(
    chat_mode: bool,
    conversation_entry: bool,
    is_local_build_on_disk: bool,
) -> bool {
    chat_mode && !conversation_entry && is_local_build_on_disk
}
/// Process-wide `--chat` must not load (or coerce) local Build disk rows.
///
/// `conversation_entry` is true only for picker/list rows with
/// `source == "conversation"` (or restore that preserved that bit) — **not**
/// merely because sticky `--chat` / `chat_mode` is set.
///
/// Short-circuits before any disk walk when `--chat` is off or the row is a
/// conversation entry.
pub fn chat_mode_refuses_local_build_load(
    chat_mode: bool,
    conversation_entry: bool,
    session_id: &str,
    cwd: &Path,
) -> bool {
    if !chat_mode || conversation_entry {
        return false;
    }
    local_build_session_on_disk(session_id, cwd)
}
/// Outcome of async materialization (local resolve / remote restore / preflight).
#[derive(Debug, Clone)]
pub enum MaterializedStartup {
    /// Create a new session with an agent-chosen ID (or defer to welcome).
    NewAuto,
    /// Create a new session with this ID (`session/new` meta.sessionId).
    NewWithId { session_id: String },
    /// Strict load of an existing session.
    Resume {
        session_id: String,
        original_cwd: Option<PathBuf>,
        title: Option<String>,
        /// The target missed local id/title resolution and was deferred to
        /// the worktree resume handler; worktree failure messages append the
        /// no-match hint only for this outcome (never inferred from shape).
        deferred_local_miss: bool,
        /// Pre-TUI conversation-only remote restore: follow-up `LoadSession`
        /// must send `echo.agent/restore_code: false` so agent `[cli] restore_code`
        /// cannot checkout in-place on the new local child.
        suppress_code_restore: bool,
    },
    /// Fork from a resolved parent, then load the child.
    Fork {
        parent_session_id: String,
        parent_cwd: Option<PathBuf>,
        parent_title: Option<String>,
        new_session_id: Option<String>,
        /// Same one-shot as [`Self::Resume::suppress_code_restore`]: the
        /// follow-up child `LoadSession` must not inherit agent restore-code.
        suppress_code_restore: bool,
    },
}
/// Whether materialization may resolve a non-id resume arg by title locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleResolution {
    /// No pre-sandbox pin ran (direct callers, tests): materialization owns
    /// title selection.
    Allowed,
    /// The composition root already pinned — or definitively missed — the
    /// target before the irreversible OS sandbox. Re-selecting by title here
    /// would race a concurrent rename/create and resume a session whose
    /// persisted profile was never checked; a pinned id that vanished must
    /// also never be reinterpreted as a title.
    PinnedPreSandbox,
}
/// Context for [`materialize_startup`] (interactive vs headless share this).
#[derive(Debug, Clone, Copy)]
pub struct MaterializeCtx {
    /// When true, skip process-cwd preflight for `NewWithId` (worktree create
    /// checks the final session cwd later).
    pub has_worktree: bool,
    /// When true, attempt remote restore if the session is not on disk.
    pub allow_remote_restore: bool,
    /// Process-wide flag: resume targets are cloud.echo-agent.invalid conversations, not
    /// the local disk store. Always `false` without the optional feature;
    /// setting it anyway errors rather than silently falling back to disk.
    pub chat_mode: bool,
    /// See [`TitleResolution`]; carried from the pre-sandbox pin outcome.
    pub title_resolution: TitleResolution,
    /// CLI `--restore-code`. Remote codebase restore is never applied in-place;
    /// this flag either defers to `--worktree` or refuses the in-place path.
    pub restore_code: bool,
    /// Pre-TUI restore progress on stdout (interactive tty). Headless keeps
    /// stdout as JSON/NDJSON and uses stderr instead.
    pub restore_progress_on_stdout: bool,
}
impl MaterializeCtx {
    /// `--resume` miss bails fast.
    pub const fn default_allow_remote_restore() -> bool {
        false
    }
    pub fn from_pager_args(args: &PagerArgs) -> Self {
        Self {
            has_worktree: args.worktree.is_some(),
            allow_remote_restore: Self::default_allow_remote_restore(),
            chat_mode: args.chat(),
            title_resolution: if args.resume_target_pinned {
                TitleResolution::PinnedPreSandbox
            } else {
                TitleResolution::Allowed
            },
            restore_code: args.restore_code,
            restore_progress_on_stdout: false,
        }
    }
}
/// Cwd where a forked child session is written (interactive + headless SSOT).
///
/// When the parent lives under another directory, the fork effect sets
/// `newCwd` to that parent session cwd — preflight must use the same path.
pub fn effective_fork_new_cwd(process_cwd: &str, parent_cwd: Option<&Path>) -> String {
    parent_cwd
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| process_cwd.to_string())
}
/// Resolve most-recent session id for cwd, or error.
async fn most_recent_session_id(cwd: &str) -> anyhow::Result<(String, Option<String>)> {
    let summaries = echo_agent_runtime::session::persistence::list_summaries(Some(cwd)).await?;
    let first = summaries.first().ok_or_else(|| {
        anyhow::anyhow!(
            "No session found for current directory. \
             Use 'echoagent' to start a new session."
        )
    })?;
    Ok((first.info.id.to_string(), first.display_title_opt()))
}
/// `AuthManager` for direct cloud.echo-agent.invalid calls made outside the agent (pre-ACP
/// `--continue` conversation listing, the GCS restore effect). Wires the
/// auth-provider refresher before the first `auth()`: without it, environments
/// that mint credentials via `auth_provider_command` report `NoOauth`.
pub(crate) fn pre_acp_auth_manager(
    agent_config: &echo_agent_runtime::agent::config::Config,
) -> std::sync::Arc<echo_agent_runtime::auth::AuthManager> {
    let auth = std::sync::Arc::new(echo_agent_runtime::auth::AuthManager::new(
        &echo_agent_runtime::util::echo_agent_home::echo_agent_home(),
        agent_config.echo_agent_com_config.clone(),
    ));
    auth.configure_refresher(
        agent_config
            .echo_agent_com_config
            .auth_provider_command
            .clone(),
        None,
    );
    auth
}
/// Pre-TUI remote restore (session state + memory only). Codebase checkout is
/// never applied on this path; `--restore-code` requires `--worktree`.
const REMOTE_RESTORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
/// `--restore-code` without `--worktree` on a remote miss: refuse in-place checkout.
const REMOTE_RESTORE_NEEDS_WORKTREE: &str = "--restore-code on a remote session requires --worktree \
     (refusing to check out snapshot code into the current directory)";
/// `--worktree` resume without `--restore-code`: conversation only.
pub(crate) const WORKTREE_NO_RESTORE_CODE_NOTICE: &str =
    "Snapshot code will not be restored into the worktree; pass --restore-code to restore it.";
/// Preflight: preferred id must be a UUID and not a persisted session under `cwd`.
///
/// Agent `session/new` rejects non-UUID `_meta.sessionId`; fail fast here so
/// CLI users get a clear error before ACP.
pub fn ensure_session_id_available(session_id: &str, cwd: &str) -> anyhow::Result<()> {
    if uuid::Uuid::try_parse(session_id).is_err() {
        anyhow::bail!("Error: --session-id must be a valid UUID (got '{session_id}').");
    }
    if echo_agent_runtime::session::persistence::session_exists_for_cwd(session_id, cwd) {
        anyhow::bail!("Error: Session ID {session_id} is already in use.");
    }
    Ok(())
}
/// Materialize CLI intent into a concrete startup plan (I/O + remote restore).
pub async fn materialize_startup(
    ctx: MaterializeCtx,
    intent: SessionStartupIntent,
) -> anyhow::Result<MaterializedStartup> {
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Failed to get cwd: {e}"))?
        .to_string_lossy()
        .to_string();
    materialize_startup_for_cwd(ctx, intent, &cwd).await
}
/// Same as [`materialize_startup`] but with an explicit process cwd (tests / headless).
pub async fn materialize_startup_for_cwd(
    ctx: MaterializeCtx,
    intent: SessionStartupIntent,
    cwd: &str,
) -> anyhow::Result<MaterializedStartup> {
    if ctx.chat_mode && matches!(intent, SessionStartupIntent::ForkFrom { .. }) {
        anyhow::bail!("{CHAT_MODE_FORK_CONFLICT}");
    }
    match intent {
        SessionStartupIntent::NewAuto => Ok(MaterializedStartup::NewAuto),
        SessionStartupIntent::NewWithId { session_id } => {
            if !ctx.has_worktree {
                ensure_session_id_available(&session_id, cwd)?;
            } else if uuid::Uuid::try_parse(&session_id).is_err() {
                anyhow::bail!("Error: --session-id must be a valid UUID (got '{session_id}').");
            }
            Ok(MaterializedStartup::NewWithId { session_id })
        }
        SessionStartupIntent::Resume {
            session_id: None,
            most_recent_for_cwd: true,
        } => {
            if ctx.chat_mode {
                anyhow::bail!("chat-mode resume requires a build with the `chat` cargo feature");
            }
            let started = std::time::Instant::now();
            let (id, title) = most_recent_session_id(cwd).await?;
            tracing::info!(
                source = "local",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "startup.continue.resolve"
            );
            Ok(MaterializedStartup::Resume {
                session_id: id,
                original_cwd: None,
                title,
                deferred_local_miss: false,
                suppress_code_restore: false,
            })
        }
        SessionStartupIntent::ForkFrom {
            source_session_id: None,
            most_recent_for_cwd: true,
            new_session_id,
        } => {
            if let Some(ref nid) = new_session_id {
                ensure_session_id_available(nid, cwd)?;
            }
            let (id, title) = most_recent_session_id(cwd).await?;
            Ok(MaterializedStartup::Fork {
                parent_session_id: id,
                parent_cwd: None,
                parent_title: title,
                new_session_id,
                suppress_code_restore: false,
            })
        }
        SessionStartupIntent::Resume {
            session_id: Some(session_id),
            ..
        } => {
            if ctx.chat_mode {
                if !valid_conversation_id_shape(&session_id) {
                    anyhow::bail!("invalid conversation id {session_id:?}");
                }
                return Ok(MaterializedStartup::Resume {
                    session_id,
                    original_cwd: None,
                    title: None,
                    deferred_local_miss: false,
                    suppress_code_restore: false,
                });
            }
            let r = resolve_existing_session(ctx, &session_id, cwd).await?;
            Ok(MaterializedStartup::Resume {
                session_id: r.id,
                original_cwd: r.original_cwd,
                title: r.title,
                deferred_local_miss: r.deferred_local_miss,
                suppress_code_restore: r.suppress_code_restore,
            })
        }
        SessionStartupIntent::ForkFrom {
            source_session_id: Some(session_id),
            new_session_id,
            ..
        } => {
            let r = resolve_existing_session(ctx, &session_id, cwd).await?;
            if let Some(ref nid) = new_session_id {
                let new_cwd = effective_fork_new_cwd(cwd, r.original_cwd.as_deref());
                ensure_session_id_available(nid, &new_cwd)?;
            }
            Ok(MaterializedStartup::Fork {
                parent_session_id: r.id,
                parent_cwd: r.original_cwd,
                parent_title: r.title,
                new_session_id,
                suppress_code_restore: r.suppress_code_restore,
            })
        }
        SessionStartupIntent::Resume {
            session_id: None,
            most_recent_for_cwd: false,
        }
        | SessionStartupIntent::ForkFrom {
            source_session_id: None,
            most_recent_for_cwd: false,
            ..
        } => {
            anyhow::bail!("internal: invalid session startup intent (unreachable from CLI flags)")
        }
    }
}
struct ResolvedExisting {
    id: String,
    original_cwd: Option<PathBuf>,
    title: Option<String>,
    /// True only for the worktree-defer arm: the target missed local
    /// id/title resolution.
    deferred_local_miss: bool,
    suppress_code_restore: bool,
}
/// Resolve an existing session for strict resume (local / any-cwd / remote / worktree defer).
async fn resolve_existing_session(
    ctx: MaterializeCtx,
    session_id: &str,
    cwd: &str,
) -> anyhow::Result<ResolvedExisting> {
    if let Some(local_id) = echo_agent_runtime::session::resolve_local_session(session_id, cwd) {
        tracing::info!(session_id = %session_id, local_id = %local_id, "Session found locally");
        if !in_place_restore_code_allowed(ctx.restore_code, ctx.has_worktree, session_id, &local_id)
        {
            anyhow::bail!("{REMOTE_RESTORE_NEEDS_WORKTREE}");
        }
        return Ok(ResolvedExisting {
            id: local_id,
            original_cwd: None,
            title: None,
            deferred_local_miss: false,
            suppress_code_restore: false,
        });
    }
    if let Some(original_cwd) =
        echo_agent_runtime::session::resolve_local_session_any_cwd(session_id)
    {
        tracing::info!(
            session_id = %session_id,
            original_cwd = %original_cwd,
            "Session found locally under different CWD"
        );
        eprintln!(
            "Session {} found locally (originally in {})",
            session_id, original_cwd
        );
        return Ok(ResolvedExisting {
            id: session_id.to_string(),
            original_cwd: Some(PathBuf::from(original_cwd)),
            title: None,
            deferred_local_miss: false,
            suppress_code_restore: false,
        });
    }
    let arg_is_uuid = super::session_title_resolve::is_uuid_shaped(session_id);
    if !arg_is_uuid
        && ctx.title_resolution == TitleResolution::Allowed
        && let Some(resolved) = resolve_session_by_title(session_id, cwd).await?
    {
        return Ok(resolved);
    }
    match plan_remote_miss(ctx, arg_is_uuid) {
        RemoteMissPlan::DeferToWorktree {
            deferred_local_miss,
        } => {
            tracing::info!(
                session_id = %session_id,
                restore_code = ctx.restore_code,
                "Session not found locally; deferring restore to worktree resume handler"
            );
            eprintln!(
                "Session {:?} not found locally; it will be restored into the new worktree.",
                session_id
            );
            if !ctx.restore_code {
                eprintln!("{WORKTREE_NO_RESTORE_CODE_NOTICE}");
            }
            Ok(ResolvedExisting {
                id: session_id.to_string(),
                original_cwd: None,
                title: None,
                deferred_local_miss,
                suppress_code_restore: !ctx.restore_code,
            })
        }
        RemoteMissPlan::RejectInPlaceCodeRestore { title_miss_hint } => {
            if title_miss_hint {
                anyhow::bail!(
                    "{REMOTE_RESTORE_NEEDS_WORKTREE}; {}",
                    super::session_title_resolve::title_miss_hint(session_id)
                );
            }
            anyhow::bail!("{REMOTE_RESTORE_NEEDS_WORKTREE}")
        }
        RemoteMissPlan::NotFound { title_miss_hint } => {
            if title_miss_hint {
                anyhow::bail!(
                    "Session does not exist: {}",
                    super::session_title_resolve::title_miss_hint(session_id)
                );
            }
            anyhow::bail!("Session does not exist")
        }
        RemoteMissPlan::RestoreConversation => {
            let restored =
                restore_session_from_remote(session_id, cwd, ctx.restore_progress_on_stdout).await;
            if arg_is_uuid {
                return restored;
            }
            restored.map_err(|e| {
                anyhow::anyhow!(
                    "{e:#}; {}",
                    super::session_title_resolve::title_miss_hint(session_id)
                )
            })
        }
    }
}
/// Policy after local id/title resolution misses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteMissPlan {
    DeferToWorktree {
        deferred_local_miss: bool,
    },
    RejectInPlaceCodeRestore {
        title_miss_hint: bool,
    },
    /// Pre-TUI restore of session state + memory only (never codebase).
    RestoreConversation,
    NotFound {
        title_miss_hint: bool,
    },
}
/// Whether `--restore-code` may run in-place for this local hit.
///
/// `resolved_id != requested_id` means the CLI handle was a remote UUID that
/// only exists as a previously restored child — still a remote session, so
/// snapshot checkout requires `--worktree`.
pub(crate) fn in_place_restore_code_allowed(
    restore_code: bool,
    has_worktree: bool,
    requested_id: &str,
    resolved_id: &str,
) -> bool {
    if !restore_code || has_worktree {
        return true;
    }
    requested_id == resolved_id
}
pub(crate) fn plan_remote_miss(ctx: MaterializeCtx, arg_is_uuid: bool) -> RemoteMissPlan {
    if ctx.has_worktree {
        return RemoteMissPlan::DeferToWorktree {
            deferred_local_miss: !arg_is_uuid,
        };
    }
    if !ctx.allow_remote_restore {
        return RemoteMissPlan::NotFound {
            title_miss_hint: !arg_is_uuid,
        };
    }
    if ctx.restore_code {
        return RemoteMissPlan::RejectInPlaceCodeRestore {
            title_miss_hint: !arg_is_uuid,
        };
    }
    RemoteMissPlan::RestoreConversation
}
/// Remote-restore tail of [`resolve_existing_session`], split out so non-id
/// targets can wrap every failure with the title-miss hint.
///
/// Always restores session state + memory only. Codebase checkout is refused
/// in-place ([`RemoteMissPlan::RejectInPlaceCodeRestore`]) or deferred to the
/// worktree handler.
///
/// On timeout the future is cancelled; partial JSONL may already be on disk
/// and is recovered via a local-child scan of the remote id.
async fn restore_session_from_remote(
    session_id: &str,
    cwd: &str,
    progress_on_stdout: bool,
) -> anyhow::Result<ResolvedExisting> {
    let raw_config = echo_agent_runtime::config::load_effective_config()
        .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;
    if let Some((false, source)) =
        echo_agent_runtime::util::config::session_registry_local_override_sourced(Some(&raw_config))
    {
        anyhow::bail!(
            "Session does not exist locally (session registry is disabled by {})",
            source.label()
        );
    }
    emit_pre_tui_restore_line(
        progress_on_stdout,
        &format!(
            "Session {:?} not found locally, restoring conversation from remote...",
            session_id
        ),
    );
    let agent_config = echo_agent_runtime::agent::config::Config::new_from_toml_cfg(&raw_config)
        .map_err(|e| anyhow::anyhow!("Failed to create agent config: {}", e))?;
    use echo_agent_runtime::agent::session_registry_client::SessionRegistryClient;
    use echo_agent_runtime::auth::{AuthManager, ensure_authenticated_or_noninteractive};
    use echo_agent_runtime::session::restore::{RestoreSessionOpts, restore_session_with_storage};
    use echo_agent_runtime::util::echo_agent_home::echo_agent_home;
    let deployment_key = agent_config.endpoints.deployment_key.clone();
    ensure_authenticated_or_noninteractive(
        &agent_config.echo_agent_com_config,
        deployment_key.is_some(),
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to authenticate for session restore: {}", e))?;
    let auth_manager = std::sync::Arc::new(AuthManager::new(
        &echo_agent_home(),
        agent_config.echo_agent_com_config.clone(),
    ));
    let registry_client =
        SessionRegistryClient::new(agent_config.endpoints.proxy_url(), String::new())
            .with_deployment_key(deployment_key.clone())
            .with_alpha_test_key(agent_config.endpoints.alpha_test_key.clone())
            .with_auth(auth_manager.clone());
    let storage_client =
        echo_agent_runtime::auth::credential_provider::build_storage_client_for_proxy(
            &agent_config.endpoints.proxy_url(),
            deployment_key,
            agent_config.endpoints.alpha_test_key.clone(),
            Some(auth_manager),
            None,
            None,
            "echo-agent-pager",
        );
    let progress: echo_agent_runtime::session::restore::ProgressCallback = Box::new(move |event| {
        emit_pre_tui_restore_line(progress_on_stdout, &format!("  {}", event.display_line()));
    });
    let timed = tokio::time::timeout(
        REMOTE_RESTORE_TIMEOUT,
        restore_session_with_storage(
            &registry_client,
            &storage_client,
            session_id,
            cwd,
            RestoreSessionOpts {
                turn_override: None,
                progress: Some(progress),
                restore_code: false,
            },
        ),
    )
    .await;
    let (timed_out, restore_result) = match timed {
        Ok(inner) => (false, Some(inner)),
        Err(_) => (true, None),
    };
    let recovered_local_id =
        echo_agent_runtime::session::find_local_child_for_remote(session_id, cwd);
    match classify_remote_restore(
        timed_out,
        restore_result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|r| r.local_session_id.as_str()),
        restore_result
            .as_ref()
            .and_then(|r| r.as_ref().err())
            .map(|e| format!("{e:#}"))
            .as_deref(),
        recovered_local_id.as_deref(),
    ) {
        RemoteRestoreOutcome::Restored { local_session_id } => {
            emit_pre_tui_restore_line(
                progress_on_stdout,
                &format!("  Restored conversation as local session {local_session_id}"),
            );
            Ok(ResolvedExisting {
                id: local_session_id,
                original_cwd: None,
                title: None,
                deferred_local_miss: false,
                suppress_code_restore: true,
            })
        }
        RemoteRestoreOutcome::RecoveredAfterFailure { local_session_id } => {
            let msg = if timed_out {
                format!(
                    "Remote restore timed out after {}s; continuing with conversation {local_session_id}.",
                    REMOTE_RESTORE_TIMEOUT.as_secs(),
                )
            } else if let Some(Err(e)) = restore_result {
                format!(
                    "Remote restore failed ({e:#}); continuing with conversation {local_session_id}."
                )
            } else {
                format!(
                    "Remote restore incomplete; continuing with conversation {local_session_id}."
                )
            };
            emit_pre_tui_restore_line(progress_on_stdout, &msg);
            Ok(ResolvedExisting {
                id: local_session_id,
                original_cwd: None,
                title: None,
                deferred_local_miss: false,
                suppress_code_restore: true,
            })
        }
        RemoteRestoreOutcome::Failed(msg) => anyhow::bail!("{msg}"),
    }
}
fn emit_pre_tui_restore_line(on_stdout: bool, line: &str) {
    use std::io::Write;
    if on_stdout {
        println!("{line}");
        let _ = std::io::stdout().flush();
    } else {
        eprintln!("{line}");
        let _ = std::io::stderr().flush();
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteRestoreOutcome {
    Restored { local_session_id: String },
    RecoveredAfterFailure { local_session_id: String },
    Failed(String),
}
pub(crate) fn classify_remote_restore(
    timed_out: bool,
    local_session_id: Option<&str>,
    restore_err: Option<&str>,
    recovered_local_id: Option<&str>,
) -> RemoteRestoreOutcome {
    if let Some(id) = local_session_id.filter(|s| !s.is_empty()) {
        return RemoteRestoreOutcome::Restored {
            local_session_id: id.to_string(),
        };
    }
    if let Some(id) = recovered_local_id.filter(|s| !s.is_empty()) {
        return RemoteRestoreOutcome::RecoveredAfterFailure {
            local_session_id: id.to_string(),
        };
    }
    if timed_out {
        return RemoteRestoreOutcome::Failed(format!(
            "Timed out restoring session from remote after {}s. Conversation cannot be recovered.",
            REMOTE_RESTORE_TIMEOUT.as_secs()
        ));
    }
    if let Some(e) = restore_err {
        return RemoteRestoreOutcome::Failed(format!("Failed to restore session from remote: {e}"));
    }
    RemoteRestoreOutcome::Failed(
        "Failed to restore session from remote: conversation history was unavailable.".to_string(),
    )
}
/// Resolve a non-id resume arg as a session title among local sessions for `cwd`.
///
/// Matching/disambiguation rules live in [`super::session_title_resolve`]
/// (shared with the pre-sandbox saved-profile peek); this adds the cwd-scoped
/// listing and the resolved-id announcement. The arg is matched in memory and
/// never used as a filesystem path.
async fn resolve_session_by_title(
    arg: &str,
    cwd: &str,
) -> anyhow::Result<Option<ResolvedExisting>> {
    let summaries = echo_agent_runtime::session::persistence::list_summaries(Some(cwd)).await?;
    let Some(chosen) = super::session_title_resolve::select_by_title(arg, &summaries)? else {
        return Ok(None);
    };
    let id = chosen.info.id.to_string();
    tracing::info!(session_id = %id, "Session resolved by title");
    eprintln!("Resuming session {} (matched by title)", id);
    Ok(Some(ResolvedExisting {
        id,
        original_cwd: None,
        title: chosen.display_title_opt(),
        deferred_local_miss: false,
        suppress_code_restore: false,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn parse(args: &[&str]) -> PagerArgs {
        PagerArgs::try_parse_from(args).unwrap()
    }
    #[test]
    fn parent_session_is_worktree_detects_standalone_marker() {
        let main = crate::test_util::TempGitRepo::init("main-only");
        let clone = main.standalone_clone("wt-branch");
        assert!(clone.path.join(".git").is_dir());
        assert!(parent_session_is_worktree("any-sid", &clone.path));
        assert!(!parent_session_is_worktree("any-sid", &main.path));
    }
    #[test]
    fn parent_session_is_worktree_detects_linked_git_worktree() {
        let main = crate::test_util::TempGitRepo::init("main-only");
        let wt = main.add_linked_worktree("wt", "wt-branch");
        assert!(wt.join(".git").is_file());
        assert!(parent_session_is_worktree("any-sid", &wt));
        assert!(!parent_session_is_worktree("any-sid", &main.path));
    }
    #[test]
    fn parent_session_is_worktree_relocated_gitdir_file_is_not_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path().join("sub");
        crate::test_util::init_git_repo_on_branch(&repo_path, "main");
        let git_dir = tmp.path().join("modules").join("sub");
        std::fs::create_dir_all(git_dir.parent().unwrap()).unwrap();
        std::fs::rename(repo_path.join(".git"), &git_dir).unwrap();
        std::fs::write(
            repo_path.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        assert!(repo_path.join(".git").is_file());
        assert!(
            git2::Repository::discover(&repo_path).is_ok(),
            "relocated gitdir must still discover"
        );
        assert!(!parent_session_is_worktree("any-sid", &repo_path));
    }
    #[test]
    fn parent_session_is_worktree_plain_clone_nested_in_linked_worktree() {
        let main = crate::test_util::TempGitRepo::init("main-only");
        let wt = main.add_linked_worktree("wt", "wt-branch");
        let nested = wt.join("vendor").join("dep");
        crate::test_util::init_git_repo_on_branch(&nested, "dep-branch");
        assert!(!parent_session_is_worktree("any-sid", &nested));
        assert!(!parent_session_is_worktree("any-sid", &main.path));
    }
    #[test]
    fn parent_session_is_worktree_broken_gitdir_file_is_last_resort_true() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".git"), "not-a-gitdir\n").unwrap();
        assert!(git2::Repository::discover(&dir).is_err());
        assert!(parent_session_is_worktree("any-sid", &dir));
    }
    #[serial_test::serial(ECHO_AGENT_HOME)]
    #[test]
    fn parent_session_is_worktree_summary_session_kind() {
        let mut fx = crate::test_util::EchoAgentHomeFixture::new();
        let repo = crate::test_util::TempGitRepo::init("main");
        let cwd = repo.path.to_string_lossy().to_string();
        fx.write_summary(
            &cwd,
            "sid-kind",
            serde_json::json!({ "session_kind": "worktree" }),
        );
        assert!(parent_session_is_worktree("sid-kind", &repo.path));
    }
    #[serial_test::serial(ECHO_AGENT_HOME)]
    #[test]
    fn parent_session_is_worktree_summary_source_workspace_dir() {
        let mut fx = crate::test_util::EchoAgentHomeFixture::new();
        let repo = crate::test_util::TempGitRepo::init("main");
        let cwd = repo.path.to_string_lossy().to_string();
        fx.write_summary(
            &cwd,
            "sid-src",
            serde_json::json!({ "source_workspace_dir": "/src/main" }),
        );
        assert!(parent_session_is_worktree("sid-src", &repo.path));
        fx.write_summary(
            &cwd,
            "sid-src-empty",
            serde_json::json!({ "source_workspace_dir": "" }),
        );
        assert!(!parent_session_is_worktree("sid-src-empty", &repo.path));
    }
    #[serial_test::serial(ECHO_AGENT_HOME)]
    #[test]
    fn parent_session_is_worktree_summary_worktree_label() {
        let mut fx = crate::test_util::EchoAgentHomeFixture::new();
        let repo = crate::test_util::TempGitRepo::init("main");
        let cwd = repo.path.to_string_lossy().to_string();
        fx.write_summary(
            &cwd,
            "sid-label",
            serde_json::json!({ "worktree_label": "my-wt" }),
        );
        assert!(parent_session_is_worktree("sid-label", &repo.path));
        fx.write_summary(
            &cwd,
            "sid-label-empty",
            serde_json::json!({ "worktree_label": "" }),
        );
        assert!(!parent_session_is_worktree("sid-label-empty", &repo.path));
    }
    #[test]
    fn deferred_startup_owner_take_is_atomic() {
        let mut actions = DeferredStartupActions {
            session: Some(DeferredSessionStartup::ForeignResume {
                tool: echo_agent_foreign_sessions::ForeignSessionTool::Cursor,
                native_id: "cursor-id".into(),
            }),
            prompt: Some("prompt".into()),
            pending_chat: true,
            ..Default::default()
        };
        assert!(!actions.is_empty());
        let snapshot = actions.take();
        assert!(actions.is_empty());
        assert!(snapshot.session.is_some());
        assert_eq!(snapshot.prompt.as_deref(), Some("prompt"));
        assert!(snapshot.pending_chat);
    }
    #[test]
    fn chat_mode_refuses_only_local_build_non_conversation() {
        assert!(chat_mode_refuses_local_build(true, false, true));
        assert!(!chat_mode_refuses_local_build(true, false, false));
        assert!(!chat_mode_refuses_local_build(true, true, true));
        assert!(!chat_mode_refuses_local_build(false, false, true));
    }
    #[test]
    fn intent_default_is_new_auto() {
        assert_eq!(
            parse(&["echoagent"]).session_startup_intent().unwrap(),
            SessionStartupIntent::NewAuto
        );
    }
    #[test]
    fn intent_resume_id() {
        assert_eq!(
            parse(&["echoagent", "--resume", "abc"])
                .session_startup_intent()
                .unwrap(),
            SessionStartupIntent::Resume {
                session_id: Some("abc".into()),
                most_recent_for_cwd: false,
            }
        );
    }
    #[test]
    fn intent_resume_empty_is_most_recent() {
        assert_eq!(
            parse(&["echoagent", "--resume"])
                .session_startup_intent()
                .unwrap(),
            SessionStartupIntent::Resume {
                session_id: None,
                most_recent_for_cwd: true,
            }
        );
    }
    #[test]
    fn intent_continue() {
        assert_eq!(
            parse(&["echoagent", "-c"])
                .session_startup_intent()
                .unwrap(),
            SessionStartupIntent::Resume {
                session_id: None,
                most_recent_for_cwd: true,
            }
        );
    }
    #[test]
    fn intent_session_id_alone_is_new_with_id() {
        assert_eq!(
            parse(&["echoagent", "--session-id", "my-id"])
                .session_startup_intent()
                .unwrap(),
            SessionStartupIntent::NewWithId {
                session_id: "my-id".into(),
            }
        );
    }
    #[test]
    fn intent_session_id_with_resume_without_fork_errors() {
        let err = parse(&["echoagent", "-r", "a", "-s", "b"])
            .session_startup_intent()
            .unwrap_err();
        assert_eq!(err, StartupFlagError::SessionIdRequiresFork);
    }
    #[test]
    fn intent_fork_with_resume() {
        assert_eq!(
            parse(&["echoagent", "-r", "old", "--fork-session"])
                .session_startup_intent()
                .unwrap(),
            SessionStartupIntent::ForkFrom {
                source_session_id: Some("old".into()),
                most_recent_for_cwd: false,
                new_session_id: None,
            }
        );
    }
    #[test]
    fn intent_fork_with_resume_and_new_id() {
        assert_eq!(
            parse(&["echoagent", "-r", "old", "--fork-session", "-s", "new"])
                .session_startup_intent()
                .unwrap(),
            SessionStartupIntent::ForkFrom {
                source_session_id: Some("old".into()),
                most_recent_for_cwd: false,
                new_session_id: Some("new".into()),
            }
        );
    }
    #[test]
    fn intent_fork_alone_errors() {
        let err = parse(&["echoagent", "--fork-session"])
            .session_startup_intent()
            .unwrap_err();
        assert_eq!(err, StartupFlagError::ForkRequiresResumeOrContinue);
    }
    #[test]
    fn intent_fork_with_worktree_errors() {
        let err = parse(&["echoagent", "-r", "a", "--fork-session", "-w"])
            .session_startup_intent()
            .unwrap_err();
        assert_eq!(err, StartupFlagError::ForkWithWorktree);
    }
    #[test]
    fn intent_from_flags_matches_pager_args() {
        let args = parse(&["echoagent", "-r", "old", "--fork-session", "-s", "new"]);
        let from_flags = session_startup_intent_from_flags(SessionStartupFlags {
            session_id: Some("new"),
            resume_session_id: Some("old"),
            resume_most_recent: false,
            continue_last_session: false,
            fork_session: true,
            has_worktree: false,
        })
        .unwrap();
        assert_eq!(from_flags, args.session_startup_intent().unwrap());
    }
    #[test]
    fn ensure_rejects_non_uuid() {
        let err = ensure_session_id_available("my-run-1", "/tmp/does-not-matter").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must be a valid UUID"),
            "unexpected message: {msg}"
        );
    }
    #[test]
    fn effective_fork_new_cwd_prefers_parent() {
        let parent = PathBuf::from("/proj-a");
        assert_eq!(
            effective_fork_new_cwd("/proj-b", Some(parent.as_path())),
            "/proj-a"
        );
        assert_eq!(effective_fork_new_cwd("/proj-b", None), "/proj-b");
    }
    #[test]
    fn fork_session_params_sets_new_session_id_and_workspace_dir() {
        let cwd = PathBuf::from("/wt");
        let p = fork_session_params("parent-1", &cwd, Some("child-uuid"), true);
        assert_eq!(p["sourceSessionId"], "parent-1");
        assert_eq!(p["newCwd"], "/wt");
        assert_eq!(p["newSessionId"], "child-uuid");
        assert_eq!(p["sourceWorkspaceDir"], "/wt");
        assert_eq!(p["sessionKind"], "fork");
    }
    #[test]
    fn fork_session_params_omits_workspace_dir_when_not_worktree() {
        let cwd = PathBuf::from("/proj");
        let p = fork_session_params("parent-1", &cwd, None, false);
        assert!(p.get("sourceWorkspaceDir").is_none());
        assert!(p.get("newSessionId").is_none());
    }
    #[test]
    fn fork_response_parses_nested_and_top_level_id() {
        assert_eq!(
            fork_response_new_session_id(r#"{"newSessionId":"a"}"#).as_deref(),
            Some("a")
        );
        assert_eq!(
            fork_response_new_session_id(r#"{"result":{"newSessionId":"b"}}"#).as_deref(),
            Some("b")
        );
        assert!(fork_response_new_session_id(r#"{"error":"nope"}"#).is_none());
        assert_eq!(
            fork_response_error(r#"{"error":"boom"}"#).as_deref(),
            Some("\"boom\"")
        );
    }
    #[test]
    fn deferred_session_intent_variants_are_distinct() {
        let load = DeferredSessionStartup::Load {
            session_id: "s".into(),
            session_cwd: None,
            chat_kind: false,
        };
        let nid = DeferredSessionStartup::NewWithId {
            session_id: "s".into(),
        };
        assert_ne!(load, nid);
    }
    fn chat_ctx() -> MaterializeCtx {
        MaterializeCtx {
            has_worktree: false,
            allow_remote_restore: true,
            chat_mode: true,
            title_resolution: TitleResolution::Allowed,
            restore_code: false,
            restore_progress_on_stdout: false,
        }
    }
    #[test]
    fn chat_mode_flag_conflict_matrix() {
        assert_eq!(
            chat_mode_flag_conflict(true, true, false),
            Some(CHAT_MODE_FORK_CONFLICT)
        );
        assert_eq!(
            chat_mode_flag_conflict(true, false, true),
            Some(CHAT_MODE_RESTORE_CODE_CONFLICT)
        );
        assert_eq!(
            chat_mode_flag_conflict(true, true, true),
            Some(CHAT_MODE_FORK_CONFLICT)
        );
        assert_eq!(chat_mode_flag_conflict(true, false, false), None);
        assert_eq!(chat_mode_flag_conflict(false, true, true), None);
    }
    #[test]
    fn materialize_ctx_chat_mode_from_args() {
        assert!(!MaterializeCtx::from_pager_args(&parse(&["echoagent"])).chat_mode);
    }
    /// hardcoded `false` here once disabled it everywhere.
    #[test]
    fn remote_restore_follows_compiled_restore_stack() {
        assert_eq!(
            MaterializeCtx::from_pager_args(&parse(&["echoagent"])).allow_remote_restore,
            false
        );
    }
    #[test]
    fn from_pager_args_does_not_probe_tty_for_progress() {
        assert!(
            !MaterializeCtx::from_pager_args(&parse(&["echoagent"])).restore_progress_on_stdout,
            "stdout vs stderr is decided at the composition root, not from_pager_args"
        );
    }
    #[test]
    fn materialize_ctx_restore_code_follows_cli_flag() {
        assert!(!MaterializeCtx::from_pager_args(&parse(&["echoagent"])).restore_code);
        assert!(!MaterializeCtx::from_pager_args(&parse(&["echoagent", "-r", "abc"])).restore_code);
        assert!(
            MaterializeCtx::from_pager_args(&parse(&["echoagent", "-r", "abc", "--restore-code"]))
                .restore_code
        );
        let wt = MaterializeCtx::from_pager_args(&parse(&[
            "echoagent",
            "-r",
            "abc",
            "--restore-code",
            "--worktree",
        ]));
        assert!(wt.restore_code);
        assert!(wt.has_worktree);
    }
    fn remote_miss_ctx(restore_code: bool, has_worktree: bool) -> MaterializeCtx {
        MaterializeCtx {
            has_worktree,
            allow_remote_restore: true,
            chat_mode: false,
            title_resolution: TitleResolution::Allowed,
            restore_code,
            restore_progress_on_stdout: false,
        }
    }
    #[test]
    fn in_place_restore_code_allowed_blocks_restored_remote_child() {
        assert!(in_place_restore_code_allowed(
            true, false, "local-id", "local-id"
        ));
        assert!(!in_place_restore_code_allowed(
            true,
            false,
            "remote-uuid",
            "restored-child"
        ));
        assert!(in_place_restore_code_allowed(
            true,
            true,
            "remote-uuid",
            "restored-child"
        ));
        assert!(in_place_restore_code_allowed(
            false,
            false,
            "remote-uuid",
            "restored-child"
        ));
    }
    #[test]
    fn plan_remote_miss_restore_code_false_restores_conversation_only() {
        assert_eq!(
            plan_remote_miss(remote_miss_ctx(false, false), true),
            RemoteMissPlan::RestoreConversation
        );
    }
    #[test]
    fn plan_remote_miss_restore_code_true_without_worktree_is_rejected() {
        assert_eq!(
            plan_remote_miss(remote_miss_ctx(true, false), true),
            RemoteMissPlan::RejectInPlaceCodeRestore {
                title_miss_hint: false,
            }
        );
        assert_eq!(
            plan_remote_miss(remote_miss_ctx(true, false), false),
            RemoteMissPlan::RejectInPlaceCodeRestore {
                title_miss_hint: true,
            }
        );
    }
    #[test]
    fn plan_remote_miss_worktree_defers_without_restore_code() {
        assert_eq!(
            plan_remote_miss(remote_miss_ctx(false, true), true),
            RemoteMissPlan::DeferToWorktree {
                deferred_local_miss: false,
            }
        );
        assert_eq!(
            plan_remote_miss(remote_miss_ctx(false, true), false),
            RemoteMissPlan::DeferToWorktree {
                deferred_local_miss: true,
            }
        );
    }
    #[test]
    fn plan_remote_miss_restore_code_true_with_worktree_defers() {
        assert_eq!(
            plan_remote_miss(remote_miss_ctx(true, true), true),
            RemoteMissPlan::DeferToWorktree {
                deferred_local_miss: false,
            }
        );
        assert_eq!(
            plan_remote_miss(remote_miss_ctx(true, true), false),
            RemoteMissPlan::DeferToWorktree {
                deferred_local_miss: true,
            }
        );
    }
    #[test]
    fn classify_remote_restore_prefers_returned_local_id() {
        assert_eq!(
            classify_remote_restore(false, Some("child"), Some("boom"), Some("other")),
            RemoteRestoreOutcome::Restored {
                local_session_id: "child".into(),
            }
        );
    }
    #[test]
    fn classify_remote_restore_recovers_disk_child_on_timeout_or_error() {
        assert_eq!(
            classify_remote_restore(true, None, None, Some("child")),
            RemoteRestoreOutcome::RecoveredAfterFailure {
                local_session_id: "child".into(),
            }
        );
        assert_eq!(
            classify_remote_restore(false, Some(""), Some("network"), Some("child")),
            RemoteRestoreOutcome::RecoveredAfterFailure {
                local_session_id: "child".into(),
            }
        );
    }
    #[test]
    fn classify_remote_restore_errors_when_conversation_missing() {
        match classify_remote_restore(true, None, None, None) {
            RemoteRestoreOutcome::Failed(msg) => {
                assert!(msg.contains("Timed out"), "{msg}");
                assert!(msg.contains("cannot be recovered"), "{msg}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        match classify_remote_restore(false, None, Some("registry 404"), None) {
            RemoteRestoreOutcome::Failed(msg) => {
                assert!(msg.contains("Failed to restore"), "{msg}");
                assert!(msg.contains("registry 404"), "{msg}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        match classify_remote_restore(false, Some(""), None, None) {
            RemoteRestoreOutcome::Failed(msg) => {
                assert!(
                    msg.contains("conversation history was unavailable"),
                    "{msg}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
    #[test]
    fn worktree_no_restore_code_notice_mentions_flag() {
        assert!(WORKTREE_NO_RESTORE_CODE_NOTICE.contains("--restore-code"));
    }
    /// `--restore-code` without `--worktree` must fail before any in-place checkout.
    #[tokio::test]
    async fn remote_miss_restore_code_without_worktree_errors() {
        let err = materialize_startup_for_cwd(
            remote_miss_ctx(true, false),
            SessionStartupIntent::Resume {
                session_id: Some("99999999-9999-4999-8999-999999999999".into()),
                most_recent_for_cwd: false,
            },
            "/nonexistent/cwd/for/remote-miss-code-no-wt",
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("--worktree"),
            "unexpected error: {err}"
        );
        let title_err = materialize_startup_for_cwd(
            remote_miss_ctx(true, false),
            SessionStartupIntent::Resume {
                session_id: Some("no such title".into()),
                most_recent_for_cwd: false,
            },
            "/nonexistent/cwd/for/remote-miss-code-no-wt-title",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(title_err.contains("--worktree"), "{title_err}");
        assert!(
            title_err.contains("no session id or title matched"),
            "{title_err}"
        );
    }
    /// `--restore-code --worktree` stays on the existing defer path.
    #[tokio::test]
    async fn remote_miss_restore_code_with_worktree_defers() {
        let id = "no such remote target";
        let out = materialize_startup_for_cwd(
            remote_miss_ctx(true, true),
            SessionStartupIntent::Resume {
                session_id: Some(id.into()),
                most_recent_for_cwd: false,
            },
            "/nonexistent/cwd/for/remote-miss-code-wt",
        )
        .await
        .unwrap();
        match out {
            MaterializedStartup::Resume {
                session_id,
                deferred_local_miss,
                suppress_code_restore,
                ..
            } => {
                assert_eq!(session_id, id);
                assert!(deferred_local_miss, "non-uuid defer must flag a title miss");
                assert!(
                    !suppress_code_restore,
                    "--restore-code --worktree must still restore snapshot code"
                );
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }
    #[tokio::test]
    async fn remote_miss_worktree_without_restore_code_suppresses_snapshot() {
        let id = "no such remote target";
        let out = materialize_startup_for_cwd(
            remote_miss_ctx(false, true),
            SessionStartupIntent::Resume {
                session_id: Some(id.into()),
                most_recent_for_cwd: false,
            },
            "/nonexistent/cwd/for/remote-miss-wt-no-code",
        )
        .await
        .unwrap();
        match out {
            MaterializedStartup::Resume {
                session_id,
                suppress_code_restore,
                ..
            } => {
                assert_eq!(session_id, id);
                assert!(suppress_code_restore);
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }
    /// Explicit-id resume under `--chat` passes the id through untouched:
    /// no disk resolution, no GCS restore (the cwd does not even exist).
    #[tokio::test]
    async fn materialize_chat_resume_id_is_conversation_direct() {
        let out = materialize_startup_for_cwd(
            chat_ctx(),
            SessionStartupIntent::Resume {
                session_id: Some("conv-e2f1".into()),
                most_recent_for_cwd: false,
            },
            "/nonexistent/cwd/for/chat-resume-test",
        )
        .await
        .unwrap();
        match out {
            MaterializedStartup::Resume {
                session_id,
                original_cwd,
                title,
                ..
            } => {
                assert_eq!(session_id, "conv-e2f1");
                assert!(original_cwd.is_none());
                assert!(title.is_none());
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }
    /// Chat-mode passthrough rejects ids that could escape the sessions tree
    /// via the collision check's path join (or are junk for the gateway).
    #[tokio::test]
    async fn materialize_chat_resume_id_rejects_unsafe_shapes() {
        for bad in ["../../../etc/passwd", "a/b", "conv id", "conv\u{7}", ""] {
            let err = materialize_startup_for_cwd(
                chat_ctx(),
                SessionStartupIntent::Resume {
                    session_id: Some(bad.into()),
                    most_recent_for_cwd: false,
                },
                "/tmp",
            )
            .await
            .unwrap_err();
            assert!(
                err.to_string().contains("invalid conversation id"),
                "expected shape rejection for {bad:?}, got: {err}"
            );
        }
        assert!(valid_conversation_id_shape(
            "aaaaaaaa-1111-2222-3333-444444444444"
        ));
        assert!(valid_conversation_id_shape("conv_abc123"));
    }
    /// A no-feature build asked for chat most-recent must fail loudly, not
    /// silently resolve a local Build session.
    #[tokio::test]
    async fn materialize_chat_most_recent_without_feature_bails() {
        let err = materialize_startup_for_cwd(
            chat_ctx(),
            SessionStartupIntent::Resume {
                session_id: None,
                most_recent_for_cwd: true,
            },
            "/nonexistent/cwd/for/no-feature-chat-test",
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("chat"), "unexpected error: {err}");
    }
    /// Without `--chat` an unknown id still goes through disk/GCS resolution
    /// (pinned via `allow_remote_restore: false` → strict "does not exist").
    #[tokio::test]
    async fn materialize_resume_id_without_chat_still_resolves_on_disk() {
        let ctx = MaterializeCtx {
            has_worktree: false,
            allow_remote_restore: false,
            chat_mode: false,
            title_resolution: TitleResolution::Allowed,
            restore_code: false,
            restore_progress_on_stdout: false,
        };
        let err = materialize_startup_for_cwd(
            ctx,
            SessionStartupIntent::Resume {
                session_id: Some("00000000-dead-beef-0000-000000000000".into()),
                most_recent_for_cwd: false,
            },
            "/nonexistent/cwd/for/build-resume-test",
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {err}"
        );
    }
    #[tokio::test]
    async fn materialize_fork_refused_under_chat_mode() {
        for intent in [
            SessionStartupIntent::ForkFrom {
                source_session_id: Some("conv-1".into()),
                most_recent_for_cwd: false,
                new_session_id: None,
            },
            SessionStartupIntent::ForkFrom {
                source_session_id: None,
                most_recent_for_cwd: true,
                new_session_id: None,
            },
        ] {
            let err = materialize_startup_for_cwd(chat_ctx(), intent, "/tmp")
                .await
                .unwrap_err();
            assert_eq!(err.to_string(), CHAT_MODE_FORK_CONFLICT);
        }
    }
    /// The chat passthrough does not bypass the cwd-collision refusal that
    /// `app/mod.rs` runs on the materialized id.
    #[serial_test::serial(ECHO_AGENT_HOME)]
    #[tokio::test]
    async fn chat_resume_passthrough_keeps_cwd_collision_refusal() {
        let home = tempfile::tempdir().expect("home tempdir");
        unsafe { std::env::set_var("ECHO_AGENT_HOME", home.path()) };
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let cwd_str = cwd.path().to_string_lossy().to_string();
        let id = "aaaaaaaa-1111-2222-3333-444444444444";
        let encoded = echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd_str);
        let sessions_cwd_dir = echo_agent_runtime::util::echo_agent_home::echo_agent_home()
            .join("sessions")
            .join(&encoded);
        struct RmDirOnDrop(std::path::PathBuf);
        impl Drop for RmDirOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = RmDirOnDrop(sessions_cwd_dir.clone());
        let session_dir = sessions_cwd_dir.join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
        let out = materialize_startup_for_cwd(
            chat_ctx(),
            SessionStartupIntent::Resume {
                session_id: Some(id.into()),
                most_recent_for_cwd: false,
            },
            &cwd_str,
        )
        .await
        .unwrap();
        match &out {
            MaterializedStartup::Resume { session_id, .. } => {
                assert_eq!(session_id, id);
                assert!(
                    chat_mode_refuses_local_build_load(true, false, session_id, cwd.path()),
                    "cwd-local Build collision must still be refused after passthrough"
                );
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }
    mod resume_by_title {
        use super::*;
        use crate::test_util::EchoAgentHomeFixture;
        fn local_ctx() -> MaterializeCtx {
            MaterializeCtx {
                has_worktree: false,
                allow_remote_restore: false,
                chat_mode: false,
                title_resolution: TitleResolution::Allowed,
                restore_code: false,
                restore_progress_on_stdout: false,
            }
        }
        async fn resume(arg: &str, cwd: &str) -> anyhow::Result<MaterializedStartup> {
            materialize_startup_for_cwd(
                local_ctx(),
                SessionStartupIntent::Resume {
                    session_id: Some(arg.into()),
                    most_recent_for_cwd: false,
                },
                cwd,
            )
            .await
        }
        /// Also covers letter-case insensitivity: the query case differs from
        /// the stored title.
        #[serial_test::serial(ECHO_AGENT_HOME)]
        #[tokio::test]
        async fn title_fallback_resumes_single_match_case_insensitively() {
            let mut fx = EchoAgentHomeFixture::new();
            let cwd_str = fx.cwd_str();
            let id = "bbbbbbbb-1111-2222-3333-444444444444";
            fx.write_summary(
                &cwd_str,
                id,
                serde_json::json!({ "generated_title": "Fix Login Bug", "title_is_manual": true }),
            );
            fx.write_summary(
                &cwd_str,
                "bbbbbbbb-1111-2222-3333-555555555555",
                serde_json::json!({ "generated_title": "Other Work" }),
            );
            match resume("fix login bug", &cwd_str).await.unwrap() {
                MaterializedStartup::Resume {
                    session_id,
                    original_cwd,
                    title,
                    ..
                } => {
                    assert_eq!(session_id, id);
                    assert!(original_cwd.is_none());
                    assert_eq!(title.as_deref(), Some("Fix Login Bug"));
                }
                other => panic!("expected Resume, got {other:?}"),
            }
        }
        /// Id resolution stays authoritative: when the arg is an on-disk
        /// session id, the title fallback is never consulted even though
        /// another session carries that exact title.
        #[serial_test::serial(ECHO_AGENT_HOME)]
        #[tokio::test]
        async fn id_hit_beats_title_fallback() {
            let mut fx = EchoAgentHomeFixture::new();
            let cwd_str = fx.cwd_str();
            fx.write_summary(
                &cwd_str,
                "release-notes",
                serde_json::json!({ "generated_title": "id-owner" }),
            );
            fx.write_summary(
                &cwd_str,
                "cccccccc-1111-2222-3333-444444444444",
                serde_json::json!({ "generated_title": "release-notes", "title_is_manual": true }),
            );
            match resume("release-notes", &cwd_str).await.unwrap() {
                MaterializedStartup::Resume {
                    session_id, title, ..
                } => {
                    assert_eq!(session_id, "release-notes");
                    assert!(title.is_none());
                }
                other => panic!("expected Resume, got {other:?}"),
            }
        }
        /// Provenance for the worktree failure hint: only the defer arm (a
        /// local id/title miss under `--worktree`) flags the target; a
        /// resolved local id — even a legacy non-UUID one — never does.
        #[serial_test::serial(ECHO_AGENT_HOME)]
        #[tokio::test]
        async fn worktree_defer_flags_local_miss_and_local_hit_does_not() {
            let mut fx = EchoAgentHomeFixture::new();
            let cwd_str = fx.cwd_str();
            fx.write_summary(&cwd_str, "release-notes", serde_json::json!({}));
            let worktree_ctx = MaterializeCtx {
                has_worktree: true,
                ..local_ctx()
            };
            let resume_intent = |arg: &str| SessionStartupIntent::Resume {
                session_id: Some(arg.into()),
                most_recent_for_cwd: false,
            };
            let hit =
                materialize_startup_for_cwd(worktree_ctx, resume_intent("release-notes"), &cwd_str)
                    .await
                    .unwrap();
            match hit {
                MaterializedStartup::Resume {
                    session_id,
                    deferred_local_miss,
                    ..
                } => {
                    assert_eq!(session_id, "release-notes");
                    assert!(!deferred_local_miss, "resolved id must not flag a miss");
                }
                other => panic!("expected Resume, got {other:?}"),
            }
            let miss = materialize_startup_for_cwd(
                worktree_ctx,
                resume_intent("no such target"),
                &cwd_str,
            )
            .await
            .unwrap();
            match miss {
                MaterializedStartup::Resume {
                    session_id,
                    deferred_local_miss,
                    ..
                } => {
                    assert_eq!(session_id, "no such target");
                    assert!(deferred_local_miss, "defer must flag the local miss");
                }
                other => panic!("expected Resume, got {other:?}"),
            }
            let uuid_miss = materialize_startup_for_cwd(
                worktree_ctx,
                resume_intent("99999999-9999-4999-8999-999999999999"),
                &cwd_str,
            )
            .await
            .unwrap();
            match uuid_miss {
                MaterializedStartup::Resume {
                    deferred_local_miss,
                    ..
                } => {
                    assert!(
                        !deferred_local_miss,
                        "UUID defer must not flag a title-capable miss"
                    );
                }
                other => panic!("expected Resume, got {other:?}"),
            }
        }
    }
    #[cfg(feature = "local-workspace")]
    fn advertised_tools_env() -> echo_agent_test_support::EnvGuard {
        echo_agent_test_support::EnvGuard::set(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS_ENV,
            "workspace.fs_list,workspace.fs_read_file,workspace.fs_write_file,workspace.fs_exists,workspace.fs_delete_file,workspace.put_files,workspace.get_files",
        )
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[test]
    fn resolve_local_workspace_attach_from_cli() {
        let _env = advertised_tools_env();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = resolve_local_workspace_config(true, None, Some("srv-dogfood"), Some(tmp.path()))
            .unwrap()
            .expect("attach config");
        assert_eq!(cfg.mode, LocalWorkspaceMode::Attach);
        assert_eq!(cfg.server_id.as_deref(), Some("srv-dogfood"));
        let canon = tmp.path().canonicalize().unwrap();
        assert_eq!(cfg.cwd.as_deref(), Some(canon.as_path()));
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID)]
    #[test]
    fn resolve_local_workspace_empty_cli_attach_falls_back_to_env() {
        let _env = advertised_tools_env();
        let _sid = echo_agent_test_support::EnvGuard::set(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID_ENV,
            "srv-from-env",
        );
        let tmp = tempfile::tempdir().unwrap();
        let cfg = resolve_local_workspace_config(true, None, Some(""), Some(tmp.path()))
            .unwrap()
            .expect("empty CLI attach should fall back to env server id");
        assert_eq!(cfg.mode, LocalWorkspaceMode::Attach);
        assert_eq!(cfg.server_id.as_deref(), Some("srv-from-env"));
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_CWD)]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE)]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE)]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID)]
    #[test]
    fn resolve_local_workspace_cwd_only_is_not_a_request() {
        let tmp = tempfile::tempdir().unwrap();
        let _cwd = echo_agent_test_support::EnvGuard::set(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_CWD_ENV,
            tmp.path().to_str().unwrap(),
        );
        let _enable = echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ENV);
        let _mode =
            echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE_ENV);
        let _sid =
            echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID_ENV);
        let cfg = resolve_local_workspace_config(true, None, None, Some(tmp.path())).unwrap();
        assert!(
            cfg.is_none(),
            "cwd-only CLI/env must not activate local workspace: {cfg:?}"
        );
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[test]
    fn resolve_local_workspace_own_from_cli() {
        let _env = advertised_tools_env();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = resolve_local_workspace_config(true, Some(Some(tmp.path())), None, None)
            .unwrap()
            .expect("own config");
        assert_eq!(cfg.mode, LocalWorkspaceMode::Own);
        assert!(
            cfg.server_id.is_none(),
            "own leaves server_id to supervisor"
        );
        let canon = tmp.path().canonicalize().unwrap();
        assert_eq!(cfg.cwd.as_deref(), Some(canon.as_path()));
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[test]
    fn resolve_local_workspace_own_env_defaults() {
        let _env = advertised_tools_env();
        let _enable =
            echo_agent_test_support::EnvGuard::set(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ENV, "1");
        let _mode =
            echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_MODE_ENV);
        let _sid =
            echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_SERVER_ID_ENV);
        let cwd = tempfile::tempdir().unwrap();
        let _cwd = echo_agent_test_support::EnvGuard::set(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_CWD_ENV,
            cwd.path().to_str().unwrap(),
        );
        let cfg = resolve_local_workspace_config(true, None, None, None)
            .unwrap()
            .expect("env own");
        assert_eq!(cfg.mode, LocalWorkspaceMode::Own);
        assert!(cfg.server_id.is_none());
        let canon = cwd.path().canonicalize().unwrap();
        assert_eq!(cfg.cwd.as_deref(), Some(canon.as_path()));
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[test]
    fn resolve_local_workspace_requires_chat() {
        let _env = advertised_tools_env();
        let err = resolve_local_workspace_config(false, None, Some("srv"), None).unwrap_err();
        assert!(
            err.to_string().contains("require --chat"),
            "unexpected: {err}"
        );
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME)]
    #[serial_test::serial(HOME)]
    #[serial_test::serial(USERPROFILE)]
    #[test]
    fn resolve_local_workspace_defaults_cwd_and_denies_home() {
        let _tools = echo_agent_test_support::EnvGuard::set(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS_ENV,
            "workspace.fs_list",
        );
        let _allow = echo_agent_test_support::EnvGuard::unset(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME_ENV,
        );
        let home = tempfile::tempdir().unwrap();
        let home_str = home.path().to_str().unwrap();
        let _home = echo_agent_test_support::EnvGuard::set("HOME", home_str);
        let _userprofile = echo_agent_test_support::EnvGuard::set("USERPROFILE", home_str);
        let err =
            resolve_local_workspace_config(true, None, Some("srv"), Some(home.path())).unwrap_err();
        assert!(err.to_string().contains("ALLOW_HOME"), "unexpected: {err}");
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[test]
    fn resolve_local_workspace_refuses_uncheckable_toolset() {
        let _tools = echo_agent_test_support::EnvGuard::unset(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS_ENV,
        );
        let tmp = tempfile::tempdir().unwrap();
        let err =
            resolve_local_workspace_config(true, None, Some("srv"), Some(tmp.path())).unwrap_err();
        assert!(
            err.to_string().contains("uncheckable") || err.to_string().contains("FS-only"),
            "unexpected: {err}"
        );
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS)]
    #[test]
    fn resolve_local_workspace_refuses_non_fs_toolset() {
        let _tools = echo_agent_test_support::EnvGuard::set(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ADVERTISED_TOOLS_ENV,
            "workspace.fs_list,workspace.bash",
        );
        let tmp = tempfile::tempdir().unwrap();
        let err =
            resolve_local_workspace_config(true, None, Some("srv"), Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("FS-only"), "unexpected: {err}");
        assert!(
            err.to_string().contains("workspace.bash"),
            "unexpected: {err}"
        );
    }
    #[cfg(feature = "local-workspace")]
    #[test]
    fn local_workspace_banner_mentions_local_machine() {
        assert!(LOCAL_WORKSPACE_BANNER.contains("on this machine"));
        assert!(LOCAL_WORKSPACE_HITL_HINT.contains("your machine"));
        assert!(LOCAL_WORKSPACE_HITL_HINT.contains("replaces the chat sandbox"));
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK)]
    #[serial_test::serial(ECHO_AGENT_HOME)]
    #[test]
    fn local_workspace_non_tty_requires_ack() {
        let _ack =
            echo_agent_test_support::EnvGuard::unset(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ACK_ENV);
        let home = tempfile::tempdir().unwrap();
        let _home = echo_agent_test_support::EnvGuard::set(
            "ECHO_AGENT_HOME",
            home.path().to_str().unwrap(),
        );
        let cfg = LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Attach,
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            server_id: Some("srv".into()),
        };
        let err = emit_local_workspace_startup_ux_with(&cfg, false).unwrap_err();
        assert!(
            err.to_string().contains("ACK") || err.to_string().contains("ack"),
            "unexpected: {err}"
        );
    }
    #[cfg(feature = "local-workspace")]
    #[serial_test::serial(ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME)]
    #[test]
    fn validate_local_workspace_cwd_denies_root() {
        let _allow = echo_agent_test_support::EnvGuard::unset(
            ECHO_AGENT_CHAT_LOCAL_WORKSPACE_ALLOW_HOME_ENV,
        );
        let err = validate_local_workspace_cwd(std::path::Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("ALLOW_HOME"), "{err}");
    }
}
