//! Plan mode state machine and prompt text generation.
//!
//! This module contains the [`PlanModeTracker`] struct that manages
//! the full plan mode lifecycle for a session. It is designed to be
//! testable in isolation — no references to `SessionActor`, conversation
//! history, or async I/O. Pure state machine logic.
//!
//! The `SessionActor` owns one `PlanModeTracker` (behind a `Mutex`) and
//! calls its methods at the appropriate points (`handle_session_mode`,
//! `handle_prompt`, `handle_completion`, `run_compact`).
use std::path::{Path, PathBuf};
/// Tracks plan mode lifecycle on the SessionActor.
///
/// Lives alongside `session_yolo_mode` and `active_agent_type` —
/// it is session-scoped mutable state, not part of AgentDefinition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanModeState {
    /// Normal operating mode. No plan mode constraints.
    Inactive,
    /// Client toggled plan mode ON, but no prompt has been sent yet.
    /// The model does not know about plan mode yet. No tool call has
    /// been made, no system-reminder injected.
    ///
    /// Transitions:
    ///   -> Active  (first user prompt triggers injection)
    ///   -> Inactive (client toggles off before any prompt)
    Pending,
    /// Plan mode is active. The model has received plan mode instructions
    /// (either via system-reminder injection or via EnterPlanMode tool result).
    /// Write tools are blocked except for the plan file.
    ///
    /// Transitions:
    ///   -> Inactive    (ExitPlanMode approved, or user toggles off when idle)
    ///   -> ExitPending (user toggles off while a turn is in-flight)
    Active,
    /// Client toggled plan mode OFF while Active and a model turn is
    /// in-flight. We need to wait for the current turn to finish (or
    /// cancel it), then cleanly exit.
    ///
    /// Transitions:
    ///   -> Inactive (after turn completes, exit attachment injected)
    ExitPending,
}
/// Tracks the full plan mode lifecycle for a session.
///
/// Designed to be testable in isolation — no references to SessionActor,
/// conversation history, or async I/O. Pure state machine logic.
///
/// The SessionActor owns one `PlanModeTracker` and calls its methods
/// at the appropriate points (handle_session_mode, handle_prompt,
/// handle_completion, run_compact).
pub struct PlanModeTracker {
    /// Current state in the lifecycle.
    state: PlanModeState,
    /// Whether plan mode was previously active in this session.
    /// Used for reentry detection — if true and we enter Active again,
    /// inject the reentry reminder instead of the standard one.
    was_previously_active: bool,
    /// Counter for full/sparse reminder alternation.
    /// Even = full reminder, odd = sparse. Reset on compaction.
    reminder_count: u32,
    /// Flag: inject a plan_mode_exit reminder on the next turn.
    /// Set only when the model has no in-context exit signal: user-initiated
    /// exits (toggle) and exits armed via [`Self::queue_exit_reminder`].
    pending_exit_reminder: bool,
    /// `exit_plan_mode` approval UI is outstanding (client has not answered).
    /// Persisted so resume can restore approval chrome.
    awaiting_plan_approval: bool,
    /// Rendered activation reminder buffered by a mid-turn toggle
    /// ([`Self::activate_mid_turn`]), awaiting delivery at the running turn's
    /// next safe drain point. While set, the model has NOT seen plan mode yet:
    /// a toggle-off withdraws it and rolls the activation back instead of
    /// deferring an exit the model never knew about. Not persisted — a restart
    /// loses the buffer, and the next turn's Active-state injection covers it.
    pending_activation: Option<PendingActivation>,
    /// Absolute path to the plan file on disk.
    /// Lives inside the session directory:
    /// `~/.echo-agent/sessions/<cwd>/<session_id>/plan.md`
    plan_file_path: PathBuf,
}
/// A buffered mid-turn activation reminder plus the state needed to roll the
/// activation back if it is withdrawn before delivery.
struct PendingActivation {
    /// Pre-wrapped `<system-reminder>` text, ready to push verbatim.
    text: String,
    /// `was_previously_active` before this activation, restored on withdrawal
    /// so a rolled-back activation doesn't fake a reentry.
    prior_was_previously_active: bool,
}
/// Serializable snapshot of plan mode lifecycle state.
///
/// Persisted to `plan_mode.json` in the session directory and restored on
/// session reload/resume so plan mode survives process restarts.
/// The `plan_file_path` is NOT persisted — it is recomputed from session metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanModeSnapshot {
    pub state: PlanModeState,
    pub was_previously_active: bool,
    pub reminder_count: u32,
    pub pending_exit_reminder: bool,
    /// Client was shown `exit_plan_mode` approval but has not answered yet.
    /// Survives process restart so the pager can restore approval chrome
    /// without treating every Active+plan.md session as pending.
    #[serde(default)]
    pub awaiting_plan_approval: bool,
}
impl PlanModeTracker {
    /// Create a new tracker. `session_dir` is the session's storage
    /// directory (e.g., `~/.echo-agent/sessions/<encoded-cwd>/<session-id>/`).
    pub fn new(session_dir: PathBuf) -> Self {
        Self {
            state: PlanModeState::Inactive,
            was_previously_active: false,
            reminder_count: 0,
            pending_exit_reminder: false,
            awaiting_plan_approval: false,
            pending_activation: None,
            plan_file_path: session_dir.join("plan.md"),
        }
    }
    /// Restore a tracker from a persisted snapshot.
    ///
    /// `session_dir` is used to recompute `plan_file_path`.
    /// If the snapshot has a transient state (`Pending` or `ExitPending`),
    /// it is collapsed: `Pending` → `Inactive`, `ExitPending` → `Inactive`
    /// (with exit reminder set), since those states depend on in-flight
    /// client/turn interactions that don't survive a restart.
    pub(crate) fn from_snapshot(session_dir: PathBuf, mut snapshot: PlanModeSnapshot) -> Self {
        match snapshot.state {
            PlanModeState::Pending => {
                snapshot.state = PlanModeState::Inactive;
            }
            PlanModeState::ExitPending => {
                snapshot.state = PlanModeState::Inactive;
                snapshot.pending_exit_reminder = true;
            }
            _ => {}
        }
        Self {
            state: snapshot.state,
            was_previously_active: snapshot.was_previously_active,
            reminder_count: snapshot.reminder_count,
            pending_exit_reminder: snapshot.pending_exit_reminder,
            awaiting_plan_approval: snapshot.awaiting_plan_approval,
            pending_activation: None,
            plan_file_path: session_dir.join("plan.md"),
        }
    }
    /// Mark that the client is waiting on plan approval (`exit_plan_mode` parked).
    pub(crate) fn set_awaiting_plan_approval(&mut self, awaiting: bool) {
        self.awaiting_plan_approval = awaiting;
    }
    /// Whether approval is outstanding (also true after resume from snapshot).
    pub(crate) fn is_awaiting_plan_approval(&self) -> bool {
        self.awaiting_plan_approval
    }
    /// Capture the current lifecycle state as a persistable snapshot.
    pub fn snapshot(&self) -> PlanModeSnapshot {
        PlanModeSnapshot {
            state: self.state,
            was_previously_active: self.was_previously_active,
            awaiting_plan_approval: self.awaiting_plan_approval,
            reminder_count: self.reminder_count,
            pending_exit_reminder: self.pending_exit_reminder,
        }
    }
    /// Returns the current plan mode state.
    pub fn state(&self) -> PlanModeState {
        self.state
    }
    /// Returns `true` if plan mode is currently active.
    pub fn is_active(&self) -> bool {
        self.state == PlanModeState::Active
    }
    /// The prompt mode the session is in according to this tracker.
    ///
    /// The prompt-mode mirrors follow the tracker, never the other way round,
    /// and a restored tracker is the only thing that knows a resumed session is
    /// still planning. Seeding a mirror `Agent` under a restored `Active` makes
    /// the first prompt resolve `Agent` and reconcile the plan mode away.
    pub(crate) fn session_prompt_mode(&self) -> PromptMode {
        if self.is_active() {
            PromptMode::Plan
        } else {
            PromptMode::Agent
        }
    }
    /// Returns the absolute path to the plan file.
    pub fn plan_file_path(&self) -> &Path {
        &self.plan_file_path
    }
    /// Returns `true` if plan mode is active and the given edit path
    /// targets the plan file. Used to bypass the permission prompt for
    /// plan file edits during plan mode.
    pub(crate) fn should_auto_approve_edit(&self, edit_path: &Path) -> bool {
        self.is_active() && is_plan_file_write(edit_path, &self.plan_file_path)
    }
    /// Whether the next reminder should be the full variant.
    /// Even count = full, odd count = sparse.
    pub(crate) fn should_use_full_reminder(&self) -> bool {
        self.reminder_count.is_multiple_of(2)
    }
    /// Whether we need to inject an exit reminder on the next turn.
    pub(crate) fn has_pending_exit_reminder(&self) -> bool {
        self.pending_exit_reminder
    }
    /// Whether this is a reentry (was previously in plan mode this session).
    pub(crate) fn is_reentry(&self) -> bool {
        self.was_previously_active && self.state == PlanModeState::Pending
    }
    /// Client toggled plan mode ON.
    ///
    /// Returns true if state actually changed. Handles re-entry from
    /// `ExitPending` by cancelling the deferred exit and returning
    /// directly to `Active` (the model already has plan mode context).
    pub(crate) fn enter_pending(&mut self) -> bool {
        match self.state {
            PlanModeState::Inactive => {
                self.state = PlanModeState::Pending;
                self.pending_exit_reminder = false;
                true
            }
            PlanModeState::ExitPending => {
                self.state = PlanModeState::Active;
                self.pending_exit_reminder = false;
                true
            }
            _ => false,
        }
    }
    /// First user prompt while Pending — activate plan mode.
    /// Returns true if state actually changed.
    pub fn activate(&mut self) -> bool {
        if self.state != PlanModeState::Pending {
            return false;
        }
        self.state = PlanModeState::Active;
        self.was_previously_active = true;
        self.reminder_count = 0;
        true
    }
    /// Mid-turn toggle: activate immediately and buffer the pre-rendered
    /// activation reminder for delivery at the running turn's next safe
    /// drain point. Only valid from `Pending` (an `ExitPending → Active`
    /// re-entry needs no reminder). Returns true if activated.
    ///
    /// The reminder is recorded (alternation counter) at delivery
    /// ([`Self::take_pending_activation`]), not here, so a withdrawn or
    /// restart-lost buffer doesn't advance the full/sparse cycle.
    pub(crate) fn activate_mid_turn(&mut self, rendered_reminder: String) -> bool {
        if self.state != PlanModeState::Pending {
            return false;
        }
        let prior_was_previously_active = self.was_previously_active;
        self.state = PlanModeState::Active;
        self.was_previously_active = true;
        self.reminder_count = 0;
        self.pending_activation = Some(PendingActivation {
            text: rendered_reminder,
            prior_was_previously_active,
        });
        true
    }
    /// Take the buffered mid-turn activation reminder for delivery.
    /// The caller pushes it into the conversation and then calls
    /// [`Self::record_reminder_injected`].
    pub(crate) fn take_pending_activation(&mut self) -> Option<String> {
        self.pending_activation.take().map(|p| p.text)
    }
    /// Whether a mid-turn activation reminder is buffered (undelivered).
    pub fn has_pending_activation(&self) -> bool {
        self.pending_activation.is_some()
    }
    /// Agent called EnterPlanMode tool \u{2014} go directly to Active.
    /// Returns true if state actually changed.
    pub(crate) fn activate_from_tool(&mut self) -> bool {
        if self.state != PlanModeState::Inactive {
            return false;
        }
        self.state = PlanModeState::Active;
        self.was_previously_active = true;
        self.reminder_count = 0;
        self.pending_exit_reminder = false;
        true
    }
    /// ExitPlanMode approved (agent-initiated exit).
    /// Returns true if state actually changed.
    ///
    /// Does NOT set `pending_exit_reminder`: callers must ensure the model gets
    /// an in-context exit signal — either by pushing a tool result that states
    /// the exit, or by explicitly arming [`Self::queue_exit_reminder`] when the
    /// result text carries no such signal. A reminder armed here would only
    /// drain at the next turn start, arriving a turn late and stale.
    pub(crate) fn deactivate_approved(&mut self) -> bool {
        if self.state != PlanModeState::Active {
            return false;
        }
        self.state = PlanModeState::Inactive;
        self.reminder_count = 0;
        self.awaiting_plan_approval = false;
        self.pending_activation = None;
        true
    }
    /// Client toggled plan mode OFF.
    /// `turn_in_flight`: whether a model turn is currently running.
    pub(crate) fn user_exit(&mut self, turn_in_flight: bool) {
        self.awaiting_plan_approval = false;
        if let Some(pending) = self.pending_activation.take()
            && self.state == PlanModeState::Active
        {
            self.state = PlanModeState::Inactive;
            self.was_previously_active = pending.prior_was_previously_active;
            return;
        }
        match self.state {
            PlanModeState::Pending => {
                self.state = PlanModeState::Inactive;
            }
            PlanModeState::Active => {
                if turn_in_flight {
                    self.state = PlanModeState::ExitPending;
                } else {
                    self.state = PlanModeState::Inactive;
                    self.pending_exit_reminder = true;
                }
            }
            _ => {}
        }
    }
    /// Current turn completed while in ExitPending.
    pub(crate) fn complete_deferred_exit(&mut self) {
        if self.state != PlanModeState::ExitPending {
            return;
        }
        self.state = PlanModeState::Inactive;
        self.pending_exit_reminder = true;
    }
    /// Arm the one-shot exit reminder for the next turn.
    ///
    /// For exit paths whose tool result carries no exit signal (the compat
    /// harness — policy and rationale live on the bridge's
    /// `queue_exit_reminder_on_approved_exit` flag).
    pub(crate) fn queue_exit_reminder(&mut self) {
        self.pending_exit_reminder = true;
    }
    /// Called after injecting a per-turn reminder. Advances the counter.
    pub(crate) fn record_reminder_injected(&mut self) {
        self.reminder_count += 1;
    }
    /// Called after injecting the exit reminder. Clears the flag.
    pub(crate) fn clear_pending_exit_reminder(&mut self) {
        self.pending_exit_reminder = false;
    }
    /// Called after compaction. Resets reminder counter so next
    /// injection is the full variant.
    pub(crate) fn reset_after_compaction(&mut self) {
        if self.state == PlanModeState::Active {
            self.reminder_count = 0;
            self.pending_activation = None;
        }
    }
}
/// Full plan mode reminder template (plan-file write rules + turn-ending tools).
///
/// Returns a MiniJinja template string with `${{ tools.by_kind.X }}` and
/// `${{ plan_path }}` / `${{ plan_has_content }}` placeholders. The caller must
/// render it via `TemplateRenderer::render_with_extra()` passing:
///
/// ```json
/// { "plan_path": "/path/to/plan.md", "plan_has_content": true }
/// ```
///
/// Tool name placeholders (`${{ tools.by_kind.edit }}`, etc.) are resolved
/// automatically from the registry's `ToolKind` \u{2192} client-facing name mapping.
pub(crate) fn plan_mode_reminder_full_template() -> &'static str {
    "\
Plan mode is active. Do not make any edits or writes to the system.

