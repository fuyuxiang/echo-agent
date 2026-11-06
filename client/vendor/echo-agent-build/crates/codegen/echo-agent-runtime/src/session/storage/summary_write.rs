// Modified by EchoAgent for the embedded desktop runtime; see ECHOAGENT_VENDOR.md.
//! Concurrency-safe, field-correct writes to a session's `summary.json`.
//!
//! The same `summary.json` is mutated by several writers and, on reconnect, by
//! more than one persistence actor. A whole-summary read-modify-write with no
//! lock loses updates: a writer holding a stale read overwrites a concurrent
//! writer's field on write-back, which silently reverted `last_active_at` and
//! `num_messages` (the active session then sank in the `/resume` picker).
//!
//! [`SummaryPatch`] expresses *intent* (a partial update) rather than a
//! whole-struct snapshot, and [`apply_patch_locked`] applies it under an
//! exclusive lock on a sidecar `summary.json.lock` (never renamed, so the lock
//! spans the entire read-modify-write). All writers funnel through it, so the
//! read-modify-writes serialize across actors and processes.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use agent_client_protocol as acp;
use chrono::{DateTime, Utc};
use echo_agent_sampling_types::ReasoningEffort;
use fs2::FileExt;

use crate::session::persistence::Summary;

/// How a counter field changes. `Increment` is applied to the in-lock fresh
/// read (never precomputed by the caller, which would re-open the race); `Set`
/// is an absolute rewrite (compaction / rewind).
#[derive(Debug, Clone)]
pub(crate) enum CounterOp {
    Increment(usize),
    Set(usize),
}

impl CounterOp {
    fn apply(&self, current: usize) -> usize {
        match self {
            CounterOp::Increment(n) => current.saturating_add(*n),
            CounterOp::Set(n) => *n,
        }
    }
}

/// Model / agent / reasoning-effort update. Each `None` leaves the existing
/// value unchanged (matches the legacy `update_current_model` semantics).
#[derive(Debug, Clone)]
pub(crate) struct ModelPatch {
    pub model_id: acp::ModelId,
    pub agent_name: Option<String>,
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
}

/// Persisted git HEAD. `commit` and `branch` are last-writer-wins, including
/// being cleared to `None`.
#[derive(Debug, Clone)]
pub(crate) struct GitHeadPatch {
    pub commit: Option<String>,
    pub branch: Option<String>,
}

/// Telemetry trace bookkeeping. `next_trace_turn` is monotonic; `request_id`
/// is applied only when this turn wins, so a stale lower-turn write cannot
/// leave a high `next_trace_turn` paired with an older `request_id` (these
/// were set together in the legacy read-modify-write path).
#[derive(Debug, Clone)]
pub(crate) struct TraceTurnPatch {
    pub next_trace_turn: u64,
    pub request_id: Option<String>,
}

