use std::path::Path;

use echo_agent_workspace::session::git::VcsKind;

// Re-export from echo-agent-chat-state — canonical definition lives there.
pub(crate) use echo_agent_chat_state::compaction_utils::extract_user_query;

/// Wraps the user query properly
pub(crate) fn user_query(user_message: String) -> String {
    format!(
        r#"<user_query>
{user_message}
</user_query>"#
    )
}

/// Environment info for constructing the `<user_info>` block.
///
/// When `None`, values are read from the local machine. When `Some`,
/// the provided values are used (e.g. from a remote workspace via
/// `workspace.info` RPC).
pub(crate) struct UserInfoOverride {
    pub os: String,
    pub shell: String,
    pub cwd: String,
}

/// Minimal user message prefix for fast-start / headless contexts.
///
/// Intentionally excludes workspace snapshot and git status.
/// When `override_info` is provided, uses remote workspace info instead
/// of local machine introspection.
pub(crate) fn construct_user_message_minimal(
    working_directory: &Path,
    override_info: Option<&UserInfoOverride>,
) -> String {
    let local_shell;
    let (os, shell, cwd) = match override_info {
        Some(info) => (info.os.as_str(), info.shell.as_str(), info.cwd.clone()),
        None => {
            local_shell = resolve_shell_display();
            (
                std::env::consts::OS,
                local_shell.as_str(),
                working_directory.to_string_lossy().to_string(),
            )
        }
    };
    let today = chrono::Local::now().format("%Y-%m-%d");
    format!(
        r#"<user_info>
OS Version: {os}
Shell: {shell}
Workspace Path: {cwd}
{USER_INFO_DATE_MARKER} {today}
Note: Prefer using relative paths over absolute paths as tool call args when possible.
</user_info>"#,
    )
}

/// Date label in the `<user_info>` prefix; `spawn::resumed_prefix_carries_fallback_date` scans for it.
pub(crate) const USER_INFO_DATE_MARKER: &str = "Today's date:";

/// Resolve a display string for the user's shell.
///
/// Unix: full path from `$SHELL` (e.g. `/bin/zsh`).
/// Windows: detected via `detect_windows_shell` cascade
/// (pwsh > powershell.exe > Git Bash > cmd.exe).
fn resolve_shell_display() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }

    #[cfg(not(unix))]
    {
        echo_agent_config::shell::detect_windows_shell()
            .name()
            .to_string()
    }
}

pub(crate) fn format_vcs_status_block(status: &str, vcs_kind: VcsKind) -> String {
    let (tag, description) = if vcs_kind.is_jj() {
        (
            "jj_status",
            "This is the Jujutsu (jj) status at the start of the conversation. This is a \
             jj-managed repository \u{2014} use `jj` commands instead of `git`. There is no staging \
             area; all changes are part of the working-copy commit (@). Use `jj describe` to \
             set commit messages and `jj new` to finalize changes.",
        )
    } else {
        (
            "git_status",
            "This is the git status at the start of the conversation. Note that this status \
             is a snapshot in time, and will not update during the conversation.",
        )
    };
    format!("\n\n<{tag}>\n{description}\n{status}\n</{tag}>\n")
}

/// Compute the VCS status block (without the `<user_info>` wrapper).
pub(crate) async fn compute_vcs_status_block(
    working_directory: &Path,
    vcs_kind: VcsKind,
) -> Option<String> {
    use echo_agent_workspace::file_system::{git_status_short, jj_status};

    if matches!(vcs_kind, VcsKind::None) {
        return None;
    }
    let mut timer = crate::instrumentation_timer!("session.user_prefix.vcs_status");
    timer.with_field("vcs", if vcs_kind.is_jj() { "jj" } else { "git" });
    timer.with_field(
        "status_mode",
        if vcs_kind.is_jj() {
            "jj"
        } else {
            "short_untracked_normal"
        },
    );
    timer.with_field("timeout_ms", 5_000_u64);
    let timeout = std::time::Duration::from_secs(5);
    let result = if vcs_kind.is_jj() {
        tokio::time::timeout(timeout, jj_status(working_directory)).await
    } else {
        tokio::time::timeout(timeout, git_status_short(working_directory)).await
    };
    match result {
        Ok(Ok(status)) => {
            timer.with_field("outcome", "success");
            timer.with_field("output_bytes", status.len() as u64);
            let status = if vcs_kind.is_jj() {
                Some(status)
            } else {
                echo_agent_core::prompt::user_message::normalize_git_status(&status)
            };
            status.map(|status| format_vcs_status_block(&status, vcs_kind))
        }
        Ok(Err(e)) => {
            timer.with_field("outcome", "error");
            timer.with_field("output_bytes", 0_u64);
            tracing::warn!("user prefix VCS status failed: {e}");
            None
        }
        Err(_) => {
            timer.with_field("outcome", "timeout");
            timer.with_field("output_bytes", 0_u64);
            tracing::warn!(vcs = ?vcs_kind, "user prefix VCS status timed out after 5s");
            None
        }
    }
}

/// Full user message prefix: `<user_info>` + VCS status.
///
/// When `override_info` is provided, uses remote workspace info and
/// `vcs_status_override` (pre-fetched from the remote workspace) instead
/// of local introspection.
pub(crate) async fn construct_user_message(
    working_directory: &Path,
    vcs_kind: VcsKind,
    override_info: Option<&UserInfoOverride>,
    vcs_status_override: Option<String>,
) -> String {
    let cwd = working_directory.to_string_lossy().to_string();
    let vcs_block = if let Some(status) = vcs_status_override {
        Some(format_vcs_status_block(&status, vcs_kind))
    } else {
        let (block, elapsed_ms) = crate::timed!({
            let mut timer = crate::instrumentation_timer!("session.user_prefix");
            timer.with_field("cwd", cwd.as_str());
            compute_vcs_status_block(working_directory, vcs_kind).await
        });
        tracing::debug!(elapsed_ms = elapsed_ms as u64, "startup: user_prefix");
        block
    };
    let mut user_info = construct_user_message_minimal(working_directory, override_info);
    if let Some(vcs) = vcs_block {
        user_info.push_str(&vcs);
    }
    user_info
}

// Tests for extract_user_query now live in echo_agent_chat_state::compaction_utils.

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_workspace::file_system::FsError;

    /// timeout even when pointed at a non-existent directory (git commands
    /// fail instantly → no timeout path exercised, but validates the happy
    /// path doesn't regress).
    #[tokio::test]
    async fn construct_user_message_returns_without_git_status_on_bad_dir() {
        let dir = std::path::Path::new("/tmp/nonexistent-echo-agent-test-dir");
        let msg = construct_user_message(dir, VcsKind::Git, None, None).await;
        // user_info block is always present
        assert!(
            msg.contains("<user_info>"),
            "must contain user_info section"
        );
        // git_status block should be absent (git commands fail on non-repo)
        assert!(
            !msg.contains("<git_status>"),
            "must not contain git_status for non-repo directory"
        );
    }

    #[test]
    fn git_status_error_omits_block() {
        // Simulate what construct_user_message does when git_status returns Err
        let git_status_res: Result<String, FsError> = Err(FsError::Other("timed out".into()));
        let mut user_info = "<user_info>test</user_info>".to_string();
        if let Ok(git_status) = git_status_res {
            user_info = format!("{user_info}\n<git_status>{git_status}</git_status>");
        }
        assert!(!user_info.contains("<git_status>"));
    }
}
