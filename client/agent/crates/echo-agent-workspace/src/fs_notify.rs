#![allow(dead_code)] // Functions consumed by handle.rs event forwarder wiring
//! FsNotify adapter functions bridging [`echo_agent_fsnotify`] events to
//! workspace subsystems (hunk tracker, codebase graph, workspace
//! event broadcast).
//!
//! Codebase graph forwarding: FsNotify file-change
//! events are converted via [`fs_event_to_codebase_graph_event`] and
//! sent to the [`IndexManagerHandle`](echo_agent_codebase_graph::IndexManagerHandle).
//! The [`refresh_codebase_graph_after_head_change`] helper handles
//! git HEAD changes by diffing `ORIG_HEAD..HEAD`.

use std::path::{Path, PathBuf};

use echo_agent_fsnotify::{FsEvent, FsEventKind};
use echo_agent_hunk_tracker::HunkTrackerHandle;

/// True if `path` lies under a hidden component below `cwd`.
///
/// The `cwd` prefix is stripped first, so a `cwd` like
/// `/home/u/.config/foo` does not flag paths inside that directory as
/// hidden. Only components below `cwd` that start with `.` (and are
/// longer than a bare `.`) are considered hidden.
pub(crate) fn is_under_hidden_dir(path: &Path, cwd: &Path) -> bool {
    let rel = path.strip_prefix(cwd).unwrap_or(path);
    rel.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.starts_with('.') && s.len() > 1)
    })
}

/// Hidden-dir filter against the longest matching materialized root.
/// Multi-repo events under `/workspace/lib` must not use `/workspace/app` cwd.
pub(crate) fn is_under_hidden_dir_any(path: &Path, roots: &[PathBuf]) -> bool {
    let Some(root) = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.as_os_str().len())
    else {
        return is_under_hidden_dir(path, Path::new(""));
    };
    is_under_hidden_dir(path, root)
}

/// Forward an fs event to the hunk tracker. Hidden-directory paths
/// (relative to `cwd`) are filtered out so the hunk tracker never
/// sees `.git/`, `.echo-agent/`, etc.
pub(crate) fn forward_to_hunk_tracker(
    paths: &[PathBuf],
    kind: FsEventKind,
    handle: &HunkTrackerHandle,
    cwd: &Path,
) {
    forward_to_hunk_tracker_roots(
        paths,
        kind,
        handle,
        std::slice::from_ref(&cwd.to_path_buf()),
    );
}

pub(crate) fn forward_to_hunk_tracker_roots(
    paths: &[PathBuf],
    kind: FsEventKind,
    handle: &HunkTrackerHandle,
    roots: &[PathBuf],
) {
    for path in paths {
        if is_under_hidden_dir_any(path, roots) {
            continue;
        }
        match kind {
            FsEventKind::Created | FsEventKind::Modified | FsEventKind::Renamed => {
                handle.handle_file_change(path.clone());
            }
            FsEventKind::Removed => {
                handle.handle_file_deleted(path.clone());
            }
            _ => {}
        }
    }
}

/// Convert to codebase graph `FileEvent` for incremental index updates.
pub(crate) fn fs_event_to_codebase_graph_event(
    paths: &[PathBuf],
    kind: FsEventKind,
) -> echo_agent_codebase_graph::FileEvent {
    use echo_agent_codebase_graph::{FileEvent, FileEventKind};
    let graph_kind = match kind {
        FsEventKind::Created => FileEventKind::Created,
        FsEventKind::Modified => FileEventKind::Modified,
        FsEventKind::Removed => FileEventKind::Removed,
        FsEventKind::Renamed => FileEventKind::Renamed,
        // `#[non_exhaustive]` fallback.
        _ => FileEventKind::Modified,
    };
    FileEvent::new(paths.to_vec(), graph_kind)
}

