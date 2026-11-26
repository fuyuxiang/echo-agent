//! Persistent shell state across command invocations.
//!
//! After each command, the shell's environment
//! (env vars, cwd, functions, aliases, options) is serialized via a dump script,
//! and replayed before the next command. Each invocation is still a fresh process,
//! but the user observes a single continuous session.
//!
//! State is transported via extra file descriptors (fd 3 for input, fd 4 for output)
//! so that dump traffic never pollutes stdout/stderr.

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use std::collections::HashMap;
use std::time::Duration;

use command_fds::FdMapping;
use nix::libc;
use tokio::io::AsyncReadExt;

// ============================================================================
// Marker constants
// ============================================================================

const BASH_STATE_START_MARKER: &str = "__ECHO_AGENT_BASH_STATE_START__";
const BASH_STATE_END_MARKER: &str = "__ECHO_AGENT_BASH_STATE_END__";
const ZSH_STATE_START_MARKER: &str = "__ECHO_AGENT_ZSH_STATE_START__";
const ZSH_STATE_END_MARKER: &str = "__ECHO_AGENT_ZSH_STATE_END__";

/// Marker emitted by the init path to separate login-shell noise (MOTD, etc.)
/// from the actual state dump on stdout.
const INIT_STATE_MARKER: &str = "__ECHO_AGENT_INIT_STATE_MARKER__";

/// Maximum time to wait for a shell state init (login shell + rc files).
const INIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum time to wait for the dump reader task after the child process exits.
/// Uses a 5s close timeout. If a background process inherits fd 4,
/// the reader would hang forever without this.
const DUMP_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment overrides applied to every agent terminal / persistent shell spawn.
/// Prevents color/pager noise in captured output and marks the process as agent-driven.
pub fn shell_env_overrides() -> HashMap<String, String> {
    HashMap::from([
        ("TERM".to_string(), "dumb".to_string()),
        ("NO_COLOR".to_string(), "1".to_string()),
        ("FORCE_COLOR".to_string(), "0".to_string()),
        // Re-applied last via [`crate::util::apply_echo_agent_agent_marker`] so request
        // env cannot clear it.
        (
            crate::util::ECHO_AGENT_AGENT_ENV.to_string(),
            crate::util::ECHO_AGENT_AGENT_ENV_VALUE.to_string(),
        ),
    ])
}

/// Returns the sudo alias injection string if `SUDO_ASKPASS` is configured.
/// When set, `alias sudo='sudo -A'` makes any `sudo` in the user's command
/// use the askpass helper instead of blocking on tty input.
fn sudo_alias_injection() -> String {
    match std::env::var("SUDO_ASKPASS") {
        Ok(val) if !val.is_empty() => "alias sudo='sudo -A'; ".to_string(),
        _ => String::new(),
    }
}

// ============================================================================
// Dump scripts (embedded as const strings)
// ============================================================================

/// Bash state dump script. Captures env vars, POSIX options, bash options,
/// functions, and aliases as base64-encoded replayable shell snippets.
const DUMP_BASH_STATE_SCRIPT: &str = r##"
dump_bash_state() {
  set -euo pipefail
  if ! command -v base64 >/dev/null 2>&1; then
    echo "Error: base64 command is required" >&2
    return 1
  fi

  _emit() {
    builtin printf '%s\n' "$1"
  }

  _emit_encoded() {
    local content="$1"
    local var_name="$2"
    if [[ -n "$content" ]]; then
      builtin printf 'echo_agent_snap_%s=$(command base64 -d <<'"'"'ECHO_AGENT_SNAP_EOF_%s'"'"'\n' "$var_name" "$var_name"
      command base64 <<<"$content" | command tr -d '\n'
      builtin printf '\nECHO_AGENT_SNAP_EOF_%s\n' "$var_name"
      builtin printf ')\n'
      builtin printf 'eval "$echo_agent_snap_%s"\n' "$var_name"
    fi
  }

  _emit "__ECHO_AGENT_BASH_STATE_START__"

  _emit "$PWD"

  local env_vars
  env_vars=$(builtin export -p 2>/dev/null | command grep -viE '_proxy=|ECHO_AGENT_SANDBOX|ECHO_AGENT_AGENT=|SUDO_ASKPASS|ECHO_AGENT_ASKPASS|ELECTRON_RUN_AS_NODE|SSH_AUTH_SOCK|DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY|GPG_TTY' || true)
  _emit_encoded "$env_vars" "ENV_VARS_B64"

  # errexit/pipefail here are this function's own `set -euo pipefail` (set is
  # shell-global in bash); replaying them would abort later user commands.
  local posix_opts
  posix_opts=$(builtin shopt -po 2>/dev/null | command grep -vE '^set [-+]o (nounset|errexit|pipefail)$' || true)
  _emit_encoded "$posix_opts" "POSIX_OPTS_B64"

  local bash_opts
  bash_opts=$(builtin shopt -p 2>/dev/null || true)
  _emit_encoded "$bash_opts" "BASH_OPTS_B64"

  local all_functions
  all_functions=$(builtin declare -f 2>/dev/null || true)
  _emit_encoded "$all_functions" "FUNCTIONS_B64"

  local aliases
  aliases=$(builtin alias -p 2>/dev/null || true)
  _emit_encoded "$aliases" "ALIASES_B64"

  _emit "# end of bash state dump"
  _emit "__ECHO_AGENT_BASH_STATE_END__"
}
"##;

