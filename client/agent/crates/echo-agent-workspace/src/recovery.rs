//! Startup restart-recovery scan for the workspace upload queue.
//!
//! After a restart, not-yet-uploaded archives survive on disk as temp +
//! `.meta.json` ([`QueueItemSidecar`]) pairs. The fresh [`UploadQueue`] worker
//! only consumes its in-process channel — it never rescans the spill dir — so
//! [`run_startup_recovery`] walks the sidecars once at startup, verifies each
//! temp file against its recorded `sha256`, and re-enqueues survivors; corrupt,
//! orphaned, and expired pairs are deleted. It runs before the workspace
//! registers with the server, so prior-life items drain before any new turn hook
//! can race against the queue.

use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use echo_agent_file_utils::queue::{
    DEFAULT_MAX_AGE, EnqueueOutcome, QueueItemSidecar, SIDECAR_SUFFIX, UploadQueue,
    temp_path_for_sidecar, try_remove_temp,
};
use prometheus::{IntCounter, IntCounterVec, register_int_counter, register_int_counter_vec};

/// Successful re-enqueues, labelled by artifact name.
static ORPHAN_RECOVERED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_orphan_recovered_total",
        "Queue-item sidecars re-enqueued from disk on startup",
        &["artifact_name"]
    )
    .unwrap()
});

/// Pairs dropped without re-enqueue, labelled by reason
/// (`missing_tmp` | `sha_mismatch` | `io_error` | `parse_error`).
static ORPHAN_LOST: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_orphan_lost_total",
        "Queue-item sidecars dropped on startup (corrupt / orphaned temp file)",
        &["reason"]
    )
    .unwrap()
});

/// Pairs dropped because they exceeded [`DEFAULT_MAX_AGE`] before recovery ran.
static ORPHAN_EXPIRED: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "echo_agent_workspace_orphan_expired_total",
        "Queue-item sidecars dropped on startup for exceeding the max age"
    )
    .unwrap()
});

/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    for artifact in [
        "tool_state.json",
        "workspace_environment.json",
        "session_artifact.tar.gz",
    ] {
        ORPHAN_RECOVERED.with_label_values(&[artifact]).inc_by(0);
    }
    for reason in [
        "missing_tmp",
        "sha_mismatch",
        "io_error",
        "parse_error",
        "unsafe_artifact_name",
        "unsafe_session_id",
        "unsafe_gcs_path",
        "gcs_path_session_mismatch",
    ] {
        ORPHAN_LOST.with_label_values(&[reason]).inc_by(0);
    }
    ORPHAN_EXPIRED.inc_by(0);
}

/// Summary of a single [`run_startup_recovery`] sweep, returned for structured
/// logging; Prometheus counters are emitted inline per sidecar.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Total `*.meta.json` sidecars examined.
    pub sidecars_scanned: u32,
    /// Sidecar/temp pairs verified and re-enqueued for upload.
    pub recovered: u32,
    /// Pairs dropped (missing/unreadable temp, unparseable sidecar, sha mismatch).
    pub lost: u32,
    /// Pairs dropped for exceeding [`DEFAULT_MAX_AGE`].
    pub expired: u32,
    /// Re-enqueue attempts that failed (worker shut down); files are left in
    /// place so a later startup can retry.
    pub reenqueue_failures: u32,
}

