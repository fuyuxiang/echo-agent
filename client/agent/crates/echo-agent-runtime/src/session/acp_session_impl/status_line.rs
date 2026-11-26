//! Building the status-line payload and pushing it to clients.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::*;

use crate::extensions::notification::{PromptUsage, PromptUsageModel, ticks_to_usd};
use echo_agent_status_line::{
    STATUS_LINE_SCHEMA_VERSION, StatusLineContext, StatusLineContextWindow, StatusLineCost,
    StatusLineEffort, StatusLineModel, StatusLineRepo, StatusLineSessionUsage, StatusLineTurn,
    StatusLineWorkspace, StatusLineWorktree,
};
use echo_agent_workspace::session::git::normalize_repo_url;

#[derive(Default)]
struct RepoState {
    repo_root: Option<PathBuf>,
    repo: Option<StatusLineRepo>,
    is_worktree: bool,
    main_root: Option<PathBuf>,
    branch: Option<String>,
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn strip_trailing_separator(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.trim_end_matches('/') {
        "" => PathBuf::from("/"),
        trimmed => PathBuf::from(trimmed),
    }
}

fn remote_url(repo: &git2::Repository) -> Option<String> {
    let origin = repo.find_remote("origin").ok()?;
    origin.url().ok().map(str::to_string)
}

fn split_normalized_remote(remote: &str) -> Option<StatusLineRepo> {
    let (host, path) = remote.split_once('/')?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let name = segments.next_back()?;
    let owner = segments.next_back();
    (!host.is_empty() && !name.is_empty()).then(|| StatusLineRepo {
        host: host.to_string(),
        owner: owner.map(str::to_string),
        name: name.to_string(),
    })
}

fn build_worktree(state: &RepoState, cwd: &Path, branch: Option<String>) -> StatusLineWorktree {
    let path = state.repo_root.as_deref().unwrap_or(cwd);
    StatusLineWorktree {
        name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
        path: path_string(path),
        branch,
        main_worktree_root: state.main_root.as_deref().map(path_string),
    }
}

fn build_context_window(
    size: u64,
    used_tokens: Option<u64>,
    totals: Option<&PromptUsageModel>,
    auto_compact_threshold_percent: u8,
) -> StatusLineContextWindow {
    // The shared rounding, not a fourth spelling of it: the field is omitted
    // rather than zero when the window is unknown, which is the only part the
    // helper cannot express.
    let used_percentage = used_tokens
        .filter(|_| size > 0)
        .map(|used| echo_agent_token_estimation::usage_percentage_u8(used, size));
    StatusLineContextWindow {
        context_window_size: (size > 0).then_some(size),
        context_tokens: used_tokens,
        session_input_tokens: totals.map(|t| t.input_tokens),
        session_output_tokens: totals.map(|t| t.output_tokens),
        session_usage: totals.filter(|t| t.model_calls > 0).map(|t| {
            // The three buckets are disjoint and must sum to `input_tokens`;
            // a violation would zero the fresh-input figure and desync the
            // reported totals, so catch a ledger regression in CI.
            debug_assert!(
                t.input_tokens >= t.cached_read_tokens + t.cache_creation_tokens,
                "input_tokens {} < cached_read {} + cache_creation {}",
                t.input_tokens,
                t.cached_read_tokens,
                t.cache_creation_tokens,
            );
            StatusLineSessionUsage {
                input_tokens: t
                    .input_tokens
                    .saturating_sub(t.cached_read_tokens)
                    .saturating_sub(t.cache_creation_tokens),
                output_tokens: t.output_tokens,
                cache_creation_input_tokens: t.cache_creation_tokens,
                cache_read_input_tokens: t.cached_read_tokens,
            }
        }),
        used_percentage,
        remaining_percentage: used_percentage.map(|pct| 100 - pct),
        auto_compact_threshold_percent: (auto_compact_threshold_percent > 0)
            .then_some(auto_compact_threshold_percent),
    }
}

/// The turn in flight, `None` between turns. Chat state keeps the start stamp
/// after a turn ends, because the laziness classifier reads it, so the stamp
/// alone would report a turn that finished. The prompt id is what a guard
/// clears when the turn does.
fn live_turn(started_at_ms: Option<i64>, prompt_id: Option<&str>) -> Option<StatusLineTurn> {
    started_at_ms
        .filter(|_| prompt_id.is_some())
        .map(|started_at_ms| StatusLineTurn { started_at_ms })
}

impl SessionActor {
    pub(super) async fn build_status_context(&self) -> StatusLineContext {
        let config = self.chat_state_handle.get_sampling_config().await;
        let model_id = config.as_ref().map(|c| c.model.clone());
        let context_window_size = config.as_ref().map_or(0, |c| c.context_window.get());
        let effort = config
            .as_ref()
            .and_then(|c| c.reasoning_effort)
            .map(|level| StatusLineEffort {
                level: level.to_string(),
            });
        let display_name = model_id.as_ref().map(|id| {
            self.models_manager
                .display_name(id)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| id.clone())
        });