## Plan File:
${%- if plan_has_content %}
A plan file exists at ${{ plan_path }}. \
You can read it and make edits using the ${{ tools.by_kind.edit }} tool.
${%- else %}
No plan written yet. Write your plan to ${{ plan_path }} \
using the ${{ tools.by_kind.edit }} tool.
${%- endif %}

You should build your plan by writing to or editing this file. \
Note that this is the only file you are allowed to edit.

Your turn should only end with either ${{ tools.by_kind.ask_user }} to clarify \
requirements or ${{ tools.by_kind.exit_plan }} to present your plan to the user."
}
/// Sparse plan mode reminder template.
///
/// Static string for alternating turns (when `reminder_count` is odd) to save
/// tokens. No MiniJinja placeholders — plan path and tool names are only in the
/// full reminder.
pub(crate) fn plan_mode_reminder_sparse_template() -> &'static str {
    "Plan mode is still active. Do not make any edits or writes to the system except for the plan file."
}
/// Reentry reminder template.
///
/// Returns a MiniJinja template string injected when entering plan mode for
/// the second+ time in the same session. Render via
/// `TemplateRenderer::render_with_extra()` with `{ "plan_path": "..." }`.
pub(crate) fn plan_mode_reentry_reminder_template() -> &'static str {
    "\
## Returning to Plan Mode

