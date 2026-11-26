//! Windows shell detection for terminal command execution.
//!
//! Default cascade: pwsh → powershell.exe → Git Bash → powershell.exe (fallback).
//!
//! PowerShell is preferred over Git Bash because MSYS2/Git Bash performs
//! POSIX-to-Windows path translation, mangling every flag starting with `/`
//! (e.g. MSBuild `/t:Build`, cl.exe `/nologo`). This breaks native Windows
//! C++/C#/.NET builds.
//!
//! Set `ECHO_AGENT_SHELL` to override auto-detection: `pwsh`, `powershell`,
//! `bash`, or `cmd`. Result is cached for the process lifetime.

/// Detected Windows shell and how to invoke it.
#[cfg(not(unix))]
#[derive(Clone, Debug)]
pub enum WindowsShell {
    GitBash(String),
    Pwsh,
    PowerShell,
    Cmd,
}

/// Detect the best available shell on Windows.
///
/// If `ECHO_AGENT_SHELL` is set, it takes precedence over auto-detection.
/// Otherwise the cascade is: pwsh → powershell.exe → Git Bash → cmd.exe.
///
/// Result is cached for the process lifetime.
#[cfg(not(unix))]
pub fn detect_windows_shell() -> &'static WindowsShell {
    use std::sync::OnceLock;
    static CACHED: OnceLock<WindowsShell> = OnceLock::new();

    CACHED.get_or_init(|| {
        // Explicit override via ECHO_AGENT_SHELL.
        if let Ok(val) = std::env::var("ECHO_AGENT_SHELL") {
            match val.trim().to_ascii_lowercase().as_str() {
                "pwsh" => {
                    tracing::info!("Windows shell (ECHO_AGENT_SHELL override): pwsh");
                    return WindowsShell::Pwsh;
                }
                "powershell" => {
                    tracing::info!("Windows shell (ECHO_AGENT_SHELL override): powershell.exe");
                    return WindowsShell::PowerShell;
                }
                "bash" | "gitbash" | "git-bash" => {
                    if let Some(path) = find_git_bash() {
                        tracing::info!(
                            shell = path,
                            "Windows shell (ECHO_AGENT_SHELL override): Git Bash"
                        );
                        return WindowsShell::GitBash(path);
                    }
                    tracing::warn!(
                        "ECHO_AGENT_SHELL={val} but Git Bash not found; falling through to auto-detect"
                    );
                }
                "cmd" | "cmd.exe" => {
                    tracing::info!("Windows shell (ECHO_AGENT_SHELL override): cmd.exe");
                    return WindowsShell::Cmd;
                }
                other => {
                    tracing::warn!(
                        "ECHO_AGENT_SHELL={other} is not recognized \
                         (expected pwsh|powershell|bash|cmd); falling through to auto-detect"
                    );
                }
            }
        }

        // Auto-detect: prefer PowerShell over Git Bash. PowerShell
        // passes `/flag` arguments through unchanged, which is required
        // for native Windows toolchains (MSBuild, cl.exe, dotnet).

        // pwsh (PowerShell 7+).
        if let Ok(output) = {
            let mut cmd = std::process::Command::new("where");
            echo_agent_tty_utils::detach_std_command(&mut cmd);
            cmd.arg("pwsh.exe").stdin(std::process::Stdio::null());
            cmd.output()
        } {
            if output.status.success() {
                tracing::info!("Windows shell: pwsh");
                return WindowsShell::Pwsh;
            }
        }

        // powershell.exe (Windows PowerShell 5.1).
        if std::path::Path::new("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
            .exists()
        {
            tracing::info!("Windows shell: powershell.exe");
            return WindowsShell::PowerShell;
        }

        // Git Bash: available but not preferred (MSYS2 path translation
        // breaks `/flag` arguments for native toolchains).
        if let Some(path) = find_git_bash() {
            tracing::info!(shell = path, "Windows shell: Git Bash");
            return WindowsShell::GitBash(path);
        }

        tracing::info!("Windows shell: powershell.exe (fallback)");
        WindowsShell::PowerShell
    })
}

