//! Dashboard rows: classification, build, filter, sort.
use super::state::{DashboardRowId, Filter, RowState};
use crate::acp::tracker::TurnActivity;
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::app::roster::{RosterActivity, RosterEntry};
use crate::app::subagent::{SubagentInfo, format_activity_label, format_subagent_label};
use indexmap::IndexMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};
/// Title prefix for a session that has no name / generated title / prompt
/// yet. The renderer paints this part in the primary colour and the trailing
/// ` #<id>` suffix in dim gray (see `render::render_row`).
pub(crate) const NEW_SESSION_LABEL: &str = "New session";
/// A single row in the dashboard. Built per-frame from `app.agents`.
#[derive(Debug, Clone)]
pub struct DashboardRow {
    pub id: DashboardRowId,
    /// Display label (e.g. `"implementer · fix login bug"`).
    pub label: String,
    /// Right-of-label subtitle painted after a ` · ` separator in
    /// dim text (e.g. `"echoagent my-branch-2 worktree"`). `None` when the
    /// row has no repo / branch context worth surfacing.
    pub subtitle: Option<String>,
    /// Coarse state used for grouping.
    pub state: RowState,
    /// Activity summary (e.g. `"Running: cargo test"`). `None` when
    /// idle or finished.
    pub activity: Option<String>,
    /// Secondary dim line painted directly below the title row —
    /// holds the last tool call, the last assistant message, or
    /// (for `NeedsInput`) a "Pending: …" preview of the front-most
    /// permission request. `None` collapses the row to a single
    /// line (e.g. very recently created with no activity yet).
    pub secondary_line: Option<String>,
    /// Working directory display string (already compacted to `~/...`).
    pub cwd_display: String,
    /// Raw cwd for downstream filter / grouping consumers.
    pub cwd: PathBuf,
    /// Wall-clock moment of the row's last change — drives the age column and
    /// is the recency tiebreaker for sort (more recent floats higher in its
    /// group).
    ///
    /// Wall-clock [`SystemTime`], not a monotonic [`Instant`]: roster
    /// timestamps can predate this process — even the machine's boot — which
    /// an `Instant` cannot represent (its floor is system boot, so an older
    /// moment underflows and collapses back to "just now"). Local rows project
    /// their live `Instant` anchors onto the wall clock via
    /// [`crate::util::system_time_from_instant`].
    pub last_change_at: SystemTime,
    /// True when this row is pinned (always floats above non-pinned).
    pub pinned: bool,
    /// True when this row is currently the active view target (so the
    /// renderer can highlight it). Reserved — wired through but
    /// always `false` in v1; populated when we eventually support
    /// "the agent you came from" highlighting from the dashboard.
    #[allow(dead_code)]
    pub is_active: bool,
    /// Optional row badges (e.g. `worktree`, `needs-input`).
    pub badges: Vec<RowBadge>,
    /// Context window usage percent, when known. Drives the mini gauge.
    pub context_pct: Option<u8>,
    /// Indent level for nested subagent rendering. 0 = top-level,
    /// 1 = subagent under its parent.
    pub indent: u8,
    /// Display title for the parent agent — used to scope group
    /// headers in `Grouping::Directory` mode where subagents must
    /// sort with their parent.
    pub parent_label: Option<String>,
    /// True when this row is a "… N more" collapse placeholder. The
    /// renderer paints it dimmed and the cursor cannot land on it
    /// (selectable=false).
    pub is_more_placeholder: bool,
    /// Number of rolled-up rows when `is_more_placeholder == true`.
    pub more_count: usize,
}
/// Compact badge rendered next to the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowBadge {
    Worktree,
    NeedsInput,
    BgTask,
    Pinned,
    Failed,
}
impl RowBadge {
    pub fn label(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::NeedsInput => "needs-input",
            Self::BgTask => "bg",
            Self::Pinned => "pinned",
            Self::Failed => "failed",
        }
    }
}
/// Maximum number of subagents to show per parent before collapsing
/// completed/failed ones into a "… N more" row (edge case 6).
pub const MAX_VISIBLE_SUBAGENTS: usize = 8;
/// Process-wide fallback anchor for the age column when an agent
/// somehow lacks both `turn_started_at` AND `last_active_at` (rare —
/// `AgentView::new` always stamps the latter, but a hypothetical
/// future caller could omit it). Initialised lazily on first call so
/// the fallback is genuinely frozen rather than re-anchoring every
/// frame.
fn fallback_epoch() -> Instant {
    static FALLBACK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *FALLBACK.get_or_init(Instant::now)
}
/// Build the dashboard row list from the live `app.agents` map.
///
/// Iterates `agents` so a stale `AgentId` can never panic (edge case 2).
/// Subagents are appended directly after their parent — never reordered
/// across parents — and a `… N more` collapse placeholder appears at
/// the parent's scope once more than [`MAX_VISIBLE_SUBAGENTS`] rows
/// exist (edge case 6).
pub fn build_rows(
    agents: &IndexMap<AgentId, AgentView>,
    pinned: &std::collections::BTreeSet<DashboardRowId>,
    reorder: &[DashboardRowId],
    active: Option<AgentId>,
    grouping: super::state::Grouping,
    filter: &Filter,
    home: Option<&str>,
) -> Vec<DashboardRow> {
    let mut rows = build_local_rows(agents, pinned, active, home, true);
    apply_filter(&mut rows, filter, home);
    sort_rows(&mut rows, grouping, reorder);
    rows
}
/// Build the row list shown by the live dashboard (rendering in
/// [`super::render::render_dashboard`] and keyboard navigation in
/// `dispatch_dashboard_select` both go through here).
///
/// Like [`build_rows`] but (a) appends "roster-only" rows for leader
/// sessions this client is NOT locally attached to (FleetView dashboard)
/// and (b) does NOT surface subagents as their own rows — only top-level
/// agents and roster sessions are listed (see [`build_local_rows`]).
///
/// Local `AgentView` rows are built first (richest data). Then, for each
/// [`RosterEntry`] whose `session_id` is not already represented by a
/// local agent, a synthetic [`DashboardRow`] is appended. In non-leader
/// mode `roster` is empty.
#[allow(clippy::too_many_arguments)]
pub fn build_rows_with_roster(
    agents: &IndexMap<AgentId, AgentView>,
    pinned: &std::collections::BTreeSet<DashboardRowId>,
    reorder: &[DashboardRowId],
    active: Option<AgentId>,
    grouping: super::state::Grouping,
    filter: &Filter,
    home: Option<&str>,
    roster: &[RosterEntry],
) -> Vec<DashboardRow> {
    let mut rows = build_local_rows(agents, pinned, active, home, false);
    append_roster_rows(&mut rows, roster, agents, pinned, home);
    apply_filter(&mut rows, filter, home);
    sort_rows(&mut rows, grouping, reorder);
    rows
}
/// Build the local-agent rows WITHOUT applying filter or sort. Shared by
/// [`build_rows`] and [`build_rows_with_roster`].
///
/// `include_subagents` controls whether each parent's subagent sessions are
/// appended as nested rows. The live dashboard passes `false` (subagents are
/// not surfaced as their own rows — see [`build_rows_with_roster`]); callers
/// that want the full tree (and the row-building unit tests) pass `true` via
/// [`build_rows`].
fn build_local_rows(
    agents: &IndexMap<AgentId, AgentView>,
    pinned: &std::collections::BTreeSet<DashboardRowId>,
    active: Option<AgentId>,
    home: Option<&str>,
    include_subagents: bool,
) -> Vec<DashboardRow> {
    let mut rows = Vec::new();
    for (id, agent) in agents.iter() {
        let top_id = DashboardRowId::TopLevel(*id);
        if is_empty_top_level(agent)
            && !pinned.contains(&top_id)
            && !matches!(
                classify_top_level(agent),
                RowState::Working | RowState::NeedsInput
            )
        {
            continue;
        }
        let row = top_level_row(
            *id,
            agent,
            pinned.contains(&top_id),
            active == Some(*id),
            home,
        );
        rows.push(row);
        if !include_subagents {
            continue;
        }
        let mut subagents: Vec<&SubagentInfo> = agent
            .subagent_sessions
            .values()
            .filter(|info| info.workflow_run_id.is_none())
            .collect();
        subagents.sort_by(|a, b| {
            let a_running = !a.finished;
            let b_running = !b.finished;
            match (a_running, b_running) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.started_at.cmp(&a.started_at),
            }
        });
        let total = subagents.len();
        let keep = MAX_VISIBLE_SUBAGENTS.min(total);
        for info in subagents.iter().take(keep) {
            rows.push(subagent_row(
                *id,
                agent,
                info,
                pinned.contains(&DashboardRowId::Subagent {
                    parent: *id,
                    child_session_id: info.child_session_id.to_string(),
                }),
                home,
            ));
        }
        if total > keep {
            let agent_label = top_level_label(agent);
            rows.push(DashboardRow {
                id: DashboardRowId::Subagent {
                    parent: *id,
                    child_session_id: format!("__more_{}", id.0),
                },
                label: format!("\u{2026} {} more", total - keep),
                subtitle: None,
                state: RowState::Idle,
                activity: None,
                secondary_line: None,
                cwd_display: String::new(),
                cwd: agent.session.cwd.clone(),
                last_change_at: SystemTime::now(),
                pinned: false,
                is_active: false,
                badges: Vec::new(),
                context_pct: None,
                indent: 1,
                parent_label: Some(agent_label),
                is_more_placeholder: true,
                more_count: total - keep,
            });
        }
    }
    rows
}
/// Map a leader [`RosterActivity`] to the dashboard's coarse [`RowState`].
/// Public so the dispatcher can gate roster-row deletion through the very
/// same `RowState::allows_delete` predicate the renderer paints `[✗]` with.
pub fn roster_activity_to_state(activity: RosterActivity) -> RowState {
    match activity {
        RosterActivity::Working => RowState::Working,
        RosterActivity::NeedsInput => RowState::NeedsInput,
        RosterActivity::Idle | RosterActivity::Dormant => RowState::Inactive,
        RosterActivity::Completed => RowState::Completed,
        RosterActivity::Dead => RowState::Failed,
    }
}
/// Append "roster-only" rows for leader sessions not represented by a
/// local `AgentView`. Skips any roster entry whose `session_id` already
/// matches a locally-attached agent (those carry richer data via
/// [`build_local_rows`]).
fn append_roster_rows(
    rows: &mut Vec<DashboardRow>,
    roster: &[RosterEntry],
    agents: &IndexMap<AgentId, AgentView>,
    pinned: &std::collections::BTreeSet<DashboardRowId>,
    home: Option<&str>,
) {
    if roster.is_empty() {
        return;
    }
    let local_ids: std::collections::HashSet<&str> = agents
        .values()
        .filter_map(|a| a.session.session_id.as_ref().map(|s| s.0.as_ref()))
        .collect();
    for entry in roster {
        if local_ids.contains(entry.session_id.as_str()) {
            continue;
        }
        let id = DashboardRowId::Roster {
            session_id: entry.session_id.clone(),
        };
        let has_title = entry
            .title
            .as_deref()
            .map(str::trim)
            .is_some_and(|t| !t.is_empty());
        let active = matches!(
            entry.activity,
            RosterActivity::Working | RosterActivity::NeedsInput
        );
        if !has_title && !active && !pinned.contains(&id) {
            continue;
        }
        let cwd = PathBuf::from(&entry.cwd);
        let label = entry
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(sanitize)
            .or_else(|| {
                cwd.file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .map(sanitize)
            })
            .unwrap_or_else(|| sanitize(&entry.session_id));
        let state = roster_activity_to_state(entry.activity);
        let activity = match state {
            RowState::NeedsInput => Some("Awaiting input".to_string()),
            RowState::Working => Some("Working".to_string()),
            _ => None,
        };
        let mut badges = Vec::new();
        if entry.is_worktree {
            badges.push(RowBadge::Worktree);
        }
        if entry.activity == RosterActivity::NeedsInput {
            badges.push(RowBadge::NeedsInput);
        }
        let cwd_display = super::state::compact_cwd(&cwd, home);
        let is_pinned = pinned.contains(&id);
        rows.push(DashboardRow {
            id,
            label,
            subtitle: None,
            state,
            activity,
            secondary_line: entry
                .last_turn_summary
                .as_deref()
                .or(entry.model_id.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(sanitize),
            cwd_display,
            cwd,
            last_change_at: crate::util::system_time_from_unix_ms(entry.last_change_unix_ms),
            pinned: is_pinned,
            is_active: false,
            badges,
            context_pct: None,
            indent: 0,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        });
    }
}
/// Classify a top-level agent.
///
/// Returns one of [`RowState::NeedsInput`], [`RowState::Working`],
/// [`RowState::Idle`].
///
/// `NeedsInput` triggers on pending permission OR pending
/// `QuestionView` (`ask_user_question` tool). `Working` covers any
/// non-idle session state, including command runs (compaction,
/// worktree create, etc.) — which is why edge case 14 (compaction)
/// works: a row in `AgentState::CommandRunning { Compact, .. }` is
/// "Working" and the activity label reads `Compacting`. A turn-idle
/// agent still classifies as `Working` while it has live background work
/// (`has_background_work`): a running background task / `monitor` or an
/// active scheduled `/loop`. Each is ongoing, user-dispatched work — and
/// monitors / loops can wake the agent for a fresh turn — so the agent
/// isn't meaningfully idle while any are running.
pub fn classify_top_level(agent: &AgentView) -> RowState {
    if !agent.permission_queue.is_empty() || agent.question_view.is_some() {
        return RowState::NeedsInput;
    }
    if !agent.session.state.is_idle()
        || agent.wake_turn_active()
        || agent.session.turn_activity().is_some()
        || !agent.session.pending_prompts.is_empty()
    {
        return RowState::Working;
    }
    if agent.session.loading_replay {
        return RowState::Working;
    }
    if has_background_work(agent) {
        return RowState::Working;
    }
    RowState::Idle
}
/// Whether `agent` has live background work that keeps it out of the
/// `Idle` group even when its turn is idle: a running background task
/// (`run_terminal_command` with `background=true`), a running `monitor`
/// (a background task with `is_monitor`), or an active scheduled `/loop`.
/// Mirrors the agent view's idle "watching" cue
/// (`crate::views::turn_status::Watchers`, minus subagents — the dashboard
/// lists those as their own rows) — any in-flight background work the user
/// dispatched should read as "Working" on the dashboard.
pub fn has_background_work(agent: &AgentView) -> bool {
    agent
        .session
        .bg_tasks
        .values()
        .any(|t| t.status == crate::app::agent::BgTaskStatus::Running)
        || !agent.session.scheduled_tasks.is_empty()
}
/// Compact `"… still running"` label summarising a turn-idle agent's live
/// background work — e.g. `"1 monitor · 2 loops still running"` or
/// `"1 task still running"`. `None` when there's no background work (the
/// caller then falls back to a bare `"Working"`). Shares the format
/// mechanics with the agent view's idle cue
/// ([`crate::views::turn_status::format_still_running`]) but keeps the
/// dashboard's own nouns ("task", not "command") and omits subagents —
/// dashboard rows list those separately. Counts come from local state (not
/// backend content), so no sanitise.
fn background_work_label(agent: &AgentView) -> Option<String> {
    let w = agent.watchers();
    crate::views::turn_status::format_still_running([
        (w.monitors, "monitor"),
        (w.loops, "loop"),
        (w.commands, "task"),
    ])
}
/// Classify a subagent.
///
/// Subagents never enter `NeedsInput` in v1 — they have no user-prompt
/// channel. Asserted in tests.
pub fn classify_subagent(info: &SubagentInfo) -> RowState {
    if info.finished {
        match info.status.as_deref() {
            Some("failed") | Some("cancelled") | Some("error") => RowState::Failed,
            _ => RowState::Completed,
        }
    } else {
        RowState::Working
    }
}
/// Issues 30 + 31 fix — sanitise every string derived from
/// backend / model-controlled content before returning. The
/// dashboard's renderer paints raw via `set_string`, which preserves
/// embedded escape sequences in the ratatui buffer.
fn sanitize(s: &str) -> String {
    crate::views::session_title::sanitize_display_text(s).into_owned()
}
/// Whether a top-level agent has no real conversation yet: no display name, no
/// generated title, no queued prompt, and no user-typed message in scrollback.
///
/// These are the exact conditions under which [`top_level_label`] falls back to
/// `New session #<id>`. A session created on pager launch carries only the
/// system prompt plus injected `<system-reminder>` context (which are not real
/// user turns and never render as `UserPrompt` blocks), so it reads as empty
/// here until the user actually sends something. Used by [`build_local_rows`]
/// to keep such sessions off the dashboard.
fn is_empty_top_level(agent: &AgentView) -> bool {
    let has_text = |s: Option<&str>| s.map(str::trim).is_some_and(|t| !t.is_empty());
    if has_text(agent.display_name.as_deref()) {
        return false;
    }
    if has_text(agent.generated_session_title.as_deref()) {
        return false;
    }
    if !agent.session.pending_prompts.is_empty() {
        return false;
    }
    if !agent.subagent_sessions.is_empty() || !agent.session.bg_tasks.is_empty() {
        return false;
    }
    if super::peek::extract_first_user_message(agent).is_some() {
        return false;
    }
    true
}
fn top_level_label(agent: &AgentView) -> String {
    if let Some(name) = agent.display_name.as_deref() {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return sanitize(trimmed);
        }
    }
    if let Some(title) = agent.generated_session_title.as_deref() {
        let trimmed = title.trim();
        if !trimmed.is_empty() {
            return sanitize(trimmed);
        }
    }
    if let Some(first) = agent.session.pending_prompts.front() {
        let preview: String = first.text.trim().chars().take(30).collect();
        if !preview.is_empty() {
            return sanitize(&preview);
        }
    }
    if let Some(first_msg) = super::peek::extract_first_user_message(agent) {
        let preview: String = first_msg.chars().take(30).collect();
        if !preview.is_empty() {
            return preview;
        }
    }
    if let Some(sid) = agent.session.session_id.as_ref() {
        let short: String = sid.0.chars().take(8).collect();
        return format!("{NEW_SESSION_LABEL} #{short}");
    }
    NEW_SESSION_LABEL.to_string()
}
fn top_level_row(
    id: AgentId,
    agent: &AgentView,
    pinned: bool,
    is_active: bool,
    home: Option<&str>,
) -> DashboardRow {
    let state = classify_top_level(agent);
    let label = top_level_label(agent);
    let subtitle = top_level_subtitle(agent);
    let activity = top_level_activity(agent, state);
    let secondary_line = top_level_secondary_line(agent, state, activity.as_deref());
    let anchor: Instant = match state {
        RowState::Working => agent
            .turn_started_at
            .or(agent.last_active_at)
            .unwrap_or_else(fallback_epoch),
        RowState::NeedsInput
        | RowState::Idle
        | RowState::Inactive
        | RowState::Completed
        | RowState::Failed
        | RowState::Blocked => agent.last_active_at.unwrap_or_else(fallback_epoch),
    };
    let last_change_at = crate::util::system_time_from_instant(anchor);
    let mut badges = Vec::new();
    if agent.is_worktree {
        badges.push(RowBadge::Worktree);
    }
    if state == RowState::NeedsInput {
        badges.push(RowBadge::NeedsInput);
    }
    if agent
        .session
        .bg_tasks
        .values()
        .any(|t| t.status == crate::app::agent::BgTaskStatus::Running)
    {
        badges.push(RowBadge::BgTask);
    }
    if pinned {
        badges.push(RowBadge::Pinned);
    }
    let cwd_display = super::state::compact_cwd(&agent.session.cwd, home);
    DashboardRow {
        id: DashboardRowId::TopLevel(id),
        label,
        subtitle,
        state,
        activity,
        secondary_line,
        cwd_display,
        cwd: agent.session.cwd.clone(),
        last_change_at,
        pinned,
        is_active,
        badges,
        context_pct: agent.context_state.as_ref().map(|c| c.usage_pct),
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    }
}
fn subagent_row(
    parent: AgentId,
    parent_view: &AgentView,
    info: &SubagentInfo,
    pinned: bool,
    home: Option<&str>,
) -> DashboardRow {
    let state = classify_subagent(info);
    let (label_raw, desc_raw) = format_subagent_label(info);
    let label = {
        let label = sanitize(&label_raw);
        let desc = sanitize(&desc_raw);
        if desc.trim().is_empty() {
            label
        } else {
            format!("{label} · {desc}")
        }
    };
    let activity = subagent_activity(info, state);
    let cwd = info
        .child_cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| parent_view.session.cwd.clone());
    let cwd_display = super::state::compact_cwd(&cwd, home);
    let last_change_at = crate::util::system_time_from_instant(info.last_progress_at);
    let mut badges = Vec::new();
    if info.worktree_path.is_some() {
        badges.push(RowBadge::Worktree);
    }
    if pinned {
        badges.push(RowBadge::Pinned);
    }
    if state == RowState::Failed {
        badges.push(RowBadge::Failed);
    }
    let parent_label = Some(top_level_label(parent_view));
    let subtitle = subagent_subtitle(info, &cwd);
    let secondary_line = subagent_secondary_line(info, state, activity.as_deref());
    DashboardRow {
        id: DashboardRowId::Subagent {
            parent,
            child_session_id: info.child_session_id.to_string(),
        },
        label,
        subtitle,
        state,
        activity,
        secondary_line,
        cwd_display,
        cwd,
        last_change_at,
        pinned,
        is_active: false,
        badges,
        context_pct: info.context_usage_pct,
        indent: 1,
        parent_label,
        is_more_placeholder: false,
        more_count: 0,
    }
}
/// Basename of a directory (the leaf folder name), or `None` for the
/// filesystem root / an empty path.
fn cwd_basename(cwd: &std::path::Path) -> Option<String> {
    cwd.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}
