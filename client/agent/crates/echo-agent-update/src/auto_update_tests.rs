use super::*;

#[test]
fn test_tmp_download_path_is_unique_per_version_and_per_attempt() {
    // The old `with_extension("tmp")` collapsed every 0.1.x versioned
    // name onto a single `echo-agent-0.1.tmp`; the helper must keep distinct
    // versions distinct AND make repeated attempts (same process, e.g.
    // concurrent tokio tasks) unique.
    let dest_181 =
        std::path::Path::new("/home/u/.echo-agent/downloads/echo-agent-0.1.181-linux-x86_64");
    let dest_182 =
        std::path::Path::new("/home/u/.echo-agent/downloads/echo-agent-0.1.182-linux-x86_64");

    let a = tmp_download_path(dest_181);
    let b = tmp_download_path(dest_182);
    assert_ne!(a, b, "different versions must not share a temp file");

    let a2 = tmp_download_path(dest_181);
    assert_ne!(
        a, a2,
        "two attempts for the same dest must not share a temp file"
    );

    let name = a.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        name.starts_with("echo-agent-0.1.181-linux-x86_64."),
        "full versioned name must be preserved: {name}"
    );
    assert!(
        name.ends_with(".tmp") && name.contains(&std::process::id().to_string()),
        "temp name must embed the PID and end in .tmp (cleanup sweeps *.tmp*): {name}"
    );
    assert_eq!(
        a.parent(),
        std::path::Path::new("/home/u/.echo-agent/downloads").into(),
        "temp file must stay in the destination directory for atomic rename"
    );
}

#[test]
fn test_needs_update_same_version() {
    assert_eq!(
        needs_update("0.1.141", "0.1.141", "stable", false),
        Some(false)
    );
}

#[test]
fn test_needs_update_invalid_versions() {
    assert_eq!(
        needs_update("not-a-version", "0.1.141", "stable", false),
        None
    );
    assert_eq!(needs_update("0.1.141", "garbage", "stable", false), None);
}

#[test]
fn test_needs_update_unknown_channel() {
    assert_eq!(needs_update("0.1.140", "0.1.141", "beta", false), None);
}