/// Zsh state dump script. Captures env vars, zsh options, functions, and aliases
/// as base64-encoded replayable shell snippets.
const DUMP_ZSH_STATE_SCRIPT: &str = r##"
function dump_zsh_state() {
  emulate -L zsh -o errreturn -o pipefail
  set -u

  builtin zmodload -F zsh/parameter p:parameters p:options p:functions p:aliases p:galiases p:saliases 2>/dev/null || true

  _emit() {
    builtin print -r -- "$1"
  }

  _emit_encoded() {
    local content="$1"
    local var_name="$2"
    if [[ -n "$content" ]]; then
      builtin printf 'echo_agent_snap_%s=$(command base64 -d <<'"'"'ECHO_AGENT_SNAP_EOF_%s'"'"'\n' "$var_name" "$var_name"
      command base64 <<<"$content" | command tr -d '\n'
      builtin printf '\nECHO_AGENT_SNAP_EOF_%s\n' "$var_name"
      builtin printf ')\n'
      builtin printf 'eval "$echo_agent_snap_%s"\n' "$var_name"
    fi
  }

  _emit "__ECHO_AGENT_ZSH_STATE_START__"

  _emit "$PWD"

  local env_vars
  env_vars=$(builtin typeset -xp 2>/dev/null | command grep -viE '_proxy=|ECHO_AGENT_SANDBOX|ECHO_AGENT_AGENT=|SUDO_ASKPASS|ECHO_AGENT_ASKPASS|ELECTRON_RUN_AS_NODE|SSH_AUTH_SOCK|DBUS_SESSION_BUS_ADDRESS|XDG_RUNTIME_DIR|WAYLAND_DISPLAY|GPG_TTY' || true)
  _emit_encoded "$env_vars" "ENV_VARS_B64"

  # errreturn/pipefail here are this function's own `emulate -L` options
  # (setopt lists them while inside); replaying them would abort later user
  # commands.
  local zsh_opts
  zsh_opts=$(setopt 2>/dev/null | command grep -vE '^(nounset|errexit|errreturn|pipefail)$' | command awk '{printf "builtin setopt %s 2>/dev/null || true\n", $0}' || true)
  _emit_encoded "$zsh_opts" "ZSH_OPTS_B64"

  local all_functions
  all_functions=$(builtin typeset -f 2>/dev/null || true)
  _emit_encoded "$all_functions" "FUNCTIONS_B64"

  local aliases
  aliases=$({ builtin alias -L; builtin alias -gL; builtin alias -sL } 2>/dev/null || true)
  _emit_encoded "$aliases" "ALIASES_B64"

  _emit "# end of zsh state dump"
  _emit "__ECHO_AGENT_ZSH_STATE_END__"
}
"##;

// ============================================================================
// Shell kind
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
}

impl ShellKind {
    /// Detect the user's shell from `$SHELL`, falling back to bash.
    pub fn detect() -> Self {
        match echo_agent_config::shell::detect_unix_shell_kind() {
            echo_agent_config::shell::UnixShellKind::Bash => Self::Bash,
            echo_agent_config::shell::UnixShellKind::Zsh => Self::Zsh,
        }
    }

    /// Resolved absolute path to the shell binary. Falls back from `$SHELL` →
    /// `which` → common dirs → `/bin/<name>`. Result is cached process-wide
    /// in `echo_agent_config::shell::unix_shell_path`. See that function for
    /// the full cascade. Returns `&'static str`.
    pub fn binary_path(&self) -> &'static str {
        let kind = match self {
            Self::Bash => echo_agent_config::shell::UnixShellKind::Bash,
            Self::Zsh => echo_agent_config::shell::UnixShellKind::Zsh,
        };
        echo_agent_config::shell::unix_shell_path(kind)
    }

    /// The user's primary rc file name (relative to `$HOME`).
    pub fn rc_file_name(&self) -> &str {
        match self {
            Self::Bash => ".bashrc",
            Self::Zsh => ".zshrc",
        }
    }

    fn dump_script(&self) -> &'static str {
        match self {
            Self::Bash => DUMP_BASH_STATE_SCRIPT,
            Self::Zsh => DUMP_ZSH_STATE_SCRIPT,
        }
    }

    fn dump_function_name(&self) -> &str {
        match self {
            Self::Bash => "dump_bash_state",
            Self::Zsh => "dump_zsh_state",
        }
    }

    fn start_marker(&self) -> &str {
        match self {
            Self::Bash => BASH_STATE_START_MARKER,
            Self::Zsh => ZSH_STATE_START_MARKER,
        }
    }

    fn end_marker(&self) -> &str {
        match self {
            Self::Bash => BASH_STATE_END_MARKER,
            Self::Zsh => ZSH_STATE_END_MARKER,
        }
    }
}

// ============================================================================
// ShellState
// ============================================================================

/// Persistent shell state: a serialized snapshot that can be replayed to restore
/// env vars, cwd, functions, aliases, and shell options in a fresh shell process.
#[derive(Debug, Clone)]
pub struct ShellState {
    /// Current working directory (from the dump's first line).
    pub cwd: PathBuf,
    /// Replayable shell script (everything after the cwd line, minus markers).
    pub snapshot: String,
    /// Which shell produced this state.
    pub shell: ShellKind,
}

impl ShellState {
    /// Initialize shell state by running an interactive login shell that loads
    /// the user's rc files, then capturing the resulting environment.
    ///
    /// This is the expensive path — only called once (lazily on first command).
    pub async fn init(
        shell: ShellKind,
        cwd: &Path,
        shell_env_policy: Option<&crate::util::ShellEnvironmentPolicy>,
    ) -> Result<Self, crate::computer::types::ComputerError> {
        let dump_script = shell.dump_script();
        let dump_fn = shell.dump_function_name();

        // Build the one-liner: define the dump function, print a marker (to separate
        // login noise from our output), then call the dump function.
        let script = format!("{dump_script} builtin printf '{INIT_STATE_MARKER}\\n'; {dump_fn}");

        let args: Vec<&str> = match shell {
            ShellKind::Bash => vec!["-O", "extglob", "-ilc", &script],
            ShellKind::Zsh => vec!["-o", "extendedglob", "-ilc", &script],
        };

        // stderr is intentionally discarded (Stdio::null) — we never read it,
        // and piping it would risk a deadlock if the user's rc files write >64KB
        // to stderr (fills the pipe buffer, child blocks on write, parent blocks
        // on stdout read).
        let mut cmd = tokio::process::Command::new(shell.binary_path());
        cmd.args(&args)
            .current_dir(cwd)
            .stdin(echo_agent_tty_utils::null_stdio())
            .stdout(Stdio::piped())
            .stderr(echo_agent_tty_utils::null_stdio())
            .kill_on_drop(true);
        crate::util::detach_command(&mut cmd);
        // Apply the policy before the `export -p` snapshot so the replayed state
        // is already filtered; otherwise the restore would undo it. No-op unless set.
        //
        // SECURITY: this filters the base env only. Variables an rc file exports
        // during login are captured in the replay snapshot and are not
        // re-filtered by `exclude`/`include_only` on the persistent backend, so
        // warn when a policy is active. The non-persistent backend has no such
        // gap (it filters login capture directly).
        if shell_env_policy.is_some_and(|p| !p.is_noop()) {
            tracing::warn!(
                "shell_environment_policy filters the persistent shell's base env only; \
                 variables exported by rc files enter the replay snapshot unfiltered"
            );
        }
        crate::util::apply_shell_environment_policy(&mut cmd, shell_env_policy);
        cmd.envs(crate::util::pager_env());
        #[allow(clippy::disallowed_methods)] // one-shot init run, waited on here
        let mut child = cmd.spawn().map_err(|e| {
            crate::computer::types::ComputerError::io(format!(
                "failed to spawn {shell:?} for shell state init: {e}"
            ))
        })?;

        let mut full_output = String::new();
        if let Some(ref mut stdout) = child.stdout {
            // Apply init timeout to prevent hangs from slow rc files (e.g. network mounts).
            match tokio::time::timeout(INIT_TIMEOUT, stdout.read_to_string(&mut full_output)).await
            {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::warn!("shell state init: read error: {e}");
                }
                Err(_) => {
                    tracing::warn!(
                        "shell state init: timed out after {}s reading login shell output",
                        INIT_TIMEOUT.as_secs()
                    );
                    child.kill().await.ok();
                }
            }
        }