/// A typed, partial mutation of a `Summary`. Only the set fields change; the
/// rest are read fresh under the lock and preserved. Per-field merge rules
/// (see [`Summary::apply_patch`]): `last_active_at` / `next_trace_turn` /
/// `chat_format_version` are monotonic (never lowered), counters apply to the
/// fresh read, everything else is last-writer-wins on that field alone.
#[derive(Debug, Clone, Default)]
pub(crate) struct SummaryPatch {
    pub record_activity: bool,
    pub messages: Option<CounterOp>,
    pub chat_messages: Option<CounterOp>,
    pub chat_format_version: Option<u8>,
    pub trace_turn: Option<TraceTurnPatch>,
    pub model: Option<ModelPatch>,
    pub git_head: Option<GitHeadPatch>,
    pub collection_id: Option<String>,
    /// Set the session title unconditionally (last-writer-wins). Used by the
    /// manual `/rename` (`/title`) path, which must always win. Also marks the
    /// title manual (`Summary::title_is_manual`).
    pub generated_title: Option<String>,
    /// Set the session title only when the session has no title yet. Used by
    /// automatic LLM title generation so it never clobbers a title the user
    /// set via `/rename`. Ignored when `generated_title` is also set.
    pub generated_title_if_absent: Option<String>,
    /// Legacy refresh request. Like initial generation, it can only fill an
    /// absent title. An established automatic or manual title remains stable.
    pub generated_title_regenerate: Option<String>,
    /// `/rename --auto`: clear the manual pin. Takes precedence over the
    /// generated-title fields. A successful clear blanks `generated_title`
    /// *and* `session_summary` so `display_title()` is empty and if-absent
    /// can adopt again (a leftover pre-rename auto title would block regen).
    pub reset_title_to_auto: bool,
    pub cwd_switch_bookkeeping_generation: Option<u64>,
    /// Per-turn dashboard summary as `(text, prompt_id)`. Outer `Some`
    /// applies (last-writer-wins); `Some(None)` clears it (conversation
    /// rewind removed the described work).
    pub last_turn_summary: Option<Option<(String, String)>>,
    /// Latest session recap preview. Outer `Some` applies (last-writer-wins);
    /// `Some(None)` clears it (rewind removed the described turns). Persisted so
    /// listing surfaces can show a recap when available.
    pub last_recap: Option<Option<String>>,
}

impl Summary {
    /// Apply `patch` in place using the per-field merge rules. `now` is the
    /// single timestamp used for both `last_active_at` (when activity is
    /// recorded) and `updated_at`.
    ///
    /// Returns `true` iff an auto title was adopted (`generated_title_if_absent`
    /// or `generated_title_regenerate`), **or** a `reset_title_to_auto` actually
    /// cleared a manual pin. Callers use the former to propagate the adopted
    /// title and the latter to reset the generator / remote pin only when unpin
    /// changed disk.
    pub(crate) fn apply_patch(&mut self, patch: &SummaryPatch, now: DateTime<Utc>) -> bool {
        if patch.record_activity {
            // Monotonic: a stale concurrent writer can never move it backwards.
            self.last_active_at = Some(
                self.last_active_at
                    .map_or(now, |existing| existing.max(now)),
            );
        }
        if let Some(op) = &patch.messages {
            self.num_messages = op.apply(self.num_messages);
        }
        if let Some(op) = &patch.chat_messages {
            self.num_chat_messages = op.apply(self.num_chat_messages);
        }
        if let Some(version) = patch.chat_format_version {
            self.chat_format_version = self.chat_format_version.max(version);
        }
        if let Some(generation) = patch.cwd_switch_bookkeeping_generation
            && generation > self.cwd_switch_bookkeeping_generation
        {
            self.cwd_switch_bookkeeping_generation = generation;
            // An explicit chat counter op already owns the resulting count
            // (append increments; history replacement sets). Without one, this
            // patch repairs a line found on disk after an earlier summary failure.
            if patch.chat_messages.is_none() {
                self.num_chat_messages = self.num_chat_messages.saturating_add(1);
            }
        }
        if let Some(trace_turn) = &patch.trace_turn {
            // next_trace_turn is monotonic; keep request_id paired with the
            // winning turn so a stale lower-turn write can't re-pair them.
            if trace_turn.next_trace_turn >= self.next_trace_turn {
                self.next_trace_turn = trace_turn.next_trace_turn;
                if let Some(request_id) = &trace_turn.request_id {
                    self.request_id = Some(request_id.clone());
                }
            }
        }
        if let Some(model) = &patch.model {
            self.current_model_id = model.model_id.clone();
            if let Some(agent_name) = &model.agent_name {
                self.agent_name = Some(agent_name.clone());
            }
            if let Some(reasoning_effort) = &model.reasoning_effort {
                self.reasoning_effort = *reasoning_effort;
            }
        }
        if let Some(git_head) = &patch.git_head {
            self.head_commit = git_head.commit.clone();
            self.head_branch = git_head.branch.clone();
        }
        if let Some(collection_id) = &patch.collection_id {
            self.collection_id = Some(collection_id.clone());
        }
        if let Some(last_turn_summary) = &patch.last_turn_summary {
            let (text, prompt_id) = last_turn_summary.clone().unzip();
            self.last_turn_summary = text;
            self.last_turn_summary_prompt_id = prompt_id;
        }
        if let Some(recap) = &patch.last_recap {
            self.last_recap = recap.clone();
        }
        let mut absent_title_applied = false;
        if patch.reset_title_to_auto {
            // Gate on a real pin (`manual_title_opt`), not a stale flag
            // over a blank `generated_title` — that would wipe a legitimate
            // auto title living in `session_summary`.
            let cleared_manual = self.manual_title_opt().is_some();
            if cleared_manual {
                self.generated_title = None;
                // Blank both fields so `display_title()` is empty and
                // `set_generated_title_if_absent` can adopt again. A leftover
                // pre-rename auto title in `session_summary` would otherwise
                // pin display forever.
                self.session_summary.clear();
            }
            self.title_is_manual = false;
            self.updated_at = now;
            return cleared_manual;
        } else if let Some(title) = &patch.generated_title {
            self.set_title(title);
            // Manual `/rename`: recorded so clients can restore the
            // prompt-border title on resume.
            self.title_is_manual = true;
        } else if let Some(title) = patch
            .generated_title_if_absent
            .as_ref()
            .or(patch.generated_title_regenerate.as_ref())
        {
            // All automatic writers share this atomic guard, including legacy
            // refreshes and results arriving after a reconnect or manual rename.
            if !title.trim().is_empty() && self.display_title().trim().is_empty() {
                self.set_title(title);
                // Defensive: an adopted auto title is never manual.
                self.title_is_manual = false;
                absent_title_applied = true;
            }
        }
        self.updated_at = now;
        absent_title_applied
    }