/// Scan `<workspace_home>/upload_queue/*.meta.json`, verify each sidecar
/// against its temp file, and re-enqueue the survivors in place via
/// [`UploadQueue::enqueue_recovered`] — the original pair is handed to the
/// worker unmodified, so its `enqueued_at` stays anchored to the first spill
/// and repeated restarts cannot slide the max-age window. Corrupt, orphaned,
/// and expired pairs are deleted with a labelled `lost`/`expired` metric. All
/// errors are absorbed into the [`RecoveryReport`] rather than propagated: a
/// partial recovery must never panic startup.
pub async fn run_startup_recovery(workspace_home: &Path, queue: &UploadQueue) -> RecoveryReport {
    let queue_dir = workspace_home.join("upload_queue");
    let mut report = RecoveryReport::default();

    // Snapshot first: re-enqueueing writes new sidecars into this same dir, and
    // we must never re-process one we just created.
    let sidecars: Vec<std::path::PathBuf> = match std::fs::read_dir(&queue_dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(SIDECAR_SUFFIX))
            })
            .collect(),
        Err(e) => {
            tracing::debug!(
                dir = %queue_dir.display(),
                error = %e,
                "restart recovery: no upload_queue dir to scan"
            );
            return report;
        }
    };

    for sidecar_path in sidecars {
        report.sidecars_scanned += 1;
        let temp_path = temp_path_for_sidecar(&sidecar_path);

        let sidecar = match std::fs::read(&sidecar_path) {
            Ok(bytes) => match serde_json::from_slice::<QueueItemSidecar>(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        sidecar = %sidecar_path.display(),
                        error = %e,
                        "restart recovery: unparseable sidecar manifest; dropping pair"
                    );
                    record_lost(&mut report, "parse_error");
                    delete_pair(&sidecar_path, temp_path.as_deref());
                    continue;
                }
            },
            Err(e) => {
                tracing::warn!(
                    sidecar = %sidecar_path.display(),
                    error = %e,
                    "restart recovery: unreadable sidecar manifest; dropping pair"
                );
                record_lost(&mut report, "io_error");
                delete_pair(&sidecar_path, temp_path.as_deref());
                continue;
            }
        };

        let Some(temp_path) = temp_path else {
            // Unreachable: filtered on SIDECAR_SUFFIX above. Defensive only.
            record_lost(&mut report, "io_error");
            try_remove_temp(&sidecar_path, None);
            continue;
        };

        if !temp_path.exists() {
            tracing::warn!(
                temp = %temp_path.display(),
                artifact = %sidecar.artifact_name,
                "restart recovery: sidecar without temp file; dropping sidecar"
            );
            record_lost(&mut report, "missing_tmp");
            try_remove_temp(&sidecar_path, None);
            continue;
        }

        let bytes = match std::fs::read(&temp_path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    temp = %temp_path.display(),
                    error = %e,
                    "restart recovery: unreadable temp file; dropping pair"
                );
                record_lost(&mut report, "io_error");
                delete_pair(&sidecar_path, Some(&temp_path));
                continue;
            }
        };

        // Corruption guard: the bytes must hash to the recorded sha256.
        let actual_sha = echo_agent_file_utils::sha256_hex(&bytes);
        if actual_sha != sidecar.sha256 {
            tracing::warn!(
                temp = %temp_path.display(),
                artifact = %sidecar.artifact_name,
                "restart recovery: sha256 mismatch (temp file truncated/corrupt); dropping pair"
            );
            record_lost(&mut report, "sha_mismatch");
            delete_pair(&sidecar_path, Some(&temp_path));
            continue;
        }

        if sidecar_age(&sidecar, &temp_path) > DEFAULT_MAX_AGE {
            tracing::info!(
                artifact = %sidecar.artifact_name,
                enqueued_at = %sidecar.enqueued_at,
                "restart recovery: orphan exceeded max age; dropping pair"
            );
            report.expired += 1;
            ORPHAN_EXPIRED.inc();
            delete_pair(&sidecar_path, Some(&temp_path));
            continue;
        }

        // The sidecar is on-disk JSON (a trust boundary): a crafted `.meta.json`
        // must not smuggle path traversal into the temp filename or upload key.
        if let Err(reason) = validate_recovered_sidecar(&sidecar) {
            tracing::warn!(
                sidecar = %sidecar_path.display(),
                artifact = %sidecar.artifact_name,
                reason,
                "restart recovery: sidecar failed field validation; dropping pair"
            );
            record_lost(&mut report, reason);
            delete_pair(&sidecar_path, Some(&temp_path));
            continue;
        }

        let outcome = queue.enqueue_recovered(&temp_path, &sidecar_path, &sidecar);
        match outcome {
            EnqueueOutcome::Enqueued
            | EnqueueOutcome::FellBackToInline
            | EnqueueOutcome::Deduplicated
            | EnqueueOutcome::Skipped { .. } => {
                // The worker owns the original pair from here: it deletes both
                // files on every terminal outcome, same as a normal enqueue.
                report.recovered += 1;
                ORPHAN_RECOVERED
                    .with_label_values(&[sidecar.artifact_name.as_str()])
                    .inc();
                tracing::info!(
                    artifact = %sidecar.artifact_name,
                    gcs_path = %sidecar.gcs_path,
                    ?outcome,
                    "restart recovery: re-enqueued orphaned archive"
                );
            }
            EnqueueOutcome::Failed { reason } => {
                report.reenqueue_failures += 1;
                tracing::warn!(
                    artifact = %sidecar.artifact_name,
                    reason,
                    "restart recovery: re-enqueue failed; leaving pair for retry"
                );
            }
        }
    }

    if report != RecoveryReport::default() {
        tracing::info!(?report, "workspace startup restart-recovery complete");
    }
    report
}

