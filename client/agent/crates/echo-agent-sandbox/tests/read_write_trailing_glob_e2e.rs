//! E2E: a `read_write` config entry with a trailing `/**` must grant the
//! parent directory — not create and grant a directory literally named `**`.
//!
//! The subprocess applies a custom profile whose only extra grant is
//! `<cache>/**`, then probes writes inside the cache tree (must succeed) and
//! outside any grant (must fail, proving enforcement was active).
//!
//! Soft-skips when kernel enforcement is unavailable;
//! `SANDBOX_E2E_REQUIRE_ENFORCEMENT=1` hard-requires it.
//!
//! ```text
//! cargo test -p echo-agent-sandbox --test read_write_trailing_glob_e2e -- --nocapture
//! ```

#![cfg(all(unix, feature = "enforce"))]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const ROOT_ENV: &str = "STARSTAR_E2E_ROOT";
const REQUIRE_ENV: &str = "SANDBOX_E2E_REQUIRE_ENFORCEMENT";

/// Removes the fixture tree on both pass and panic, so failed runs don't
/// accumulate under the real `~/.cache`.
struct CleanupGuard(PathBuf);

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn skip_if_enforcement_unavailable() -> bool {
    let support = echo_agent_sandbox::SandboxManager::support_info();
    if !support.is_supported {
        assert!(
            std::env::var(REQUIRE_ENV).is_err(),
            "enforcement required but sandbox unsupported: {}",
            support.details
        );
        eprintln!("skipping: sandbox unsupported ({})", support.details);
        return true;
    }
    false
}

#[test]
fn trailing_glob_read_write_grants_parent_directory() {
    if skip_if_enforcement_unavailable() {
        return;
    }

    // Not under TMPDIR or the test workspace: base profiles already
    // write-allow those trees, which would hide allow-path isolation.
    let root = dirs::home_dir()
        .expect("home dir required: the control probe relies on HOME-relative paths")
        .join(".cache")
        .join("echo-agent-starstar-e2e")
        .join(format!(
            "run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_millis()
        ));
    let _cleanup = CleanupGuard(root.clone());
    let workspace = root.join("ws");
    let home = root.join("home");
    let echo_agent_home = root.join("echo-agent-home");
    // Under the fixture HOME, which no base grant covers: only the explicit
    // read_write entry can make this tree writable.
    let cache = home.join("cargo-cache");
    for dir in [&workspace, &echo_agent_home, &cache.join("registry-a1b2")] {
        fs::create_dir_all(dir).expect("create fixture dir");
    }

    let cache_abs = dunce::canonicalize(&cache).expect("canonicalize cache");
    fs::write(
        echo_agent_home.join("sandbox.toml"),
        format!(
            "[profiles.cargo]\nextends = \"workspace\"\nread_write = [{:?}]\n",
            format!("{}/**", cache_abs.display())
        ),
    )
    .expect("write sandbox.toml");

    let exe = std::env::current_exe().expect("current_exe");
    let output = Command::new(&exe)
        .env(ROOT_ENV, root.as_os_str())
        .env("HOME", home.as_os_str())
        .env("ECHO_AGENT_HOME", echo_agent_home.as_os_str())
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .arg("subprocess_entry")
        .output()
        .expect("spawn subprocess");
    eprintln!(
        "subprocess stdout:\n{}\nsubprocess stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "subprocess failed: {output:?}");

    let starstar = cache_abs.join("**");
    assert!(
        !starstar.exists(),
        "apply must not create a literal '**' directory at {starstar:?}"
    );
    assert!(
        cache_abs
            .join("registry-a1b2")
            .join("write-marker")
            .is_file(),
        "write under the granted cache tree must have succeeded"
    );
}

/// Applies the sandbox (irreversible), so this runs only as a subprocess of
/// the test above.
#[test]
#[ignore]
fn subprocess_entry() {
    let Ok(root) = std::env::var(ROOT_ENV) else {
        return;
    };
    let root = PathBuf::from(root);
    let workspace = root.join("ws");
    let cache = dunce::canonicalize(root.join("home").join("cargo-cache")).expect("cache");

    let profile: echo_agent_sandbox::ProfileName = "cargo".parse().expect("profile name");
    let mut mgr = echo_agent_sandbox::SandboxManager::new(profile, &workspace);
    if let Err(e) = mgr.apply(&workspace) {
        eprintln!("FAIL: apply errored: {e}");
        std::process::exit(3);
    }

    if let Err(e) = fs::write(cache.join("registry-a1b2").join("write-marker"), b"ok") {
        eprintln!("FAIL: write under granted cache tree denied: {e}");
        std::process::exit(4);
    }
    // Control probe: HOME itself carries no grant, so enforcement being
    // active is observable — without it the positive probe proves nothing.
    if fs::write(root.join("home").join("denied-marker"), b"no").is_ok() {
        eprintln!("FAIL: write outside all grants succeeded (sandbox not enforced?)");
        std::process::exit(5);
    }
    std::process::exit(0);
}