/// Locate Git Bash on disk. Checks common install paths, then falls back
/// to `where bash.exe` (filtering for Git paths to avoid WSL bash).
#[cfg(not(unix))]
fn find_git_bash() -> Option<String> {
    let candidates = [
        std::env::var("PROGRAMFILES")
            .map(|pf| format!("{pf}\\Git\\bin\\bash.exe"))
            .unwrap_or_default(),
        std::env::var("PROGRAMFILES(X86)")
            .map(|pf| format!("{pf}\\Git\\bin\\bash.exe"))
            .unwrap_or_default(),
        std::env::var("LOCALAPPDATA")
            .map(|la| format!("{la}\\Programs\\Git\\bin\\bash.exe"))
            .unwrap_or_default(),
    ];
    for candidate in &candidates {
        if !candidate.is_empty() && std::path::Path::new(candidate).exists() {
            return Some(candidate.clone());
        }
    }
    // Fall back to PATH; prefer Git Bash over WSL bash.
    if let Ok(output) = {
        let mut cmd = std::process::Command::new("where");
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        cmd.arg("bash.exe").stdin(std::process::Stdio::null());
        cmd.output()
    } {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let line = line.trim();
                if line.to_ascii_lowercase().contains("git") {
                    return Some(line.to_string());
                }
            }
        }
    }
    None
}

#[cfg(not(unix))]
impl WindowsShell {
    /// Short display name for user-facing contexts (e.g. "bash", "pwsh").
    pub fn name(&self) -> &'static str {
        match self {
            Self::GitBash(_) => "bash",
            Self::Pwsh => "pwsh",
            Self::PowerShell => "powershell",
            Self::Cmd => "cmd.exe",
        }
    }

    /// Whether this shell supports the `&&` pipeline chain operator for
    /// error-propagating command chaining.
    ///
    /// - `pwsh` (PowerShell 7+): `&&` added in PS 7.0.
    /// - Git Bash: standard bash `&&`.
    /// - `powershell.exe` (5.1): no `&&` support; use `;`.
    /// - `cmd.exe`: `&&` works but is inconsistent with the `-Command`
    ///   invocation style used elsewhere; use `;` for uniformity.
    pub fn supports_chain_operator(&self) -> bool {
        matches!(self, Self::Pwsh | Self::GitBash(_))
    }

    /// Whether `grep`, `head`, `tail`, `sed`, `awk`, `find` are usable
    /// from this shell. True for Git Bash (MSYS2 bundles them inside the
    /// bash subprocess); false for PowerShell and `cmd.exe`.
    pub fn has_unix_utilities(&self) -> bool {
        matches!(self, Self::GitBash(_))
    }

    /// How this shell interprets a bare `&` token. Drives the `run_terminal_cmd`
    /// background-operator validation, which must differ per shell.
    pub fn ampersand_semantics(&self) -> AmpersandSemantics {
        match self {
            Self::GitBash(_) => AmpersandSemantics::PosixBackground,
            Self::Pwsh => AmpersandSemantics::PowerShellCore,
            Self::PowerShell => AmpersandSemantics::WindowsPowerShell,
            Self::Cmd => AmpersandSemantics::CmdSeparator,
        }
    }
}

/// Returns the appropriate command chaining separator for the current
/// platform and detected shell.
///
/// - Unix: always `"&&"` (bash/zsh).
/// - Windows with pwsh or Git Bash: `"&&"` (both support pipeline chain
///   operators).
/// - Windows with powershell.exe (5.1) or cmd.exe: `";"`.
pub fn chain_separator() -> &'static str {
    #[cfg(unix)]
    {
        "&&"
    }
    #[cfg(not(unix))]
    {
        if detect_windows_shell().supports_chain_operator() {
            "&&"
        } else {
            ";"
        }
    }
}

