//! Shadow `find`→`bfs` and `grep`→`ugrep` when those binaries resolve.
//!
//! Per-tool enable state (default on) is resolved by the host via the shared
//! config helper `echo-agent-runtime::util::config::resolve_search_tools_enabled`
//! (requirements > env `ECHO_AGENT_TOOLS_FIND_BFS` / `ECHO_AGENT_TOOLS_GREP_UGREP` (+
//! `ECHO_AGENT_FIND_BFS` / `ECHO_AGENT_GREP_UGREP` aliases, `DISABLE_EMBEDDED_SEARCH_TOOLS`
//! master) > `[toolset.bash]` config.toml > managed > default), baked into the
//! `LocalTerminalBackend` as a [`SearchShadowConfig`] and passed to
//! [`search_injection`] per command. The enable state lives on the backend (not
//! a process-global): a subagent that reuses the parent's backend inherits the
//! parent's shadows instead of clobbering a shared static. This module no longer
//! parses the flags itself.
//!
//! Resolve (host side, memoized): env override if a regular file → bundled binary
//! (release builds, self-extracted to `~/.echo-agent/vendor/<name>-<ver>-<target>`) →
//! `~/.echo-agent/vendor/{name}` if a regular file → `which` on the agent `$PATH`.
//! Env/vendor only require `is_file()` as a lenient hint (no `--version` probe).
//! This memoized path is only a *hint*: the injected shadow re-resolves at
//! **call time** — it uses the hint when it's still *executable* (`[ -x ]`), else
//! `command -v {bin}` on the live shell `PATH` (which includes login/rc additions
//! the agent process may lack), else falls back to the OS `{name}`. So a removed
//! or non-executable binary self-heals to OS `find`/`grep`, and a binary
//! reachable only through the login shell is still found.
//!
//! Inject is **always** non-empty on Unix callers: either install a shadow
//! function (which tags itself with a `__echo_agent_shadow_{name}` marker) or a
//! marker-gated `unalias`+`unset -f` that drops *only* a prior harness shadow —
//! never a user-defined `find`/`grep` function replayed from the snapshot.

use super::SearchShadowConfig;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// BRE + ignore-files; recursive flags are no-ops on stdin (computer parity).
const UGREP_DEFAULT_ARGS: &[&str] = &[
    "-G",
    "--ignore-files",
    "--hidden",
    "-I",
    "--exclude-dir=.git",
    "--exclude-dir=.svn",
    "--exclude-dir=.hg",
    "--exclude-dir=.bzr",
    "--exclude-dir=.jj",
    "--exclude-dir=.sl",
];

// Binaries embedded by build.rs when `ECHO_AGENT_TOOLS_BUNDLE_{BFS,UGREP}_PATH` is set
// (release pipeline). Self-extracted to `~/.echo-agent/vendor` on first use, mirroring
// the ripgrep bundling in `echo_agent_build::grep::ripgrep`.
#[cfg(bundle_bfs)]
const BFS_BYTES: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/bundle-bfs/bfs-",
    env!("ECHO_AGENT_TOOLS_BFS_VER"),
    "-",
    env!("ECHO_AGENT_TOOLS_BFS_TARGET"),
    ".bin"
));

#[cfg(bundle_ugrep)]
const UGREP_BYTES: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/bundle-ugrep/ugrep-",
    env!("ECHO_AGENT_TOOLS_UGREP_VER"),
    "-",
    env!("ECHO_AGENT_TOOLS_UGREP_TARGET"),
    ".bin"
));

/// Oneline inject for shell wrappers; always ends with `"; "`.
///
/// `cfg` is the backend's resolved per-tool enable state (see module docs); it
/// is passed in per command rather than read from a process-global so subagents
/// sharing a backend can't clobber each other's shadows.
pub fn search_injection(cfg: SearchShadowConfig) -> String {
    build_injection(cfg.find_bfs, cfg.grep_ugrep, resolved_tools())
}