/// Convert [`echo_agent_fsnotify::FsEventKind`] to the wire-type
/// [`echo_agent_workspace_types::FsEventKind`].
///
/// Identity mapping today; kept explicit so all known variants are
/// consciously mapped. Unknown future variants fall back to
/// `Modified` via the `#[non_exhaustive]` wildcard arm.
pub(crate) fn to_workspace_event_kind(
    kind: FsEventKind,
) -> echo_agent_workspace_types::FsEventKind {
    match kind {
        FsEventKind::Created => echo_agent_workspace_types::FsEventKind::Created,
        FsEventKind::Modified => echo_agent_workspace_types::FsEventKind::Modified,
        FsEventKind::Removed => echo_agent_workspace_types::FsEventKind::Removed,
        FsEventKind::Renamed => echo_agent_workspace_types::FsEventKind::Renamed,
        // `#[non_exhaustive]` fallback.
        _ => echo_agent_workspace_types::FsEventKind::Modified,
    }
}

/// Spawn a background task that reads [`FsEvent`]s from a broadcast
/// receiver, forwards `FilesChanged` to the hunk tracker, and
/// re-broadcasts each affected path as
/// [`WorkspaceEvent::FsChanged`](echo_agent_workspace_types::WorkspaceEvent::FsChanged)
/// on the workspace event bus.
///
/// The task exits when:
/// - the broadcast sender drops (all `FsEventSource`s for this
///   receiver are gone), or
/// - `cancel` is cancelled.
pub(crate) fn spawn_fs_event_forwarder(
    rx: tokio::sync::broadcast::Receiver<FsEvent>,
    hunk_tracker: HunkTrackerHandle,
    events_tx: tokio::sync::broadcast::Sender<echo_agent_workspace_types::WorkspaceEvent>,
    cwd: PathBuf,
    cancel: tokio_util::sync::CancellationToken,
    codebase_index: Option<std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>>,
) {
    spawn_fs_event_forwarder_roots(
        rx,
        hunk_tracker,
        events_tx,
        vec![cwd],
        cancel,
        codebase_index,
    );
}