/// Age of an orphan pair: prefer the sidecar's recorded `enqueued_at`; if it is
/// unparseable, fall back to the temp file's mtime; if neither resolves, treat
/// it as fresh (`Duration::ZERO`) so a parsing hiccup never deletes live data.
fn sidecar_age(sidecar: &QueueItemSidecar, temp_path: &Path) -> Duration {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&sidecar.enqueued_at) {
        let enqueued: SystemTime = dt.with_timezone(&chrono::Utc).into();
        // `Err` means `enqueued` is in the future (clock skew) → treat as fresh.
        return SystemTime::now()
            .duration_since(enqueued)
            .unwrap_or(Duration::ZERO);
    }
    std::fs::metadata(temp_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or(Duration::ZERO)
}

/// Record a `lost` outcome on the report and bump the labelled counter.
fn record_lost(report: &mut RecoveryReport, reason: &str) {
    report.lost += 1;
    ORPHAN_LOST.with_label_values(&[reason]).inc();
}

/// Delete a sidecar and (optionally) its temp file, tolerating already-absent
/// files. Returns `true` only when both are now gone, so callers can surface a
/// leaked pair that would otherwise be re-processed on the next restart.
fn delete_pair(sidecar: &Path, temp: Option<&Path>) -> bool {
    let removed_sidecar = remove_if_present(sidecar);
    let removed_temp = temp.map(remove_if_present).unwrap_or(true);
    removed_sidecar && removed_temp
}

/// Remove `path`, treating an already-absent file as success. Returns `false`
/// only on a real (non-`NotFound`) removal error.
fn remove_if_present(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "restart recovery: failed to remove queue file"
            );
            false
        }
    }
}

/// Reject manifest-derived fields that, fed back into `enqueue_bytes_blocking`,
/// could escape the `upload_queue/` dir or the upload destination:
/// `artifact_name`/`session_id` are interpolated into the temp filename (no
/// separators, `..`, or NUL); `gcs_path` is the upload key (`/` allowed, `..`
/// and NUL not). Returns `Err(reason)` — a stable metric label.
fn validate_recovered_sidecar(sidecar: &QueueItemSidecar) -> Result<(), &'static str> {
    fn is_filename_safe(s: &str) -> bool {
        !s.is_empty()
            && !s.contains('/')
            && !s.contains('\\')
            && !s.contains("..")
            && !s.contains('\0')
    }
    if !is_filename_safe(&sidecar.artifact_name) {
        return Err("unsafe_artifact_name");
    }
    if !is_filename_safe(&sidecar.session_id) {
        return Err("unsafe_session_id");
    }
    if sidecar.gcs_path.contains("..") || sidecar.gcs_path.contains('\0') {
        return Err("unsafe_gcs_path");
    }
    // Session binding: every workspace artifact is keyed under its session
    // prefix, so a sidecar whose `gcs_path` escapes `<session_id>/` is either
    // corrupt or tampered with — without this check a manifest could keep a
    // valid session_id while redirecting verified bytes to another prefix.
    if !sidecar
        .gcs_path
        .strip_prefix(&sidecar.session_id)
        .is_some_and(|rest| rest.starts_with('/'))
    {
        return Err("gcs_path_session_mismatch");
    }
    Ok(())
}

/// Opt-out startup path: delete every spilled pair (and
/// lone queue file) from a prior life instead of re-enqueueing it. A workspace
/// launched opted out must neither upload nor retain bytes that
/// a prior, differently-configured life left behind. Returns the number of
/// files removed.
pub fn purge_spilled_items(workspace_home: &Path) -> u32 {
    let queue_dir = workspace_home.join("upload_queue");
    let entries = match std::fs::read_dir(&queue_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut removed = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && remove_if_present(&path) {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            removed,
            "restart recovery: collection disabled; purged spilled queue files"
        );
    }
    removed
}