/// Build the dim right-of-label subtitle for a top-level row.
///
/// Format: `{branch} {location} [worktree]` where `location` is the
/// worktree's human label when the agent's cwd is a managed worktree
/// (falling back to the cwd's folder name), and the cwd's folder name
/// otherwise. Branch first, then the worktree/dir name, then the
/// `worktree` marker. Examples:
///
/// - Worktree: `alice/feature location-picker worktree`
/// - Plain repo / subdir: `main foo`
/// - No branch (detached / non-git): just the folder name, or `None`
///   when even that is unavailable.
///
/// `current_branch` / `is_worktree` / `worktree_label` are refreshed when
/// the dashboard opens (see `dispatch_open_dashboard`) so the branch is
/// the latest, not a stale notification value.
fn top_level_subtitle(agent: &AgentView) -> Option<String> {
    let lazy = crate::git_info::cwd_git_info_lazy(&agent.session.cwd);
    let trimmed = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    let branch = agent
        .current_branch
        .as_deref()
        .and_then(trimmed)
        .or_else(|| {
            lazy.as_ref()
                .and_then(|i| i.branch.as_deref())
                .and_then(trimmed)
        });
    let is_worktree = agent.is_worktree || lazy.as_ref().is_some_and(|i| i.is_worktree);
    let location = if is_worktree {
        agent
            .worktree_label
            .as_deref()
            .and_then(trimmed)
            .or_else(|| {
                lazy.as_ref()
                    .and_then(|i| i.worktree_label.as_deref())
                    .and_then(trimmed)
            })
            .or_else(|| cwd_basename(&agent.session.cwd))
    } else {
        cwd_basename(&agent.session.cwd)
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = branch {
        parts.push(sanitize(&b));
    }
    if let Some(loc) = location {
        parts.push(sanitize(&loc));
    }
    if parts.is_empty() {
        return None;
    }
    if is_worktree {
        parts.push("worktree".to_string());
    }
    Some(parts.join(" "))
}
/// Compose the dim "last tool / message" line painted directly below
/// the title row. Driven by row state so the most relevant detail
/// surfaces:
///
/// - **NeedsInput** — front-most permission's title (`"Pending: {title}"`)
///   or the question view's prompt. Falls back to `activity` if both
///   are absent (shouldn't happen in practice; defensive).
/// - **Working** — the activity string (`"read foo.rs"`, etc.).
/// - **Idle / Completed / Failed / Blocked** — the last
///   `AgentMessage` block in scrollback, trimmed to a single line.
///   When no message is available yet (e.g. a session whose
///   scrollback hasn't been replayed because the user hasn't opened
///   it) it falls back to the model name so the line isn't blank;
///   `None` only when the model is also unknown.
///
/// All strings are sanitised before returning — the renderer paints
/// the result directly via `set_string` with no escape filtering.
fn top_level_secondary_line(
    agent: &AgentView,
    state: RowState,
    activity: Option<&str>,
) -> Option<String> {
    match state {
        RowState::NeedsInput => {
            if let Some(perm) = agent.permission_queue.front() {
                let title = perm.title.trim();
                if !title.is_empty() {
                    return Some(format!("Pending: {}", sanitize(title)));
                }
            }
            if agent.question_view.is_some() {
                return Some("Pending: question".to_string());
            }
            activity.map(sanitize)
        }
        RowState::Working => activity.map(sanitize),
        RowState::Idle
        | RowState::Inactive
        | RowState::Completed
        | RowState::Failed
        | RowState::Blocked => agent
            .last_turn_summary
            .as_deref()
            .map(sanitize)
            .or_else(|| last_agent_message_preview(agent)),
    }
}
/// Walk the scrollback from the end, returning the first
/// `AgentMessage` block's text trimmed to a single line. Returns
/// `None` when no agent message has been produced yet.
fn last_agent_message_preview(agent: &AgentView) -> Option<String> {
    use crate::scrollback::block::RenderBlock;
    let len = agent.scrollback.len();
    for idx in (0..len).rev() {
        let entry = agent.scrollback.get(idx)?;
        if let RenderBlock::AgentMessage(msg) = &entry.block {
            let text = msg.text();
            let line = first_nonempty_line(&text)?;
            return Some(sanitize(line.trim()));
        }
    }
    None
}
/// Return the first non-empty trimmed line of `s`, or `None` when
/// every line is blank.
fn first_nonempty_line(s: &str) -> Option<&str> {
    for line in s.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(line);
        }
    }
    None
}
/// Subagent equivalent of `top_level_subtitle`. Subagents don't carry
/// their own branch / repo metadata, so we show the cwd's folder name
/// (which is the worktree folder name when running in a worktree) plus a
/// `worktree` suffix when one is set.
fn subagent_subtitle(info: &SubagentInfo, cwd: &std::path::Path) -> Option<String> {
    let name = cwd_basename(cwd)?;
    if info.worktree_path.is_some() {
        Some(format!("{name} worktree"))
    } else {
        Some(name)
    }
}
/// Secondary line for subagent rows: the running tool (working) or
/// the duration summary (finished).
fn subagent_secondary_line(
    _info: &SubagentInfo,
    _state: RowState,
    activity: Option<&str>,
) -> Option<String> {
    activity.map(sanitize)
}
fn top_level_activity(agent: &AgentView, state: RowState) -> Option<String> {
    match state {
        RowState::NeedsInput => Some("Awaiting your input".to_string()),
        RowState::Working => {
            if let Some(cmd) = agent.session.state.command_in_flight() {
                Some(format!("{}…", cmd.display_name()))
            } else if let Some(activity) = agent.resolve_turn_activity() {
                Some(sanitize(&format_activity_label(&activity)))
            } else if agent.session.loading_replay {
                Some("Loading…".to_string())
            } else if let Some(bg) = background_work_label(agent) {
                Some(bg)
            } else {
                Some("Working".to_string())
            }
        }
        _ => None,
    }
}
fn subagent_activity(info: &SubagentInfo, state: RowState) -> Option<String> {
    if state == RowState::Working {
        if let Some(label) = info
            .activity_label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(sanitize(label));
        }
        let last_tool = info.tools_used.last().map(|s| s.as_ref()).unwrap_or("");
        if last_tool.is_empty() {
            Some("Working".to_string())
        } else {
            Some(sanitize(&format_activity_label(
                &TurnActivity::ToolRunning {
                    title: last_tool.to_string(),
                    description: None,
                },
            )))
        }
    } else if info.finished {
        let turns = info.turns.unwrap_or(0);
        let tools = info.tool_calls.unwrap_or(0);
        let toks = info.tokens_used.unwrap_or(0);
        Some(format!("{tools} tools · {toks} tok · {turns} turns"))
    } else {
        None
    }
}
/// Apply a filter to the list in place.
///
/// Filter cases — see edge case 11.
///
/// Needle is lowercased once outside `retain` instead
/// of per-row, so a 100-row list doesn't allocate 100 fresh
/// lowercase Strings per keystroke.
pub fn apply_filter(rows: &mut Vec<DashboardRow>, filter: &Filter, _home: Option<&str>) {
    if matches!(filter, Filter::None) {
        return;
    }
    let needle_lower: Option<String> = match filter {
        Filter::Agent(s) | Filter::Substring(s) => Some(s.to_lowercase()),
        _ => None,
    };
    rows.retain(|r| {
        if r.is_more_placeholder {
            return true;
        }
        match filter {
            Filter::None => true,
            Filter::Agent(_) => {
                let n = needle_lower.as_deref().unwrap_or("");
                r.label.to_lowercase().contains(n)
                    || r.parent_label
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(n))
            }
            Filter::State(rs) => r.state == *rs,
            Filter::Substring(_) => {
                let n = needle_lower.as_deref().unwrap_or("");
                if n.is_empty() {
                    return true;
                }
                r.label.to_lowercase().contains(n) || r.cwd_display.to_lowercase().contains(n)
            }
        }
    });
}
/// Sort rows in place.
///
/// Order:
///  1. Pinned first (regardless of group).
///  2. Group primitive: descending state-priority in `State` grouping;
///     `cwd` in `Directory` grouping.
///  3. `reorder` overrides (Shift+↑/↓) float a row to its declared index
///     INSIDE its group only — never across groups. In `State` grouping
///     this means within the same state (so the group can't be split
///     into `Idle → Working → Idle`); in `Directory` grouping it means
///     within the same cwd. See [`sort_cluster_key`].
///  4. Then descending by `last_change_at`.
///  5. Final tiebreak by `DashboardRowId` so the order is
///     deterministic across rebuilds even when every other field
///     ties. Asserted by `sort_rows_tiebreaks_by_id_when_keys_equal`.
///  6. Subagents stay glued to their parent (we never sort the row
///     vector across parents — that's already enforced by the build
///     step which emits subagents immediately after their parent).
pub fn sort_rows(
    rows: &mut [DashboardRow],
    grouping: super::state::Grouping,
    reorder: &[DashboardRowId],
) {
    use super::state::Grouping;
    match grouping {
        Grouping::State => sort_within_groups(rows, reorder),
        Grouping::Directory => sort_within_directory_groups(rows, reorder),
    }
}
fn sort_within_groups(rows: &mut [DashboardRow], reorder: &[DashboardRowId]) {
    let clusters = build_clusters(rows);
    let mut indexed: Vec<(usize, usize, ClusterKey)> = clusters
        .iter()
        .map(|(start, end)| {
            let parent = &rows[*start];
            (
                *start,
                *end,
                ClusterKey {
                    pinned: parent.pinned,
                    state: parent.state.group_priority(),
                    last_change_at: parent.last_change_at,
                    reorder_idx: reorder.iter().position(|r| *r == parent.id),
                    id: parent.id.clone(),
                },
            )
        })
        .collect();
    indexed.sort_by(|a, b| sort_cluster_key(&a.2, &b.2, true));
    let snapshot: Vec<DashboardRow> = rows.to_vec();
    let mut write = 0usize;
    for (start, end, _) in &indexed {
        for src in snapshot.iter().take(*end).skip(*start) {
            rows[write] = src.clone();
            write += 1;
        }
    }
}
fn sort_within_directory_groups(rows: &mut [DashboardRow], reorder: &[DashboardRowId]) {
    let clusters = build_clusters(rows);
    let mut indexed: Vec<(usize, usize, (String, ClusterKey))> = clusters
        .iter()
        .map(|(start, end)| {
            let parent = &rows[*start];
            let key = ClusterKey {
                pinned: parent.pinned,
                state: parent.state.group_priority(),
                last_change_at: parent.last_change_at,
                reorder_idx: reorder.iter().position(|r| *r == parent.id),
                id: parent.id.clone(),
            };
            (*start, *end, (parent.cwd_display.clone(), key))
        })
        .collect();
    indexed.sort_by(|a, b| match a.2.0.cmp(&b.2.0) {
        std::cmp::Ordering::Equal => sort_cluster_key(&a.2.1, &b.2.1, false),
        other => other,
    });
    let snapshot: Vec<DashboardRow> = rows.to_vec();
    let mut write = 0usize;
    for (start, end, _) in &indexed {
        for src in snapshot.iter().take(*end).skip(*start) {
            rows[write] = src.clone();
            write += 1;
        }
    }
}
#[derive(Debug, Clone)]
struct ClusterKey {
    pinned: bool,
    state: u8,
    last_change_at: SystemTime,
    reorder_idx: Option<usize>,
    /// Final tiebreak: when every other field is equal,
    /// fall back to the parent row's id so the order is deterministic
    /// across rebuilds (no reliance on `sort_by`'s stable-on-equal
    /// pass-through).
    id: DashboardRowId,
}
/// Compare two cluster sort keys.
///
/// `state_before_reorder` decides where the explicit-reorder override
/// (Shift+↑/↓) sits relative to the state-priority key — and that choice
/// is what keeps the rendered groups contiguous:
///
/// - **State grouping (`true`)** — state is the grouping primitive AND
///   the primary sort key, so a reorder must only float a row *within*
///   its own state group. Ranking `reorder_idx` above `state` (the old
///   behaviour) let a reordered Idle row jump above the Working group,
///   which made the renderer emit a second "Idle" header below the
///   Working one (Idle → Working → Idle). Keeping `state` first confines
///   the reorder to one group, matching the documented contract
///   ("float to position inside the row's group") in
///   [`super::super::app::dispatch`]'s `dispatch_dashboard_reorder`.
/// - **Directory grouping (`false`)** — the cwd is the grouping
///   primitive (sorted ahead of this key in
///   [`sort_within_directory_groups`]), so within a cwd the reorder is
///   free to float across states; there are no state sub-headers to
///   split.
fn sort_cluster_key(
    a: &ClusterKey,
    b: &ClusterKey,
    state_before_reorder: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if a.pinned != b.pinned {
        return b.pinned.cmp(&a.pinned);
    }
    let by_reorder = |a: &ClusterKey, b: &ClusterKey| match (a.reorder_idx, b.reorder_idx) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let by_state = |a: &ClusterKey, b: &ClusterKey| b.state.cmp(&a.state);
    if state_before_reorder {
        if a.state != b.state {
            return by_state(a, b);
        }
        match by_reorder(a, b) {
            Ordering::Equal => {}
            ord => return ord,
        }
    } else {
        match by_reorder(a, b) {
            Ordering::Equal => {}
            ord => return ord,
        }
        if a.state != b.state {
            return by_state(a, b);
        }
    }
    b.last_change_at
        .cmp(&a.last_change_at)
        .then_with(|| a.id.cmp(&b.id))
}
/// Identify `(start, end)` ranges of clustered rows — each cluster is
/// one top-level row followed by zero or more subagents.
fn build_clusters(rows: &[DashboardRow]) -> Vec<(usize, usize)> {
    let mut clusters = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let start = i;
        i += 1;
        while i < rows.len() && rows[i].indent > 0 {
            i += 1;
        }
        clusters.push((start, i));
    }
    clusters
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};
    fn make_subagent(child_id: &str, finished: bool, status: Option<&str>) -> SubagentInfo {
        let now = Instant::now();
        SubagentInfo {
            subagent_id: Arc::from(format!("sa-{child_id}")),
            child_session_id: Arc::from(child_id),
            description: Arc::from("test task"),
            subagent_type: Arc::from("explore"),
            persona: None,
            role: None,
            model: None,
            context_source: None,
            resumed_from: None,
            capability_mode: None,
            workflow_run_id: None,
            context_normalized: false,
            transcript: Default::default(),
            parent_prompt_id: None,
            started_at: now,
            last_progress_at: now,
            finished,
            status: status.map(Arc::from),
            error: None,
            duration_ms: None,
            tool_calls: None,
            turns: None,
            turn_count: None,
            tool_call_count: None,
            tokens_used: None,
            context_window_tokens: None,
            context_usage_pct: None,
            tools_used: Vec::new(),
            error_count: None,
            activity_label: None,
            is_background: false,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            prompt: None,
            child_cwd: None,
            worktree_path: None,
        }
    }
    #[test]
    fn classify_subagent_running() {
        let info = make_subagent("a", false, None);
        assert_eq!(classify_subagent(&info), RowState::Working);
    }
    #[test]
    fn full_tree_excludes_workflow_owned_subagent_rows() {
        let mut agents = IndexMap::new();
        let mut agent = crate::app::agent_view::test_fixtures::make_agent();
        let mut workflow_child = make_subagent("workflow-child", false, None);
        workflow_child.workflow_run_id = Some(Arc::from("wf_1"));
        agent
            .subagent_sessions
            .insert("workflow-child".into(), workflow_child);
        agents.insert(AgentId(0), agent);
        let rows = build_rows(
            &agents,
            &Default::default(),
            &[],
            Some(AgentId(0)),
            super::super::state::Grouping::State,
            &Filter::default(),
            None,
        );
        assert!(
            rows.iter()
                .all(|row| !matches!(row.id, DashboardRowId::Subagent { .. }))
        );
    }
    #[test]
    fn classify_subagent_completed() {
        let info = make_subagent("a", true, Some("completed"));
        assert_eq!(classify_subagent(&info), RowState::Completed);
    }
    #[test]
    fn classify_subagent_failed() {
        let info = make_subagent("a", true, Some("failed"));
        assert_eq!(classify_subagent(&info), RowState::Failed);
    }
    #[test]
    fn classify_subagent_cancelled() {
        let info = make_subagent("a", true, Some("cancelled"));
        assert_eq!(classify_subagent(&info), RowState::Failed);
    }
    #[test]
    fn subagent_activity_prefers_live_label_over_tool_reconstruction() {
        let mut info = make_subagent("a", false, None);
        info.tools_used = vec![Arc::from("bash")];
        info.activity_label = Some("Running: cargo build".into());
        assert_eq!(
            subagent_activity(&info, RowState::Working).as_deref(),
            Some("Running: cargo build")
        );
        info.activity_label = None;
        assert_eq!(
            subagent_activity(&info, RowState::Working).as_deref(),
            Some("Running: bash")
        );
    }
    /// Edge case 7: subagent rows never reach `NeedsInput` in v1.
    #[test]
    fn subagent_classifier_never_emits_needs_input() {
        for finished in [false, true] {
            for status in [None, Some("completed"), Some("failed"), Some("cancelled")] {
                let info = make_subagent("a", finished, status);
                let state = classify_subagent(&info);
                assert_ne!(
                    state,
                    RowState::NeedsInput,
                    "finished={finished} status={status:?}"
                );
            }
        }
    }
    #[test]
    fn cluster_keeps_subagents_with_parent() {
        let rows = vec![
            make_row("a", 0, RowState::Working),
            make_row("a:1", 1, RowState::Working),
            make_row("a:2", 1, RowState::Idle),
            make_row("b", 0, RowState::Idle),
            make_row("b:1", 1, RowState::Working),
        ];
        let clusters = build_clusters(&rows);
        assert_eq!(clusters, vec![(0, 3), (3, 5)]);
    }
    fn make_row(label: &str, indent: u8, state: RowState) -> DashboardRow {
        DashboardRow {
            id: if indent == 0 {
                DashboardRowId::TopLevel(AgentId(label.len()))
            } else {
                DashboardRowId::Subagent {
                    parent: AgentId(0),
                    child_session_id: label.to_string(),
                }
            },
            label: label.to_string(),
            subtitle: None,
            state,
            activity: None,
            secondary_line: None,
            cwd_display: "~/x".to_string(),
            cwd: PathBuf::from("/tmp"),
            last_change_at: SystemTime::now(),
            pinned: false,
            is_active: false,
            badges: Vec::new(),
            context_pct: None,
            indent,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        }
    }
    fn make_row_with_id(id: DashboardRowId, indent: u8, state: RowState) -> DashboardRow {
        DashboardRow {
            id,
            label: "row".to_string(),
            subtitle: None,
            state,
            activity: None,
            secondary_line: None,
            cwd_display: "~/x".to_string(),
            cwd: PathBuf::from("/tmp"),
            last_change_at: SystemTime::now(),
            pinned: false,
            is_active: false,
            badges: Vec::new(),
            context_pct: None,
            indent,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        }
    }
    /// Sort: pinned floats above non-pinned regardless of state.
    #[test]
    fn sort_pinned_first() {
        let mut rows = vec![
            DashboardRow {
                pinned: false,
                state: RowState::Working,
                ..make_row_with_id(DashboardRowId::TopLevel(AgentId(2)), 0, RowState::Working)
            },
            DashboardRow {
                pinned: true,
                state: RowState::Idle,
                ..make_row_with_id(DashboardRowId::TopLevel(AgentId(1)), 0, RowState::Idle)
            },
        ];
        sort_rows(&mut rows, super::super::state::Grouping::State, &[]);
        assert!(rows[0].pinned);
        assert!(!rows[1].pinned);
    }
    #[test]
    fn sort_state_priority_within_group() {
        let mut rows = vec![
            make_row_with_id(DashboardRowId::TopLevel(AgentId(1)), 0, RowState::Idle),
            make_row_with_id(
                DashboardRowId::TopLevel(AgentId(2)),
                0,
                RowState::NeedsInput,
            ),
            make_row_with_id(DashboardRowId::TopLevel(AgentId(3)), 0, RowState::Working),
        ];
        sort_rows(&mut rows, super::super::state::Grouping::State, &[]);
        assert_eq!(rows[0].state, RowState::NeedsInput);
        assert_eq!(rows[1].state, RowState::Working);
        assert_eq!(rows[2].state, RowState::Idle);
    }
    /// Renamed from `sort_deterministic_with_equal_keys`.
    /// The original name implied a tiebreak guarantee that
    /// `sort_cluster_key` does NOT provide; documents the actual
    /// behavioural contract: idempotent on identical inputs.
    #[test]
    fn sort_is_idempotent_for_identical_inputs() {
        let now = SystemTime::now();
        let mut rows1 = vec![
            DashboardRow {
                last_change_at: now,
                ..make_row_with_id(DashboardRowId::TopLevel(AgentId(1)), 0, RowState::Idle)
            },
            DashboardRow {
                last_change_at: now,
                ..make_row_with_id(DashboardRowId::TopLevel(AgentId(2)), 0, RowState::Idle)
            },
        ];
        let mut rows2 = rows1.clone();
        sort_rows(&mut rows1, super::super::state::Grouping::State, &[]);
        sort_rows(&mut rows2, super::super::state::Grouping::State, &[]);
        assert_eq!(
            rows1.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            rows2.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
        );
    }
    /// When every other field ties, the sort tiebreaks
    /// by id, producing a fully deterministic order independent of
    /// the input row order. AgentId(1) sorts before AgentId(2)
    /// regardless of which appears first in the input vector.
    #[test]
    fn sort_rows_tiebreaks_by_id_when_keys_equal() {
        let now = SystemTime::now();
        let id1 = DashboardRowId::TopLevel(AgentId(1));
        let id2 = DashboardRowId::TopLevel(AgentId(2));
        let mut forward = vec![
            DashboardRow {
                last_change_at: now,
                ..make_row_with_id(id1.clone(), 0, RowState::Idle)
            },
            DashboardRow {
                last_change_at: now,
                ..make_row_with_id(id2.clone(), 0, RowState::Idle)
            },
        ];
        let mut reverse = vec![
            DashboardRow {
                last_change_at: now,
                ..make_row_with_id(id2.clone(), 0, RowState::Idle)
            },
            DashboardRow {
                last_change_at: now,
                ..make_row_with_id(id1.clone(), 0, RowState::Idle)
            },
        ];
        sort_rows(&mut forward, super::super::state::Grouping::State, &[]);
        sort_rows(&mut reverse, super::super::state::Grouping::State, &[]);
        assert_eq!(forward[0].id, id1, "forward: id1 must sort first");
        assert_eq!(forward[1].id, id2, "forward: id2 must sort second");
        assert_eq!(reverse[0].id, id1, "reverse: id1 must sort first");
        assert_eq!(reverse[1].id, id2, "reverse: id2 must sort second");
    }
    #[test]
    fn filter_substring_label_match() {
        let mut rows = vec![
            DashboardRow {
                label: "fix login bug".to_string(),
                ..make_row("a", 0, RowState::Working)
            },
            DashboardRow {
                label: "investigate cache".to_string(),
                ..make_row("b", 0, RowState::Working)
            },
        ];
        apply_filter(&mut rows, &Filter::Substring("login".into()), None);
        assert_eq!(rows.len(), 1);
    }
    #[test]
    fn filter_agent_matches_label_and_parent() {
        let mut rows = vec![
            DashboardRow {
                label: "implementer".to_string(),
                parent_label: None,
                ..make_row("a", 0, RowState::Working)
            },
            DashboardRow {
                label: "explore".to_string(),
                parent_label: Some("implementer".to_string()),
                ..make_row("a:1", 1, RowState::Working)
            },
            DashboardRow {
                label: "other".to_string(),
                parent_label: None,
                ..make_row("b", 0, RowState::Working)
            },
        ];
        apply_filter(&mut rows, &Filter::Agent("implementer".into()), None);
        assert_eq!(rows.len(), 2);
    }
    #[test]
    fn filter_state_matches_exact() {
        let mut rows = vec![
            DashboardRow {
                state: RowState::NeedsInput,
                ..make_row("a", 0, RowState::NeedsInput)
            },
            DashboardRow {
                state: RowState::Idle,
                ..make_row("b", 0, RowState::Idle)
            },
        ];
        apply_filter(&mut rows, &Filter::State(RowState::Idle), None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, RowState::Idle);
    }
    /// Sort: explicit reorderings (Shift+↑/↓) float a row to its
    /// declared position WITHIN its state group. Two Working rows: the
    /// older one (id2) is reordered above the more-recent one (id1).
    #[test]
    fn sort_reorder_floats_within_state_group() {
        let id1 = DashboardRowId::TopLevel(AgentId(1));
        let id2 = DashboardRowId::TopLevel(AgentId(2));
        let newer = SystemTime::now();
        let older = newer - Duration::from_secs(60);
        let mut rows = vec![
            DashboardRow {
                last_change_at: newer, // recency would put id1 first
                ..make_row_with_id(id1.clone(), 0, RowState::Working)
            },
            DashboardRow {
                last_change_at: older,
                ..make_row_with_id(id2.clone(), 0, RowState::Working)
            },
        ];
        sort_rows(
            &mut rows,
            super::super::state::Grouping::State,
            std::slice::from_ref(&id2),
        );
        assert_eq!(rows[0].id, id2, "reorder must float id2 to the top");
        assert_eq!(rows[1].id, id1);
    }
    /// Regression: reordering must NOT split a state group. With Working
    /// and Idle rows interleaved in the input, reordering an Idle row
    /// keeps every Working row above every Idle row — the renderer would
    /// otherwise emit `Idle → Working → Idle` headers (the reported bug).
    /// The reorder only floats the Idle row WITHIN the Idle group.
    #[test]
    fn reorder_does_not_split_state_groups() {
        let w1 = DashboardRowId::TopLevel(AgentId(1));
        let i1 = DashboardRowId::TopLevel(AgentId(2));
        let w2 = DashboardRowId::TopLevel(AgentId(3));
        let i2 = DashboardRowId::TopLevel(AgentId(4));
        let mut rows = vec![
            make_row_with_id(w1, 0, RowState::Working),
            make_row_with_id(i1.clone(), 0, RowState::Idle),
            make_row_with_id(w2, 0, RowState::Working),
            make_row_with_id(i2.clone(), 0, RowState::Idle),
        ];
        sort_rows(
            &mut rows,
            super::super::state::Grouping::State,
            &[i1.clone(), i2.clone()],
        );
        let states: Vec<RowState> = rows.iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            vec![
                RowState::Working,
                RowState::Working,
                RowState::Idle,
                RowState::Idle,
            ],
            "reorder must not interleave states, got {states:?}",
        );
        let idle_order: Vec<DashboardRowId> = rows
            .iter()
            .filter(|r| r.state == RowState::Idle)
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(idle_order, vec![i1, i2], "reorder must order within Idle");
    }
    /// Sort: when both rows are pinned, ordering falls back to the
    /// secondary key (state).
    #[test]
    fn sort_two_pinned_rows_order_by_state() {
        let mut rows = vec![
            DashboardRow {
                pinned: true,
                state: RowState::Idle,
                ..make_row_with_id(DashboardRowId::TopLevel(AgentId(1)), 0, RowState::Idle)
            },
            DashboardRow {
                pinned: true,
                state: RowState::Working,
                ..make_row_with_id(DashboardRowId::TopLevel(AgentId(2)), 0, RowState::Working)
            },
        ];
        sort_rows(&mut rows, super::super::state::Grouping::State, &[]);
        assert!(rows[0].pinned);
        assert_eq!(rows[0].state, RowState::Working);
    }
    /// Filter: agent prefix with empty needle keeps everything (treated
    /// as `Filter::None` upstream by `Filter::from_value`).
    #[test]
    fn filter_agent_with_empty_string_does_not_hide_rows() {
        let mut rows = vec![
            make_row("a", 0, RowState::Working),
            make_row("b", 0, RowState::Idle),
        ];
        apply_filter(&mut rows, &Filter::Agent(String::new()), None);
        assert_eq!(rows.len(), 2);
    }
    /// Edge case 6: more-placeholder rows survive filtering.
    #[test]
    fn filter_keeps_more_placeholders() {
        let mut rows = vec![
            DashboardRow {
                label: "real row".to_string(),
                ..make_row("a", 1, RowState::Idle)
            },
            DashboardRow {
                label: "… 3 more".to_string(),
                is_more_placeholder: true,
                ..make_row("b", 1, RowState::Idle)
            },
        ];
        apply_filter(
            &mut rows,
            &Filter::Substring("nothing matches".into()),
            None,
        );
        assert!(rows.iter().any(|r| r.is_more_placeholder));
    }
    fn roster_entry(session_id: &str, last_change_unix_ms: i64) -> RosterEntry {
        use crate::app::roster::RosterOrigin;
        RosterEntry {
            session_id: session_id.to_string(),
            title: Some(format!("title {session_id}")),
            cwd: "/repo".to_string(),
            is_worktree: false,
            model_id: None,
            yolo: false,
            activity: RosterActivity::Dormant,
            last_turn_summary: None,
            resident: false,
            last_change_unix_ms,
            origin: RosterOrigin::default(),
        }
    }
    fn now_unix_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }
    /// The age rendered for a roster row (`render` uses
    /// `last_change_at.elapsed()`).
    fn roster_row_age(entry: RosterEntry) -> Duration {
        let mut rows = Vec::new();
        append_roster_rows(
            &mut rows,
            &[entry],
            &IndexMap::new(),
            &std::collections::BTreeSet::new(),
            None,
        );
        assert_eq!(rows.len(), 1);
        rows[0].last_change_at.elapsed().unwrap_or_default()
    }
    /// A roster entry touched a while ago must NOT render as "just now":
    /// its `last_change_at` is derived from `last_change_unix_ms`, so the
    /// age reflects the real wall-clock age (regression guard for the bug
    /// where every roster row stamped `Instant::now()`).
    #[test]
    fn roster_row_age_reflects_last_change_unix_ms() {
        let two_hours_ago = now_unix_ms() - 2 * 3_600_000;
        let elapsed = roster_row_age(roster_entry("sess-old", two_hours_ago));
        assert!(
            elapsed.as_secs() >= 3600,
            "roster row age should reflect the real timestamp, got {elapsed:?}"
        );
        assert_eq!(crate::util::format_time_ago(elapsed), "2h");
    }
    /// Roster rows sort by their real `last_change_unix_ms`: a session
    /// touched long ago floats below a recently-touched one.
    #[test]
    fn roster_rows_sort_by_real_timestamp() {
        let now_ms = now_unix_ms();
        let mut rows = Vec::new();
        let agents = IndexMap::new();
        let pinned = std::collections::BTreeSet::new();
        append_roster_rows(
            &mut rows,
            &[
                roster_entry("sess-old", now_ms - 86_400_000),
                roster_entry("sess-new", now_ms - 5_000),
            ],
            &agents,
            &pinned,
            None,
        );
        sort_rows(&mut rows, super::super::state::Grouping::State, &[]);
        assert_eq!(
            rows[0].id,
            DashboardRowId::Roster {
                session_id: "sess-new".to_string()
            }
        );
        assert_eq!(
            rows[1].id,
            DashboardRowId::Roster {
                session_id: "sess-old".to_string()
            }
        );
    }
    /// Build a roster entry with an explicit title + activity for the
    /// empty-session filtering tests.
    fn roster_entry_with(
        session_id: &str,
        title: Option<&str>,
        activity: RosterActivity,
    ) -> RosterEntry {
        RosterEntry {
            title: title.map(String::from),
            activity,
            ..roster_entry(session_id, now_unix_ms())
        }
    }
    fn collect_roster(
        entries: &[RosterEntry],
        pinned: &std::collections::BTreeSet<DashboardRowId>,
    ) -> Vec<DashboardRow> {
        let mut rows = Vec::new();
        append_roster_rows(&mut rows, entries, &IndexMap::new(), pinned, None);
        rows
    }
    /// An untitled, inactive roster session is the "New session" noise the
    /// dashboard hides — every pager launch leaves one behind.
    #[test]
    fn append_roster_rows_hides_untitled_idle_entry() {
        let empty = std::collections::BTreeSet::new();
        for activity in [
            RosterActivity::Idle,
            RosterActivity::Dormant,
            RosterActivity::Completed,
            RosterActivity::Dead,
        ] {
            let rows = collect_roster(&[roster_entry_with("e", None, activity)], &empty);
            assert!(
                rows.is_empty(),
                "untitled {activity:?} entry must be hidden"
            );
        }
    }
    /// An untitled session that is actively working / awaiting input stays
    /// visible: its title hasn't been generated yet but it is doing real work.
    #[test]
    fn append_roster_rows_keeps_untitled_active_entry() {
        let empty = std::collections::BTreeSet::new();
        for activity in [RosterActivity::Working, RosterActivity::NeedsInput] {
            let rows = collect_roster(&[roster_entry_with("a", None, activity)], &empty);
            assert_eq!(rows.len(), 1, "untitled {activity:?} entry must be kept");
        }
    }
    /// Titled (real) sessions always render, even when idle/dormant.
    #[test]
    fn append_roster_rows_keeps_titled_entry() {
        let empty = std::collections::BTreeSet::new();
        let rows = collect_roster(
            &[roster_entry_with(
                "t",
                Some("Fix the bug"),
                RosterActivity::Dormant,
            )],
            &empty,
        );
        assert_eq!(rows.len(), 1, "titled entries are kept");
    }
    /// A blank/whitespace title is treated as no title.
    #[test]
    fn append_roster_rows_hides_blank_titled_idle_entry() {
        let empty = std::collections::BTreeSet::new();
        let rows = collect_roster(
            &[roster_entry_with("b", Some("   "), RosterActivity::Idle)],
            &empty,
        );
        assert!(rows.is_empty(), "blank-title idle entry must be hidden");
    }
    /// A user-pinned row is always shown, even untitled + idle.
    #[test]
    fn append_roster_rows_keeps_pinned_untitled_entry() {
        let mut pinned = std::collections::BTreeSet::new();
        pinned.insert(DashboardRowId::Roster {
            session_id: "p".to_string(),
        });
        let rows = collect_roster(
            &[roster_entry_with("p", None, RosterActivity::Idle)],
            &pinned,
        );
        assert_eq!(rows.len(), 1, "pinned untitled entries are kept");
    }
    /// A roster session with a known model surfaces it on the otherwise
    /// blank second line (the wire carries no last-message preview, so
    /// the model is the only per-session detail left to show).
    #[test]
    fn append_roster_rows_uses_model_id_as_secondary_line() {
        let empty = std::collections::BTreeSet::new();
        let entry = RosterEntry {
            model_id: Some("echo-agent-4.5".to_string()),
            ..roster_entry_with("m", Some("Fix the bug"), RosterActivity::Dormant)
        };
        let rows = collect_roster(&[entry], &empty);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].secondary_line.as_deref(), Some("echo-agent-4.5"));
    }
    /// The last-turn summary wins the secondary line over the model id.
    #[test]
    fn append_roster_rows_prefers_last_turn_summary() {
        let empty = std::collections::BTreeSet::new();
        let entry = RosterEntry {
            model_id: Some("echo-agent-4.5".to_string()),
            last_turn_summary: Some("Fixed the roster merge".to_string()),
            ..roster_entry_with("m", Some("Fix the bug"), RosterActivity::Dormant)
        };
        let rows = collect_roster(&[entry], &empty);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].secondary_line.as_deref(),
            Some("Fixed the roster merge")
        );
    }
    /// Without a model id there's genuinely nothing to show, so the
    /// second line stays empty rather than rendering a placeholder.
    #[test]
    fn append_roster_rows_secondary_line_none_without_model() {
        let empty = std::collections::BTreeSet::new();
        let rows = collect_roster(
            &[roster_entry_with(
                "m",
                Some("Fix the bug"),
                RosterActivity::Dormant,
            )],
            &empty,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].secondary_line, None);
    }
    /// Build a minimal idle local agent with an empty scrollback (no
    /// replayed messages) and an optional current model — mirrors a
    /// session listed on the dashboard that the user hasn't opened yet.
    fn make_idle_agent_with_model(model: Option<&str>) -> AgentView {
        use crate::acp::model_state::ModelState;
        use crate::app::agent::{AgentSession, AgentState};
        use crate::scrollback::state::ScrollbackState;
        use agent_client_protocol as acp;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut models = ModelState::default();
        if let Some(m) = model {
            models.set_current(acp::ModelId::new(std::sync::Arc::from(m)), None);
        }
        let session = AgentSession {
            id: AgentId(0),
            acp_tx: tx,
            session_id: Some(acp::SessionId::new("test-session")),
            models,
            state: AgentState::Idle,
            tracker: crate::acp::tracker::AcpUpdateTracker::new(),
            cwd: PathBuf::from("/tmp"),
            is_worktree: false,
            forked_from: None,
            pending_prompts: std::collections::VecDeque::new(),
            next_queue_id: 0,
            yolo_mode: false,
            auto_mode: false,
            prompt_history: Vec::new(),
            prompt_history_loading: false,
            loading_replay: false,
            restore_degree: None,
            rate_limited: false,
            model_incompatible: false,
            credit_limit_blocked: false,
            free_usage_blocked: false,
            available_commands: Vec::new(),
            available_commands_generation: 0,
            available_tools: None,
            model_switch_pending: false,
            user_model_preference: None,
            deferred_model_switch: None,
            bg_tasks: std::collections::BTreeMap::new(),
            bg_tool_call_to_task: std::collections::HashMap::new(),
            scheduled_tasks: std::collections::HashMap::new(),
            in_flight_prompt: None,
            compact_held_prompt: None,
            current_prompt_id: None,
            created_via_new: false,
        };
        AgentView::new(session, ScrollbackState::new())
    }
    /// An idle local agent with no last message has a BLANK second line —
    /// the model is no longer used as a fallback there (it now shows in
    /// the peek panel's bottom-border badge for the selected row, keeping
    /// the list uncluttered).
    #[test]
    fn idle_local_agent_without_message_has_blank_secondary() {
        let agent = make_idle_agent_with_model(Some("echo-agent-4.5"));
        assert_eq!(classify_top_level(&agent), RowState::Idle);
        let row = top_level_row(AgentId(0), &agent, false, false, None);
        assert_eq!(
            row.secondary_line, None,
            "the model must not appear on the list row anymore",
        );
    }
    /// When neither a message nor a model is known the second line is
    /// left empty (no placeholder).
    #[test]
    fn idle_local_agent_without_message_or_model_has_no_secondary() {
        let agent = make_idle_agent_with_model(None);
        let row = top_level_row(AgentId(0), &agent, false, false, None);
        assert_eq!(row.secondary_line, None);
    }
    /// A worktree agent's subtitle shows the branch + the worktree's
    /// human label + a `worktree` marker.
    #[test]
    fn subtitle_worktree_shows_label_branch_and_marker() {
        let mut agent = make_idle_agent_with_model(None);
        agent.session.cwd = PathBuf::from("/home/me/.echo-agent/worktrees/x/location-picker");
        agent.is_worktree = true;
        agent.worktree_label = Some("location-picker".to_string());
        agent.current_branch = Some("kevin/feature".to_string());
        assert_eq!(
            top_level_subtitle(&agent).as_deref(),
            Some("kevin/feature location-picker worktree"),
        );
    }
    /// A worktree without a DB label falls back to the cwd folder name.
    #[test]
    fn subtitle_worktree_without_label_uses_folder_name() {
        let mut agent = make_idle_agent_with_model(None);
        agent.session.cwd = PathBuf::from("/home/me/wt/my-wt-dir");
        agent.is_worktree = true;
        agent.worktree_label = None;
        agent.current_branch = Some("main".to_string());
        assert_eq!(
            top_level_subtitle(&agent).as_deref(),
            Some("main my-wt-dir worktree"),
        );
    }
    /// A non-worktree agent shows the branch + the cwd folder name (the
    /// actual working subdir, not the repo root), no worktree marker.
    #[test]
    fn subtitle_non_worktree_shows_branch_and_folder() {
        let mut agent = make_idle_agent_with_model(None);
        agent.session.cwd = PathBuf::from("/home/me/echoagent/crates/foo");
        agent.is_worktree = false;
        agent.current_branch = Some("main".to_string());
        assert_eq!(top_level_subtitle(&agent).as_deref(), Some("main foo"));
    }
    /// With no branch known, the subtitle is just the folder name.
    #[test]
    fn subtitle_no_branch_shows_only_folder() {
        let mut agent = make_idle_agent_with_model(None);
        agent.session.cwd = PathBuf::from("/home/me/projects/bar");
        agent.is_worktree = false;
        agent.current_branch = None;
        assert_eq!(top_level_subtitle(&agent).as_deref(), Some("bar"));
    }
    /// The `[bg]` badge reflects RUNNING background tasks only —
    /// `bg_tasks` retains finished (`Done` / `Failed`) tasks for the
    /// tasks-pane history, and those must not pin a stale badge on the
    /// row forever.
    #[test]
    fn bg_badge_only_for_running_tasks() {
        use crate::app::agent::{BgTaskState, BgTaskStatus};
        let make_task = |status: BgTaskStatus| BgTaskState {
            task_id: "t1".into(),
            tool_call_id: String::new(),
            command: "sleep 99".into(),
            description: None,
            cwd: String::new(),
            output_file: String::new(),
            status,
            start_time: std::time::SystemTime::now(),
            end_time: None,
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stdout_line_count: 0,
            truncated: false,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            is_monitor: false,
            restored_from_replay: false,
        };
        for (status, expect_badge) in [
            (BgTaskStatus::Running, true),
            (BgTaskStatus::Done, false),
            (BgTaskStatus::Failed, false),
        ] {
            let mut agent = make_idle_agent_with_model(None);
            agent
                .session
                .bg_tasks
                .insert("t1".into(), make_task(status));
            let row = top_level_row(AgentId(0), &agent, false, false, None);
            assert_eq!(
                row.badges.contains(&RowBadge::BgTask),
                expect_badge,
                "status={status:?} badge expectation",
            );
        }
    }
    /// A running background task (`run_terminal_command background=true`).
    fn running_bg_task(task_id: &str, is_monitor: bool) -> crate::app::agent::BgTaskState {
        crate::app::agent::BgTaskState {
            task_id: task_id.into(),
            tool_call_id: String::new(),
            command: "sleep 99".into(),
            description: None,
            cwd: String::new(),
            output_file: String::new(),
            status: crate::app::agent::BgTaskStatus::Running,
            start_time: std::time::SystemTime::now(),
            end_time: None,
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stdout_line_count: 0,
            truncated: false,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            is_monitor,
            restored_from_replay: false,
        }
    }
    /// An active scheduled `/loop` task.
    fn scheduled_loop(task_id: &str) -> crate::app::agent::ScheduledTaskInfo {
        crate::app::agent::ScheduledTaskInfo {
            task_id: task_id.into(),
            prompt: "check things".into(),
            human_schedule: "every 5m".into(),
            created_at: std::time::Instant::now(),
            next_fire_at: None,
            tag: "loop".into(),
            last_subagent_id: None,
        }
    }
    /// A turn-idle agent with a RUNNING background task is `Working`, not
    /// `Idle`. Finished tasks (Done / Failed) don't keep it Working — they
    /// linger in `bg_tasks` only for the tasks-pane history.
    #[test]
    fn running_bg_task_classifies_as_working() {
        use crate::app::agent::BgTaskStatus;
        for (status, expected) in [
            (BgTaskStatus::Running, RowState::Working),
            (BgTaskStatus::Done, RowState::Idle),
            (BgTaskStatus::Failed, RowState::Idle),
        ] {
            let mut agent = make_idle_agent_with_model(None);
            let mut task = running_bg_task("t1", false);
            task.status = status;
            agent.session.bg_tasks.insert("t1".into(), task);
            assert_eq!(
                classify_top_level(&agent),
                expected,
                "status={status:?} must classify as {expected:?}",
            );
        }
    }
    /// A running `monitor` (a bg task with `is_monitor`) keeps the agent
    /// `Working`, and the activity line names it.
    #[test]
    fn running_monitor_classifies_as_working_with_label() {
        let mut agent = make_idle_agent_with_model(None);
        agent
            .session
            .bg_tasks
            .insert("m1".into(), running_bg_task("m1", true));
        assert_eq!(classify_top_level(&agent), RowState::Working);
        let row = top_level_row(AgentId(0), &agent, false, false, None);
        assert_eq!(row.activity.as_deref(), Some("1 monitor still running"));
    }
    /// An active scheduled `/loop` keeps the agent `Working` even with a
    /// fully idle turn, labelled as a loop.
    #[test]
    fn scheduled_loop_classifies_as_working_with_label() {
        let mut agent = make_idle_agent_with_model(None);
        agent
            .session
            .scheduled_tasks
            .insert("l1".into(), scheduled_loop("l1"));
        assert_eq!(classify_top_level(&agent), RowState::Working);
        let row = top_level_row(AgentId(0), &agent, false, false, None);
        assert_eq!(row.activity.as_deref(), Some("1 loop still running"));
    }
    /// The background-work label lists every non-zero kind (monitors,
    /// then loops, then plain tasks) with correct singular/plural nouns.
    #[test]
    fn background_work_label_lists_all_kinds() {
        let mut agent = make_idle_agent_with_model(None);
        agent
            .session
            .bg_tasks
            .insert("m1".into(), running_bg_task("m1", true));
        agent
            .session
            .bg_tasks
            .insert("b1".into(), running_bg_task("b1", false));
        agent
            .session
            .bg_tasks
            .insert("b2".into(), running_bg_task("b2", false));
        agent
            .session
            .scheduled_tasks
            .insert("l1".into(), scheduled_loop("l1"));
        assert_eq!(classify_top_level(&agent), RowState::Working);
        let row = top_level_row(AgentId(0), &agent, false, false, None);
        assert_eq!(
            row.activity.as_deref(),
            Some("1 monitor · 1 loop · 2 tasks still running"),
        );
    }
    /// The background-work label is the LAST activity fallback: a more
    /// specific Working signal (here, replay loading) still wins over
    /// "… still running", so a real turn is never masked by it.
    #[test]
    fn specific_working_activity_wins_over_background_label() {
        let mut agent = make_idle_agent_with_model(None);
        agent.session.loading_replay = true;
        agent
            .session
            .bg_tasks
            .insert("m1".into(), running_bg_task("m1", true));
        assert_eq!(classify_top_level(&agent), RowState::Working);
        let row = top_level_row(AgentId(0), &agent, false, false, None);
        assert_eq!(
            row.activity.as_deref(),
            Some("Loading…"),
            "loading-replay activity must win over the background label",
        );
    }
    /// Roster-only idle / dormant sessions classify as `Inactive` — the
    /// dedicated section for sessions not loaded in this pager — while
    /// the active roster states keep their existing mapping. (Local
    /// idle agents stay `Idle`; `classify_top_level` never returns
    /// `Inactive` — pinned by `idle_local_agent_without_message_has_blank_secondary`.)
    #[test]
    fn roster_idle_and_dormant_classify_as_inactive() {
        let empty = std::collections::BTreeSet::new();
        for (activity, expected) in [
            (RosterActivity::Idle, RowState::Inactive),
            (RosterActivity::Dormant, RowState::Inactive),
            (RosterActivity::Working, RowState::Working),
            (RosterActivity::NeedsInput, RowState::NeedsInput),
            (RosterActivity::Completed, RowState::Completed),
            (RosterActivity::Dead, RowState::Failed),
        ] {
            let rows = collect_roster(&[roster_entry_with("m", Some("Task"), activity)], &empty);
            assert_eq!(rows.len(), 1, "activity={activity:?}");
            assert_eq!(rows[0].state, expected, "activity={activity:?}");
        }
    }
    /// In state grouping, `Inactive` sorts below `Idle` (not loaded
    /// here → less immediately actionable) but above `Done` / `Failed`
    /// (still live, resumable sessions).
    #[test]
    fn sort_rows_places_inactive_between_idle_and_done() {
        let mut rows = vec![
            make_row("done", 0, RowState::Completed),
            make_row("inactive", 0, RowState::Inactive),
            make_row("idle", 0, RowState::Idle),
            make_row("failed", 0, RowState::Failed),
        ];
        sort_rows(&mut rows, super::super::state::Grouping::State, &[]);
        let order: Vec<RowState> = rows.iter().map(|r| r.state).collect();
        assert_eq!(
            order,
            vec![
                RowState::Idle,
                RowState::Inactive,
                RowState::Completed,
                RowState::Failed,
            ],
        );
    }
}
