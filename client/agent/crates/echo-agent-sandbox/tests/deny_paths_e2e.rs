//! E2E path-deny and EchoAgent hook write-deny (subprocess; arm64-tagged).
//! Soft-skips when enforcement is unavailable; only
//! `SANDBOX_E2E_REQUIRE_ENFORCEMENT` hard-requires a usable backend.

#![cfg(all(unix, feature = "enforce"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCENARIO_ENV: &str = "SANDBOX_E2E_SCENARIO";
const WORKSPACE_ENV: &str = "SANDBOX_E2E_WORKSPACE";
const ECHO_AGENT_HOME_ENV: &str = "SANDBOX_E2E_ECHO_AGENT_HOME";
const HOME_ENV: &str = "SANDBOX_E2E_HOME";
const PROFILE_ENV: &str = "SANDBOX_E2E_PROFILE";
const TARGETS_ENV: &str = "SANDBOX_E2E_TARGETS";
const CONTROLS_ENV: &str = "SANDBOX_E2E_CONTROLS";
const POSTLAUNCH_ENV: &str = "SANDBOX_E2E_POSTLAUNCH";
const MARKER: &str = "deny-paths-e2e-marker-9f3c1a";
const REQUIRE_ENV: &str = "SANDBOX_E2E_REQUIRE_ENFORCEMENT";

fn apply_fixture_env(cmd: &mut Command, home: &Path, echo_agent_home: &Path, workspace: &Path) {
    cmd.env(WORKSPACE_ENV, workspace.as_os_str())
        .env(HOME_ENV, home.as_os_str())
        .env(ECHO_AGENT_HOME_ENV, echo_agent_home.as_os_str())
        .env("HOME", home.as_os_str())
        .env("ECHO_AGENT_HOME", echo_agent_home.as_os_str());
}