/// Default maximum age for a per-session state directory before the
/// [`cleanup_stale_sessions`] janitor reclaims it (7 days).
pub const DEFAULT_SESSION_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Remove per-session state directories under `<workspace_home>/sessions/`
/// whose mtime is older than `max_age`, bounding the unbounded growth a
/// long-lived workspace (or reused sandbox) would otherwise accumulate.
///
/// A directory's mtime advances on every atomic-rename persistence write (the
/// rename mutates the directory entry), so it tracks last activity closely
/// enough for a best-effort reclaim. All errors are swallowed — a startup
/// janitor must never fail boot. Only directories with a resolvable, expired
/// mtime are removed; stray files and future-mtime entries are left untouched.
pub async fn cleanup_stale_sessions(workspace_home: &Path, max_age: Duration) {
    let sessions_dir = workspace_home.join("sessions");
    let Ok(mut entries) = tokio::fs::read_dir(&sessions_dir).await else {
        return; // No sessions dir yet (first boot).
    };

    let mut removed = 0u32;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(ft) = entry.file_type().await else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let path = entry.path();
        if let Ok(metadata) = tokio::fs::metadata(&path).await
            && let Ok(modified) = metadata.modified()
            && let Ok(age) = modified.elapsed()
            && age > max_age
        {
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => {
                    removed += 1;
                    tracing::info!(
                        path = %path.display(),
                        age_secs = age.as_secs(),
                        "cleanup_stale_sessions: removed stale session dir"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "cleanup_stale_sessions: failed to remove stale session dir"
                    );
                }
            }
        }
    }

    if removed > 0 {
        tracing::info!(
            removed,
            "cleanup_stale_sessions: stale session-dir sweep complete"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use echo_agent_file_utils::queue::{TraceExportSource, UploadRetryPolicy, sidecar_path_for};
    use echo_agent_file_utils::{TraceExportConfig, UploadMethod};

    /// Resolver pointing at an unreachable proxy; these tests only assert on
    /// the synchronous re-enqueue / file-deletion path, never upload completion.
    struct UnreachableResolver;
    impl TraceExportSource for UnreachableResolver {
        fn resolve(&self) -> TraceExportConfig {
            TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                upload_method: UploadMethod::Proxy {
                    // 127.0.0.1:1 is never listening.
                    proxy_base_url: "http://127.0.0.1:1/v1".to_string(),
                    user_token: String::new(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
            }
        }
    }

    /// Huge initial backoff so the worker never deletes a re-enqueued temp file
    /// mid-test.
    fn slow_policy() -> UploadRetryPolicy {
        UploadRetryPolicy {
            initial_delay: Duration::from_secs(3600),
            ..UploadRetryPolicy::default()
        }
    }

    fn spawn_queue(workspace_home: &Path) -> UploadQueue {
        UploadQueue::spawn(workspace_home, Arc::new(UnreachableResolver), slow_policy())
    }

    /// Write an orphan pair as a prior workspace life would have left it;
    /// `sha256` is computed over `content` so the pair passes the corruption
    /// guard. Returns the (temp, sidecar) paths.
    fn write_orphan_pair(
        queue_dir: &Path,
        stem: &str,
        content: &[u8],
        artifact_name: &str,
        enqueued_at: String,
    ) -> (PathBuf, PathBuf) {
        let temp = queue_dir.join(stem);
        std::fs::write(&temp, content).unwrap();
        let sidecar = QueueItemSidecar {
            schema_version: 1,
            session_id: "session-abcdef12".to_string(),
            turn_number: 4,
            gcs_path: format!("session-abcdef12/turn_4/{artifact_name}"),
            content_type: "application/gzip".to_string(),
            artifact_name: artifact_name.to_string(),
            enqueued_at,
            sha256: echo_agent_file_utils::sha256_hex(content),
        };
        let sidecar_path = sidecar_path_for(&temp);
        std::fs::write(&sidecar_path, serde_json::to_vec(&sidecar).unwrap()).unwrap();
        (temp, sidecar_path)
    }

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// Hours-ago RFC3339 timestamp for the age/expiry tests.
    fn hours_ago_rfc3339(hours: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::hours(hours))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn sidecar_with(session_id: &str, artifact_name: &str, gcs_path: &str) -> QueueItemSidecar {
        QueueItemSidecar {
            schema_version: 1,
            session_id: session_id.to_string(),
            turn_number: 1,
            gcs_path: gcs_path.to_string(),
            content_type: "application/gzip".to_string(),
            artifact_name: artifact_name.to_string(),
            enqueued_at: now_rfc3339(),
            sha256: "0".repeat(64),
        }
    }

    /// Path traversal in any manifest-derived field is rejected with a stable
    /// reason label.
    #[test]
    fn validate_recovered_sidecar_rejects_traversal() {
        assert!(
            validate_recovered_sidecar(&sidecar_with(
                "session-abcdef12",
                "tool_state.json",
                "session-abcdef12/turn_1/tool_state.json",
            ))
            .is_ok()
        );

        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("s", "../../etc/passwd", "ok")),
            Err("unsafe_artifact_name")
        );
        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("s", "a/b", "ok")),
            Err("unsafe_artifact_name")
        );
        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("../evil", "ok", "ok")),
            Err("unsafe_session_id")
        );
        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("", "ok", "ok")),
            Err("unsafe_session_id")
        );

        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("s", "a", "sess/../../escape")),
            Err("unsafe_gcs_path")
        );
        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("s", "a", "sess/ok\0null")),
            Err("unsafe_gcs_path")
        );
    }

    /// `gcs_path` must live under the sidecar's own `<session_id>/` prefix.
    #[test]
    fn validate_recovered_sidecar_binds_gcs_path_to_session() {
        assert!(validate_recovered_sidecar(&sidecar_with("s", "a", "s/turn_1/a")).is_ok());
        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("s", "a", "other-session/turn_1/a")),
            Err("gcs_path_session_mismatch")
        );
        // Prefix must be the full path segment, not a string prefix.
        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("s", "a", "s-extra/turn_1/a")),
            Err("gcs_path_session_mismatch")
        );
        // Bare session id with no object below it is not a valid upload key.
        assert_eq!(
            validate_recovered_sidecar(&sidecar_with("s", "a", "s")),
            Err("gcs_path_session_mismatch")
        );
    }

    /// Opt-out purge removes every spilled queue file instead of re-enqueueing.
    #[test]
    fn purge_spilled_items_removes_all_queue_files() {
        let home = tempfile::TempDir::new().unwrap();
        let queue_dir = home.path().join("upload_queue");
        std::fs::create_dir_all(&queue_dir).unwrap();
        let (temp, sidecar) = write_orphan_pair(
            &queue_dir,
            "abcdef12_turn4_tool_state.json_333_0",
            b"zdr-bytes",
            "tool_state.json",
            now_rfc3339(),
        );
        let lone = queue_dir.join("legacy_lone_temp");
        std::fs::write(&lone, b"legacy").unwrap();

        let removed = purge_spilled_items(home.path());

        assert_eq!(removed, 3, "pair + lone file all purged");
        assert!(!temp.exists());
        assert!(!sidecar.exists());
        assert!(!lone.exists());
    }

    /// Missing queue dir is a no-op purge.
    #[test]
    fn purge_spilled_items_missing_dir_is_noop() {
        let home = tempfile::TempDir::new().unwrap();
        assert_eq!(purge_spilled_items(home.path()), 0);
    }

    #[tokio::test]
    async fn recovers_valid_orphan_pair_in_place() {
        let home = tempfile::TempDir::new().unwrap();
        let queue_dir = home.path().join("upload_queue");
        std::fs::create_dir_all(&queue_dir).unwrap();

        let content = b"recoverable-archive-bytes";
        let (temp, sidecar) = write_orphan_pair(
            &queue_dir,
            "abcdef12_turn4_tool_state.json_111_0",
            content,
            "tool_state.json",
            now_rfc3339(),
        );

        let queue = spawn_queue(home.path());
        let recovered_before = ORPHAN_RECOVERED
            .with_label_values(&["tool_state.json"])
            .get();
        let enqueued_before = queue
            .stats()
            .enqueued
            .load(std::sync::atomic::Ordering::Relaxed);

        let report = run_startup_recovery(home.path(), &queue).await;

        // The re-enqueue really happened: report + queue stat + metric all moved.
        assert_eq!(report.recovered, 1, "one orphan re-enqueued");
        assert_eq!(report.lost, 0);
        assert_eq!(report.expired, 0);
        assert_eq!(report.sidecars_scanned, 1);
        assert_eq!(
            queue
                .stats()
                .enqueued
                .load(std::sync::atomic::Ordering::Relaxed),
            enqueued_before + 1,
            "the item entered the queue"
        );
        assert_eq!(
            ORPHAN_RECOVERED
                .with_label_values(&["tool_state.json"])
                .get(),
            recovered_before + 1,
            "recovered counter moved"
        );

        // The ORIGINAL pair is handed to the worker unmodified — `enqueued_at`
        // stays anchored to the first spill so restarts cannot slide the
        // max-age window. The worker deletes the pair on its terminal outcome.
        assert!(temp.exists(), "original temp reused in place");
        assert!(sidecar.exists(), "original sidecar reused in place");
    }

    #[tokio::test]
    async fn corrupt_temp_file_is_dropped_on_sha_mismatch() {
        let home = tempfile::TempDir::new().unwrap();
        let queue_dir = home.path().join("upload_queue");
        std::fs::create_dir_all(&queue_dir).unwrap();

        let (temp, sidecar) = write_orphan_pair(
            &queue_dir,
            "abcdef12_turn4_workspace_environment.json_222_0",
            b"original-bytes",
            "workspace_environment.json",
            now_rfc3339(),
        );
        // Corrupt the temp file AFTER the sidecar recorded the original sha256.
        std::fs::write(&temp, b"TRUNCATED").unwrap();

        let queue = spawn_queue(home.path());
        let lost_before = ORPHAN_LOST.with_label_values(&["sha_mismatch"]).get();
        let enqueued_before = queue
            .stats()
            .enqueued
            .load(std::sync::atomic::Ordering::Relaxed);

        let report = run_startup_recovery(home.path(), &queue).await;

        assert_eq!(report.lost, 1, "corrupt pair counted as lost");
        assert_eq!(report.recovered, 0, "corrupt pair never re-enqueued");
        assert_eq!(
            queue
                .stats()
                .enqueued
                .load(std::sync::atomic::Ordering::Relaxed),
            enqueued_before,
            "nothing entered the queue"
        );
        assert_eq!(
            ORPHAN_LOST.with_label_values(&["sha_mismatch"]).get(),
            lost_before + 1,
            "sha_mismatch lost counter moved"
        );
        assert!(!temp.exists(), "corrupt temp deleted");
        assert!(!sidecar.exists(), "corrupt pair's sidecar deleted");
    }

    #[tokio::test]
    async fn expired_orphan_is_dropped() {
        let home = tempfile::TempDir::new().unwrap();
        let queue_dir = home.path().join("upload_queue");
        std::fs::create_dir_all(&queue_dir).unwrap();

        // enqueued 3h ago — older than DEFAULT_MAX_AGE (2h).
        let (temp, sidecar) = write_orphan_pair(
            &queue_dir,
            "abcdef12_turn4_tool_state.json_333_0",
            b"stale-archive",
            "tool_state.json",
            hours_ago_rfc3339(3),
        );

        let queue = spawn_queue(home.path());
        let expired_before = ORPHAN_EXPIRED.get();

        let report = run_startup_recovery(home.path(), &queue).await;

        assert_eq!(report.expired, 1, "stale pair counted as expired");
        assert_eq!(report.recovered, 0, "stale pair never re-enqueued");
        assert_eq!(report.lost, 0);
        assert_eq!(
            ORPHAN_EXPIRED.get(),
            expired_before + 1,
            "expired counter moved"
        );
        assert!(!temp.exists(), "expired temp deleted");
        assert!(!sidecar.exists(), "expired sidecar deleted");
    }

    #[tokio::test]
    async fn sidecar_without_temp_file_is_lost() {
        let home = tempfile::TempDir::new().unwrap();
        let queue_dir = home.path().join("upload_queue");
        std::fs::create_dir_all(&queue_dir).unwrap();

        // Sidecar present, temp file absent (deleted out from under it).
        let (temp, sidecar) = write_orphan_pair(
            &queue_dir,
            "abcdef12_turn4_workspace_environment.json_444_0",
            b"bytes",
            "workspace_environment.json",
            now_rfc3339(),
        );
        std::fs::remove_file(&temp).unwrap();

        let queue = spawn_queue(home.path());
        let lost_before = ORPHAN_LOST.with_label_values(&["missing_tmp"]).get();

        let report = run_startup_recovery(home.path(), &queue).await;

        assert_eq!(report.lost, 1, "lone sidecar counted as lost");
        assert_eq!(report.recovered, 0);
        assert_eq!(
            ORPHAN_LOST.with_label_values(&["missing_tmp"]).get(),
            lost_before + 1,
            "missing_tmp lost counter moved"
        );
        assert!(!sidecar.exists(), "lone sidecar deleted");
    }

    #[tokio::test]
    async fn empty_queue_dir_is_a_noop() {
        let home = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("upload_queue")).unwrap();
        let queue = spawn_queue(home.path());

        let report = run_startup_recovery(home.path(), &queue).await;

        assert_eq!(report, RecoveryReport::default(), "nothing to recover");
    }

    // -----------------------------------------------------------------------
    // cleanup_stale_sessions
    // -----------------------------------------------------------------------

    /// A session dir older than `max_age` is removed.
    #[tokio::test]
    async fn cleanup_removes_stale_session_dir() {
        let home = tempfile::TempDir::new().unwrap();
        let stale = home.path().join("sessions").join("sess-old");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("tool_state.json"), b"{}").unwrap();
        // Guarantee `age > Duration::ZERO` regardless of mtime resolution.
        tokio::time::sleep(Duration::from_millis(15)).await;

        cleanup_stale_sessions(home.path(), Duration::ZERO).await;

        assert!(
            !stale.exists(),
            "a session dir older than max_age must be removed"
        );
    }

    /// A session dir younger than `max_age` is kept.
    #[tokio::test]
    async fn cleanup_keeps_fresh_session_dir() {
        let home = tempfile::TempDir::new().unwrap();
        let fresh = home.path().join("sessions").join("sess-new");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("tool_state.json"), b"{}").unwrap();

        cleanup_stale_sessions(home.path(), Duration::from_secs(3600)).await;

        assert!(
            fresh.exists(),
            "a session dir younger than max_age must be kept"
        );
        assert!(
            fresh.join("tool_state.json").exists(),
            "a kept session dir must retain its contents"
        );
    }

    /// No `sessions/` directory (first boot) is a silent no-op, not a panic.
    #[tokio::test]
    async fn cleanup_missing_sessions_dir_is_noop() {
        let home = tempfile::TempDir::new().unwrap();
        cleanup_stale_sessions(home.path(), Duration::ZERO).await;
        assert!(home.path().exists(), "home is left untouched");
    }

    /// Stray files under `sessions/` are never removed — directories only.
    #[tokio::test]
    async fn cleanup_ignores_non_dir_entries() {
        let home = tempfile::TempDir::new().unwrap();
        let sessions = home.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let stray = sessions.join("stray.txt");
        std::fs::write(&stray, b"not a session dir").unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;

        cleanup_stale_sessions(home.path(), Duration::ZERO).await;

        assert!(
            stray.exists(),
            "stray files under sessions/ must never be removed"
        );
    }

    /// Mixed sweep: the `max_age` comparison is per-entry, not all-or-nothing.
    #[tokio::test]
    async fn cleanup_removes_only_expired_dirs() {
        let home = tempfile::TempDir::new().unwrap();
        let sessions = home.path().join("sessions");
        let old = sessions.join("sess-old");
        std::fs::create_dir_all(&old).unwrap();
        // Wide gap (100ms) vs a 50ms threshold so neither side is sensitive to
        // scheduler jitter.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let max_age = Duration::from_millis(50);
        let fresh = sessions.join("sess-fresh");
        std::fs::create_dir_all(&fresh).unwrap();

        cleanup_stale_sessions(home.path(), max_age).await;

        assert!(!old.exists(), "the >max_age dir must be removed");
        assert!(
            fresh.exists(),
            "the <max_age dir must survive the same sweep"
        );
    }
}