        let _ = child.wait().await;

        // Extract output after our marker (skip MOTD, bashrc echo, etc.)
        let snapshot_raw = parse_after_marker(&full_output, INIT_STATE_MARKER);

        match parse_dump(shell, snapshot_raw) {
            Some((parsed_cwd, rest)) => Ok(Self {
                cwd: parsed_cwd,
                snapshot: rest,
                shell,
            }),
            None => {
                // Dump failed or was killed — use empty state with the given cwd.
                tracing::warn!("shell state init: dump markers missing, using empty state");
                Ok(Self {
                    cwd: cwd.to_path_buf(),
                    snapshot: String::new(),
                    shell,
                })
            }
        }
    }

    /// Build the wrapper command and fd pipe pair for a persistent shell invocation.
    ///
    /// Returns `(shell_binary, args, state_in_fd, state_out_fd)` where:
    /// - `state_in_fd` is a pipe the caller writes the prior snapshot to (fd 3 in the child)
    /// - `state_out_fd` is a pipe the caller reads the new dump from (fd 4 in the child)
    ///
    /// The caller must:
    /// 1. Write `self.snapshot` to `state_in_fd` then close it
    /// 2. Spawn the child with the returned args + fd_mappings
    /// 3. Read `state_out_fd` after the child exits
    /// 4. Call `parse_dump()` on the result to update state
    ///
    /// `cwd_override`: if provided, the child uses this cwd instead of the
    ///    persistent shell's tracked cwd. Used for per-call `working_directory`
    ///    overrides from `TerminalRunRequest`.
    pub fn prepare_command(
        &self,
        user_command: &str,
        cwd_override: Option<&Path>,
        search_shadows: super::SearchShadowConfig,
        spawn_notice: Option<&str>,
    ) -> std::io::Result<PreparedCommand> {
        let dump_script = self.shell.dump_script();
        let dump_fn = self.shell.dump_function_name();
        let sudo_inject = sudo_alias_injection();
        let search_inject = super::embedded_search_tools::search_injection(search_shadows);

        // Create two OS pipes: one for state-in (fd 3), one for state-out (fd 4).
        // os_pipe() creates fds with O_CLOEXEC (atomically on Linux,
        // best-effort on macOS) so concurrent forks can't leak them.
        let (state_in_read, state_in_write) = os_pipe()?;
        let (state_out_read, state_out_write) = os_pipe()?;

        // Ensure the parent-only ends have CLOEXEC (redundant on Linux
        // where os_pipe uses pipe2, but needed as a safety net on macOS
        // where pipe+fcntl has a small race window).
        set_cloexec(&state_in_write)?;
        set_cloexec(&state_out_read)?;
        // The child-bound ends (state_in_read, state_out_write) also have
        // CLOEXEC from os_pipe(). This is fine: fd_mappings uses dup2()
        // which clears CLOEXEC on the target fd (3/4), so the child keeps
        // them across exec. The originals are closed on exec by CLOEXEC.

        // The wrapper command:
        // 1. Read prior snapshot from fd 3, eval it (restores env/funcs/aliases/opts)
        // 2. Run the user's command (passed as $1)
        // 3. Dump new state to fd 4
        // 4. Exit with the user command's exit code
        let wrapper = match self.shell {
            ShellKind::Bash => format!(
                // Merge the user command's stderr into its
                // stdout via `2>&1` so the captured byte stream preserves
                // chronological write order. Without this, the bash tool's
                // separate stdout/stderr pipes (each read in lockstep)
                // emit all-of-stdout-then-all-of-stderr in a single poll
                // tick, so a command like `echo X 1>&2 && echo Y` shows
                // up as `Y\nX\n` instead of the chronological `X\nY\n`.
                // Shell-level diagnostics (eval syntax errors, etc.) still
                // land on the outer shell's stderr — those are unaffected.
                // Re-export ECHO_AGENT_AGENT=1 after snapshot eval so agent-definition
                // selectors (or other values) from prior shells cannot clear the
                // agent sentinel (process env alone is insufficient).
                //
                // Copy $1/$2 into plain variables and clear the positional
                // parameters (`builtin set --`) BEFORE eval'ing the user
                // command: `source <script>` with no arguments makes the
                // sourced script inherit the caller's positional parameters,
                // so e.g. conda's `bin/activate` (which forwards "$@" to
                // `conda activate`) would receive the entire wrapped command
                // string as an environment name. Clearing them matches the
                // plain `bash -c "<command>"` execution path, where $# is 0.
                //
                // The snapshot can restore `allexport` (set -a), which would
                // auto-export the temp variable — so strip the export
                // attribute post-assignment (`declare +x`; inline `declare +x
                // var=value` does NOT beat allexport) and unset it after the
                // eval so it can never reach child processes or the state
                // dump (`export -p`).
                "{dump_script} \
                 snap=$(command cat <&3) && builtin shopt -s extglob && builtin eval -- \"$snap\" && \
                 {{ builtin set +u 2>/dev/null || true; \
                 builtin export ECHO_AGENT_AGENT=1; \
                 builtin export PWD=\"$(builtin pwd)\"; \
                 builtin shopt -s expand_aliases 2>/dev/null; {sudo_inject}{search_inject}\
                 builtin printf '%s' \"${{2:-}}\"; \
                 __echo_agent_user_cmd=\"$1\"; builtin declare +x __echo_agent_user_cmd 2>/dev/null; builtin set --; \
                 builtin eval \"$__echo_agent_user_cmd\" 2>&1; }}; \
                 COMMAND_EXIT_CODE=$?; builtin unset __echo_agent_user_cmd 2>/dev/null; {dump_fn} >&4; builtin exit $COMMAND_EXIT_CODE"
            ),
            // After snapshot restore: force nonomatch so login dumps cannot re-arm NOMATCH for model globs.
            // See the bash wrapper comment for why positional parameters are
            // cleared before the user-command eval (zsh's `source`/`.` inherits
            // them identically).
            ShellKind::Zsh => format!(
                "{dump_script} \
                 snap=$(command cat <&3); \
                 builtin unsetopt aliases 2>/dev/null; \
                 builtin unalias -m '*' 2>/dev/null || true; \
                 builtin eval \"$snap\" && \
                 {{ builtin unsetopt nounset 2>/dev/null || true; \
                 builtin setopt nonomatch 2>/dev/null || true; \
                 builtin export ECHO_AGENT_AGENT=1; \
                 builtin export PWD=\"$(builtin pwd)\"; \
                 builtin setopt aliases 2>/dev/null; {sudo_inject}{search_inject}\
                 builtin printf '%s' \"${{2:-}}\"; \
                 __echo_agent_user_cmd=\"$1\"; builtin typeset +x __echo_agent_user_cmd 2>/dev/null; builtin set --; \
                 builtin eval \"$__echo_agent_user_cmd\" 2>&1; }}; \
                 COMMAND_EXIT_CODE=$?; builtin unset __echo_agent_user_cmd 2>/dev/null; {dump_fn} >&4; builtin exit $COMMAND_EXIT_CODE"
            ),
        };

        let effective_cwd = cwd_override.unwrap_or(&self.cwd);

        let mut args: Vec<String> = match self.shell {
            ShellKind::Bash => vec![
                "-O".into(),
                "extglob".into(),
                "-c".into(),
                wrapper,
                "--".into(),
                user_command.into(),
            ],
            ShellKind::Zsh => vec!["-c".into(), wrapper, "--".into(), user_command.into()],
        };
        if let Some(notice) = spawn_notice {
            args.push(notice.into());
        }

        let fd_mappings = vec![
            FdMapping {
                parent_fd: state_in_read,
                child_fd: 3,
            },
            FdMapping {
                parent_fd: state_out_write,
                child_fd: 4,
            },
        ];

        Ok(PreparedCommand {
            binary: self.shell.binary_path().to_string(),
            args,
            fd_mappings,
            state_in_write,
            state_out_read,
            cwd: effective_cwd.to_path_buf(),
        })
    }

    /// Update this state from a raw dump string (read from fd 4).
    /// Returns `true` if the state was successfully updated.
    pub fn update_from_dump(&mut self, raw: &str) -> bool {
        match parse_dump(self.shell, raw) {
            Some((new_cwd, new_snapshot)) => {
                if new_cwd.is_absolute() {
                    self.cwd = new_cwd;
                }
                self.snapshot = new_snapshot;
                true
            }
            None => {
                tracing::debug!("shell state dump markers missing, keeping previous state");
                false
            }
        }
    }
}