/// Re-invoke this test binary as a subprocess driving `profile` over `targets`
/// (denied) and `controls` (must stay readable). `postlaunch` paths are created
/// AFTER apply to exercise the macOS runtime-regex (post-launch) coverage.
fn run_scenario(
    home: &Path,
    echo_agent_home: &Path,
    workspace: &Path,
    profile: &str,
    targets: &[&str],
    controls: &[&str],
    postlaunch: &[&str],
) -> (std::process::ExitStatus, String) {
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(exe);
    apply_fixture_env(&mut cmd, home, echo_agent_home, workspace);
    let output = cmd
        .env(SCENARIO_ENV, "block_deny")
        .env(PROFILE_ENV, profile)
        .env(TARGETS_ENV, targets.join(","))
        .env(CONTROLS_ENV, controls.join(","))
        .env(POSTLAUNCH_ENV, postlaunch.join(","))
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .arg("subprocess_entry")
        .output()
        .expect("failed to spawn subprocess");
    (
        output.status,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Re-invoke as a subprocess for the direct-hook write-deny scenarios.
fn run_hook_write_deny_scenario(
    home: &Path,
    echo_agent_home: &Path,
    workspace: &Path,
    scenario: &str,
) -> (std::process::ExitStatus, String) {
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(exe);
    apply_fixture_env(&mut cmd, home, echo_agent_home, workspace);
    let output = cmd
        .env(SCENARIO_ENV, scenario)
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .arg("subprocess_entry")
        .output()
        .expect("failed to spawn subprocess");
    (
        output.status,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Soft-skip when the platform cannot enforce kernel denials.
/// Only `SANDBOX_E2E_REQUIRE_ENFORCEMENT` hard-requires enforcement; generic
/// CI/`GITHUB_ACTIONS` alone must not (remote arm64 may lack usable bwrap).
fn skip_if_enforcement_unavailable() -> bool {
    let require = std::env::var(REQUIRE_ENV).is_ok();

    let support = echo_agent_sandbox::SandboxManager::support_info();
    if !support.is_supported {
        if require {
            panic!(
                "enforcement required ({REQUIRE_ENV}) but sandbox unsupported: {}",
                support.details
            );
        }
        eprintln!("skipping: sandbox not supported ({})", support.details);
        return true;
    }

    #[cfg(target_os = "linux")]
    if !bwrap_available() {
        if require {
            panic!(
                "enforcement required ({REQUIRE_ENV}) but bwrap unavailable \
                 (required for Linux path / hook write-deny)"
            );
        }
        eprintln!("skipping: bwrap not installed (required for Linux path / hook write-deny)");
        return true;
    }

    false
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "echo-agent-sandbox-e2e-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dunce::canonicalize(&dir).expect("canonicalize temp dir")
}

/// Decode a comma-joined env list (empty/missing -> empty vec).
fn list_from_env(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .map(|v| {
            v.split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

// EROFS too: a root writer on Linux bypasses the mode-000 DAC check via
// CAP_DAC_OVERRIDE and hits the read-only bind-mount instead — still a denial.
fn is_permission_denied(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EROFS)
    )
}

/// Unlink of a read-only bind-mounted leaf can return EBUSY (ResourceBusy) on
/// Linux bubblewrap rather than EACCES/EPERM — still an effective denial.
fn is_unlink_denied(e: &std::io::Error) -> bool {
    is_permission_denied(e) || e.raw_os_error() == Some(libc::EBUSY)
}

/// Rename of a RO bind-mount leaf/mountpoint can return EXDEV or EBUSY — still
/// an effective denial (no destination created).
fn is_rename_denied(e: &std::io::Error) -> bool {
    is_permission_denied(e) || matches!(e.raw_os_error(), Some(libc::EXDEV) | Some(libc::EBUSY))
}

/// Spawn a child command and `exit(1)` if its stdout exposes the secret MARKER.
/// Asserts marker-absence rather than a non-zero exit: a root reader of the
/// mode-000 placeholder gets empty output, which still means the path is shadowed.
fn assert_child_cannot_read(label: &str, program: &str, args: &[&str]) {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {program}: {e}"));
    if String::from_utf8_lossy(&out.stdout).contains(MARKER) {
        eprintln!("FAIL: {label} exposed MARKER");
        std::process::exit(1);
    }
}

/// Assert a denied file's bytes are unreadable via an in-process read, a `cat`
/// child (the `bash`/`grep` tools), and a nested `sh -c "cat"` child (the shell a
/// subagent shells out through). The property is MARKER-absence (EACCES/EPERM, or
/// empty output under root, all satisfy it).
fn assert_read_blocked(label: &str, path: &Path) {
    if let Ok(content) = fs::read_to_string(path)
        && content.contains(MARKER)
    {
        eprintln!("FAIL: {label} in-process read exposed MARKER");
        std::process::exit(1);
    }
    let s = path.display().to_string();
    assert_child_cannot_read(label, "cat", &[s.as_str()]);
    let sh_cmd = format!("cat '{s}'");
    assert_child_cannot_read(label, "sh", &["-c", sh_cmd.as_str()]);
    eprintln!("OK: {label} read blocked");
}

/// Assert a denied file cannot be overwritten (write must EACCES/EPERM, not
/// succeed — a permitted write would enable the relocation bypass below).
fn assert_write_denied(label: &str, path: &Path) {
    match fs::write(path, "overwrite-attempt") {
        Err(e) if is_permission_denied(&e) => eprintln!("OK: {label} write denied"),
        Err(e) => {
            eprintln!("FAIL: unexpected {label} write error: {e}");
            std::process::exit(1);
        }
        Ok(()) => {
            eprintln!("FAIL: {label} write was permitted (relocation bypass possible)");
            std::process::exit(1);
        }
    }
}

/// Assert the `mv x y && cat y` relocation bypass does not expose the bytes:
/// the rename must fail (unlink of the source is denied) so the moved copy never
/// materializes with the secret.
fn assert_rename_bypass_blocked(label: &str, path: &Path, workspace: &Path) {
    let name = path.file_name().unwrap().to_string_lossy();
    let moved = workspace.join(format!("exfil-{name}"));
    let _ = fs::rename(path, &moved); // expected to fail; bytes must not leak
    match fs::read_to_string(&moved) {
        Ok(c) if c.contains(MARKER) => {
            eprintln!("FAIL: {label} rename bypass exposed MARKER");
            std::process::exit(1);
        }
        _ => eprintln!("OK: {label} rename bypass blocked"),
    }
}

#[cfg(target_os = "linux")]
fn bwrap_available() -> bool {
    // `--version` only checks the binary exists; remote CI may have bwrap but
    // deny user namespace creation ("Creating new namespace failed: Operation not permitted").
    Command::new("bwrap")
        .args(["--bind", "/", "/", "--", "true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The custom profile under test, read from the env the parent set.
fn profile_from_env() -> echo_agent_sandbox::ProfileName {
    echo_agent_sandbox::ProfileName::Custom(std::env::var(PROFILE_ENV).expect(PROFILE_ENV))
}

// ── Subprocess entry point ──────────────────────────────────────────────

/// `#[ignore]`d — only runs when invoked by the parent test via `run_scenario`
/// / `run_hook_write_deny_scenario`.
#[test]
#[ignore]
fn subprocess_entry() {
    let scenario = match std::env::var(SCENARIO_ENV) {
        Ok(s) => s,
        Err(_) => return,
    };
    let workspace = std::env::var(WORKSPACE_ENV).expect(WORKSPACE_ENV);
    let workspace = dunce::canonicalize(&workspace).expect("canonicalize workspace");
    let workspace = workspace.as_path();

    // Isolate HOME/ECHO_AGENT_HOME before any config OnceLock init.
    let home = PathBuf::from(std::env::var(HOME_ENV).expect(HOME_ENV));
    let echo_agent_home =
        PathBuf::from(std::env::var(ECHO_AGENT_HOME_ENV).expect(ECHO_AGENT_HOME_ENV));
    // SAFETY: isolated subprocess; set before sandbox/config first use.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("ECHO_AGENT_HOME", &echo_agent_home);
    }

    match scenario.as_str() {
        "block_deny" => subprocess_block_deny(workspace),
        "hook_write_deny" => subprocess_hook_write_deny(workspace, /* first_run */ false),
        "hook_write_deny_first_run" => {
            subprocess_hook_write_deny(workspace, /* first_run */ true)
        }
        "hook_write_deny_marker_spoof" => subprocess_hook_write_deny_marker_spoof(&echo_agent_home),
        other => {
            eprintln!("unknown scenario: {other}");
            std::process::exit(99);
        }
    }
}

fn subprocess_profile_and_bwrap_reexec(
    profile: &echo_agent_sandbox::ProfileName,
    workspace: &Path,
) {
    #[cfg(target_os = "linux")]
    {
        if !echo_agent_sandbox::is_inside_bwrap() {
            // Drive the REAL routing the shell uses at startup — computing the
            // profile's deny / write-deny set, building the plan, and failing
            // closed on a partial bind — rather than hand-rolling a single-path
            // `bwrap_reexec_command`.
            match echo_agent_sandbox::bwrap_reexec_for_profile(profile, workspace) {
                Some(mut cmd) => {
                    use std::os::unix::process::CommandExt;
                    let err = cmd.exec(); // returns only if exec failed
                    eprintln!("bwrap re-exec failed: {err}");
                    std::process::exit(2);
                }
                // Outside bwrap with no command means the deny set could not
                // be secured. The shell fails closed here; mirror that.
                None => {
                    eprintln!("FAIL: bwrap_reexec_for_profile returned None outside bwrap");
                    std::process::exit(2);
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (profile, workspace);
    }
}

fn subprocess_block_deny(workspace: &Path) {
    let targets = list_from_env(TARGETS_ENV);
    let controls = list_from_env(CONTROLS_ENV);
    let profile = profile_from_env();
    subprocess_profile_and_bwrap_reexec(&profile, workspace);

    let mut sandbox = echo_agent_sandbox::SandboxManager::new(profile, workspace);
    if let Err(e) = sandbox.apply(workspace) {
        eprintln!("sandbox apply failed: {e}");
        std::process::exit(3);
    }
    if !sandbox.is_applied() {
        eprintln!("sandbox was not applied (unsupported platform?)");
        std::process::exit(4);
    }

    for rel in &targets {
        let path = workspace.join(rel);
        assert_read_blocked(rel, &path);
        assert_write_denied(rel, &path);
        assert_rename_bypass_blocked(rel, &path, workspace);
    }

    for rel in &controls {
        match fs::read_to_string(workspace.join(rel)) {
            Ok(c) if c.contains("hello") => eprintln!("OK: {rel} control readable"),
            Ok(_) => {
                eprintln!("FAIL: control {rel} readable but missing marker");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("FAIL: control {rel} should stay readable: {e}");
                std::process::exit(1);
            }
        }
    }

    #[cfg(target_os = "macos")]
    for rel in list_from_env(POSTLAUNCH_ENV) {
        match fs::write(workspace.join(&rel), MARKER) {
            Err(e) if is_permission_denied(&e) => {
                eprintln!("OK: {rel} post-launch write denied")
            }
            Err(e) => {
                eprintln!("FAIL: unexpected {rel} post-launch write error: {e}");
                std::process::exit(1);
            }
            Ok(()) => {
                eprintln!("FAIL: {rel} post-launch matching path was writable");
                std::process::exit(1);
            }
        }
    }
    #[cfg(target_os = "macos")]
    if !list_from_env(POSTLAUNCH_ENV).is_empty() {
        match fs::write(workspace.join("late-control.txt"), "hello") {
            Ok(()) => eprintln!("OK: post-launch control writable"),
            Err(e) => {
                eprintln!("FAIL: non-matching post-launch path should be writable: {e}");
                std::process::exit(1);
            }
        }
    }

    std::process::exit(0);
}

/// Assert a path cannot be created via `create_dir` (mkdir denied).
fn assert_mkdir_denied(label: &str, path: &Path) {
    match fs::create_dir(path) {
        Err(e) if is_permission_denied(&e) => eprintln!("OK: {label} mkdir denied"),
        Err(e) => {
            eprintln!("FAIL: unexpected {label} mkdir error: {e}");
            std::process::exit(1);
        }
        Ok(()) => {
            eprintln!("FAIL: {label} mkdir was permitted");
            let _ = fs::remove_dir(path);
            std::process::exit(1);
        }
    }
}

/// Assert a path cannot be unlinked.
fn assert_unlink_denied(label: &str, path: &Path) {
    match fs::remove_file(path) {
        Err(e) if is_unlink_denied(&e) => eprintln!("OK: {label} unlink denied"),
        other => {
            eprintln!("FAIL: {label} unlink expected denial, got {other:?}");
            std::process::exit(1);
        }
    }
}

/// Assert a rename of `from` out of the deny set fails.
fn assert_rename_denied(label: &str, from: &Path, to: &Path) {
    match fs::rename(from, to) {
        Err(e) if is_rename_denied(&e) => eprintln!("OK: {label} rename denied"),
        other => {
            eprintln!("FAIL: {label} rename expected denial, got {other:?}");
            std::process::exit(1);
        }
    }
}

/// Assert a non-denied sibling path is writable.
fn assert_write_ok(label: &str, path: &Path) {
    match fs::write(path, "ok") {
        Ok(()) => eprintln!("OK: {label} writable"),
        Err(e) => {
            eprintln!("FAIL: {label} should be writable: {e}");
            std::process::exit(1);
        }
    }
}

/// Marker spoof: claim to be inside bwrap without real RO mounts — verify must fail.
/// Linux-only (verify is a no-op on macOS). Isolated subprocess; no shared env mutation.
fn subprocess_hook_write_deny_marker_spoof(_echo_agent_home: &Path) {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("OK: marker spoof N/A on non-linux");
        std::process::exit(0);
    }
    #[cfg(target_os = "linux")]
    {
        // Fixture already has hooks/ + hooks-paths from parent.
        // SAFETY: isolated subprocess.
        unsafe {
            std::env::set_var("__ECHO_AGENT_INSIDE_BWRAP", "1");
        }
        match echo_agent_sandbox::verify_hook_write_deny_enforced() {
            Ok(()) => {
                eprintln!("FAIL: marker alone must not satisfy write-deny verification");
                std::process::exit(1);
            }
            Err(msg) => {
                if msg.contains("read-only")
                    || msg.contains("NotReadOnly")
                    || msg.contains("hook write-deny")
                    || msg.contains("effectively read-only")
                {
                    eprintln!("OK: marker spoof refused ({msg})");
                    std::process::exit(0);
                }
                eprintln!("FAIL: unexpected verify error: {msg}");
                std::process::exit(1);
            }
        }
    }
}

/// Workspace-profile EchoAgent-owned hook write-deny probes (existing sources + first-run).
fn subprocess_hook_write_deny(workspace: &Path, first_run: bool) {
    let home = PathBuf::from(std::env::var(ECHO_AGENT_HOME_ENV).expect(ECHO_AGENT_HOME_ENV));

    let profile = echo_agent_sandbox::ProfileName::Workspace;
    subprocess_profile_and_bwrap_reexec(&profile, workspace);

    let mut sandbox = echo_agent_sandbox::SandboxManager::new(profile, workspace);
    if let Err(e) = sandbox.apply(workspace) {
        eprintln!("sandbox apply failed: {e}");
        std::process::exit(3);
    }
    // Seatbelt is the macOS enforcement path; on Linux the write-denies are
    // primarily the bwrap ro-binds established above.
    #[cfg(target_os = "macos")]
    if !sandbox.is_applied() {
        eprintln!("sandbox was not applied");
        std::process::exit(4);
    }

    let hooks_dir = home.join("hooks");
    let hooks_paths = home.join("hooks-paths");

    if first_run {
        // Fixed slots are ensured as real host paths before apply; they exist
        // and must be write-denied (not private placeholders).
        if !hooks_dir.is_dir() {
            eprintln!("FAIL: first-run expected real hooks dir to be ensured");
            std::process::exit(1);
        }
        if !hooks_paths.is_file() {
            eprintln!("FAIL: first-run expected real hooks-paths file to be ensured");
            std::process::exit(1);
        }
        assert_write_denied("hooks-paths (first-run)", &hooks_paths);
        assert_mkdir_denied("hooks nested (first-run)", &hooks_dir.join("nested"));
        assert_write_denied(
            "hooks nested file (first-run)",
            &hooks_dir.join("planted.json"),
        );
        eprintln!("OK: first-run EchoAgent hook slots denied");
    } else {
        // Existing hook content stays readable.
        let keep = hooks_dir.join("keep.json");
        match fs::read_to_string(&keep) {
            Ok(c) if c.contains("keep-me") => eprintln!("OK: hooks readable"),
            other => {
                eprintln!("FAIL: expected readable hook, got {other:?}");
                std::process::exit(1);
            }
        }

        assert_write_denied("hooks file", &hooks_dir.join("planted.json"));
        assert_write_denied("hooks-paths", &hooks_paths);
        let dynamic = home.join("sessions").join("extra-hooks");
        assert_write_denied("dynamic target", &dynamic.join("x.json"));

        assert_unlink_denied("hooks-paths", &hooks_paths);
        assert_rename_denied("hooks", &keep, &home.join("keep.exfil"));
        assert_mkdir_denied("hooks nested dir", &hooks_dir.join("nested-deny"));

        // Parent-rename bypass: renaming `sessions` must fail; leaf stays protected.
        let sessions = home.join("sessions");
        let sessions_old = home.join("sessions-old");
        match fs::rename(&sessions, &sessions_old) {
            Err(e) if is_rename_denied(&e) => {
                eprintln!("OK: parent rename denied");
            }
            other => {
                // If rename somehow succeeded, the lexical target must still
                // not be a writable fresh tree — but success is a hard fail.
                let _ = fs::rename(&sessions_old, &sessions);
                eprintln!("FAIL: parent rename expected denial, got {other:?}");
                std::process::exit(1);
            }
        }
        // Sibling under sessions still writable (ancestor pin is node-only on macOS;
        // on Linux the sessions dir is a RW mountpoint so creates inside still work).
        assert_write_ok(
            "sessions sibling",
            &sessions.join(format!("runtime-{}.lock", std::process::id())),
        );

        // Configured source under workspace (writable grant root): parent rename
        // denied; sibling under the same parent remains writable.
        let ws_parent = workspace.join("extra-parent");
        let ws_hooks = ws_parent.join("vendor-hooks");
        if ws_hooks.is_dir() {
            assert_write_denied("ws configured", &ws_hooks.join("x.json"));
            let renamed = workspace.join("extra-parent-old");
            match fs::rename(&ws_parent, &renamed) {
                Err(e) if is_rename_denied(&e) => {
                    eprintln!("OK: workspace parent rename denied");
                }
                other => {
                    let _ = fs::rename(&renamed, &ws_parent);
                    eprintln!("FAIL: workspace parent rename expected denial, got {other:?}");
                    std::process::exit(1);
                }
            }
            assert_write_ok(
                "workspace sibling under parent",
                &ws_parent.join(format!("sib-{}.lock", std::process::id())),
            );
        }
    }

    // Nested userns: exploit must run *inside* unshare; seccomp must make
    // unshare fail (non-success). Host hooks must stay unchanged.
    #[cfg(target_os = "linux")]
    if !first_run {
        let planted = hooks_dir.join("userns-plant.json");
        let alias = home.join("userns-alias");
        let inner = format!(
            "mkdir -p '{alias}' && mount --bind '{home}' '{alias}' && \
             echo nested > '{alias}/hooks/userns-plant.json'",
            alias = alias.display(),
            home = home.display(),
        );
        let sh = format!("unshare -Ur -m sh -c {inner:?}");
        // Confirm `unshare` exists so failure is not a missing binary.
        let which = Command::new("sh")
            .args(["-c", "command -v unshare"])
            .output()
            .expect("command -v unshare");
        if !which.status.success() {
            eprintln!("FAIL: unshare binary missing; cannot assert seccomp denial");
            std::process::exit(1);
        }
        let out = Command::new("sh")
            .args(["-c", &sh])
            .output()
            .expect("spawn unshare probe");
        if out.status.success() {
            eprintln!(
                "FAIL: unshare exploit succeeded (seccomp should EPERM); stderr={}",
                String::from_utf8_lossy(&out.stderr)
            );
            std::process::exit(1);
        }
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        // Kernel/seccomp typically surfaces EPERM; also accept "not permitted".
        if !(err.contains("not permitted")
            || err.contains("operation not permitted")
            || err.contains("eperm")
            || out.status.code() == Some(1))
        {
            eprintln!(
                "FAIL: expected seccomp EPERM-style denial, got status={:?} stderr={err}",
                out.status
            );
            std::process::exit(1);
        }
        if planted.exists()
            && let Ok(c) = fs::read_to_string(&planted)
            && c.contains("nested")
        {
            eprintln!("FAIL: nested userns rewrote host hooks");
            std::process::exit(1);
        }
        eprintln!("OK: nested userns did not rewrite hooks");
    }

    // Root-only: even if CAP_SYS_ADMIN were present, --cap-drop ALL should deny
    // mount; skip when not uid 0.
    #[cfg(target_os = "linux")]
    if !first_run {
        let uid = unsafe { libc::getuid() };
        if uid == 0 {
            let m = Command::new("mount")
                .args([
                    "-o",
                    "bind",
                    "/",
                    &home.join("cap-drop-probe").display().to_string(),
                ])
                .output();
            if let Ok(o) = m
                && o.status.success()
            {
                eprintln!("FAIL: mount succeeded despite --cap-drop ALL");
                std::process::exit(1);
            }
            eprintln!("OK: cap-drop mount denied as root");
        } else {
            eprintln!("OK: cap-drop root probe skipped (non-root)");
        }
    }

    // Parent grants remain creatable: EchoAgent runtime sibling, workspace, temp.
    assert_write_ok(
        "echoagent runtime sibling",
        &home.join(format!("leader-{}.lock", std::process::id())),
    );
    assert_write_ok("workspace sibling", &workspace.join("fresh.rs"));
    let tmp = std::env::temp_dir().join(format!("hook-wd-tmp-{}", std::process::id()));
    assert_write_ok("temp sibling", &tmp);
    let _ = fs::remove_file(&tmp);

    eprintln!("OK: hook write-deny e2e passed");
    std::process::exit(0);
}

// ── Parent test cases ───────────────────────────────────────────────────

/// Create isolated HOME + ECHO_AGENT_HOME fixture dirs for a scenario.
fn fixture_homes(
    tag: &str,
) -> (
    PathBuf,
    PathBuf,
    PathBuf,
    TempDirGuard,
    TempDirGuard,
    TempDirGuard,
) {
    let home = unique_temp_dir(&format!("{tag}-home"));
    let echoagent = unique_temp_dir(&format!("{tag}-echo-agent"));
    let workspace = unique_temp_dir(&format!("{tag}-ws"));
    // Empty global sandbox config under fixture ECHO_AGENT_HOME so generic tests do
    // not inherit the developer/runner's ~/.echo-agent/sandbox.toml.
    fs::write(echoagent.join("sandbox.toml"), "").expect("empty global sandbox.toml");
    (
        home.clone(),
        echoagent.clone(),
        workspace.clone(),
        TempDirGuard(home),
        TempDirGuard(echoagent),
        TempDirGuard(workspace),
    )
}

/// Drive one deny case end-to-end: define a custom profile whose `deny` list is
/// `deny_entries` (exact paths and/or globs), create each `target` (with the
/// MARKER) and each `control` (readable), then assert in an isolated subprocess
/// that every target is read/write/rename-denied and every control stays
/// readable. Shared by the exact-path and glob cases.
fn run_deny_case(
    tag: &str,
    profile: &str,
    deny_entries: &[&str],
    targets: &[&str],
    controls: &[&str],
    postlaunch: &[&str],
) {
    if skip_if_enforcement_unavailable() {
        return;
    }

    let (home, echoagent, tmp, _ch, _cg, _cw) = fixture_homes(tag);

    let deny_list = deny_entries
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::create_dir_all(tmp.join(".echo-agent")).expect("mkdir .echo-agent");
    fs::write(
        tmp.join(".echo-agent").join("sandbox.toml"),
        format!("[profiles.{profile}]\nextends = \"workspace\"\ndeny = [{deny_list}]\n"),
    )
    .expect("write sandbox.toml");

    // Ensure EchoAgent fixed slots exist so workspace-based custom profiles can
    // resolve hook write-deny without depending on the real user tree.
    fs::create_dir_all(echoagent.join("hooks")).expect("mkdir fixture hooks");
    fs::write(echoagent.join("hooks-paths"), b"").expect("write fixture hooks-paths");

    for rel in targets {
        let path = tmp.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir denied parent");
        }
        fs::write(&path, format!("SECRET={MARKER}")).expect("write denied file");
    }
    for rel in controls {
        let path = tmp.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir control parent");
        }
        fs::write(&path, "hello workspace").expect("write control");
    }

    let (status, stderr) = run_scenario(
        &home, &echoagent, &tmp, profile, targets, controls, postlaunch,
    );
    assert!(
        status.success(),
        "[{tag}] custom-profile deny should block read/write/rename\nstderr: {stderr}"
    );
    for rel in targets {
        assert!(
            stderr.contains(&format!("OK: {rel} read blocked")),
            "[{tag}] expected '{rel}' read block confirmation\nstderr: {stderr}"
        );
        assert!(
            stderr.contains(&format!("OK: {rel} write denied")),
            "[{tag}] expected '{rel}' write to be denied\nstderr: {stderr}"
        );
        assert!(
            stderr.contains(&format!("OK: {rel} rename bypass blocked")),
            "[{tag}] expected '{rel}' rename bypass to be blocked\nstderr: {stderr}"
        );
    }
    for rel in controls {
        assert!(
            stderr.contains(&format!("OK: {rel} control readable")),
            "[{tag}] expected non-denied control '{rel}' to stay readable\nstderr: {stderr}"
        );
    }
    #[cfg(target_os = "macos")]
    for rel in postlaunch {
        assert!(
            stderr.contains(&format!("OK: {rel} post-launch write denied")),
            "[{tag}] expected post-launch matching '{rel}' to be write-denied\nstderr: {stderr}"
        );
    }
    #[cfg(target_os = "macos")]
    if !postlaunch.is_empty() {
        assert!(
            stderr.contains("OK: post-launch control writable"),
            "[{tag}] expected non-matching post-launch path to stay writable\nstderr: {stderr}"
        );
    }

    // Generic harness must not leave vendor stubs under fixture HOME.
    assert!(
        !home.join(".claude").exists(),
        "generic deny must not create ~/.claude under fixture HOME"
    );
    assert!(
        !home.join(".cursor").exists(),
        "generic deny must not create ~/.cursor under fixture HOME"
    );
}

#[test]
fn deny_exact_paths_block_read_write_rename() {
    run_deny_case(
        "exact",
        "denytest",
        &[".env", "src/server.pem", "secretdir"],
        &[".env", "src/server.pem", "secretdir/inner.pem"],
        &["readable.txt"],
        &[],
    );
}

#[test]
fn deny_globs_block_read_write_rename() {
    run_deny_case(
        "glob",
        "denyglob",
        &["**/*.pem", "**/.env", "secrets/**"],
        &["sub/dir/key.pem", ".env", "sub/.env", "secrets/inner.key"],
        &["readable.txt", "sub/dir/keep.txt"],
        &["late.pem"],
    );
}

/// Hard-linked registry file must refuse sandbox startup (writable alias).
#[test]
fn hardlinked_hooks_paths_refuses_startup() {
    if skip_if_enforcement_unavailable() {
        return;
    }
    let (home, echoagent, workspace, _ch, _cg, _cw) = fixture_homes("hook-hl");
    fs::create_dir_all(echoagent.join("hooks")).unwrap();
    let reg = echoagent.join("hooks-paths");
    let alias = echoagent.join("hooks-paths-alias");
    fs::write(&reg, b"").unwrap();
    fs::hard_link(&reg, &alias).unwrap();

    let (status, stderr) =
        run_hook_write_deny_scenario(&home, &echoagent, &workspace, "hook_write_deny");
    assert!(
        !status.success(),
        "hard-linked hooks-paths must refuse startup\nstderr: {stderr}"
    );
    // Plan/materialization path should surface hard-link or identity failure.
    assert!(
        stderr.contains("hard-link")
            || stderr.contains("HardLink")
            || stderr.contains("hook write-deny")
            || stderr.contains("nlink"),
        "expected hard-link refusal signal\nstderr: {stderr}"
    );
}

/// Workspace profile: EchoAgent-owned direct hook sources are write-denied but readable;
/// create / overwrite / unlink / rename / mkdir fail; absolute hooks-paths
/// targets are denied; parent rename is blocked; EchoAgent/CWD/temp siblings stay writable.
#[test]
fn workspace_protects_direct_hook_sources() {
    if skip_if_enforcement_unavailable() {
        return;
    }

    let (home, echoagent, workspace, _ch, _cg, _cw) = fixture_homes("hook");

    fs::create_dir_all(echoagent.join("hooks")).expect("mkdir hooks");
    fs::write(
        echoagent.join("hooks").join("keep.json"),
        r#"{"keep-me":true}"#,
    )
    .expect("write keep.json");
    let dynamic = echoagent.join("sessions").join("extra-hooks");
    fs::create_dir_all(&dynamic).expect("mkdir dynamic hooks target");
    fs::write(dynamic.join("x.json"), r#"{"x":1}"#).expect("write dynamic hook");
    // Configured target under workspace (absolute) for grant-root ancestor pins.
    let ws_hooks = workspace.join("extra-parent").join("vendor-hooks");
    fs::create_dir_all(&ws_hooks).expect("mkdir ws vendor hooks");
    fs::write(ws_hooks.join("x.json"), r#"{"x":1}"#).expect("write ws hook");
    fs::write(
        echoagent.join("hooks-paths"),
        format!("{}\n{}\n", dynamic.display(), ws_hooks.display()),
    )
    .expect("write hooks-paths");

    let (status, stderr) =
        run_hook_write_deny_scenario(&home, &echoagent, &workspace, "hook_write_deny");
    assert!(
        status.success(),
        "hook write-deny e2e failed: {status}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("OK: hook write-deny e2e passed"),
        "missing pass marker\nstderr: {stderr}"
    );
    for needle in [
        "OK: hooks readable",
        "OK: hooks file write denied",
        "OK: hooks-paths write denied",
        "OK: dynamic target write denied",
        "OK: hooks-paths unlink denied",
        "OK: hooks rename denied",
        "OK: hooks nested dir mkdir denied",
        "OK: parent rename denied",
        "OK: sessions sibling writable",
        "OK: workspace parent rename denied",
        "OK: workspace sibling under parent writable",
        "OK: echoagent runtime sibling writable",
        "OK: workspace sibling writable",
        "OK: temp sibling writable",
    ] {
        assert!(
            stderr.contains(needle),
            "expected '{needle}'\nstderr: {stderr}"
        );
    }
    #[cfg(target_os = "linux")]
    assert!(
        stderr.contains("OK: nested userns did not rewrite hooks"),
        "expected nested userns check\nstderr: {stderr}"
    );
}

/// Hard-linked or symlinked discovery JSON under hooks/ must refuse startup.
#[test]
fn hardlinked_hooks_json_refuses_startup() {
    if skip_if_enforcement_unavailable() {
        return;
    }
    let (home, echoagent, workspace, _ch, _cg, _cw) = fixture_homes("hook-json-hl");
    fs::create_dir_all(echoagent.join("hooks")).unwrap();
    fs::write(echoagent.join("hooks-paths"), b"").unwrap();
    let active = echoagent.join("hooks").join("active.json");
    let alias = echoagent.join("hooks").join("active-alias.json");
    fs::write(&active, r#"{"hooks":{}}"#).unwrap();
    fs::hard_link(&active, &alias).unwrap();

    let (status, stderr) =
        run_hook_write_deny_scenario(&home, &echoagent, &workspace, "hook_write_deny");
    assert!(
        !status.success(),
        "hard-linked hooks JSON must refuse startup\nstderr: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn symlinked_hooks_json_refuses_startup() {
    if skip_if_enforcement_unavailable() {
        return;
    }
    let (home, echoagent, workspace, _ch, _cg, _cw) = fixture_homes("hook-json-sym");
    fs::create_dir_all(echoagent.join("hooks")).unwrap();
    fs::write(echoagent.join("hooks-paths"), b"").unwrap();
    let real = echoagent.join("real-active.json");
    let active = echoagent.join("hooks").join("active.json");
    fs::write(&real, r#"{"hooks":{}}"#).unwrap();
    std::os::unix::fs::symlink(&real, &active).unwrap();

    let (status, stderr) =
        run_hook_write_deny_scenario(&home, &echoagent, &workspace, "hook_write_deny");
    assert!(
        !status.success(),
        "symlinked hooks JSON must refuse startup\nstderr: {stderr}"
    );
}

/// First-run: missing fixed slots are created as real EchoAgent state before apply,
/// then write-denied. Parent asserts post-exit host tree is valid (no vendor stubs).
#[test]
fn workspace_protects_direct_hook_sources_first_run() {
    if skip_if_enforcement_unavailable() {
        return;
    }

    let (home, echoagent, workspace, _ch, _cg, _cw) = fixture_homes("hook-fr");
    // Intentionally leave hooks/ and hooks-paths absent (first-run ensure path).

    let (status, stderr) =
        run_hook_write_deny_scenario(&home, &echoagent, &workspace, "hook_write_deny_first_run");
    assert!(
        status.success(),
        "hook write-deny first-run e2e failed: {status}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("OK: hook write-deny e2e passed"),
        "missing pass marker\nstderr: {stderr}"
    );
    for needle in [
        "OK: first-run EchoAgent hook slots denied",
        "OK: hooks-paths (first-run) write denied",
        "OK: hooks nested (first-run) mkdir denied",
        "OK: hooks nested file (first-run) write denied",
        "OK: echoagent runtime sibling writable",
        "OK: workspace sibling writable",
        "OK: temp sibling writable",
    ] {
        assert!(
            stderr.contains(needle),
            "expected '{needle}'\nstderr: {stderr}"
        );
    }

    // Post-exit host: EchoAgent slots exist and are valid; no vendor artifacts.
    assert!(
        echoagent.join("hooks").is_dir(),
        "post-exit: hooks dir must exist as a real directory"
    );
    assert!(
        echoagent.join("hooks-paths").is_file(),
        "post-exit: hooks-paths must exist as a real file"
    );
    assert_eq!(
        fs::read(echoagent.join("hooks-paths")).expect("read hooks-paths"),
        b"",
        "post-exit: first-run hooks-paths must be empty"
    );
    assert!(
        !home.join(".claude").exists(),
        "post-exit: must not create ~/.claude"
    );
    assert!(
        !home.join(".cursor").exists(),
        "post-exit: must not create ~/.cursor"
    );
}

/// Marker spoof in an isolated subprocess (no env-mutating unit test).
#[test]
fn hook_write_deny_refuses_marker_spoof() {
    // Always runnable on Linux unit path via soft-skip only when not requiring
    // kernel enforcement — marker spoof only needs the verify API.
    #[cfg(not(target_os = "linux"))]
    {}
    #[cfg(target_os = "linux")]
    {
        let (home, echoagent, workspace, _ch, _cg, _cw) = fixture_homes("hook-spoof");
        fs::create_dir_all(echoagent.join("hooks")).unwrap();
        fs::write(echoagent.join("hooks").join("x.json"), b"{}").unwrap();
        fs::write(echoagent.join("hooks-paths"), b"").unwrap();
        let (status, stderr) = run_hook_write_deny_scenario(
            &home,
            &echoagent,
            &workspace,
            "hook_write_deny_marker_spoof",
        );
        assert!(
            status.success(),
            "marker spoof e2e failed: {status}\nstderr: {stderr}"
        );
        assert!(
            stderr.contains("OK: marker spoof refused"),
            "expected spoof refusal\nstderr: {stderr}"
        );
    }
}

struct TempDirGuard(std::path::PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