/// Whether `grep`, `head`, `tail`, `sed`, `awk`, `find` are usable from
/// the active shell. True on Unix and Windows + Git Bash; false on
/// Windows + PowerShell or `cmd.exe`.
///
/// Tool descriptions branch on this to swap Unix-centric guidance for
/// shell-aware guidance and avoid `'grep' is not recognized` failures.
pub fn has_unix_utilities() -> bool {
    #[cfg(unix)]
    {
        true
    }
    #[cfg(not(unix))]
    {
        detect_windows_shell().has_unix_utilities()
    }
}

/// Whether `name` resolves to an executable on the current `$PATH`.
///
/// Used by the truncated-MCP steer to name only tools that are actually present
/// on the tool server's `$PATH` (no "if available" hedge). `which` handles the
/// platform details (PATHEXT and App Execution Aliases on Windows).
///
/// Probes the base environment (tool server is co-located with the shell tool
/// in production). Per-session `export PATH` mutations inside the persistent
/// shell are not reflected (uncommon for `jq`/`python`/`sed`/`cut`).
pub fn is_command_available(name: &str) -> bool {
    which::which(name).is_ok()
}

/// How a shell interprets a bare `&` token. Drives `run_terminal_cmd`
/// background-operator detection and remediation, which must differ per shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmpersandSemantics {
    /// Bash/POSIX: a bare `&` backgrounds the command (Unix shells, Git Bash).
    PosixBackground,
    /// PowerShell 7+ (`pwsh`): a *leading* `&` is the call/invocation operator;
    /// a *trailing* `&` starts a background job.
    PowerShellCore,
    /// Windows PowerShell 5.1 (`powershell.exe`): a *leading* `&` is the call
    /// operator; a *trailing* `&` is a parse error.
    WindowsPowerShell,
    /// `cmd.exe`: `&` is an unconditional sequential command separator.
    CmdSeparator,
}

/// How the active shell interprets a bare `&`. Unix shells are always
/// [`AmpersandSemantics::PosixBackground`]; on Windows it depends on the
/// detected shell (Git Bash vs. PowerShell vs. `cmd.exe`).
pub fn ampersand_semantics() -> AmpersandSemantics {
    #[cfg(unix)]
    {
        AmpersandSemantics::PosixBackground
    }
    #[cfg(not(unix))]
    {
        detect_windows_shell().ampersand_semantics()
    }
}

/// How to invoke a command in the detected Windows shell.
#[cfg(not(unix))]
pub struct ShellInvocation {
    pub program: String,
    pub args: Vec<String>,
    /// Env vars that must be set on the child process (e.g. `MSYS_NO_PATHCONV`
    /// for Git Bash to prevent POSIX-to-Windows path translation of `/flags`).
    pub env: Vec<(&'static str, &'static str)>,
}

/// Build `(program, args, env)` for running `command` in the detected shell.
#[cfg(not(unix))]
pub fn shell_command_argv(command: &str) -> ShellInvocation {
    invocation_for(detect_windows_shell(), command)
}

/// Pure builder split out of `shell_command_argv` so tests can exercise every
/// `WindowsShell` variant, not just the one installed on the test host.
#[cfg(not(unix))]
fn invocation_for(shell: &WindowsShell, command: &str) -> ShellInvocation {
    // Force UTF-8 for descendant tools. Windows' legacy ANSI codepage (cp1252)
    // makes locale-sensitive children mis-decode UTF-8 subprocess output — e.g.
    // Python's text-mode `subprocess` raised `UnicodeDecodeError` on `gh` output.
    // `PYTHONUTF8=1` is the fix (forces `locale.getpreferredencoding` to utf-8);
    // `PYTHONIOENCODING` covers the interpreter's own stdio, `surrogateescape`
    // matching UTF-8 Mode's leniency. Applied before the per-request env, so an
    // explicit caller value still overrides these defaults.
    let utf8_env = [
        ("PYTHONUTF8", "1"),
        ("PYTHONIOENCODING", "utf-8:surrogateescape"),
    ];
    match shell {
        WindowsShell::GitBash(path) => ShellInvocation {
            program: path.clone(),
            args: vec!["-c".to_string(), command.to_string()],
            // Disable MSYS2 POSIX-to-Windows path translation so `/flag`
            // arguments (MSBuild /t:, cl.exe /nologo, etc.) pass through.
            env: vec![
                ("MSYS_NO_PATHCONV", "1"),
                ("MSYS2_ARG_CONV_EXCL", "*"),
                utf8_env[0],
                utf8_env[1],
            ],
        },
        WindowsShell::Pwsh => ShellInvocation {
            program: "pwsh".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                command.to_string(),
            ],
            env: utf8_env.to_vec(),
        },
        WindowsShell::PowerShell => ShellInvocation {
            program: "powershell.exe".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                command.to_string(),
            ],
            env: utf8_env.to_vec(),
        },
        WindowsShell::Cmd => ShellInvocation {
            program: "cmd".to_string(),
            args: vec!["/C".to_string(), command.to_string()],
            env: utf8_env.to_vec(),
        },
    }
}