/// Compose the inject from per-tool enable flags + resolved binaries. An enabled
/// tool installs a self-resolving shadow (the memoized `tools` path is only a
/// fast-path hint; the shadow re-resolves at call time and falls back to the OS
/// binary — see [`shell_function`]). A disabled tool emits a marker-gated
/// `restore` that drops only a prior harness shadow. Kept pure (flags/tools
/// passed in) so tests need no process-global env mutation — that is UB against
/// the `shell_state` integration tests that read env / spawn children
/// concurrently.
fn build_injection(find_on: bool, grep_on: bool, tools: &ResolvedTools) -> String {
    let find = if find_on {
        shell_function("find", "bfs", tools.bfs.as_deref(), &[])
    } else {
        restore_command("find")
    };
    let grep = if grep_on {
        shell_function("grep", "ugrep", tools.ugrep.as_deref(), UGREP_DEFAULT_ARGS)
    } else {
        restore_command("grep")
    };
    format!("{find}; {grep}; ")
}

/// Drop a *previously installed harness* shadow so command-word `{name}` uses the
/// OS binary again. Gated on the `__echo_agent_shadow_{name}` marker that
/// [`shell_function`] sets, so a user-defined `{name}` function replayed from the
/// shell snapshot is left intact — only the harness's own shadow is removed.
/// `set -u`/`set -e` safe and idempotent (`unset -f` is bash + zsh).
fn restore_command(name: &str) -> String {
    format!(
        "if [ -n \"${{__echo_agent_shadow_{name}-}}\" ]; then \
           unalias {name} 2>/dev/null || true; \
           unset -f {name} 2>/dev/null || true; \
           unset __echo_agent_shadow_{name} 2>/dev/null || true; \
         fi"
    )
}

struct ResolvedTools {
    bfs: Option<PathBuf>,
    ugrep: Option<PathBuf>,
}

fn resolved_tools() -> &'static ResolvedTools {
    static TOOLS: OnceLock<ResolvedTools> = OnceLock::new();
    TOOLS.get_or_init(|| ResolvedTools {
        bfs: resolve_tool("bfs", "ECHO_AGENT_TOOLS_BFS_PATH", bundled_bfs()),
        ugrep: resolve_tool("ugrep", "ECHO_AGENT_TOOLS_UGREP_PATH", bundled_ugrep()),
    })
}

/// Write embedded `bytes` to `~/.echo-agent/vendor/<versioned_name>` (chmod 755) on
/// first use and return the path; reused on later runs. Versioned so bumping the
/// bundled version writes a fresh file instead of reusing a stale one.
#[cfg(any(bundle_bfs, bundle_ugrep))]
fn extract_bundled(versioned_name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::util::echo_agent_home().join("vendor");
    let dest = dir.join(versioned_name);
    if !dest.exists() {
        std::fs::create_dir_all(&dir)?;
        // Write to a unique temp then atomically rename, so a concurrent first
        // use (or an interrupted write) can't leave a half-written binary that
        // gets cached and exec'd.
        let tmp = dir.join(format!(
            "{versioned_name}.tmp.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&tmp, bytes)?;
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)?;
        // Rename is atomic on the same filesystem. If another process won the
        // race, `dest` already exists and is correct — drop our temp copy.
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            let _ = std::fs::remove_file(&tmp);
            if !dest.exists() {
                return Err(e);
            }
        }
    }
    Ok(dest)
}

/// Path to the bundled `bfs` (extracted on first use), or `None` when not bundled.
fn bundled_bfs() -> Option<PathBuf> {
    #[cfg(bundle_bfs)]
    {
        extract_bundled(
            concat!(
                "bfs-",
                env!("ECHO_AGENT_TOOLS_BFS_VER"),
                "-",
                env!("ECHO_AGENT_TOOLS_BFS_TARGET")
            ),
            BFS_BYTES,
        )
        .ok()
    }
    #[cfg(not(bundle_bfs))]
    {
        None
    }
}

/// Path to the bundled `ugrep` (extracted on first use), or `None` when not bundled.
fn bundled_ugrep() -> Option<PathBuf> {
    #[cfg(bundle_ugrep)]
    {
        extract_bundled(
            concat!(
                "ugrep-",
                env!("ECHO_AGENT_TOOLS_UGREP_VER"),
                "-",
                env!("ECHO_AGENT_TOOLS_UGREP_TARGET")
            ),
            UGREP_BYTES,
        )
        .ok()
    }
    #[cfg(not(bundle_ugrep))]
    {
        None
    }
}