    /// Set `generated_title`, mirroring into `session_summary` while that field
    /// is still empty so older clients that only read `session_summary` see the
    /// title too.
    fn set_title(&mut self, title: &str) {
        self.generated_title = Some(title.to_owned());
        if self.session_summary.is_empty()
            || (!self.title_is_manual
                && crate::session::helpers::session_summary::contains_internal_reasoning_markup(
                    &self.session_summary,
                ))
        {
            self.session_summary = title.to_owned();
        }
    }
}

/// Read → apply `patch` → write `summary_path`, serialized by an exclusive lock
/// on the sidecar `lock_path`. The lock is held across the whole read-modify-
/// write so concurrent writers cannot lose each other's updates. Synchronous:
/// callers run it on `spawn_blocking` because the lock acquisition blocks.
///
/// Returns whether a `generated_title_if_absent` was applied (see
/// [`Summary::apply_patch`]). Because the read-modify-write happens under the
/// lock, this "set the title only if absent" check is atomic against a
/// concurrent manual rename.
pub(crate) fn apply_patch_locked(
    summary_path: &Path,
    lock_path: &Path,
    patch: &SummaryPatch,
) -> io::Result<bool> {
    let lock = open_lock_file(lock_path)?;
    lock.lock_exclusive()?;
    let result = read_modify_write(summary_path, patch);
    let _ = lock.unlock();
    result
}