// =============================================================================
// Unix shell resolution
// =============================================================================
//
// Locates an absolute path to a bash/zsh binary on Unix:
//
//   1. `$ECHO_AGENT_SHELL` override, if it names the requested kind and is runnable.
//   2. `$SHELL`, if it names the requested kind and is runnable.
//      Covers most NixOS / Homebrew / `nix-darwin` setups where the user's
//      login shell already lives at the resolved path (e.g.
//      `/run/current-system/sw/bin/bash`, `/opt/homebrew/bin/bash`).
//   3. `which::which(name)` — walks `$PATH`. Catches NixOS profile shells in
//      `/nix/store/...` or `/etc/profiles/per-user/<u>/bin/` when `/bin/bash`
//      is absent.
//   4. A fixed candidate list: `{/bin, /usr/bin, /usr/local/bin,
//      /opt/homebrew/bin} × {bash,zsh}`.
//   5. Hardcoded `/bin/<name>` — historical behavior, only reached when every
//      earlier step has failed.
//
// The result is cached per kind in a process-wide `OnceLock`, so the cascade
// is run at most once per shell kind per process.

/// Which Unix shell we're asking about. Bash and zsh are the only kinds
/// supported by the persistent shell-state backend (the dump scripts are
/// bash/zsh-specific). Fish / dash / ksh users fall through to bash.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnixShellKind {
    Bash,
    Zsh,
}

#[cfg(unix)]
impl UnixShellKind {
    /// Binary file name (`"bash"` / `"zsh"`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    /// Hardcoded historical default. Only used as the last-resort fallback.
    fn hardcoded_default(self) -> &'static str {
        match self {
            Self::Bash => "/bin/bash",
            Self::Zsh => "/bin/zsh",
        }
    }
}

/// Detect the user's preferred Unix shell kind from `$SHELL`. Defaults to
/// `Bash` when `$SHELL` is unset or unrecognized. Cheap; not cached.
#[cfg(unix)]
pub fn detect_unix_shell_kind() -> UnixShellKind {
    match std::env::var("SHELL") {
        Ok(s) if s.contains("zsh") => UnixShellKind::Zsh,
        _ => UnixShellKind::Bash,
    }
}

/// Absolute path to the requested Unix shell binary, computed via the
/// cascade above. Cached for the process lifetime.
#[cfg(unix)]
pub fn unix_shell_path(kind: UnixShellKind) -> &'static str {
    use std::sync::OnceLock;
    static BASH: OnceLock<String> = OnceLock::new();
    static ZSH: OnceLock<String> = OnceLock::new();
    let cache = match kind {
        UnixShellKind::Bash => &BASH,
        UnixShellKind::Zsh => &ZSH,
    };
    cache.get_or_init(|| {
        let path = resolve_unix_shell_path(kind);
        tracing::debug!(kind = ?kind, resolved = %path, "resolved Unix shell path");
        path
    })
}

