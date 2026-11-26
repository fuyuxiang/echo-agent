//! Static (replay-only) login-shell capture for the non-persistent bash path.
//!
//! Sources the user's rc once at init and captures function and alias
//! definitions; every command replays that fixed snapshot in a fresh shell.
//! Nothing is ever written back: no state dump, no tracked cwd, no
//! persistence across calls. Env vars are deliberately not captured here —
//! the host-side login env capture applies them with fill-gaps precedence.
//!
//! Self-contained by design: independent of the cursor persistent shell's
//! `shell_state` machinery so changes to either path cannot affect the other.

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use command_fds::FdMapping;
use nix::libc;
use tokio::io::AsyncReadExt;

pub use echo_agent_config::shell::UnixShellKind;

const INIT_MARKER: &str = "__ECHO_AGENT_STATIC_SHELL_MARKER__";
const INIT_TIMEOUT: Duration = Duration::from_secs(15);

/// A fixed snapshot of rc-defined functions and aliases, captured once.
#[derive(Debug, Clone)]
pub struct StaticShellSnapshot {
    pub snapshot: String,
    pub shell: UnixShellKind,
}

fn shell_binary(shell: UnixShellKind) -> &'static str {
    echo_agent_config::shell::unix_shell_path(shell)
}

fn rc_file_name(shell: UnixShellKind) -> &'static str {
    match shell {
        UnixShellKind::Bash => ".bashrc",
        UnixShellKind::Zsh => ".zshrc",
    }
}

fn sudo_alias_injection() -> String {
    match std::env::var("SUDO_ASKPASS") {
        Ok(val) if !val.is_empty() => "alias sudo='sudo -A'; ".to_string(),
        _ => String::new(),
    }
}

impl StaticShellSnapshot {
    /// Source the rc once in a login shell and capture alias and function
    /// definitions between SOH markers. Returns an empty snapshot on any
    /// failure or timeout, degrading to a plain shell.
    pub async fn init(cwd: &Path) -> Self {
        let shell = echo_agent_config::shell::detect_unix_shell_kind();

        let capture = match shell {
            UnixShellKind::Bash => "builtin alias -p 2>/dev/null; builtin declare -f 2>/dev/null",
            UnixShellKind::Zsh => {
                "{ builtin alias -L; builtin alias -gL; builtin alias -sL } 2>/dev/null; \
                 builtin typeset -f 2>/dev/null"
            }
        };
        let script = format!(
            "source \"$HOME/{rc}\" 2>/dev/null; \
             printf '\\x01'; {capture}; printf '\\x01'",
            rc = rc_file_name(shell)
        );

        let result = tokio::time::timeout(INIT_TIMEOUT, async {
            let mut cmd = tokio::process::Command::new(shell_binary(shell));
            cmd.args(["-lc", &script])
                .current_dir(cwd)
                .stdin(echo_agent_tty_utils::null_stdio())
                .stdout(Stdio::piped())
                .stderr(echo_agent_tty_utils::null_stdio())
                .kill_on_drop(true);
            crate::util::detach_command(&mut cmd);
            cmd.envs(crate::util::pager_env());
            #[allow(clippy::disallowed_methods)] // probe killed on drop
            let mut child = cmd.spawn().ok()?;

            let mut stdout_buf = Vec::new();
            if let Some(ref mut stdout) = child.stdout {
                stdout.read_to_end(&mut stdout_buf).await.ok();
            }
            let status = child.wait().await.ok()?;
            if !status.success() {
                return None;
            }

            let stdout = String::from_utf8_lossy(&stdout_buf);
            let parts: Vec<&str> = stdout.split('\x01').collect();
            parts.get(1).map(|s| s.to_string())
        })
        .await;

        let snapshot = match result {
            Ok(Some(s)) => s,
            Ok(None) => {
                tracing::warn!("static shell capture failed; using empty snapshot");
                String::new()
            }
            Err(_) => {
                tracing::warn!(
                    "static shell capture timed out after {}s; using empty snapshot",
                    INIT_TIMEOUT.as_secs()
                );
                String::new()
            }
        };
        let _ = INIT_MARKER;
        Self { snapshot, shell }
    }