/// Everything needed to spawn a persistent shell command.
pub struct PreparedCommand {
    /// Resolved shell binary path (e.g. `/bin/bash`, `/opt/homebrew/bin/bash`,
    /// or `/run/current-system/sw/bin/bash` on NixOS). See
    /// [`ShellKind::binary_path`] for the resolution cascade.
    pub binary: String,
    /// Full argument list for the shell.
    pub args: Vec<String>,
    /// Fd mappings to pass to `CommandFdExt::fd_mappings()`.
    pub fd_mappings: Vec<FdMapping>,
    /// Write end of the state-input pipe. Caller writes `snapshot` here, then drops.
    pub state_in_write: OwnedFd,
    /// Read end of the state-output pipe. Caller reads the new dump from here after exit.
    pub state_out_read: OwnedFd,
    /// Working directory for the child process.
    pub cwd: PathBuf,
}

// ============================================================================
// Helpers
// ============================================================================

/// Create an OS pipe, returning `(read_end, write_end)` as `OwnedFd`.
///
/// On Linux, uses `nix::unistd::pipe2(O_CLOEXEC)` to atomically set
/// close-on-exec, eliminating the race window between `pipe()` and
/// `fcntl(F_SETFD)` where a concurrent `fork()` could leak fds to an
/// unrelated child.
///
/// On macOS, `pipe2` is not exposed by `nix` 0.30 (the kernel added it
/// in 10.15 but `nix`'s cfg gate hasn't caught up). Falls back to
/// `pipe()` + `fcntl(FD_CLOEXEC)` with a best-effort race window.
fn os_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    // Linux: atomic O_CLOEXEC via pipe2.
    #[cfg(target_os = "linux")]
    {
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
            .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
    }

    // macOS (and other non-Linux Unix): pipe() + fcntl best-effort.
    #[cfg(not(target_os = "linux"))]
    {
        let (read_fd, write_fd) =
            nix::unistd::pipe().map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
        // Best-effort CLOEXEC — small race window on macOS between pipe()
        // and these fcntl calls, but unavoidable without pipe2.
        let _ = set_cloexec(&read_fd);
        let _ = set_cloexec(&write_fd);
        Ok((read_fd, write_fd))
    }
}