#[test]
fn test_needs_update_enterprise_channel_behaves_like_stable() {
    // Enterprise uses the same conservative pre-release rules as stable.
    // Same version: no update.
    assert_eq!(
        needs_update("0.1.206", "0.1.206", "enterprise", false),
        Some(false)
    );
    // Newer stable: update.
    assert_eq!(
        needs_update("0.1.205", "0.1.206", "enterprise", false),
        Some(true)
    );
    // Older stable: no downgrade (allow_downgrade=false).
    assert_eq!(
        needs_update("0.1.207", "0.1.206", "enterprise", false),
        Some(false)
    );
    // Pre-release candidate rejected on enterprise channel.
    assert_eq!(
        needs_update("0.1.205", "0.1.206-alpha.1", "enterprise", false),
        Some(false)
    );
    // Current pre-release on enterprise forces upgrade (even to equal base).
    assert_eq!(
        needs_update("0.1.206-alpha.3", "0.1.206", "enterprise", false),
        Some(true)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_creates_new_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("binary-v1");
    std::fs::write(&target, "v1").unwrap();

    let link = dir.path().join("echoagent");
    // No existing symlink — should create one.
    atomic_symlink_swap(&target, &link).await.unwrap();

    assert!(link.is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v1");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_replaces_existing() {
    let dir = tempfile::tempdir().unwrap();

    let target_v1 = dir.path().join("binary-v1");
    std::fs::write(&target_v1, "v1").unwrap();
    let target_v2 = dir.path().join("binary-v2");
    std::fs::write(&target_v2, "v2").unwrap();

    let link = dir.path().join("echoagent");
    // Set up initial symlink to v1.
    std::os::unix::fs::symlink(&target_v1, &link).unwrap();
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v1");

    // Swap to v2.
    atomic_symlink_swap(&target_v2, &link).await.unwrap();

    assert!(link.is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), target_v2);
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v2");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_preserves_old_target() {
    let dir = tempfile::tempdir().unwrap();

    let target_v1 = dir.path().join("binary-v1");
    std::fs::write(&target_v1, "v1-content").unwrap();
    let target_v2 = dir.path().join("binary-v2");
    std::fs::write(&target_v2, "v2-content").unwrap();

    let link = dir.path().join("echoagent");
    std::os::unix::fs::symlink(&target_v1, &link).unwrap();

    // Swap to v2.
    atomic_symlink_swap(&target_v2, &link).await.unwrap();

    // The old target file must still exist on disk — this is the key
    // property that prevents SIGKILL on macOS.  Running processes that
    // have binary-v1 mmap'd can continue to page-fault from it.
    assert!(target_v1.exists(), "old binary must not be deleted");
    assert_eq!(std::fs::read_to_string(&target_v1).unwrap(), "v1-content");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_no_intermediate_missing_state() {
    // Verify that the link path always exists (is never absent) during
    // the swap.  We can't truly test atomicity without threads, but we
    // can at least verify the path exists before and after.
    let dir = tempfile::tempdir().unwrap();

    let target_v1 = dir.path().join("binary-v1");
    std::fs::write(&target_v1, "v1").unwrap();
    let target_v2 = dir.path().join("binary-v2");
    std::fs::write(&target_v2, "v2").unwrap();

    let link = dir.path().join("echoagent");
    std::os::unix::fs::symlink(&target_v1, &link).unwrap();
    assert!(link.exists(), "link should exist before swap");

    atomic_symlink_swap(&target_v2, &link).await.unwrap();
    assert!(link.exists(), "link should exist after swap");

    // No tmp-link file should be left behind.
    let tmp_link = link.with_extension("tmp-link");
    assert!(!tmp_link.exists(), "temp link should be cleaned up");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_replaces_regular_file() {
    // If the canonical path is a regular file (from an old non-symlink
    // installation), the swap should still work by replacing it.
    let dir = tempfile::tempdir().unwrap();

    let target = dir.path().join("binary-v2");
    std::fs::write(&target, "v2").unwrap();

    let link = dir.path().join("echoagent");
    // Simulate an old installation where echoagent is a regular file.
    std::fs::write(&link, "old-binary").unwrap();

    atomic_symlink_swap(&target, &link).await.unwrap();

    assert!(link.is_symlink());
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v2");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_succeeds_despite_leftover_tmp_link() {
    // A leftover .tmp-link from a crashed swap must not block a new swap:
    // unique per-racer temp names mean no collision.
    let dir = tempfile::tempdir().unwrap();

    let target_v1 = dir.path().join("binary-v1");
    std::fs::write(&target_v1, "v1").unwrap();
    let target_v2 = dir.path().join("binary-v2");
    std::fs::write(&target_v2, "v2").unwrap();

    let link = dir.path().join("echoagent");
    std::os::unix::fs::symlink(&target_v1, &link).unwrap();
    std::os::unix::fs::symlink(&target_v1, link.with_extension("tmp-link")).unwrap();

    atomic_symlink_swap(&target_v2, &link).await.unwrap();

    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v2");
}

#[cfg(unix)]
fn managed_layout() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let downloads = dir.path().join("downloads");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&downloads).unwrap();
    (dir, bin, downloads)
}

#[test]
fn test_installer_manages_bin_entrypoints_gate() {
    assert!(installer_manages_bin_entrypoints("internal"));
    assert!(installer_manages_bin_entrypoints("gh-release"));
    assert!(!installer_manages_bin_entrypoints("npm"));
    assert!(!installer_manages_bin_entrypoints("unknown"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_reconcile_agent_repoints_diverged_agent() {
    let (_dir, bin, downloads) = managed_layout();
    std::fs::write(downloads.join("echo-agent-0.2.101-macos-aarch64"), "new").unwrap();
    std::fs::write(downloads.join("echo-agent-0.1.199-macos-aarch64"), "old").unwrap();

    std::os::unix::fs::symlink(
        "../downloads/echo-agent-0.2.101-macos-aarch64",
        bin.join("echoagent"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../downloads/echo-agent-0.1.199-macos-aarch64",
        bin.join("agent"),
    )
    .unwrap();

    reconcile_agent_to_echo_agent(&bin).await;

    assert_eq!(
        std::fs::read_link(bin.join("agent")).unwrap(),
        std::path::PathBuf::from("../downloads/echo-agent-0.2.101-macos-aarch64"),
    );
    assert_eq!(std::fs::read_to_string(bin.join("agent")).unwrap(), "new");
    assert!(downloads.join("echo-agent-0.1.199-macos-aarch64").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn test_reconcile_agent_heals_legacy_unversioned_agent() {
    let (_dir, bin, downloads) = managed_layout();
    std::fs::write(downloads.join("echo-agent-0.2.101-macos-aarch64"), "new").unwrap();
    std::fs::write(downloads.join("echo-agent-macos-aarch64"), "legacy").unwrap();

    std::os::unix::fs::symlink(
        "../downloads/echo-agent-0.2.101-macos-aarch64",
        bin.join("echoagent"),
    )
    .unwrap();
    std::os::unix::fs::symlink("../downloads/echo-agent-macos-aarch64", bin.join("agent")).unwrap();

    reconcile_agent_to_echo_agent(&bin).await;

    assert_eq!(
        std::fs::read_link(bin.join("agent")).unwrap(),
        std::path::PathBuf::from("../downloads/echo-agent-0.2.101-macos-aarch64"),
    );
    assert_eq!(std::fs::read_to_string(bin.join("agent")).unwrap(), "new");
}

#[cfg(unix)]
#[tokio::test]
async fn test_reconcile_agent_creates_missing_agent() {
    let (_dir, bin, downloads) = managed_layout();
    std::fs::write(downloads.join("echo-agent-0.2.101-macos-aarch64"), "new").unwrap();
    std::os::unix::fs::symlink(
        "../downloads/echo-agent-0.2.101-macos-aarch64",
        bin.join("echoagent"),
    )
    .unwrap();

    reconcile_agent_to_echo_agent(&bin).await;

    assert!(bin.join("agent").is_symlink());
    assert_eq!(std::fs::read_to_string(bin.join("agent")).unwrap(), "new");
}

#[cfg(unix)]
#[tokio::test]
async fn test_reconcile_agent_noop_when_consistent() {
    let (_dir, bin, downloads) = managed_layout();
    std::fs::write(downloads.join("echo-agent-0.2.101-macos-aarch64"), "new").unwrap();
    let target = "../downloads/echo-agent-0.2.101-macos-aarch64";
    std::os::unix::fs::symlink(target, bin.join("echoagent")).unwrap();
    std::os::unix::fs::symlink(target, bin.join("agent")).unwrap();

    reconcile_agent_to_echo_agent(&bin).await;

    assert_eq!(
        std::fs::read_link(bin.join("agent")).unwrap(),
        std::path::PathBuf::from(target),
    );
    let leftovers = std::fs::read_dir(&bin)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-link"))
        .count();
    assert_eq!(leftovers, 0, "no temp links from a no-op reconcile");
}

#[cfg(unix)]
#[tokio::test]
async fn test_reconcile_agent_skips_when_echo_agent_dangling() {
    let (_dir, bin, downloads) = managed_layout();
    std::os::unix::fs::symlink(
        "../downloads/echo-agent-0.2.101-macos-aarch64",
        bin.join("echoagent"),
    )
    .unwrap();
    std::fs::write(downloads.join("echo-agent-0.1.199-macos-aarch64"), "old").unwrap();
    std::os::unix::fs::symlink(
        "../downloads/echo-agent-0.1.199-macos-aarch64",
        bin.join("agent"),
    )
    .unwrap();

    reconcile_agent_to_echo_agent(&bin).await;

    assert_eq!(
        std::fs::read_link(bin.join("agent")).unwrap(),
        std::path::PathBuf::from("../downloads/echo-agent-0.1.199-macos-aarch64"),
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_reconcile_agent_skips_when_echo_agent_not_symlink() {
    let (_dir, bin, downloads) = managed_layout();
    std::fs::write(bin.join("echoagent"), "copy-binary").unwrap();
    std::fs::write(downloads.join("echo-agent-0.1.199-macos-aarch64"), "old").unwrap();
    std::os::unix::fs::symlink(
        "../downloads/echo-agent-0.1.199-macos-aarch64",
        bin.join("agent"),
    )
    .unwrap();

    reconcile_agent_to_echo_agent(&bin).await;

    assert_eq!(
        std::fs::read_link(bin.join("agent")).unwrap(),
        std::path::PathBuf::from("../downloads/echo-agent-0.1.199-macos-aarch64"),
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_sweep_stale_tmp_links_removes_stale_keeps_fresh_and_active() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("binary-v1");
    std::fs::write(&target, "v1").unwrap();
    let link = dir.path().join("echoagent");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    // Old- and new-style leftover temp links.
    let leftover_old = dir.path().join("echoagent.tmp-link");
    let leftover_new = dir.path().join("echoagent.123-0.tmp-link");
    std::os::unix::fs::symlink(&target, &leftover_old).unwrap();
    std::os::unix::fs::symlink(&target, &leftover_new).unwrap();

    // max_age = ZERO: every leftover is stale and removed; the active
    // `echoagent` link (no `.tmp-link` suffix) is untouched.
    sweep_stale_tmp_links(&link, Duration::ZERO).await;
    assert!(!leftover_old.exists() && !leftover_new.exists());
    assert!(link.is_symlink(), "active link must be preserved");

    // A fresh leftover under a real max_age is preserved — it could be a
    // concurrent racer's in-flight link.
    let fresh = dir.path().join("echoagent.999-9.tmp-link");
    std::os::unix::fs::symlink(&target, &fresh).unwrap();
    sweep_stale_tmp_links(&link, Duration::from_secs(3600)).await;
    assert!(fresh.exists(), "fresh tmp-link must be preserved");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_multiple_sequential_swaps() {
    // Simulate v1 -> v2 -> v3 -> v4 sequential swaps.
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("echoagent");

    for i in 1..=4 {
        let target = dir.path().join(format!("binary-v{}", i));
        std::fs::write(&target, format!("content-v{}", i)).unwrap();
        atomic_symlink_swap(&target, &link).await.unwrap();

        assert!(link.is_symlink());
        assert_eq!(
            std::fs::read_to_string(&link).unwrap(),
            format!("content-v{}", i)
        );
    }

    // All old binaries should still be on disk.
    for i in 1..=4 {
        let target = dir.path().join(format!("binary-v{}", i));
        assert!(target.exists(), "binary-v{} should still exist", i);
    }

    // No temp files should remain.
    let tmp_link = link.with_extension("tmp-link");
    assert!(!tmp_link.exists(), "no temp link should remain");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_with_absolute_target() {
    // atomic_symlink_swap stores whatever path is given — if absolute,
    // readlink returns the absolute path.
    let dir = tempfile::tempdir().unwrap();

    let binary = dir.path().join("echo-agent-0.1.141");
    std::fs::write(&binary, "v141").unwrap();

    let link = dir.path().join("echoagent");
    atomic_symlink_swap(&binary, &link).await.unwrap();

    assert!(link.is_symlink());
    // readlink returns the absolute path we passed.
    assert_eq!(std::fs::read_link(&link).unwrap(), binary);
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v141");
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_with_relative_target() {
    // When given a relative path, the symlink stores a relative target.
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("downloads");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    std::fs::write(downloads.join("echo-agent-0.1.203"), "v203").unwrap();

    let rel_target = std::path::Path::new("../downloads/echo-agent-0.1.203");
    let link = bin.join("echoagent");
    atomic_symlink_swap(rel_target, &link).await.unwrap();

    assert!(link.is_symlink());
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        std::path::PathBuf::from("../downloads/echo-agent-0.1.203")
    );
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v203");
}

#[cfg(unix)]
#[test]
fn test_relative_symlink_target_sibling_dirs() {
    // bin/echoagent -> ../downloads/echo-agent-0.1.203
    let target = std::path::Path::new("/home/alice/.echo-agent/downloads/echo-agent-0.1.203");
    let link = std::path::Path::new("/home/alice/.echo-agent/bin/echoagent");
    let result = relative_symlink_target(target, link);
    assert_eq!(
        result,
        std::path::PathBuf::from("../downloads/echo-agent-0.1.203")
    );
}

#[cfg(unix)]
#[test]
fn test_relative_symlink_target_same_dir() {
    // downloads/echo-agent-latest -> echo-agent-0.1.203 (same directory)
    let target = std::path::Path::new("/home/alice/.echo-agent/downloads/echo-agent-0.1.203");
    let link = std::path::Path::new("/home/alice/.echo-agent/downloads/echo-agent-latest");
    let result = relative_symlink_target(target, link);
    assert_eq!(result, std::path::PathBuf::from("echo-agent-0.1.203"));
}

#[cfg(unix)]
#[test]
fn test_relative_symlink_target_cross_tree_stays_absolute() {
    // /usr/local/bin/echoagent -> /home/alice/.echo-agent/downloads/echo-agent-0.1.203
    // Different grandparents — should stay absolute.
    let target = std::path::Path::new("/home/alice/.echo-agent/downloads/echo-agent-0.1.203");
    let link = std::path::Path::new("/usr/local/bin/echoagent");
    let result = relative_symlink_target(target, link);
    assert_eq!(
        result,
        std::path::PathBuf::from("/home/alice/.echo-agent/downloads/echo-agent-0.1.203")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_relative_symlink_survives_directory_move() {
    // Simulates Docker bind-mount: create ~/.echo-agent/ layout at path A,
    // then move it to path B and verify the symlink still resolves.
    let dir = tempfile::tempdir().unwrap();

    // Create alice's layout
    let alice = dir.path().join("alice").join(".echo-agent");
    let alice_downloads = alice.join("downloads");
    let alice_bin = alice.join("bin");
    std::fs::create_dir_all(&alice_downloads).unwrap();
    std::fs::create_dir_all(&alice_bin).unwrap();
    std::fs::write(alice_downloads.join("echo-agent-0.1.203"), "binary-content").unwrap();

    // Create a relative symlink (what the fix produces)
    let rel_target = std::path::Path::new("../downloads/echo-agent-0.1.203");
    let link = alice_bin.join("echoagent");
    atomic_symlink_swap(rel_target, &link).await.unwrap();

    // Verify it works at the original location
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "binary-content");

    // "Bind-mount" to bob: copy the entire .echo-agent tree
    let bob_home = dir.path().join("bob");
    std::fs::create_dir_all(&bob_home).unwrap();
    let bob = bob_home.join(".echo-agent");
    let copy_status = std::process::Command::new("cp")
        .args(["-a", alice.to_str().unwrap(), bob.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(copy_status.success());

    // Verify the symlink resolves at bob's path too
    let bob_link = bob.join("bin").join("echoagent");
    assert!(bob_link.is_symlink());
    assert_eq!(
        std::fs::read_link(&bob_link).unwrap(),
        std::path::PathBuf::from("../downloads/echo-agent-0.1.203"),
        "symlink target should be relative"
    );
    assert_eq!(
        std::fs::read_to_string(&bob_link).unwrap(),
        "binary-content",
        "relative symlink should resolve at the new path"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_atomic_symlink_swap_broken_symlink_target() {
    // If the current symlink is broken (target deleted externally),
    // the swap should still succeed.
    let dir = tempfile::tempdir().unwrap();

    let link = dir.path().join("echoagent");
    // Create a broken symlink — points to a file that doesn't exist.
    std::os::unix::fs::symlink(dir.path().join("deleted-binary"), &link).unwrap();
    assert!(link.is_symlink());
    assert!(!link.exists(), "broken symlink should not 'exist'");

    // New target to swap to.
    let target = dir.path().join("binary-v2");
    std::fs::write(&target, "v2").unwrap();

    atomic_symlink_swap(&target, &link).await.unwrap();

    assert!(link.is_symlink());
    assert!(link.exists(), "symlink should now resolve");
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "v2");
}

#[test]
fn test_needs_update_prerelease_to_stable_forces_install() {
    // Inadmissible current (pre-release on stable channel) → install even
    // if the candidate is semver-lower.
    assert_eq!(
        needs_update("0.1.149-alpha.1", "0.1.148", "stable", false),
        Some(true)
    );
    assert_eq!(
        needs_update("0.1.148-alpha.3", "0.1.148", "stable", false),
        Some(true)
    );
}

#[test]
fn test_needs_update_stable_to_alpha_no_install_when_candidate_equal() {
    // Server returns max(stable, alpha) for alpha channel. When the user's
    // stable version already IS the candidate, no install needed.
    assert_eq!(
        needs_update("0.1.148", "0.1.148", "alpha", false),
        Some(false)
    );
}

#[test]
fn test_needs_update_stable_channel_never_gets_prerelease() {
    assert_eq!(
        needs_update("0.1.139", "0.1.140-alpha.1", "stable", false),
        Some(false)
    );
    assert_eq!(
        needs_update("0.1.0", "0.1.1-beta.1", "stable", false),
        Some(false)
    );
}

#[test]
fn test_needs_update_valid_current_only_upgrades() {
    // Admissible current on the target channel → pure semver (allow_downgrade=false).
    assert_eq!(
        needs_update("0.1.140", "0.1.141", "stable", false),
        Some(true)
    );
    assert_eq!(
        needs_update("0.1.141", "0.1.140", "stable", false),
        Some(false)
    );
    assert_eq!(
        needs_update("0.1.140-alpha.8", "0.1.140", "alpha", false),
        Some(true)
    );
    assert_eq!(
        needs_update("0.1.140", "0.1.139-alpha.5", "alpha", false),
        Some(false)
    );
    // Alpha → newer alpha: upgrade.
    assert_eq!(
        needs_update("0.1.148-alpha.1", "0.1.148-alpha.3", "alpha", false),
        Some(true)
    );
    // Alpha → older alpha: no downgrade (allow_downgrade=false).
    assert_eq!(
        needs_update("0.1.148-alpha.3", "0.1.148-alpha.2", "alpha", false),
        Some(false)
    );
}

#[test]
fn test_needs_update_large_version_numbers() {
    // Ensure no overflow on realistic version numbers
    assert_eq!(
        needs_update("0.1.140", "0.1.999", "stable", false),
        Some(true)
    );
    assert_eq!(
        needs_update("0.1.999", "0.2.0", "stable", false),
        Some(true)
    );
    assert_eq!(
        needs_update("99.99.99", "100.0.0", "stable", false),
        Some(true)
    );
}

#[tokio::test]
async fn test_cleanup_old_downloads_keeps_current_plus_one() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    // Simulate 5 old echoagent binaries in downloads dir.
    for v in ["0.1.140", "0.1.141", "0.1.142", "0.1.143", "0.1.144"] {
        std::fs::write(d.join(format!("echo-agent-{}-macos-aarch64", v)), v).unwrap();
    }
    // Current version.
    std::fs::write(d.join("echo-agent-0.1.145-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.145").await;

    // Current must survive.
    assert!(
        d.join("echo-agent-0.1.145-macos-aarch64").exists(),
        "current"
    );
    // Newest old version (0.1.144) must survive.
    assert!(d.join("echo-agent-0.1.144-macos-aarch64").exists(), "N-1");
    // Everything else should be deleted.
    assert!(
        !d.join("echo-agent-0.1.143-macos-aarch64").exists(),
        "0.1.143 should be deleted"
    );
    assert!(
        !d.join("echo-agent-0.1.142-macos-aarch64").exists(),
        "0.1.142 should be deleted"
    );
    assert!(
        !d.join("echo-agent-0.1.141-macos-aarch64").exists(),
        "0.1.141 should be deleted"
    );
    assert!(
        !d.join("echo-agent-0.1.140-macos-aarch64").exists(),
        "0.1.140 should be deleted"
    );
}

#[tokio::test]
async fn test_cleanup_old_downloads_does_not_touch_other_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    // echoagent and echo-agent-pager should not interfere with each other.
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64"), "old-echo-agent").unwrap();
    std::fs::write(
        d.join("echo-agent-0.1.141-macos-aarch64"),
        "current-echo-agent",
    )
    .unwrap();
    std::fs::write(
        d.join("echo-agent-pager-0.1.140-macos-aarch64"),
        "old-pager",
    )
    .unwrap();
    std::fs::write(
        d.join("echo-agent-pager-0.1.141-macos-aarch64"),
        "current-pager",
    )
    .unwrap();

    // Cleanup only echoagent — pager files must be untouched.
    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(d.join("echo-agent-0.1.141-macos-aarch64").exists());
    assert!(d.join("echo-agent-0.1.140-macos-aarch64").exists()); // only old, kept as N-1
    assert!(
        d.join("echo-agent-pager-0.1.140-macos-aarch64").exists(),
        "pager untouched"
    );
    assert!(
        d.join("echo-agent-pager-0.1.141-macos-aarch64").exists(),
        "pager untouched"
    );
}

/// Backdate a file's mtime past [`STALE_TMP_AGE`] so cleanup treats it
/// as an abandoned download / genuinely old binary.
fn make_stale(path: &std::path::Path) {
    let old = std::time::SystemTime::now() - (STALE_TMP_AGE + Duration::from_secs(60));
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();
}

/// Backdate every file in `dir`. Cleanup deliberately never deletes a
/// freshly-written binary or temp file (it may belong to a concurrent
/// in-flight install), so retention-policy tests must age their fixtures
/// to look like real leftovers from previous releases.
fn make_all_stale(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_file() {
            make_stale(&p);
        }
    }
}

#[tokio::test]
async fn test_cleanup_old_downloads_removes_stale_tmp_keeps_fresh_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    // Stale tmp: abandoned by a crashed updater — swept.
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64.tmp"), "partial").unwrap();
    make_stale(&d.join("echo-agent-0.1.140-macos-aarch64.tmp"));
    // Fresh tmp: a concurrent updater's in-flight download — kept, or
    // its atomic rename would fail with ENOENT.
    std::fs::write(
        d.join("echo-agent-0.1.142-macos-aarch64.77-0.tmp"),
        "inflight",
    )
    .unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(
        !d.join("echo-agent-0.1.140-macos-aarch64.tmp").exists(),
        "stale tmp cleaned up"
    );
    assert!(
        d.join("echo-agent-0.1.142-macos-aarch64.77-0.tmp").exists(),
        "fresh in-flight tmp must NOT be swept"
    );
    assert!(
        d.join("echo-agent-0.1.141-macos-aarch64").exists(),
        "current kept"
    );
}

#[tokio::test]
async fn test_cleanup_old_downloads_keeps_fresh_versioned_binary() {
    // A versioned binary written moments ago may be a concurrent
    // installer's just-renamed download whose symlink swap hasn't
    // happened yet — even when the retention policy would otherwise
    // delete it, it must survive until it ages.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    // Three old versions + current: policy would delete .138 and .139.
    for v in ["0.1.138", "0.1.139", "0.1.140"] {
        std::fs::write(d.join(format!("echo-agent-{v}-macos-aarch64")), v).unwrap();
    }
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();
    make_all_stale(d);
    // .138 is re-written NOW — simulating a racer that just renamed its
    // download into place (e.g. a rollback install racing an upgrade).
    std::fs::write(d.join("echo-agent-0.1.138-macos-aarch64"), "in-flight").unwrap();

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(
        d.join("echo-agent-0.1.141-macos-aarch64").exists(),
        "current"
    );
    assert!(
        d.join("echo-agent-0.1.140-macos-aarch64").exists(),
        "N-1 kept"
    );
    assert!(
        d.join("echo-agent-0.1.138-macos-aarch64").exists(),
        "fresh just-renamed binary must NOT be deleted"
    );
    assert!(
        !d.join("echo-agent-0.1.139-macos-aarch64").exists(),
        "genuinely old binary still swept"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_cleanup_old_downloads_skips_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    // echo-agent-latest is a symlink — must be skipped.
    let target = d.join("echo-agent-0.1.141-macos-aarch64");
    std::fs::write(&target, "current").unwrap();
    std::os::unix::fs::symlink(&target, d.join("echo-agent-latest")).unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(
        d.join("echo-agent-latest").exists(),
        "symlink must not be deleted"
    );
    assert!(target.exists(), "current must not be deleted");
}

#[tokio::test]
async fn test_cleanup_old_downloads_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    // Should not panic or error on empty directory.
    make_all_stale(dir.path());

    cleanup_old_downloads(dir.path(), "echo-agent", "0.1.141").await;
}

#[tokio::test]
async fn test_cleanup_old_downloads_version_prefix_collision() {
    // Regression test: version "0.1.14" must not protect "0.1.140", "0.1.141", etc.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    std::fs::write(d.join("echo-agent-0.1.14-macos-aarch64"), "current").unwrap();
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64"), "old-140").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "old-141").unwrap();
    std::fs::write(d.join("echo-agent-0.1.13-macos-aarch64"), "old-13").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.14").await;

    // Current must survive.
    assert!(
        d.join("echo-agent-0.1.14-macos-aarch64").exists(),
        "current 0.1.14"
    );
    // Newest old version (0.1.141) must survive as N-1.
    assert!(
        d.join("echo-agent-0.1.141-macos-aarch64").exists(),
        "N-1 is 0.1.141"
    );
    // 0.1.140 and 0.1.13 should be deleted.
    assert!(
        !d.join("echo-agent-0.1.140-macos-aarch64").exists(),
        "0.1.140 should be deleted"
    );
    assert!(
        !d.join("echo-agent-0.1.13-macos-aarch64").exists(),
        "0.1.13 should be deleted"
    );
}

#[tokio::test]
async fn test_cleanup_old_downloads_pager_multi_version() {
    // Verify cleanup works for echo-agent-pager with multiple old versions.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    for v in ["0.1.148", "0.1.149", "0.1.150"] {
        std::fs::write(d.join(format!("echo-agent-pager-{}-linux-x64", v)), v).unwrap();
    }
    std::fs::write(d.join("echo-agent-pager-0.1.151-linux-x64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent-pager", "0.1.151").await;

    assert!(
        d.join("echo-agent-pager-0.1.151-linux-x64").exists(),
        "current"
    );
    assert!(
        d.join("echo-agent-pager-0.1.150-linux-x64").exists(),
        "N-1 kept"
    );
    assert!(
        !d.join("echo-agent-pager-0.1.149-linux-x64").exists(),
        "0.1.149 deleted"
    );
    assert!(
        !d.join("echo-agent-pager-0.1.148-linux-x64").exists(),
        "0.1.148 deleted"
    );
}

#[tokio::test]
async fn test_cleanup_old_downloads_npm_layout() {
    // npm layout: files are just `echo-agent-{version}` (no platform suffix).
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    for v in ["0.1.138", "0.1.139", "0.1.140"] {
        std::fs::write(d.join(format!("echo-agent-{}", v)), v).unwrap();
    }
    std::fs::write(d.join("echo-agent-0.1.141"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(d.join("echo-agent-0.1.141").exists(), "current");
    assert!(d.join("echo-agent-0.1.140").exists(), "N-1 kept");
    assert!(!d.join("echo-agent-0.1.139").exists(), "0.1.139 deleted");
    assert!(!d.join("echo-agent-0.1.138").exists(), "0.1.138 deleted");
}

#[tokio::test]
async fn test_cleanup_old_downloads_alpha_versions() {
    // Alpha version filenames include pre-release tags:
    //   echo-agent-0.1.150-alpha.1-macos-aarch64
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    std::fs::write(
        d.join("echo-agent-0.1.148-alpha.1-macos-aarch64"),
        "alpha-148-1",
    )
    .unwrap();
    std::fs::write(
        d.join("echo-agent-0.1.148-alpha.2-macos-aarch64"),
        "alpha-148-2",
    )
    .unwrap();
    std::fs::write(
        d.join("echo-agent-0.1.149-alpha.1-macos-aarch64"),
        "alpha-149-1",
    )
    .unwrap();
    // Current version is the newest alpha.
    std::fs::write(
        d.join("echo-agent-0.1.150-alpha.1-macos-aarch64"),
        "current",
    )
    .unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.150-alpha.1").await;

    // Current must survive.
    assert!(
        d.join("echo-agent-0.1.150-alpha.1-macos-aarch64").exists(),
        "current alpha"
    );
    // Newest old (0.1.149-alpha.1) kept as N-1.
    assert!(
        d.join("echo-agent-0.1.149-alpha.1-macos-aarch64").exists(),
        "N-1 alpha"
    );
    // Older alphas deleted.
    assert!(
        !d.join("echo-agent-0.1.148-alpha.2-macos-aarch64").exists(),
        "0.1.148-alpha.2 deleted"
    );
    assert!(
        !d.join("echo-agent-0.1.148-alpha.1-macos-aarch64").exists(),
        "0.1.148-alpha.1 deleted"
    );
}

#[tokio::test]
async fn test_cleanup_old_downloads_mixed_stable_and_alpha() {
    // Mix of stable and alpha binaries in the same directory.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    std::fs::write(d.join("echo-agent-0.1.148-macos-aarch64"), "stable-148").unwrap();
    std::fs::write(
        d.join("echo-agent-0.1.149-alpha.1-macos-aarch64"),
        "alpha-149",
    )
    .unwrap();
    std::fs::write(d.join("echo-agent-0.1.149-macos-aarch64"), "stable-149").unwrap();
    // Current is a stable release.
    std::fs::write(d.join("echo-agent-0.1.150-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.150").await;

    // Current must survive.
    assert!(
        d.join("echo-agent-0.1.150-macos-aarch64").exists(),
        "current"
    );
    // Newest old is 0.1.149 stable (semver: 0.1.149 > 0.1.149-alpha.1).
    assert!(
        d.join("echo-agent-0.1.149-macos-aarch64").exists(),
        "N-1 is stable 0.1.149"
    );
    // The rest should be deleted.
    assert!(
        !d.join("echo-agent-0.1.149-alpha.1-macos-aarch64").exists(),
        "alpha 0.1.149-alpha.1 deleted"
    );
    assert!(
        !d.join("echo-agent-0.1.148-macos-aarch64").exists(),
        "stable 0.1.148 deleted"
    );
}

// ──────────────────────────────────────────────────────────────────────
// reinstall_hint
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_reinstall_hint_npm_mentions_npm_command() {
    let hint = reinstall_hint("npm", "stable");
    assert!(hint.contains("npm i -g"), "should suggest npm i -g: {hint}");
    assert!(
        hint.contains("@echo-agent-official/echoagent"),
        "should name the package: {hint}"
    );
}

#[test]
fn test_reinstall_hint_gh_release_mentions_gh_command() {
    let hint = reinstall_hint("gh-release", "stable");
    assert!(
        hint.contains("gh release download"),
        "should suggest gh release download: {hint}"
    );
    assert!(
        hint.contains("fuyuxiang/echo-agent-desktop"),
        "should name the repo: {hint}"
    );
}

#[test]
fn test_reinstall_hint_internal_mentions_platform_installer() {
    let hint = reinstall_hint("internal", "stable");
    if cfg!(windows) {
        assert!(hint.contains("irm"), "should suggest irm install: {hint}");
        assert!(
            hint.contains("install.ps1"),
            "should reference install.ps1: {hint}"
        );
        assert!(
            !hint.contains("ECHO_AGENT_CHANNEL"),
            "stable must not set channel: {hint}"
        );
    } else {
        assert!(hint.contains("curl"), "should suggest curl install: {hint}");
        assert!(
            hint.contains("install.sh"),
            "should reference install.sh: {hint}"
        );
        assert!(
            !hint.contains("ECHO_AGENT_CHANNEL"),
            "stable must not set channel: {hint}"
        );
    }
}

#[test]
fn test_reinstall_hint_internal_alpha_sets_channel() {
    let hint = reinstall_hint("internal", "alpha");
    if cfg!(windows) {
        assert!(
            hint.contains("$env:ECHO_AGENT_CHANNEL='alpha'"),
            "alpha should set ECHO_AGENT_CHANNEL: {hint}"
        );
    } else {
        assert!(
            hint.contains("| ECHO_AGENT_CHANNEL='alpha' bash"),
            "alpha must set ECHO_AGENT_CHANNEL on bash (the process running \
             install.sh), not curl: {hint}"
        );
    }
}

#[test]
fn test_reinstall_hint_enterprise_uses_enterprise_script() {
    // Enterprise ships via its own bootstrap script (channel hardcoded
    // there), never install.sh + ECHO_AGENT_CHANNEL.
    let hint = reinstall_hint("internal", "enterprise");
    assert!(
        hint.contains("/enterprise-install."),
        "enterprise must use the published enterprise-install script: {hint}"
    );
    assert!(
        !hint.contains("ECHO_AGENT_CHANNEL"),
        "enterprise script needs no channel env: {hint}"
    );
}

#[test]
fn test_reinstall_hint_malformed_channel_falls_back_to_stable() {
    // Free-text config channels never reach the shell one-liner unless
    // they are plain [A-Za-z0-9._-] tokens.
    for bad in ["al pha", "x'; rm -rf ~;'", "a\"b", ""] {
        let hint = reinstall_hint("internal", bad);
        assert!(
            !hint.contains("ECHO_AGENT_CHANNEL"),
            "malformed channel {bad:?} must fall back to stable: {hint}"
        );
    }
}

#[test]
fn test_reinstall_hint_unknown_falls_back_to_internal() {
    // Unknown installer falls back to the same hint as "internal".
    let unknown = reinstall_hint("homebrew", "stable");
    let internal = reinstall_hint("internal", "stable");
    assert_eq!(unknown, internal);
}

#[test]
fn test_reinstall_hint_empty_falls_back_to_internal() {
    let hint = reinstall_hint("", "stable");
    assert_eq!(hint, reinstall_hint("internal", "stable"));
}

#[test]
fn test_smoke_test_failure_messages_distinguish_causes() {
    let timeout = SmokeTestFailure::Timeout.to_string();
    assert!(
        timeout.contains(&format!(
            "timed out after {}s",
            SMOKE_TEST_TIMEOUT.as_secs()
        )),
        "{timeout}"
    );
    let spawn = SmokeTestFailure::Spawn("os error 2".into()).to_string();
    assert!(spawn.contains("could not start: os error 2"), "{spawn}");
    let nz = SmokeTestFailure::NonZero {
        status: "137".into(),
        stderr: "killed".into(),
    }
    .to_string();
    assert!(nz.contains("exited 137"), "{nz}");
    assert!(nz.contains("stderr: killed"), "{nz}");
}

#[test]
fn test_truncate_err() {
    assert_eq!(truncate_err("  short  ", 10), "short");
    assert_eq!(truncate_err("abcdefghij", 8), "abcde...");
}

/// Every failure kind classifies from its typed source; unmarked
/// errors (npm / gh-release) fall through to `Other`.
#[test]
fn test_classify_install_error() {
    let download: anyhow::Error = InstallPhaseError::Download(anyhow::anyhow!("HTTP 404")).into();
    assert_eq!(
        classify_install_error(&download),
        CliUpdateErrorKind::Download
    );
    let activate: anyhow::Error =
        InstallPhaseError::Activate(anyhow::anyhow!("rename failed")).into();
    assert_eq!(
        classify_install_error(&activate),
        CliUpdateErrorKind::Activate
    );
    assert_eq!(
        classify_install_error(&anyhow::anyhow!("npm install failed")),
        CliUpdateErrorKind::Other
    );
    for (smoke, expected) in [
        (SmokeTestFailure::Timeout, CliUpdateErrorKind::SmokeTimeout),
        (
            SmokeTestFailure::Spawn("os error 2".into()),
            CliUpdateErrorKind::SmokeSpawn,
        ),
        (
            SmokeTestFailure::NonZero {
                status: "137".into(),
                stderr: String::new(),
            },
            CliUpdateErrorKind::SmokeNonzero,
        ),
    ] {
        assert_eq!(classify_install_error(&smoke.into()), expected);
    }
}

// ──────────────────────────────────────────────────────────────────────
// UpdateStatus serialization (camelCase contract for --json clients)
// ──────────────────────────────────────────────────────────────────────

fn make_status() -> UpdateStatus {
    UpdateStatus {
        current_version: "0.1.150".to_string(),
        latest_version: Some("0.1.151".to_string()),
        update_available: true,
        installer: Some("npm".to_string()),
        channel: "stable".to_string(),
        auto_update: Some(true),
        error: None,
    }
}

#[test]
fn test_update_status_serializes_camel_case_keys() {
    let s = make_status();
    let v = serde_json::to_value(&s).unwrap();
    let obj = v.as_object().unwrap();
    assert!(obj.contains_key("currentVersion"));
    assert!(obj.contains_key("latestVersion"));
    assert!(obj.contains_key("updateAvailable"));
    assert!(obj.contains_key("installer"));
    assert!(obj.contains_key("channel"));
    assert!(obj.contains_key("autoUpdate"));
    assert!(obj.contains_key("error"));
    // Snake-case names must NOT leak.
    assert!(!obj.contains_key("current_version"));
    assert!(!obj.contains_key("latest_version"));
    assert!(!obj.contains_key("update_available"));
    assert!(!obj.contains_key("auto_update"));
}

#[test]
fn test_update_status_field_values_round_trip_through_json() {
    let s = make_status();
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["currentVersion"], "0.1.150");
    assert_eq!(v["latestVersion"], "0.1.151");
    assert_eq!(v["updateAvailable"], true);
    assert_eq!(v["installer"], "npm");
    assert_eq!(v["channel"], "stable");
    assert_eq!(v["autoUpdate"], true);
    assert!(v["error"].is_null());
}

#[test]
fn test_update_status_optional_none_serializes_to_null() {
    let s = UpdateStatus {
        current_version: "0.1.150".to_string(),
        latest_version: None,
        update_available: false,
        installer: None,
        channel: "stable".to_string(),
        auto_update: None,
        error: None,
    };
    let v = serde_json::to_value(&s).unwrap();
    assert!(v["latestVersion"].is_null());
    assert!(v["installer"].is_null());
    assert!(v["autoUpdate"].is_null());
    assert!(v["error"].is_null());
    assert_eq!(v["updateAvailable"], false);
}

#[test]
fn test_update_status_with_error_field_serialized() {
    let s = UpdateStatus {
        current_version: "0.1.150".to_string(),
        latest_version: None,
        update_available: false,
        installer: Some("npm".to_string()),
        channel: "stable".to_string(),
        auto_update: Some(true),
        error: Some("npm view failed: ENETUNREACH".to_string()),
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["error"], "npm view failed: ENETUNREACH");
}

#[test]
fn test_update_status_alpha_channel_serialized() {
    let s = UpdateStatus {
        current_version: "0.1.150-alpha.1".to_string(),
        latest_version: Some("0.1.150-alpha.2".to_string()),
        update_available: true,
        installer: Some("npm".to_string()),
        channel: "alpha".to_string(),
        auto_update: Some(true),
        error: None,
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["channel"], "alpha");
    assert_eq!(v["currentVersion"], "0.1.150-alpha.1");
    assert_eq!(v["latestVersion"], "0.1.150-alpha.2");
}

#[test]
fn test_update_status_json_is_valid_single_object() {
    // Whatever we add to UpdateStatus in the future, the serialization
    // must remain a single JSON object (not an array, primitive, etc.).
    let s = make_status();
    let json = serde_json::to_string(&s).unwrap();
    assert!(json.starts_with('{'), "must be a JSON object: {json}");
    assert!(json.ends_with('}'), "must be a JSON object: {json}");
    // Single line: no embedded newlines (the wire format is one line).
    assert!(!json.contains('\n'), "must be single line: {json}");
}

// ──────────────────────────────────────────────────────────────────────
// print_update_status — exercise both code paths via JSON serialization
// (the human path writes to stdout/stderr which is hard to capture
//  without altering the function signature).
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_print_update_status_json_returns_ok() {
    let s = make_status();
    // We can't easily capture stdout, but we can confirm the function
    // doesn't panic or return Err on a well-formed status.
    print_update_status(&s, true).unwrap();
}

#[test]
fn test_print_update_status_human_returns_ok_when_update_available() {
    let s = make_status();
    print_update_status(&s, false).unwrap();
}

#[test]
fn test_print_update_status_human_returns_ok_when_no_installer() {
    let s = UpdateStatus {
        current_version: "0.1.150".to_string(),
        latest_version: None,
        update_available: false,
        installer: None,
        channel: "stable".to_string(),
        auto_update: None,
        error: None,
    };
    print_update_status(&s, false).unwrap();
}

#[test]
fn test_print_update_status_human_returns_ok_with_error() {
    let s = UpdateStatus {
        current_version: "0.1.150".to_string(),
        latest_version: None,
        update_available: false,
        installer: Some("npm".to_string()),
        channel: "stable".to_string(),
        auto_update: Some(true),
        error: Some("network down".to_string()),
    };
    print_update_status(&s, false).unwrap();
}

#[test]
fn test_print_update_status_human_returns_ok_when_up_to_date() {
    let s = UpdateStatus {
        current_version: "0.1.150".to_string(),
        latest_version: Some("0.1.150".to_string()),
        update_available: false,
        installer: Some("npm".to_string()),
        channel: "stable".to_string(),
        auto_update: Some(true),
        error: None,
    };
    print_update_status(&s, false).unwrap();
}

// ──────────────────────────────────────────────────────────────────────
// needs_update — additional edge cases
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_needs_update_empty_current_returns_none() {
    assert_eq!(needs_update("", "0.1.141", "stable", false), None);
}

#[test]
fn test_needs_update_empty_latest_returns_none() {
    assert_eq!(needs_update("0.1.141", "", "stable", false), None);
}

#[test]
fn test_needs_update_whitespace_returns_none() {
    // Leading/trailing whitespace is not stripped — semver::parse rejects.
    assert_eq!(needs_update("  0.1.141", "0.1.142", "stable", false), None);
    assert_eq!(needs_update("0.1.141", "0.1.142  ", "stable", false), None);
}

#[test]
fn test_needs_update_channel_is_case_sensitive() {
    // "STABLE", "Stable", "ENTERPRISE" etc. are not recognized — must be exact lowercase.
    assert_eq!(needs_update("0.1.140", "0.1.141", "STABLE", false), None);
    assert_eq!(needs_update("0.1.140", "0.1.141", "Stable", false), None);
    assert_eq!(needs_update("0.1.140", "0.1.141", "ALPHA", false), None);
    assert_eq!(
        needs_update("0.1.140", "0.1.141", "ENTERPRISE", false),
        None
    );
}

#[test]
fn test_needs_update_unknown_channels_return_none() {
    // Unknown channels (not stable/alpha/enterprise) return None.
    assert_eq!(needs_update("0.1.140", "0.1.141", "beta", false), None);
    assert_eq!(needs_update("0.1.140", "0.1.141", "nightly", false), None);
    assert_eq!(needs_update("0.1.140", "0.1.141", "", false), None);
    assert_eq!(needs_update("0.1.140", "0.1.141", "rc", false), None);
    // Enterprise is explicitly supported (behaves like stable).
    assert_eq!(
        needs_update("0.1.140", "0.1.141", "enterprise", false),
        Some(true)
    );
    // Unknown channels return None regardless of allow_downgrade.
    assert_eq!(needs_update("0.1.140", "0.1.141", "beta", true), None);
    assert_eq!(needs_update("0.1.140", "0.1.141", "", true), None);
}

#[test]
fn test_needs_update_zero_versions() {
    assert_eq!(needs_update("0.0.0", "0.0.1", "stable", false), Some(true));
    assert_eq!(needs_update("0.0.0", "0.0.0", "stable", false), Some(false));
}

#[test]
fn test_needs_update_major_version_jump() {
    assert_eq!(needs_update("0.9.99", "1.0.0", "stable", false), Some(true));
    assert_eq!(
        needs_update("1.99.99", "2.0.0", "stable", false),
        Some(true)
    );
    // Major downgrade: not an upgrade (allow_downgrade=false).
    assert_eq!(
        needs_update("2.0.0", "1.99.99", "stable", false),
        Some(false)
    );
}

#[test]
fn test_needs_update_alpha_to_alpha_same_version_not_upgrade() {
    assert_eq!(
        needs_update("0.1.150-alpha.5", "0.1.150-alpha.5", "alpha", false),
        Some(false)
    );
}

#[test]
fn test_needs_update_alpha_to_beta_same_base_is_upgrade_per_semver() {
    // semver: alpha.5 < beta.1 (lexicographic on identifiers per spec)
    assert_eq!(
        needs_update("0.1.150-alpha.5", "0.1.150-beta.1", "alpha", false),
        Some(true)
    );
}

#[test]
fn test_needs_update_with_build_metadata_uses_semver_crate_ordering() {
    // SUBTLE: per the semver SPEC, build metadata (after `+`) MUST be
    // ignored when determining version precedence. However the `semver`
    // crate's `PartialOrd` impl compares build metadata lexicographically
    // for differing values. So `0.1.141+xyz > 0.1.141+abc` returns true
    // here even though spec-wise they are equal.
    //
    // This means CI publishers MUST NOT publish multiple builds of the
    // same version differing only in build metadata, or auto-update will
    // bounce users between them. Today our pipeline doesn't, so this is
    // latent — but the test locks in the surprising behavior so it can't
    // change silently.
    assert_eq!(
        needs_update("0.1.141+abc", "0.1.141+xyz", "stable", false),
        Some(true),
        "semver crate orders by build metadata lexicographically (contra spec)"
    );
    // No build metadata vs with build metadata: semver crate treats
    // a version with build > the same version without it.
    assert_eq!(
        needs_update("0.1.141", "0.1.141+abc", "stable", false),
        Some(true)
    );
}

#[test]
fn test_needs_update_partial_versions_rejected() {
    assert_eq!(needs_update("0.1", "0.1.141", "stable", false), None);
    assert_eq!(needs_update("0", "0.1.141", "stable", false), None);
    assert_eq!(needs_update("0.1.141", "1", "stable", false), None);
}

#[test]
fn test_needs_update_alpha_channel_with_invalid_versions_returns_none() {
    // Same parse-failure behavior on alpha as stable.
    assert_eq!(needs_update("garbage", "0.1.141", "alpha", false), None);
    assert_eq!(needs_update("0.1.141", "garbage", "alpha", false), None);
}

#[test]
fn test_needs_update_alpha_channel_treats_release_as_higher_than_prerelease() {
    // On alpha channel, a release version is semver-higher than its
    // matching pre-release: 0.1.150 > 0.1.150-alpha.99.
    assert_eq!(
        needs_update("0.1.150-alpha.99", "0.1.150", "alpha", false),
        Some(true)
    );
}

#[test]
fn test_needs_update_stable_does_not_install_when_pre_and_pre() {
    // current is pre-release, latest is also pre-release on stable channel:
    // latest is rejected as pre-release, so no install.
    assert_eq!(
        needs_update("0.1.150-alpha.1", "0.1.151-alpha.1", "stable", false),
        Some(false)
    );
}

// ──────────────────────────────────────────────────────────────────────
// needs_update — allow_downgrade=true (rollback support)
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_needs_update_downgrade_stable_when_allowed() {
    // Rollback scenario: stable pointer moved from 0.2.7 → 0.2.5.
    // GCS/internal installer: allow_downgrade=true → triggers update.
    assert_eq!(needs_update("0.2.7", "0.2.5", "stable", true), Some(true));
}

#[test]
fn test_needs_update_downgrade_stable_blocked_when_disallowed() {
    // Same rollback scenario but npm installer: allow_downgrade=false → no update.
    assert_eq!(needs_update("0.2.7", "0.2.5", "stable", false), Some(false));
}

#[test]
fn test_needs_update_downgrade_alpha_when_allowed() {
    // Alpha rollback: pointer moved backward.
    assert_eq!(needs_update("0.2.7", "0.2.5", "alpha", true), Some(true));
    // Alpha pre-release downgrade.
    assert_eq!(
        needs_update("0.1.148-alpha.3", "0.1.148-alpha.2", "alpha", true),
        Some(true)
    );
}

#[test]
fn test_needs_update_downgrade_enterprise_when_allowed() {
    assert_eq!(
        needs_update("0.1.207", "0.1.206", "enterprise", true),
        Some(true)
    );
}

#[test]
fn test_needs_update_same_version_unaffected_by_allow_downgrade() {
    // Same version → no update regardless of allow_downgrade setting.
    assert_eq!(needs_update("0.2.5", "0.2.5", "stable", true), Some(false));
    assert_eq!(needs_update("0.2.5", "0.2.5", "stable", false), Some(false));
    assert_eq!(needs_update("0.2.5", "0.2.5", "alpha", true), Some(false));
}

#[test]
fn test_needs_update_upgrade_unaffected_by_allow_downgrade() {
    // Upgrade works regardless of allow_downgrade setting.
    assert_eq!(needs_update("0.2.5", "0.2.7", "stable", true), Some(true));
    assert_eq!(needs_update("0.2.5", "0.2.7", "stable", false), Some(true));
    assert_eq!(needs_update("0.2.5", "0.2.7", "alpha", true), Some(true));
    assert_eq!(needs_update("0.2.5", "0.2.7", "alpha", false), Some(true));
}

#[test]
fn test_needs_update_downgrade_major_version_when_allowed() {
    // Major version downgrade (e.g. v2 → v1 rollback).
    assert_eq!(needs_update("2.0.0", "1.99.99", "stable", true), Some(true));
}

#[test]
fn test_needs_update_downgrade_prerelease_still_rejected_on_stable() {
    // Even with allow_downgrade=true, pre-release targets are rejected on
    // stable/enterprise channels (safety net).
    assert_eq!(
        needs_update("0.2.7", "0.2.5-alpha.1", "stable", true),
        Some(false)
    );
    assert_eq!(
        needs_update("0.2.7", "0.2.5-alpha.1", "enterprise", true),
        Some(false)
    );
}

#[test]
fn test_needs_update_prerelease_current_forces_install_regardless_of_allow_downgrade() {
    // Pre-release current on stable channel → force-install, independent
    // of allow_downgrade.
    assert_eq!(
        needs_update("0.1.149-alpha.1", "0.1.148", "stable", true),
        Some(true)
    );
    assert_eq!(
        needs_update("0.1.149-alpha.1", "0.1.148", "stable", false),
        Some(true)
    );
}

// ──────────────────────────────────────────────────────────────────────
// installer_allows_downgrade
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_installer_allows_downgrade_internal() {
    assert!(installer_allows_downgrade("internal"));
}

#[test]
fn test_installer_allows_downgrade_gh_release() {
    assert!(installer_allows_downgrade("gh-release"));
}

#[test]
fn test_installer_allows_downgrade_npm_blocked() {
    // npm registries can return stale/misconfigured versions — no downgrade.
    assert!(!installer_allows_downgrade("npm"));
}

#[test]
fn test_installer_allows_downgrade_unknown_blocked() {
    assert!(!installer_allows_downgrade("unknown"));
    assert!(!installer_allows_downgrade(""));
    assert!(!installer_allows_downgrade("homebrew"));
}

// ──────────────────────────────────────────────────────────────────────
// detect_platform
// ──────────────────────────────────────────────────────────────────────

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn test_detect_platform_returns_known_os() {
    let (os, arch) = detect_platform().unwrap();
    assert!(
        os == "macos" || os == "linux" || os == "windows",
        "got os={os}"
    );
    assert!(arch == "x86_64" || arch == "aarch64", "got arch={arch}");
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn test_detect_platform_matches_compile_time_cfg() {
    let (os, arch) = detect_platform().unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(os, "macos");
    }
    if cfg!(target_os = "linux") {
        assert_eq!(os, "linux");
    }
    if cfg!(target_os = "windows") {
        assert_eq!(os, "windows");
    }
    if cfg!(target_arch = "x86_64") {
        // The one intentional divergence from compile-time cfg: an
        // x86_64 test binary running under Rosetta selects aarch64.
        if os == "macos" && running_under_rosetta_on_apple_silicon() {
            assert_eq!(arch, "aarch64");
        } else {
            assert_eq!(arch, "x86_64");
        }
    }
    if cfg!(target_arch = "aarch64") {
        assert_eq!(arch, "aarch64");
    }
}

/// Rosetta correction applies exactly to macos/x86_64 on Apple Silicon;
/// every other (os, arch, host) combination keeps the compile-time arch.
#[test]
fn test_corrected_arch() {
    assert_eq!(corrected_arch("macos", "x86_64", true), "aarch64");
    assert_eq!(corrected_arch("macos", "x86_64", false), "x86_64");
    assert_eq!(corrected_arch("macos", "aarch64", true), "aarch64");
    assert_eq!(corrected_arch("linux", "x86_64", true), "x86_64");
    assert_eq!(corrected_arch("windows", "x86_64", true), "x86_64");
}

// ──────────────────────────────────────────────────────────────────────
// cleanup_old_downloads — additional edge cases
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cleanup_old_downloads_invalid_current_version_is_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64"), "v140").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "v141").unwrap();

    // Invalid version string → cleanup must early-return without deleting.
    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "not-a-version").await;
    assert!(d.join("echo-agent-0.1.140-macos-aarch64").exists());
    assert!(d.join("echo-agent-0.1.141-macos-aarch64").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_missing_dir_no_panic() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    // Must not panic when the directory doesn't exist.
    cleanup_old_downloads(&missing, "echo-agent", "0.1.141").await;
}

#[tokio::test]
async fn test_cleanup_old_downloads_files_with_non_digit_suffix_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Files matching prefix but with a non-digit-leading suffix must be
    // ignored (e.g. echo-agent-latest and echo-agent-pager-* when the artifact
    // prefix is echo-agent).
    std::fs::write(d.join("echo-agent-latest"), "alias").unwrap();
    std::fs::write(d.join("echo-agent-pager-0.1.141-macos-aarch64"), "pager").unwrap();
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64"), "v140").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    // echo-agent-latest and echo-agent-pager-* must be untouched.
    assert!(d.join("echo-agent-latest").exists());
    assert!(d.join("echo-agent-pager-0.1.141-macos-aarch64").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_unparseable_version_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Files with prefix + digit but unparseable as semver are ignored
    // (not deleted, not counted).
    std::fs::write(d.join("echo-agent-9garbage-macos-aarch64"), "junk").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(
        d.join("echo-agent-9garbage-macos-aarch64").exists(),
        "unparseable file must be ignored, not deleted"
    );
    assert!(d.join("echo-agent-0.1.141-macos-aarch64").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_only_current_present_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(d.join("echo-agent-0.1.141-macos-aarch64").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_only_one_old_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64"), "v140").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    // Only one old version → keep it as N-1.
    assert!(
        d.join("echo-agent-0.1.140-macos-aarch64").exists(),
        "N-1 kept"
    );
    assert!(
        d.join("echo-agent-0.1.141-macos-aarch64").exists(),
        "current"
    );
}

#[tokio::test]
async fn test_cleanup_old_downloads_unrelated_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Files that don't start with the prefix must never be touched.
    std::fs::write(d.join("README.md"), "readme").unwrap();
    std::fs::write(d.join("config.toml"), "config").unwrap();
    std::fs::write(d.join("other-tool-0.1.0"), "other").unwrap();
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64"), "v140").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(d.join("README.md").exists());
    assert!(d.join("config.toml").exists());
    assert!(d.join("other-tool-0.1.0").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_multiplatform_in_same_dir() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Same version, multiple platforms (uncommon, but possible).
    // Both should be considered "current" via the version equality check.
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "mac").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-linux-x86_64"), "linux").unwrap();
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64"), "old-mac").unwrap();
    std::fs::write(d.join("echo-agent-0.1.139-macos-aarch64"), "older-mac").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    // Both platform variants of current must survive.
    assert!(d.join("echo-agent-0.1.141-macos-aarch64").exists());
    assert!(d.join("echo-agent-0.1.141-linux-x86_64").exists());
    // N-1 (0.1.140) kept, older deleted.
    assert!(d.join("echo-agent-0.1.140-macos-aarch64").exists());
    assert!(!d.join("echo-agent-0.1.139-macos-aarch64").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_tmp_files_deleted_even_when_unparseable() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Stale tmp files are deleted regardless of version-parseability.
    std::fs::write(d.join("echo-agent-junk.tmp"), "partial").unwrap();
    make_stale(&d.join("echo-agent-junk.tmp"));
    std::fs::write(d.join("echo-agent-0.1.140-macos-aarch64.tmp"), "partial2").unwrap();
    make_stale(&d.join("echo-agent-0.1.140-macos-aarch64.tmp"));
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(!d.join("echo-agent-junk.tmp").exists(), "junk tmp deleted");
    assert!(
        !d.join("echo-agent-0.1.140-macos-aarch64.tmp").exists(),
        "versioned tmp deleted"
    );
    assert!(d.join("echo-agent-0.1.141-macos-aarch64").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_three_olds_keeps_only_newest() {
    // Regression: keep exactly N-1, not N-2 or older.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for v in ["0.1.138", "0.1.139", "0.1.140"] {
        std::fs::write(d.join(format!("echo-agent-{}-macos-aarch64", v)), v).unwrap();
    }
    std::fs::write(d.join("echo-agent-0.1.141-macos-aarch64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(
        d.join("echo-agent-0.1.141-macos-aarch64").exists(),
        "current"
    );
    assert!(
        d.join("echo-agent-0.1.140-macos-aarch64").exists(),
        "N-1 only"
    );
    assert!(!d.join("echo-agent-0.1.139-macos-aarch64").exists());
    assert!(!d.join("echo-agent-0.1.138-macos-aarch64").exists());
}

#[tokio::test]
async fn test_cleanup_old_downloads_darwin_platform_recognized() {
    // The `darwin` alias for macOS is in PLATFORM_OS — versions on
    // echo-agent-X.Y.Z-darwin-* layouts must split correctly.
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("echo-agent-0.1.140-darwin-arm64"), "v140").unwrap();
    std::fs::write(d.join("echo-agent-0.1.141-darwin-arm64"), "current").unwrap();

    make_all_stale(d);

    cleanup_old_downloads(d, "echo-agent", "0.1.141").await;

    assert!(
        d.join("echo-agent-0.1.141-darwin-arm64").exists(),
        "current"
    );
    assert!(d.join("echo-agent-0.1.140-darwin-arm64").exists(), "N-1");
}

// ──────────────────────────────────────────────────────────────────────
// ──────────────────────────────────────────────────────────────────────
// UpdateRunMode
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_update_run_mode_is_copy_clone_debug() {
    // The ergonomic Copy/Clone/Debug derives must not regress: we pass
    // `run_mode` by value through several layers.
    let m1 = UpdateRunMode::Blocking;
    let m2 = m1; // Copy
    let m3 = m1; // Copy again, m1 not moved
    assert!(matches!(m1, UpdateRunMode::Blocking));
    assert!(matches!(m2, UpdateRunMode::Blocking));
    assert!(matches!(m3, UpdateRunMode::Blocking));
    // Debug exists.
    let _ = format!("{:?}", UpdateRunMode::NonBlocking);
}

// ──────────────────────────────────────────────────────────────────────
// Constants — lock them in so silent renames are caught.
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_user_facing_constants_are_stable() {
    assert_eq!(PROMPT_UPDATE_NOW, "Update now? [Y/n/d]");
    assert_eq!(
        MSG_AUTO_UPDATE_BACKGROUND,
        "Auto-update running in background."
    );
    assert_eq!(
        MSG_RUN_UPDATE_MANUAL,
        "Run `echoagent update` to get the latest version."
    );
}

// ──────────────────────────────────────────────────────────────────────
// env_installer — env-var based, must run serially.
//
// Resolution order (matches function body):
//   1. ECHO_AGENT_INSTALLER (npm | internal | gh-release | gh)
//   2. ECHO_AGENT_MANAGED_BY_NPM       → npm
//   3. ECHO_AGENT_MANAGED_BY_INTERNAL  → internal
//   4. npm_config_user_agent      → npm
//   5. None
// ──────────────────────────────────────────────────────────────────────

/// Snapshot every installer-related env var so the test can clear them
/// at start and restore them at end. Without this, a parent shell that
/// sets e.g. `npm_config_user_agent` (which happens whenever you run via
/// `npm run`) silently makes every "no env vars" test misbehave.
struct InstallerEnvGuard {
    prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl InstallerEnvGuard {
    fn isolate() -> Self {
        const VARS: &[&str] = &[
            "ECHO_AGENT_INSTALLER",
            "ECHO_AGENT_MANAGED_BY_NPM",
            "ECHO_AGENT_MANAGED_BY_INTERNAL",
            "npm_config_user_agent",
            "NPM_TOKEN",
        ];
        let prev: Vec<_> = VARS.iter().map(|k| (*k, std::env::var_os(k))).collect();
        unsafe {
            for k in VARS {
                std::env::remove_var(k);
            }
        }
        Self { prev }
    }
}

impl Drop for InstallerEnvGuard {
    fn drop(&mut self) {
        unsafe {
            for (k, v) in &self.prev {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

#[test]
#[serial_test::serial]
fn test_env_installer_no_vars_returns_none() {
    let _g = InstallerEnvGuard::isolate();
    assert_eq!(env_installer(), None);
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_npm() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "npm") };
    assert_eq!(env_installer(), Some("npm"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_internal() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "internal") };
    assert_eq!(env_installer(), Some("internal"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_gh_release() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "gh-release") };
    assert_eq!(env_installer(), Some("gh-release"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_gh_alias() {
    // `gh` is shorthand for `gh-release`.
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "gh") };
    assert_eq!(env_installer(), Some("gh-release"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_uppercase_normalized() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "NPM") };
    assert_eq!(env_installer(), Some("npm"));

    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "Gh-Release") };
    assert_eq!(env_installer(), Some("gh-release"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_unknown_value_returns_none() {
    // CRITICAL: when the explicit env var is set to something we don't
    // recognize, we early-return None. This means we do NOT fall through
    // to the other env vars or to config. So `ECHO_AGENT_INSTALLER=brew`
    // disables the env-installer detection entirely.
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "brew") };
    // Even if MANAGED_BY_NPM is also set, the explicit var wins (and rejects).
    unsafe { std::env::set_var("ECHO_AGENT_MANAGED_BY_NPM", "1") };
    assert_eq!(
        env_installer(),
        None,
        "explicit ECHO_AGENT_INSTALLER=brew must early-return None, not fall through"
    );
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_empty_returns_none() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "") };
    assert_eq!(env_installer(), None);
}

#[test]
#[serial_test::serial]
fn test_env_installer_managed_by_npm() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_MANAGED_BY_NPM", "1") };
    assert_eq!(env_installer(), Some("npm"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_managed_by_npm_any_value() {
    // The check is `is_some` — any value (including empty) wins.
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_MANAGED_BY_NPM", "") };
    assert_eq!(env_installer(), Some("npm"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_managed_by_internal() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("ECHO_AGENT_MANAGED_BY_INTERNAL", "1") };
    assert_eq!(env_installer(), Some("internal"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_npm_config_user_agent_implies_npm() {
    // npm sets npm_config_user_agent in the env of any process it spawns.
    // The trampoline relies on this fallback when MANAGED_BY_NPM was lost.
    let _g = InstallerEnvGuard::isolate();
    unsafe {
        std::env::set_var(
            "npm_config_user_agent",
            "npm/10.2.0 node/v20.11.0 darwin arm64 workspaces/false",
        )
    };
    assert_eq!(env_installer(), Some("npm"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_managed_by_npm_wins_over_npm_config_user_agent() {
    // Both set: the order in env_installer is MANAGED_BY_NPM checked first,
    // so MANAGED_BY_NPM wins. (Result is the same — both → npm — but the
    // resolution path matters for future maintainers.)
    let _g = InstallerEnvGuard::isolate();
    unsafe {
        std::env::set_var("ECHO_AGENT_MANAGED_BY_NPM", "1");
        std::env::set_var("npm_config_user_agent", "npm/10");
    }
    assert_eq!(env_installer(), Some("npm"));
}

#[test]
#[serial_test::serial]
fn test_env_installer_explicit_internal_wins_over_npm_managed() {
    // ECHO_AGENT_INSTALLER=internal must override an inherited MANAGED_BY_NPM.
    let _g = InstallerEnvGuard::isolate();
    unsafe {
        std::env::set_var("ECHO_AGENT_INSTALLER", "internal");
        std::env::set_var("ECHO_AGENT_MANAGED_BY_NPM", "1");
    }
    assert_eq!(env_installer(), Some("internal"));
}

// ──────────────────────────────────────────────────────────────────────
// create_temp_npmrc — also env-var based (NPM_TOKEN), must run serially.
// ──────────────────────────────────────────────────────────────────────

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_no_token_returns_none() {
    let _g = InstallerEnvGuard::isolate();
    let result = create_temp_npmrc(None).unwrap();
    assert!(result.is_none(), "no NPM_TOKEN must yield None");
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_empty_token_returns_none() {
    // An empty token is not a real token — must not write a file.
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "") };
    let result = create_temp_npmrc(None).unwrap();
    assert!(result.is_none(), "empty NPM_TOKEN must yield None");
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_whitespace_only_token_returns_none() {
    // Whitespace-only is treated as empty after trim.
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "   \t\n  ") };
    let result = create_temp_npmrc(None).unwrap();
    assert!(result.is_none(), "whitespace NPM_TOKEN must yield None");
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_default_registry() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "secret123") };
    let path = create_temp_npmrc(None).unwrap().expect("file written");
    let body = std::fs::read_to_string(&path).unwrap();

    assert!(
        body.contains("registry.npmjs.org"),
        "default registry: {body}"
    );
    assert!(body.contains("_authToken=secret123"), "token: {body}");
    assert!(body.starts_with("//"), "must be // prefix: {body}");
    assert!(body.ends_with('\n'), "must end with newline: {body}");

    let _ = std::fs::remove_file(&path);
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_token_trimmed() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "  padded-token  ") };
    let path = create_temp_npmrc(None).unwrap().expect("file written");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("_authToken=padded-token"),
        "token must be trimmed: {body}"
    );
    assert!(
        !body.contains("padded-token  "),
        "trailing whitespace must be stripped: {body}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_custom_registry_extracts_host_and_path() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "tok") };
    let path = create_temp_npmrc(Some("https://npm.example.com/repository/npm/"))
        .unwrap()
        .expect("file written");
    let body = std::fs::read_to_string(&path).unwrap();

    // Host + path must be preserved (trailing slash stripped per impl).
    assert!(
        body.contains("npm.example.com/repository/npm"),
        "registry host+path: {body}"
    );
    assert!(body.contains("_authToken=tok"));

    let _ = std::fs::remove_file(&path);
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_custom_registry_with_port() {
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "tok") };
    let path = create_temp_npmrc(Some("https://npm.example.com:8443/"))
        .unwrap()
        .expect("file written");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("npm.example.com:8443"),
        "port must be preserved: {body}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_invalid_registry_url_falls_back_to_default() {
    // If the registry string doesn't parse as a URL, fall back to the
    // public npm host so the auth token isn't silently lost.
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "tok") };
    let path = create_temp_npmrc(Some("not a url"))
        .unwrap()
        .expect("file written");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("registry.npmjs.org"),
        "invalid URL falls back: {body}"
    );
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_file_perms_are_0600() {
    // The file contains an auth token — must be readable only by owner.
    use std::os::unix::fs::PermissionsExt;
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "secret") };
    let path = create_temp_npmrc(None).unwrap().expect("file written");

    let perms = std::fs::metadata(&path).unwrap().permissions();
    let mode = perms.mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "npmrc must be 0600 to protect the auth token, got {mode:o}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
#[serial_test::serial]
fn test_create_temp_npmrc_unique_path_per_pid() {
    // Two parallel installs would clobber each other if the path didn't
    // include the PID. Verify the filename includes the current PID.
    let _g = InstallerEnvGuard::isolate();
    unsafe { std::env::set_var("NPM_TOKEN", "tok") };
    let path = create_temp_npmrc(None).unwrap().expect("file written");
    let pid = std::process::id().to_string();
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        name.contains(&pid),
        "filename should include PID: {name} (pid={pid})"
    );
    let _ = std::fs::remove_file(&path);
}

// ──────────────────────────────────────────────────────────────────────
// windows_replace_exe — runs only on Windows CI
// ──────────────────────────────────────────────────────────────────────

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_creates_dest_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new-binary.exe");
    std::fs::write(&src, "new content").unwrap();
    let dest = dir.path().join("echoagent.exe");

    windows_replace_exe(&src, &dest).await.unwrap();

    assert!(dest.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"new content");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_overwrites_unlocked_dest() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new-binary.exe");
    std::fs::write(&src, "new content").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "old content").unwrap();

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), b"new content");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_preserves_binary_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let body: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let src = dir.path().join("binary.exe");
    std::fs::write(&src, &body).unwrap();
    let dest = dir.path().join("echoagent.exe");

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_cleans_stale_old_backup() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new.exe");
    std::fs::write(&src, "new").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "current").unwrap();
    let old = dir.path().join("echoagent.exe.old");
    std::fs::write(&old, "stale-from-prior-update").unwrap();

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
    assert!(!old.exists(), "stale .old must be removed");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_no_filename_returns_err() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.exe");
    std::fs::write(&src, "data").unwrap();

    let bad_dest = dir.path().join("..");
    let err = windows_replace_exe(&src, &bad_dest).await.unwrap_err();
    assert!(format!("{err:#}").contains("no filename"), "error: {err:#}");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_locked_file_renames_aside() {
    // Simulate a running .exe: blocks writes but allows rename.
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x00000001;
    const FILE_SHARE_DELETE: u32 = 0x00000004;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new.exe");
    std::fs::write(&src, "updated binary").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "running binary").unwrap();

    let _lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(&dest)
        .unwrap();

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "updated binary");

    let old = dir.path().join("echoagent.exe.old");
    assert!(old.exists(), ".old must exist after rename fallback");
    drop(_lock);
    assert_eq!(std::fs::read_to_string(&old).unwrap(), "running binary");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_rollback_on_copy_failure() {
    // No stale .old: the aside IS echoagent.exe.old, so this pins the
    // non-diverted rollback branch (rename .old back onto dest).
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x00000001;
    const FILE_SHARE_DELETE: u32 = 0x00000004;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new.exe");
    std::fs::write(&src, "updated binary").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "original").unwrap();

    // Dest locked like a running exe: blocks writes but allows rename.
    let _dest_lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(&dest)
        .unwrap();
    // Exclusive src lock: both copies fail with a sharing violation, so
    // the rename runs and the second copy triggers the rollback.
    let _src_lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&src)
        .unwrap();

    let result = windows_replace_exe(&src, &dest).await;
    drop(_src_lock);
    drop(_dest_lock);

    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "original",
        "rollback must restore the original binary"
    );
    let old = dir.path().join("echoagent.exe.old");
    assert!(!old.exists(), "rollback must consume the .old aside");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_idempotent_same_content() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("binary.exe");
    std::fs::write(&src, "same content").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "same content").unwrap();

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "same content");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_empty_binary() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("empty.exe");
    std::fs::write(&src, b"").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "non-empty").unwrap();

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::metadata(&dest).unwrap().len(), 0);
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_locked_stale_old_does_not_block_update() {
    // A leftover .old can still be a running image (the session live
    // during the previous update): undeletable, so the rename must
    // divert to a unique aside instead of failing on the locked name.
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x00000001;
    const FILE_SHARE_DELETE: u32 = 0x00000004;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new.exe");
    std::fs::write(&src, "updated binary").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "running binary").unwrap();
    let old = dir.path().join("echoagent.exe.old");
    std::fs::write(&old, "previous binary").unwrap();

    // No FILE_SHARE_DELETE: .old cannot be deleted or rename-replaced.
    let _old_lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&old)
        .unwrap();
    // Dest locked like a running exe: blocks writes but allows rename.
    let _dest_lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(&dest)
        .unwrap();

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "updated binary");
    assert_eq!(
        std::fs::read_to_string(&old).unwrap(),
        "previous binary",
        "locked .old must be left in place"
    );
    let asides: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("echoagent.exe.old.") && n.ends_with(".old"))
        })
        .collect();
    assert_eq!(
        asides.len(),
        1,
        "dest must be renamed to a unique aside: {asides:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&asides[0]).unwrap(),
        "running binary"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_rollback_restores_from_diverted_aside() {
    // Copy failure after a divert must roll dest back from the unique
    // aside, not the hardcoded .old (which still holds the locked image).
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x00000001;
    const FILE_SHARE_DELETE: u32 = 0x00000004;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new.exe");
    std::fs::write(&src, "updated binary").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "running binary").unwrap();
    let old = dir.path().join("echoagent.exe.old");
    std::fs::write(&old, "previous binary").unwrap();

    // No FILE_SHARE_DELETE: .old survives the sweep and forces a divert.
    let _old_lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&old)
        .unwrap();
    // Dest locked like a running exe: blocks writes but allows rename.
    let _dest_lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(&dest)
        .unwrap();
    // Exclusive src lock: both copies fail with a sharing violation, so
    // the rename dance runs and the second copy triggers the rollback.
    let _src_lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&src)
        .unwrap();

    let result = windows_replace_exe(&src, &dest).await;

    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "running binary",
        "rollback must restore dest from the diverted aside"
    );
    assert_eq!(std::fs::read_to_string(&old).unwrap(), "previous binary");
    let leftover_asides = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("echoagent.exe.old.") && name.ends_with(".old")
        })
        .count();
    assert_eq!(leftover_asides, 0, "rollback must consume the aside");
}

