//! Off-draw-thread full-file syntax highlight for edit diffs.
//!
//! Mirrors [`super::mermaid_worker`]: one `std::thread` + mpsc, coalesced by
//! `entry_id` (latest job wins), polled each tick via `try_recv`. First paint
//! stays hunk-only on the UI thread; this worker upgrades to file-scoped styles
//! when the post-edit file is readable and under
//! [`crate::scrollback::blocks::tool::EDIT_HL_MAX_BYTES`] /
//! [`crate::scrollback::blocks::tool::EDIT_HL_MAX_LINES`].
//!
//! # Cost model
//!
//! - **First paint** — hunk-only; never full-file on the UI thread.
//! - **Upgrade** — one syntect pass up to the last hunk line, off-thread;
//!   paints then overlay the style map onto the ordinary hunk render.
//! - **Over cap / read fail** — stay hunk-only; no unbounded work.
//!
//! Magnitudes: `benches/edit_highlight`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use indexmap::IndexMap;

use crate::app::agent_view::AgentView;
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::tool::{
    EDIT_HL_MAX_BYTES, EditHighlightPhase, EditLineStyles, ToolCallBlock,
    compute_file_scoped_styles, file_text_within_hl_caps,
};
use crate::scrollback::entry::EntryId;
use echo_agent_pager_diff::DiffHunk;

/// Tracing target. Filter with `RUST_LOG=edit_hl=debug`.
pub const EDIT_HL_TRACING_TARGET: &str = "edit_hl";

/// A unit of work for the edit-highlight worker.
#[derive(Debug, Clone)]
pub struct EditHlJob {
    pub job_id: u64,
    pub entry_id: EntryId,
    /// Absolute path to the post-edit file on disk.
    pub abs_path: PathBuf,
    /// Display path used for syntax selection (extension).
    pub path: String,
    /// Hunks whose new-side lines need styles.
    pub hunks: Vec<DiffHunk>,
}

/// Outcome of one highlight attempt.
#[derive(Debug, Clone)]
pub enum EditHlOutcome {
    /// Full-file styles for new-side lines (hunk text verified equal to disk).
    Ready {
        by_new_line: Arc<HashMap<usize, EditLineStyles>>,
        /// Theme the styles were baked under (paint skips a mismatched map).
        theme: crate::theme::ThemeKind,
    },
    /// Cap, I/O, non-UTF8, mismatch, or missing syntax — stay hunk-only.
    Failed,
}

/// Result returned over the worker channel.
#[derive(Debug, Clone)]
pub struct EditHlResult {
    pub job_id: u64,
    pub entry_id: EntryId,
    /// Display path at submit time (stale check).
    pub path: String,
    pub outcome: EditHlOutcome,
}

/// Per-[`AgentView`] edit-HL runtime: channels + in-flight bookkeeping.
pub struct EditHlRuntime {
    tx: Sender<EditHlJob>,
    rx: Receiver<EditHlResult>,
    /// Outstanding `(job_id, entry_id)` for `needs_tick`.
    pending: Vec<(u64, EntryId)>,
    next_job_id: u64,
}

impl EditHlRuntime {
    fn new() -> Self {
        let (tx, rx) = spawn_worker();
        Self {
            tx,
            rx,
            pending: Vec::new(),
            next_job_id: 1,
        }
    }

    fn alloc_job_id(&mut self) -> u64 {
        let id = self.next_job_id;
        self.next_job_id = self.next_job_id.wrapping_add(1);
        id
    }
}

/// Spawn the single edit-highlight worker thread and return its channels.
pub fn spawn_worker() -> (Sender<EditHlJob>, Receiver<EditHlResult>) {
    let (job_tx, job_rx) = std::sync::mpsc::channel::<EditHlJob>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<EditHlResult>();

    std::thread::Builder::new()
        .name("edit-hl".to_string())
        .spawn(move || {
            while let Ok(first) = job_rx.recv() {
                let pending = drain_coalesced(first, &job_rx);
                for (_, job) in pending {
                    let outcome = run_job(&job);
                    let result = EditHlResult {
                        job_id: job.job_id,
                        entry_id: job.entry_id,
                        path: job.path,
                        outcome,
                    };
                    if result_tx.send(result).is_err() {
                        return;
                    }
                }
            }
        })
        .expect("spawn edit-hl thread");

    (job_tx, result_rx)
}