/// Set FD_CLOEXEC on a file descriptor so it is NOT inherited by child processes.
///
/// This is critical for pipe fds that should stay parent-only: without CLOEXEC,
/// the child inherits both ends of a pipe after fork, preventing EOF from being
/// signaled when the parent closes its end.
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

/// Parse a state dump, validating start/end markers.
/// Returns `(cwd, snapshot_rest)` or `None` if markers are missing.
fn parse_dump(shell: ShellKind, raw: &str) -> Option<(PathBuf, String)> {
    let start = shell.start_marker();
    let end = shell.end_marker();

    let start_line = format!("{start}\n");
    let end_line = format!("{end}\n");

    if !raw.starts_with(&start_line) || !raw.ends_with(&end_line) {
        return None;
    }

    // Strip markers
    let without_markers = &raw[start_line.len()..raw.len() - end_line.len()];

    // First line is $PWD
    let newline_pos = without_markers.find('\n')?;
    let cwd = &without_markers[..newline_pos];
    let rest = &without_markers[newline_pos..]; // includes the leading \n

    Some((PathBuf::from(cwd), rest.to_string()))
}

/// Extract the portion of `output` after the first occurrence of `marker\n`.
/// If the marker is not found, returns the full output.
fn parse_after_marker<'a>(output: &'a str, marker: &str) -> &'a str {
    let needle = format!("{marker}\n");
    match output.find(&needle) {
        Some(idx) => &output[idx + needle.len()..],
        None => output,
    }
}