        // A failed read stays absent rather than 0, which renders as `0% ctx`.
        let used_tokens = self
            .chat_state_handle
            .try_get_estimated_total_tokens()
            .await;

        let usage = self
            .chat_state_handle
            .try_get_session_usage()
            .await
            .ok()
            .map(|ledger| PromptUsage::from(&ledger));
        let totals = usage.as_ref().map(|u| &u.totals);

        let cwd = self.tool_context.cwd.as_path().to_path_buf();
        // Both stats run on the blocking pool, off the actor's thread.
        let transcript = self.transcript_path();
        let (repo_state, transcript_path, turn_start_ms) = tokio::join!(
            Self::repo_state(cwd.clone()),
            tokio::task::spawn_blocking(move || {
                transcript
                    .exists()
                    .then(|| transcript.to_string_lossy().into_owned())
            }),
            async {
                self.chat_state_handle
                    .get_notification_meta()
                    .await
                    .and_then(|meta| meta.turn_start_ms)
            },
        );
        let transcript_path = transcript_path.unwrap_or_default();

        let branch = repo_state.branch.clone().filter(|b| !b.is_empty());
        let worktree = repo_state
            .is_worktree
            .then(|| build_worktree(&repo_state, &cwd, branch.clone()));
        let prompt_id = match self.current_prompt_id.lock() {
            Ok(id) => id.clone(),
            // Recovered rather than dropped: the value behind the lock is one
            // optional id, which a panic elsewhere cannot leave half-written,
            // and losing it would stop the turn timer for the session. Logged
            // because the panic that poisoned it is worth knowing about.
            Err(poisoned) => {
                tracing::warn!(
                    "status_line: current_prompt_id lock poisoned; using its last value"
                );
                poisoned.into_inner().clone()
            }
        };
        let cwd = path_string(&cwd);
        let repo_root = repo_state.repo_root.as_deref().map(path_string);

        StatusLineContext {
            schema_version: Some(STATUS_LINE_SCHEMA_VERSION),
            cwd: cwd.clone(),
            session_id: Some(self.session_info.id.0.to_string()),
            session_name: None,
            prompt_id: prompt_id.clone(),
            transcript_path,
            model: StatusLineModel {
                id: model_id,
                display_name,
            },
            workspace: StatusLineWorkspace {
                current_dir: cwd,
                repo_root,
                branch,
                git_worktree: worktree.as_ref().and_then(|w| w.name.clone()),
                repo: repo_state.repo,
            },
            version: echo_agent_version::VERSION.to_string(),
            cost: StatusLineCost {
                total_cost_usd: totals.and_then(|t| t.cost_usd_ticks).map(ticks_to_usd),
                total_duration_ms: self.session_start.elapsed().as_millis() as u64,
                total_api_duration_ms: totals.map(|t| t.api_duration_ms),
            },
            context_window: build_context_window(
                context_window_size,
                used_tokens,
                totals,
                self.compaction.threshold_percent.get(),
            ),
            effort,
            worktree,
            turn: live_turn(turn_start_ms, prompt_id.as_deref()),
            // Like `session_name`: a run property the client stamps, not the
            // agent's to send.
            trigger: None,
        }
    }