#[cfg(windows)]
#[tokio::test]
async fn test_windows_replace_exe_sweeps_accumulated_asides() {
    // Asides pile up while superseded sessions keep running; a later
    // update must collect the no-longer-locked ones — but never another
    // executable's leftovers.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("new.exe");
    std::fs::write(&src, "new").unwrap();
    let dest = dir.path().join("echoagent.exe");
    std::fs::write(&dest, "current").unwrap();
    let old = dir.path().join("echoagent.exe.old");
    std::fs::write(&old, "stale").unwrap();
    let aside_a = dir.path().join("echoagent.exe.old.1234-0.old");
    let aside_b = dir.path().join("echoagent.exe.old.1234-1.old");
    std::fs::write(&aside_a, "aside-a").unwrap();
    std::fs::write(&aside_b, "aside-b").unwrap();
    let agent_old = dir.path().join("agent.exe.old");
    std::fs::write(&agent_old, "agent-old").unwrap();

    windows_replace_exe(&src, &dest).await.unwrap();

    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
    assert!(!old.exists(), "legacy .old must be swept");
    assert!(!aside_a.exists(), "aside must be swept");
    assert!(!aside_b.exists(), "aside must be swept");
    assert!(
        agent_old.exists(),
        "other executables' leftovers must be untouched"
    );
}