fn resolve_tool(bin_name: &str, env_override: &str, bundled: Option<PathBuf>) -> Option<PathBuf> {
    resolve_tool_from(
        std::env::var_os(env_override).map(PathBuf::from),
        bundled,
        crate::util::echo_agent_home().join("vendor").join(bin_name),
        bin_name,
    )
}

/// Resolution order: explicit env path → bundled (self-extracted) →
/// `~/.echo-agent/vendor/<bin>` → `which`. Env and vendor only require `is_file()` here
/// (a lenient hint, no `+x` probe) so an odd-permission copy still resolves; the
/// injected shadow gates on `[ -x ]` at call time and falls back to the OS binary
/// if the hint isn't executable, so a non-exec path can't hard-fail `find`/`grep`.
fn resolve_tool_from(
    env_path: Option<PathBuf>,
    bundled: Option<PathBuf>,
    vendor: PathBuf,
    bin_name: &str,
) -> Option<PathBuf> {
    if let Some(path) = env_path
        && path.is_file()
    {
        return Some(path);
    }
    if let Some(path) = bundled {
        return Some(path);
    }
    if vendor.is_file() {
        return Some(vendor);
    }
    which::which(bin_name).ok()
}

fn bash_safe_quote(s: &str) -> String {
    if s.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '=' | ':' | '@' | '+' | '-')
    }) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Oneline `name() { … }` for `-c` inject — a *self-resolving* shadow.