#[cfg(unix)]
fn resolve_unix_shell_path(kind: UnixShellKind) -> String {
    let name = kind.name();
    let matches_kind = |p: &std::path::Path| p.file_name().and_then(|n| n.to_str()) == Some(name);

    // 1) Explicit override via $ECHO_AGENT_SHELL.
    if let Ok(s) = std::env::var("ECHO_AGENT_SHELL") {
        let p = std::path::PathBuf::from(&s);
        if matches_kind(&p) && is_executable(&p) {
            return s;
        }
    }

    // 2) $SHELL, when it matches the requested kind.
    if let Ok(s) = std::env::var("SHELL") {
        let p = std::path::PathBuf::from(&s);
        if matches_kind(&p) && is_executable(&p) {
            return s;
        }
    }

    // 3) `which` walks $PATH (handles NixOS, Homebrew, custom profiles).
    if let Ok(p) = which::which(name)
        && is_executable(&p)
    {
        return p.to_string_lossy().into_owned();
    }

    // 4) Common install dirs.
    for dir in ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        let p = std::path::PathBuf::from(dir).join(name);
        if is_executable(&p) {
            return p.to_string_lossy().into_owned();
        }
    }

    // 5) Hardcoded fallback — same as historical behavior. Spawn will fail at
    //    runtime on a pure NixOS host with no bash, but that's no worse than
    //    before this resolver existed.
    kind.hardcoded_default().to_string()
}