pub(crate) fn spawn_fs_event_forwarder_roots(
    mut rx: tokio::sync::broadcast::Receiver<FsEvent>,
    hunk_tracker: HunkTrackerHandle,
    events_tx: tokio::sync::broadcast::Sender<echo_agent_workspace_types::WorkspaceEvent>,
    roots: Vec<PathBuf>,
    cancel: tokio_util::sync::CancellationToken,
    codebase_index: Option<std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                result = rx.recv() => {
                    match result {
                        Ok(FsEvent::FilesChanged { ref paths, kind }) => {
                            // Forward to hunk tracker (hidden-dir filtered per mount).
                            forward_to_hunk_tracker_roots(paths, kind, &hunk_tracker, &roots);
                            // Forward to codebase graph for incremental
                            // index updates (hidden-dir paths are indexed
                            // -- the graph's own ignore logic handles them).
                            if let Some(ref idx) = codebase_index {
                                let graph_event = fs_event_to_codebase_graph_event(paths, kind);
                                if let Err(e) = idx.send_event(graph_event) {
                                    tracing::debug!(
                                        error = %e,
                                        "failed to forward fs event to codebase graph"
                                    );
                                }
                            }
                            // Broadcast per-path WorkspaceEvent::FsChanged.
                            let ws_kind = to_workspace_event_kind(kind);
                            for path in paths {
                                let _ = events_tx.send(
                                    echo_agent_workspace_types::WorkspaceEvent::FsChanged {
                                        path: path.clone(),
                                        kind: ws_kind,
                                    },
                                );
                            }
                        }
                        Ok(other) => {
                            // Git meta / operation events -- not yet bridged
                            // to WorkspaceEvent::GitHeadChanged etc.
                            tracing::trace!(?other, "fs event forwarder: unhandled event variant");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                lagged = n,
                                "fs event forwarder lagged; some events were dropped"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });
}

const GIT_DIFF_REBUILD_THRESHOLD: usize = 500;

fn parse_diff_name_status_line(
    line: &str,
    repo_root: &Path,
) -> Option<echo_agent_codebase_graph::FileEvent> {
    let mut parts = line.splitn(3, '\t');
    let status = parts.next()?.trim();
    let path = parts.next()?;

    match status.chars().next()? {
        'A' => Some(echo_agent_codebase_graph::FileEvent::created(
            repo_root.join(path),
        )),
        'D' => Some(echo_agent_codebase_graph::FileEvent::removed(
            repo_root.join(path),
        )),
        'R' | 'C' => {
            let new_path = parts.next()?;
            Some(echo_agent_codebase_graph::FileEvent::renamed(
                repo_root.join(path),
                repo_root.join(new_path),
            ))
        }
        _ => Some(echo_agent_codebase_graph::FileEvent::modified(
            repo_root.join(path),
        )),
    }
}

/// After a HEAD change, diff `ORIG_HEAD..HEAD` and send targeted
/// events to the codebase graph. Falls back to full rebuild if too
/// many files changed.
///
/// Emits [`WorkspaceEvent::CodebaseIndexUpdated`] on the provided
/// `events_tx` after the index has been updated (either via targeted
/// events or a full rebuild). Skips the event if the index actor
/// channel is closed (i.e. the actor has been dropped).
pub(crate) async fn refresh_codebase_graph_after_head_change(
    idx: &echo_agent_codebase_graph::IndexManagerHandle,
    repo_root: &Path,
    events_tx: &tokio::sync::broadcast::Sender<echo_agent_workspace_types::WorkspaceEvent>,
) {
    let mut diff_cmd = tokio::process::Command::new("git");
    diff_cmd
        .args(["diff", "--name-status", "ORIG_HEAD", "HEAD"])
        .current_dir(repo_root)
        .stdin(std::process::Stdio::null());
    echo_agent_tools::util::detach_command(&mut diff_cmd);
    diff_cmd.envs(echo_agent_tools::util::pager_env());
    let diff_output = diff_cmd.output().await;

    // `None` means the update failed entirely (channel closed) --
    // skip the event so subscribers are not misled.
    let files_updated: Option<u64>;

    match diff_output {
        Ok(output) if output.status.success() => {
            let changed: Vec<_> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| parse_diff_name_status_line(l, repo_root))
                .collect();

            let count = changed.len();
            if count > GIT_DIFF_REBUILD_THRESHOLD {
                tracing::debug!(
                    "git_refresh: {count} changed files exceeds threshold, falling back to rebuild"
                );
                files_updated = match idx.rebuild() {
                    Ok(()) => Some(count as u64),
                    Err(e) => {
                        tracing::debug!("git_refresh: rebuild failed: {:?}", e);
                        None
                    }
                };
            } else if let Err(e) = idx.send_events(changed) {
                tracing::debug!("git_refresh: failed to send graph events: {:?}", e);
                files_updated = None;
            } else {
                tracing::debug!("git_refresh: sent {count} changed files to codebase graph");
                files_updated = Some(count as u64);
            }
        }
        _ => {
            tracing::debug!("git_refresh: git diff failed, falling back to rebuild");
            files_updated = match idx.rebuild() {
                Ok(()) => Some(0),
                Err(e) => {
                    tracing::debug!("git_refresh: rebuild fallback also failed: {:?}", e);
                    None
                }
            };
        }
    }

    if let Some(count) = files_updated {
        let _ = events_tx.send(
            echo_agent_workspace_types::WorkspaceEvent::CodebaseIndexUpdated {
                files_indexed: count,
            },
        );
    }
}