    async fn repo_state(cwd: PathBuf) -> RepoState {
        tokio::task::spawn_blocking(move || {
            let Ok(repo) = git2::Repository::discover(&cwd) else {
                return RepoState::default();
            };
            let common_dir = repo.commondir().to_path_buf();
            let is_worktree = repo.path() != common_dir;
            let branch = match repo.head_detached() {
                Ok(false) => repo
                    .head()
                    .ok()
                    .and_then(|h| h.shorthand().ok().map(str::to_string)),
                Ok(true) | Err(_) => None,
            };
            RepoState {
                branch,
                repo_root: repo.workdir().map(strip_trailing_separator),
                repo: remote_url(&repo)
                    .as_deref()
                    .and_then(normalize_repo_url)
                    .as_deref()
                    .and_then(split_normalized_remote),
                is_worktree,
                main_root: is_worktree
                    .then(|| common_dir.parent().map(strip_trailing_separator))
                    .flatten(),
            }
        })
        .await
        .unwrap_or_default()
    }

    /// Wakes [`run_status_emitter`] rather than building inline: the payload
    /// takes a git discovery and three chat-state round trips, nothing waits on it.
    pub(crate) fn emit_status_snapshot_detached(&self) {
        self.status_wake.notify_one();
    }

    async fn emit_status_snapshot(&self) {
        // `send_echo_agent_notification_transient` checks this too; here it skips the
        // build, which an attach re-requests once the gate is open.
        if !self.notifications.gateway_enabled.load(Ordering::Relaxed) {
            return;
        }
        let context = self.build_status_context().await;
        self.send_echo_agent_notification_transient(EchoAgentSessionUpdate::SessionStatus(
            Box::new(context),
        ));
    }
}

/// Seeds the row, then rebuilds it once per wake. The single enforcement point
/// for the capability: every other trigger only wakes this loop, and the
/// capability is re-read each pass, since a resident session outlives the client
/// that created it. `is_subagent` cannot change, so it is read once. The session
/// is held only across a build, so an idle emitter does not keep a finished one
/// and its MCP clients alive.
pub(super) async fn run_status_emitter(session: std::sync::Weak<SessionActor>) {
    let wake = match session.upgrade() {
        Some(s) if !s.startup_hints.is_subagent => s.status_wake.handle(),
        _ => return,
    };
    emit_loop(wake, || {
        let session = session.upgrade()?;
        Some(async move {
            if session.status_line_enabled.load(Ordering::Relaxed) {
                session.emit_status_snapshot().await;
            }
        })
    })
    .await;
}

/// The emitter's wake, which also ends it: dropping this wakes the loop a last
/// time and the upgrade that follows fails. Otherwise the task parks on a wake
/// nobody will send, for the life of a process whose sessions share one
/// `LocalSet`. A type rather than `impl Drop for SessionActor`, which would
/// forbid moving fields out of the actor, as several call sites do.
#[derive(Debug, Default)]
pub(crate) struct StatusWake(Arc<tokio::sync::Notify>);

impl StatusWake {
    /// A handle for something that only signals, and so must not end the loop
    /// when it goes away.
    pub(crate) fn handle(&self) -> Arc<tokio::sync::Notify> {
        self.0.clone()
    }

    pub(crate) fn notify_one(&self) {
        self.0.notify_one();
    }
}

impl Drop for StatusWake {
    fn drop(&mut self) {
        // `notify_one`, not `notify_waiters`: a session dropped the moment a
        // build finishes has no waiter yet, and only `notify_one` leaves the
        // permit that releases the park that comes next.
        self.0.notify_one();
    }
}

/// Builds once, then once more per wake. Awaiting each build before the next
/// prevents two racing, and `Notify` collapses a burst into one extra build.
async fn emit_loop<F, Fut>(wake: Arc<tokio::sync::Notify>, mut build: F)
where
    F: FnMut() -> Option<Fut>,
    Fut: Future<Output = ()>,
{
    loop {
        match build() {
            Some(snapshot) => snapshot.await,
            None => return,
        }
        wake.notified().await;
    }
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod tests;