/// Whether `path` is an executable file.
///
/// First tries the file's mode bits (any-x). If that's inconclusive, falls
/// back to actually invoking `<path> --version`. The `--version` fallback
/// exists for Nix and other environments where the `X_OK` mode-bit check can
/// be misleading: some Nix overlay filesystems expose binaries whose
/// owner/group/world mode bits don't reflect their real executability.
///
/// The probe is spawned via `echo_agent_tty_utils::detach_std_command` so that
/// the child does NOT inherit the parent's controlling TTY. The resolver
/// runs lazily inside `unix_shell_path`'s `OnceLock::get_or_init` which
/// can fire during interactive TUI/pager startup; without detach, a
/// misbehaving shell binary that emits mouse-tracking escapes or asks
/// for a controlling tty during `--version` would spew garbage onto the
/// pager screen. `stdin`, `stdout`, and `stderr` are pinned to `null`
/// to drop any output the binary does emit. See `codegen-conventions`
/// SKILL.md for the workspace-wide subprocess rule.
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.is_file()
        && meta.permissions().mode() & 0o111 != 0
    {
        return true;
    }

    // Nix fallback. Detach from the controlling TTY via echo_agent_tty_utils so
    // the probe (which the resolver may run during interactive TUI/pager
    // startup) cannot leak escapes onto the parent's terminal.
    let mut cmd = std::process::Command::new(path);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn has_unix_utilities_is_true_on_unix() {
        assert!(has_unix_utilities());
    }

    #[cfg(unix)]
    #[test]
    fn chain_separator_is_ampersand_on_unix() {
        assert_eq!(chain_separator(), "&&");
    }

    #[test]
    fn is_command_available_detects_present_and_absent() {
        // A shell present on every host of this OS resolves; a bogus name never
        // does. `cmd` resolves via PATHEXT on Windows, `sh` lives on $PATH on Unix.
        #[cfg(windows)]
        let present = "cmd";
        #[cfg(not(windows))]
        let present = "sh";
        assert!(is_command_available(present));
        assert!(!is_command_available(
            "echo-agent-definitely-not-a-real-command-xyz"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn ampersand_semantics_is_posix_background_on_unix() {
        assert_eq!(ampersand_semantics(), AmpersandSemantics::PosixBackground);
    }

    #[cfg(unix)]
    #[test]
    fn unix_shell_path_returns_a_bash() {
        // Whatever it returns, it must end in "bash" (the resolver guarantees
        // the result's file_name matches the requested kind, even for the
        // hardcoded `/bin/bash` fallback).
        let p = unix_shell_path(UnixShellKind::Bash);
        assert!(
            std::path::Path::new(p).file_name().and_then(|n| n.to_str()) == Some("bash"),
            "expected a path ending in 'bash', got {p}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_shell_path_is_cached() {
        // Two calls return the same `&'static str` (pointer equality).
        let a = unix_shell_path(UnixShellKind::Bash);
        let b = unix_shell_path(UnixShellKind::Bash);
        assert!(
            std::ptr::eq(a.as_ptr(), b.as_ptr()),
            "result should be cached"
        );
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_recognizes_bin_sh() {
        // /bin/sh is the one path POSIX promises across every Unix variant
        // we care about; on macOS and Linux distros it's always executable.
        // (Pure NixOS images may lack it, in which case this test is
        // skipped — same approach as the existing `/bin/bash` gated tests.)
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        assert!(is_executable(std::path::Path::new("/bin/sh")));
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_rejects_non_executable() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Mode bits explicitly cleared — not executable.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(tmp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn detect_unix_shell_kind_falls_back_to_bash() {
        // We can't safely mutate $SHELL in a multithreaded test runner, so just
        // sanity-check the function returns *something* and doesn't panic.
        let _ = detect_unix_shell_kind();
    }

    /// Only Git Bash bundles the Unix utilities; the PowerShell and cmd
    /// variants do not.
    #[cfg(not(unix))]
    #[test]
    fn has_unix_utilities_only_true_for_gitbash() {
        assert!(
            WindowsShell::GitBash("C:\\Program Files\\Git\\bin\\bash.exe".into())
                .has_unix_utilities()
        );
        assert!(!WindowsShell::Pwsh.has_unix_utilities());
        assert!(!WindowsShell::PowerShell.has_unix_utilities());
        assert!(!WindowsShell::Cmd.has_unix_utilities());
    }

    /// Git Bash backgrounds with a bare `&`; PowerShell uses `&` as the call
    /// operator; `cmd.exe` uses it as a sequential separator.
    #[cfg(not(unix))]
    #[test]
    fn ampersand_semantics_per_windows_shell() {
        assert_eq!(
            WindowsShell::GitBash("C:\\Program Files\\Git\\bin\\bash.exe".into())
                .ampersand_semantics(),
            AmpersandSemantics::PosixBackground
        );
        assert_eq!(
            WindowsShell::Pwsh.ampersand_semantics(),
            AmpersandSemantics::PowerShellCore
        );
        assert_eq!(
            WindowsShell::PowerShell.ampersand_semantics(),
            AmpersandSemantics::WindowsPowerShell
        );
        assert_eq!(
            WindowsShell::Cmd.ampersand_semantics(),
            AmpersandSemantics::CmdSeparator
        );
    }

    /// Every Windows shell variant injects the UTF-8 env defaults. Builds all
    /// four variants directly so it doesn't depend on the test host's shell.
    #[cfg(not(unix))]
    #[test]
    fn invocation_for_sets_utf8_env_on_every_variant() {
        let variants = [
            WindowsShell::GitBash("C:\\Program Files\\Git\\bin\\bash.exe".into()),
            WindowsShell::Pwsh,
            WindowsShell::PowerShell,
            WindowsShell::Cmd,
        ];
        for shell in &variants {
            let inv = invocation_for(shell, "echo hi");
            assert!(
                inv.env.contains(&("PYTHONUTF8", "1")),
                "expected PYTHONUTF8=1 in env for {shell:?}, got {:?}",
                inv.env
            );
            assert!(
                inv.env
                    .contains(&("PYTHONIOENCODING", "utf-8:surrogateescape")),
                "expected PYTHONIOENCODING=utf-8:surrogateescape in env for {shell:?}, got {:?}",
                inv.env
            );
        }
    }

    /// GitBash keeps its pre-existing MSYS2 path-translation guards in addition
    /// to the UTF-8 defaults (the UTF-8 entries are appended, not replacing).
    #[cfg(not(unix))]
    #[test]
    fn invocation_for_gitbash_keeps_msys_vars() {
        let inv = invocation_for(
            &WindowsShell::GitBash("C:\\Program Files\\Git\\bin\\bash.exe".into()),
            "echo hi",
        );
        assert!(
            inv.env.contains(&("MSYS_NO_PATHCONV", "1")),
            "{:?}",
            inv.env
        );
        assert!(
            inv.env.contains(&("MSYS2_ARG_CONV_EXCL", "*")),
            "{:?}",
            inv.env
        );
    }
}
