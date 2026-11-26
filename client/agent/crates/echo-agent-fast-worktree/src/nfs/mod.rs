//! Grove worktree strategy (macOS NFS / Linux FUSE): IPC client, fallback,
//! removal, pin-GC.
//!
//! Dest probes and teardown (`dest_is_*`, `force_unmount`) are shared by NFS
//! and FUSE. Create IPC wire types stay local (daemon owns attach).
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod client;
mod confined;
pub(crate) use confined::is_safe_worktree_id;
pub mod create_latency_stamp;
#[cfg_attr(not(feature = "metadata"), allow(dead_code))]
mod liveness;
mod mount_table;
mod remove;
pub use client::{
    CleanArtifactsReply, DetachReply, NfsAdopted, NfsCreateDecision, NfsStatusView, NfsTryError,
    NfsWorktreeClient, SalvageReply,
};
pub use liveness::WORKTREE_BACKING_DIR;
#[cfg(feature = "metadata")]
pub(crate) use liveness::candidate_data_dirs;
#[cfg(feature = "metadata")]
#[allow(unused_imports)]
pub use liveness::{
    NfsIdentity, PIN_GC_GRACE_SECS, RANK_DB, collect_identities, gc_orphan_pins,
    identities_from_worktree_records, merge_nfs_identities, nfs_record_is_dead,
};
#[allow(unused_imports)]
pub use mount_table::{
    dest_is_known_unmounted, dest_is_mountpoint, dest_is_nfs_mount, dest_is_projected_mount,
};
#[allow(unused_imports)]
pub(crate) use mount_table::{dest_path_contains, dest_paths_equivalent};
pub use remove::try_nfs_remove;
#[cfg(test)]
pub(crate) static GROVE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
use crate::copy::CopyStats;
use crate::worktree::CreateWorktreeResult;
use crate::worktree::plan::WorktreePlan;
use crate::{IgnoredFilesMode, OUT_OF_DISK_CONTEXT, WorkingTreeMode};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;
/// Explicit NFS enablement passed into [`crate::WorktreeBuilder`].
///
/// The library never reads pager / grove config; callers resolve flags and
/// pass the result here.
#[derive(Clone, Debug)]
pub struct NfsWorktreeOpts {
    pub enabled: bool,
    /// Override for `GROVE_CONTROL_SOCK` / `$XDG_RUNTIME_DIR/grove/control.sock`.
    pub control_sock: Option<std::path::PathBuf>,
    /// Grove data dir (`daemon.db`, `mounts.toml`, `worktree-backing/`).
    pub data_dir: Option<std::path::PathBuf>,
    /// Grove runtime dir (`daemon.lock`). Defaults beside the control socket.
    pub runtime_dir: Option<std::path::PathBuf>,
    /// Ping budget (design: 250 ms, mirroring `SAME_PATH_CANON_TIMEOUT`).
    pub ping_timeout: Duration,
    /// CreateWorktree RPC timeout. Lost reply ⇒ poll, never re-issue create.
    pub create_timeout: Duration,
    /// Bound on QueryWorktreeCreate polling after a lost create reply.
    pub query_timeout: Duration,
    pub query_interval: Duration,
}
impl Default for NfsWorktreeOpts {
    fn default() -> Self {
        Self {
            enabled: false,
            control_sock: None,
            data_dir: None,
            runtime_dir: None,
            ping_timeout: Duration::from_millis(250),
            create_timeout: Duration::from_secs(180),
            query_timeout: Duration::from_secs(30),
            query_interval: Duration::from_millis(50),
        }
    }
}
/// Try the grove worktree arm (Linux FUSE / macOS NFS).
///
/// `Ok(None)` is a silent fallthrough (no side effects, or daemon confirmed
/// abort / provably-dead). `Err` must not copy-fallback: either ENOSPC or an
/// in-flight create whose dest must not be double-written.
pub(crate) fn try_grove_worktree(plan: &WorktreePlan) -> Result<Option<CreateWorktreeResult>> {
    let Some(opts) = plan.nfs.as_ref() else {
        return Ok(None);
    };
    if !opts.enabled {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    {
        if !grove_fuse_ready() {
            tracing::info!("grove-fuse skipped: /dev/fuse or fusermount missing");
            return Ok(None);
        }
        let has_delegate = plan.btrfs_delegate.is_some();
        if !has_delegate
            && matches!(
                crate::mount_info::current_mount_ns_status(),
                crate::mount_info::MountNsStatus::Private
            )
        {
            tracing::info!("grove-fuse skipped: private mount namespace");
            return Ok(None);
        }
    }
    if !confined::is_safe_worktree_id(&plan.worktree_id) {
        anyhow::bail!("invalid worktree id {:?}", plan.worktree_id);
    }
    if dest_is_projected_mount(&plan.source) {
        tracing::info!(
            source = %plan.source.display(),
            "nfs worktree skipped: source is itself an NFS mount"
        );
        return Ok(None);
    }
    if !dest_is_known_unmounted(&plan.source) && !dest_is_mountpoint(&plan.source) {
        tracing::info!(
            source = %plan.source.display(),
            "nfs worktree skipped: source mount table inconclusive"
        );
        return Ok(None);
    }
    if is_jj_source(&plan.source) {
        tracing::info!(
            source = %plan.source.display(),
            "nfs worktree skipped: jj source repo"
        );
        return Ok(None);
    }
    if matches!(plan.working_tree, WorkingTreeMode::PreserveWorkingTree)
        && plan.git_ref != "HEAD"
        && plan.git_ref != "head"
    {
        tracing::info!(
            git_ref = %plan.git_ref,
            "nfs worktree skipped: preserve + non-HEAD is a typed decline"
        );
        return Ok(None);
    }
    let client = NfsWorktreeClient::from_opts(opts);
    match client.create_worktree(plan) {
        Ok(NfsCreateDecision::Adopted(adopted)) => {
            let commit = match crate::git::get_head_commit(&adopted.dest) {
                Ok(c) => c,
                Err(e) => {
                    return teardown_after_failed_head_read(&client, &adopted.dest, e);
                }
            };
            let backing = resolved_backing_path(opts, &plan.worktree_id)
                .map(|p| p.display().to_string())
                .filter(|s| !s.is_empty());
            let pin = format!("refs/echoagent/worktrees/{}", plan.worktree_id);
            let transport = grove_transport_name(&adopted.transport);
            let mut grove = serde_json::json!({
                "transport": transport,
                "mount_id": adopted.mount_id,
                "source_pin": pin,
            });
            if adopted.port != 0 {
                grove["port"] = serde_json::json!(adopted.port);
            }
            if let Some(b) = backing {
                grove["backing"] = serde_json::Value::String(b);
            }
            let metadata = serde_json::json!({ "grove": grove });
            Ok(Some(CreateWorktreeResult {
                worktree_path: adopted.dest,
                commit,
                copy_stats: CopyStats::default(),
                ignored_stats: None,
                dirty_files_report: None,
                resolved_strategy: grove_resolved_strategy(&adopted.transport),
                strategy_metadata: Some(metadata),
            }))
        }
        Ok(NfsCreateDecision::Fallback) => Ok(None),
        Err(NfsTryError::StorageFull) => {
            let err = std::io::Error::from(std::io::ErrorKind::StorageFull);
            Err(anyhow::Error::new(err).context(OUT_OF_DISK_CONTEXT))
        }
        Err(NfsTryError::InFlight { phase }) => Err(anyhow::anyhow!(
            "nfs worktree create still in progress (phase={phase}); not falling back to copy"
        )),
        Err(NfsTryError::Other(e)) => Err(e).context("nfs worktree create failed"),
    }
}
/// Adopt succeeded but dest HEAD is unreadable. Tear the mount down so dest
/// is not left projected with no worktrees.db row. Copy-fallback only when
/// dest is known unmounted.
fn teardown_after_failed_head_read(
    client: &NfsWorktreeClient,
    dest: &Path,
    head_err: anyhow::Error,
) -> Result<Option<CreateWorktreeResult>> {
    if let Err(rm) = client.remove_worktree(dest, true) {
        tracing::warn!(
            error = %rm,
            dest = %dest.display(),
            "grove remove after failed HEAD read"
        );
    }
    if dest_is_mountpoint(dest) || !dest_is_known_unmounted(dest) {
        return Err(head_err).context(format!(
            "read HEAD after grove adopt {}; dest still mounted; not falling back to copy",
            dest.display()
        ));
    }
    tracing::warn!(
        dest = %dest.display(),
        "tore down grove dest after failed HEAD read; falling through"
    );
    Ok(None)
}
fn grove_resolved_strategy(transport: &str) -> &'static str {
    if transport.eq_ignore_ascii_case("fuse") {
        crate::worktree::STRATEGY_GROVE_FUSE
    } else {
        crate::worktree::STRATEGY_GROVE_NFS
    }
}
fn grove_transport_name(transport: &str) -> &'static str {
    if transport.eq_ignore_ascii_case("fuse") {
        "fuse"
    } else {
        "nfs"
    }
}
/// Transport written when the daemon omits mount info. Linux is FUSE; macOS is NFS.
#[must_use]
pub(crate) fn default_grove_transport() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "fuse"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "nfs"
    }
}
/// `creation_mode` for a rediscovered grove identity with no live mount fstype.
#[must_use]
#[cfg_attr(not(feature = "metadata"), allow(dead_code))]
pub(crate) fn default_grove_creation_mode() -> &'static str {
    grove_resolved_strategy(default_grove_transport())
}
#[cfg(all(target_os = "linux", not(test)))]
fn grove_fuse_ready() -> bool {
    let dev = std::path::Path::new("/dev/fuse");
    if !dev.exists() {
        return false;
    }
    let c_path = std::ffi::CString::new("/dev/fuse").unwrap_or_default();
    let writable = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 };
    if !writable {
        return false;
    }
    ["fusermount3", "fusermount"]
        .into_iter()
        .any(fusermount_on_path)
}
/// PATH lookup only — do not spawn. A `fusermount -V` child would inherit
/// the pager TTY.
#[cfg(all(target_os = "linux", not(test)))]
fn fusermount_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let p = dir.join(name);
                p.is_file()
            })
        })
        .unwrap_or(false)
}
#[cfg(all(target_os = "linux", test))]
fn grove_fuse_ready() -> bool {
    true
}
/// Grove data dir + `worktree-backing/<id>` for metadata / remove / GC.
/// Prefers an explicit opt, then a candidate that already has the backing
/// dir (post-create). Never invents a path: `nfs_record_is_dead` treats a
/// missing non-empty backing as dead, which would let pin-GC drop a live
/// worktree. Empty/unknown stays fail-closed.
fn resolved_backing_path(opts: &NfsWorktreeOpts, worktree_id: &str) -> Option<std::path::PathBuf> {
    if let Some(d) = opts.data_dir.as_ref() {
        return Some(d.join(WORKTREE_BACKING_DIR).join(worktree_id));
    }
    for d in liveness::candidate_data_dirs() {
        let b = d.join(WORKTREE_BACKING_DIR).join(worktree_id);
        if b.is_dir() {
            return Some(b);
        }
    }
    None
}
fn is_jj_source(source: &Path) -> bool {
    source.join(".jj").is_dir()
        || source.join(".git").is_dir() && source.join(".git").join("jj").exists()
}
pub(crate) fn working_tree_wire(mode: &WorkingTreeMode) -> &'static str {
    match mode {
        WorkingTreeMode::PreserveWorkingTree => "preserve",
        WorkingTreeMode::CleanTracked => "clean_tracked",
        WorkingTreeMode::CleanAll => "clean_all",
    }
}
pub(crate) fn ignored_wire(mode: &IgnoredFilesMode) -> &'static str {
    match mode {
        IgnoredFilesMode::Skip => "skip",
        IgnoredFilesMode::Copy { .. } | IgnoredFilesMode::CopyOnly { .. } => "clone",
    }
}
/// True when dispatch must not fall through to the copy engine.
pub(crate) fn nfs_error_blocks_fallback(err: &anyhow::Error) -> bool {
    err.chain().any(|c| {
        if c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull)
        {
            return true;
        }
        let s = c.to_string();
        s.contains(OUT_OF_DISK_CONTEXT)
            || s.contains("still in progress")
            || s.contains("not falling back")
    })
}
#[cfg(test)]
mod resolved_backing_tests {
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn unknown_id_is_none_not_a_guessed_path() {
        let opts = NfsWorktreeOpts {
            data_dir: None,
            ..NfsWorktreeOpts::default()
        };
        assert!(
            resolved_backing_path(&opts, "no-such-wt-id-for-gc-test").is_none(),
            "guessing the first grove data dir would look dead to pin-GC"
        );
    }
    #[test]
    fn explicit_data_dir_is_used_even_if_backing_missing() {
        let dir = TempDir::new().unwrap();
        let opts = NfsWorktreeOpts {
            data_dir: Some(dir.path().to_path_buf()),
            ..NfsWorktreeOpts::default()
        };
        let p = resolved_backing_path(&opts, "abc").expect("explicit opt");
        assert_eq!(p, dir.path().join(WORKTREE_BACKING_DIR).join("abc"));
    }
}
#[cfg(test)]
mod fallback_gate_tests {
    use super::*;
    use crate::worktree::plan::WorktreePlan;
    use crate::{CreationMode, IgnoredFilesMode, WorkingTreeMode};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    fn spawn_counting_daemon(
        sock: std::path::PathBuf,
        creates: Arc<AtomicUsize>,
    ) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(&sock).unwrap();
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { break };
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let op = serde_json::from_str::<serde_json::Value>(line.trim())
                    .ok()
                    .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                if op == "ping" {
                    let _ = writeln!(stream, r#"{{"status":"ok","data":{{"v":1,"pong":true}}}}"#);
                } else if op == "create_worktree" {
                    creates.fetch_add(1, Ordering::SeqCst);
                    let _ = writeln!(
                        stream,
                        r#"{{"status":"ok","data":{{"v":1,"create_phase":"committed","mount":{{"port":1,"mount_id":"1","transport":"nfs"}}}}}}"#
                    );
                }
            }
        })
    }
    fn base_plan(tmp: &TempDir, nfs: Option<NfsWorktreeOpts>) -> WorktreePlan {
        let dest = tmp.path().join("dest");
        WorktreePlan {
            source: tmp.path().join("repo"),
            dest: dest.clone(),
            git_ref: "HEAD".into(),
            parallelism: 1,
            channel_buffer: 8,
            working_tree: WorkingTreeMode::PreserveWorkingTree,
            ignored_files: IgnoredFilesMode::Skip,
            ignored_parallelism: 1,
            creation_mode: CreationMode::Linked,
            cancellation_token: CancellationToken::new(),
            btrfs_delegate: None,
            worktree_id: crate::worktree::plan::worktree_id_from_path(&dest),
            nfs,
        }
    }
    #[test]
    fn ordinary_source_is_known_unmounted() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        assert!(
            dest_is_known_unmounted(&tmp.path().join("repo")),
            "a regular dir must stay safe to stat after the inconclusive skip"
        );
    }
    #[test]
    fn default_opts_are_fail_closed() {
        let d = NfsWorktreeOpts::default();
        assert!(!d.enabled);
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let _h = spawn_counting_daemon(sock.clone(), Arc::clone(&creates));
        thread::sleep(Duration::from_millis(20));
        let opts = NfsWorktreeOpts {
            control_sock: Some(sock),
            ..NfsWorktreeOpts::default()
        };
        let plan = base_plan(&tmp, Some(opts));
        assert!(try_grove_worktree(&plan).unwrap().is_none());
        assert_eq!(creates.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn flag_off_never_contacts_daemon() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let _h = spawn_counting_daemon(sock.clone(), Arc::clone(&creates));
        thread::sleep(Duration::from_millis(20));
        let plan = base_plan(&tmp, None);
        assert!(try_grove_worktree(&plan).unwrap().is_none());
        assert_eq!(creates.load(Ordering::SeqCst), 0);
        let opts = NfsWorktreeOpts {
            enabled: false,
            control_sock: Some(sock),
            ..Default::default()
        };
        let plan = base_plan(&tmp, Some(opts));
        assert!(try_grove_worktree(&plan).unwrap().is_none());
        assert_eq!(creates.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn jj_source_never_contacts_daemon() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo/.jj")).unwrap();
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let _h = spawn_counting_daemon(sock.clone(), Arc::clone(&creates));
        thread::sleep(Duration::from_millis(20));
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ping_timeout: Duration::from_millis(80),
            ..Default::default()
        };
        let plan = base_plan(&tmp, Some(opts));
        assert!(try_grove_worktree(&plan).unwrap().is_none());
        assert_eq!(creates.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn preserve_non_head_never_contacts_daemon() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let _h = spawn_counting_daemon(sock.clone(), Arc::clone(&creates));
        thread::sleep(Duration::from_millis(20));
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ..Default::default()
        };
        let mut plan = base_plan(&tmp, Some(opts));
        plan.git_ref = "main".into();
        assert!(try_grove_worktree(&plan).unwrap().is_none());
        assert_eq!(creates.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn storage_full_maps_to_out_of_disk_context() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        let sock = tmp.path().join("c.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { break };
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                let _ = reader.read_line(&mut line);
                let op = serde_json::from_str::<serde_json::Value>(line.trim())
                    .ok()
                    .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                if op == "ping" {
                    let _ = writeln!(stream, r#"{{"status":"ok","data":{{"v":1,"pong":true}}}}"#);
                } else {
                    let _ = writeln!(
                        stream,
                        r#"{{"status":"ok","data":{{"v":1,"storage_full":true}}}}"#
                    );
                }
            }
        });
        thread::sleep(Duration::from_millis(20));
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ping_timeout: Duration::from_millis(80),
            create_timeout: Duration::from_millis(80),
            ..Default::default()
        };
        let plan = base_plan(&tmp, Some(opts));
        let err = try_grove_worktree(&plan).unwrap_err();
        assert_eq!(err.to_string(), OUT_OF_DISK_CONTEXT);
        assert!(nfs_error_blocks_fallback(&err));
    }
    #[test]
    fn head_read_failure_after_adopt_tears_down_and_falls_through() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let removes = Arc::new(AtomicUsize::new(0));
        let listener = UnixListener::bind(&sock).unwrap();
        let creates_d = Arc::clone(&creates);
        let removes_d = Arc::clone(&removes);
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { break };
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let op = serde_json::from_str::<serde_json::Value>(line.trim())
                    .ok()
                    .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                if op == "ping" {
                    let _ = writeln!(stream, r#"{{"status":"ok","data":{{"v":1,"pong":true}}}}"#);
                } else if op == "create_worktree" {
                    creates_d.fetch_add(1, Ordering::SeqCst);
                    let _ = writeln!(
                        stream,
                        r#"{{"status":"ok","data":{{"v":1,"create_phase":"committed","mount":{{"port":1,"mount_id":"1","transport":"nfs"}}}}}}"#
                    );
                } else if op == "remove_worktree" {
                    removes_d.fetch_add(1, Ordering::SeqCst);
                    let _ = writeln!(stream, r#"{{"status":"ok","data":{{"v":1}}}}"#);
                }
            }
        });
        thread::sleep(Duration::from_millis(20));
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ping_timeout: Duration::from_millis(80),
            create_timeout: Duration::from_millis(80),
            ..Default::default()
        };
        let plan = base_plan(&tmp, Some(opts));
        assert!(
            try_grove_worktree(&plan).unwrap().is_none(),
            "unmounted dest after failed HEAD must fall through"
        );
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        assert_eq!(removes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn invalid_worktree_id_never_contacts_daemon() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let _h = spawn_counting_daemon(sock.clone(), Arc::clone(&creates));
        thread::sleep(Duration::from_millis(20));
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ping_timeout: Duration::from_millis(80),
            ..Default::default()
        };
        let mut plan = base_plan(&tmp, Some(opts));
        plan.worktree_id = "wt name\nnewline-deadbeef".into();
        let err = try_grove_worktree(&plan).unwrap_err();
        assert!(err.to_string().contains("invalid worktree id"), "{err}");
        assert_eq!(creates.load(Ordering::SeqCst), 0);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn adopted_nfs_create_does_not_enter_copy() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        echo_agent_test_utils::git::init_git_repo(&repo);
        std::fs::write(repo.join("marker.txt"), "copied-if-entered").unwrap();
        echo_agent_test_utils::git::git_commit_all(&repo, "c");
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let _h = spawn_counting_daemon(sock.clone(), Arc::clone(&creates));
        thread::sleep(Duration::from_millis(20));
        let dest = tmp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        echo_agent_test_utils::git::init_git_repo(&dest);
        std::fs::write(dest.join("adopted.txt"), "nfs").unwrap();
        echo_agent_test_utils::git::git_commit_all(&dest, "adopted");
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ping_timeout: Duration::from_millis(80),
            create_timeout: Duration::from_millis(80),
            ..Default::default()
        };
        let copy_before = crate::grove_wt_create_count("copy");
        let plan = WorktreePlan {
            source: repo,
            dest: dest.clone(),
            git_ref: "HEAD".into(),
            parallelism: 1,
            channel_buffer: 8,
            working_tree: WorkingTreeMode::PreserveWorkingTree,
            ignored_files: IgnoredFilesMode::Skip,
            ignored_parallelism: 1,
            creation_mode: CreationMode::Linked,
            cancellation_token: CancellationToken::new(),
            btrfs_delegate: None,
            worktree_id: crate::worktree::plan::worktree_id_from_path(&dest),
            nfs: Some(opts),
        };
        let result = crate::worktree::execute_plan(plan).unwrap();
        assert_eq!(
            result.resolved_strategy,
            crate::worktree::STRATEGY_GROVE_NFS
        );
        assert_eq!(result.copy_stats.files_copied, 0);
        assert!(
            !dest.join("marker.txt").exists(),
            "dispatch must not copy-fallback after NFS adopt"
        );
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        assert_eq!(crate::grove_wt_create_count("copy"), copy_before);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_dispatch_invokes_grove_after_overlay_and_btrfs_none() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        echo_agent_test_utils::git::init_git_repo(&repo);
        std::fs::write(repo.join("marker.txt"), "copied-if-entered").unwrap();
        echo_agent_test_utils::git::git_commit_all(&repo, "c");
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let listener = UnixListener::bind(&sock).unwrap();
        let creates_for_daemon = Arc::clone(&creates);
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { break };
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let op = serde_json::from_str::<serde_json::Value>(line.trim())
                    .ok()
                    .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                if op == "ping" {
                    let _ = writeln!(stream, r#"{{"status":"ok","data":{{"v":1,"pong":true}}}}"#);
                } else if op == "create_worktree" {
                    creates_for_daemon.fetch_add(1, Ordering::SeqCst);
                    let _ = writeln!(
                        stream,
                        r#"{{"status":"ok","data":{{"v":1,"create_phase":"committed","mount":{{"port":0,"mount_id":"1","transport":"fuse"}}}}}}"#
                    );
                }
            }
        });
        thread::sleep(Duration::from_millis(20));
        let dest = tmp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        echo_agent_test_utils::git::init_git_repo(&dest);
        std::fs::write(dest.join("adopted.txt"), "fuse").unwrap();
        echo_agent_test_utils::git::git_commit_all(&dest, "adopted");
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ping_timeout: Duration::from_millis(80),
            create_timeout: Duration::from_millis(80),
            ..Default::default()
        };
        let plan = WorktreePlan {
            source: repo,
            dest: dest.clone(),
            git_ref: "HEAD".into(),
            parallelism: 1,
            channel_buffer: 8,
            working_tree: WorkingTreeMode::PreserveWorkingTree,
            ignored_files: IgnoredFilesMode::Skip,
            ignored_parallelism: 1,
            creation_mode: CreationMode::Linked,
            cancellation_token: CancellationToken::new(),
            btrfs_delegate: None,
            worktree_id: crate::worktree::plan::worktree_id_from_path(&dest),
            nfs: Some(opts),
        };
        let result = crate::worktree::execute_plan(plan).unwrap();
        assert_eq!(
            result.resolved_strategy,
            crate::worktree::STRATEGY_GROVE_FUSE
        );
        assert_eq!(result.copy_stats.files_copied, 0);
        assert!(
            !dest.join("marker.txt").exists(),
            "dispatch must not copy-fallback after grove-fuse adopt"
        );
        assert_eq!(creates.load(Ordering::SeqCst), 1);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn overlay_some_does_not_invoke_grove() {
        echo_agent_test_utils::require_git!();
        crate::worktree::execute::set_inject_overlay_some(true);
        let _reset = scopeguard_reset_injects();
        let (strategy, creates) = dispatch_with_mock_daemon();
        assert_eq!(strategy, crate::worktree::STRATEGY_OVERLAY);
        assert_eq!(creates, 0);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn btrfs_some_does_not_invoke_grove() {
        echo_agent_test_utils::require_git!();
        crate::worktree::execute::set_inject_btrfs_some(true);
        let _reset = scopeguard_reset_injects();
        let (strategy, creates) = dispatch_with_mock_daemon();
        assert_eq!(strategy, crate::worktree::STRATEGY_BTRFS);
        assert_eq!(creates, 0);
    }
    #[cfg(target_os = "linux")]
    fn scopeguard_reset_injects() -> impl Drop {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::worktree::execute::set_inject_overlay_some(false);
                crate::worktree::execute::set_inject_btrfs_some(false);
            }
        }
        Reset
    }
    #[cfg(target_os = "linux")]
    fn dispatch_with_mock_daemon() -> (&'static str, usize) {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        echo_agent_test_utils::git::init_git_repo(&repo);
        std::fs::write(repo.join("marker.txt"), "x").unwrap();
        echo_agent_test_utils::git::git_commit_all(&repo, "c");
        let sock = tmp.path().join("c.sock");
        let creates = Arc::new(AtomicUsize::new(0));
        let _h = spawn_counting_daemon(sock.clone(), Arc::clone(&creates));
        thread::sleep(Duration::from_millis(20));
        let dest = tmp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let opts = NfsWorktreeOpts {
            enabled: true,
            control_sock: Some(sock),
            ping_timeout: Duration::from_millis(80),
            create_timeout: Duration::from_millis(80),
            ..Default::default()
        };
        let plan = WorktreePlan {
            source: repo,
            dest: dest.clone(),
            git_ref: "HEAD".into(),
            parallelism: 1,
            channel_buffer: 8,
            working_tree: WorkingTreeMode::PreserveWorkingTree,
            ignored_files: IgnoredFilesMode::Skip,
            ignored_parallelism: 1,
            creation_mode: CreationMode::Linked,
            cancellation_token: CancellationToken::new(),
            btrfs_delegate: None,
            worktree_id: crate::worktree::plan::worktree_id_from_path(&dest),
            nfs: Some(opts),
        };
        let result = crate::worktree::execute_plan(plan).unwrap();
        (result.resolved_strategy, creates.load(Ordering::SeqCst))
    }
}