    /// Build the replay wrapper: read the snapshot from fd 3, eval it (alias
    /// and function definitions), then eval the user command; the shell exits
    /// with the user command's status. A failing snapshot replay does not
    /// abort the command.
    pub fn prepare_command(
        &self,
        user_command: &str,
        search_shadows: super::SearchShadowConfig,
    ) -> std::io::Result<PreparedStaticCommand> {
        let sudo_inject = sudo_alias_injection();
        let search_inject = super::embedded_search_tools::search_injection(search_shadows);

        let (state_in_read, state_in_write) = os_pipe()?;
        set_cloexec(&state_in_write)?;

        // Copy $1 into a plain variable and clear the positional parameters
        // (`builtin set --`) BEFORE eval'ing the user command: `source
        // <script>` with no arguments makes the sourced script inherit the
        // caller's positional parameters, so e.g. conda's `bin/activate`
        // (which forwards "$@" to `conda activate`) would receive the entire
        // wrapped command string as an environment name. Clearing them
        // matches the plain `bash -c "<command>"` execution path, where $# is 0.
        //
        // The login snapshot can restore `allexport` (set -a), which would
        // auto-export the temp variable into the user command's child
        // processes — strip the export attribute post-assignment (inline
        // `declare +x var=value` does NOT beat allexport).
        let wrapper = match self.shell {
            UnixShellKind::Bash => format!(
                "snap=$(command cat <&3); builtin shopt -s extglob 2>/dev/null; \
                 builtin shopt -s expand_aliases 2>/dev/null; \
                 builtin eval -- \"$snap\"; \
                 builtin export ECHO_AGENT_AGENT=1; \
                 builtin export PWD=\"$(builtin pwd)\"; {sudo_inject}{search_inject}\
                 __echo_agent_user_cmd=\"$1\"; builtin declare +x __echo_agent_user_cmd 2>/dev/null; builtin set --; \
                 builtin eval \"$__echo_agent_user_cmd\" 2>&1"
            ),
            UnixShellKind::Zsh => format!(
                "snap=$(command cat <&3); \
                 builtin setopt nonomatch 2>/dev/null; \
                 builtin eval \"$snap\"; \
                 builtin export ECHO_AGENT_AGENT=1; \
                 builtin export PWD=\"$(builtin pwd)\"; \
                 builtin setopt aliases 2>/dev/null; {sudo_inject}{search_inject}\
                 __echo_agent_user_cmd=\"$1\"; builtin typeset +x __echo_agent_user_cmd 2>/dev/null; builtin set --; \
                 builtin eval \"$__echo_agent_user_cmd\" 2>&1"
            ),
        };

        let args: Vec<String> = match self.shell {
            UnixShellKind::Bash => vec![
                "-O".into(),
                "extglob".into(),
                "-c".into(),
                wrapper,
                "--".into(),
                user_command.into(),
            ],
            UnixShellKind::Zsh => vec!["-c".into(), wrapper, "--".into(), user_command.into()],
        };

        Ok(PreparedStaticCommand {
            binary: shell_binary(self.shell).to_string(),
            args,
            fd_mappings: vec![FdMapping {
                parent_fd: state_in_read,
                child_fd: 3,
            }],
            state_in_write,
        })
    }
}

pub struct PreparedStaticCommand {
    pub binary: String,
    pub args: Vec<String>,
    pub fd_mappings: Vec<FdMapping>,
    pub state_in_write: OwnedFd,
}