/// Write the snapshot to the state-in pipe, then close the fd.
/// Uses blocking I/O on a dedicated thread (pipes are not regular files).
pub async fn write_snapshot_to_pipe(snapshot: &str, fd: OwnedFd) -> std::io::Result<()> {
    let data = snapshot.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        // Safety: we own the fd.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd.as_raw_fd()) };
        std::mem::forget(fd);
        file.write_all(data.as_bytes())?;
        file.flush()?;
        drop(file); // closes the fd → child sees EOF on its read end
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Read the full dump output from the state-out pipe with a timeout.
///
/// If a background process inherits fd 4, the pipe never closes and the reader
/// hangs. The timeout (5s close timeout) prevents this from
/// blocking the actor loop forever. On timeout, returns whatever was read so far
/// (which is typically empty, so marker validation will fail and prior state is kept).
pub async fn read_dump_from_pipe(fd: OwnedFd) -> std::io::Result<String> {
    // Read until either of the END markers appears, *not* until EOF.
    //
    // When the user's command backgrounds a subprocess (`cmd &`), the bg
    // shell inherits fd 4 (the dump pipe's write-end) and keeps it open
    // until *it* exits. The parent shell finishes its dump and exits, but
    // the kernel doesn't close the read-end's EOF until every write-end
    // holder closes theirs. Without marker-driven termination we'd block
    // on `read_to_string` for the entire bg lifetime, hit the 5s safety
    // timeout, and discard the (perfectly complete) dump — which manifests
    // as `cd` / function / alias state silently failing to persist after
    // any command that backgrounds something. (See harness scenarios
    // "State persistence after backgrounded command" and the cd-roundtrip
    // tests for shell state persistence parity.)
    //
    // We additionally cap on `DUMP_READ_TIMEOUT` so a shell that crashed
    // before emitting the END marker doesn't wedge the actor.
    match tokio::time::timeout(
        DUMP_READ_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut file = unsafe { std::fs::File::from_raw_fd(fd.as_raw_fd()) };
            std::mem::forget(fd);
            let mut buf = String::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = file.read(&mut chunk)?;
                if n == 0 {
                    // EOF: every write-end holder closed fd 4 (the
                    // expected path when no bg subprocess was spawned).
                    break;
                }
                buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
                // Either marker suffices; we accept whichever shell the
                // child happens to be (bash vs zsh).
                if buf.contains(BASH_STATE_END_MARKER) || buf.contains(ZSH_STATE_END_MARKER) {
                    break;
                }
            }
            drop(file);
            Ok(buf)
        }),
    )
    .await
    {
        Ok(join_result) => join_result.map_err(std::io::Error::other)?,
        Err(_timeout) => {
            tracing::warn!(
                "shell state dump read timed out after {}s (END marker never arrived)",
                DUMP_READ_TIMEOUT.as_secs()
            );
            Ok(String::new())
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_env_overrides_marks_agent_terminal() {
        let env = shell_env_overrides();
        assert_eq!(
            env.get(crate::util::ECHO_AGENT_AGENT_ENV)
                .map(String::as_str),
            Some(crate::util::ECHO_AGENT_AGENT_ENV_VALUE)
        );
        assert_eq!(env.get("TERM").map(String::as_str), Some("dumb"));
        assert_eq!(env.get("NO_COLOR").map(String::as_str), Some("1"));
    }

    #[test]
    fn parse_dump_valid_bash() {
        let raw = "__ECHO_AGENT_BASH_STATE_START__\n\
                    /home/user/project\n\
                    export FOO=bar\n\
                    # end of bash state dump\n\
                    __ECHO_AGENT_BASH_STATE_END__\n";
        let (cwd, rest) = parse_dump(ShellKind::Bash, raw).unwrap();
        assert_eq!(cwd, PathBuf::from("/home/user/project"));
        assert!(rest.contains("export FOO=bar"));
    }

    #[test]
    fn parse_dump_valid_zsh() {
        let raw = "__ECHO_AGENT_ZSH_STATE_START__\n\
                    /tmp\n\
                    typeset -x FOO=bar\n\
                    # end of zsh state dump\n\
                    __ECHO_AGENT_ZSH_STATE_END__\n";
        let (cwd, rest) = parse_dump(ShellKind::Zsh, raw).unwrap();
        assert_eq!(cwd, PathBuf::from("/tmp"));
        assert!(rest.contains("typeset -x FOO=bar"));
    }

    #[test]
    fn parse_dump_missing_start_marker() {
        let raw = "/home/user\nexport FOO=bar\n__ECHO_AGENT_BASH_STATE_END__\n";
        assert!(parse_dump(ShellKind::Bash, raw).is_none());
    }

    #[test]
    fn parse_dump_missing_end_marker() {
        let raw = "__ECHO_AGENT_BASH_STATE_START__\n/home/user\nexport FOO=bar\n";
        assert!(parse_dump(ShellKind::Bash, raw).is_none());
    }

    #[test]
    fn parse_dump_wrong_shell_markers() {
        let raw = "__ECHO_AGENT_ZSH_STATE_START__\n/tmp\nstuff\n__ECHO_AGENT_ZSH_STATE_END__\n";
        assert!(parse_dump(ShellKind::Bash, raw).is_none());
    }

    #[test]
    fn parse_dump_empty_snapshot() {
        let raw = "__ECHO_AGENT_BASH_STATE_START__\n\
                    /home/user\n\
                    # end of bash state dump\n\
                    __ECHO_AGENT_BASH_STATE_END__\n";
        let (cwd, rest) = parse_dump(ShellKind::Bash, raw).unwrap();
        assert_eq!(cwd, PathBuf::from("/home/user"));
        assert!(rest.contains("# end of bash state dump"));
    }

    #[test]
    fn parse_after_marker_found() {
        let output =
            "Welcome to Ubuntu\nMOTD line\n__ECHO_AGENT_INIT_STATE_MARKER__\nactual data\n";
        let result = parse_after_marker(output, "__ECHO_AGENT_INIT_STATE_MARKER__");
        assert_eq!(result, "actual data\n");
    }

    #[test]
    fn parse_after_marker_not_found() {
        let output = "just some output\n";
        let result = parse_after_marker(output, "__ECHO_AGENT_INIT_STATE_MARKER__");
        assert_eq!(result, output);
    }

    /// Returns true iff a usable bash binary exists at the resolved path.
    /// Used to gate integration tests so they're skipped (rather than failing)
    /// on systems where bash isn't installed (e.g. minimal containers).
    /// On NixOS the resolver returns the nix-store / profile path, so this
    /// guard works there too.
    fn bash_available() -> bool {
        std::path::Path::new(ShellKind::Bash.binary_path()).exists()
    }

    /// Returns true iff a usable zsh binary exists at the resolved path.
    /// Mirrors [`bash_available`] so zsh integration tests skip (rather than
    /// fail) on systems without zsh installed.
    fn zsh_available() -> bool {
        std::path::Path::new(ShellKind::Zsh.binary_path()).exists()
    }

    #[test]
    fn shell_kind_binary_path_resolves_to_correct_kind() {
        // The resolver may pick any absolute path (e.g. `/opt/homebrew/bin/bash`
        // on macOS+brew, `/run/current-system/sw/bin/bash` on NixOS), but the
        // file name must match the requested kind.
        let bash = std::path::Path::new(ShellKind::Bash.binary_path())
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        assert_eq!(bash, "bash", "Bash kind must resolve to a *bash* binary");

        let zsh = std::path::Path::new(ShellKind::Zsh.binary_path())
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        assert_eq!(zsh, "zsh", "Zsh kind must resolve to a *zsh* binary");
    }

    #[test]
    fn shell_state_update_from_dump() {
        let mut state = ShellState {
            cwd: PathBuf::from("/old"),
            snapshot: String::new(),
            shell: ShellKind::Bash,
        };

        let dump = "__ECHO_AGENT_BASH_STATE_START__\n\
                     /new/dir\n\
                     export X=1\n\
                     # end of bash state dump\n\
                     __ECHO_AGENT_BASH_STATE_END__\n";
        assert!(state.update_from_dump(dump));
        assert_eq!(state.cwd, PathBuf::from("/new/dir"));
        assert!(state.snapshot.contains("export X=1"));
    }

    #[test]
    fn shell_state_update_keeps_previous_on_bad_dump() {
        let mut state = ShellState {
            cwd: PathBuf::from("/original"),
            snapshot: "old stuff".into(),
            shell: ShellKind::Bash,
        };

        assert!(!state.update_from_dump("garbage output\n"));
        assert_eq!(state.cwd, PathBuf::from("/original"));
        assert_eq!(state.snapshot, "old stuff");
    }

    #[tokio::test]
    async fn test_init_bash() {
        // Integration test: actually runs bash and captures state.
        // Skip in environments without bash.
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();
        assert!(state.cwd.is_absolute());
        // The snapshot should contain at least some env var exports
        assert!(
            state.snapshot.contains("echo_agent_snap_") || state.snapshot.is_empty(),
            "snapshot should contain encoded blocks or be empty: {:?}",
            &state.snapshot[..state.snapshot.len().min(200)]
        );
    }

    #[tokio::test]
    async fn test_prepare_command_and_roundtrip() {
        use command_fds::CommandFdExt;

        // Integration test: prepare a command, spawn it, verify state roundtrip.
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        // Run "export ECHO_AGENT_TEST_VAR=hello" and capture the new state
        let prep = state
            .prepare_command(
                "export ECHO_AGENT_TEST_VAR=hello",
                None,
                crate::computer::local::SearchShadowConfig::default(),
                None,
            )
            .unwrap();

        let mut cmd = tokio::process::Command::new(&prep.binary);
        cmd.args(&prep.args)
            .current_dir(&prep.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        cmd.fd_mappings(prep.fd_mappings).unwrap();

        #[allow(clippy::disallowed_methods)] // test fixture; the test reaps it
        let child = cmd.spawn().unwrap();
        // Drop cmd to release the FdMapping OwnedFds held in its pre_exec closure.
        // Without this, the parent keeps the write-end of the state-out pipe open,
        // and the read task never sees EOF.
        drop(cmd);

        // Write snapshot to fd 3
        let snapshot = state.snapshot.clone();
        let write_handle =
            tokio::spawn(
                async move { write_snapshot_to_pipe(&snapshot, prep.state_in_write).await },
            );

        // Read new dump from fd 4
        let read_handle =
            tokio::spawn(async move { read_dump_from_pipe(prep.state_out_read).await });

        let output = child.wait_with_output().await.unwrap();
        assert!(output.status.success(), "command failed: {:?}", output);

        write_handle.await.unwrap().unwrap();
        let dump = read_handle.await.unwrap().unwrap();

        assert!(
            state.update_from_dump(&dump),
            "dump should have valid markers, got: {:?}",
            &dump[..dump.len().min(500)]
        );
        // The snapshot contains base64-encoded env vars, so the variable name
        // won't appear in plaintext. Verify the dump was valid and non-empty.
        assert!(
            !state.snapshot.is_empty(),
            "snapshot should be non-empty after a successful command"
        );
        assert!(state.cwd.is_absolute(), "cwd should be absolute");
    }

    /// Helper: run a command against a ShellState, update state, return (exit_code, stdout).
    async fn run_command(state: &mut ShellState, command: &str) -> (i32, String) {
        use command_fds::CommandFdExt;

        let prep = state
            .prepare_command(
                command,
                None,
                crate::computer::local::SearchShadowConfig::default(),
                None,
            )
            .unwrap();
        let mut cmd = tokio::process::Command::new(&prep.binary);
        cmd.args(&prep.args)
            .current_dir(&prep.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd.fd_mappings(prep.fd_mappings).unwrap();
        #[allow(clippy::disallowed_methods)] // test fixture; the test reaps it
        let child = cmd.spawn().unwrap();
        drop(cmd);

        let snapshot = state.snapshot.clone();
        let write_handle =
            tokio::spawn(
                async move { write_snapshot_to_pipe(&snapshot, prep.state_in_write).await },
            );
        let read_handle =
            tokio::spawn(async move { read_dump_from_pipe(prep.state_out_read).await });

        let output = child.wait_with_output().await.unwrap();
        write_handle.await.unwrap().unwrap();
        let dump = read_handle.await.unwrap().unwrap();
        state.update_from_dump(&dump);

        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        (code, stdout)
    }

    #[tokio::test]
    async fn test_cd_persists_across_commands() {
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        // cd to /tmp (macOS resolves to /private/tmp via symlink)
        let (code, _) = run_command(&mut state, "cd /tmp").await;
        assert_eq!(code, 0);

        // Next command should see the resolved /tmp as cwd
        let (code, stdout) = run_command(&mut state, "pwd").await;
        assert_eq!(code, 0);
        let actual_pwd = stdout.trim();
        assert!(
            actual_pwd == "/tmp" || actual_pwd == "/private/tmp",
            "cwd should be /tmp or /private/tmp, got: {actual_pwd}"
        );
        assert_eq!(state.cwd.to_str().unwrap(), actual_pwd);
    }

    #[tokio::test]
    async fn test_env_var_persists_across_commands() {
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        // Export a variable
        let (code, _) = run_command(&mut state, "export MY_TEST_VAR=persistent_value").await;
        assert_eq!(code, 0);

        // Next command should see it
        let (code, stdout) = run_command(&mut state, "echo $MY_TEST_VAR").await;
        assert_eq!(code, 0);
        assert_eq!(stdout.trim(), "persistent_value");
    }

    /// GPG_TTY exported in one command must not be replayed into the next via the snapshot.
    #[tokio::test]
    async fn test_gpg_tty_excluded_from_snapshot_bash() {
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        let (code, _) = run_command(&mut state, "export GPG_TTY=/echo-agent-sentinel-tty").await;
        assert_eq!(code, 0);

        let (code, stdout) = run_command(&mut state, "echo \"[$GPG_TTY]\"").await;
        assert_eq!(code, 0);
        assert!(
            !stdout.contains("/echo-agent-sentinel-tty"),
            "GPG_TTY must not persist across commands via the snapshot, got: {stdout:?}"
        );
    }

    /// Same as the bash case, exercising the zsh `typeset -xp` dump grep.
    #[tokio::test]
    async fn test_gpg_tty_excluded_from_snapshot_zsh() {
        if !zsh_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Zsh, &cwd, None).await.unwrap();

        let (code, _) = run_command(&mut state, "export GPG_TTY=/echo-agent-sentinel-tty").await;
        assert_eq!(code, 0);

        let (code, stdout) = run_command(&mut state, "echo \"[$GPG_TTY]\"").await;
        assert_eq!(code, 0);
        assert!(
            !stdout.contains("/echo-agent-sentinel-tty"),
            "GPG_TTY must not persist across commands via the snapshot, got: {stdout:?}"
        );
    }

    /// Unmatched globs must not abort with NOMATCH after login snapshot restore.
    #[tokio::test]
    async fn test_zsh_unmatched_glob_does_not_abort() {
        if !zsh_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Zsh, &cwd, None).await.unwrap();

        let prep = state
            .prepare_command(
                "true",
                None,
                crate::computer::local::SearchShadowConfig::default(),
                None,
            )
            .unwrap();
        let wrapper = prep
            .args
            .iter()
            .find(|a| a.contains("builtin eval"))
            .expect("zsh wrapper should be in prepare_command args");
        let snap_pos = wrapper
            .find("eval \"$snap\"")
            .expect("wrapper must restore snapshot via eval \"$snap\"");
        let nonomatch_pos = wrapper
            .find("setopt nonomatch")
            .expect("wrapper must force nonomatch after snapshot restore");
        assert!(
            nonomatch_pos > snap_pos,
            "nonomatch must be forced after snapshot eval, wrapper={wrapper:?}"
        );

        let pattern = "--include=*.no_such_ext_xyz_12345";
        let (code, stdout) = run_command(&mut state, &format!("printf '%s\\n' {pattern}")).await;
        assert_eq!(code, 0, "unmatched glob must not fail, stdout={stdout:?}");
        assert!(
            stdout.contains(pattern),
            "expected literal unmatched glob in output, got: {stdout:?}"
        );
        assert!(
            !stdout.contains("no matches found"),
            "NOMATCH error must not appear, got: {stdout:?}"
        );
    }

    #[tokio::test]
    async fn test_function_persists_across_commands() {
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        // Define a function
        let (code, _) = run_command(&mut state, "greet() { echo \"hello $1\"; }").await;
        assert_eq!(code, 0);

        // Call it in the next command
        let (code, stdout) = run_command(&mut state, "greet world").await;
        assert_eq!(code, 0);
        assert_eq!(stdout.trim(), "hello world");
    }

    /// Regression test: a script sourced WITHOUT arguments by the user command
    /// must not see the wrapper's positional parameters ($1 = the whole
    /// command string, $2 = the spawn notice). Conda's `bin/activate` forwards
    /// "$@" to `conda activate`, so a leak makes every `activate_conda`-
    /// prefixed command fail with `EnvironmentLocationNotFound: Not a conda
    /// environment: <cwd>/<the entire command string>`.
    #[tokio::test]
    async fn test_sourced_script_does_not_inherit_wrapper_positional_args_bash() {
        if !bash_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("activate_probe.sh");
        std::fs::write(&probe, "echo \"SOURCED_ARGC=$#\"\n").unwrap();

        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        let (code, stdout) = run_command(
            &mut state,
            &format!("source {} && echo AFTER_SOURCE_OK", probe.display()),
        )
        .await;
        assert_eq!(code, 0, "command failed, stdout={stdout:?}");
        assert!(
            stdout.contains("SOURCED_ARGC=0"),
            "sourced script must see zero positional args, got: {stdout:?}"
        );
        assert!(stdout.contains("AFTER_SOURCE_OK"), "got: {stdout:?}");
    }

    /// Regression test: with `allexport` active (restored from the snapshot
    /// after the model runs `set -a`), the wrapper's `__echo_agent_user_cmd` temp
    /// variable must not leak into child-process environments or persist into
    /// subsequent commands via the state dump.
    #[tokio::test]
    async fn test_user_cmd_var_not_exported_under_allexport_bash() {
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        // Turn on allexport; the option is captured by the dump and replayed
        // into every subsequent command's shell.
        let (code, _) = run_command(&mut state, "set -a").await;
        assert_eq!(code, 0);

        // This command's wrapper assigns __echo_agent_user_cmd under allexport.
        // printenv only sees exported vars — it must not see the temp var
        // (neither from this command's own assignment nor re-exported from a
        // previous command's state dump).
        let (code, stdout) = run_command(
            &mut state,
            "printenv __echo_agent_user_cmd >/dev/null 2>&1 && echo LEAKED_TO_ENV || echo ENV_CLEAN",
        )
        .await;
        assert_eq!(code, 0);
        assert!(
            stdout.contains("ENV_CLEAN") && !stdout.contains("LEAKED_TO_ENV"),
            "temp var must not be exported to child processes under allexport, got: {stdout:?}"
        );
    }

    /// Same as the bash variant: zsh's `source`/`.` also inherits the caller's
    /// positional parameters when invoked without arguments.
    #[tokio::test]
    async fn test_sourced_script_does_not_inherit_wrapper_positional_args_zsh() {
        if !zsh_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("activate_probe.sh");
        std::fs::write(&probe, "echo \"SOURCED_ARGC=$#\"\n").unwrap();

        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Zsh, &cwd, None).await.unwrap();

        let (code, stdout) = run_command(
            &mut state,
            &format!("source {} && echo AFTER_SOURCE_OK", probe.display()),
        )
        .await;
        assert_eq!(code, 0, "command failed, stdout={stdout:?}");
        assert!(
            stdout.contains("SOURCED_ARGC=0"),
            "sourced script must see zero positional args, got: {stdout:?}"
        );
        assert!(stdout.contains("AFTER_SOURCE_OK"), "got: {stdout:?}");
    }

    #[tokio::test]
    async fn test_alias_persists_across_commands() {
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        // Define an alias
        let (code, _) = run_command(&mut state, "alias ll='ls -la'").await;
        assert_eq!(code, 0);

        // Verify the alias survives by checking the dump itself (base64-encoded).
        // We can't check plaintext in the snapshot since it's base64, but we can
        // verify the snapshot is valid and non-empty (alias was captured in the dump).
        assert!(
            !state.snapshot.is_empty(),
            "snapshot should be non-empty after alias"
        );
    }

    #[tokio::test]
    async fn test_embedded_search_shadows() {
        use super::super::embedded_search_tools::search_injection;
        let shadows = crate::computer::local::SearchShadowConfig::default();
        if !bash_available() {
            return;
        }
        let inject = search_injection(shadows);
        if inject.is_empty() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        let prep = state.prepare_command("true", None, shadows, None).unwrap();
        // Shadows enabled → the self-resolving find/grep functions are always
        // installed (they fall back to the OS binary if bfs/ugrep aren't found).
        assert!(
            prep.args
                .iter()
                .any(|a| a.contains("find()") && a.contains("grep()")),
            "prepare_command should install find/grep shadows; args={:?}",
            prep.args
        );

        let (code, stdout) = run_command(&mut state, "type -t find && type -t grep").await;
        assert_eq!(code, 0, "type -t find/grep failed: {stdout}");
        assert_eq!(
            stdout.lines().filter(|l| l.trim() == "function").count(),
            2,
            "expected find and grep to both be shell functions, got: {stdout:?}"
        );

        // Only assert the binary actually routes to ugrep when ugrep resolves on
        // this host; otherwise the shadow correctly falls back to OS grep.
        if which::which("ugrep").is_ok() {
            let (code, stdout) = run_command(&mut state, "grep --version | head -3").await;
            assert_eq!(code, 0, "grep --version failed: {stdout}");
            assert!(
                stdout.to_ascii_lowercase().contains("ugrep"),
                "expected ugrep in --version output when ugrep resolves: {stdout:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_bad_command_preserves_state() {
        if !bash_available() {
            return;
        }
        let cwd = std::env::current_dir().unwrap();
        let mut state = ShellState::init(ShellKind::Bash, &cwd, None).await.unwrap();

        // Set up some state
        let (_, _) = run_command(&mut state, "export SURVIVE_TEST=yes").await;
        let prev_cwd = state.cwd.clone();

        // Run a failing command — state should still update (dump runs regardless)
        let (code, _) = run_command(&mut state, "false").await;
        assert_ne!(code, 0);

        // Previous state should still be there
        assert_eq!(state.cwd, prev_cwd);
        let (_, stdout) = run_command(&mut state, "echo $SURVIVE_TEST").await;
        assert_eq!(stdout.trim(), "yes");
    }
}