/// Coalesce `first` plus queued jobs by `entry_id` (latest wins). FIFO across keys.
fn drain_coalesced(first: EditHlJob, rx: &Receiver<EditHlJob>) -> IndexMap<EntryId, EditHlJob> {
    let mut pending: IndexMap<EntryId, EditHlJob> = IndexMap::new();
    pending.insert(first.entry_id, first);
    while let Ok(job) = rx.try_recv() {
        pending.insert(job.entry_id, job);
    }
    pending
}

/// Read post-edit file under caps and compute file-scoped styles.
fn run_job(job: &EditHlJob) -> EditHlOutcome {
    let Some(file_text) = read_file_capped(&job.abs_path, EDIT_HL_MAX_BYTES) else {
        tracing::debug!(
            target: EDIT_HL_TRACING_TARGET,
            path = %job.abs_path.display(),
            "edit HL skipped (read/cap/utf8)"
        );
        return EditHlOutcome::Failed;
    };
    if !file_text_within_hl_caps(&file_text) {
        tracing::debug!(
            target: EDIT_HL_TRACING_TARGET,
            path = %job.abs_path.display(),
            "edit HL skipped (line cap)"
        );
        return EditHlOutcome::Failed;
    }

    let path = std::path::Path::new(&job.path);
    // Read the theme beside the syntect walk so the result is labeled with the
    // kind its foregrounds were actually baked under.
    let theme = crate::theme::cache::current_kind();
    match compute_file_scoped_styles(path, &file_text, &job.hunks) {
        Some(by_new_line) => EditHlOutcome::Ready {
            by_new_line: Arc::new(by_new_line),
            theme,
        },
        None => {
            tracing::debug!(
                target: EDIT_HL_TRACING_TARGET,
                path = %job.abs_path.display(),
                "edit HL skipped (disk/hunk mismatch or no syntax)"
            );
            EditHlOutcome::Failed
        }
    }
}