/// Write the snapshot to the pipe, then close the fd so the child sees EOF.
pub async fn write_snapshot_to_pipe(snapshot: &str, fd: OwnedFd) -> std::io::Result<()> {
    let data = snapshot.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        // Safety: we own the fd.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd.as_raw_fd()) };
        std::mem::forget(fd);
        file.write_all(data.as_bytes())?;
        file.flush()?;
        drop(file);
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

fn os_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    #[cfg(target_os = "linux")]
    {
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let (read_fd, write_fd) =
            nix::unistd::pipe().map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
        let _ = set_cloexec(&read_fd);
        let _ = set_cloexec(&write_fd);
        Ok((read_fd, write_fd))
    }
}

fn set_cloexec(fd: &OwnedFd) -> std::io::Result<()> {
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ret = unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use command_fds::CommandFdExt;

    fn bash_available() -> bool {
        std::path::Path::new("/bin/bash").exists()
    }

    async fn run_static(snapshot: &str, command: &str) -> std::process::Output {
        let state = StaticShellSnapshot {
            snapshot: snapshot.to_string(),
            shell: UnixShellKind::Bash,
        };
        let prep = state
            .prepare_command(
                command,
                crate::computer::local::SearchShadowConfig::default(),
            )
            .unwrap();
        let mut cmd = tokio::process::Command::new(&prep.binary);
        cmd.args(&prep.args)
            .current_dir(std::env::current_dir().unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd.fd_mappings(prep.fd_mappings).unwrap();
        #[allow(clippy::disallowed_methods)] // test fixture; the test reaps it
        let child = cmd.spawn().unwrap();
        drop(cmd);

        let snap = state.snapshot.clone();
        let write_handle =
            tokio::spawn(async move { write_snapshot_to_pipe(&snap, prep.state_in_write).await });
        let output = child.wait_with_output().await.unwrap();
        write_handle.await.unwrap().unwrap();
        output
    }

    #[tokio::test]
    async fn replays_aliases_and_functions() {
        if !bash_available() {
            return;
        }
        let output = run_static(
            "alias echo_agent_alias_probe='echo ALIAS_OK'\necho_agent_fn_probe() { echo FN_OK; }\n",
            "echo_agent_alias_probe && echo_agent_fn_probe",
        )
        .await;
        assert!(output.status.success(), "command failed: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("ALIAS_OK") && stdout.contains("FN_OK"),
            "alias and function must be replayed: {stdout:?}"
        );
    }

    #[tokio::test]
    async fn user_command_exit_code_propagates_past_bad_snapshot() {
        if !bash_available() {
            return;
        }
        let output = run_static("this-is-not-a-command 2>/dev/null\n", "exit 7").await;
        assert_eq!(
            output.status.code(),
            Some(7),
            "user command exit code must propagate: {output:?}"
        );
    }

    #[tokio::test]
    async fn empty_snapshot_runs_plain() {
        if !bash_available() {
            return;
        }
        let output = run_static("", "echo PLAIN_OK").await;
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("PLAIN_OK"),
            "empty snapshot must degrade to a plain shell"
        );
    }

    /// Regression test: a script sourced WITHOUT arguments by the user command
    /// must not see the wrapper's positional parameters ($1 = the whole
    /// command string). Conda's `bin/activate` forwards "$@" to `conda
    /// activate`, so a leak makes every `activate_conda`-prefixed command fail
    /// with `EnvironmentLocationNotFound: Not a conda environment: <cwd>/<the
    /// entire command string>`.
    #[tokio::test]
    async fn sourced_script_does_not_inherit_wrapper_positional_args() {
        if !bash_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("activate_probe.sh");
        std::fs::write(&probe, "echo \"SOURCED_ARGC=$#\"\n").unwrap();

        let output = run_static(
            "",
            &format!("source {} && echo AFTER_SOURCE_OK", probe.display()),
        )
        .await;
        assert!(output.status.success(), "command failed: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("SOURCED_ARGC=0"),
            "sourced script must see zero positional args, got: {stdout:?}"
        );
        assert!(stdout.contains("AFTER_SOURCE_OK"), "got: {stdout:?}");
    }
}