pub(crate) fn ws_event_to_codebase_graph_event(
    path: &std::path::Path,
    kind: echo_agent_workspace_types::FsEventKind,
) -> echo_agent_codebase_graph::FileEvent {
    use echo_agent_codebase_graph::{FileEvent, FileEventKind};
    let graph_kind = match kind {
        echo_agent_workspace_types::FsEventKind::Created => FileEventKind::Created,
        echo_agent_workspace_types::FsEventKind::Modified => FileEventKind::Modified,
        echo_agent_workspace_types::FsEventKind::Removed => FileEventKind::Removed,
        echo_agent_workspace_types::FsEventKind::Renamed => FileEventKind::Renamed,
    };
    FileEvent::new(vec![path.to_path_buf()], graph_kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn hidden_dir_positive() {
        assert!(is_under_hidden_dir(
            &PathBuf::from("/workspace/.echo-agent/worktrees/abc/src/main.rs"),
            &PathBuf::from("/workspace"),
        ));
    }

    #[test]
    fn hidden_dir_ignores_cwd_components() {
        assert!(!is_under_hidden_dir(
            &PathBuf::from("/home/user/.config/project/src/lib.rs"),
            &PathBuf::from("/home/user/.config/project"),
        ));
    }

    #[test]
    fn hidden_dir_dot_only() {
        // A bare `.` component is not "hidden".
        assert!(!is_under_hidden_dir(
            &PathBuf::from("/workspace/./src/main.rs"),
            &PathBuf::from("/workspace"),
        ));
    }

    #[test]
    fn hidden_dir_any_uses_longest_mount_prefix() {
        let roots = vec![
            PathBuf::from("/workspace/app"),
            PathBuf::from("/workspace/lib"),
        ];
        assert!(is_under_hidden_dir_any(
            Path::new("/workspace/lib/.git/HEAD"),
            &roots,
        ));
        assert!(!is_under_hidden_dir_any(
            Path::new("/workspace/lib/src/lib.rs"),
            &roots,
        ));
        assert!(!is_under_hidden_dir_any(
            Path::new("/workspace/app/src/main.rs"),
            &roots,
        ));
    }

    #[test]
    fn workspace_event_kind_round_trip() {
        use echo_agent_workspace_types::FsEventKind as WsKind;
        assert_eq!(
            to_workspace_event_kind(FsEventKind::Created),
            WsKind::Created
        );
        assert_eq!(
            to_workspace_event_kind(FsEventKind::Modified),
            WsKind::Modified
        );
        assert_eq!(
            to_workspace_event_kind(FsEventKind::Removed),
            WsKind::Removed
        );
        assert_eq!(
            to_workspace_event_kind(FsEventKind::Renamed),
            WsKind::Renamed
        );
    }

    #[test]
    fn codebase_graph_event_mapping() {
        use echo_agent_codebase_graph::FileEventKind;
        let paths = vec![PathBuf::from("/workspace/src/main.rs")];

        let ev = fs_event_to_codebase_graph_event(&paths, FsEventKind::Created);
        assert_eq!(ev.kind, FileEventKind::Created);

        let ev = fs_event_to_codebase_graph_event(&paths, FsEventKind::Modified);
        assert_eq!(ev.kind, FileEventKind::Modified);

        let ev = fs_event_to_codebase_graph_event(&paths, FsEventKind::Removed);
        assert_eq!(ev.kind, FileEventKind::Removed);

        let ev = fs_event_to_codebase_graph_event(&paths, FsEventKind::Renamed);
        assert_eq!(ev.kind, FileEventKind::Renamed);
    }

    #[test]
    fn parse_diff_name_status_all_variants() {
        use echo_agent_codebase_graph::FileEventKind;
        use std::path::Path;
        let root = Path::new("/repo");

        let ev = parse_diff_name_status_line("M\tsrc/main.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Modified);

        let ev = parse_diff_name_status_line("A\tnew_file.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Created);

        let ev = parse_diff_name_status_line("D\told_file.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Removed);

        let ev = parse_diff_name_status_line("R100\told.rs\tnew.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Renamed);

        assert!(parse_diff_name_status_line("", root).is_none());
    }
}