fn read_modify_write(summary_path: &Path, patch: &SummaryPatch) -> io::Result<bool> {
    let mut summary = read_summary(summary_path)?;
    let absent_title_applied = summary.apply_patch(patch, Utc::now());
    write_summary_atomic(summary_path, &summary)?;
    Ok(absent_title_applied)
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn read_summary(path: &Path) -> io::Result<Summary> {
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("summary.json is empty (0 bytes): {}", path.display()),
        ));
    }
    serde_json::from_slice::<Summary>(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_summary_atomic(summary_path: &Path, summary: &Summary) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(summary)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    crate::session::storage::write_bytes_atomic(summary_path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::info::Info;
    use crate::session::storage::StorageAdapter;
    use crate::session::storage::jsonl::JsonlStorageAdapter;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::Barrier;

    fn test_info() -> Info {
        Info {
            id: acp::SessionId::new("concurrent-summary-test"),
            cwd: "/test".into(),
        }
    }

    /// Regression guard for the `/resume` "frozen `last_active_at`" lost-update
    /// race. Two adapters (standing in for two persistence actors) hammer the
    /// SAME `summary.json` concurrently: one appends, the other writes metadata.
    /// Every write is a whole-summary read-modify-write, so without the sidecar
    /// lock the metadata writer reverts the appender's `num_messages` /
    /// `last_active_at` (and vice versa). The invariants below are exact, so a
    /// regression that drops the lock fails this deterministically: the counter
    /// must equal the number of appends and the monotonic field must not regress.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_do_not_lose_updates() {
        const N: usize = 300;
        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("session");
        let info = test_info();

        let init = JsonlStorageAdapter::with_explicit_session_dir(session_dir.clone());
        init.init_session(&info, acp::ModelId::new("test-model"))
            .await
            .unwrap();

        let appender = JsonlStorageAdapter::with_explicit_session_dir(session_dir.clone());
        let metadata = JsonlStorageAdapter::with_explicit_session_dir(session_dir.clone());
        let barrier = Arc::new(Barrier::new(2));

        let info_a = info.clone();
        let barrier_a = barrier.clone();
        let task_a = tokio::spawn(async move {
            barrier_a.wait().await;
            for _ in 0..N {
                appender
                    .apply_summary_patch(
                        &info_a,
                        SummaryPatch {
                            record_activity: true,
                            messages: Some(CounterOp::Increment(1)),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
            }
        });

        let info_b = info.clone();
        let barrier_b = barrier.clone();
        let task_b = tokio::spawn(async move {
            barrier_b.wait().await;
            for turn in 0..N {
                metadata
                    .apply_summary_patch(
                        &info_b,
                        SummaryPatch {
                            trace_turn: Some(TraceTurnPatch {
                                next_trace_turn: turn as u64,
                                request_id: None,
                            }),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
            }
        });

        task_a.await.unwrap();
        task_b.await.unwrap();

        let summary = read_summary(&session_dir.join("summary.json")).unwrap();
        assert_eq!(
            summary.num_messages, N,
            "lost an append increment to a racing metadata write",
        );
        assert_eq!(
            summary.next_trace_turn,
            (N - 1) as u64,
            "monotonic next_trace_turn regressed under contention",
        );
        assert!(
            summary.last_active_at.is_some(),
            "activity timestamp was lost",
        );
    }

    /// A freshly-initialized (untitled) session: returns its adapter and the
    /// path to the on-disk `summary.json`.
    async fn new_session(dir: &TempDir) -> (JsonlStorageAdapter, Info, std::path::PathBuf) {
        let session_dir = dir.path().join("session");
        let info = test_info();
        let adapter = JsonlStorageAdapter::with_explicit_session_dir(session_dir.clone());
        adapter
            .init_session(&info, acp::ModelId::new("test-model"))
            .await
            .unwrap();
        (adapter, info, session_dir.join("summary.json"))
    }

    /// Auto title generation writes (and reports `true`) when the session has
    /// no title yet, mirroring into `session_summary` for old clients.
    #[tokio::test]
    async fn auto_title_applies_when_session_has_no_title() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        let applied = adapter
            .set_generated_title_if_absent(&info, "Auto Title".into())
            .await
            .unwrap();

        assert!(applied);
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "Auto Title");
        assert_eq!(summary.session_summary, "Auto Title");
        assert!(!summary.title_is_manual);
        assert!(summary.manual_title_opt().is_none());
    }

    /// Regression guard for the `/rename`-during-turn race: an auto-generated
    /// title that lands after a manual `/rename` must not overwrite it, and
    /// must report `false` so callers skip the remote/registry sync.
    #[tokio::test]
    async fn auto_title_does_not_clobber_manual_rename() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        adapter
            .update_session_title(&info, "Manual Title".into())
            .await
            .unwrap();
        let applied = adapter
            .set_generated_title_if_absent(&info, "Auto Title".into())
            .await
            .unwrap();

        assert!(!applied);
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "Manual Title");
        assert_eq!(summary.manual_title_opt().as_deref(), Some("Manual Title"));
    }

    /// A manual `/rename` overwrites a title that was already auto-generated.
    #[tokio::test]
    async fn manual_rename_overrides_existing_auto_title() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        adapter
            .set_generated_title_if_absent(&info, "Auto Title".into())
            .await
            .unwrap();
        adapter
            .update_session_title(&info, "Manual Title".into())
            .await
            .unwrap();

        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "Manual Title");
        assert!(summary.title_is_manual, "manual rename must mark the title");
    }

    /// The race resolved under contention: whichever of a concurrent manual
    /// rename / auto title generation grabs the summary lock first, the manual
    /// title is always the final on-disk value (the unconditional manual write
    /// wins if it lands last; the auto write defers if it lands last). Many
    /// iterations so a regression to an unconditional auto overwrite — or
    /// moving the "if absent" check outside the lock — fails reliably.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_manual_rename_always_wins_over_auto_title() {
        for _ in 0..100 {
            let dir = TempDir::new().unwrap();
            let (adapter, info, summary_path) = new_session(&dir).await;
            let barrier = Arc::new(Barrier::new(2));

            let manual = adapter.clone();
            let info_m = info.clone();
            let barrier_m = barrier.clone();
            let task_m = tokio::spawn(async move {
                barrier_m.wait().await;
                manual
                    .update_session_title(&info_m, "Manual Title".into())
                    .await
                    .unwrap();
            });

            let auto = adapter.clone();
            let info_a = info.clone();
            let barrier_a = barrier.clone();
            let task_a = tokio::spawn(async move {
                barrier_a.wait().await;
                auto.set_generated_title_if_absent(&info_a, "Auto Title".into())
                    .await
                    .unwrap();
            });

            task_m.await.unwrap();
            task_a.await.unwrap();

            let summary = read_summary(&summary_path).unwrap();
            assert_eq!(summary.display_title(), "Manual Title");
            // Manual-ness survives the race in either landing order, so the
            // prompt-border title is restored on resume.
            assert!(summary.title_is_manual);
        }
    }

    /// `/rename --auto` after a rename-before-auto: both generated_title and
    /// the mirrored session_summary are cleared, so if-absent can adopt again.
    #[tokio::test]
    async fn reset_title_to_auto_clears_manual_and_unmirrors_equal_summary() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        adapter
            .update_session_title(&info, "Manual Title".into())
            .await
            .unwrap();
        let applied = adapter
            .set_generated_title_if_absent(&info, "Auto Title".into())
            .await
            .unwrap();
        assert!(!applied, "manual pin must block auto title");

        assert!(adapter.reset_title_to_auto(&info).await.unwrap());

        let summary = read_summary(&summary_path).unwrap();
        assert!(!summary.title_is_manual);
        assert!(summary.generated_title.is_none());
        assert!(
            summary.session_summary.is_empty(),
            "mirrored session_summary must be cleared so display_title is blank"
        );
        assert!(summary.display_title().trim().is_empty());
        assert!(summary.manual_title_opt().is_none());

        let applied = adapter
            .set_generated_title_if_absent(&info, "Fresh Auto".into())
            .await
            .unwrap();
        assert!(applied, "reset session must accept a subsequent auto title");
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "Fresh Auto");
        assert!(!summary.title_is_manual);
    }

    /// Common path: auto → `/rename` → `/rename --auto`. The leftover
    /// pre-rename auto title in `session_summary` must not block if-absent.
    #[tokio::test]
    async fn reset_after_auto_then_manual_blanks_display_and_accepts_if_absent() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        adapter
            .set_generated_title_if_absent(&info, "Auto Title".into())
            .await
            .unwrap();
        adapter
            .update_session_title(&info, "Manual Title".into())
            .await
            .unwrap();
        let before = read_summary(&summary_path).unwrap();
        assert_eq!(before.session_summary, "Auto Title");
        assert_eq!(before.display_title(), "Manual Title");

        assert!(adapter.reset_title_to_auto(&info).await.unwrap());

        let summary = read_summary(&summary_path).unwrap();
        assert!(!summary.title_is_manual);
        assert!(summary.generated_title.is_none());
        assert!(
            summary.session_summary.is_empty(),
            "pre-rename auto leftover must be cleared so if-absent can fire"
        );
        assert!(
            summary.display_title().trim().is_empty(),
            "load/resume would mark_done() if display_title stayed non-blank"
        );

        let applied = adapter
            .set_generated_title_if_absent(&info, "Fresh Auto".into())
            .await
            .unwrap();
        assert!(
            applied,
            "auto→rename→unpin must accept a subsequent auto title"
        );
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "Fresh Auto");
        assert!(!summary.title_is_manual);
    }

    /// Even a legacy refresh cannot replace the first adopted automatic title.
    #[tokio::test]
    async fn regenerate_preserves_initial_auto_title() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        adapter
            .set_generated_title_if_absent(&info, "First Prompt Title".into())
            .await
            .unwrap();
        let applied = adapter
            .regenerate_generated_title(&info, "Refined Real Topic".into())
            .await
            .unwrap();

        assert!(!applied);
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "First Prompt Title");
        assert_eq!(summary.session_summary, "First Prompt Title");
        assert!(!summary.title_is_manual);
    }

    #[tokio::test]
    async fn initial_title_survives_followups_resume_and_summary_updates() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;
        assert!(
            adapter
                .set_generated_title_if_absent(&info, "首问任务".into())
                .await
                .unwrap()
        );
        for turn in 2..=8 {
            let resumed = JsonlStorageAdapter::with_explicit_session_dir(
                summary_path.parent().unwrap().to_path_buf(),
            );
            assert!(
                !resumed
                    .regenerate_generated_title(&info, format!("第 {turn} 轮新话题"))
                    .await
                    .unwrap()
            );
            assert!(
                !resumed
                    .set_generated_title_if_absent(&info, "重复的初始结果".into())
                    .await
                    .unwrap()
            );
            resumed
                .set_last_recap(&info, Some(format!("第 {turn} 轮进展")))
                .await
                .unwrap();
        }
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "首问任务");
        assert_eq!(summary.session_summary, "首问任务");
        assert_eq!(summary.last_recap.as_deref(), Some("第 8 轮进展"));
        assert!(!summary.title_is_manual);
        assert!(
            summary.last_active_at.is_none(),
            "metadata is not new user activity"
        );
    }

    #[tokio::test]
    async fn concurrent_refresh_and_manual_rename_preserve_the_users_choice() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;
        adapter
            .set_generated_title_if_absent(&info, "首问任务".into())
            .await
            .unwrap();
        let (refresh, rename) = tokio::join!(
            adapter.regenerate_generated_title(&info, "后续新话题".into()),
            adapter.update_session_title(&info, "用户指定名称".into()),
        );
        assert!(!refresh.unwrap());
        rename.unwrap();
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "用户指定名称");
        assert!(summary.title_is_manual);
    }

    #[tokio::test]
    async fn legacy_summary_titles_and_empty_results_do_not_reopen_naming() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;
        assert!(
            !adapter
                .set_generated_title_if_absent(&info, "  ".into())
                .await
                .unwrap()
        );
        let mut summary = read_summary(&summary_path).unwrap();
        summary.session_summary = "历史会话标题".into();
        write_summary_atomic(&summary_path, &summary).unwrap();
        assert!(
            !adapter
                .regenerate_generated_title(&info, "新话题".into())
                .await
                .unwrap()
        );
        assert_eq!(
            read_summary(&summary_path).unwrap().display_title(),
            "历史会话标题"
        );
    }

    /// A title refresh never overwrites a manual `/rename`, and reports `false`.
    #[tokio::test]
    async fn regenerate_never_clobbers_manual_title() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        adapter
            .update_session_title(&info, "Manual Title".into())
            .await
            .unwrap();
        let applied = adapter
            .regenerate_generated_title(&info, "Auto Refresh".into())
            .await
            .unwrap();

        assert!(!applied);
        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(summary.display_title(), "Manual Title");
        assert!(summary.title_is_manual);
    }

    /// A recap persists into `summary.json` (last-writer-wins) and clears on
    /// rewind, separate from the last-turn summary.
    #[tokio::test]
    async fn set_last_recap_persists_and_clears() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        adapter
            .set_last_recap(&info, Some("Where we left off: fixing the parser".into()))
            .await
            .unwrap();

        let summary = read_summary(&summary_path).unwrap();
        assert_eq!(
            summary.last_recap.as_deref(),
            Some("Where we left off: fixing the parser")
        );
        // Distinct from the last-turn summary.
        assert!(summary.last_turn_summary.is_none());

        // A rewind clears it.
        adapter.set_last_recap(&info, None).await.unwrap();
        let summary = read_summary(&summary_path).unwrap();
        assert!(summary.last_recap.is_none());
    }

    /// Unpin on a never-renamed / auto-titled session is a no-op: the auto
    /// title stays, the flag stays false.
    #[tokio::test]
    async fn reset_title_to_auto_is_noop_when_not_manual() {
        let dir = TempDir::new().unwrap();
        let (adapter, info, summary_path) = new_session(&dir).await;

        assert!(!adapter.reset_title_to_auto(&info).await.unwrap());
        let empty = read_summary(&summary_path).unwrap();
        assert!(!empty.title_is_manual);
        assert!(empty.generated_title.is_none());

        adapter
            .set_generated_title_if_absent(&info, "Auto Title".into())
            .await
            .unwrap();
        assert!(!adapter.reset_title_to_auto(&info).await.unwrap());
        let summary = read_summary(&summary_path).unwrap();
        assert!(!summary.title_is_manual);
        assert_eq!(summary.display_title(), "Auto Title");
        assert_eq!(summary.session_summary, "Auto Title");
        assert!(
            !adapter
                .set_generated_title_if_absent(&info, "Fresh".into())
                .await
                .unwrap(),
            "no-op unpin must leave the auto title blocking if-absent"
        );
    }

    /// Reset and a racing auto-title never leave a manual pin on disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_reset_and_auto_title_never_leave_manual_pin() {
        for _ in 0..50 {
            let dir = TempDir::new().unwrap();
            let (adapter, info, summary_path) = new_session(&dir).await;
            adapter
                .update_session_title(&info, "Manual Title".into())
                .await
                .unwrap();
            let barrier = Arc::new(Barrier::new(2));

            let reset = adapter.clone();
            let info_r = info.clone();
            let barrier_r = barrier.clone();
            let task_r = tokio::spawn(async move {
                barrier_r.wait().await;
                reset.reset_title_to_auto(&info_r).await.unwrap();
            });

            let auto = adapter.clone();
            let info_a = info.clone();
            let barrier_a = barrier.clone();
            let task_a = tokio::spawn(async move {
                barrier_a.wait().await;
                auto.set_generated_title_if_absent(&info_a, "Auto Title".into())
                    .await
                    .unwrap();
            });

            task_r.await.unwrap();
            task_a.await.unwrap();

            let summary = read_summary(&summary_path).unwrap();
            assert!(
                !summary.title_is_manual,
                "reset must clear the manual pin under contention"
            );
            assert_ne!(summary.display_title(), "Manual Title");
        }
    }
}