You are entering plan mode again after having previously exited it. \
A plan file exists at ${{ plan_path }} from your previous planning session.

Your turn should only end with either ${{ tools.by_kind.ask_user }} to clarify requirements or ${{ tools.by_kind.exit_plan }} to present your plan to the user."
}
/// Rejection message for an edit outside the plan file while plan mode is
/// active. Returned as the tool result so the model knows the only editable
/// path.
///
/// Render via `TemplateRenderer::render_with_extra()` with
/// `{ "plan_path": "..." }`.
pub(crate) fn plan_mode_edit_rejected_template() -> &'static str {
    "Rejected: file edits are not allowed in plan mode - the only editable file is the plan file (${{ plan_path }})."
}
/// Exit reminder template.
///
/// Returns a MiniJinja template string injected once after exiting plan mode
/// (user-initiated exit via toggle). Contains no placeholders.
pub(crate) fn plan_mode_exit_reminder_template() -> &'static str {
    "\
You have exited plan mode. You can now make edits, run tools, and take actions."
}
/// Check if a write target matches the plan file.
///
/// `target_path` is the absolute path the tool is trying to write to.
/// `plan_file` is the absolute path from [`PlanModeTracker::plan_file_path`].
pub(crate) fn is_plan_file_write(target_path: &Path, plan_file: &Path) -> bool {
    target_path == plan_file
}
/// Whether the path's final component ends with a markdown suffix (case-insensitive).
///
/// Suffixes align with client / workspace `MARKDOWN_SUFFIXES`:
/// `.md`, `.markdown`, `.mdown`, `.mkd`, `.mkdn`, `.mdx`.
///
/// In plan mode the shell rejects `Write` and `StrReplace` when this is
/// false while plan mode is active (see `prepare_tool_call` in `acp_session.rs`).
pub(crate) fn is_markdown_file_path(path: &Path) -> bool {
    const MARKDOWN_SUFFIXES: &[&str] = &[".md", ".markdown", ".mdown", ".mkd", ".mkdn", ".mdx"];
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let bytes = name.as_bytes();
    MARKDOWN_SUFFIXES.iter().any(|suffix| {
        let suffix = suffix.as_bytes();
        bytes.len() >= suffix.len()
            && bytes[bytes.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    })
}
/// True if a plan file exists at `path` with non-zero size. An empty
/// pre-seeded plan file (created by enter_plan_mode) reports false so the
/// reminder still tells the model to write its plan.
///
/// Divergence: uses `metadata().len() > 0` (cheap per-turn stat), so a
/// whitespace-only file counts as content here whereas `exit_plan_mode` trims
/// and treats it as empty; harmless because the seed is always `b""`.
pub(crate) async fn plan_file_has_content(path: &std::path::Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}
/// The prompt mode sent by the client in `_meta.mode`.
///
/// Determines whether the prompt expects tool use / file edits (`Agent`) or
/// is read-only (`Ask` / `Plan`). Used to decide whether a forked session
/// needs worktrees or can run in read-only mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, strum::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PromptMode {
    /// Full agent with tool use and file edits.
    #[default]
    Agent,
    /// Question-answering only, no tool use.
    Ask,
    /// Planning/reasoning only, no tool use.
    Plan,
}
impl PromptMode {
    /// Parse from the `_meta.mode` string. Unknown values default to `Agent`.
    pub(crate) fn from_meta_str(s: &str) -> Self {
        match s {
            "ask" => Self::Ask,
            "plan" => Self::Plan,
            _ => Self::Agent,
        }
    }
    /// Whether this mode is read-only (no file mutations expected).
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Ask | Self::Plan)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn test_tracker() -> PlanModeTracker {
        PlanModeTracker::new(PathBuf::from("/tmp/test-session"))
    }
    #[test]
    fn user_initiated_lifecycle() {
        let mut t = test_tracker();
        assert_eq!(t.state(), PlanModeState::Inactive);
        assert!(t.enter_pending());
        assert_eq!(t.state(), PlanModeState::Pending);
        assert!(t.activate());
        assert_eq!(t.state(), PlanModeState::Active);
        assert!(t.deactivate_approved());
        assert_eq!(t.state(), PlanModeState::Inactive);
    }
    #[test]
    fn user_exit_while_turn_in_flight() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.user_exit(true);
        assert_eq!(t.state(), PlanModeState::ExitPending);
        t.complete_deferred_exit();
        assert_eq!(t.state(), PlanModeState::Inactive);
        assert!(t.has_pending_exit_reminder());
    }
    #[test]
    fn pending_cancel_is_clean() {
        let mut t = test_tracker();
        t.enter_pending();
        t.user_exit(false);
        assert_eq!(t.state(), PlanModeState::Inactive);
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn agent_initiated_skips_pending() {
        let mut t = test_tracker();
        assert!(t.activate_from_tool());
        assert_eq!(t.state(), PlanModeState::Active);
    }
    #[test]
    fn reentry_detected() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.deactivate_approved();
        t.enter_pending();
        assert!(t.is_reentry());
    }
    #[test]
    fn reminder_alternation() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        assert!(t.should_use_full_reminder());
        t.record_reminder_injected();
        assert!(!t.should_use_full_reminder());
        t.record_reminder_injected();
        assert!(t.should_use_full_reminder());
    }
    #[test]
    fn plan_file_in_session_dir() {
        let t = PlanModeTracker::new(PathBuf::from(
            "/home/user/.echo-agent/sessions/proj/abc-123",
        ));
        assert_eq!(
            t.plan_file_path(),
            Path::new("/home/user/.echo-agent/sessions/proj/abc-123/plan.md")
        );
    }
    #[test]
    fn compaction_resets_to_full_reminder() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.record_reminder_injected();
        t.reset_after_compaction();
        assert!(t.should_use_full_reminder());
    }
    #[test]
    fn midturn_activation_buffers_and_delivers_exactly_once() {
        let mut t = test_tracker();
        t.enter_pending();
        assert!(t.activate_mid_turn("reminder text".into()));
        assert_eq!(t.state(), PlanModeState::Active);
        assert!(t.has_pending_activation());
        assert!(t.should_use_full_reminder());
        assert_eq!(
            t.take_pending_activation().as_deref(),
            Some("reminder text")
        );
        assert!(!t.has_pending_activation());
        t.record_reminder_injected();
        assert!(!t.should_use_full_reminder());
        assert_eq!(t.take_pending_activation(), None);
        assert_eq!(t.take_pending_activation(), None);
    }
    #[test]
    fn midturn_activation_requires_pending() {
        let mut t = test_tracker();
        assert!(!t.activate_mid_turn("x".into()));
        t.enter_pending();
        t.activate();
        assert!(!t.activate_mid_turn("dup".into()));
        assert!(!t.has_pending_activation());
        t.user_exit(true);
        assert!(!t.activate_mid_turn("x".into()));
        assert!(!t.has_pending_activation());
    }
    #[test]
    fn user_exit_withdraws_undelivered_activation() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate_mid_turn("reminder text".into());
        t.user_exit(true);
        assert_eq!(t.state(), PlanModeState::Inactive);
        assert!(!t.has_pending_activation());
        assert!(!t.has_pending_exit_reminder());
        t.enter_pending();
        assert!(!t.is_reentry());
    }
    #[test]
    fn user_exit_after_delivery_defers_exit_normally() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate_mid_turn("reminder text".into());
        t.take_pending_activation();
        t.record_reminder_injected();
        t.user_exit(true);
        assert_eq!(t.state(), PlanModeState::ExitPending);
    }
    #[test]
    fn withdrawal_preserves_real_reentry_flag() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.deactivate_approved();
        t.enter_pending();
        t.activate_mid_turn("reminder text".into());
        t.user_exit(true);
        t.enter_pending();
        assert!(t.is_reentry());
    }
    #[test]
    fn compaction_drops_undelivered_activation() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate_mid_turn("reminder text".into());
        t.reset_after_compaction();
        assert!(!t.has_pending_activation());
        assert_eq!(t.state(), PlanModeState::Active);
    }
    use echo_agent_tools::types::template_renderer::TemplateRenderer;
    use echo_agent_tools::types::tool::ToolKind;
    use std::collections::HashMap;
    /// Build a test TemplateRenderer with standard EchoAgent Build tool mappings.
    fn test_renderer() -> TemplateRenderer {
        let tools: HashMap<ToolKind, String> = [
            (ToolKind::Edit, "search_replace".to_owned()),
            (ToolKind::Read, "read_file".to_owned()),
            (ToolKind::List, "list_dir".to_owned()),
            (ToolKind::Search, "grep".to_owned()),
            (ToolKind::AskUser, "ask_user_question".to_owned()),
            (ToolKind::ExitPlan, "exit_plan_mode".to_owned()),
        ]
        .into();
        TemplateRenderer::new(tools, HashMap::new())
    }
    /// Build a test TemplateRenderer that includes the Task tool.
    fn test_renderer_with_task() -> TemplateRenderer {
        let tools: HashMap<ToolKind, String> = [
            (ToolKind::Edit, "search_replace".to_owned()),
            (ToolKind::Read, "read_file".to_owned()),
            (ToolKind::List, "list_dir".to_owned()),
            (ToolKind::Search, "grep".to_owned()),
            (ToolKind::AskUser, "ask_user_question".to_owned()),
            (ToolKind::ExitPlan, "exit_plan_mode".to_owned()),
            (ToolKind::Task, "task".to_owned()),
        ]
        .into();
        TemplateRenderer::new(tools, HashMap::new())
    }
    /// Build a TemplateRenderer with custom (non-default) tool names.
    fn custom_renderer() -> TemplateRenderer {
        let tools: HashMap<ToolKind, String> = [
            (ToolKind::Edit, "EditFile".to_owned()),
            (ToolKind::Read, "ReadFile".to_owned()),
            (ToolKind::List, "ListFiles".to_owned()),
            (ToolKind::Search, "SearchContent".to_owned()),
            (ToolKind::AskUser, "AskUser".to_owned()),
            (ToolKind::ExitPlan, "FinishPlan".to_owned()),
        ]
        .into();
        TemplateRenderer::new(tools, HashMap::new())
    }
    fn render(
        renderer: &TemplateRenderer,
        template: &str,
        plan_path: &str,
        plan_has_content: bool,
    ) -> String {
        let extra = serde_json::json!({
            "plan_path": plan_path,
            "plan_has_content": plan_has_content,
        });
        renderer.render_with_extra(template, &extra).unwrap()
    }
    #[test]
    fn full_reminder_with_existing_plan() {
        let r = test_renderer();
        let text = render(
            &r,
            plan_mode_reminder_full_template(),
            "/tmp/session/plan.md",
            true,
        );
        assert!(text.contains("A plan file exists at /tmp/session/plan.md"));
        assert!(text.contains("search_replace tool"));
        assert!(text.contains("Plan mode is active"));
        assert!(text.contains("## Plan File:"));
        assert!(text.contains("only file you are allowed to edit"));
        assert!(!text.contains("No plan written yet"));
    }
    #[test]
    fn full_reminder_without_plan() {
        let r = test_renderer();
        let text = render(
            &r,
            plan_mode_reminder_full_template(),
            "/tmp/session/plan.md",
            false,
        );
        assert!(text.contains("No plan written yet"));
        assert!(text.contains("/tmp/session/plan.md"));
        assert!(text.contains("search_replace tool"));
        assert!(text.contains("Plan mode is active"));
        assert!(!text.contains("A plan file exists at"));
    }
    #[test]
    fn full_reminder_resolves_all_tool_names() {
        let r = test_renderer();
        let text = render(&r, plan_mode_reminder_full_template(), "/tmp/plan.md", true);
        assert!(text.contains("search_replace tool"));
        assert!(text.contains("ask_user_question to clarify requirements"));
        assert!(text.contains("exit_plan_mode to present your plan to the user"));
        assert!(
            !text.contains("${{"),
            "unresolved template placeholder found"
        );
    }
    #[test]
    fn full_reminder_with_custom_tool_names() {
        let r = custom_renderer();
        let text = render(&r, plan_mode_reminder_full_template(), "/tmp/plan.md", true);
        assert!(text.contains("EditFile tool"));
        assert!(text.contains("AskUser to clarify requirements"));
        assert!(text.contains("FinishPlan to present your plan to the user"));
        assert!(!text.contains("search_replace"));
        assert!(!text.contains("ask_user_question"));
        assert!(!text.contains("exit_plan_mode"));
    }
    #[test]
    fn full_reminder_has_no_subagent_guidance() {
        let r = test_renderer_with_task();
        let text = render(&r, plan_mode_reminder_full_template(), "/tmp/plan.md", true);
        assert!(
            !text.contains("subagent_type"),
            "full reminder should not include subagent guidance: {text}"
        );
        let r = test_renderer();
        let text = render(&r, plan_mode_reminder_full_template(), "/tmp/plan.md", true);
        assert!(!text.contains("subagent_type"));
    }
    #[test]
    fn full_reminder_has_no_phase_workflow() {
        let r = test_renderer();
        let text = render(&r, plan_mode_reminder_full_template(), "/tmp/plan.md", true);
        assert!(!text.contains("Phase 1:"));
        assert!(!text.contains("Plan Workflow"));
        assert!(!text.contains("Iterative Planning Workflow"));
        assert!(!text.contains("The Loop"));
    }
    #[test]
    fn sparse_reminder_is_static_read_only_nudge() {
        let r = test_renderer();
        let text = render(
            &r,
            plan_mode_reminder_sparse_template(),
            "/tmp/plan.md",
            false,
        );
        assert_eq!(
            text,
            "Plan mode is still active. Do not make any edits or writes to the system except for the plan file."
        );
        assert!(!text.contains("/tmp/plan.md"));
        assert!(!text.contains("exit_plan_mode"));
        assert!(!text.contains("${{"));
    }
    #[test]
    fn sparse_reminder_ignores_custom_tool_names() {
        let r = custom_renderer();
        let text = render(
            &r,
            plan_mode_reminder_sparse_template(),
            "/tmp/plan.md",
            false,
        );
        assert!(!text.contains("AskUser"));
        assert!(!text.contains("FinishPlan"));
        assert!(text.contains("Plan mode is still active"));
    }
    #[test]
    fn reentry_reminder_renders() {
        let r = test_renderer();
        let text = render(
            &r,
            plan_mode_reentry_reminder_template(),
            "/tmp/plan.md",
            false,
        );
        assert!(text.contains("Returning to Plan Mode"));
        assert!(text.contains("/tmp/plan.md"));
        assert!(text.contains("entering plan mode again"));
        assert!(text.contains("exit_plan_mode"));
        assert!(text.contains("ask_user_question"));
        assert!(!text.contains("${{"));
    }
    #[test]
    fn reentry_reminder_with_custom_names() {
        let r = custom_renderer();
        let text = render(
            &r,
            plan_mode_reentry_reminder_template(),
            "/tmp/plan.md",
            false,
        );
        assert!(text.contains("FinishPlan to present your plan to the user"));
        assert!(text.contains("AskUser to clarify requirements"));
        assert!(!text.contains("exit_plan_mode"));
        assert!(!text.contains("ask_user_question"));
    }
    #[test]
    fn exit_reminder_renders() {
        let r = test_renderer();
        let text = render(
            &r,
            plan_mode_exit_reminder_template(),
            "/tmp/plan.md",
            false,
        );
        assert_eq!(
            text,
            "You have exited plan mode. You can now make edits, run tools, and take actions."
        );
        assert!(!text.contains("/tmp/plan.md"));
        assert!(!text.contains("/implement"));
        assert!(!text.contains("${{"));
    }
    #[test]
    fn edit_rejected_template_renders() {
        let r = test_renderer();
        let text = render(
            &r,
            plan_mode_edit_rejected_template(),
            "/tmp/session/plan.md",
            false,
        );
        assert_eq!(
            text,
            "Rejected: file edits are not allowed in plan mode - the only editable file is the plan file (/tmp/session/plan.md)."
        );
    }
    #[test]
    fn templates_are_static_with_no_hardcoded_tool_names() {
        let hardcoded_names = [
            "search_replace",
            "read_file",
            "list_dir",
            "grep",
            "ask_user_question",
            "exit_plan_mode",
        ];
        let templates = [
            plan_mode_reminder_full_template(),
            plan_mode_reminder_sparse_template(),
            plan_mode_reentry_reminder_template(),
            plan_mode_exit_reminder_template(),
            plan_mode_edit_rejected_template(),
        ];
        for template in &templates {
            for name in &hardcoded_names {
                assert!(
                    !template.contains(name),
                    "template contains hardcoded tool name '{name}': {template:.80}..."
                );
            }
        }
    }
    #[test]
    fn is_plan_file_write_exact_match() {
        let plan = Path::new("/home/user/.echo-agent/sessions/proj/abc/plan.md");
        let target = Path::new("/home/user/.echo-agent/sessions/proj/abc/plan.md");
        assert!(is_plan_file_write(target, plan));
    }
    #[test]
    fn is_plan_file_write_different_path() {
        let plan = Path::new("/home/user/.echo-agent/sessions/proj/abc/plan.md");
        let target = Path::new("/home/user/project/src/main.rs");
        assert!(!is_plan_file_write(target, plan));
    }
    #[test]
    fn is_markdown_file_path_recognizes_extensions() {
        assert!(is_markdown_file_path(Path::new("/x/plan.md")));
        assert!(is_markdown_file_path(Path::new("notes.MDX")));
        assert!(is_markdown_file_path(Path::new("readme.markdown")));
        assert!(is_markdown_file_path(Path::new("/a/guide.mdown")));
        assert!(is_markdown_file_path(Path::new("x.mkd")));
        assert!(is_markdown_file_path(Path::new("x.MKDN")));
        assert!(!is_markdown_file_path(Path::new("/src/lib.rs")));
        assert!(!is_markdown_file_path(Path::new("/no-extension")));
        assert!(!is_markdown_file_path(Path::new("/src/notmd.rs")));
        assert!(!is_markdown_file_path(Path::new("企业AI决策清单.html")));
        assert!(is_markdown_file_path(Path::new("企业AI决策清单.md")));
        assert!(is_markdown_file_path(Path::new("计划.markdown")));
        assert!(!is_markdown_file_path(Path::new("md")));
        assert!(!is_markdown_file_path(Path::new("x")));
    }
    #[test]
    fn auto_approve_edit_when_active_and_plan_file() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        let plan = t.plan_file_path().to_path_buf();
        assert!(t.should_auto_approve_edit(&plan));
    }
    #[test]
    fn no_auto_approve_edit_when_active_but_different_file() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        assert!(!t.should_auto_approve_edit(Path::new("/some/other/file.rs")));
    }
    #[test]
    fn no_auto_approve_edit_when_inactive() {
        let t = test_tracker();
        let plan = t.plan_file_path().to_path_buf();
        assert!(!t.should_auto_approve_edit(&plan));
    }
    #[test]
    fn no_auto_approve_edit_when_pending() {
        let mut t = test_tracker();
        t.enter_pending();
        let plan = t.plan_file_path().to_path_buf();
        assert!(!t.should_auto_approve_edit(&plan));
    }
    #[test]
    fn double_enter_pending_is_noop() {
        let mut t = test_tracker();
        assert!(t.enter_pending());
        assert!(!t.enter_pending());
        assert_eq!(t.state(), PlanModeState::Pending);
    }
    #[test]
    fn activate_from_inactive_only() {
        let mut t = test_tracker();
        assert!(!t.activate());
        assert_eq!(t.state(), PlanModeState::Inactive);
    }
    #[test]
    fn activate_from_tool_when_already_active() {
        let mut t = test_tracker();
        t.activate_from_tool();
        assert!(!t.activate_from_tool());
        assert_eq!(t.state(), PlanModeState::Active);
    }
    #[test]
    fn deactivate_when_not_active() {
        let mut t = test_tracker();
        assert!(!t.deactivate_approved());
        assert_eq!(t.state(), PlanModeState::Inactive);
    }
    #[test]
    fn user_exit_from_inactive_is_noop() {
        let mut t = test_tracker();
        t.user_exit(false);
        assert_eq!(t.state(), PlanModeState::Inactive);
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn complete_deferred_exit_when_not_exit_pending_is_noop() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.complete_deferred_exit();
        assert_eq!(t.state(), PlanModeState::Active);
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn user_exit_while_idle_sets_exit_reminder() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.user_exit(false);
        assert_eq!(t.state(), PlanModeState::Inactive);
        assert!(t.has_pending_exit_reminder());
        t.clear_pending_exit_reminder();
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn enter_pending_clears_pending_exit_reminder() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.user_exit(false);
        assert!(t.has_pending_exit_reminder());
        t.enter_pending();
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn activate_from_tool_clears_pending_exit_reminder() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.user_exit(false);
        assert!(t.has_pending_exit_reminder());
        t.activate_from_tool();
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn deactivate_approved_does_not_set_pending_exit_reminder() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        assert!(!t.has_pending_exit_reminder());
        t.deactivate_approved();
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn queue_exit_reminder_arms_flag() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.deactivate_approved();
        assert!(!t.has_pending_exit_reminder());
        t.queue_exit_reminder();
        assert!(t.has_pending_exit_reminder());
        t.clear_pending_exit_reminder();
        assert!(!t.has_pending_exit_reminder());
    }
    #[test]
    fn compaction_reset_only_when_active() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.record_reminder_injected();
        t.deactivate_approved();
        t.reset_after_compaction();
    }
    #[test]
    fn snapshot_round_trip_active() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.record_reminder_injected();
        let snap = t.snapshot();
        assert_eq!(snap.state, PlanModeState::Active);
        assert!(snap.was_previously_active);
        assert_eq!(snap.reminder_count, 1);
        let restored = PlanModeTracker::from_snapshot(PathBuf::from("/tmp/test-session"), snap);
        assert_eq!(restored.state(), PlanModeState::Active);
        assert!(!restored.should_use_full_reminder());
    }
    /// A resumed session that was planning still reports `Plan`. The mirrors
    /// are seeded from this at spawn: seeding `Agent` instead makes the first
    /// prompt resolve `Agent`, reconcile, and drop the plan mode silently.
    #[test]
    fn session_prompt_mode_follows_a_restored_active_tracker() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        let restored = PlanModeTracker::from_snapshot(PathBuf::from("/tmp/test"), t.snapshot());
        assert_eq!(restored.state(), PlanModeState::Active);
        assert_eq!(restored.session_prompt_mode(), PromptMode::Plan);
        assert_eq!(
            test_tracker().session_prompt_mode(),
            PromptMode::Agent,
            "an inactive tracker is agent mode"
        );
    }
    #[test]
    fn snapshot_pending_collapses_to_inactive() {
        let mut t = test_tracker();
        t.enter_pending();
        let snap = t.snapshot();
        assert_eq!(snap.state, PlanModeState::Pending);
        let restored = PlanModeTracker::from_snapshot(PathBuf::from("/tmp/test-session"), snap);
        assert_eq!(restored.state(), PlanModeState::Inactive);
    }
    #[test]
    fn snapshot_exit_pending_collapses_to_inactive_with_reminder() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.user_exit(true);
        let snap = t.snapshot();
        assert_eq!(snap.state, PlanModeState::ExitPending);
        let restored = PlanModeTracker::from_snapshot(PathBuf::from("/tmp/test-session"), snap);
        assert_eq!(restored.state(), PlanModeState::Inactive);
        assert!(restored.has_pending_exit_reminder());
    }
    #[test]
    fn snapshot_inactive_restores_cleanly() {
        let t = test_tracker();
        let snap = t.snapshot();
        let restored = PlanModeTracker::from_snapshot(PathBuf::from("/tmp/test-session"), snap);
        assert_eq!(restored.state(), PlanModeState::Inactive);
        assert!(!restored.has_pending_exit_reminder());
    }
    #[test]
    fn reenter_from_exit_pending_cancels_deferred_exit() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.user_exit(true);
        assert_eq!(t.state(), PlanModeState::ExitPending);
        assert!(t.enter_pending());
        assert_eq!(t.state(), PlanModeState::Active);
        assert!(!t.has_pending_exit_reminder());
        t.complete_deferred_exit();
        assert_eq!(t.state(), PlanModeState::Active);
    }
    #[test]
    fn was_previously_active_persists_through_agent_exit() {
        let mut t = test_tracker();
        t.activate_from_tool();
        assert!(t.is_active());
        t.deactivate_approved();
        assert_eq!(t.state(), PlanModeState::Inactive);
        t.enter_pending();
        assert!(t.is_reentry());
    }
    #[test]
    fn full_lifecycle_with_exit_pending() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        assert!(t.should_use_full_reminder());
        t.record_reminder_injected();
        assert!(!t.should_use_full_reminder());
        t.record_reminder_injected();
        t.user_exit(true);
        assert_eq!(t.state(), PlanModeState::ExitPending);
        t.complete_deferred_exit();
        assert_eq!(t.state(), PlanModeState::Inactive);
        assert!(t.has_pending_exit_reminder());
        t.clear_pending_exit_reminder();
        assert!(!t.has_pending_exit_reminder());
        t.enter_pending();
        assert!(t.is_reentry());
        t.activate();
        assert_eq!(t.state(), PlanModeState::Active);
        assert!(t.should_use_full_reminder());
    }
    #[test]
    fn test_prompt_mode_from_meta_str_known_values() {
        assert_eq!(PromptMode::from_meta_str("ask"), PromptMode::Ask);
        assert_eq!(PromptMode::from_meta_str("plan"), PromptMode::Plan);
        assert_eq!(PromptMode::from_meta_str("agent"), PromptMode::Agent);
    }
    #[test]
    fn test_prompt_mode_from_meta_str_unknown_defaults_to_agent() {
        assert_eq!(PromptMode::from_meta_str(""), PromptMode::Agent);
        assert_eq!(PromptMode::from_meta_str("unknown"), PromptMode::Agent);
        assert_eq!(PromptMode::from_meta_str("ASK"), PromptMode::Agent);
        assert_eq!(PromptMode::from_meta_str("Plan"), PromptMode::Agent);
        assert_eq!(PromptMode::from_meta_str("code"), PromptMode::Agent);
    }
    #[test]
    fn test_prompt_mode_is_read_only() {
        assert!(!PromptMode::Agent.is_read_only());
        assert!(PromptMode::Ask.is_read_only());
        assert!(PromptMode::Plan.is_read_only());
    }
    #[test]
    fn test_prompt_mode_default_is_agent() {
        assert_eq!(PromptMode::default(), PromptMode::Agent);
    }
    #[test]
    fn test_prompt_mode_serde_round_trip() {
        for mode in [PromptMode::Agent, PromptMode::Ask, PromptMode::Plan] {
            let json = serde_json::to_string(&mode).unwrap();
            let deserialized: PromptMode = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, mode, "round-trip failed for {json}");
        }
    }
    #[test]
    fn test_prompt_mode_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&PromptMode::Agent).unwrap(),
            r#""agent""#
        );
        assert_eq!(serde_json::to_string(&PromptMode::Ask).unwrap(), r#""ask""#);
        assert_eq!(
            serde_json::to_string(&PromptMode::Plan).unwrap(),
            r#""plan""#
        );
    }
    #[test]
    fn awaiting_plan_approval_survives_snapshot_round_trip() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.set_awaiting_plan_approval(true);
        assert!(t.is_awaiting_plan_approval());
        let restored =
            PlanModeTracker::from_snapshot(PathBuf::from("/tmp/test-session"), t.snapshot());
        assert_eq!(restored.state(), PlanModeState::Active);
        assert!(restored.is_awaiting_plan_approval());
    }
    #[test]
    fn deactivate_approved_clears_awaiting_flag() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.set_awaiting_plan_approval(true);
        t.deactivate_approved();
        assert!(!t.is_awaiting_plan_approval());
    }
    #[test]
    fn user_exit_clears_awaiting_flag() {
        let mut t = test_tracker();
        t.enter_pending();
        t.activate();
        t.set_awaiting_plan_approval(true);
        t.user_exit(false);
        assert!(!t.is_awaiting_plan_approval());
    }
    #[test]
    fn snapshot_without_awaiting_field_defaults_false() {
        let legacy = r#"{
            "state": "Active",
            "was_previously_active": true,
            "reminder_count": 0,
            "pending_exit_reminder": false
        }"#;
        let snapshot: PlanModeSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(!snapshot.awaiting_plan_approval);
        let restored = PlanModeTracker::from_snapshot(PathBuf::from("/tmp/test-session"), snapshot);
        assert!(!restored.is_awaiting_plan_approval());
    }
}