///
/// At call time it picks the binary: the host-resolved `preferred` path
/// (bundled/env/vendor/which) when that file still exists, else `command -v
/// {bin_name}` on the live shell `PATH` (which carries login/rc additions the
/// agent process may not have), else it falls back to the OS `{name}`. This keeps
/// the fast hard-coded path for the common case while self-healing when the
/// binary was removed (revalidation) or is only reachable through the shell's
/// richer `PATH`.
///
/// `exec -a` runs inside a subshell so a top-level call can't replace the wrapper
/// shell (it must survive to dump state); a call already inside a subshell
/// (`BASH_SUBSHELL > 0`, bash) execs directly to skip a fork. `${ZSH_VERSION-}`
/// keeps the probe `set -u`-safe (a bare `$ZSH_VERSION` aborts bash under
/// nounset). `exec -a` gives the binary the `find`/`grep` argv0 (ps display +
/// ugrep grep-personality) in both bash and zsh. The trailing
/// `__echo_agent_shadow_{name}=1` marks this as a harness shadow so `restore_command`
/// only ever removes our own function — never a user's.
fn shell_function(
    name: &str,
    bin_name: &str,
    preferred: Option<&Path>,
    prepend_args: &[&str],
) -> String {
    let qpref = preferred
        .map(|p| bash_safe_quote(&p.to_string_lossy()))
        .unwrap_or_else(|| "''".to_string());
    let prepend = {
        let qargs: Vec<String> = prepend_args.iter().map(|a| bash_safe_quote(a)).collect();
        if qargs.is_empty() {
            String::new()
        } else {
            format!("{} ", qargs.join(" "))
        }
    };
    // `local __echo_agent_bin` is re-resolved every call. The host hint is trusted
    // only when it's *executable* (`[ -x ]`, not just `[ -f ]`): the resolver
    // accepts any regular file as a hint, but `exec` needs `+x`, so a non-exec
    // hint must fall through rather than hard-fail with no OS fallback. Then
    // `command -v` on the live shell PATH (returns an executable), else the OS
    // binary. `|| __echo_agent_bin=''` keeps the lookup `set -e`-safe (a failed
    // `command -v` would otherwise abort the function under errexit). The OS
    // fallback uses `command {name}` to bypass this function. `{prepend}` is
    // empty for find, the ugrep default flags for grep (and is omitted from the
    // OS fallback, which gets the original args).
    format!(
        "unalias {name} 2>/dev/null || true; \
         {name}() {{ \
           local __echo_agent_bin={qpref}; \
           [ -x \"$__echo_agent_bin\" ] || __echo_agent_bin=$(command -v {bin_name} 2>/dev/null) || __echo_agent_bin=''; \
           if [ -z \"$__echo_agent_bin\" ]; then command {name} \"$@\"; return; fi; \
           if [[ -z ${{ZSH_VERSION-}} ]] && (( BASH_SUBSHELL > 0 )); then \
             exec -a {name} \"$__echo_agent_bin\" {prepend}\"$@\"; \
           else \
             (exec -a {name} \"$__echo_agent_bin\" {prepend}\"$@\"); \
           fi; \
         }}; \
         __echo_agent_shadow_{name}=1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both binaries resolved, for `build_injection` shape tests.
    fn both_tools() -> ResolvedTools {
        ResolvedTools {
            bfs: Some(PathBuf::from("/tmp/bfs")),
            ugrep: Some(PathBuf::from("/tmp/ugrep")),
        }
    }

    #[test]
    fn shell_function_shape() {
        let fn_body = shell_function("find", "bfs", Some(Path::new("/tmp/bfs")), &[]);
        assert!(fn_body.contains("unalias find"));
        // Preferred path is the fast-path hint; the shadow execs `$__echo_agent_bin`.
        assert!(fn_body.contains("local __echo_agent_bin=/tmp/bfs"));
        assert!(fn_body.contains("exec -a find \"$__echo_agent_bin\" \"$@\""));
        // Hint is trusted only when executable (`[ -x ]`, not `[ -f ]`), so a
        // non-exec hint falls through instead of hard-failing exec.
        assert!(fn_body.contains("[ -x \"$__echo_agent_bin\" ]"));
        assert!(!fn_body.contains("[ -f \"$__echo_agent_bin\" ]"));
        // Self-heal: live-PATH lookup + OS fallback.
        assert!(fn_body.contains("command -v bfs"));
        assert!(fn_body.contains("command find \"$@\""));
        assert!(fn_body.contains("BASH_SUBSHELL > 0"));
        assert!(fn_body.contains("(exec -a find"));
        // Marker so `restore_command` only removes our own shadow.
        assert!(fn_body.contains("__echo_agent_shadow_find=1"));
        // set -u-safe zsh probe (a bare $ZSH_VERSION aborts bash under nounset).
        assert!(fn_body.contains("${ZSH_VERSION-}"));
        assert!(!fn_body.contains("[[ -n $ZSH_VERSION ]]"));
    }

    #[test]
    fn shell_function_unresolved_uses_empty_hint() {
        // No host-resolved path → empty hint, relies on live-PATH `command -v`.
        let fn_body = shell_function("find", "bfs", None, &[]);
        assert!(fn_body.contains("local __echo_agent_bin=''"));
        assert!(fn_body.contains("command -v bfs"));
        assert!(fn_body.contains("command find \"$@\""));
    }

    #[test]
    fn grep_prepends_ugrep_defaults() {
        let fn_body = shell_function(
            "grep",
            "ugrep",
            Some(Path::new("/tmp/ugrep")),
            UGREP_DEFAULT_ARGS,
        );
        assert!(fn_body.contains("local __echo_agent_bin=/tmp/ugrep"));
        assert!(fn_body.contains("\"$__echo_agent_bin\" -G --ignore-files --hidden -I"));
        assert!(fn_body.contains("--exclude-dir=.git"));
        assert!(fn_body.contains("command -v ugrep"));
    }

    #[test]
    fn bash_safe_quote_escapes_metacharacters() {
        assert_eq!(bash_safe_quote("/usr/bin/bfs"), "/usr/bin/bfs");
        assert_eq!(bash_safe_quote("/tmp/my bfs"), "'/tmp/my bfs'");
        let body = shell_function("find", "bfs", Some(Path::new("/tmp/evil$(id)")), &[]);
        assert!(
            body.contains("local __echo_agent_bin='/tmp/evil$(id)'"),
            "{body}"
        );
        assert!(!body.contains("=/tmp/evil$(id)"));
    }

    #[test]
    fn restore_command_is_marker_gated() {
        let r = restore_command("find");
        // Only removes the harness shadow when our marker is set.
        assert!(r.contains("if [ -n \"${__echo_agent_shadow_find-}\" ]"));
        assert!(r.contains("unalias find"));
        assert!(r.contains("unset -f find"));
        assert!(r.contains("unset __echo_agent_shadow_find"));
    }

    #[test]
    fn config_default_is_both_on() {
        // Standalone/no-host backends default to shadowing both tools.
        let cfg = SearchShadowConfig::default();
        assert!(cfg.find_bfs);
        assert!(cfg.grep_ugrep);
    }

    #[test]
    fn build_injection_off_emits_marker_gated_restore_not_function() {
        // Disabled tools emit a marker-gated restore so a stale harness shadow
        // from a prior snapshot is dropped, but a user function is left intact.
        let inject = build_injection(false, false, &both_tools());
        assert!(inject.ends_with("; "));
        assert!(inject.contains("if [ -n \"${__echo_agent_shadow_find-}\" ]"));
        assert!(inject.contains("if [ -n \"${__echo_agent_shadow_grep-}\" ]"));
        assert!(inject.contains("unset -f find"));
        assert!(inject.contains("unset -f grep"));
        assert!(!inject.contains("find()"));
        assert!(!inject.contains("grep()"));
    }

    #[test]
    fn build_injection_on_shadows_resolved_tools() {
        let inject = build_injection(true, true, &both_tools());
        assert!(inject.contains("find()"));
        assert!(inject.contains("grep()"));
        assert!(inject.contains("-G --ignore-files"));
        assert!(inject.contains("__echo_agent_shadow_find=1"));
    }

    #[test]
    fn build_injection_enabled_unresolved_still_self_heals() {
        // Enabled but no host-resolved path → still install a self-resolving
        // shadow (live-PATH `command -v` + OS fallback), never a bare restore.
        let tools = ResolvedTools {
            bfs: None,
            ugrep: None,
        };
        let inject = build_injection(true, true, &tools);
        assert!(inject.contains("find()"));
        assert!(inject.contains("grep()"));
        assert!(inject.contains("command -v bfs"));
        assert!(inject.contains("command -v ugrep"));
        // OS fallback present; not a marker-gated restore.
        assert!(inject.contains("command find \"$@\""));
        assert!(!inject.contains("if [ -n \"${__echo_agent_shadow_find-}\" ]"));
    }

    #[test]
    fn injection_always_nonempty_and_trailing_sep() {
        // Structural invariants that hold regardless of host flags/binaries:
        // never empty (so a stale snapshot shadow is always overwritten) and a
        // trailing `"; "` separator before the user command.
        for cfg in [
            SearchShadowConfig {
                find_bfs: true,
                grep_ugrep: true,
            },
            SearchShadowConfig {
                find_bfs: false,
                grep_ugrep: false,
            },
        ] {
            let inject = search_injection(cfg);
            assert!(!inject.is_empty());
            assert!(inject.ends_with("; "));
            assert!(!inject.contains('\n'));
        }
    }

    #[test]
    fn env_override_accepts_regular_file_without_exec_bit() {
        let bin = std::env::temp_dir().join(format!(
            "echo-agent-bfs-noexec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&bin, b"#!/bin/sh\necho ok\n").unwrap();
        // Mode 0o644 — no execute bit; Nix-like / restricted copies.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&bin, perms).unwrap();
        }
        // Bundled + vendor intentionally absent so the env override is what wins.
        let got = resolve_tool_from(
            Some(bin.clone()),
            None,
            PathBuf::from("/nonexistent/vendor/bfs"),
            "bfs",
        );
        let _ = std::fs::remove_file(&bin);
        assert_eq!(got.as_deref(), Some(bin.as_path()));
    }

    #[test]
    fn resolve_tool_precedence() {
        // Real temp files so the is_file() checks pass.
        let dir =
            std::env::temp_dir().join(format!("echo-agent-resolve-{}-{:?}", std::process::id(), {
                use std::time::{SystemTime, UNIX_EPOCH};
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            }));
        std::fs::create_dir_all(&dir).unwrap();
        let envp = dir.join("env");
        let bundled = dir.join("bundled");
        let vendor = dir.join("vendor");
        for p in [&envp, &bundled, &vendor] {
            std::fs::write(p, b"x").unwrap();
        }

        // env override beats everything.
        assert_eq!(
            resolve_tool_from(
                Some(envp.clone()),
                Some(bundled.clone()),
                vendor.clone(),
                "bfs"
            )
            .as_deref(),
            Some(envp.as_path())
        );
        // bundled beats the manual vendor copy.
        assert_eq!(
            resolve_tool_from(None, Some(bundled.clone()), vendor.clone(), "bfs").as_deref(),
            Some(bundled.as_path())
        );
        // vendor used when neither env nor bundled is present.
        assert_eq!(
            resolve_tool_from(None, None, vendor.clone(), "bfs").as_deref(),
            Some(vendor.as_path())
        );
        // A non-file env override is ignored and falls through to vendor.
        assert_eq!(
            resolve_tool_from(Some(dir.join("missing")), None, vendor.clone(), "bfs").as_deref(),
            Some(vendor.as_path())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only compiled when the binaries are actually bundled (release pipeline, or
    /// `ECHO_AGENT_TOOLS_BUNDLE_{BFS,UGREP}_PATH` at build time). Verifies the embedded
    /// bytes self-extract under `~/.echo-agent/vendor` and the extracted `bfs` runs.
    #[cfg(all(bundle_bfs, bundle_ugrep))]
    #[test]
    fn bundled_binaries_extract_and_run() {
        let vendor = crate::util::echo_agent_home().join("vendor");
        let bfs = bundled_bfs().expect("bfs should be bundled");
        let ugrep = bundled_ugrep().expect("ugrep should be bundled");
        assert!(bfs.is_file() && bfs.starts_with(&vendor), "bfs at {bfs:?}");
        assert!(
            ugrep.is_file() && ugrep.starts_with(&vendor),
            "ugrep at {ugrep:?}"
        );
        let v = std::process::Command::new(&bfs)
            .arg("--version")
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&v.stdout)
                .to_lowercase()
                .contains("bfs"),
            "bfs --version: {}",
            String::from_utf8_lossy(&v.stdout)
        );
    }

    /// Regression: a bare `$ZSH_VERSION` probe aborted the shadow under `set -u`
    /// ("unbound variable"), killing find/grep and dropping the state dump. Runs
    /// the generated function with `/bin/echo` standing in for the binary.
    #[test]
    fn shadow_runs_under_nounset_bash() {
        let Ok(bash) = which::which("bash") else {
            return;
        };
        let inject = shell_function("find", "bfs", Some(Path::new("/bin/echo")), &[]);
        let script = format!("set -euo pipefail; {inject}; find hello world");
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "find shadow aborted under set -u: {:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello world");
    }

    /// Command substitution runs with `BASH_SUBSHELL > 0`, taking the direct
    /// `exec -a` branch; it must still forward args and succeed under `set -u`,
    /// and the outer shell must survive (so its state dump can run).
    #[test]
    fn shadow_execs_directly_in_subshell_bash() {
        let Ok(bash) = which::which("bash") else {
            return;
        };
        let inject = shell_function("find", "bfs", Some(Path::new("/bin/echo")), &[]);
        let script =
            format!("set -euo pipefail; {inject}; printf '[%s]' \"$(find sub shell)\"; echo ALIVE");
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "[sub shell]ALIVE\n");
    }

    /// zsh takes the subshell `exec -a` branch (consistent argv0 with bash); it
    /// must run under nounset without referencing the bash-only `BASH_SUBSHELL`.
    #[test]
    fn shadow_runs_under_nounset_zsh() {
        let Ok(zsh) = which::which("zsh") else {
            return;
        };
        let inject = shell_function("grep", "ugrep", Some(Path::new("/bin/echo")), &[]);
        let script = format!("setopt nounset errexit pipefail; {inject}; grep hi there");
        let out = std::process::Command::new(&zsh)
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "grep shadow aborted in zsh: {:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi there");
    }

    /// #3 regression: a user-defined `find` must survive a disabled-tool restore.
    /// Without the marker gate, the unconditional `unset -f find` dropped it.
    #[test]
    fn restore_preserves_user_function_without_marker() {
        let Ok(bash) = which::which("bash") else {
            return;
        };
        let restore = restore_command("find");
        let script =
            format!("set -euo pipefail; find() {{ echo USERFIND; }}; {restore}; find anything");
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "USERFIND");
    }

    /// The marker-gated restore *does* drop a prior harness shadow (which set the
    /// marker), so a flipped-off tool falls back to the OS binary.
    #[test]
    fn restore_removes_harness_shadow_with_marker() {
        let Ok(bash) = which::which("bash") else {
            return;
        };
        let shadow = shell_function("find", "bfs", Some(Path::new("/bin/echo")), &[]);
        let restore = restore_command("find");
        let script = format!("set -euo pipefail; {shadow}; {restore}; type -t find || echo NOFUNC");
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.lines().any(|l| l.trim() == "function"),
            "find should no longer be a function after restore: {stdout:?}"
        );
    }

    /// #1 + #4: the host hint points at a missing file, but the binary is on the
    /// live shell `PATH` — the shadow re-resolves via `command -v` at call time.
    #[test]
    fn shadow_self_heals_via_path_lookup() {
        let Ok(bash) = which::which("bash") else {
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "echo-agent-selfheal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("myfind");
        std::fs::write(&bin, "#!/bin/sh\necho SELFHEAL \"$@\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
        }
        // Hint is a nonexistent path; `myfind` is only reachable via PATH.
        let shadow = shell_function("find", "myfind", Some(Path::new("/nonexistent/bfs")), &[]);
        let script = format!(
            "set -euo pipefail; export PATH={}:\"$PATH\"; {shadow}; find X",
            bash_safe_quote(&dir.to_string_lossy())
        );
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "SELFHEAL X");
    }

    /// #1 fallback: neither the hint nor `command -v` resolves → the OS binary
    /// runs (via `command find`), so the shadow never breaks `find`.
    #[test]
    fn shadow_falls_back_to_os_when_binary_absent() {
        let Ok(bash) = which::which("bash") else {
            return;
        };
        let shadow = shell_function(
            "find",
            "echo_agent_no_such_search_bin_xyz",
            Some(Path::new("/nonexistent/bfs")),
            &[],
        );
        // `find /dev/null` prints the path on every find implementation.
        let script = format!("set -euo pipefail; {shadow}; find /dev/null");
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "/dev/null");
    }

    /// #1 regression: a host hint that exists but is **not executable** (e.g. a
    /// mode-0644 `ECHO_AGENT_TOOLS_*_PATH` / vendor copy) must fall through to the OS
    /// binary rather than hard-fail `exec` with EACCES. The `[ -x ]` guard (not
    /// `[ -f ]`) is what makes this work.
    #[test]
    fn shadow_falls_back_when_hint_not_executable() {
        let Ok(bash) = which::which("bash") else {
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "echo-agent-noexec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hint = dir.join("bfs");
        std::fs::write(&hint, "#!/bin/sh\necho SHOULD_NOT_RUN\n").unwrap();
        {
            // Mode 0644 — exists but not executable by anyone.
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&hint).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&hint, perms).unwrap();
        }
        // Hint is the non-exec file; bin_name isn't on PATH → must reach OS find.
        let shadow = shell_function(
            "find",
            "echo_agent_no_such_search_bin_xyz",
            Some(hint.as_path()),
            &[],
        );
        let script = format!("set -euo pipefail; {shadow}; find /dev/null");
        let out = std::process::Command::new(&bash)
            .args(["-c", &script])
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "non-exec hint should fall back to OS find, not fail: {:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.trim(), "/dev/null");
        assert!(
            !stdout.contains("SHOULD_NOT_RUN"),
            "non-exec hint was run: {stdout:?}"
        );
    }
}