/// Read at most `max_bytes`; reject oversized / non-UTF8 / missing.
fn read_file_capped(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > max_bytes {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Resolve a tool path against session cwd for compatibility callers.
pub fn resolve_edit_abs_path(path: &str, session_cwd: Option<&std::path::Path>) -> PathBuf {
    resolve_edit_target_path(path, session_cwd).unwrap_or_else(|| PathBuf::from(path))
}

fn resolve_edit_target_path(path: &str, session_cwd: Option<&std::path::Path>) -> Option<PathBuf> {
    crate::render::tool_paths::resolve_tool_path_target(path, session_cwd)
}

impl AgentView {
    /// Keep ticking while an edit-HL job is outstanding (mpsc has no waker).
    pub fn edit_hl_needs_tick(&self) -> bool {
        self.edit_hl
            .as_ref()
            .is_some_and(|rt| !rt.pending.is_empty())
    }

    /// Poll worker results and attach FileScoped styles. Returns true if redraw.
    pub fn edit_hl_tick(&mut self) -> bool {
        self.poll_edit_hl_results()
    }

    fn ensure_edit_hl_runtime(&mut self) -> &mut EditHlRuntime {
        if self.edit_hl.is_none() {
            self.edit_hl = Some(EditHlRuntime::new());
        }
        self.edit_hl.as_mut().expect("just created")
    }

    /// Submit full-file HL for a completed Edit entry. Builds the job from the
    /// live block; sets `Pending` without cache invalidate (paint-identical to
    /// HunkOnly). No-op if missing / failed / empty hunks.
    pub fn submit_edit_highlight(&mut self, entry_id: EntryId) {
        let (path, hunks) = {
            let Some(entry) = self.scrollback.get_by_id(entry_id) else {
                return;
            };
            let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &entry.block else {
                return;
            };
            if edit.error.is_some() || edit.hunks.is_empty() {
                return;
            }
            (edit.path.clone(), edit.hunks.clone())
        };

        let Some(target_path) = resolve_edit_target_path(&path, Some(self.session.cwd.as_path()))
        else {
            return;
        };

        let rt = self.ensure_edit_hl_runtime();
        let job_id = rt.alloc_job_id();
        // Drop older pending for this entry so coalesce cannot pin Fast tick.
        rt.pending.retain(|(_, eid)| *eid != entry_id);

        if let Some(entry) = self.scrollback.get_by_id_mut(entry_id)
            && let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &mut entry.block
        {
            edit.highlight = EditHighlightPhase::Pending { job_id };
        }

        let job = EditHlJob {
            job_id,
            entry_id,
            abs_path: target_path,
            path,
            hunks,
        };

        let rt = self.edit_hl.as_mut().expect("runtime just ensured");
        if rt.tx.send(job).is_ok() {
            rt.pending.push((job_id, entry_id));
            tracing::debug!(
                target: EDIT_HL_TRACING_TARGET,
                job_id,
                entry_id = entry_id.value(),
                "edit HL job submitted"
            );
        } else {
            // Send fails only when the worker thread is gone: revert this
            // entry and abandon every earlier in-flight job (their results
            // can never arrive either).
            if let Some(entry) = self.scrollback.get_by_id_mut(entry_id)
                && let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &mut entry.block
            {
                edit.highlight = EditHighlightPhase::HunkOnly;
            }
            self.abandon_edit_hl_worker("job send failed");
        }
    }

    fn poll_edit_hl_results(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;

        let mut results = Vec::new();
        let mut disconnected = false;
        if let Some(rt) = self.edit_hl.as_ref() {
            loop {
                match rt.rx.try_recv() {
                    Ok(r) => results.push(r),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut redraw = false;
        for result in results {
            if let Some(rt) = self.edit_hl.as_mut() {
                rt.pending.retain(|(id, _)| *id != result.job_id);
            }

            let Some(entry) = self.scrollback.get_by_id_mut(result.entry_id) else {
                continue;
            };
            let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &mut entry.block else {
                continue;
            };
            if edit.path != result.path {
                continue;
            }
            match &edit.highlight {
                EditHighlightPhase::Pending { job_id } if *job_id == result.job_id => {}
                _ => continue,
            }

            match result.outcome {
                EditHlOutcome::Ready { by_new_line, theme } => {
                    edit.highlight = EditHighlightPhase::FileScoped { by_new_line, theme };
                    entry.invalidate_cache();
                    redraw = true;
                    tracing::debug!(
                        target: EDIT_HL_TRACING_TARGET,
                        job_id = result.job_id,
                        entry_id = result.entry_id.value(),
                        "edit HL FileScoped applied"
                    );
                }
                EditHlOutcome::Failed => {
                    edit.highlight = EditHighlightPhase::HunkOnly;
                }
            }
        }
        if disconnected {
            // Near-unreachable in shipped builds (panic=abort kills the whole
            // pager with the worker, as mermaid_worker documents); cheap
            // insurance so a dead worker can't strand `pending` and pin
            // `TickDemand::Fast` forever in unwind builds.
            self.abandon_edit_hl_worker("result channel disconnected");
        }
        redraw
    }

    /// Forget a dead worker: drop its runtime (a later submit respawns a fresh
    /// one) and revert every in-flight entry to `HunkOnly`, which paints
    /// identically to `Pending`, so no redraw or cache invalidate is needed.
    fn abandon_edit_hl_worker(&mut self, context: &'static str) {
        let Some(rt) = self.edit_hl.take() else {
            return;
        };
        tracing::warn!(
            target: EDIT_HL_TRACING_TARGET,
            context,
            pending = rt.pending.len(),
            "edit HL worker gone; reverting pending jobs to hunk-only"
        );
        for (_, entry_id) in rt.pending {
            if let Some(entry) = self.scrollback.get_by_id_mut(entry_id)
                && let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &mut entry.block
                && matches!(edit.highlight, EditHighlightPhase::Pending { .. })
            {
                edit.highlight = EditHighlightPhase::HunkOnly;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_pager_diff::DiffLine;
    use similar::ChangeTag;
    use std::sync::mpsc;

    fn sample_hunks() -> Vec<DiffHunk> {
        vec![vec![DiffLine {
            text: "x = 1\n".into(),
            lo: 1,
            ln: 1,
            tag: ChangeTag::Insert,
        }]]
    }

    #[test]
    fn drain_coalesced_latest_wins_per_entry() {
        let (tx, rx) = mpsc::channel();
        let e1 = EntryId::new(1);
        let e2 = EntryId::new(2);
        let first = EditHlJob {
            job_id: 1,
            entry_id: e1,
            abs_path: PathBuf::from("/a"),
            path: "a.py".into(),
            hunks: sample_hunks(),
        };
        tx.send(EditHlJob {
            job_id: 2,
            entry_id: e1,
            abs_path: PathBuf::from("/a2"),
            path: "a.py".into(),
            hunks: sample_hunks(),
        })
        .unwrap();
        tx.send(EditHlJob {
            job_id: 3,
            entry_id: e2,
            abs_path: PathBuf::from("/b"),
            path: "b.py".into(),
            hunks: sample_hunks(),
        })
        .unwrap();
        drop(tx);

        let coalesced = drain_coalesced(first, &rx);
        assert_eq!(coalesced.len(), 2);
        assert_eq!(coalesced.get(&e1).unwrap().job_id, 2);
        assert_eq!(coalesced.get(&e1).unwrap().abs_path, PathBuf::from("/a2"));
        assert_eq!(coalesced.get(&e2).unwrap().job_id, 3);
    }

    #[test]
    fn public_edit_highlight_path_api_remains_source_compatible() {
        let job = EditHlJob {
            job_id: 1,
            entry_id: EntryId::new(1),
            abs_path: PathBuf::from("/repo/src/lib.rs"),
            path: "src/lib.rs".into(),
            hunks: sample_hunks(),
        };
        assert_eq!(job.abs_path, PathBuf::from("/repo/src/lib.rs"));
        assert_eq!(
            resolve_edit_abs_path("src/lib.rs", Some(std::path::Path::new("/repo"))),
            PathBuf::from("/repo/src/lib.rs")
        );
    }

    #[test]
    fn resolve_edit_target_path_preserves_filesystem_semantics() {
        let joined = resolve_edit_target_path("src/lib.rs", Some(std::path::Path::new("/proj")));
        assert_eq!(joined, Some(PathBuf::from("/proj/src/lib.rs")));
        let already = resolve_edit_target_path("/abs/x.rs", Some(std::path::Path::new("/proj")));
        assert_eq!(already, Some(PathBuf::from("/abs/x.rs")));
        let parent_sensitive = resolve_edit_target_path(
            "/repo/link/../target.rs",
            Some(std::path::Path::new("/ignored")),
        );
        assert_eq!(
            parent_sensitive,
            Some(PathBuf::from("/repo/link/../target.rs"))
        );
        let tilde = resolve_edit_target_path("~/x.rs", None).expect("home-relative target");
        let tilde_s = tilde.to_string_lossy();
        assert!(
            tilde.is_absolute() && tilde_s.ends_with("x.rs") && !tilde_s.starts_with("~/"),
            "tilde path should expand, got {tilde:?}"
        );
    }

    #[test]
    fn run_job_fails_on_missing_file() {
        let job = EditHlJob {
            job_id: 1,
            entry_id: EntryId::new(9),
            abs_path: PathBuf::from("/nonexistent/edit_hl_probe_does_not_exist.py"),
            path: "probe.py".into(),
            hunks: sample_hunks(),
        };
        assert!(matches!(run_job(&job), EditHlOutcome::Failed));
    }

    #[test]
    fn run_job_succeeds_on_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.py");
        let body = "x = 1\ny = 2\n";
        std::fs::write(&path, body).unwrap();
        let job = EditHlJob {
            job_id: 1,
            entry_id: EntryId::new(1),
            abs_path: path,
            path: "probe.py".into(),
            hunks: sample_hunks(),
        };
        match run_job(&job) {
            EditHlOutcome::Ready { by_new_line, theme } => {
                assert!(by_new_line.contains_key(&1));
                assert_eq!(theme, crate::theme::cache::current_kind());
            }
            EditHlOutcome::Failed => panic!("expected Ready for temp python file"),
        }
    }

    #[test]
    fn run_job_fails_on_hunk_disk_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.py");
        std::fs::write(&path, "x = 1\n").unwrap();
        let job = EditHlJob {
            job_id: 1,
            entry_id: EntryId::new(1),
            abs_path: path,
            path: "probe.py".into(),
            hunks: vec![vec![DiffLine {
                text: "x = 999\n".into(),
                lo: 1,
                ln: 1,
                tag: ChangeTag::Insert,
            }]],
        };
        assert!(matches!(run_job(&job), EditHlOutcome::Failed));
    }

    #[test]
    fn poll_drops_stale_job_id() {
        use crate::scrollback::blocks::tool::EditToolCallBlock;

        let mut agent = crate::app::agent_view::test_agent_view(
            Some("edit-hl-stale"),
            PathBuf::from("/tmp/edit-hl-test"),
        );
        let block = EditToolCallBlock::new("probe.py", sample_hunks());
        let entry_id = agent
            .scrollback
            .push_block(RenderBlock::ToolCall(ToolCallBlock::Edit(block)));

        if let Some(entry) = agent.scrollback.get_by_id_mut(entry_id)
            && let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &mut entry.block
        {
            edit.highlight = EditHighlightPhase::Pending { job_id: 2 };
        }

        let (job_tx, _job_rx) = mpsc::channel::<EditHlJob>();
        let (res_tx, res_rx) = mpsc::channel::<EditHlResult>();
        agent.edit_hl = Some(EditHlRuntime {
            tx: job_tx,
            rx: res_rx,
            pending: vec![(2, entry_id)],
            next_job_id: 3,
        });
        res_tx
            .send(EditHlResult {
                job_id: 1,
                entry_id,
                path: "probe.py".into(),
                outcome: EditHlOutcome::Ready {
                    by_new_line: Arc::new(HashMap::new()),
                    theme: crate::theme::cache::current_kind(),
                },
            })
            .unwrap();

        let redraw = agent.edit_hl_tick();
        assert!(!redraw, "stale job must not apply");
        let entry = agent.scrollback.get_by_id(entry_id).expect("entry");
        let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &entry.block else {
            panic!("edit block missing");
        };
        assert!(
            matches!(edit.highlight, EditHighlightPhase::Pending { job_id: 2 }),
            "pending job_id=2 must remain"
        );
    }

    #[test]
    fn disconnected_worker_reverts_pending_and_unpins_tick() {
        use crate::scrollback::blocks::tool::EditToolCallBlock;

        let mut agent = crate::app::agent_view::test_agent_view(
            Some("edit-hl-disconnect"),
            PathBuf::from("/tmp/edit-hl-test"),
        );
        let block = EditToolCallBlock::new("probe.py", sample_hunks());
        let entry_id = agent
            .scrollback
            .push_block(RenderBlock::ToolCall(ToolCallBlock::Edit(block)));

        if let Some(entry) = agent.scrollback.get_by_id_mut(entry_id)
            && let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &mut entry.block
        {
            edit.highlight = EditHighlightPhase::Pending { job_id: 7 };
        }

        // Both worker-side channel ends dropped = dead worker thread.
        let (job_tx, _) = mpsc::channel::<EditHlJob>();
        let (_, res_rx) = mpsc::channel::<EditHlResult>();
        agent.edit_hl = Some(EditHlRuntime {
            tx: job_tx,
            rx: res_rx,
            pending: vec![(7, entry_id)],
            next_job_id: 8,
        });

        assert!(agent.edit_hl_needs_tick(), "pending must demand ticks");
        let redraw = agent.edit_hl_tick();
        assert!(!redraw, "revert paints identically to Pending");
        assert!(
            !agent.edit_hl_needs_tick(),
            "dead worker must not pin fast ticks"
        );
        assert!(agent.edit_hl.is_none(), "runtime dropped for lazy respawn");
        let entry = agent.scrollback.get_by_id(entry_id).expect("entry");
        let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &entry.block else {
            panic!("edit block missing");
        };
        assert!(
            matches!(edit.highlight, EditHighlightPhase::HunkOnly),
            "stranded Pending must revert to HunkOnly"
        );
    }

    #[test]
    fn double_submit_prunes_pending_after_latest() {
        use crate::scrollback::blocks::tool::EditToolCallBlock;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.py");
        std::fs::write(&path, "x = 1\n").unwrap();

        let mut agent = crate::app::agent_view::test_agent_view(
            Some("edit-hl-coalesce"),
            dir.path().to_path_buf(),
        );
        let rel = path.file_name().unwrap().to_string_lossy().into_owned();
        let block = EditToolCallBlock::new(&rel, sample_hunks());
        let entry_id = agent
            .scrollback
            .push_block(RenderBlock::ToolCall(ToolCallBlock::Edit(block)));

        agent.submit_edit_highlight(entry_id);
        agent.submit_edit_highlight(entry_id);
        // After double submit, only the latest job_id should remain in pending.
        {
            let rt = agent.edit_hl.as_ref().expect("runtime");
            assert_eq!(rt.pending.len(), 1, "submit must prune prior entry pending");
            assert_eq!(rt.pending[0].1, entry_id);
        }

        // Pump until settle (worker has no waker).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while agent.edit_hl_needs_tick() {
            agent.edit_hl_tick();
            if std::time::Instant::now() > deadline {
                panic!("edit HL did not settle");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            !agent.edit_hl_needs_tick(),
            "pending must be empty after latest result"
        );
    }
}
