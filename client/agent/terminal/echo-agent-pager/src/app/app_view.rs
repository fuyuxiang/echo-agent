//! Root view component.
//!
//! [`AppView`] owns all application state and provides the top-level
//! `handle_input()` and `draw()` methods. The event loop calls these
//! and knows nothing about input routing, overlays, or view internals.
use super::ScreenMode;
use crate::acp::model_state::ModelState;
use crate::actions::{ActionId, ActionRegistry, When};
use crate::app::consent::ConsentState;
use crate::appearance::AppearanceConfig;
use crate::input::KeyboardNormalizer;
use crate::input::key::KeyShortcut;
use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::input::mouse::{MouseScrollState, ScrollConfig, ScrollDirection};
use crate::key;
use crate::notifications::NotificationService;
use crate::render::draw::CursorState;
use crate::scrollback::render::ScratchBuffer;
use crate::views::prompt_widget::PromptWidget;
use crate::views::welcome::WelcomePromptFocus;
use agent_client_protocol as acp;
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use echo_agent_acp::AcpAgentTx;
use indexmap::IndexMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
/// State for the "New Worktree" popup dialog on the welcome screen.
#[derive(Debug, Default)]
pub struct NewWorktreeDialogState {
    /// Text input for the worktree label (empty = auto-generated name).
    label: LineEditor,
}
const MAX_WORKTREE_LABEL_BYTES: usize = 100;
impl NewWorktreeDialogState {
    pub fn new() -> Self {
        Self {
            label: LineEditor::default(),
        }
    }
    pub fn label(&self) -> &str {
        self.label.text()
    }
    pub(crate) fn viewport(&self, width: usize) -> echo_agent_ratatui_textarea::SingleLineViewport {
        self.label.viewport(width)
    }
    #[cfg(test)]
    pub(crate) fn set_label(&mut self, label: impl Into<String>) {
        self.label.set_text(label);
    }
    #[cfg(test)]
    pub(crate) fn set_cursor_byte(&mut self, cursor_byte: usize) -> LineEditOutcome {
        self.label.set_cursor_byte(cursor_byte)
    }
    pub fn insert_paste(&mut self, text: &str) -> NewWorktreeDialogOutcome {
        Self::from_line_edit(
            self.label
                .insert_paste_with_byte_limit(text, MAX_WORKTREE_LABEL_BYTES),
        )
    }
    /// Handle a key event. Returns the dialog outcome.
    pub fn handle_key(&mut self, key: &crossterm::event::KeyEvent) -> NewWorktreeDialogOutcome {
        use crossterm::event::{KeyCode, KeyModifiers};
        if crate::input::key::is_paste_key(key) {
            return crate::clipboard::system_clipboard_get()
                .map_or(NewWorktreeDialogOutcome::Unchanged, |text| {
                    self.insert_paste(&text)
                });
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !crate::input::key::is_altgr(key.modifiers)
            && matches!(key.code, KeyCode::Char('c' | 'd' | 'q'))
        {
            return NewWorktreeDialogOutcome::Cancelled;
        }
        if key.code == KeyCode::Enter && !key.modifiers.is_empty() {
            return NewWorktreeDialogOutcome::Unchanged;
        }
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                let label = self.label().trim().to_string();
                NewWorktreeDialogOutcome::Submitted(if label.is_empty() {
                    None
                } else {
                    Some(label)
                })
            }
            KeyCode::Esc => NewWorktreeDialogOutcome::Cancelled,
            _ => {
                let remaining = MAX_WORKTREE_LABEL_BYTES.saturating_sub(self.label().len());
                let outcome = self.label.handle_key_with_insert_policy(key, |character| {
                    character.len_utf8() <= remaining
                });
                Self::from_line_edit(outcome)
            }
        }
    }
    fn from_line_edit(outcome: LineEditOutcome) -> NewWorktreeDialogOutcome {
        match outcome {
            LineEditOutcome::TextChanged
            | LineEditOutcome::CursorChanged
            | LineEditOutcome::HandledNoChange => NewWorktreeDialogOutcome::Changed,
            LineEditOutcome::Unhandled => NewWorktreeDialogOutcome::Unchanged,
        }
    }
}
/// Per-visit announcement UI state on the welcome screen. Reset on every
/// return-to-welcome transition (see `show_welcome`) so a previously expanded
/// announcement can't leak into a freshly shown screen; the non-`expanded`
/// fields are recomputed each frame, so resetting them is harmless.
#[derive(Debug, Default)]
pub struct WelcomeAnnouncementState {
    /// Whether a long announcement is expanded inline (default: 2 lines + `…`).
    pub expanded: bool,
    /// Mouse last over the announcement block (drives hover color + redraws).
    pub on_cta: bool,
    /// Whether the announcement overflowed (the "expandable" signal).
    pub truncated: bool,
    /// Hit-test rect for the full announcement block (click anywhere to toggle).
    pub rect: Option<ratatui::layout::Rect>,
}
/// Outcome of handling input in the new-worktree dialog.
#[derive(Debug)]
pub enum NewWorktreeDialogOutcome {
    /// User pressed Enter — create the worktree.
    /// `None` means auto-generate the name.
    Submitted(Option<String>),
    /// User pressed Esc — close without creating.
    Cancelled,
    /// Input changed (redraw needed).
    Changed,
    /// Nothing happened.
    Unchanged,
}
/// Persisted worktree preference for `/new` and `/fork`.
///
/// Controls whether the worktree question popup is shown when starting a
/// new session or forking. Each command has its own config key:
/// - `[hints] new_session_worktree_mode` (default: `ask`)
/// - `[hints] fork_worktree_mode` (default: `ask`)
///
/// The legacy `[hints] worktree_mode` key is read as a fallback when
/// neither per-command key is set.
///
/// Startup resolution lives in
/// [`echo_agent_runtime::util::config::resolve_hints`]; this type is the pager's
/// in-memory mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeMode {
    /// Always show the popup.
    Ask,
    /// Always create a worktree, skip the popup.
    Always,
    /// Never create a worktree, skip the popup.
    Never,
}
impl From<echo_agent_runtime::util::config::WorktreeHintMode> for WorktreeMode {
    fn from(mode: echo_agent_runtime::util::config::WorktreeHintMode) -> Self {
        use echo_agent_runtime::util::config::WorktreeHintMode;
        match mode {
            WorktreeHintMode::Ask => Self::Ask,
            WorktreeHintMode::Always => Self::Always,
            WorktreeHintMode::Never => Self::Never,
        }
    }
}
impl WorktreeMode {
    /// Parse from a TOML string value. Unrecognised values fall back to
    /// [`WorktreeMode::Never`] with a debug-level log.
    pub fn from_config_str(s: &str) -> Self {
        echo_agent_runtime::util::config::WorktreeHintMode::from_config_str(s).into()
    }
    /// Serialise to the TOML string representation.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
    /// Resolve per-command worktree modes from a parsed TOML document.
    ///
    /// Returns `(new_session_worktree_mode, fork_worktree_mode)`.
    ///
    /// Resolution order:
    /// - `/new`: `new_session_worktree_mode` key, else legacy `worktree_mode`, else `Never` (no popup).
    /// - `/fork`: `fork_worktree_mode` key, else legacy `worktree_mode`, else `Ask`.
    pub fn resolve_from_hints(hints: Option<&toml_edit::Item>) -> (Self, Self) {
        let get_str = |key: &str| -> Option<Self> {
            hints
                .and_then(|h| h.get(key))
                .and_then(|v| v.as_str())
                .map(Self::from_config_str)
        };
        Self::resolve_from_hint_strings(get_str)
    }
    /// Same as [`Self::resolve_from_hints`], for merged effective config (`toml::Value`).
    pub fn resolve_from_hints_value(hints: Option<&toml::Value>) -> (Self, Self) {
        let (new_session, fork) =
            echo_agent_runtime::util::config::WorktreeHintMode::resolve_pair(hints);
        (new_session.into(), fork.into())
    }
    fn resolve_from_hint_strings(get_str: impl Fn(&str) -> Option<Self>) -> (Self, Self) {
        let legacy = get_str("worktree_mode");
        let new_session = get_str("new_session_worktree_mode")
            .or(legacy)
            .unwrap_or(Self::Never);
        let fork = get_str("fork_worktree_mode")
            .or(legacy)
            .unwrap_or(Self::Ask);
        (new_session, fork)
    }
}
use super::PagerTerminal;
use super::actions::Action;
use super::agent::AgentId;
use super::agent_view::{AgentView, AppRenderParams, McpInitProgress};
use super::bundle::BundleState;
/// Which view is currently displayed.
///
/// Note: `AgentDashboard` does not carry state directly because
/// `DashboardState` is not `Copy`. The dashboard view-state lives on
/// `AppView::dashboard` and is only "active" when `active_view == AgentDashboard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Welcome,
    Agent(AgentId),
    /// The top-level Agent Dashboard. State lives in `AppView::dashboard`.
    AgentDashboard,
}
impl ActiveView {
    /// The agent on screen, or `None` for a view that shows no single agent.
    pub fn agent_id(self) -> Option<AgentId> {
        match self {
            ActiveView::Agent(id) => Some(id),
            ActiveView::Welcome | ActiveView::AgentDashboard => None,
        }
    }
}
/// Target restored when leaving the dashboard (Ctrl+\ / Esc).
/// Consumed by `dispatch_exit_dashboard`; dead agents fall back to
/// insertion-order first / Welcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardReturn {
    /// Plain agent view (no session-overlay chrome).
    Agent(AgentId),
    /// Session overlay: re-set `attached_agent` on the way back.
    Overlay(AgentId),
}
impl DashboardReturn {
    pub fn agent_id(self) -> AgentId {
        match self {
            Self::Agent(id) | Self::Overlay(id) => id,
        }
    }
    pub fn is_overlay(self) -> bool {
        matches!(self, Self::Overlay(_))
    }
}
/// Tick cadence demanded by the current view state — see
/// [`AppView::tick_demand`]. Ordered: `None < Slow < Fast`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TickDemand {
    /// Nothing animates or polls: the event loop parks (zero wakeups).
    None,
    /// Only low-frequency work is pending (welcome logo shimmer at ~12fps,
    /// the macOS Cmd link-hover poll): tick at [`SLOW_TICK_INTERVAL`].
    Slow,
    /// Real animation is on screen: tick at the configured animation fps.
    Fast,
}
/// Tick cadence for [`TickDemand::Slow`] (~12fps). Matches the welcome logo's
/// `SHIMMER_FPS` so slow ticks sample every shimmer frame, and bounds the
/// latency of the macOS Cmd link-hover underline.
pub const SLOW_TICK_INTERVAL: Duration = Duration::from_millis(83);
/// Welcome toast lifetime (wall clock, so the duration holds whether the
/// event loop is ticking Slow or Fast).
const WELCOME_TOAST_DURATION: Duration = Duration::from_secs(2);
fn reconnect_success_hides_mismatch(current: Option<&str>, incoming: &str) -> bool {
    current.is_some_and(crate::acp::is_version_mismatch_banner)
        && (incoming.starts_with("Reconnected.") || incoming.starts_with("Session restored."))
}
/// Which prompt box in-flight voice dictation appends its finalized text to.
/// Captured when recording **starts** so a trailing STT final still lands where
/// the user was dictating, even if they navigate away — or toggle a dashboard
/// row's peek panel — mid-utterance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceTarget {
    /// A live agent session's prompt box.
    Agent(AgentId),
    /// The dashboard's new-agent dispatch input (no row peek was open at start).
    DashboardDispatch,
    /// The dashboard's peek reply input, bound to the agent whose peek was open
    /// at start. The id pins the row: selecting a different row mid-utterance
    /// stops capture (the reply widget is shared and clears on row change), so a
    /// final can't land on the wrong agent's reply.
    DashboardPeekReply(AgentId),
}
/// The voice-dictation lifecycle. Exactly one state holds at a time, so the
/// "is the mic live / is a start queued / does a Ctrl+Space hold own it / which
/// box receives finals" facts can never disagree (they once lived as separate
/// booleans and repeatedly drifted apart). All transitions go through the
/// `AppView::voice_*` methods.
///
/// `hold` marks a session begun by a Ctrl+Space hold-press: its matching
/// Ctrl+Space release ends it (and only it), while `/voice` / toggle sessions
/// leave `hold` false so a Ctrl+Space release can't touch them. `target` is the
/// prompt box bound at
/// **start**, so a trailing final lands where the user was dictating. `interim`
/// (the live partial transcript) lives inside the recording states so it can't
/// linger as a stale overlay once dictation ends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum VoiceState {
    /// No dictation in flight.
    #[default]
    Idle,
    /// A start was requested before the lazy pipeline existed; the event loop
    /// spawns it once and then opens the mic.
    ColdStart { hold: bool, target: VoiceTarget },
    /// Mic is open and streaming audio to STT.
    Recording {
        hold: bool,
        target: VoiceTarget,
        interim: Option<String>,
    },
    /// Capture was explicitly stopped (Esc / Ctrl+Space / [stop] / Ctrl+Space
    /// release), but the target — and the last interim — are kept so a trailing
    /// STT final still lands without the overlay flickering in the meantime.
    Stopping {
        target: VoiceTarget,
        interim: Option<String>,
    },
}
impl VoiceState {
    /// Mic is live (the `Recording` state).
    pub fn listening(&self) -> bool {
        matches!(self, Self::Recording { .. })
    }
    /// A start is queued for the lazy pipeline (the `ColdStart` state).
    pub fn pending_cold_start(&self) -> bool {
        matches!(self, Self::ColdStart { .. })
    }
    /// The prompt box that owns this session's dictation, if any.
    pub fn target(&self) -> Option<VoiceTarget> {
        match self {
            Self::ColdStart { target, .. }
            | Self::Recording { target, .. }
            | Self::Stopping { target, .. } => Some(*target),
            Self::Idle => None,
        }
    }
    /// The live partial transcript shown in the prompt overlay, if any.
    pub fn interim(&self) -> Option<&str> {
        match self {
            Self::Recording { interim, .. } | Self::Stopping { interim, .. } => interim.as_deref(),
            _ => None,
        }
    }
    /// Whether a hold-press owns the current session (so its key release ends
    /// it). `/voice` and toggle-style starts leave this false.
    pub(crate) fn hold(&self) -> bool {
        matches!(self, Self::ColdStart { hold, .. } | Self::Recording { hold, .. } if *hold)
    }
}
/// Entry from the session list wire: welcome/resume pickers and non-leader
/// dashboard roster fallback (`session_picker_entry_to_roster`).
#[derive(Debug, Clone)]
pub struct SessionPickerEntry {
    pub id: String,
    pub summary: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub cwd: String,
    pub hostname: Option<String>,
    pub source: String,
    pub model_id: Option<String>,
    pub num_messages: usize,
    /// When the session last had content added (most recent of local and remote).
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Git branch associated with the session (if available from the server response).
    pub branch: Option<String>,
    /// Repo display name derived from the CWD path (last 2 path components joined by `-`).
    pub repo_name: String,
    /// Human-readable worktree label (if the session was created in a named worktree).
    pub worktree_label: Option<String>,
    /// Per-turn secondary line (`lastTurnSummary` on the session/list wire).
    /// Shown as the "Last turn" line on the expanded resume card and used for
    /// non-leader dashboard roster rows.
    pub last_turn_summary: Option<String>,
    /// Latest session recap (`lastRecap` on the session/list wire), shown on the
    /// expanded resume card whenever available. Distinct from `last_turn_summary`.
    pub last_recap: Option<String>,
    /// Lazy-loaded detail for the expanded card view.
    pub card_detail: Option<CardDetail>,
}
/// Detail loaded on-demand when a session card is expanded.
#[derive(Debug, Clone)]
pub struct CardDetail {
    pub turn_count: usize,
    pub tool_call_count: usize,
    pub first_prompt_preview: String,
}
/// Authentication state for the welcome screen.
///
/// Drives the login flow UI and input routing on the welcome screen.
#[derive(Debug)]
pub enum AuthState {
    /// No login required (API key, cached token, or already authenticated).
    Done,
    /// Login required -- show login menu on welcome screen.
    /// `error` is set after a failed auth attempt so the user sees what went wrong.
    Pending { error: Option<String> },
    /// Auth flow is in progress.
    Authenticating {
        /// Sequence number for this auth attempt (stale results are ignored).
        request_seq: u64,
        /// Abort handle for the in-flight Authenticate task.
        handle: Option<tokio::task::AbortHandle>,
        /// Auth URL from the provider (populated by AuthUrlReady).
        auth_url: Option<String>,
        /// How the auth flow presents itself to the user.
        mode: AuthMode,
    },
}
/// How the auth flow presents itself to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Mode not yet determined (waiting for auth URL response).
    Pending,
    /// Browser opened automatically by external provider.
    Command,
    /// Manual: user must visit URL and paste token.
    Loopback,
    /// RFC 8628 device flow: device code + copyable URL, no paste box.
    Device,
}
/// Folder-trust state for the welcome screen.
///
/// Mirrors [`AuthState`]: a welcome sub-state that drives the "Do you trust the
/// contents of this directory?" question and gates session creation until it is
/// answered. Seeded once before the first render from the pure
/// [`echo_agent_workspace::folder_trust::decide`] verdict; when the feature flag
/// is off `decide` returns trusted, so this is always [`TrustState::Done`].
#[derive(Debug)]
pub enum TrustState {
    /// No question needed (feature off, already trusted, nothing to gate) or
    /// the question has been answered. Session creation may proceed.
    Done,
    /// An untrusted folder with repo-local code-exec config: show the trust
    /// question and defer session creation until the user answers.
    Pending {
        /// The resolved workspace root (git root) that is trusted on accept and
        /// is shown in the question.
        workspace: std::path::PathBuf,
    },
}
/// Result of `handle_input`. Tells the event loop what to do next.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum InputOutcome {
    /// Dispatch this action, then redraw.
    Action(Action),
    /// Dispatch this action, then re-process the same event through the
    /// (now-changed) active view. The event loop batches both dispatches into
    /// one effect wave so state from the forward pass may shape the first
    /// action's meta (e.g. welcome create + CycleMode sharing session/new flags).
    ActionThenForward(Action),
    /// Dispatch+process the first action, then dispatch+process the second
    /// (intentional effect barrier between them; e.g. revert preview then open reset).
    ActionPair(Action, Action),
    /// Arm a double-press pending action (e.g. idle Esc clear/rewind).
    /// AppView installs [`PendingAction`]; second press within `ttl` fires
    /// `action`. `label: None` arms silently (no shortcuts-bar hint).
    ArmPending {
        action: Action,
        shortcut: KeyShortcut,
        label: Option<&'static str>,
        ttl: Duration,
    },
    /// Something changed visually (prompt text, scroll). Redraw needed.
    Changed,
    /// Nothing happened. Skip redraw to preserve cursor blink.
    Unchanged,
}
/// Immutable origin carried beside a paste event until its target consumes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteProvenance {
    /// Terminal bracketed paste or pager key-coalesced paste.
    Terminal,
    /// Linux X11 PRIMARY read triggered by one unmodified middle-button down.
    #[allow(dead_code)]
    X11Primary,
}
impl PasteProvenance {
    pub(crate) fn may_probe_clipboard_attachments(self) -> bool {
        matches!(self, Self::Terminal)
    }
}
/// A pending action awaiting double-press confirmation.
///
/// Set when a `requires_confirmation` action is triggered. The shortcuts bar
/// shows "press again to {label}" when [`Self::label`] is `Some`. If the same
/// key is pressed within the TTL, the action fires. Any other key or expiry
/// clears it. `label: None` = silent arm (idle-empty Esc→rewind).
pub struct PendingAction {
    /// The action to fire on second press.
    pub action: Action,
    /// The specific key that was pressed (narrowed from the binding).
    pub shortcut: KeyShortcut,
    /// When `Some`, shortcuts bar shows "press again to {label}".
    pub label: Option<&'static str>,
    /// When this pending action expires.
    pub expires_at: Instant,
}
impl PendingAction {
    pub const TTL: Duration = Duration::from_millis(1000);
    /// Double-press timeout for idle Esc clear / rewind arms.
    pub const ESC_DOUBLE_PRESS_TTL: Duration = Duration::from_millis(800);
    pub fn new(action: Action, shortcut: KeyShortcut, label: &'static str) -> Self {
        Self::with_ttl(action, shortcut, Some(label), Self::TTL)
    }
    /// Like [`Self::new`] but with an explicit confirm window. Used by
    /// the dashboard-overlay stop (Ctrl+X), which mirrors the
    /// dashboard's [`crate::views::dashboard::state::CONFIRM_WINDOW`]
    /// rather than the default double-press TTL.
    pub fn with_ttl(
        action: Action,
        shortcut: KeyShortcut,
        label: Option<&'static str>,
        ttl: Duration,
    ) -> Self {
        Self {
            action,
            shortcut,
            label,
            expires_at: Instant::now() + ttl,
        }
    }
    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}
/// Cap for the `ECHO_AGENT_ESC_DOUBLE_PRESS_MS` override; the pty_e2e suite sets
/// exactly this value.
pub const ESC_DOUBLE_PRESS_TEST_MS: u64 = 60_000;
/// Idle-Esc double-press confirm window, `ECHO_AGENT_ESC_DOUBLE_PRESS_MS`-overridable
/// (read once, bounded). Test seam: a loaded pty_e2e shard's render round-trip
/// between the two presses can outlast the 800ms default and expire the arm.
pub(crate) fn esc_double_press_ttl() -> Duration {
    use std::sync::OnceLock;
    static TTL: OnceLock<Duration> = OnceLock::new();
    *TTL.get_or_init(|| parse_esc_ttl(std::env::var("ECHO_AGENT_ESC_DOUBLE_PRESS_MS").ok()))
}
/// Extracted pure (no `OnceLock`) so the bounds — zero/garbage → default,
/// oversized → clamp — are unit-testable.
fn parse_esc_ttl(raw: Option<String>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(|ms| Duration::from_millis(ms.min(ESC_DOUBLE_PRESS_TEST_MS)))
        .unwrap_or(PendingAction::ESC_DOUBLE_PRESS_TTL)
}
/// Slash commands unavailable on the free and X Basic subscription tiers.
///
/// To restrict another command for these tiers, add its canonical name
/// (no leading `/`) here — matching covers aliases automatically via
/// [`crate::slash::registry::CommandRegistry::set_restricted_commands`].
///
/// Current set:
/// - `usage` — coding credit / billing UI (alias: `/cost`)
/// - `imagine` — image generation entry point
/// - `imagine-video` — video generation entry point
/// - `voice` — voice dictation entry point (the Ctrl+Space / F8 keybinding is
///   gated separately in [`crate::app::dispatch::voice`], since it bypasses the
///   slash registry)
pub(crate) const TIER_RESTRICTED_COMMANDS: &[&str] =
    &["usage", "imagine", "imagine-video", "voice"];
/// Whether a subscription-tier display name is a tier with restricted
/// commands: the free tier (no subscription ⇒ `None`, or an explicit
/// "Free") and X Basic (CCP display name "X Basic"; JWT claim fallback
/// "x_basic"). Everything else — paid tiers and unknown future names —
/// is unrestricted (fail-open).
///
/// The string classification is shared with the shell's capability
/// (toolset) gate via [`echo_agent_runtime::tier::is_restricted_tier_name`] so
/// the two can't drift. The pager's *cosmetic* slash-command gate treats an
/// absent tier (`None`) as restricted (it recovers live on the next settings
/// update); the shell's capability gate treats absence as unrestricted.
fn is_restricted_tier(tier: Option<&str>) -> bool {
    match tier {
        None => true,
        Some(t) => echo_agent_runtime::tier::is_restricted_tier_name(t),
    }
}
/// True for API-key labels from shell/CCP: `"ApiKey"`, `"API Key"`, `"api_key"`.
pub(crate) fn is_api_key_label(s: &str) -> bool {
    s.trim().to_ascii_lowercase().replace([' ', '_', '-'], "") == "apikey"
}
/// Pending re-exec into another screen mode (see `/minimal` / `/fullscreen`).
#[derive(Debug, Clone)]
pub struct ScreenModeRelaunch {
    /// `true` → `--minimal`; `false` → fullscreen (non-minimal).
    pub minimal: bool,
    /// Active session to reopen via `--resume`.
    pub session_id: String,
}
/// A consented `/feedback` trace upload deferred until the coding-data
/// sharing opt-in write claimed at `seq` resolves.
#[derive(Debug, Clone)]
pub struct PendingFeedbackTraceUpload {
    /// The `coding_data_write_seq` generation this upload waits on.
    pub seq: u64,
    pub agent_id: AgentId,
    pub session_id: acp::SessionId,
}
/// Root view component — owns all application state.
pub struct AppView {
    /// Taken by whichever path reaches a usable session (or interactive idle) first.
    pub pending_startup: Option<echo_agent_telemetry::startup::PendingStartup>,
    /// Which view is currently active.
    pub active_view: ActiveView,
    /// View to return to after a mid-session login flow completes or is
    /// cancelled. `Some` only while a `/login` (or 401-triggered re-auth)
    /// initiated from an active session is in progress — it lets the auth
    /// UI take over the `Welcome` screen and then restore the caller's view
    /// (e.g. `Agent`) afterwards. `None` at startup so the normal
    /// login-then-load flow is preserved.
    pub auth_return_view: Option<ActiveView>,
    /// Per-agent views (keyed by AgentId).
    pub agents: IndexMap<AgentId, AgentView>,
    /// Monotonically increasing counter for agent ID allocation.
    /// Never reuse IDs after `shift_remove` to avoid collisions.
    pub next_agent_id: usize,
    /// Available/selected models (shared across agents).
    pub models: ModelState,
    /// Keybinding definitions.
    pub registry: ActionRegistry,
    /// Settings registry — canonical metadata for user-tunable preferences.
    pub settings_registry: Arc<crate::settings::SettingsRegistry>,
    /// In-memory snapshot of the effective `UiConfig`. Seeded once at
    /// startup; updated synchronously by `set_X_inner` so dispatch
    /// stays sans-IO.
    pub current_ui: echo_agent_runtime::agent::config::UiConfig,
    /// Working directory.
    pub cwd: PathBuf,
    /// Whether the cwd is inside a git repository (any ancestor has `.git`).
    /// Pre-computed at startup so dispatch stays free of filesystem I/O.
    pub cwd_has_git_ancestor: bool,
    /// ACP channel for sending requests (shared resource, cloned into agents).
    pub acp_tx: AcpAgentTx,
    /// Local cache of bundle sync/status state from the shell.
    pub(crate) bundle_state: BundleState,
    /// Reusable scratch buffer for rendering.
    pub scratch: ScratchBuffer,
    /// Cursor state for blink-preserving cursor management.
    /// See [`crate::render::draw`] for the full rationale.
    pub cursor: CursorState,
    /// Pending double-press confirmation (quit, etc.).
    pub pending_action: Option<PendingAction>,
    /// Pending exit-session confirmation for slash command path.
    /// Set when `/home` is first typed; confirmed on second invocation within TTL.
    pub exit_session_pending: Option<Instant>,
    /// Mouse scroll normalization state (wheel/trackpad detection, acceleration).
    /// App-level because scroll is a physical input property, not per-agent.
    pub scroll_state: MouseScrollState,
    /// Scroll config derived from terminal detection.
    pub scroll_config: ScrollConfig,
    /// Current appearance config (hot-reloadable from ~/.echo-agent/pager.toml).
    /// Stored here so new agents inherit the current config.
    pub appearance: AppearanceConfig,
    /// Notification service (terminal bell, OSC sequences, title updates).
    pub notification_service: NotificationService,
    /// The status row follows whichever agent is on screen, so the app owns it.
    pub(crate) status_line: crate::app::status_line::StatusLineState,
    /// Escape sequences (title, progress bar) accumulated by the last
    /// `update_notifications()` tick. Consumed by `draw()` and appended
    /// to the frame's `post_flush_escapes` so they are written inside the
    /// synchronized output block.
    pub(crate) pending_notification_escapes: Option<String>,
    /// Notification deferred by several ticks so the terminal has time to
    /// process the idle title escape before the notification fires.
    ///
    /// The idle title goes through the frame pipeline (writer thread
    /// channel), then Ghostty must read it from the PTY and apply it.
    /// Ghostty debounces `setTitle()` by 75 ms, so we need >75 ms
    /// before the notification reads `self.title` for the subtitle.
    /// 3 ticks × 33 ms ≈ 99 ms covers the debounce comfortably.
    ///
    /// The `u8` counts remaining ticks; the notification fires when it
    /// reaches 0.
    pub(crate) deferred_notification: Option<(crate::notifications::NotificationEvent, u8)>,
    /// Tracing log channel receiver. Set by the event loop after
    /// `init_tracing()`. Drained into `tracing_pane` each tick in debug/dev
    /// builds; otherwise drained-and-discarded.
    pub tracing_rx: Option<crate::tracing::LogRx>,
    /// Scroll-diagnostics HUD (`ECHO_AGENT_SCROLL_DEBUG` env / `/scroll-debug`).
    /// Release-compiled behind its runtime gate — see the module doc.
    pub scroll_debug_hud: crate::views::scroll_debug_hud::ScrollDebugHud,
    /// Release-safe FPS HUD (`/debug fps`; `ECHO_AGENT_FPS` env on release
    /// builds, where the dev overlay is compiled out) — see the module doc.
    pub fps_hud: crate::views::fps_hud::FpsHud,
    pub active_announcements: Vec<echo_agent_announcements::RemoteAnnouncement>,
    /// Persisted hide keys, filtered at the banner selection gate — hiding one
    /// critical reveals the next unhidden one, and a NEW id re-arms the banner.
    pub hidden_announcement_ids: std::collections::BTreeSet<String>,
    pub announcements_last_gen: u64,
    /// Selected welcome announcement for this pager launch.
    pub announcement: Option<echo_agent_announcements::RemoteAnnouncement>,
    /// Cached changelog markdown (for `/release-notes`). Populated by
    /// `FetchChangelog` at startup; `None` until the fetch completes.
    pub changelog_markdown: Option<String>,
    /// Cached changelog bullets (for welcome screen). Populated by
    /// `FetchChangelog` at startup; empty until the fetch completes.
    pub changelog_bullets: Vec<String>,
    /// Resolved tip list from config layers.
    pub tips: Vec<String>,
    /// Selected tip for the current launch/session.
    pub tip: Option<String>,
    /// Whether to show the resolved model ID in /session-info output.
    pub show_resolved_model: bool,
    /// Whether the `/share` slash command is available. Currently forced off
    /// while session share links are temporarily disabled in clients.
    pub sharing_enabled: bool,
    /// Whether the plugin marketplace CTA is enabled. Env `ECHO_AGENT_PLUGIN_CTA`
    /// overrides `RemoteSettings.plugin_cta` (remote settings); defaults to `false`.
    pub plugin_cta_enabled: bool,
    /// Marketplace source name the plugin CTA draws candidates from, when
    /// `[marketplace].plugin_cta_marketplace` is set in the effective config.
    /// `None` keeps the default EchoAgent Official source.
    pub plugin_cta_marketplace: Option<String>,
    pub workspace_dashboard_enabled: bool,
    /// Consumer billing surface (credit fetches / warnings). False for team
    /// and API-key auth. `/usage` itself stays available for session token/cost
    /// unless [`Self::has_external_auth_provider`].
    pub usage_visible: bool,
    /// External `auth_provider_command` deployment.
    /// No cloud.echo-agent.invalid billing session exists; `/usage` and credit UI stay off.
    pub has_external_auth_provider: bool,
    /// Slash commands denied for the current subscription tier
    /// ([`TIER_RESTRICTED_COMMANDS`] when the user is on the free / X Basic
    /// tier, empty otherwise). Recomputed by [`Self::apply_tier_restrictions`]
    /// and fanned out to every slash registry (welcome prompt, agents,
    /// dashboard); deny wins over all other visibility gates.
    pub tier_restricted_commands: Vec<String>,
    /// Whether the pager is connected via a leader (leader mode). The Agent
    /// Dashboard entry points (`/dashboard`, `Ctrl+\`, `echoagent dashboard`, the
    /// startup hook) are only meaningful when a leader is coordinating a
    /// fleet of sessions, so they are gated on this flag. Set in
    /// `event_loop::run` from `connection.leader_status_rx.is_some()`;
    /// defaults to `false` (non-leader, dashboard hidden).
    pub leader_mode: bool,
    /// App-level credit balance used to show the usage warning on the
    /// welcome screen before any agent session exists.
    pub credit_balance: Option<crate::views::credit_bar::CreditBalance>,
    /// App-level auto top-up rule paired with `credit_balance` for the warning.
    pub auto_topup: Option<crate::views::credit_bar::AutoTopupInfo>,
    /// Periodic billing poll requested (credits >= 99%).
    pub billing_poll_wanted: bool,
    /// Leader-mode session roster (FleetView dashboard). Populated from
    /// `echo.agent/sessions/list` polls and `echo.agent/sessions/changed` broadcasts.
    /// Empty in non-leader mode, which naturally gates roster rendering.
    pub leader_roster: Vec<crate::app::roster::RosterEntry>,
    /// Local on-disk session list (dormant/idle sessions) surfaced on the
    /// dashboard when NOT in leader mode. There is no live leader roster to
    /// poll outside leader mode, so we fetch the same `echo.agent/session/list` the
    /// resume picker uses and render those as idle rows. Entries are stored as
    /// [`crate::app::roster::RosterEntry`] (activity `Dormant`) so they reuse
    /// the existing roster-row rendering / attach path. Empty in leader mode.
    pub dashboard_local_sessions: Vec<crate::app::roster::RosterEntry>,
    /// Whether the dashboard is currently loading local sessions (non-leader mode).
    pub dashboard_sessions_loading: bool,
    /// Server-authoritative shared prompt queues, keyed by `sessionId`
    /// Reconciled from `echo.agent/queue/changed` broadcasts so
    /// every client renders the same ordered queue (including prompts queued
    /// by other clients). Empty in non-leader mode.
    pub shared_prompt_queues:
        std::collections::HashMap<String, Vec<crate::app::prompt_queue::QueueEntryWire>>,
    /// Optimistic echo rows for prompts the pager sent server-authoritatively
    /// (plain prompt typed while a turn is running) but for which the
    /// confirming `echo.agent/queue/changed` broadcast has not yet arrived. Keyed by
    /// `sessionId`. Pinned into `shared_prompt_queues` on reconcile so the row
    /// doesn't flicker, and dropped once the authoritative broadcast reflects
    /// the id (or it starts running). Never persisted.
    pub optimistic_prompt_echoes:
        std::collections::HashMap<String, Vec<crate::app::prompt_queue::QueueEntryWire>>,
    /// Server-authoritative running prompts that drained into the running slot
    /// while the previous turn was still finishing locally (handoff race).
    /// Keyed by `AgentId`. Consumed by the `PromptResponse` handler after
    /// `finish_turn` clears `current_prompt_id`, which then adopts the prompt
    /// and runs the turn-start shim. Never persisted.
    pub(crate) pending_running_adoptions:
        std::collections::HashMap<AgentId, crate::app::acp_handler::PendingRunningAdoption>,
    /// Whether the session picker groups entries by repo name with
    /// non-selectable headers. Gated by `ECHO_AGENT_SESSION_PICKER_GROUPED` env var
    /// or remote settings `session_picker_grouped`; defaults to `false`.
    pub session_picker_grouped: bool,
    /// Startup-only seed for `AgentView::scheduler_background_loops`, resolved
    /// once from the config layers plus the remote tier known at connect.
    /// Read only until a session's own value arrives on its `session/new` /
    /// `session/load` response, and by the session-less dashboard. Never
    /// refreshed afterwards — the authoritative value is per session, pinned by
    /// the shell when that session's actor spawned.
    pub scheduler_background_loops_seed: bool,
    /// Whether Ctrl+C before first server activity rewinds the prompt
    /// back into the input box. Gated by `ECHO_AGENT_CANCEL_REWIND` env /
    /// `[features] cancel_rewind` config / remote settings flag.
    pub cancel_rewind_enabled: bool,
    /// Whether session recap (`/recap` + automatic away recap) is rolled out,
    /// resolved by the shell and advertised on ACP initialize (`sessionRecap`).
    /// When false, the pager must not request recaps (zero `echo.agent/recap` traffic).
    pub session_recap_available: bool,
    /// Shell-advertised eligibility for the `/feedback` trace-upload offer,
    /// exactly as received (initialize meta / auth-meta refreshes). Read it
    /// through [`Self::feedback_trace_offer`], which subtracts the latch.
    pub shell_feedback_trace_offer: bool,
    /// A persisted card answer was made this session; keeps auth-meta
    /// refreshes from re-offering before the async config write lands.
    pub feedback_trace_choice_latched: bool,
    /// Trace upload parked until the same card answer's sharing opt-in write
    /// confirms (the storage proxy rejects uploads while opted out).
    pub feedback_trace_upload_pending: Option<PendingFeedbackTraceUpload>,
    /// Stateful prompt widget rendered on the welcome screen (persists input across frames).
    pub welcome_prompt: PromptWidget,
    /// The single slash-command MRU/recency store. Owned here and injected
    /// into every agent prompt and the dashboard dispatch via
    /// [`PromptWidget::adopt_slash_mru`] so command recency is shared across
    /// surfaces (single-threaded UI; no process-global singleton).
    pub(crate) slash_mru: std::rc::Rc<std::cell::RefCell<crate::slash::mru::SlashMru>>,
    /// The single resolved per-command tag map (canonical name → free-form tag).
    /// Owned here and injected into every agent prompt and the dashboard dispatch
    /// via [`PromptWidget::adopt_command_tags`] so slash-dropdown tags are shared
    /// across surfaces. Populated from remote settings + local config; updated
    /// in place so adopters see refreshes without re-adopting.
    pub(crate) command_tags:
        std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, String>>>,
    /// Whether the welcome screen prompt is currently capturing focus (user typed in it).
    /// When true, menu shortcuts like n/w/q are disabled and Escape unfocuses the prompt.
    pub welcome_prompt_focused: bool,
    /// Sticky flag: set once the user types in the welcome prompt, hides the
    /// tip for the rest of the session (even if the input is cleared).
    pub welcome_tip_typing_dismissed: bool,
    /// Effects queued by notification handlers (drained by the event loop).
    pub pending_effects: Vec<crate::app::actions::Effect>,
    /// Typed `$EDITOR` work consumed by the event loop after the current cycle.
    /// Both configuration-file and prompt-draft edits share the existing
    /// leave-raw-mode / child / restore handoff.
    pub(crate) pending_editor: Option<crate::app::external_editor::PendingEditorRequest>,
    /// Path to open in `$PAGER` (default `less`) after the current event cycle.
    /// Set by `Action::OpenTranscriptPager` (`/transcript`); consumed by the
    /// event loop which suspends the inline TUI, spawns the pager, then restores
    /// and deletes the temp file. Primarily for minimal mode (no interactive
    /// scrollback pane), but works in every mode.
    pub pending_pager_path: Option<std::path::PathBuf>,
    /// Whether [`pending_pager_path`](Self::pending_pager_path) holds an
    /// ANSI-colored file (the minimal "full view" transcript). When true the
    /// event loop ensures the pager renders raw control codes (`less -R`) so the
    /// colors show instead of literal escapes. Plain-text transcripts (`/export`
    /// markdown) leave this false.
    pub pending_pager_ansi: bool,
    /// Minimal mode only: the Ctrl+T **force-show** pin for the todo panel.
    /// Minimal-mode-only per-session state, consolidated into a single field so
    /// the central `AppView` isn't peppered with loose minimal flags. Default-
    /// empty and inert outside `--minimal`; the `echo-agent-pager-minimal` crate
    /// reads/mutates it through the `crate::minimal_api` accessors. See
    /// [`crate::minimal_api::MinimalState`].
    pub(crate) minimal_state: crate::minimal_api::MinimalState,
    /// Currently highlighted menu item on the welcome screen (arrow keys / hover).
    pub welcome_menu_index: Option<usize>,
    /// Hit-test rects for welcome menu items (populated during render).
    pub welcome_menu_rects: Vec<ratatui::layout::Rect>,
    /// Whether the welcome menu currently includes a "Changelog" row (above
    /// Quit). Set during render; the input handler uses it to size the menu and
    /// map the extra row to the release-notes action.
    pub welcome_show_changelog_action: bool,
    /// Hit-test rect for the import-claude banner on the welcome screen.
    pub welcome_import_banner_rect: Option<ratatui::layout::Rect>,
    /// Last known mouse position (column, row), updated on every Mouse event.
    /// Used by the welcome screen to render fine-grained hover effects (e.g.
    /// brighter red on the import row's `[x]` when the mouse is exactly on
    /// those cells).
    pub last_mouse_pos: Option<(u16, u16)>,
    /// Origin (column, row) of the in-progress scroll gesture. Reused by
    /// `update_tick`'s residual flush so sub-line carry / stream-gap flushes
    /// route via `hit_test` to the originating pane instead of leaking into
    /// scrollback. Cleared on the finalize-transition tick.
    pub last_scroll_pos: Option<(u16, u16)>,
    /// Last off-screen render-cache eviction sweep (see
    /// [`Self::maybe_evict_offscreen_caches`]).
    pub(super) last_cache_evict_at: Option<Instant>,
    /// Hit-test rect for welcome prompt input (populated during render).
    pub welcome_prompt_rect: Option<ratatui::layout::Rect>,
    /// Hit-test rect for the auth URL (click-to-open during Authenticating).
    pub welcome_auth_url_rect: Option<ratatui::layout::Rect>,
    /// Whether the mouse pointer was last over the auth URL (for OSC 22 cursor shape).
    pub welcome_on_auth_url: bool,
    /// Mouse last over the changelog block (drives hover color + redraws).
    pub welcome_on_changelog_cta: bool,
    /// Per-visit announcement UI state on the welcome screen (expansion, hover,
    /// overflow flag, hit-rect).
    pub welcome_announcement: WelcomeAnnouncementState,
    /// Hit-test rect for the "show full URL" fallback link.
    pub welcome_auth_fallback_rect: Option<ratatui::layout::Rect>,
    /// Hit-test rect for the "[Refresh]" button on the paywall tier line.
    pub welcome_refresh_rect: Option<ratatui::layout::Rect>,
    /// Hit-test rect for the gate URL link on the paywall CTA.
    pub welcome_gate_url_rect: Option<ratatui::layout::Rect>,
    /// Rewritten by every welcome frame, so a resize leaves no stale click target.
    pub welcome_consent_link_rects: Vec<(usize, ratatui::layout::Rect)>,
    /// Consent link the mouse is over, so every run of a wrapped link brightens together.
    pub welcome_consent_hover_link: Option<usize>,
    /// The disk write is a spawned task, so a settings refresh that lands first would otherwise
    /// re-arm a notice the user has already accepted.
    pub consent_answered: Option<(String, i32)>,
    /// Hit-test rect for the welcome hero upgrade CTA `[label]` button
    /// (click → `AnnouncementsOpenCta(Welcome)`).
    pub welcome_upgrade_cta_rect: Option<ratatui::layout::Rect>,
    pub welcome_privacy_banner_opt_in_rect: Option<ratatui::layout::Rect>,
    pub welcome_privacy_banner_opt_out_rect: Option<ratatui::layout::Rect>,
    pub welcome_privacy_banner_terms_rect: Option<ratatui::layout::Rect>,
    pub welcome_privacy_banner_policy_rect: Option<ratatui::layout::Rect>,
    /// Hit-test rects for the welcome workspace-mode picker.
    #[cfg(feature = "local-workspace")]
    pub welcome_workspace_mode_rects: crate::views::welcome::WorkspaceModeHitRects,
    /// Sticky hover flag for the workspace-mode picker (redraw on enter/leave).
    #[cfg(feature = "local-workspace")]
    pub welcome_on_workspace_mode: bool,
    /// Transient welcome toast: (message, wall-clock expiry).
    pub welcome_toast: Option<(String, std::time::Instant)>,
    /// Sticky hover flag for the privacy banner buttons (redraw on enter/leave).
    pub welcome_on_privacy_banner: bool,
    /// Sticky hover flag for the welcome upgrade CTA (redraw on enter/leave).
    pub welcome_on_upgrade_cta: bool,
    /// Hit-test rect for the clickable changelog info block (opens release notes).
    pub welcome_changelog_cta_rect: Option<ratatui::layout::Rect>,
    /// Show the raw auth URL with mouse capture disabled for manual copy.
    pub auth_show_raw_url: bool,
    /// We turned capture off for native select and owe a restore on leave.
    pub native_select_hold: bool,
    /// Fetched session list for the session picker (None = not yet fetched).
    pub session_picker_entries: Option<Vec<SessionPickerEntry>>,
    /// Whether the session list is currently being fetched.
    pub session_picker_loading: bool,
    /// Unified picker state for the session picker.
    pub session_picker_state: crate::views::picker::PickerState,
    /// Source filter for the welcome-screen session picker.
    pub session_picker_source_filter: crate::views::session_picker::SourceFilter,
    /// Directory whose relaxed-scope notice has fired, keyed by the browse cwd
    /// (`app.cwd`); a cwd-scoped browse clears it so a later relax re-notifies.
    pub session_picker_relaxed_notified_for: Option<std::path::PathBuf>,
    /// Content-based (deep search) results from ACP session search.
    pub session_picker_content_results:
        Option<Vec<echo_agent_runtime::extensions::session_search::SearchSessionHit>>,
    /// Whether a deep search is currently in flight.
    pub session_picker_content_loading: bool,
    /// Monotonically increasing sequence number for deep search requests.
    pub session_picker_deep_search_seq: u64,
    /// Monotonically increasing sequence number for session list fetches
    /// (`Effect::FetchSessionList`): only the seq-current response is
    /// applied, so a stale completion can't clobber newer results. Bumped
    /// only under chat mode (server-search supersede); in Build mode it
    /// stays 0 so plain list responses keep their pre-existing
    /// last-write-wins behavior.
    pub session_picker_list_seq: u64,
    /// Resolved compat-session cells used before checking resume-skill paths.
    pub(crate) foreign_session_compat: echo_agent_foreign_sessions::EnabledForeignSessionSources,
    /// Monotonic picker scan sequence, bumped on every open and close.
    pub(crate) foreign_session_scan_seq: u64,
    /// Coalesces obsolete foreign scans across welcome and modal pickers.
    pub(crate) foreign_scan_coordinator: crate::app::ForeignScanCoordinator,
    /// Foreign lane completion and deferred native-lane notice.
    pub(crate) session_picker_lanes: crate::views::session_picker::SessionPickerLanes,
    /// Invalidates detail reads when picker rows or filters change.
    pub(crate) session_picker_detail_generation: u64,
    /// The search query `session_picker_entries` were server-fetched with
    /// (`None` = unfiltered fetch). Via
    /// [`crate::views::session_picker::effective_filter_query`], skips the
    /// local fuzzy re-filter for server search results.
    pub session_picker_entries_query: Option<String>,
    pub session_picker_pending_delete: Option<crate::views::session_picker::PendingDelete>,
    /// Tick counter for welcome screen spinner animation.
    pub welcome_tick: u64,
    /// Last shimmer frame drawn on the welcome screen. Lets `tick` throttle the
    /// wall-clock logo animation to a few fps instead of the full tick rate.
    pub welcome_shimmer_frame: u64,
    /// CLI model override (`-m` / `--model`). Seeded into every new
    /// `AgentSession.deferred_model_switch` so the model is applied once
    /// the session is created.
    pub cli_model_override: Option<acp::ModelId>,
    /// CLI effort token (`--reasoning-effort` / `--effort`). Applied on session create.
    pub cli_effort_token: Option<String>,
    /// Default YOLO for new sessions, seeded at startup from `effective_yolo_for_launch`.
    pub default_yolo: bool,
    /// Soft-default still owns the mode: settings/update may rewrite UI +
    /// `default_yolo`. Cleared on user Shift+Tab / settings / CLI claim.
    /// Not inferred from the rendered permission string.
    pub permission_mode_from_soft_default: bool,
    /// Whether the **auto** permission-mode feature gate is enabled (resolved at
    /// startup from env / `[auto_mode] enabled` / remote settings, default OFF). When
    /// `false`, the Shift+Tab cycle skips Auto. See
    /// `echo_agent_runtime::util::config::resolve_auto_permission_mode_enabled`.
    pub auto_mode_gate: bool,
    /// Managed-policy pin (set at startup); gates every runtime always-approve enable.
    pub yolo_policy_block: Option<&'static str>,
    /// One-shot notice that a launch `--yolo` was pinned off; shown on the first agent view.
    pub yolo_launch_block_notice: Option<&'static str>,
    /// One-shot switch-back toast after a screen-mode re-exec.
    pub screen_mode_switch_hint: Option<&'static str>,
    /// Require explicit plan approval via the plan viewer UI even in
    /// always-approve (YOLO) mode. Loaded from `[ui] require_plan_approval`
    /// in config.toml at startup.
    pub require_plan_approval: bool,
    /// Enable plan mode for new sessions (`--plan`).
    /// Adds `enter_plan_mode`, `exit_plan_mode` tools; implies `ask_user`.
    pub plan_mode: bool,
    /// Enable subagent spawning for new sessions (`--subagents`).
    /// Adds the `TaskTool` for spawning subagents.
    pub subagents: bool,
    /// Enable the ask-user-question tool for new sessions (`--ask-user`).
    /// Automatically enabled by `plan_mode`.
    pub ask_user: bool,
    /// Process-wide gateway light-frontend from CLI `--chat` only.
    /// Stamps `_meta["echo.agent/session"].kind = "chat"` and omits Build agent
    /// profiles on create/load while set. `/chat` does **not** set this
    /// (uses [`Self::deferred_startup`] one-shot state instead).
    pub chat_mode: bool,
    /// Welcome picker mode; ignored when `local_workspace_startup_locked`.
    #[cfg(feature = "local-workspace")]
    pub welcome_workspace_mode: crate::views::welcome::WelcomeWorkspaceMode,
    /// CLI/env already stamped local workspace; welcome must not override.
    #[cfg(feature = "local-workspace")]
    pub local_workspace_startup_locked: bool,
    /// One-shot next-session stamp: `Some(None)` sandbox, `Some(cfg)` local.
    #[cfg(feature = "local-workspace")]
    pub welcome_session_local_workspace:
        Option<Option<crate::app::session_startup::LocalWorkspaceConfig>>,
    /// First-run Local ACK still pending in the TUI.
    #[cfg(feature = "local-workspace")]
    pub welcome_local_workspace_ack_pending: bool,
    /// Next welcome history load is local-disk/build (does not set `chat_mode`).
    #[cfg(feature = "local-workspace")]
    pub welcome_history_load_as_build: bool,
    /// Whether mouse capture is currently enabled. Disabled during the
    /// Authenticating state so the terminal handles native text selection.
    pub mouse_captured: bool,
    /// Active "New Worktree" dialog on the welcome screen.
    pub new_worktree_dialog: Option<NewWorktreeDialogState>,
    /// Resolved per-tip gates for the contextual ephemeral hints (undo tip,
    /// plan nudge, clipboard-image tip, send-now tip). Default all ON; resolved
    /// at startup and on settings toggles from `ECHO_AGENT_CONTEXTUAL_HINTS` (master)
    /// > `[ui.contextual_hints]` user config > remote tier > default.
    pub contextual_hints: echo_agent_runtime::util::config::ResolvedContextualHints,
    /// Remote tier for the contextual hints, kept so a settings toggle can
    /// re-resolve the untouched tips against the same remote defaults.
    pub remote_contextual_hints: Option<echo_agent_runtime::util::config::ContextualHintsRemote>,
    /// Per-key seen counts that gate seen-capped ephemeral tips; the single
    /// copy of this state. Passed to `show_ephemeral_tip`, which increments the
    /// matching key in place. In-memory only and per-session — never persisted
    /// to disk, so each pager run starts fresh (count 0).
    pub tip_seen_counts: std::collections::HashMap<&'static str, u32>,
    /// Terminal height (rows) from startup / the last `Event::Resize`. Feeds
    /// the auto-compact derivation (`views::agent::effective_compact`): the
    /// render-value compact flag is forced on while the terminal is
    /// `AUTO_COMPACT_MAX_ROWS` or shorter. 0 = unknown (never forces compact).
    pub last_known_terminal_rows: u16,
    /// One-shot gate for the small-screen `/compact-mode` tip: set after the
    /// first evaluation at a stable agent-view draw (regardless of outcome),
    /// so later resizes can never re-trigger the tip within this run.
    pub small_screen_tip_evaluated: bool,
    /// One-shot gate for the SSH `echoagent wrap` tip: set after the first
    /// evaluation at a stable agent-view draw (the environment gates are
    /// process-constant, so one evaluation decides the run).
    pub ssh_wrap_tip_evaluated: bool,
    /// Focus-scoped, opportunistically-polled clipboard-image tip state: poll
    /// throttle, changeCount delta-detection, fire cooldown, and changeCount
    /// dedup (macOS-only at the probe layer).
    pub clipboard_focus_tip: crate::tips::clipboard_focus::ClipboardFocusTipState,
    /// Persisted worktree preference for `/new`.
    /// Defaults to [`WorktreeMode::Never`] (no popup).
    pub new_session_worktree_mode: WorktreeMode,
    /// Persisted worktree preference for `/fork`.
    /// Defaults to [`WorktreeMode::Ask`] (show popup).
    pub fork_worktree_mode: WorktreeMode,
    /// Restore code state on resume (`--restore-code`).
    pub restore_code: Option<bool>,
    /// One-shot session id: matching `LoadSession` / worktree resume injects
    /// `restore_code: false`, then this clears. Used after conversation-only
    /// remote restore (and remote worktree without `--restore-code`) so agent
    /// `[cli] restore_code` cannot checkout in-place. Not sticky.
    pub suppress_code_restore_once: Option<String>,
    /// Startup resume target that missed local id/title resolution and was
    /// deferred to the worktree resume handler (set from materialization).
    /// Worktree failure messages append the no-match hint only for this
    /// exact target.
    pub resume_local_miss: Option<String>,
    pub agent_override: Option<serde_json::Value>,
    /// ACP-advertised commands seeded into every new `AgentSession` so
    /// autocomplete has shell builtins and skills before any runtime
    /// `AvailableCommandsUpdate` arrives.
    ///
    /// Initially populated from `InitializeResponse.meta.availableCommands`
    /// (AlwaysOn builtins only). Updated whenever the active agent receives
    /// an `AvailableCommandsUpdate` that includes skills, so subsequent
    /// sessions start with the full command catalog immediately.
    pub bootstrap_acp_commands: Vec<agent_client_protocol::AvailableCommand>,
    /// Auth methods from the ACP connection (preserved for re-login after logout).
    pub auth_methods: Vec<acp::AuthMethod>,
    /// Authentication state for the welcome screen login flow.
    pub auth_state: AuthState,
    /// Folder-trust state for the welcome screen. Mirrors [`AppView::auth_state`]:
    /// when `Pending`, the welcome screen shows the trust question and session
    /// creation is deferred (gated after auth) until it is answered.
    pub trust_state: TrustState,
    /// Resolves before folder trust: the account-level answer gates the workspace-level one.
    pub consent_state: crate::app::consent::ConsentState,
    /// Scopes the consent answer, the only identity the pager has for it.
    pub account_email: Option<String>,
    /// Login button label from `AuthMethod.name` (e.g., "cloud.echo-agent.invalid", "Acme Corp").
    pub login_label: Option<String>,
    /// The auth method ID to use for login.
    pub login_method_id: Option<acp::AuthMethodId>,
    /// Initial auth mode hint from method metadata.
    pub auth_start_mode: AuthMode,
    /// Text buffer for manual auth token paste (loopback mode).
    pub(crate) auth_code_input: LineEditor,
    /// Monotonically increasing sequence number for auth requests.
    pub next_auth_request_seq: u64,
    /// Abort handle for the in-flight `PollAuthUrl` task (with its request_seq).
    /// Aborted alongside the Authenticate task in single-flight re-login.
    pub auth_url_poll_handle: Option<(u64, tokio::task::AbortHandle)>,
    /// Every session/chat/worktree/prompt action deferred behind startup gates.
    pub deferred_startup: crate::app::session_startup::DeferredStartupActions,
    /// Whether deferred welcome-screen login should force OAuth.
    pub auth_use_oauth: bool,
    /// Delivery state from the last clipboard copy during auth.
    pub auth_clipboard_delivery: Option<crate::clipboard::ClipboardDelivery>,
    /// Generation of the current auth copy feedback and its clear timer.
    pub auth_clipboard_feedback_generation: u64,
    /// Team principal UUID from auth (`None` for personal sessions).
    pub team_id: Option<String>,
    /// Team name from auth (displayed in the shortcuts bar).
    pub team_name: Option<String>,
    /// Whether the user's team has enterprise Zero Data Retention enabled.
    pub is_zdr: bool,
    /// Team role (e.g. "Admin", "Member", "Read Only") for access-control checks.
    pub team_role: Option<String>,
    /// Whether the user has opted out of coding data retention.
    pub coding_data_retention_opt_out: bool,
    /// Remote settings `privacy_notice_rollout` (cohort on for this user).
    pub privacy_notice_rollout: bool,
    /// Remote `privacy_banner_reshow_days`. None/0 = never re-show after ack.
    pub privacy_banner_reshow_days: Option<u64>,
    /// Local `[privacy].privacy_banner_acked` (RFC 3339 UTC).
    pub privacy_banner_acked: Option<String>,
    /// In-flight opt-in write whose ack waits on ACP success.
    pub privacy_banner_opt_in_inflight: bool,
    /// Newest `SetCodingDataSharing` write. Bumped per dispatch and echoed
    /// on the `TaskResult`, so an older write's late reply — whose
    /// `rollback_to_opted_in` was captured before the newer one — cannot
    /// clobber the current value.
    pub coding_data_write_seq: u64,
    /// Persisted `[cli].show_tips` mirror. `None` = no override (default `true`).
    pub show_tips: Option<bool>,
    /// Persisted `[cli].auto_update` mirror. `None` = no override (default `false`).
    pub auto_update: Option<bool>,
    /// Persisted `[toolset.ask_user_question].timeout_enabled` mirror, seeded
    /// from the effective TOML merge like `show_tips`. `None` = unset in TOML
    /// (default `true`); toggles write the user layer.
    pub ask_user_question_timeout_enabled: Option<bool>,
    /// Whether ZDR users are allowed to use the product.
    /// Server-controlled via RemoteSettings (remote settings). Default `false` (blocked) during beta.
    pub zdr_access_enabled: bool,
    /// When set, `/usage` shows a link to this URL instead of fetching billing
    /// data from the backend. Server-controlled via RemoteSettings (remote settings
    /// `echo_agent_build_usage_redirect_url`, targeted at personal-team users).
    /// `None` (default) fetches usage from the backend.
    pub usage_billing_redirect_url: Option<String>,
    pub access_gate_shown_logged: bool,
    /// (hide-key, surface) pairs whose `AnnouncementCtaShown` impression was
    /// already logged — once per pager process, cleared on logout. Keyed by
    /// `announcement_hide_key` (stable even for id-less items, unlike the
    /// event's `id`).
    pub announcement_cta_impressions_logged:
        std::collections::BTreeSet<(String, echo_agent_telemetry::events::AnnouncementCtaSurface)>,
    /// Access gate from `echo_agent_build_access_gate`. `Some` = blocked.
    pub gate: Option<echo_agent_runtime::auth::GateInfo>,
    /// User-friendly subscription tier name (e.g. "EchoAgent", "Free").
    pub subscription_tier: Option<String>,
    /// When the pager started auto-checking subscriptions (for 10-min timeout).
    pub paywall_check_started: Option<std::time::Instant>,
    /// Debounce stamp for watch/focus subscription checks (see
    /// [`super::subscription`]).
    pub last_subscription_check_at: Option<std::time::Instant>,
    /// Server override (seconds) for the subscription-watch cadence.
    pub subscription_watch_interval_secs: Option<u64>,
    /// A stale-source gate held out of `gate` while a live check verifies
    /// it (see [`super::subscription`]).
    pub pending_gate_verification: Option<echo_agent_runtime::auth::GateInfo>,
    /// Generation stamp of the current gate verification.
    pub gate_verify_gen: u64,
    /// Whether a leader reconnect is in progress (blocks prompt submission).
    pub reconnect_pending: bool,
    /// Structured startup warnings collected from the terminal diagnostics
    /// engine at launch. Empty when the environment is healthy.
    pub startup_warnings: Vec<crate::startup::StartupWarning>,
    /// Whether the user authenticated with an API key (shown in the version badge).
    pub is_api_key_auth: bool,
    /// Latest version string from a background update check. Set when
    /// a newer version is detected; rendered as a notification on the
    /// welcome screen.
    pub pending_update_version: Option<String>,
    /// When true, the event loop should exit so the user can relaunch
    /// to pick up the downloaded update.
    pub quit_for_update: bool,
    /// Generation and state for the one launch-scoped foreign resume detection.
    pub(crate) foreign_resume_launch_generation: u64,
    pub(crate) foreign_resume_launch: Option<crate::app::foreign_sessions::ForeignResumeLaunch>,
    /// When set, the event loop should exit and the process re-exec into the
    /// other screen mode. Driven by `/minimal` and `/fullscreen`. Captures the
    /// session id at action time so a later teardown cannot drop `--resume`.
    pub relaunch: Option<ScreenModeRelaunch>,
    /// Whether importable `.claude/` settings were detected at startup.
    pub has_claude_import: bool,
    /// When set, the welcome screen renders an interactive import modal instead of normal content.
    pub import_claude_modal: Option<crate::views::import_claude_modal::ImportClaudeModalState>,
    /// Doc viewer overlay for the welcome screen (release notes via Ctrl+L).
    pub welcome_doc_viewer: Option<crate::views::modal::ActiveModal>,
    /// Whether the pager uses fullscreen (alt-screen) or inline mode.
    /// Set from the resolved terminal state at startup; updated by the
    /// in-process `/minimal` ⇄ `/fullscreen` switch (`mode_switch`).
    pub(crate) screen_mode: super::ScreenMode,
    /// Pending in-process mode-switch target, consumed by the event loop.
    pub(crate) pending_screen_mode_switch: Option<super::ScreenMode>,
    /// Onboarding tutorial overlay, if open. Top-level (not per-agent) so it
    /// works over both the welcome screen and an agent session. Opened by
    /// `/tutorial` (also in the command palette).
    pub tutorial: Option<crate::views::tutorial::TutorialState>,
    /// Agent Dashboard state. `Some(_)` only when the dashboard view
    /// is active (`active_view == AgentDashboard`) or recently closed.
    /// Held outside the `ActiveView` discriminant because `DashboardState`
    /// is not `Copy` (owns its prompt widget, peek panel, etc.).
    pub dashboard: Option<crate::views::dashboard::DashboardState>,
    /// Where to return when leaving the dashboard. See [`DashboardReturn`].
    pub dashboard_return: Option<DashboardReturn>,
    /// Persisted dashboard configuration (pinned rows, reorderings,
    /// grouping). Loaded once on startup from
    /// `~/.echo-agent/config.toml`. `None` when the file/section is absent
    /// or contained malformed data — falls back to in-memory defaults.
    pub dashboard_persisted: Option<crate::views::dashboard::PersistedDashboard>,
    /// Per-platform key event normalizer.
    ///
    /// NOTE: new event consumers that bypass `AppView::handle_input`
    /// will not get rescued modifiers unless also normalizing.
    pub(crate) keyboard_normalizer: KeyboardNormalizer,
    /// Voice gate (GA default on at startup resolution). When false — remote
    /// kill switch or `ECHO_AGENT_VOICE_MODE=0` — the STT pipeline is not started and
    /// session voice mode cannot turn on. Unit tests leave this false until
    /// they call [`Self::apply_voice_mode_enabled`].
    pub voice_mode_enabled: bool,
    /// Session UI mode from `/voice` (this CLI process only — not in config.toml).
    /// When true and the pipeline is up, the in-prompt dictation overlay can show
    /// and capture may start. Cleared on exit or when the remote flag turns off.
    pub voice_ui_active: bool,
    /// Optional `[voice]` overrides from config (`api_base`, `language`, …).
    pub voice_config: echo_agent_voice::VoiceConfig,
    /// Auth for STT (OAuth session via shell `AuthManager`, or `ECHO_AGENT_API_KEY`).
    /// `None` until the pipeline is first started (lazy on `/voice`).
    pub voice_auth: Option<echo_agent_voice::SharedVoiceAuth>,
    /// Commands into the voice pipeline (start/stop capture — toggle, not hold).
    pub voice_cmd_tx: Option<tokio::sync::mpsc::Sender<echo_agent_voice::VoiceCommand>>,
    /// The dictation lifecycle (idle / queued / recording / stopping), including
    /// the live interim transcript. One state at a time, so inconsistent
    /// combinations are unrepresentable; production mutates it only through the
    /// `AppView::voice_*` transition methods.
    pub voice_state: VoiceState,
}
/// Reshow window elapsed? None/0 = never. Unparseable ack fails open (show).
fn privacy_banner_reshow_elapsed(acked_at: &str, reshow_days: Option<u64>) -> bool {
    let Some(days) = reshow_days.filter(|d| *d > 0) else {
        return false;
    };
    let Ok(acked) = chrono::DateTime::parse_from_rfc3339(acked_at) else {
        return true;
    };
    let acked_utc = acked.with_timezone(&chrono::Utc);
    let Some(next) = acked_utc.checked_add_signed(chrono::Duration::days(days as i64)) else {
        return false;
    };
    chrono::Utc::now() >= next
}
impl AppView {
    /// Finishes startup if this view still holds the obligation; does nothing after.
    pub(crate) fn finish_startup(
        &mut self,
        outcome: echo_agent_telemetry::startup::StartupOutcome,
    ) {
        echo_agent_telemetry::startup::PendingStartup::finish_held(
            &mut self.pending_startup,
            outcome,
        );
    }
    /// Releases the obligation without recording; does nothing after finish.
    pub(crate) fn abandon_startup(&mut self) {
        if let Some(pending) = self.pending_startup.take() {
            pending.abandon();
        }
    }
    pub fn is_zdr_blocked(&self) -> bool {
        self.is_zdr && !self.zdr_access_enabled
    }
    /// User is not gated (no gate from remote settings or subscription fallback).
    pub fn has_access(&self) -> bool {
        self.gate.is_none()
    }
    /// True when the user should not see the prompt (gate, subscription, or ZDR).
    pub fn is_access_blocked(&self) -> bool {
        !self.has_access() || self.is_zdr_blocked()
    }
    /// Coding-data preference is team-admin-owned for non-admin members.
    pub fn is_team_non_admin(&self) -> bool {
        self.team_name.is_some()
            && !self
                .team_role
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case("admin"))
    }
    /// Whether `/feedback` may offer the trace-consent card: the shell
    /// advertised the offer and no card answer latched it off this session.
    /// Derived so no code path can fabricate an offer the shell never made.
    pub fn feedback_trace_offer(&self) -> bool {
        self.shell_feedback_trace_offer && !self.feedback_trace_choice_latched
    }
    /// Why `coding_data_sharing` is locked for this user (`None` = editable).
    /// Mirrors the dispatch guards in `set_coding_data_sharing`.
    pub fn coding_data_sharing_lock(&self) -> Option<crate::settings::CodingDataSharingLock> {
        if self.is_zdr {
            Some(crate::settings::CodingDataSharingLock::Zdr)
        } else if self.is_team_non_admin() {
            Some(crate::settings::CodingDataSharingLock::TeamManaged)
        } else {
            None
        }
    }
    /// Welcome privacy banner visibility gates.
    pub fn privacy_banner_should_show(&self) -> bool {
        if self.screen_mode.is_minimal() {
            return false;
        }
        if !self.privacy_notice_rollout {
            return false;
        }
        if self.is_zdr || self.is_team_non_admin() {
            return false;
        }
        if !self.coding_data_retention_opt_out {
            return false;
        }
        if !matches!(self.auth_state, AuthState::Done)
            || !self.has_access()
            || self.is_zdr_blocked()
            || !matches!(self.trust_state, TrustState::Done)
        {
            return false;
        }
        match self.privacy_banner_acked.as_deref() {
            None => true,
            Some(acked_at) => {
                privacy_banner_reshow_elapsed(acked_at, self.privacy_banner_reshow_days)
            }
        }
    }
    /// Whether deferred session-startup actions may run: both auth AND folder
    /// trust must be resolved. Mirrors the auth gate at the session-creating
    /// startup sites; trust is gated AFTER auth so a pending trust question
    /// defers session creation until answered.
    pub fn session_startup_allowed(&self) -> bool {
        matches!(self.auth_state, AuthState::Done)
            && matches!(self.trust_state, TrustState::Done)
            && matches!(self.consent_state, ConsentState::Done)
    }
    /// Whether startup type-ahead captured while the app was loading may be
    /// replayed into the input channel: every startup screen that consumes raw
    /// keystrokes must be resolved so the composer is the active consumer.
    /// Mirrors the folder-trust interceptor's gate (auth Done, has access, not
    /// ZDR-blocked) plus trust Done. When this is false at launch the captured
    /// prompt is dropped rather than replayed (see `event_loop::run`), so e.g. a
    /// prompt starting with "n" cannot answer the folder-trust question and quit.
    pub fn ready_for_startup_typeahead(&self) -> bool {
        matches!(self.auth_state, AuthState::Done)
            && self.has_access()
            && !self.is_zdr_blocked()
            && matches!(self.trust_state, TrustState::Done)
            && matches!(self.consent_state, ConsentState::Done)
    }
    /// Extract `GateInfo` from `RemoteSettings`.
    pub fn gate_from_settings(
        rs: &echo_agent_runtime::util::config::RemoteSettings,
    ) -> Option<echo_agent_runtime::auth::GateInfo> {
        let msg = rs.gate_message.as_ref()?;
        if msg.is_empty() {
            return None;
        }
        Some(echo_agent_runtime::auth::GateInfo {
            message: msg.clone(),
            url: rs.gate_url.clone(),
            label: rs.gate_label.clone(),
        })
    }
    /// Apply typed auth metadata from the shell.
    pub fn apply_auth_meta(&mut self, meta: &echo_agent_runtime::auth::AuthMeta) {
        self.pending_gate_verification = None;
        let was_gated = self.gate.is_some();
        self.account_email = meta.email.clone();
        self.team_id = meta.team_id.clone();
        self.team_name = meta.team_name.clone();
        self.is_zdr = meta.is_zdr;
        self.team_role = meta.team_role.clone();
        self.coding_data_retention_opt_out = meta.coding_data_retention_opt_out;
        self.shell_feedback_trace_offer = meta.feedback_trace_offer;
        self.gate = meta.gate.clone();
        if was_gated && self.gate.is_none() {
            self.paywall_check_started = None;
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::SubscriptionActivated {
                    auth_method: self.login_method_id.as_ref().map(|id| id.0.to_string()),
                    upsell_shown_this_session: self.access_gate_shown_logged,
                },
            );
        }
        self.subscription_tier = meta.subscription_tier.clone();
        let was_api_key = self.is_api_key_auth;
        self.is_api_key_auth = meta.auth_mode.as_deref().is_some_and(is_api_key_label)
            || meta
                .subscription_tier
                .as_deref()
                .is_some_and(is_api_key_label);
        self.usage_visible =
            meta.team_name.is_none() && !self.is_api_key_auth && !self.has_external_auth_provider;
        self.sync_billing_surface_to_agents();
        self.apply_tier_restrictions();
        if self.is_api_key_auth {
            self.ensure_voice_for_api_key();
        } else if was_api_key && is_restricted_tier(self.subscription_tier.as_deref()) {
            self.voice_reset();
            self.voice_ui_active = false;
            self.apply_voice_mode_enabled(false);
        }
        if let Some(show) = meta.show_resolved_model {
            self.show_resolved_model = show;
        }
    }
    /// Mirror billing + `/usage` gates onto every slash surface (agents,
    /// welcome, dashboard dispatch / peek-reply).
    pub(crate) fn sync_billing_surface_to_agents(&mut self) {
        let billing = self.usage_visible;
        let usage_cmd = !self.has_external_auth_provider;
        for agent in self.agents.values_mut() {
            agent.set_billing_surface_visible(billing);
            agent.set_usage_command_visible(usage_cmd);
        }
        self.welcome_prompt
            .slash_controller
            .set_billing_surface_visible(billing);
        self.welcome_prompt
            .slash_controller
            .set_usage_command_visible(usage_cmd);
        if let Some(dash) = self.dashboard.as_mut() {
            dash.dispatch
                .slash_controller
                .set_billing_surface_visible(billing);
            dash.dispatch
                .slash_controller
                .set_usage_command_visible(usage_cmd);
            dash.peek_reply
                .slash_controller
                .set_billing_surface_visible(billing);
            dash.peek_reply
                .slash_controller
                .set_usage_command_visible(usage_cmd);
        }
    }
    /// Force voice on for API-key sessions when only a remote rule left it off.
    /// Requirement / env / config pins still win.
    pub(crate) fn ensure_voice_for_api_key(&mut self) {
        if !self.is_api_key_auth || self.voice_mode_enabled {
            return;
        }
        if crate::app::resolve_voice_mode_live(None, false) {
            self.apply_voice_mode_enabled(true);
        }
    }
    /// Create a new AppView with the given ACP connection details.
    pub fn new(
        acp_tx: AcpAgentTx,
        models: ModelState,
        bootstrap_acp_commands: Vec<agent_client_protocol::AvailableCommand>,
    ) -> Self {
        let slash_mru =
            std::rc::Rc::new(std::cell::RefCell::new(crate::slash::mru::SlashMru::new()));
        let command_tags =
            std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
        let mut welcome_prompt = PromptWidget::new();
        welcome_prompt.adopt_slash_mru(slash_mru.clone());
        welcome_prompt.adopt_command_tags(command_tags.clone());
        Self {
            pending_startup: None,
            active_view: ActiveView::Welcome,
            auth_return_view: None,
            agents: IndexMap::new(),
            next_agent_id: 0,
            models,
            registry: ActionRegistry::defaults(),
            settings_registry: Arc::new(crate::settings::SettingsRegistry::defaults()),
            current_ui: echo_agent_runtime::agent::config::UiConfig::default(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            cwd_has_git_ancestor: std::env::current_dir()
                .ok()
                .is_some_and(|c| c.ancestors().any(|p| p.join(".git").exists())),
            acp_tx,
            bundle_state: BundleState::default(),
            scratch: ScratchBuffer::new(),
            cursor: CursorState::new(),
            pending_action: None,
            exit_session_pending: None,
            scroll_state: MouseScrollState::default(),
            scroll_config: ScrollConfig::from_settings(),
            appearance: AppearanceConfig::default(),
            notification_service: NotificationService::new(Default::default()),
            status_line: Default::default(),
            pending_notification_escapes: None,
            deferred_notification: None,
            tracing_rx: None,
            scroll_debug_hud: crate::views::scroll_debug_hud::ScrollDebugHud::new(),
            fps_hud: crate::views::fps_hud::FpsHud::new(),
            active_announcements: Vec::new(),
            hidden_announcement_ids: Default::default(),
            announcements_last_gen: 0,
            announcement: None,
            changelog_markdown: None,
            changelog_bullets: Vec::new(),
            tips: Vec::new(),
            tip: None,
            welcome_prompt,
            slash_mru,
            command_tags,
            welcome_prompt_focused: true,
            welcome_tip_typing_dismissed: false,
            pending_effects: Vec::new(),
            pending_editor: None,
            pending_pager_path: None,
            pending_pager_ansi: false,
            minimal_state: crate::minimal_api::MinimalState::default(),
            welcome_menu_index: None,
            welcome_menu_rects: Vec::new(),
            welcome_show_changelog_action: false,
            welcome_import_banner_rect: None,
            last_mouse_pos: None,
            last_scroll_pos: None,
            last_cache_evict_at: None,
            welcome_prompt_rect: None,
            welcome_auth_url_rect: None,
            welcome_on_auth_url: false,
            welcome_on_changelog_cta: false,
            welcome_announcement: WelcomeAnnouncementState::default(),
            welcome_auth_fallback_rect: None,
            welcome_refresh_rect: None,
            welcome_gate_url_rect: None,
            welcome_consent_link_rects: Vec::new(),
            welcome_consent_hover_link: None,
            consent_answered: None,
            welcome_upgrade_cta_rect: None,
            welcome_privacy_banner_opt_in_rect: None,
            welcome_privacy_banner_opt_out_rect: None,
            welcome_privacy_banner_terms_rect: None,
            welcome_privacy_banner_policy_rect: None,
            #[cfg(feature = "local-workspace")]
            welcome_workspace_mode_rects: Default::default(),
            #[cfg(feature = "local-workspace")]
            welcome_on_workspace_mode: false,
            welcome_toast: None,
            welcome_on_privacy_banner: false,
            welcome_on_upgrade_cta: false,
            welcome_changelog_cta_rect: None,
            auth_show_raw_url: false,
            native_select_hold: false,
            session_picker_entries: None,
            session_picker_loading: false,
            session_picker_state: crate::views::picker::PickerState::with_mode(
                crate::views::picker::PickerMode::FullScreen,
            ),
            session_picker_source_filter: crate::views::session_picker::SourceFilter::default(),
            session_picker_relaxed_notified_for: None,
            session_picker_content_results: None,
            session_picker_content_loading: false,
            session_picker_deep_search_seq: 0,
            session_picker_list_seq: 0,
            foreign_session_compat: Default::default(),
            foreign_session_scan_seq: 0,
            foreign_scan_coordinator: Default::default(),
            session_picker_lanes: Default::default(),
            session_picker_detail_generation: 0,
            session_picker_entries_query: None,
            session_picker_pending_delete: None,
            welcome_tick: 0,
            welcome_shimmer_frame: 0,
            cli_model_override: None,
            cli_effort_token: None,
            default_yolo: false,
            permission_mode_from_soft_default: true,
            auto_mode_gate:
                echo_agent_runtime::util::config::auto_permission_mode_enabled_from_disk(),
            yolo_policy_block: None,
            yolo_launch_block_notice: None,
            screen_mode_switch_hint: None,
            require_plan_approval: false,
            plan_mode: false,
            subagents: false,
            ask_user: false,
            chat_mode: false,
            #[cfg(feature = "local-workspace")]
            welcome_workspace_mode: crate::views::welcome::WelcomeWorkspaceMode::Sandbox,
            #[cfg(feature = "local-workspace")]
            local_workspace_startup_locked: false,
            #[cfg(feature = "local-workspace")]
            welcome_session_local_workspace: None,
            #[cfg(feature = "local-workspace")]
            welcome_local_workspace_ack_pending: false,
            #[cfg(feature = "local-workspace")]
            welcome_history_load_as_build: false,
            mouse_captured: true,
            new_worktree_dialog: None,
            contextual_hints: Default::default(),
            remote_contextual_hints: None,
            tip_seen_counts: Default::default(),
            last_known_terminal_rows: 0,
            small_screen_tip_evaluated: false,
            ssh_wrap_tip_evaluated: false,
            clipboard_focus_tip: Default::default(),
            new_session_worktree_mode: WorktreeMode::Never,
            fork_worktree_mode: WorktreeMode::Ask,
            restore_code: None,
            suppress_code_restore_once: None,
            resume_local_miss: None,
            agent_override: None,
            bootstrap_acp_commands,
            auth_methods: Vec::new(),
            auth_state: AuthState::Done,
            trust_state: TrustState::Done,
            consent_state: crate::app::consent::ConsentState::Done,
            account_email: None,
            login_label: None,
            login_method_id: None,
            auth_start_mode: AuthMode::Pending,
            auth_code_input: LineEditor::default(),
            next_auth_request_seq: 1,
            auth_url_poll_handle: None,
            deferred_startup: Default::default(),
            auth_use_oauth: false,
            auth_clipboard_delivery: None,
            auth_clipboard_feedback_generation: 0,
            team_id: None,
            team_name: None,
            is_zdr: false,
            team_role: None,
            coding_data_retention_opt_out: true,
            privacy_notice_rollout: false,
            privacy_banner_reshow_days: None,
            privacy_banner_acked: None,
            privacy_banner_opt_in_inflight: false,
            coding_data_write_seq: 0,
            show_tips: None,
            auto_update: None,
            ask_user_question_timeout_enabled: None,
            zdr_access_enabled: false,
            usage_billing_redirect_url: None,
            access_gate_shown_logged: false,
            announcement_cta_impressions_logged: Default::default(),
            gate: None,
            subscription_tier: None,
            paywall_check_started: None,
            last_subscription_check_at: None,
            subscription_watch_interval_secs: None,
            pending_gate_verification: None,
            gate_verify_gen: 0,
            reconnect_pending: false,
            startup_warnings: Vec::new(),
            is_api_key_auth: false,
            pending_update_version: None,
            foreign_resume_launch_generation: 0,
            foreign_resume_launch: None,
            quit_for_update: false,
            relaunch: None,
            has_claude_import: false,
            import_claude_modal: None,
            welcome_doc_viewer: None,
            screen_mode: ScreenMode::Inline,
            pending_screen_mode_switch: None,
            show_resolved_model: true,
            sharing_enabled: false,
            plugin_cta_enabled: false,
            plugin_cta_marketplace: None,
            workspace_dashboard_enabled: false,
            usage_visible: true,
            has_external_auth_provider: false,
            tier_restricted_commands: Vec::new(),
            leader_mode: false,
            credit_balance: None,
            auto_topup: None,
            billing_poll_wanted: false,
            leader_roster: Vec::new(),
            dashboard_local_sessions: Vec::new(),
            dashboard_sessions_loading: false,
            shared_prompt_queues: std::collections::HashMap::new(),
            optimistic_prompt_echoes: std::collections::HashMap::new(),
            pending_running_adoptions: std::collections::HashMap::new(),
            session_picker_grouped: false,
            scheduler_background_loops_seed: true,
            cancel_rewind_enabled: true,
            session_recap_available: false,
            shell_feedback_trace_offer: false,
            feedback_trace_choice_latched: false,
            feedback_trace_upload_pending: None,
            tutorial: None,
            dashboard: None,
            dashboard_return: None,
            dashboard_persisted: None,
            keyboard_normalizer: KeyboardNormalizer::from_terminal_context(),
            voice_mode_enabled: false,
            voice_ui_active: false,
            voice_config: echo_agent_voice::VoiceConfig::default(),
            voice_auth: None,
            voice_cmd_tx: None,
            voice_state: VoiceState::Idle,
        }
    }
    /// Seed `deferred_model_switch` from CLI `-m`. The CLI effort token is
    /// resolved later against the authoritative session catalog in
    /// [`take_deferred_model_switch`](crate::app::dispatch::session::lifecycle::take_deferred_model_switch);
    /// resolving it here would use the pre-session dashboard catalog and a
    /// remapped menu id could resolve differently.
    pub fn deferred_model_switch_from_cli(&self) -> Option<crate::app::agent::DeferredModelSwitch> {
        Some(crate::app::agent::DeferredModelSwitch {
            model_id: self.cli_model_override.clone()?,
            effort: None,
            prev_model_id: None,
        })
    }
    /// Voice capture is armed: the in-prompt dictation overlay can show and
    /// Ctrl+Space can start capture.
    ///
    /// Requires the voice gate, session `/voice` mode, and a live pipeline.
    /// Stopping capture remains allowed when the kill switch flips mid-record
    /// (see `dispatch_voice_toggle`).
    pub fn voice_available(&self) -> bool {
        self.voice_mode_enabled && self.voice_ui_active && self.voice_cmd_tx.is_some()
    }
    /// Whether launch may spawn the background STT pipeline (independent of
    /// `/voice`). Gated on the voice gate + a build that compiled in audio
    /// capture. Free-tier upsell is separate ([`Self::is_voice_tier_restricted`]).
    pub fn voice_can_start_pipeline(&self) -> bool {
        self.voice_mode_enabled && echo_agent_voice::AUDIO_SUPPORTED
    }
    /// Sync voice availability into slash surfaces, cheatsheet, and settings.
    /// Mirrors `apply_session_recap_available` for `/recap`.
    pub fn apply_voice_mode_enabled(&mut self, enabled: bool) {
        self.voice_mode_enabled = enabled;
        crate::app::VOICE_MODE_ENABLED.store(enabled, std::sync::atomic::Ordering::Release);
        for agent in self.agents.values_mut() {
            agent.set_voice_mode_available(enabled);
            match agent.active_modal.as_mut() {
                Some(crate::views::modal::ActiveModal::Settings { state }) => {
                    state.rebuild_rows();
                }
                Some(crate::views::modal::ActiveModal::ResetSettingsConfirm {
                    settings_state,
                    ..
                }) => {
                    settings_state.rebuild_rows();
                }
                _ => {}
            }
        }
        self.welcome_prompt.set_voice_visible(enabled);
        if let Some(dashboard) = self.dashboard.as_mut() {
            dashboard.set_voice_visible(enabled);
        }
    }
    /// Sync the auto permission-mode feature gate into every slash surface.
    /// `/auto` is hard-hidden when `self.auto_mode_gate` is off; otherwise both
    /// `/always-approve` and `/auto` stay offered as true toggles. Mirrors
    /// [`Self::apply_voice_mode_enabled`]. Call after gate flips, startup,
    /// reconnect, and session create/switch (so new agents inherit the gate).
    pub fn sync_permission_mode_slash_gate(&mut self) {
        let available = self.auto_mode_gate;
        for agent in self.agents.values_mut() {
            agent.prompt.set_auto_mode_available(available);
        }
        self.welcome_prompt.set_auto_mode_available(available);
        if let Some(dashboard) = self.dashboard.as_mut() {
            dashboard.set_auto_mode_available(available);
        }
    }
    /// Recompute the tier-restricted slash commands from the current auth
    /// state and sync the deny list into every slash surface (welcome
    /// prompt, all agents, dashboard) so restricted commands hide/show in
    /// lockstep. Mirrors [`Self::apply_voice_mode_enabled`].
    ///
    /// Called from [`Self::apply_auth_meta`] (startup / login) and from the
    /// `echo.agent/settings/update` handler when the subscription tier changes, so
    /// a mid-session upgrade lifts the restrictions without a restart.
    pub fn apply_tier_restrictions(&mut self) {
        let restricted = self.team_name.is_none()
            && !self.is_api_key_auth
            && !self.has_external_auth_provider
            && is_restricted_tier(self.subscription_tier.as_deref());
        let names: Vec<String> = if restricted {
            TIER_RESTRICTED_COMMANDS
                .iter()
                .map(|n| (*n).to_string())
                .collect()
        } else {
            Vec::new()
        };
        for agent in self.agents.values_mut() {
            agent.set_restricted_commands(&names);
        }
        self.welcome_prompt.set_restricted_commands(&names);
        if let Some(dashboard) = self.dashboard.as_mut() {
            dashboard.set_restricted_commands(&names);
        }
        self.tier_restricted_commands = names;
    }
    /// Whether voice mode is withheld for the current subscription tier
    /// (free / X Basic personal accounts). Derived from the computed
    /// [`Self::tier_restricted_commands`] deny list so it stays in lockstep
    /// with the slash-command gate. Used to gate the Ctrl+Space / F8 voice
    /// keybinding, which bypasses the slash registry entirely (see
    /// [`crate::app::dispatch::voice`]).
    pub fn is_voice_tier_restricted(&self) -> bool {
        self.tier_restricted_commands.iter().any(|c| c == "voice")
    }
    /// Draw-time expiry can flip the live-announcement predicate between
    /// pushes; resync the slash gate only when it diverges from the stored
    /// flags (checked per frame, fan-out runs only on change).
    pub fn resync_announcement_slash_gate_on_divergence(&mut self) {
        let has =
            crate::views::announcements::has_session_announcements(&self.active_announcements);
        if self
            .agents
            .values()
            .any(|a| a.prompt.slash_controller.has_session_announcements() != has)
        {
            self.sync_session_announcement_slash_gate();
        }
    }
    /// Offer `/announcements` only when session items (critical or promo)
    /// exist (even if currently hidden — user may still run `/announcements
    /// show`).
    pub fn sync_session_announcement_slash_gate(&mut self) {
        let has =
            crate::views::announcements::has_session_announcements(&self.active_announcements);
        for agent in self.agents.values_mut() {
            agent
                .prompt
                .slash_controller
                .set_has_session_announcements(has);
            for child in agent.subagent_views.values_mut() {
                child.set_has_session_announcements(has);
            }
        }
    }
    /// Mic is live (the [`VoiceState::Recording`] state).
    pub fn voice_listening(&self) -> bool {
        self.voice_state.listening()
    }
    /// Whether the in-flight session is owned by a hold-press (so its key
    /// release ends it). `/voice` and toggle-style starts leave this false.
    pub fn voice_hold_owned(&self) -> bool {
        self.voice_state.hold()
    }
    /// The prompt box that owns in-flight dictation, if any.
    pub fn voice_recording_target(&self) -> Option<VoiceTarget> {
        self.voice_state.target()
    }
    /// The live partial transcript shown in the prompt overlay, if any.
    pub fn voice_interim(&self) -> Option<&str> {
        self.voice_state.interim()
    }
    /// Best-effort one-shot command into the voice pipeline (no-op if it isn't up).
    fn voice_send(&self, cmd: echo_agent_voice::VoiceCommand) {
        if let Some(tx) = &self.voice_cmd_tx
            && tx.try_send(cmd).is_err()
        {
            tracing::trace!("voice command dropped: pipeline channel full or closed");
        }
    }
    /// Open the mic now (pipeline already up) and enter [`VoiceState::Recording`]
    /// bound to `target`. `hold` marks a Ctrl+Space hold-press start.
    pub(crate) fn voice_begin_recording(&mut self, target: VoiceTarget, hold: bool) {
        self.voice_send(echo_agent_voice::VoiceCommand::PttPress);
        self.voice_state = VoiceState::Recording {
            hold,
            target,
            interim: None,
        };
    }
    /// Set the live interim transcript. No-op unless recording, so a late event
    /// after a stop can't repopulate the overlay.
    pub(crate) fn voice_set_interim(&mut self, text: String) -> bool {
        if let VoiceState::Recording { interim, .. } = &mut self.voice_state {
            *interim = Some(text);
            true
        } else {
            false
        }
    }
    /// Clear the interim in place, keeping the current state. Called when a final
    /// commits (or yields empty) so the overlay drops the partial without a
    /// teardown.
    pub(crate) fn voice_clear_interim(&mut self) {
        match &mut self.voice_state {
            VoiceState::Recording { interim, .. } | VoiceState::Stopping { interim, .. } => {
                *interim = None;
            }
            VoiceState::Idle | VoiceState::ColdStart { .. } => {}
        }
    }
    /// Explicit stop (Esc / Ctrl+Space / `[stop]`): release the mic but keep
    /// the target and last interim so a trailing STT final still lands. Always
    /// allowed — never leaves a hot mic. No-op unless recording.
    pub(crate) fn voice_stop_keeping_final(&mut self) {
        let VoiceState::Recording {
            target, interim, ..
        } = &mut self.voice_state
        else {
            return;
        };
        let target = *target;
        let interim = interim.take();
        self.voice_send(echo_agent_voice::VoiceCommand::PttRelease);
        self.voice_state = VoiceState::Stopping { target, interim };
    }
    /// Hard teardown (submit / error / kill-switch / navigate-away): release the
    /// mic and forget the session — no trailing final, no queued start.
    pub(crate) fn voice_reset(&mut self) {
        if self.voice_state.listening() {
            self.voice_send(echo_agent_voice::VoiceCommand::PttRelease);
        }
        self.voice_state = VoiceState::Idle;
    }
    /// Ctrl+Space hold release: end only a session a Ctrl+Space hold started —
    /// cancel a queued hold cold-start, or stop a live hold recording (keeping
    /// its trailing final). A `/voice` / toggle session (`hold` false) is left
    /// untouched, so a Ctrl+Space release can neither cancel nor stop it.
    pub(crate) fn voice_hold_release(&mut self) {
        match self.voice_state {
            VoiceState::ColdStart { hold: true, .. } => self.voice_reset(),
            VoiceState::Recording { hold: true, .. } => self.voice_stop_keeping_final(),
            _ => {}
        }
    }
    /// Whether the active view still owns the bound dictation `target` — i.e. the
    /// box dictation started in is the one currently on screen and selected. The
    /// target is bound at capture start; on the dashboard that means dispatch
    /// requires no peek open, a peek reply requires the *same* top-level row still
    /// peeked (the shared reply widget clears on row change), and any open
    /// attached-agent popup (which occludes the dashboard inputs) disqualifies it.
    /// `false` when no dictation is bound.
    fn voice_target_on_active_surface(&self) -> bool {
        let Some(target) = self.voice_recording_target() else {
            return false;
        };
        if matches!(self.active_view, ActiveView::AgentDashboard)
            && self
                .dashboard
                .as_ref()
                .is_some_and(|d| d.attached_agent.is_some())
        {
            return false;
        }
        let peeked_top_level = self
            .dashboard
            .as_ref()
            .and_then(|d| match d.peek.as_ref()?.row {
                crate::views::dashboard::DashboardRowId::TopLevel(id) => Some(id),
                _ => None,
            });
        match (self.active_view, target) {
            (ActiveView::Agent(active), VoiceTarget::Agent(rec)) => active == rec,
            (ActiveView::AgentDashboard, VoiceTarget::DashboardDispatch) => {
                self.dashboard.as_ref().is_none_or(|d| d.peek.is_none())
            }
            (ActiveView::AgentDashboard, VoiceTarget::DashboardPeekReply(rec)) => {
                peeked_top_level == Some(rec)
            }
            _ => false,
        }
    }
    /// Auto-release the mic if the user navigates away from the box that started
    /// recording (another agent / dashboard popup / a changed peek row). Keeps
    /// stop controls and the recording session aligned. Event-loop each tick;
    /// no-op unless recording.
    pub fn enforce_voice_session_bound(&mut self) {
        if !self.voice_state.listening() || self.voice_target_on_active_surface() {
            return;
        }
        self.voice_reset();
    }
    /// Esc handling shared by the agent and dashboard surfaces: while voice is
    /// active, Esc aborts it (and consumes the key) rather than falling into the
    /// surface's own Esc behaviour. Gated on voice state only (not the remote
    /// flag) so Esc can always abort. `None` means Esc isn't ours — the caller
    /// continues its normal routing.
    fn voice_esc_outcome(
        &mut self,
        key_event: Option<&crossterm::event::KeyEvent>,
    ) -> Option<InputOutcome> {
        let key = key_event?;
        if key.code != KeyCode::Esc || !key.modifiers.is_empty() {
            return None;
        }
        if self.voice_listening() {
            Some(InputOutcome::Action(Action::VoiceToggle))
        } else if self.voice_state.pending_cold_start() {
            self.voice_reset();
            Some(InputOutcome::Changed)
        } else {
            None
        }
    }
    /// App-level Esc owners that consume the key BEFORE any agent input
    /// routing — the render-boundary decision handed to the agent hint path
    /// (`AgentView::draw` → `esc_would_cancel_turn`) so a hint bar rendered
    /// beneath one of these never advertises `Esc cancel`.
    ///
    /// Mirrors `handle_input`'s intercepts, in their order: the focused dev
    /// tracing pane (step 1a consumes all non-global keys), the cloud modal
    /// (step 1d), the import-Claude modal (agent-arm intercept),
    /// [`Self::voice_esc_outcome`] — listening OR pending cold-start, the
    /// handler's actual condition, not the render-only recording flag — and
    /// the dashboard's attached-agent popup (dashboard-arm intercept). Keep
    /// this list in lockstep with those intercepts when adding a top-level
    /// Esc owner.
    pub(crate) fn esc_owned_before_agent(&self) -> bool {
        if matches!(self.active_view, ActiveView::AgentDashboard)
            && self
                .dashboard
                .as_ref()
                .and_then(|d| d.attached_agent)
                .is_some_and(|id| self.agents.contains_key(&id))
        {
            return true;
        }
        self.import_claude_modal.is_some()
            || self.voice_listening()
            || self.voice_state.pending_cold_start()
    }
    /// Commit interim on real send keys only (not multiline bare Enter).
    fn maybe_commit_voice_interim_before_submit_key(&mut self, key: &crossterm::event::KeyEvent) {
        if self.registry.matches_id(ActionId::InterjectPrompt, key) {
            let _ = crate::voice::commit_interim_into_prompt(self);
            return;
        }
        let multiline = match self.active_view {
            ActiveView::Agent(id) => self.agents.get(&id).is_some_and(|a| a.multiline_mode),
            ActiveView::AgentDashboard => self.dashboard.as_ref().is_some_and(|d| d.multiline_mode),
            _ => false,
        };
        let is_send = if multiline {
            crate::input::is_mod_enter(key)
        } else {
            matches!(key.code, KeyCode::Enter)
                || self.registry.matches_id(ActionId::SendPrompt, key)
        };
        if is_send {
            let _ = crate::voice::commit_interim_into_prompt(self);
        }
    }
    /// The active agent's view, when an agent tab is focused.
    ///
    /// Always the root agent, even when a subagent view is focused within the
    /// tab; for subagent-aware resolution use `dispatch::ctx::get_active_agent`.
    pub fn active_agent(&self) -> Option<&AgentView> {
        match self.active_view {
            ActiveView::Agent(id) => self.agents.get(&id),
            _ => None,
        }
    }
    /// Session ID of the active agent, if one exists and has an established session.
    pub fn active_session_id(&self) -> Option<&str> {
        match self.active_view {
            ActiveView::Agent(id) => self
                .agents
                .get(&id)
                .and_then(|a| a.session.session_id.as_ref())
                .map(|sid| sid.0.as_ref()),
            _ => None,
        }
    }
    /// Show a toast on the currently active view.
    ///
    /// From the dashboard, toasts route into the dispatch input's inline
    /// error slot. From an agent view the existing per-agent toast machinery
    /// fires. On welcome, an overlay above the prompt for
    /// [`WELCOME_TOAST_DURATION`].
    ///
    /// Reconnect success copy is skipped when a leader version-mismatch toast
    /// is already showing: registration (and thus the mismatch notif) finishes
    /// during reconnect, and the later "Reconnected." / "Session restored…"
    /// line would hide a still-true skew. Restore-failed and connection-failed
    /// toasts still replace it.
    pub fn show_toast(&mut self, msg: &str) {
        match self.active_view {
            ActiveView::Agent(id) => {
                if let Some(agent) = self.agents.get_mut(&id) {
                    if let Some(child_sid) = agent.active_subagent.clone()
                        && let Some(child) = agent.subagent_views.get_mut(&child_sid)
                    {
                        if reconnect_success_hides_mismatch(
                            child.toast.as_ref().map(|(m, _)| m.as_str()),
                            msg,
                        ) {
                            return;
                        }
                        child.show_toast(msg);
                    } else {
                        if reconnect_success_hides_mismatch(
                            agent.toast.as_ref().map(|(m, _)| m.as_str()),
                            msg,
                        ) {
                            return;
                        }
                        agent.show_toast(msg);
                    }
                }
            }
            ActiveView::AgentDashboard => {
                if let Some(d) = self.dashboard.as_mut() {
                    if reconnect_success_hides_mismatch(d.error_toast.as_deref(), msg) {
                        return;
                    }
                    d.error_toast = Some(crate::glyphs::sanitize_toast_message(msg).into_owned());
                }
            }
            ActiveView::Welcome => {
                if reconnect_success_hides_mismatch(
                    self.welcome_toast.as_ref().map(|(m, _)| m.as_str()),
                    msg,
                ) {
                    return;
                }
                self.welcome_toast = Some((
                    crate::glyphs::sanitize_toast_message(msg).into_owned(),
                    std::time::Instant::now() + WELCOME_TOAST_DURATION,
                ));
            }
        }
    }
    /// Insert or replace a leader roster entry, keyed by `session_id`.
    pub fn upsert_roster_entry(&mut self, entry: crate::app::roster::RosterEntry) {
        if let Some(existing) = self
            .leader_roster
            .iter_mut()
            .find(|e| e.session_id == entry.session_id)
        {
            *existing = entry;
        } else {
            self.leader_roster.push(entry);
        }
    }
    /// Remove a leader roster entry by `session_id`.
    pub fn remove_roster_entry(&mut self, sid: &str) {
        self.leader_roster.retain(|e| e.session_id != sid);
    }
    /// The roster source the dashboard renders alongside locally-hosted
    /// agents. In leader mode this is the live leader roster (FleetView). With
    /// no leader there is nothing to poll, so we fall back to the local
    /// on-disk session list ([`Self::dashboard_local_sessions`]) so the
    /// dashboard still shows idle/dormant sessions instead of being empty.
    pub fn dashboard_roster(&self) -> &[crate::app::roster::RosterEntry] {
        if self.leader_mode {
            &self.leader_roster
        } else {
            &self.dashboard_local_sessions
        }
    }
    /// Reconcile the shared prompt queue for a session from a
    /// `echo.agent/queue/changed` broadcast. The broadcast is
    /// authoritative: it fully replaces the previously-known queue for that
    /// session. An empty list clears the entry.
    ///
    /// Returns `(old_id, new_id)` for echoes retired via the kind+text
    /// fallback (re-keyed: the old id never appears in any broadcast). The
    /// caller routes these through `AgentView::note_queue_echo_rekeyed` so
    /// per-agent state moves with the message instead of leaking.
    pub fn apply_queue_changed(
        &mut self,
        changed: crate::app::prompt_queue::QueueChanged,
    ) -> Vec<(String, String)> {
        let crate::app::prompt_queue::QueueChanged {
            session_id,
            mut entries,
            running_prompt_id,
            running_text: _,
            running_kind: _,
            running_combined_texts: _,
        } = changed;
        let mut rekeyed_echo_ids: Vec<(String, String)> = Vec::new();
        let running_row: Option<(String, String)> = running_prompt_id.as_ref().and_then(|pid| {
            self.shared_prompt_queues
                .get(&session_id)
                .and_then(|q| q.iter().find(|e| &e.id == pid))
                .map(|e| (e.kind.clone(), e.text.clone()))
        });
        if let Some(opt) = self.optimistic_prompt_echoes.get_mut(&session_id) {
            opt.retain(|e| {
                let id_matches_running = running_prompt_id.as_deref() == Some(e.id.as_str());
                let id_matches_entry = entries.iter().any(|x| x.id == e.id);
                let content_match_id = running_row
                    .as_ref()
                    .filter(|(kind, text)| *kind == e.kind && *text == e.text)
                    .and_then(|_| running_prompt_id.clone())
                    .or_else(|| {
                        entries
                            .iter()
                            .find(|x| x.kind == e.kind && x.text == e.text)
                            .map(|x| x.id.clone())
                    });
                let retired = id_matches_running || id_matches_entry || content_match_id.is_some();
                if retired
                    && !id_matches_running
                    && !id_matches_entry
                    && let Some(new_id) = content_match_id
                {
                    rekeyed_echo_ids.push((e.id.clone(), new_id));
                }
                !retired
            });
            for e in opt.iter() {
                if !entries.iter().any(|x| x.id == e.id) {
                    let mut pinned = e.clone();
                    pinned.position = entries.len();
                    entries.push(pinned);
                }
            }
            if opt.is_empty() {
                self.optimistic_prompt_echoes.remove(&session_id);
            }
        }
        if entries.is_empty() {
            self.shared_prompt_queues.remove(&session_id);
        } else {
            self.shared_prompt_queues.insert(session_id, entries);
        }
        rekeyed_echo_ids
    }
    /// Push an optimistic echo row for a server-authoritative prompt the pager
    /// just sent (a plain prompt or agent-bound kind typed while a turn is
    /// running). The row is keyed by `prompt_id` so the authoritative
    /// `echo.agent/queue/changed` broadcast replaces it (matched by `id`) rather than
    /// duplicating it. `kind` (`"prompt"`/`"bash"`/…) drives the row's display
    /// and, on adoption, the turn-start shim's block + focus flag.
    pub fn push_optimistic_prompt_echo(
        &mut self,
        session_id: &str,
        prompt_id: &str,
        text: &str,
        kind: &str,
    ) {
        let entry = crate::app::prompt_queue::QueueEntryWire {
            id: prompt_id.to_string(),
            version: 0,
            owner: None,
            last_editor: None,
            kind: kind.to_string(),
            text: text.to_string(),
            combined_texts: None,
            position: 0,
        };
        let opt = self
            .optimistic_prompt_echoes
            .entry(session_id.to_string())
            .or_default();
        if !opt.iter().any(|e| e.id == entry.id) {
            opt.push(entry.clone());
        }
        let shared = self
            .shared_prompt_queues
            .entry(session_id.to_string())
            .or_default();
        if !shared.iter().any(|e| e.id == entry.id) {
            let mut e = entry;
            e.position = shared.len();
            shared.push(e);
        }
    }
    /// The shared (server-authoritative) prompt queue for a session, if any.
    pub fn shared_prompt_queue(
        &self,
        session_id: &str,
    ) -> Option<&Vec<crate::app::prompt_queue::QueueEntryWire>> {
        self.shared_prompt_queues.get(session_id)
    }
    /// Apply a (possibly hot-reloaded) appearance config to all agents.
    pub fn set_appearance(&mut self, config: AppearanceConfig) {
        crate::render::bidi::set_enabled(config.scrollback.display.rtl_bidi);
        for agent in self.agents.values_mut() {
            agent.scrollback.set_appearance(config.clone());
            for child in agent.subagent_views.values_mut() {
                child.scrollback.set_appearance(config.clone());
                child.prompt.sync_tab_width_from_appearance();
            }
            agent
                .prompt
                .slash_controller
                .registry_mut()
                .set_plugins_visible(!config.disable_plugins);
            agent.prompt.sync_tab_width_from_appearance();
        }
        self.welcome_prompt.sync_tab_width_from_appearance();
        self.appearance = config;
    }
    /// Recompute the render-value compact flag from the user setting +
    /// terminal height (`views::agent::effective_compact`) and propagate it
    /// to the appearance fan-out and every agent's prompt widget when it
    /// changed. In-memory only: never touches the user setting
    /// (`current_ui.compact_mode`), the render cache, or disk — auto-compact
    /// is derived, so growing the window restores the user's choice.
    pub(crate) fn apply_effective_compact(&mut self) {
        let derived = crate::views::agent::effective_compact(
            self.current_ui.compact_mode,
            self.last_known_terminal_rows,
        );
        if self.appearance.prompt.compact == derived {
            return;
        }
        let mut config = self.appearance.clone();
        config.prompt.compact = derived;
        self.set_appearance(config);
        for agent in self.agents.values_mut() {
            agent.prompt.set_compact(derived);
        }
    }
    /// Viewport height (rows) of the surface a scroll would move — the
    /// active agent's (or its fullscreen subagent's) scrollback pane, as
    /// measured at the last draw. 0 = unknown (welcome/dashboard views),
    /// which keeps the trackpad per-flush cap at its floor.
    fn scroll_viewport_height(&self) -> u16 {
        match self.active_view {
            ActiveView::Agent(id) => self.agents.get(&id).map_or(0, |agent| {
                let scrollback = agent
                    .active_subagent
                    .as_ref()
                    .and_then(|sid| agent.subagent_views.get(sid))
                    .map_or(&agent.scrollback, |child| &child.scrollback);
                scrollback.scroll_info().1
            }),
            _ => 0,
        }
    }
    /// Assemble the scroll-debug HUD's per-frame params (`None` unless
    /// enabled). Called by `draw()` BEFORE the frame closure: all scroll
    /// state updates for this frame already happened (input/ticks run before
    /// draw), and the snapshot is read-only, so the HUD observes exactly the
    /// state the frame renders without perturbing it.
    fn scroll_debug_panel(&self) -> Option<crate::views::scroll_debug_hud::ScrollDebugPanel> {
        if !self.scroll_debug_hud.enabled() {
            return None;
        }
        let config = self
            .scroll_config
            .with_viewport_height(self.scroll_viewport_height());
        let snapshot = self
            .scroll_state
            .debug_snapshot(&config, std::time::Instant::now());
        let view = match self.active_view {
            ActiveView::Agent(id) => self.agents.get(&id).map(|agent| {
                let scrollback = agent
                    .active_subagent
                    .as_ref()
                    .and_then(|sid| agent.subagent_views.get(sid))
                    .map_or(&agent.scrollback, |child| &child.scrollback);
                let (scroll_offset, viewport, total_height) = scrollback.scroll_info();
                let max_offset = total_height.saturating_sub(viewport as usize);
                crate::views::scroll_debug_hud::ViewportDebug {
                    scroll_offset,
                    max_offset,
                    total_height,
                    follow_mode: scrollback.is_follow_mode(),
                    at_bottom: scroll_offset >= max_offset,
                }
            }),
            _ => None,
        };
        let top_offset = self.dev_fps_rows() + self.fps_hud.overlay_height();
        Some(crate::views::scroll_debug_hud::ScrollDebugPanel {
            snapshot,
            view,
            top_offset,
        })
    }
    /// Rows the dev `ECHO_AGENT_FPS` overlay occupies (0 in non-dev builds), so
    /// runtime debug overlays stack below instead of overpainting it.
    fn dev_fps_rows(&self) -> u16 {
        0
    }
    /// Route a scroll delta to the active view.
    fn dispatch_scroll(&mut self, lines: i32, column: u16, row: u16) {
        match self.active_view {
            ActiveView::Agent(id) => {
                if let Some(agent) = self.agents.get_mut(&id) {
                    if let Some(child_sid) = agent.active_subagent.clone()
                        && let Some(child) = agent.subagent_views.get_mut(&child_sid)
                    {
                        child.handle_scroll(lines, column, row);
                        return;
                    }
                    agent.handle_scroll(lines, column, row);
                }
            }
            ActiveView::Welcome => {
                if let Some(crate::views::modal::ActiveModal::DocViewer { scroll, .. }) =
                    self.welcome_doc_viewer.as_mut()
                {
                    crate::views::modal::apply_doc_scroll_delta(scroll, lines);
                }
            }
            ActiveView::AgentDashboard => {
                let popup_target = self.dashboard.as_ref().and_then(|d| {
                    d.attached_agent
                        .zip(d.popup_outer_rect)
                        .filter(|(_, outer)| {
                            column >= outer.x
                                && column < outer.x + outer.width
                                && row >= outer.y
                                && row < outer.y + outer.height
                        })
                });
                if let Some((agent_id, _outer)) = popup_target {
                    if let Some(agent) = self.agents.get_mut(&agent_id) {
                        if let Some(child_sid) = agent.active_subagent.clone()
                            && let Some(child) = agent.subagent_views.get_mut(&child_sid)
                        {
                            child.handle_scroll(lines, column, row);
                            return;
                        }
                        agent.handle_scroll(lines, column, row);
                    }
                    return;
                }
                let in_file_search_dropdown = self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.file_search_dropdown_items_area)
                    .is_some_and(|dd| {
                        column >= dd.x
                            && column < dd.x + dd.width
                            && row >= dd.y
                            && row < dd.y + dd.height
                    });
                if in_file_search_dropdown {
                    if let Some(ref mut dashboard) = self.dashboard {
                        dashboard
                            .dropdown_file_search_mut()
                            .move_selection(lines.signum() as isize);
                    }
                    return;
                }
                let in_slash_dropdown = self
                    .dashboard
                    .as_ref()
                    .and_then(|d| d.slash_dropdown_items_area)
                    .is_some_and(|dd| {
                        column >= dd.x
                            && column < dd.x + dd.width
                            && row >= dd.y
                            && row < dd.y + dd.height
                    });
                if in_slash_dropdown {
                    if let Some(ref mut dashboard) = self.dashboard {
                        dashboard
                            .dispatch
                            .slash_scroll_selection(lines.signum() as isize);
                    }
                    return;
                }
                if let Some(ref mut dashboard) = self.dashboard {
                    dashboard.handle_scroll(lines);
                }
            }
        }
    }
}
impl AppView {
    /// Handle a terminal event. Routes through the input layer stack:
    ///
    /// 1. Pending action check (double-press confirmation)
    /// 2. Active view (Welcome or Agent — agent does pane + agent-level)
    /// 3. Global actions (quit with confirmation)
    ///
    /// Quit always goes through double-press confirmation, even when
    /// escalated from agent-level (e.g., Ctrl-C while cancelling).
    pub fn handle_input(&mut self, ev: &Event) -> InputOutcome {
        self.handle_input_at_with_paste_provenance(ev, Instant::now(), PasteProvenance::Terminal)
    }
    pub(crate) fn handle_input_at_with_paste_provenance(
        &mut self,
        ev: &Event,
        arrived_at: Instant,
        paste_provenance: PasteProvenance,
    ) -> InputOutcome {
        debug_assert!(
            matches!(ev, Event::Paste(_)) || paste_provenance == PasteProvenance::Terminal,
            "non-paste events cannot carry paste provenance"
        );
        let normalized = self.keyboard_normalizer.rescue(ev);
        let ev: &Event = &normalized;
        let key_event = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => Some(k),
            _ => None,
        };
        if let Event::Resize(_, rows) = ev {
            for agent in self.agents.values_mut() {
                agent.note_terminal_resize();
                for child in agent.subagent_views.values_mut() {
                    child.note_terminal_resize();
                }
            }
            self.last_known_terminal_rows = *rows;
            self.apply_effective_compact();
        }
        if let Some(key) = key_event
            && let Some(pending) = &self.pending_action
        {
            let stale_idle_arm_while_busy = matches!(
                pending.action,
                Action::ClearPrompt | Action::RewindShowPicker
            ) && matches!(
                self.active_view,
                ActiveView::Agent(id) if self.agents.get(&id).is_some_and(|a| {
                    a.session.state.is_turn_running()
                        || a.session.state.is_cancelling()
                        || a.wake_turn_active()
                })
            );
            if !stale_idle_arm_while_busy && !pending.expired() && pending.shortcut.matches(key) {
                let action = self.pending_action.take().unwrap().action;
                return InputOutcome::Action(action);
            }
            self.pending_action = None;
        }
        let modal_open = self.is_scroll_blocking_modal_open();
        if let Event::Mouse(mouse) = ev
            && let Some(direction) = ScrollDirection::from_mouse_event(mouse)
            && !modal_open
        {
            let config = self
                .scroll_config
                .with_viewport_height(self.scroll_viewport_height());
            let update = self
                .scroll_state
                .on_scroll_event_at(arrived_at, direction, config);
            let pos = (mouse.column, mouse.row);
            self.last_scroll_pos = Some(pos);
            if update.lines != 0 {
                self.dispatch_scroll(update.lines, pos.0, pos.1);
                return InputOutcome::Changed;
            }
            return InputOutcome::Unchanged;
        }
        if let Event::Mouse(mouse) = ev {
            self.last_mouse_pos = Some((mouse.column, mouse.row));
            let is_mouse_action = matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Up(MouseButton::Left)
                    | MouseEventKind::Moved
            );
            if is_mouse_action {}
        }
        if let Some(tutorial) = self.tutorial.as_mut()
            && matches!(ev, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
        {
            match crate::views::tutorial::handle_tutorial_input(ev, tutorial) {
                crate::views::tutorial::TutorialOutcome::Closed => {
                    self.tutorial = None;
                }
                crate::views::tutorial::TutorialOutcome::Consumed => {}
            }
            return InputOutcome::Changed;
        }
        let zdr_blocked = self.is_zdr_blocked();
        let has_access = self.has_access();
        let welcome_pinned_upgrade_cta = crate::views::announcements::promo_cta(
            &self.active_announcements,
            &self.hidden_announcement_ids,
        )
        .is_some_and(|(owner, _, _)| !crate::views::announcements::is_dismissible(owner));
        let has_foreign_resume = self.foreign_resume_hint().is_some();
        let sp_loading = crate::views::session_picker::loading_spinner_active(
            self.session_picker_entries.as_deref(),
            self.session_picker_source_filter,
            self.session_picker_loading,
            &self.session_picker_lanes,
        );
        #[cfg(feature = "local-workspace")]
        let session_picker_open = self.session_picker_entries.is_some() || sp_loading;
        let outcome = match self.active_view {
            ActiveView::Welcome => handle_welcome_input(
                ev,
                &mut WelcomeInputCtx {
                    auth_state: &self.auth_state,
                    trust_state: &self.trust_state,
                    consent_state: &self.consent_state,
                    consent_link_rects: &self.welcome_consent_link_rects,
                    consent_hover_link: &mut self.welcome_consent_hover_link,
                    arrived_at,
                    cwd: &self.cwd,
                    mid_session_login: self.auth_return_view.is_some(),
                    auth_code_input: &mut self.auth_code_input,
                    prompt: &mut self.welcome_prompt,
                    prompt_focused: &mut self.welcome_prompt_focused,
                    new_worktree_dialog: &mut self.new_worktree_dialog,
                    menu_index: &mut self.welcome_menu_index,
                    menu_rects: &self.welcome_menu_rects,
                    menu_count: if zdr_blocked {
                        2
                    } else {
                        3 + if self.has_claude_import { 1 } else { 0 }
                            + if self.welcome_show_changelog_action {
                                1
                            } else {
                                0
                            }
                    },
                    prompt_rect: self.welcome_prompt_rect.as_ref(),
                    import_banner_rect: self.welcome_import_banner_rect.as_ref(),
                    auth_url_rect: self.welcome_auth_url_rect.as_ref(),
                    auth_fallback_rect: self.welcome_auth_fallback_rect.as_ref(),
                    refresh_rect: self.welcome_refresh_rect.as_ref(),
                    gate_url_rect: self.welcome_gate_url_rect.as_ref(),
                    upgrade_cta_rect: self.welcome_upgrade_cta_rect.as_ref(),
                    privacy_banner_opt_in_rect: self.welcome_privacy_banner_opt_in_rect.as_ref(),
                    privacy_banner_opt_out_rect: self.welcome_privacy_banner_opt_out_rect.as_ref(),
                    privacy_banner_terms_rect: self.welcome_privacy_banner_terms_rect.as_ref(),
                    privacy_banner_policy_rect: self.welcome_privacy_banner_policy_rect.as_ref(),
                    on_privacy_banner: &mut self.welcome_on_privacy_banner,
                    on_upgrade_cta: &mut self.welcome_on_upgrade_cta,
                    upgrade_cta_keyboard: welcome_pinned_upgrade_cta,
                    changelog_cta_rect: self.welcome_changelog_cta_rect.as_ref(),
                    on_changelog_cta: &mut self.welcome_on_changelog_cta,
                    announcement_truncated: self.welcome_announcement.truncated,
                    announcement_rect: self.welcome_announcement.rect.as_ref(),
                    on_announcement_cta: &mut self.welcome_announcement.on_cta,
                    announcement_expanded: &mut self.welcome_announcement.expanded,
                    show_raw_url: &mut self.auth_show_raw_url,
                    has_access,
                    is_zdr_blocked: zdr_blocked,
                    sp_entries: &mut self.session_picker_entries,
                    sp_loading,
                    sp_state: &mut self.session_picker_state,
                    sp_content_results: &self.session_picker_content_results,
                    sp_content_loading: self.session_picker_content_loading,
                    sp_entries_query: &self.session_picker_entries_query,
                    has_claude_import: self.has_claude_import,
                    import_claude_modal: &mut self.import_claude_modal,
                    welcome_doc_viewer: &mut self.welcome_doc_viewer,
                    changelog_markdown: &self.changelog_markdown,
                    show_changelog_action: self.welcome_show_changelog_action,
                    has_pending_update: self.pending_update_version.is_some(),
                    has_foreign_resume,
                    cwd_has_git_ancestor: self.cwd_has_git_ancestor,
                    session_picker_grouped: self.session_picker_grouped,
                    sp_source_filter: &mut self.session_picker_source_filter,
                    sp_pending_delete: &mut self.session_picker_pending_delete,
                    chat_mode: self.chat_mode,
                    #[cfg(feature = "local-workspace")]
                    workspace_mode: &mut self.welcome_workspace_mode,
                    #[cfg(feature = "local-workspace")]
                    workspace_mode_rects: &self.welcome_workspace_mode_rects,
                    #[cfg(feature = "local-workspace")]
                    on_workspace_mode: &mut self.welcome_on_workspace_mode,
                    #[cfg(feature = "local-workspace")]
                    workspace_mode_startup_locked: self.local_workspace_startup_locked,
                    #[cfg(feature = "local-workspace")]
                    workspace_mode_ack_pending: &mut self.welcome_local_workspace_ack_pending,
                    #[cfg(feature = "local-workspace")]
                    history_load_as_build: &mut self.welcome_history_load_as_build,
                    #[cfg(feature = "local-workspace")]
                    deferred_startup: &mut self.deferred_startup,
                    #[cfg(feature = "local-workspace")]
                    session_picker_open,
                },
            ),
            ActiveView::Agent(id) => {
                let overlay_active = self
                    .dashboard
                    .as_ref()
                    .is_some_and(|d| d.attached_agent == Some(id));
                if !overlay_active
                    && let Event::Key(key) = ev
                    && key.kind != KeyEventKind::Release
                {
                    match self
                        .registry
                        .lookup(key, crate::actions::When::DashboardOverlay)
                    {
                        Some(crate::actions::ActionId::DashboardOverlayPrev) => {
                            return InputOutcome::Action(Action::DashboardOverlayPrev);
                        }
                        Some(crate::actions::ActionId::DashboardOverlayNext) => {
                            return InputOutcome::Action(Action::DashboardOverlayNext);
                        }
                        _ => {}
                    }
                }
                if overlay_active {
                    if let Event::Key(key) = ev
                        && key.kind != KeyEventKind::Release
                    {
                        let lookup = self
                            .registry
                            .lookup(key, crate::actions::When::DashboardOverlay)
                            .or_else(|| self.registry.lookup(key, crate::actions::When::Always));
                        match lookup {
                            Some(crate::actions::ActionId::OpenDashboard)
                            | Some(crate::actions::ActionId::DashboardOverlayExit) => {
                                return InputOutcome::Action(Action::DashboardOverlayExit);
                            }
                            Some(crate::actions::ActionId::DashboardOverlayPrev) => {
                                return InputOutcome::Action(Action::DashboardOverlayPrev);
                            }
                            Some(crate::actions::ActionId::DashboardOverlayNext) => {
                                return InputOutcome::Action(Action::DashboardOverlayNext);
                            }
                            Some(crate::actions::ActionId::DashboardOverlayStop) => {
                                if let Some(agent) = self.agents.get_mut(&id)
                                    && agent.arm_dashboard_stop()
                                {
                                    return InputOutcome::Action(Action::CancelTurn);
                                }
                                self.pending_action = Some(PendingAction::with_ttl(
                                    Action::DashboardOverlayStop,
                                    KeyShortcut::from(*key),
                                    Some("close this session"),
                                    crate::views::dashboard::state::CONFIRM_WINDOW,
                                ));
                                return InputOutcome::Changed;
                            }
                            _ => {}
                        }
                        if key.code == KeyCode::Left
                            && key.modifiers.is_empty()
                            && self
                                .agents
                                .get(&id)
                                .is_some_and(|a| a.is_empty_focused_prompt())
                        {
                            return InputOutcome::Action(Action::DashboardOverlayExit);
                        }
                        if key.code == KeyCode::Esc
                            && key.modifiers.is_empty()
                            && self
                                .agents
                                .get(&id)
                                .is_some_and(|a| a.overlay_esc_backs_out_from_prompt())
                        {
                            return InputOutcome::Action(Action::DashboardOverlayExit);
                        }
                        if key.modifiers.is_empty()
                            && self.agents.get(&id).is_some_and(|a| match key.code {
                                KeyCode::Esc => a.overlay_esc_backs_out(),
                                KeyCode::Left => a.overlay_left_backs_out(),
                                _ => false,
                            })
                        {
                            return InputOutcome::Action(Action::DashboardOverlayExit);
                        }
                        let neutral = self.agents.get(&id).is_some_and(|a| {
                            a.is_bare_scrollback() && a.no_input_overlay_pending()
                        });
                        if key.code == KeyCode::Char('q') && key.modifiers.is_empty() && neutral {
                            return InputOutcome::Action(Action::DashboardOverlayExit);
                        }
                        if key.code == KeyCode::Esc
                            && key.modifiers.is_empty()
                            && neutral
                            && self.agents.get(&id).is_some_and(|a| {
                                a.no_esc_consumer_pending()
                                    && !a.session.state.is_turn_running()
                                    && !a.session.state.is_cancelling()
                                    && !a.wake_turn_active()
                            })
                        {
                            return InputOutcome::Action(Action::DashboardOverlayExit);
                        }
                    }
                    if let Event::Mouse(mouse) = ev {
                        use crossterm::event::{MouseButton, MouseEventKind};
                        match mouse.kind {
                            MouseEventKind::Moved => {
                                let mut changed = false;
                                if let Some(d) = self.dashboard.as_mut() {
                                    changed |=
                                        d.overlay_close_hit.update_hover(mouse.column, mouse.row);
                                    changed |=
                                        d.overlay_prev_hit.update_hover(mouse.column, mouse.row);
                                    changed |=
                                        d.overlay_next_hit.update_hover(mouse.column, mouse.row);
                                }
                                if changed {
                                    return InputOutcome::Changed;
                                }
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some(d) = self.dashboard.as_ref() {
                                    if d.overlay_close_hit.contains(mouse.column, mouse.row) {
                                        return InputOutcome::Action(Action::DashboardOverlayExit);
                                    }
                                    if d.overlay_prev_hit.contains(mouse.column, mouse.row) {
                                        return InputOutcome::Action(Action::DashboardOverlayPrev);
                                    }
                                    if d.overlay_next_hit.contains(mouse.column, mouse.row) {
                                        return InputOutcome::Action(Action::DashboardOverlayNext);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(modal) = self.import_claude_modal.as_mut() {
                    use crate::views::import_claude_modal::ImportClaudeModalOutcome;
                    let outcome_to_input = |o: ImportClaudeModalOutcome| match o {
                        ImportClaudeModalOutcome::Confirmed => {
                            InputOutcome::Action(Action::ImportClaudeConfirm)
                        }
                        ImportClaudeModalOutcome::Cancelled => {
                            InputOutcome::Action(Action::ImportClaudeCancel)
                        }
                        ImportClaudeModalOutcome::Changed => InputOutcome::Changed,
                        ImportClaudeModalOutcome::Unchanged => InputOutcome::Unchanged,
                    };
                    if let Event::Key(key) = ev {
                        if key.kind == KeyEventKind::Release {
                            return InputOutcome::Unchanged;
                        }
                        return outcome_to_input(modal.handle_key(key));
                    }
                    if let Event::Mouse(mouse) = ev {
                        return outcome_to_input(modal.handle_mouse(
                            mouse.kind,
                            mouse.column,
                            mouse.row,
                        ));
                    }
                    return InputOutcome::Unchanged;
                }
                if let Some(outcome) = self.voice_esc_outcome(key_event) {
                    return outcome;
                }
                if let Event::Key(key) = ev
                    && key.kind != KeyEventKind::Release
                {
                    self.maybe_commit_voice_interim_before_submit_key(key);
                }
                if self.screen_mode.is_minimal()
                    && let Event::Key(key) = ev
                    && key.kind != KeyEventKind::Release
                    && let Some(outcome) = self.minimal_key_intercept(key)
                {
                    return outcome;
                }
                let prompt_paging = !overlay_active && !self.screen_mode.is_minimal();
                let outcome = match self.agents.get_mut(&id) {
                    Some(agent) => {
                        let transcript_before = agent.active_subagent.clone();
                        let workflows_before = agent.show_workflows;
                        let outcome = if self.screen_mode.is_minimal() {
                            agent.handle_minimal_input(ev, &self.registry)
                        } else if prompt_paging {
                            agent.handle_input_with_prompt_paging(ev, &self.registry)
                        } else {
                            agent.handle_input(ev, &self.registry)
                        };
                        let transcript_opened =
                            transcript_before.is_none() && agent.active_subagent.is_some();
                        let workflows_opened = !workflows_before && agent.show_workflows;
                        if let Event::Key(key) = ev {
                            agent.record_input(key, &outcome);
                        }
                        self.pending_effects.append(&mut agent.pending_effects);
                        if transcript_opened || workflows_opened {
                            self.scroll_state.cancel_stream();
                            self.last_scroll_pos = None;
                        }
                        outcome
                    }
                    None => InputOutcome::Unchanged,
                };
                if self.pending_editor.is_some()
                    && matches!(outcome, InputOutcome::Action(Action::EditPromptExternal))
                {
                    InputOutcome::Unchanged
                } else {
                    outcome
                }
            }
            ActiveView::AgentDashboard => {
                if let Some(outcome) = self.voice_esc_outcome(key_event) {
                    return outcome;
                }
                if let Event::Key(key) = ev
                    && key.kind != KeyEventKind::Release
                {
                    self.maybe_commit_voice_interim_before_submit_key(key);
                }
                let attached_raw = self.dashboard.as_ref().and_then(|d| d.attached_agent);
                let attached = attached_raw.filter(|id| self.agents.contains_key(id));
                if attached_raw.is_some()
                    && attached.is_none()
                    && let Some(d) = self.dashboard.as_mut()
                {
                    d.close_popup();
                }
                if let Some(agent_id) = attached {
                    if let Event::Key(key) = ev
                        && key.kind != KeyEventKind::Release
                    {
                        let close_via_esc = key.code == KeyCode::Esc;
                        let close_via_action = self
                            .registry
                            .lookup(key, crate::actions::When::Always)
                            .is_some_and(|id| {
                                matches!(
                                    id,
                                    crate::actions::ActionId::OpenDashboard
                                        | crate::actions::ActionId::DashboardExit
                                )
                            });
                        if close_via_action || close_via_esc {
                            if let Some(d) = self.dashboard.as_mut() {
                                d.close_popup();
                            }
                            if let Some(agent) = self.agents.get_mut(&agent_id) {
                                agent.close_subagent_fullscreen();
                            }
                            return InputOutcome::Changed;
                        }
                    }
                    if let Event::Mouse(mouse) = ev
                        && matches!(
                            mouse.kind,
                            crossterm::event::MouseEventKind::Down(
                                crossterm::event::MouseButton::Left
                            )
                        )
                    {
                        let (close_rect, outer_rect, row_target) = {
                            let dash = self.dashboard.as_ref();
                            let close_rect = dash.and_then(|d| d.popup_close_rect);
                            let outer_rect = dash.and_then(|d| d.popup_outer_rect);
                            let row_target = dash.and_then(|d| {
                                d.row_rects
                                    .iter()
                                    .find(|(_, r)| {
                                        mouse.column >= r.x
                                            && mouse.column < r.x + r.width
                                            && mouse.row >= r.y
                                            && mouse.row < r.y + r.height
                                    })
                                    .map(|(id, _)| id.clone())
                            });
                            (close_rect, outer_rect, row_target)
                        };
                        let in_close = close_rect.is_some_and(|r| {
                            mouse.column >= r.x
                                && mouse.column < r.x + r.width
                                && mouse.row >= r.y
                                && mouse.row < r.y + r.height
                        });
                        let in_outer = outer_rect.is_some_and(|r| {
                            mouse.column >= r.x
                                && mouse.column < r.x + r.width
                                && mouse.row >= r.y
                                && mouse.row < r.y + r.height
                        });
                        if in_close {
                            if let Some(d) = self.dashboard.as_mut() {
                                d.close_popup();
                            }
                            if let Some(agent) = self.agents.get_mut(&agent_id) {
                                agent.close_subagent_fullscreen();
                            }
                            return InputOutcome::Changed;
                        }
                        if !in_outer && let Some(target) = row_target {
                            return InputOutcome::Action(Action::DashboardAttach(target));
                        }
                        if !in_outer {
                            return InputOutcome::Unchanged;
                        }
                    }
                    match self.agents.get_mut(&agent_id) {
                        Some(agent) => {
                            let transcript_before = agent.active_subagent.clone();
                            let workflows_before = agent.show_workflows;
                            let outcome = agent.handle_input(ev, &self.registry);
                            let transcript_opened =
                                transcript_before.is_none() && agent.active_subagent.is_some();
                            let workflows_opened = !workflows_before && agent.show_workflows;
                            if let Event::Key(key) = ev {
                                agent.record_input(key, &outcome);
                            }
                            self.pending_effects.append(&mut agent.pending_effects);
                            if transcript_opened || workflows_opened {
                                self.scroll_state.cancel_stream();
                                self.last_scroll_pos = None;
                            }
                            if matches!(outcome, InputOutcome::Action(Action::ExitSession)) {
                                if let Some(d) = self.dashboard.as_mut() {
                                    d.close_popup();
                                }
                                if let Some(agent) = self.agents.get_mut(&agent_id) {
                                    agent.close_subagent_fullscreen();
                                }
                                return InputOutcome::Changed;
                            }
                            outcome
                        }
                        None => InputOutcome::Unchanged,
                    }
                } else if let Some(ref mut dashboard) = self.dashboard {
                    let outcome = dashboard.handle_input_with_paste_provenance(
                        ev,
                        &self.registry,
                        paste_provenance,
                    );
                    self.pending_effects.append(&mut dashboard.pending_effects);
                    outcome
                } else {
                    InputOutcome::Unchanged
                }
            }
        };
        if let InputOutcome::Action(Action::Quit) = &outcome {
            return self.apply_quit_confirmation(key_event);
        }
        if let InputOutcome::Action(Action::QuitConfirmed) = &outcome {
            return InputOutcome::Action(Action::Quit);
        }
        if let InputOutcome::Action(Action::ExitSessionConfirmed) = &outcome {
            return InputOutcome::Action(Action::ExitSession);
        }
        if let InputOutcome::ArmPending {
            action,
            shortcut,
            label,
            ttl,
        } = outcome
        {
            self.pending_action = Some(PendingAction::with_ttl(action, shortcut, label, ttl));
            return InputOutcome::Changed;
        }
        if let InputOutcome::Action(Action::ExitSession) = &outcome
            && matches!(self.active_view, ActiveView::Agent(_))
        {
            return self.apply_exit_session_confirmation(key_event);
        }
        if matches!(
            outcome,
            InputOutcome::Action(Action::NewSession | Action::NewWorktreeSession { .. })
        ) && matches!(self.active_view, ActiveView::Agent(_))
            && let Some(key) = key_event
        {
            let (action, action_id) = match &outcome {
                InputOutcome::Action(Action::NewSession) => {
                    (Action::NewSession, ActionId::NewSession)
                }
                InputOutcome::Action(Action::NewWorktreeSession {
                    load_session_id,
                    label,
                    git_ref,
                }) => {
                    let action = Action::NewWorktreeSession {
                        load_session_id: load_session_id.clone(),
                        label: label.clone(),
                        git_ref: git_ref.clone(),
                    };
                    let shortcut = KeyShortcut::from(*key);
                    self.pending_action =
                        Some(PendingAction::new(action, shortcut, "new in worktree"));
                    return InputOutcome::Changed;
                }
                _ => unreachable!(),
            };
            if let Some(def) = self.registry.find(action_id)
                && def.requires_confirmation
            {
                let shortcut = if def.default_key.matches(key)
                    || def.alt_keys.iter().any(|alt| alt.matches(key))
                {
                    KeyShortcut::from(*key)
                } else {
                    def.default_key
                };
                self.pending_action = Some(PendingAction::new(action, shortcut, def.label));
                return InputOutcome::Changed;
            }
        }
        if !matches!(outcome, InputOutcome::Unchanged) {
            return outcome;
        }
        if matches!(ev, Event::Resize(_, _)) {
            return InputOutcome::Changed;
        }
        if let Some(key) = key_event
            && let Some(action_id) = self.registry.lookup(key, When::Always)
        {
            return self.handle_global_action(action_id, key);
        }
        if let Some(key) = key_event
            && (key!('c', CONTROL).matches(key) || key!('d', CONTROL).matches(key))
            && matches!(
                self.active_view,
                ActiveView::Agent(_) | ActiveView::AgentDashboard
            )
        {
            self.pending_action = Some(PendingAction::new(
                Action::Quit,
                KeyShortcut::from(*key),
                "quit",
            ));
            return InputOutcome::Changed;
        }
        InputOutcome::Unchanged
    }
    /// Handle a global-level action. Applies confirmation if required.
    fn handle_global_action(
        &mut self,
        action_id: ActionId,
        key: &crossterm::event::KeyEvent,
    ) -> InputOutcome {
        let Some(def) = self.registry.find(action_id) else {
            return InputOutcome::Unchanged;
        };
        let action = match action_id {
            ActionId::Quit => Action::Quit,
            ActionId::NewSession => Action::NewSession,
            ActionId::NewSessionInWorktree => Action::NewWorktreeSession {
                load_session_id: None,
                label: None,
                git_ref: None,
            },
            ActionId::OpenDashboard => Action::OpenDashboard,
            ActionId::VoiceToggle => {
                if !self.current_ui.voice_keybind_enabled.unwrap_or(true) {
                    return InputOutcome::Unchanged;
                }
                Action::VoiceToggle
            }
            _ => return InputOutcome::Unchanged,
        };
        if def.requires_confirmation {
            let shortcut = KeyShortcut::from(*key);
            let action = if action_id == ActionId::NewSession
                && matches!(self.active_view, ActiveView::Agent(_))
                && self.new_session_worktree_mode == WorktreeMode::Ask
            {
                Action::ChooseNewSessionMode
            } else {
                action
            };
            self.pending_action = Some(PendingAction::new(action, shortcut, def.label));
            InputOutcome::Changed
        } else {
            InputOutcome::Action(action)
        }
    }
    /// Apply quit confirmation (double-press). Used both for direct global
    /// quit and for escalated quit from agent-level cancel.
    fn apply_quit_confirmation(
        &mut self,
        key_event: Option<&crossterm::event::KeyEvent>,
    ) -> InputOutcome {
        let Some(key) = key_event else {
            return InputOutcome::Action(Action::Quit);
        };
        let Some(def) = self.registry.find(ActionId::Quit) else {
            return InputOutcome::Action(Action::Quit);
        };
        if def.requires_confirmation {
            let shortcut = KeyShortcut::from(*key);
            self.pending_action = Some(PendingAction::new(Action::Quit, shortcut, def.label));
            InputOutcome::Changed
        } else {
            InputOutcome::Action(Action::Quit)
        }
    }
    /// Apply exit-session confirmation (double-press). Works like quit confirmation
    /// but transitions to the welcome screen instead of quitting.
    fn apply_exit_session_confirmation(
        &mut self,
        key_event: Option<&crossterm::event::KeyEvent>,
    ) -> InputOutcome {
        let Some(key) = key_event else {
            return InputOutcome::Action(Action::ExitSession);
        };
        let Some(def) = self.registry.find(ActionId::ExitSession) else {
            return InputOutcome::Action(Action::ExitSession);
        };
        if def.requires_confirmation {
            let shortcut = KeyShortcut::from(*key);
            self.pending_action =
                Some(PendingAction::new(Action::ExitSession, shortcut, def.label));
            InputOutcome::Changed
        } else {
            InputOutcome::Action(Action::ExitSession)
        }
    }
}
pub(crate) use crate::views::session_picker::filter_session_entries;
use crate::views::session_picker::{
    CONTENT_EXPAND_OFFSET, PickerItem, SessionPickerWorktreeSelection, build_entry_map,
    session_picker_worktree_selection, sync_session_picker_query_expansion,
};
/// Context for welcome-view input handling.
struct WelcomeInputCtx<'a> {
    auth_state: &'a AuthState,
    /// Folder-trust state. When `Pending` (and auth is `Done`), the trust
    /// question intercepts keys and swallows the rest so no session starts.
    trust_state: &'a TrustState,
    consent_state: &'a ConsentState,
    consent_link_rects: &'a [(usize, ratatui::layout::Rect)],
    consent_hover_link: &'a mut Option<usize>,
    /// When this event reached the process, so a key typed before the notice painted is no answer.
    arrived_at: Instant,
    /// Live working directory (tracks `Effect::SetWorkingDir`), used to pin
    /// the current repo's group to the top of the session picker.
    cwd: &'a std::path::Path,
    /// `true` when the welcome screen is showing only to host a login flow
    /// that was started from inside a session. Esc / `q` then cancel the
    /// login and return to the session rather than quitting the app.
    mid_session_login: bool,
    auth_code_input: &'a mut LineEditor,
    prompt: &'a mut PromptWidget,
    prompt_focused: &'a mut bool,
    new_worktree_dialog: &'a mut Option<NewWorktreeDialogState>,
    menu_index: &'a mut Option<usize>,
    menu_rects: &'a [ratatui::layout::Rect],
    menu_count: usize,
    prompt_rect: Option<&'a ratatui::layout::Rect>,
    import_banner_rect: Option<&'a ratatui::layout::Rect>,
    auth_url_rect: Option<&'a ratatui::layout::Rect>,
    auth_fallback_rect: Option<&'a ratatui::layout::Rect>,
    refresh_rect: Option<&'a ratatui::layout::Rect>,
    gate_url_rect: Option<&'a ratatui::layout::Rect>,
    /// Hit-test rect for the welcome hero upgrade CTA `[label]` button
    /// (click → open the promo url).
    upgrade_cta_rect: Option<&'a ratatui::layout::Rect>,
    privacy_banner_opt_in_rect: Option<&'a ratatui::layout::Rect>,
    privacy_banner_opt_out_rect: Option<&'a ratatui::layout::Rect>,
    privacy_banner_terms_rect: Option<&'a ratatui::layout::Rect>,
    privacy_banner_policy_rect: Option<&'a ratatui::layout::Rect>,
    /// Sticky hover flag for the privacy banner buttons (redraw on
    /// enter/leave/crossing so they brighten/dim).
    on_privacy_banner: &'a mut bool,
    /// Sticky hover flag for the upgrade CTA (redraw on enter/leave so the
    /// button brightens/dims).
    on_upgrade_cta: &'a mut bool,
    /// A pinned (non-dismissible) promo CTA is live, so `Ctrl+O` opens it
    /// (the welcome screen has no YOLO toggle to preserve).
    upgrade_cta_keyboard: bool,
    /// Hit-test rect for the clickable changelog info block (opens release notes).
    changelog_cta_rect: Option<&'a ratatui::layout::Rect>,
    /// Sticky hover flag for the changelog block (redraw on enter/leave).
    on_changelog_cta: &'a mut bool,
    /// Whether the announcement overflowed — the "expandable" signal for click-to-toggle.
    announcement_truncated: bool,
    /// Hit-test rect for the full announcement block (click anywhere to toggle).
    announcement_rect: Option<&'a ratatui::layout::Rect>,
    /// Sticky hover flag for the announcement block (redraw on enter/leave).
    on_announcement_cta: &'a mut bool,
    /// Whether the long announcement is currently expanded inline.
    announcement_expanded: &'a mut bool,
    show_raw_url: &'a mut bool,
    has_access: bool,
    is_zdr_blocked: bool,
    sp_entries: &'a mut Option<Vec<SessionPickerEntry>>,
    /// Mirrors the render's `session_picker_loading` param: the spinner-only
    /// picker still owns input (Esc must dismiss it, not hit the hidden menu).
    sp_loading: bool,
    sp_state: &'a mut crate::views::picker::PickerState,
    sp_content_results:
        &'a Option<Vec<echo_agent_runtime::extensions::session_search::SearchSessionHit>>,
    sp_content_loading: bool,
    /// The query `sp_entries` were server-fetched with (see
    /// [`crate::views::session_picker::effective_filter_query`]).
    sp_entries_query: &'a Option<String>,
    has_claude_import: bool,
    import_claude_modal: &'a mut Option<crate::views::import_claude_modal::ImportClaudeModalState>,
    welcome_doc_viewer: &'a mut Option<crate::views::modal::ActiveModal>,
    changelog_markdown: &'a Option<String>,
    /// Whether the welcome menu currently includes a "Changelog" row (above
    /// Quit), so index→action mapping accounts for it.
    show_changelog_action: bool,
    has_pending_update: bool,
    /// A recent foreign session is available to resume when no update is pending.
    has_foreign_resume: bool,
    cwd_has_git_ancestor: bool,
    session_picker_grouped: bool,
    sp_source_filter: &'a mut crate::views::session_picker::SourceFilter,
    sp_pending_delete: &'a mut Option<crate::views::session_picker::PendingDelete>,
    /// Process-wide `--chat`: the session picker hides its source filter
    /// (conversations-only list), so `f` must not cycle it.
    chat_mode: bool,
    #[cfg(feature = "local-workspace")]
    workspace_mode: &'a mut crate::views::welcome::WelcomeWorkspaceMode,
    #[cfg(feature = "local-workspace")]
    workspace_mode_rects: &'a crate::views::welcome::WorkspaceModeHitRects,
    #[cfg(feature = "local-workspace")]
    on_workspace_mode: &'a mut bool,
    #[cfg(feature = "local-workspace")]
    workspace_mode_startup_locked: bool,
    #[cfg(feature = "local-workspace")]
    workspace_mode_ack_pending: &'a mut bool,
    #[cfg(feature = "local-workspace")]
    history_load_as_build: &'a mut bool,
    #[cfg(feature = "local-workspace")]
    deferred_startup: &'a mut crate::app::session_startup::DeferredStartupActions,
    #[cfg(feature = "local-workspace")]
    session_picker_open: bool,
}
/// Welcome view input -- auth-state-aware routing.
fn handle_welcome_input(ev: &Event, ctx: &mut WelcomeInputCtx<'_>) -> InputOutcome {
    if let Some(modal) = ctx.import_claude_modal.as_mut() {
        use crate::views::import_claude_modal::ImportClaudeModalOutcome;
        let outcome_to_input = |o: ImportClaudeModalOutcome| match o {
            ImportClaudeModalOutcome::Confirmed => {
                InputOutcome::Action(Action::ImportClaudeConfirm)
            }
            ImportClaudeModalOutcome::Cancelled => InputOutcome::Action(Action::ImportClaudeCancel),
            ImportClaudeModalOutcome::Changed => InputOutcome::Changed,
            ImportClaudeModalOutcome::Unchanged => InputOutcome::Unchanged,
        };
        if let Event::Key(key) = ev {
            if key.kind == crossterm::event::KeyEventKind::Release {
                return InputOutcome::Unchanged;
            }
            return outcome_to_input(modal.handle_key(key));
        }
        if let Event::Mouse(mouse) = ev {
            return outcome_to_input(modal.handle_mouse(mouse.kind, mouse.column, mouse.row));
        }
        return InputOutcome::Unchanged;
    }
    if let Some(modal) = ctx.welcome_doc_viewer {
        if let Event::Key(key) = ev {
            if key.kind == crossterm::event::KeyEventKind::Release {
                return InputOutcome::Unchanged;
            }
            use crate::views::modal_window as mw;
            if let crate::views::modal::ActiveModal::DocViewer { window, scroll, .. } = modal {
                let chrome_cfg = mw::ModalWindowConfig {
                    title: "",
                    tabs: None,
                    shortcuts: &[],
                    sizing: mw::ModalSizing::default(),
                    fold_info: None,
                };
                match mw::handle_modal_key(window, key, &chrome_cfg) {
                    mw::ModalWindowOutcome::CloseRequested => {
                        *ctx.welcome_doc_viewer = None;
                        return InputOutcome::Changed;
                    }
                    mw::ModalWindowOutcome::Unhandled => {
                        if crate::views::modal::apply_doc_scroll(key.code, scroll) {
                            return InputOutcome::Changed;
                        }
                        return InputOutcome::Unchanged;
                    }
                    _ => return InputOutcome::Changed,
                }
            }
        }
        if let Event::Mouse(mouse) = ev {
            use crate::views::modal_window as mw;
            if let crate::views::modal::ActiveModal::DocViewer { window, scroll, .. } = modal {
                match mw::handle_modal_mouse(window, mouse.kind, mouse.column, mouse.row) {
                    mw::ModalWindowOutcome::CloseRequested => {
                        *ctx.welcome_doc_viewer = None;
                        return InputOutcome::Changed;
                    }
                    mw::ModalWindowOutcome::Unhandled => {
                        if crate::views::modal::apply_doc_mouse_scroll(mouse.kind, scroll) {
                            return InputOutcome::Changed;
                        }
                    }
                    _ => return InputOutcome::Changed,
                }
            }
        }
        return InputOutcome::Unchanged;
    }
    if let Some(dialog) = ctx.new_worktree_dialog.as_mut() {
        let outcome = match ev {
            Event::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                dialog.handle_key(key)
            }
            Event::Paste(text) => dialog.insert_paste(text),
            Event::Resize(_, _) => return InputOutcome::Changed,
            _ => NewWorktreeDialogOutcome::Unchanged,
        };
        match outcome {
            NewWorktreeDialogOutcome::Submitted(label) => {
                *ctx.new_worktree_dialog = None;
                return InputOutcome::Action(Action::NewWorktreeSession {
                    load_session_id: None,
                    label,
                    git_ref: None,
                });
            }
            NewWorktreeDialogOutcome::Cancelled => {
                *ctx.new_worktree_dialog = None;
                return InputOutcome::Changed;
            }
            NewWorktreeDialogOutcome::Changed => return InputOutcome::Changed,
            NewWorktreeDialogOutcome::Unchanged => return InputOutcome::Unchanged,
        }
    }
    if matches!(ctx.auth_state, AuthState::Done)
        && ctx.has_access
        && !ctx.is_zdr_blocked
        && matches!(ctx.consent_state, ConsentState::Pending { .. })
    {
        return crate::app::consent::handle_answer(
            ev,
            &mut crate::app::consent::ConsentInputCtx {
                state: ctx.consent_state,
                arrived_at: ctx.arrived_at,
                menu_rects: ctx.menu_rects,
                link_rects: ctx.consent_link_rects,
                menu_index: ctx.menu_index,
                hover_link: ctx.consent_hover_link,
            },
        );
    }
    if matches!(ctx.auth_state, AuthState::Done)
        && ctx.has_access
        && !ctx.is_zdr_blocked
        && matches!(ctx.trust_state, TrustState::Pending { .. })
    {
        if let Event::Key(key) = ev {
            if key.kind == KeyEventKind::Release {
                return InputOutcome::Unchanged;
            }
            if key!('y').matches(key) || key!('Y').matches(key) || key!(Enter).matches(key) {
                return InputOutcome::Action(Action::TrustFolder);
            }
            if key!('n').matches(key) || key!('N').matches(key) || key!(Esc).matches(key) {
                return InputOutcome::Action(Action::QuitConfirmed);
            }
            if key!('c', CONTROL).matches(key) || key!('d', CONTROL).matches(key) {
                return InputOutcome::Action(Action::Quit);
            }
            return InputOutcome::Unchanged;
        }
        if let Event::Mouse(mouse) = ev
            && matches!(
                mouse.kind,
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            )
        {
            for (i, rect) in ctx.menu_rects.iter().enumerate() {
                if rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row)) {
                    return InputOutcome::Action(if i == 0 {
                        Action::TrustFolder
                    } else {
                        Action::QuitConfirmed
                    });
                }
            }
            return InputOutcome::Unchanged;
        }
        if matches!(ev, Event::Resize(_, _)) {
            return InputOutcome::Changed;
        }
        return InputOutcome::Unchanged;
    }
    #[cfg(feature = "local-workspace")]
    if *ctx.workspace_mode_ack_pending
        && matches!(ctx.auth_state, AuthState::Done)
        && ctx.has_access
        && !ctx.is_zdr_blocked
    {
        if let Event::Key(key) = ev {
            if key.kind == KeyEventKind::Release {
                return InputOutcome::Unchanged;
            }
            if key!('y').matches(key) || key!('Y').matches(key) || key!(Enter).matches(key) {
                return InputOutcome::Action(Action::ConfirmWelcomeLocalWorkspaceAck);
            }
            if key!('n').matches(key) || key!('N').matches(key) || key!(Esc).matches(key) {
                *ctx.workspace_mode_ack_pending = false;
                *ctx.workspace_mode = crate::views::welcome::WelcomeWorkspaceMode::Sandbox;
                let was_worktree = ctx.deferred_startup.worktree;
                ctx.deferred_startup.worktree = false;
                ctx.deferred_startup.worktree_label = None;
                ctx.deferred_startup.worktree_ref = None;
                if was_worktree {
                    ctx.deferred_startup.session = None;
                    ctx.deferred_startup.preferred_session_id = None;
                }
                *ctx.history_load_as_build = false;
                ctx.deferred_startup.history_load_as_build = false;
                crate::views::welcome::workspace_mode::log_welcome_ack("cancelled");
                return InputOutcome::Changed;
            }
            return InputOutcome::Unchanged;
        }
        if matches!(ev, Event::Resize(_, _)) {
            return InputOutcome::Changed;
        }
        return InputOutcome::Unchanged;
    }
    #[cfg(feature = "local-workspace")]
    if crate::views::welcome::workspace_mode::picker_interactive(
        ctx.chat_mode,
        ctx.has_access,
        matches!(ctx.auth_state, AuthState::Done),
        ctx.is_zdr_blocked,
        ctx.session_picker_open,
        ctx.workspace_mode_startup_locked,
    ) {
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key!('e', CONTROL).matches(key)
        {
            *ctx.workspace_mode = ctx.workspace_mode.cycle_next();
            crate::views::welcome::workspace_mode::log_welcome_mode_selected(
                *ctx.workspace_mode,
                "ctrl_e",
                ctx.workspace_mode_startup_locked,
            );
            return InputOutcome::Changed;
        }
        if let Event::Mouse(mouse) = ev
            && matches!(
                mouse.kind,
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            )
            && let Some(mode) = crate::views::welcome::hit_test_workspace_mode(
                ctx.workspace_mode_rects,
                mouse.column,
                mouse.row,
            )
        {
            *ctx.workspace_mode = mode;
            crate::views::welcome::workspace_mode::log_welcome_mode_selected(
                mode,
                "click",
                ctx.workspace_mode_startup_locked,
            );
            return InputOutcome::Changed;
        }
    }
    if (ctx.sp_entries.is_some() || ctx.sp_loading) && matches!(ctx.auth_state, AuthState::Done) {
        use crate::views::picker::{PickerConfig, PickerOutcome, handle_picker_input};
        let source_filter = *ctx.sp_source_filter;
        let current_repo =
            crate::views::session_picker::repo_name_from_cwd(&ctx.cwd.to_string_lossy());
        let entry_map = build_entry_map(
            ctx.sp_entries.as_deref(),
            ctx.sp_content_results.as_deref(),
            crate::views::session_picker::effective_filter_query(
                ctx.sp_state.query(),
                ctx.sp_entries_query.as_deref(),
            ),
            ctx.session_picker_grouped,
            ctx.sp_content_loading,
            source_filter,
            Some(current_repo.as_str()),
        );
        let entry_count = entry_map.len();
        let non_selectable_flags: Vec<bool> = entry_map.iter().map(|e| e.is_none()).collect();
        let focused_is_foreign = match entry_map
            .get(ctx.sp_state.selected)
            .and_then(|entry| entry.as_ref())
        {
            Some(PickerItem::Fuzzy { original_index }) => ctx
                .sp_entries
                .as_ref()
                .and_then(|entries| entries.get(*original_index))
                .is_some_and(|entry| {
                    crate::app::foreign_sessions::is_foreign_picker_source(&entry.source)
                }),
            _ => false,
        };
        let config = PickerConfig {
            title: Some("Resume session"),
            show_search_hint: true,
            expandable: true,
            esc_clears_query: true,
            shortcuts: Some(crate::views::picker::picker_shortcuts()),
            pending_hint: None,
            non_selectable: &non_selectable_flags,
            non_selectable_clickable: &[],
            shortcuts_area: None,
            tabs: None,
            active_tab: 0,
            filter_label: (!ctx.chat_mode).then(|| source_filter.label()),
            filter_key_hint: (!ctx.chat_mode).then_some("f"),
            filter_active: !ctx.chat_mode && source_filter.is_active(),
            header_note: None,
            action_keys: if ctx.chat_mode || focused_is_foreign {
                &[]
            } else {
                &[('d', "delete")]
            },
            disable_search: false,
            compact_bottom_bar: false,
            search_only_on_slash: false,
            vim_normal_first: crate::appearance::cache::load_vim_mode(),
        };
        match crate::views::session_picker::handle_pending_delete_key(ctx.sp_pending_delete, ev) {
            crate::views::session_picker::PendingDeleteKey::Confirm(pd) => {
                return InputOutcome::Action(Action::DeleteSession {
                    source: pd.source,
                    session_id: pd.session_id,
                    cwd: pd.cwd,
                });
            }
            crate::views::session_picker::PendingDeleteKey::Cancel => {
                return InputOutcome::Changed;
            }
            crate::views::session_picker::PendingDeleteKey::Disarmed
            | crate::views::session_picker::PendingDeleteKey::NotArmed => {}
        }
        if let Event::Key(key) = ev {
            if key.kind == KeyEventKind::Press
                && (key!('c', CONTROL).matches(key) || key!('d', CONTROL).matches(key))
            {
                return InputOutcome::Action(Action::Quit);
            }
            if let Some(selection) = session_picker_worktree_selection(
                key,
                ctx.sp_state,
                &entry_map,
                &non_selectable_flags,
                ctx.sp_entries.as_deref(),
                ctx.sp_content_results.as_deref(),
            ) {
                return InputOutcome::Action(match selection {
                    SessionPickerWorktreeSelection::Fuzzy(original_index) => {
                        Action::PickSessionInWorktree(original_index)
                    }
                    SessionPickerWorktreeSelection::Content { session_id, cwd } => {
                        Action::PickContentSessionInWorktree { session_id, cwd }
                    }
                    SessionPickerWorktreeSelection::Unavailable => {
                        return InputOutcome::Changed;
                    }
                });
            }
        }
        let selected_before = ctx.sp_state.selected;
        let outcome = handle_picker_input(ev, ctx.sp_state, entry_count, &config);
        if ctx.sp_pending_delete.is_some() && ctx.sp_state.selected != selected_before {
            *ctx.sp_pending_delete = None;
        }
        match outcome {
            PickerOutcome::Selected(i) => match entry_map.get(i).and_then(|e| e.as_ref()) {
                Some(PickerItem::Fuzzy { original_index }) => {
                    return InputOutcome::Action(Action::PickSession(*original_index));
                }
                Some(PickerItem::Content { hit_index }) => {
                    if let Some(hits) = ctx.sp_content_results.as_ref()
                        && let Some(hit) = hits.get(*hit_index)
                    {
                        return InputOutcome::Action(Action::PickContentSession {
                            session_id: hit.session_id.clone(),
                            cwd: hit.cwd.clone(),
                        });
                    }
                    return InputOutcome::Changed;
                }
                None => return InputOutcome::Changed,
            },
            PickerOutcome::SubmitQuery => {
                if let Some(sid) =
                    crate::views::session_picker::session_id_for_direct_load(ctx.sp_state.query())
                {
                    return InputOutcome::Action(Action::LoadSession(sid.to_string(), None, false));
                }
                return InputOutcome::Unchanged;
            }
            PickerOutcome::Closed => {
                *ctx.sp_entries = None;
                ctx.sp_state.reset();
                *ctx.sp_source_filter = crate::views::session_picker::SourceFilter::default();
                *ctx.sp_pending_delete = None;
                return InputOutcome::Action(Action::SessionPickerClosed);
            }
            PickerOutcome::Expand(i) => {
                match entry_map.get(i).and_then(|e| e.as_ref()) {
                    Some(PickerItem::Fuzzy { original_index }) => {
                        if let Some(ents) = ctx.sp_entries.as_ref()
                            && let Some(entry) = ents.get(*original_index)
                            && !crate::app::foreign_sessions::is_foreign_picker_source(
                                &entry.source,
                            )
                        {
                            return InputOutcome::Action(Action::ExpandSessionCard {
                                source: entry.source.clone(),
                                session_id: entry.id.clone(),
                            });
                        }
                    }
                    Some(PickerItem::Content { hit_index }) => {
                        if let Some(hits) = ctx.sp_content_results.as_ref()
                            && let Some(hit) = hits.get(*hit_index)
                        {
                            return InputOutcome::Action(Action::ExpandSessionCard {
                                source: "local".into(),
                                session_id: hit.session_id.clone(),
                            });
                        }
                    }
                    None => {}
                }
                return InputOutcome::Changed;
            }
            PickerOutcome::Collapse(i) => {
                match entry_map.get(i).and_then(|e| e.as_ref()) {
                    Some(PickerItem::Fuzzy { original_index }) => {
                        if ctx.sp_state.expanded.contains(original_index)
                            && let Some(ents) = ctx.sp_entries.as_ref()
                            && let Some(entry) = ents.get(*original_index)
                        {
                            return InputOutcome::Action(Action::ExpandSessionCard {
                                source: entry.source.clone(),
                                session_id: entry.id.clone(),
                            });
                        }
                    }
                    Some(PickerItem::Content { hit_index }) => {
                        let key = CONTENT_EXPAND_OFFSET + hit_index;
                        if ctx.sp_state.expanded.contains(&key)
                            && let Some(hits) = ctx.sp_content_results.as_ref()
                            && let Some(hit) = hits.get(*hit_index)
                        {
                            return InputOutcome::Action(Action::ExpandSessionCard {
                                source: "local".into(),
                                session_id: hit.session_id.clone(),
                            });
                        }
                    }
                    None => {}
                }
                return InputOutcome::Changed;
            }
            PickerOutcome::Copy(i) => {
                if let Some(Some(PickerItem::Fuzzy { original_index })) = entry_map.get(i) {
                    return InputOutcome::Action(Action::CopySessionId(*original_index));
                }
                return InputOutcome::Changed;
            }
            PickerOutcome::QueryChanged => {
                sync_session_picker_query_expansion(
                    ctx.sp_entries.as_deref(),
                    ctx.sp_content_results.as_deref(),
                    ctx.sp_entries_query.as_deref(),
                    ctx.sp_state,
                    ctx.session_picker_grouped,
                    ctx.sp_content_loading,
                    source_filter,
                    Some(current_repo.as_str()),
                );
                return InputOutcome::Action(Action::TriggerDeepSearch);
            }
            PickerOutcome::Changed => return InputOutcome::Changed,
            PickerOutcome::Unchanged => {
                if let Event::Key(key) = ev
                    && key.kind == KeyEventKind::Press
                    && key!('/', CONTROL).matches(key)
                    && !ctx.sp_state.query().trim().is_empty()
                {
                    return InputOutcome::Action(Action::ForceDeepSearch);
                }
                return InputOutcome::Unchanged;
            }
            PickerOutcome::FilterCycled => {
                return InputOutcome::Action(Action::CycleSessionSourceFilter);
            }
            PickerOutcome::Action('d') => {
                *ctx.sp_pending_delete =
                    crate::views::session_picker::pending_delete_from_selection(
                        ctx.sp_state.selected,
                        &entry_map,
                        ctx.sp_entries.as_deref(),
                        ctx.sp_content_results.as_deref(),
                    );
                return InputOutcome::Changed;
            }
            PickerOutcome::NonSelectableClick(_)
            | PickerOutcome::TabChanged(_)
            | PickerOutcome::Action(_) => {
                return InputOutcome::Changed;
            }
        }
    }
    if let Event::Key(key) = ev {
        if key.kind == KeyEventKind::Release {
            return InputOutcome::Unchanged;
        }
        if ctx.is_zdr_blocked && matches!(ctx.auth_state, AuthState::Done) {
            return handle_menu_shortcuts(
                key,
                ctx.menu_index,
                &['l', 'q'],
                dispatch_zdr_menu_action,
            );
        }
        if !ctx.has_access && matches!(ctx.auth_state, AuthState::Done) {
            return handle_menu_shortcuts(
                key,
                ctx.menu_index,
                &['g', 'l', 'q'],
                dispatch_access_gate_menu_action,
            );
        }
        if matches!(ctx.auth_state, AuthState::Done)
            && key!(Enter).matches(key)
            && key.modifiers.is_empty()
        {
            return InputOutcome::Action(Action::NewSession);
        }
        if matches!(ctx.auth_state, AuthState::Done) {
            if ctx.upgrade_cta_keyboard && key!('o', CONTROL).matches(key) {
                return InputOutcome::Action(Action::AnnouncementsOpenCta(
                    echo_agent_telemetry::events::AnnouncementCtaSurface::Keyboard,
                ));
            }
            if key!('w', CONTROL).matches(key) && ctx.cwd_has_git_ancestor {
                return InputOutcome::Action(Action::OpenNewWorktreeDialog);
            }
            if key!(F(3)).matches(key) {
                return InputOutcome::Action(Action::FetchSessionList);
            }
            if ctx.has_pending_update && key!('u', CONTROL).matches(key) {
                return InputOutcome::Action(Action::QuitForUpdate);
            }
            if ctx.has_foreign_resume && key!('u', CONTROL).matches(key) {
                return InputOutcome::Action(Action::ResumeForeignSession);
            }
            if ctx.has_claude_import && key!('i', CONTROL).matches(key) {
                return InputOutcome::Action(Action::ImportClaudeSettings);
            }
            if ctx.has_claude_import && key!('I', CONTROL | SHIFT).matches(key) {
                return InputOutcome::Action(Action::DismissClaudeImport);
            }
        }
        if matches!(ctx.auth_state, AuthState::Done) && crate::input::key::is_shift_tab(key) {
            return InputOutcome::ActionThenForward(Action::NewSession);
        }
        if *ctx.prompt_focused
            && matches!(ctx.auth_state, AuthState::Done)
            && let KeyCode::Char(ch) = key.code
            && (crate::input::key::is_text_input_key(key)
                || (ch == 'v' && crate::input::key::is_paste_key(key)))
        {
            return InputOutcome::ActionThenForward(Action::NewSession);
        }
        if *ctx.prompt_focused {
            let had_highlight = ctx.prompt.textarea.selection_range().is_some();
            match ctx.prompt.handle_key(key) {
                crate::views::prompt_widget::PromptEvent::Edited => {
                    return InputOutcome::Changed;
                }
                crate::views::prompt_widget::PromptEvent::Ignored => {
                    if key!(Esc).matches(key) {
                        *ctx.prompt_focused = false;
                        return InputOutcome::Changed;
                    }
                    if had_highlight && ctx.prompt.textarea.selection_range().is_none() {
                        return InputOutcome::Changed;
                    }
                }
            }
        }
        if !*ctx.prompt_focused && matches!(ctx.auth_state, AuthState::Done) {
            if let Some(outcome) = handle_menu_nav(key, ctx.menu_index, ctx.menu_count) {
                return outcome;
            }
            if key!(Enter).matches(key)
                && let Some(idx) = *ctx.menu_index
            {
                return dispatch_menu_action(
                    idx,
                    ctx.has_claude_import,
                    ctx.show_changelog_action,
                    ctx.changelog_markdown.as_deref(),
                );
            }
            if crate::input::key::is_text_input_key(key) {
                *ctx.prompt_focused = true;
                *ctx.menu_index = None;
                return InputOutcome::ActionThenForward(Action::NewSession);
            }
        }
        match ctx.auth_state {
            AuthState::Done => {
                if key!('c', CONTROL).matches(key) || key!('d', CONTROL).matches(key) {
                    return InputOutcome::Action(Action::Quit);
                }
            }
            AuthState::Pending { .. } => {
                if key!('q').matches(key)
                    || key!('c', CONTROL).matches(key)
                    || key!('d', CONTROL).matches(key)
                {
                    if ctx.mid_session_login {
                        return InputOutcome::Action(Action::CancelLogin);
                    }
                    return InputOutcome::Action(Action::QuitConfirmed);
                }
                if key!('l').matches(key) || key!(Enter).matches(key) {
                    return InputOutcome::Action(Action::Login);
                }
            }
            AuthState::Authenticating { .. } if *ctx.show_raw_url => {
                if key!('q', CONTROL).matches(key) || key!('c', CONTROL).matches(key) {
                    return InputOutcome::Action(Action::HideRawAuthUrl);
                }
                return InputOutcome::Unchanged;
            }
            AuthState::Authenticating {
                mode: AuthMode::Loopback,
                ..
            } => {
                if key!(Esc).matches(key)
                    || key!('q', CONTROL).matches(key)
                    || key!('c', CONTROL).matches(key)
                {
                    if ctx.mid_session_login {
                        return InputOutcome::Action(Action::CancelLogin);
                    }
                    return InputOutcome::Action(Action::QuitConfirmed);
                }
                if key!(Enter).matches(key) {
                    let trimmed = ctx.auth_code_input.text().trim().to_string();
                    if !trimmed.is_empty() {
                        return InputOutcome::Action(Action::SubmitAuthCode(trimmed));
                    }
                    return InputOutcome::Unchanged;
                }
                let outcome = if crate::input::key::is_paste_key(key) {
                    let Some(text) = crate::clipboard::system_clipboard_get() else {
                        return InputOutcome::Unchanged;
                    };
                    ctx.auth_code_input.insert_paste(&text)
                } else if key.modifiers.intersects(
                    crossterm::event::KeyModifiers::CONTROL
                        | crossterm::event::KeyModifiers::ALT
                        | crossterm::event::KeyModifiers::SUPER,
                ) && !crate::input::key::is_altgr(key.modifiers)
                {
                    return InputOutcome::Changed;
                } else {
                    ctx.auth_code_input
                        .handle_key_with_insert_policy(key, |character| !character.is_control())
                };
                return match outcome {
                    LineEditOutcome::TextChanged
                    | LineEditOutcome::CursorChanged
                    | LineEditOutcome::HandledNoChange => InputOutcome::Changed,
                    LineEditOutcome::Unhandled => InputOutcome::Unchanged,
                };
            }
            AuthState::Authenticating { .. } => {
                if key!(Esc).matches(key)
                    || key!('q', CONTROL).matches(key)
                    || key!('c', CONTROL).matches(key)
                {
                    if ctx.mid_session_login {
                        return InputOutcome::Action(Action::CancelLogin);
                    }
                    return InputOutcome::Action(Action::QuitConfirmed);
                }
            }
        }
    }
    if let Event::Paste(text) = ev {
        match ctx.auth_state {
            AuthState::Done => {
                if !ctx.has_access || ctx.is_zdr_blocked {
                    return InputOutcome::Unchanged;
                }
                return InputOutcome::ActionThenForward(Action::NewSession);
            }
            AuthState::Authenticating {
                mode: AuthMode::Loopback,
                ..
            } => {
                let _ = ctx.auth_code_input.insert_paste(text);
                return InputOutcome::Changed;
            }
            _ => {}
        }
    }
    if matches!(ev, Event::Resize(_, _)) {
        return InputOutcome::Changed;
    }
    if let Event::Mouse(mouse) = ev {
        use crossterm::event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                for (i, rect) in ctx.menu_rects.iter().enumerate() {
                    if mouse.column >= rect.x
                        && mouse.column < rect.x + rect.width
                        && mouse.row >= rect.y
                        && mouse.row < rect.y + rect.height
                    {
                        if matches!(ctx.auth_state, AuthState::Pending { .. }) {
                            return dispatch_pending_menu_action(i);
                        }
                        if ctx.is_zdr_blocked {
                            return dispatch_zdr_menu_action(i);
                        }
                        if !ctx.has_access {
                            return dispatch_access_gate_menu_action(i);
                        }
                        if ctx.has_claude_import
                            && i == 0
                            && mouse.column >= rect.x + rect.width.saturating_sub(4)
                            && mouse.column < rect.x + rect.width.saturating_sub(1)
                        {
                            return InputOutcome::Action(Action::DismissClaudeImport);
                        }
                        return dispatch_menu_action(
                            i,
                            ctx.has_claude_import,
                            ctx.show_changelog_action,
                            ctx.changelog_markdown.as_deref(),
                        );
                    }
                }
                if let Some(rect) = ctx.refresh_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::CheckSubscription);
                }
                if let Some(rect) = ctx.gate_url_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::OpenSuperechoagentUrl);
                }
                if let Some(rect) = ctx.upgrade_cta_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::AnnouncementsOpenCta(
                        echo_agent_telemetry::events::AnnouncementCtaSurface::Welcome,
                    ));
                }
                if let Some(rect) = ctx.privacy_banner_opt_in_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::PrivacyBannerOptIn);
                }
                if let Some(rect) = ctx.privacy_banner_opt_out_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::PrivacyBannerOptOut);
                }
                if let Some(rect) = ctx.privacy_banner_terms_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::OpenUrl(
                        crate::views::privacy_banner::PRIVACY_BANNER_TERMS_URL.to_string(),
                    ));
                }
                if let Some(rect) = ctx.privacy_banner_policy_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::OpenUrl(
                        crate::views::privacy_banner::PRIVACY_BANNER_POLICY_URL.to_string(),
                    ));
                }
                if let Some(rect) = ctx.changelog_cta_rect
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                    && let Some(md) = ctx.changelog_markdown.as_deref()
                {
                    return InputOutcome::Action(Action::ShowReleaseNotes {
                        title: "Release Notes".to_string(),
                        content: md.trim().to_string(),
                    });
                }
                if let Some(rect) = ctx.announcement_rect
                    && (ctx.announcement_truncated || *ctx.announcement_expanded)
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    *ctx.announcement_expanded = !*ctx.announcement_expanded;
                    return InputOutcome::Changed;
                }
                if let Some(rect) = ctx.auth_url_rect
                    && matches!(ctx.auth_state, AuthState::Authenticating { .. })
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::CopyAuthUrl);
                }
                if let Some(rect) = ctx.auth_fallback_rect
                    && matches!(ctx.auth_state, AuthState::Authenticating { .. })
                    && rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                {
                    return InputOutcome::Action(Action::ShowRawAuthUrl);
                }
                if let Some(rect) = ctx.import_banner_rect
                    && matches!(ctx.auth_state, AuthState::Done)
                    && mouse.column >= rect.x
                    && mouse.column < rect.x + rect.width
                    && mouse.row >= rect.y
                    && mouse.row < rect.y + rect.height
                {
                    return InputOutcome::Action(Action::ImportClaudeSettings);
                }
                if let Some(rect) = ctx.prompt_rect
                    && matches!(ctx.auth_state, AuthState::Done)
                    && mouse.column >= rect.x
                    && mouse.column < rect.x + rect.width
                    && mouse.row >= rect.y
                    && mouse.row < rect.y + rect.height
                {
                    *ctx.prompt_focused = true;
                    return InputOutcome::Changed;
                }
            }
            MouseEventKind::Moved => {
                let mut new_index = None;
                for (i, rect) in ctx.menu_rects.iter().enumerate() {
                    if mouse.column >= rect.x
                        && mouse.column < rect.x + rect.width
                        && mouse.row >= rect.y
                        && mouse.row < rect.y + rect.height
                    {
                        new_index = Some(i);
                        break;
                    }
                }
                if new_index != *ctx.menu_index {
                    *ctx.menu_index = new_index;
                    return InputOutcome::Changed;
                }
                if ctx.has_claude_import && new_index == Some(0) {
                    return InputOutcome::Changed;
                }
                let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                let over_cta = ctx.changelog_cta_rect.is_some_and(|r| r.contains(pos));
                if over_cta != *ctx.on_changelog_cta {
                    *ctx.on_changelog_cta = over_cta;
                    return InputOutcome::Changed;
                }
                let over_upgrade = ctx.upgrade_cta_rect.is_some_and(|r| r.contains(pos));
                if over_upgrade != *ctx.on_upgrade_cta {
                    *ctx.on_upgrade_cta = over_upgrade;
                    return InputOutcome::Changed;
                }
                #[cfg(feature = "local-workspace")]
                {
                    let over_ws = ctx
                        .workspace_mode_rects
                        .row
                        .is_some_and(|r| r.contains(pos));
                    if over_ws != *ctx.on_workspace_mode {
                        *ctx.on_workspace_mode = over_ws;
                        return InputOutcome::Changed;
                    }
                    if over_ws {
                        return InputOutcome::Changed;
                    }
                }
                let over_banner = ctx
                    .privacy_banner_opt_in_rect
                    .is_some_and(|r| r.contains(pos))
                    || ctx
                        .privacy_banner_opt_out_rect
                        .is_some_and(|r| r.contains(pos))
                    || ctx
                        .privacy_banner_terms_rect
                        .is_some_and(|r| r.contains(pos))
                    || ctx
                        .privacy_banner_policy_rect
                        .is_some_and(|r| r.contains(pos));
                if over_banner || *ctx.on_privacy_banner {
                    *ctx.on_privacy_banner = over_banner;
                    return InputOutcome::Changed;
                }
                let over_ann = (ctx.announcement_truncated || *ctx.announcement_expanded)
                    && ctx.announcement_rect.is_some_and(|r| r.contains(pos));
                if over_ann != *ctx.on_announcement_cta {
                    *ctx.on_announcement_cta = over_ann;
                    return InputOutcome::Changed;
                }
                if matches!(ctx.auth_state, AuthState::Authenticating { .. })
                    && ctx.auth_url_rect.is_some()
                {
                    return InputOutcome::Changed;
                }
            }
            _ => {}
        }
    }
    InputOutcome::Unchanged
}
/// Handle Up/Down arrow key cycling through a menu of `count` items.
fn is_quit_signal(key: &crossterm::event::KeyEvent) -> bool {
    key!('c', CONTROL).matches(key) || key!('d', CONTROL).matches(key)
}
/// `shortcuts[i]` triggers `menu_dispatch(i)`.
fn handle_menu_shortcuts(
    key: &crossterm::event::KeyEvent,
    menu_index: &mut Option<usize>,
    shortcuts: &[char],
    menu_dispatch: fn(usize) -> InputOutcome,
) -> InputOutcome {
    if is_quit_signal(key) {
        return InputOutcome::Action(Action::Quit);
    }
    for (i, &ch) in shortcuts.iter().enumerate() {
        if key.code == KeyCode::Char(ch) {
            return menu_dispatch(i);
        }
    }
    if key!(Enter).matches(key) {
        return menu_dispatch(menu_index.unwrap_or(0));
    }
    if let Some(outcome) = handle_menu_nav(key, menu_index, shortcuts.len()) {
        return outcome;
    }
    InputOutcome::Unchanged
}
fn handle_menu_nav(
    key: &crossterm::event::KeyEvent,
    index: &mut Option<usize>,
    count: usize,
) -> Option<InputOutcome> {
    match key.code {
        KeyCode::Down => {
            *index = Some(match *index {
                Some(i) if i + 1 < count => i + 1,
                Some(_) | None => 0,
            });
            Some(InputOutcome::Changed)
        }
        KeyCode::Up => {
            *index = Some(match *index {
                Some(0) | None => count.saturating_sub(1),
                Some(i) => i - 1,
            });
            Some(InputOutcome::Changed)
        }
        _ => None,
    }
}
/// Dispatch an action for a welcome menu item when not yet authenticated.
/// Menu layout: 0 = Login, 1 = Quit.
fn dispatch_pending_menu_action(index: usize) -> InputOutcome {
    match index {
        0 => InputOutcome::Action(Action::Login),
        1 => InputOutcome::Action(Action::Quit),
        _ => InputOutcome::Unchanged,
    }
}
/// Dispatch an action for a welcome menu item when ZDR-blocked.
/// Menu layout: 0 = Switch account, 1 = Quit.
fn dispatch_zdr_menu_action(index: usize) -> InputOutcome {
    match index {
        0 => InputOutcome::Action(Action::SwitchAccount),
        1 => InputOutcome::Action(Action::Quit),
        _ => InputOutcome::Unchanged,
    }
}
/// Menu actions when user is access-gated: 0 = Subscribe CTA, 1 = Logout, 2 = Quit.
/// "Refresh" (ctrl-r) is handled as a direct key shortcut, not a menu item.
fn dispatch_access_gate_menu_action(index: usize) -> InputOutcome {
    match index {
        0 => InputOutcome::Action(Action::OpenSuperechoagentUrl),
        1 => InputOutcome::Action(Action::Logout),
        2 => InputOutcome::Action(Action::Quit),
        _ => InputOutcome::Unchanged,
    }
}
/// Dispatch an action for a welcome menu item by index.
///
/// Menu order: `[Import]`, New worktree, Resume session, `[Changelog]`, Quit.
/// `show_changelog_action` is true when the Changelog row is rendered; release
/// notes open only once `changelog_md` is available.
fn dispatch_menu_action(
    index: usize,
    has_claude_import: bool,
    show_changelog_action: bool,
    changelog_md: Option<&str>,
) -> InputOutcome {
    let base = if has_claude_import { 1 } else { 0 };
    let worktree_idx = base;
    let resume_idx = base + 1;
    let (changelog_idx, quit_idx) = if show_changelog_action {
        (Some(base + 2), base + 3)
    } else {
        (None, base + 2)
    };
    if has_claude_import && index == 0 {
        return InputOutcome::Action(Action::ImportClaudeSettings);
    }
    if index == worktree_idx {
        return InputOutcome::Action(Action::OpenNewWorktreeDialog);
    }
    if index == resume_idx {
        return InputOutcome::Action(Action::FetchSessionList);
    }
    if Some(index) == changelog_idx {
        if let Some(md) = changelog_md {
            return InputOutcome::Action(Action::ShowReleaseNotes {
                title: "Release Notes".to_string(),
                content: md.trim().to_string(),
            });
        }
        return InputOutcome::Unchanged;
    }
    if index == quit_idx {
        return InputOutcome::Action(Action::Quit);
    }
    InputOutcome::Unchanged
}
impl AppView {
    /// Merge notification escape sequences with render-produced post-flush
    /// escapes. Both inputs are optional; returns `None` only when both are
    /// `None`.
    fn merge_escapes(
        notif: Option<String>,
        render: Option<crate::terminal::overlay::PostFlush>,
    ) -> Option<crate::terminal::overlay::PostFlush> {
        Self::merge_post_flush(
            notif.map(crate::terminal::overlay::PostFlush::plain),
            render,
        )
    }
    fn merge_post_flush(
        first: Option<crate::terminal::overlay::PostFlush>,
        second: Option<crate::terminal::overlay::PostFlush>,
    ) -> Option<crate::terminal::overlay::PostFlush> {
        match (first, second) {
            (Some(mut first), Some(second)) => {
                first.append(second);
                Some(first)
            }
            (Some(first), None) => Some(first),
            (None, second) => second,
        }
    }
    /// Build the Kitty delete escapes that remove image placements left
    /// behind by agent views that are not drawn this frame.
    ///
    /// Kitty graphics survive cell redraws until explicitly deleted, and
    /// every regular clear lives inside `AgentView::draw` / the prompt
    /// widget's per-frame self-heal. Once the dashboard takes over the
    /// frame those paths stop running, so an image overlay (or inline
    /// scrollback media) the user left open in the agent view would float
    /// above the dashboard forever. Called from the
    /// `ActiveView::AgentDashboard` draw branch every frame:
    ///
    /// - Placement id 1 is cleared only when no popup agent is drawn; a popup
    ///   owns and reuses that slot across consecutive dashboard frames.
    /// - Inline scrollback media ids (2+) are drained per agent via
    ///   `AgentView::take_inline_media_clear_escapes`, which resets the
    ///   agent's placement tracking — a one-shot sweep per transition,
    ///   not a per-frame cost. The popup-attached agent is skipped: it
    ///   just drew and manages its own placements. The clears-before-popup
    ///   ordering also means a drained id that collides with one the popup
    ///   re-places this frame ends up displayed, not deleted.
    fn dashboard_stale_image_clears(
        agents: &mut IndexMap<AgentId, AgentView>,
        drawn_agent: Option<AgentId>,
    ) -> Option<crate::terminal::overlay::PostFlush> {
        if crate::terminal::image::detect_graphics_protocol()
            == crate::terminal::image::GraphicsProtocol::None
        {
            return None;
        }
        let mut clears = crate::terminal::overlay::PostFlush::default();
        let mut has_escapes = false;
        for (id, agent) in agents.iter_mut() {
            if Some(*id) == drawn_agent {
                continue;
            }
            if let Some(esc) = agent.take_inline_media_clear_escapes() {
                clears.append_plain(&esc);
                has_escapes = true;
            }
        }
        if drawn_agent.is_none() {
            clears.append(crate::terminal::overlay::clear_kitty().into());
            has_escapes = true;
        }
        has_escapes.then_some(clears)
    }
    /// Minimal mode: queue the most-recently committed folded block (collapsed
    /// reasoning / truncated tool output) to be re-printed fully expanded below
    /// the conversation on the next draw (design decision K10). Returns whether
    /// something was queued. No-op when nothing folded remains to expand.
    pub(crate) fn minimal_expand_last(&mut self) -> bool {
        let ActiveView::Agent(id) = &self.active_view else {
            return false;
        };
        let id = *id;
        let found = match self.agents.get_mut(&id) {
            Some(agent) => agent.scrollback.take_expandable_committed(),
            None => None,
        };
        if let Some(eid) = found {
            self.minimal_state.pending_expand.push(eid);
            true
        } else {
            false
        }
    }
    /// Minimal-mode key overrides, handled inline instead of by
    /// `agent.handle_input`. Returns `Some` when the key was consumed here.
    /// Callers gate on `is_minimal()` + non-release before dispatching.
    ///
    /// These keys carry full-TUI meanings that don't apply to the
    /// scrollback-native mode, so minimal remaps them:
    /// - `Ctrl+T` pins/unpins the todo panel (force-show). It otherwise
    ///   auto-hides once all todos are done (`minimal::live::todo_panel_visible`);
    ///   the pin keeps a finished list visible for review. The full-TUI
    ///   Ctrl+T toggles the todo overlay pane, which minimal never renders.
    /// - `Ctrl+E` re-prints the most-recently committed folded block fully
    ///   expanded below the conversation (K10) — committed terminal text can't be
    ///   mutated, so expansion is an honest re-print. The full-TUI Ctrl+E toggles
    ///   the scrollback-pane fold.
    /// - `Ctrl+O` opens the whole conversation fully expanded in `$PAGER` (the
    ///   "expand everything" view, the honest equivalent of a full
    ///   transcript mode for a static native scrollback). The full-TUI Ctrl+O is
    ///   interject, which keeps its Ctrl+Enter / Ctrl+I alt bindings —
    ///   **except on Apple Terminal**, where Ctrl+O *is* the interject chord
    ///   (kitty keyboard protocol unavailable → Ctrl+Enter doesn't arrive and
    ///   Ctrl+I aliases to Tab, see `default_actions`'s terminal-aware
    ///   `InterjectPrompt` binding). There the remap yields to interject only
    ///   while an interject would actually consume the press (turn running with
    ///   a non-empty composer, turn running with a queued follow-up on an empty
    ///   composer, or editing a queued row) — otherwise minimal on Apple
    ///   Terminal would have no working interject key at all. At idle / with an
    ///   empty composer and no queue the interject path is a silent no-op, so
    ///   the remap keeps the key and the transcript opens (it looked simply
    ///   dead before); see `minimal_api::minimal_ctrl_o_opens_transcript`, which
    ///   the info-row hint shares so it always advertises what a press would do.
    /// - The `ToggleQueue` chord (Ctrl+; by default; registry-resolved because
    ///   it is remappable and terminal-dependent) commits the read-only
    ///   `/queue` snapshot instead of toggling the full-TUI queue pane: the
    ///   pane never renders in minimal, so the toggle focused an *invisible*
    ///   pane that ate every keystroke (the same class of trap as the
    ///   never-rendered `/mcps` modal). Queue edits stay full-TUI-only; K13's
    ///   panes-become-committed-blocks rule applies.
    fn minimal_key_intercept(&mut self, key: &crossterm::event::KeyEvent) -> Option<InputOutcome> {
        if key!('t', CONTROL).matches(key) {
            self.minimal_state.show_todos = !self.minimal_state.show_todos;
        } else if self
            .registry
            .matches_id(crate::actions::ActionId::ToggleQueue, key)
        {
            return Some(InputOutcome::Action(crate::app::actions::Action::ShowQueue));
        } else if key!('e', CONTROL).matches(key) {
            self.minimal_expand_last();
        } else if key!('o', CONTROL).matches(key) {
            if crate::minimal_api::minimal_ctrl_o_opens_transcript(self) {
                return Some(InputOutcome::Action(
                    crate::app::actions::Action::OpenTranscriptPager,
                ));
            }
            if let ActiveView::Agent(id) = &self.active_view {
                let id = *id;
                if let Some(agent) = self.agents.get_mut(&id) {
                    return Some(agent.handle_prompt_key(key, &self.registry, false));
                }
            }
            return None;
        } else {
            return None;
        }
        Some(InputOutcome::Changed)
    }
    /// Release capture while a native-select surface is on screen so the terminal owns
    /// drag-select. Restore only if we took the hold: a user who already had
    /// `/toggle-mouse-reporting` off must stay off.
    fn sync_native_selection_mouse(&mut self) {
        if self.screen_mode.is_minimal() {
            return;
        }
        let want_off = self.auth_show_raw_url
            && matches!(self.active_view, ActiveView::Welcome)
            && matches!(self.auth_state, AuthState::Authenticating { .. });
        let capture_on = super::MOUSE_CAPTURE_ENABLED.load(std::sync::atomic::Ordering::Acquire);
        if want_off && capture_on {
            self.native_select_hold = true;
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                let _ = crossterm::execute!(stderr, crossterm::event::DisableMouseCapture);
            });
            #[cfg(windows)]
            super::win_native_selection::enable_native_selection();
            super::MOUSE_CAPTURE_ENABLED.store(false, std::sync::atomic::Ordering::Release);
        } else if !want_off && self.native_select_hold {
            self.native_select_hold = false;
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                let _ = crossterm::execute!(stderr, crossterm::event::EnableMouseCapture);
            });
            super::MOUSE_CAPTURE_ENABLED.store(true, std::sync::atomic::Ordering::Release);
            for agent in self.agents.values_mut() {
                agent.set_sticky_toast_recursive(None);
            }
        }
    }
    /// Render the current view to the terminal.
    pub fn draw(&mut self, terminal: &mut PagerTerminal) {
        self.draw_inner(terminal);
        echo_agent_telemetry::startup::record_first_frame();
        crate::memory_release::run_deferred_release();
    }
    fn draw_inner(&mut self, terminal: &mut PagerTerminal) {
        self.resync_announcement_slash_gate_on_divergence();
        if self.screen_mode.is_minimal() {
            if let Some(hooks) = crate::minimal_hook::hooks() {
                (hooks.draw)(self, terminal);
            }
            return;
        }
        if self.welcome_on_auth_url
            && !matches!(
                (&self.active_view, &self.auth_state),
                (ActiveView::Welcome, AuthState::Authenticating { .. })
            )
        {
            self.welcome_on_auth_url = false;
            if crate::terminal::terminal_context()
                .hyperlink_capabilities()
                .osc22_cursor
            {
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    let _ = crossterm::execute!(stderr, crate::terminal::SetDefaultCursor);
                });
            }
        }
        self.sync_native_selection_mouse();
        self.maybe_trigger_small_screen_tip();
        self.maybe_trigger_ssh_wrap_tip();
        let compact = self.appearance.prompt.compact;
        let (header_pad_left, header_pad_right, header_pad_top) = {
            let layout_cfg = &self.appearance.scrollback.layout;
            (
                layout_cfg.eff_hpad_left(compact),
                layout_cfg.eff_hpad_right(compact),
                layout_cfg.eff_outer_vpad(compact),
            )
        };
        let zdr_blocked_for_draw = self.is_zdr_blocked();
        let has_access = self.has_access();
        let privacy_banner = self.privacy_banner_should_show();
        let voice_available = self.voice_available();
        let voice_on_surface = self.voice_target_on_active_surface();
        let voice_listening = voice_on_surface && self.voice_listening();
        let voice_interim = voice_on_surface
            .then(|| self.voice_interim().map(str::to_owned))
            .flatten();
        let esc_owned_before_agent = self.esc_owned_before_agent();
        let scroll_debug_panel = self.scroll_debug_panel();
        let dev_fps_rows = self.dev_fps_rows();
        let fps_overlay = self.fps_hud.overlay(dev_fps_rows);
        let foreign_resume_hint = self.foreign_resume_hint().cloned();
        let privacy_banner_agent = self.privacy_banner_should_show()
            && !crate::views::announcements::has_critical_session_announcement(
                &self.active_announcements,
                &self.hidden_announcement_ids,
            );
        let agent_mouse_pos = self.last_mouse_pos;
        let status_line_frame = self.status_line_frame();
        let Self {
            active_view,
            agents,
            registry,
            scratch,
            cursor,
            pending_action,
            pending_notification_escapes,
            ..
        } = self;
        let notif_escapes = pending_notification_escapes.take();
        let pending_hint = pending_action
            .as_ref()
            .filter(|p| !p.expired())
            .and_then(|p| {
                p.label
                    .map(|label| crate::views::shortcuts_bar::PendingHint {
                        shortcut: p.shortcut,
                        label,
                    })
            });
        let fps_frame_started = fps_overlay.as_ref().map(|_| std::time::Instant::now());
        crate::render::draw::draw_frame(terminal, cursor, |f, link_spans| {
            let full_area = f.area();
            let tracing_height = 0u16;
            #[allow(unused_variables)]
            let (tracing_area, view_area) =
                if tracing_height > 0 && tracing_height < full_area.height {
                    let tracing = ratatui::layout::Rect {
                        x: full_area.x,
                        y: full_area.y,
                        width: full_area.width,
                        height: tracing_height,
                    };
                    let view = ratatui::layout::Rect {
                        x: full_area.x,
                        y: full_area.y + tracing_height,
                        width: full_area.width,
                        height: full_area.height.saturating_sub(tracing_height),
                    };
                    (Some(tracing), view)
                } else if tracing_height >= full_area.height {
                    let tracing = full_area;
                    (Some(tracing), ratatui::layout::Rect::default())
                } else {
                    (None, full_area)
                };
            if view_area.height > 0 {
                match *active_view {
                    ActiveView::Welcome => {
                        let mut flags_vec: Vec<crate::views::prompt_widget::PromptFlag<'_>> =
                            Vec::new();
                        if self.default_yolo {
                            flags_vec.push(crate::views::prompt_widget::PromptFlag {
                                text: "always-approve",
                                color: None,
                                bold: false,
                            });
                        }
                        if !self.welcome_prompt.text().is_empty() {
                            self.welcome_tip_typing_dismissed = true;
                        }
                        let tip = if self.welcome_tip_typing_dismissed {
                            None
                        } else {
                            self.tip.as_deref()
                        };
                        let model_name_base = self.models.current_model_name().unwrap_or_default();
                        let model_name = match self.models.reasoning_effort {
                            Some(eff) => format!("{model_name_base} ({eff})"),
                            None => model_name_base,
                        };
                        let hero_cta = crate::views::announcements::promo_cta(
                            &self.active_announcements,
                            &self.hidden_announcement_ids,
                        );
                        let hero_announcement = hero_cta
                            .map(|(owner, _, _)| owner)
                            .or_else(|| {
                                crate::views::announcements::first_session_announcement(
                                    &self.active_announcements,
                                    &self.hidden_announcement_ids,
                                )
                            })
                            .or(self.announcement.as_ref());
                        let welcome_params = crate::views::welcome::WelcomeRenderParams {
                            prompt_focus: if self.welcome_prompt_focused {
                                WelcomePromptFocus::Focused
                            } else {
                                WelcomePromptFocus::Unfocused
                            },
                            cwd: &self.cwd,
                            auth_state: &self.auth_state,
                            trust_state: &self.trust_state,
                            consent_state: &self.consent_state,
                            consent_hover_link: self.welcome_consent_hover_link,
                            login_label: self.login_label.as_deref(),
                            auth_code_input: self.auth_code_input.text(),
                            auth_code_cursor_byte: self.auth_code_input.cursor_byte(),
                            clipboard_delivery: self.auth_clipboard_delivery,
                            show_raw_url: self.auth_show_raw_url,
                            announcement: hero_announcement,
                            tip,
                            model_name: &model_name,
                            flags: &flags_vec,
                            selected: self.welcome_menu_index,
                            team_name: self.team_name.as_deref(),
                            has_access,
                            has_claude_import: self.has_claude_import,
                            mouse_pos: self.last_mouse_pos,
                            is_zdr_blocked: zdr_blocked_for_draw,
                            session_picker: self.session_picker_entries.as_deref(),
                            session_picker_loading:
                                crate::views::session_picker::loading_spinner_active(
                                    self.session_picker_entries.as_deref(),
                                    self.session_picker_source_filter,
                                    self.session_picker_loading,
                                    &self.session_picker_lanes,
                                ),
                            compact,
                            pending_hint,
                            startup_warnings: &self.startup_warnings,
                            pending_update_version: self.pending_update_version.as_deref(),
                            foreign_resume_hint: foreign_resume_hint.as_ref(),
                            session_picker_content_results: self
                                .session_picker_content_results
                                .as_deref(),
                            session_picker_content_loading: self.session_picker_content_loading,
                            session_picker_entries_query: self
                                .session_picker_entries_query
                                .as_deref(),
                            welcome_tick: self.welcome_tick,
                            gate: self.gate.as_ref(),
                            subscription_tier: self.subscription_tier.as_deref(),
                            session_picker_grouped: self.session_picker_grouped,
                            session_picker_source_filter: self.session_picker_source_filter,
                            session_picker_pending_delete: self
                                .session_picker_pending_delete
                                .is_some(),
                            chat_mode: self.chat_mode,
                            credit_balance: self.credit_balance.as_ref(),
                            auto_topup: self.auto_topup.as_ref(),
                            usage_visible: self.usage_visible,
                            is_api_key_auth: self.is_api_key_auth,
                            changelog_bullets: &self.changelog_bullets,
                            changelog_has_full_notes: self.changelog_markdown.is_some(),
                            welcome_announcement_expanded: self.welcome_announcement.expanded,
                            upgrade_cta: hero_cta.map(|(_owner, label, _)| label),
                            privacy_banner,
                            #[cfg(feature = "local-workspace")]
                            workspace_mode: self.welcome_workspace_mode,
                            #[cfg(feature = "local-workspace")]
                            workspace_mode_startup_locked: self.local_workspace_startup_locked,
                            #[cfg(feature = "local-workspace")]
                            workspace_mode_ack_pending: self.welcome_local_workspace_ack_pending,
                        };
                        let result = crate::views::welcome::render_welcome(
                            view_area,
                            f.buffer_mut(),
                            &welcome_params,
                            &mut self.welcome_prompt,
                            &mut self.session_picker_state,
                        );
                        self.welcome_menu_rects = result.menu_rects;
                        self.welcome_show_changelog_action = result.changelog_action_present;
                        self.welcome_prompt_rect = result.prompt_rect;
                        self.welcome_import_banner_rect = result.import_banner_rect;
                        self.welcome_auth_url_rect = result.auth_url_rect;
                        self.welcome_auth_fallback_rect = result.auth_fallback_rect;
                        self.welcome_refresh_rect = result.refresh_rect;
                        self.welcome_gate_url_rect = result.gate_url_rect;
                        self.welcome_consent_link_rects = result.consent_link_rects;
                        if self.welcome_consent_link_rects.is_empty() {
                            self.welcome_consent_hover_link = None;
                        }
                        record_consent_paint(&mut self.consent_state, result.consent_legibility);
                        self.welcome_upgrade_cta_rect = result.upgrade_cta_rect;
                        self.welcome_privacy_banner_opt_in_rect = result.privacy_banner_opt_in_rect;
                        self.welcome_privacy_banner_opt_out_rect =
                            result.privacy_banner_opt_out_rect;
                        self.welcome_privacy_banner_terms_rect = result.privacy_banner_terms_rect;
                        self.welcome_privacy_banner_policy_rect = result.privacy_banner_policy_rect;
                        #[cfg(feature = "local-workspace")]
                        {
                            self.welcome_workspace_mode_rects = result.workspace_mode_rects;
                        }
                        self.welcome_changelog_cta_rect = result.changelog_cta_rect;
                        if let Some((ref msg, _)) = self.welcome_toast {
                            crate::views::welcome::paint_welcome_toast(
                                f.buffer_mut(),
                                view_area,
                                msg,
                                self.welcome_prompt_rect,
                            );
                        }
                        self.welcome_announcement.truncated = result.announcement_truncated;
                        self.welcome_announcement.rect = result.announcement_rect;
                        self.session_picker_state.hit_areas = result.session_picker_hit_areas;
                        if let Some(modal) = self.import_claude_modal.as_mut() {
                            let theme = crate::theme::Theme::current();
                            crate::views::import_claude_modal::render_import_claude_modal(
                                f.buffer_mut(),
                                view_area,
                                modal,
                                &theme,
                                compact,
                            );
                        }
                        if let Some(dialog) = self.new_worktree_dialog.as_ref() {
                            crate::views::new_worktree_dialog::render_new_worktree_dialog(
                                view_area,
                                f.buffer_mut(),
                                dialog,
                            );
                        }
                        if let Some(crate::views::modal::ActiveModal::DocViewer {
                            ref title,
                            ref content,
                            ref mut scroll,
                            ref mut window,
                            ref mut cached_lines,
                            ..
                        }) = self.welcome_doc_viewer
                        {
                            let theme = crate::theme::Theme::current();
                            crate::views::modal::render_doc_viewer_overlay(
                                f.buffer_mut(),
                                view_area,
                                window,
                                title,
                                content,
                                scroll,
                                cached_lines,
                                compact,
                                &theme,
                            );
                        }
                        if !has_access && !self.access_gate_shown_logged {
                            self.access_gate_shown_logged = true;
                            echo_agent_telemetry::session_ctx::log_event(
                                echo_agent_telemetry::events::EchoAgentUpsellShown {
                                    source:
                                        echo_agent_telemetry::events::EchoAgentUpsell::WelcomeScreen,
                                    auth_method: self
                                        .login_method_id
                                        .as_ref()
                                        .map(|id| id.0.to_string()),
                                },
                            );
                        }
                        if let Some(tutorial) = self.tutorial.as_mut() {
                            crate::views::tutorial::render_tutorial(
                                f.buffer_mut(),
                                view_area,
                                tutorial,
                                compact,
                            );
                        }
                        if let Some(fps) = &fps_overlay {
                            fps.render(full_area, f.buffer_mut());
                        }
                        if let Some(panel) = &scroll_debug_panel {
                            panel.render(full_area, f.buffer_mut());
                        }
                        let has_cloud_modal = false;
                        let cursor = if has_cloud_modal || self.tutorial.is_some() {
                            None
                        } else {
                            result.cursor_pos
                        };
                        let on_url = self.welcome_auth_url_rect.as_ref().is_some_and(|r| {
                            matches!(self.auth_state, AuthState::Authenticating { .. })
                                && self.last_mouse_pos.is_some_and(|(mx, my)| {
                                    mx >= r.x
                                        && mx < r.x + r.width
                                        && my >= r.y
                                        && my < r.y + r.height
                                })
                        });
                        let mut post_flush = result.post_flush_escapes;
                        if crate::terminal::terminal_context()
                            .hyperlink_capabilities()
                            .osc22_cursor
                            && on_url != self.welcome_on_auth_url
                        {
                            use crossterm::Command;
                            let mut buf = String::new();
                            if on_url {
                                let _ = crate::terminal::SetPointerCursor.write_ansi(&mut buf);
                            } else {
                                let _ = crate::terminal::SetDefaultCursor.write_ansi(&mut buf);
                            }
                            match post_flush.as_mut() {
                                Some(existing) => existing.append_plain(&buf),
                                None => {
                                    post_flush =
                                        Some(crate::terminal::overlay::PostFlush::plain(buf));
                                }
                            }
                        }
                        self.welcome_on_auth_url = on_url;
                        return (cursor, post_flush);
                    }
                    ActiveView::Agent(id) => {
                        let overlay_focused = false;
                        let overlay_active = self
                            .dashboard
                            .as_ref()
                            .is_some_and(|d| d.attached_agent == Some(id));
                        let position: Option<(usize, usize)> =
                            if overlay_active && let Some(d) = self.dashboard.as_ref() {
                                let order = crate::views::dashboard::overlay_cycle_order(d, agents);
                                order
                                    .iter()
                                    .position(|i| *i == id)
                                    .map(|idx| (idx + 1, order.len()))
                            } else {
                                None
                            };
                        let overlay_can_cycle = position.is_some_and(|(_, n)| n > 1);
                        let (agent_area, header) = if overlay_active {
                            let theme = crate::theme::Theme::current();
                            let title = agents
                                .get(&id)
                                .map(crate::views::session_title::entry_title)
                                .unwrap_or_else(|| "(session)".to_string());
                            let (hover_prev, hover_next, hover_close) = self
                                .dashboard
                                .as_ref()
                                .map(|d| {
                                    (
                                        d.overlay_prev_hit.hovered,
                                        d.overlay_next_hit.hovered,
                                        d.overlay_close_hit.hovered,
                                    )
                                })
                                .unwrap_or((false, false, false));
                            let header = crate::views::dashboard::render_dashboard_session_header(
                                f.buffer_mut(),
                                view_area,
                                &theme,
                                &title,
                                position,
                                hover_prev,
                                hover_next,
                                hover_close,
                                header_pad_left,
                                header_pad_right,
                                header_pad_top,
                            );
                            match header {
                                Some(chrome) => (chrome.content, Some(chrome)),
                                None => (view_area, None),
                            }
                        } else {
                            (view_area, None)
                        };
                        if let Some(d) = self.dashboard.as_mut() {
                            d.overlay_close_hit.set(header.and_then(|c| c.close_rect));
                            d.overlay_prev_hit.set(header.and_then(|c| c.prev_rect));
                            d.overlay_next_hit.set(header.and_then(|c| c.next_rect));
                        }
                        if let Some(d) = self.dashboard.as_mut()
                            && d.peek_viewport.is_some()
                        {
                            d.restore_peek_viewport(agents);
                        }
                        if let Some(agent) = agents.get_mut(&id) {
                            let announcement_banner_h =
                                crate::views::announcements::session_banner_height(
                                    &self.active_announcements,
                                    &self.hidden_announcement_ids,
                                );
                            let privacy_banner = privacy_banner_agent;
                            let show_session_tip =
                                !privacy_banner && self.tip.is_some() && agent.should_show_tip();
                            let has_mode_banner = agent.mode_switch_banner.is_some();
                            let banner_height = if privacy_banner {
                                crate::views::privacy_banner::MIN_HEIGHT
                            } else if has_mode_banner {
                                1
                            } else if announcement_banner_h > 0 {
                                announcement_banner_h
                            } else if show_session_tip {
                                1
                            } else {
                                0
                            };
                            let result = agent.draw(
                                agent_area,
                                f.buffer_mut(),
                                registry,
                                scratch,
                                pending_hint,
                                overlay_focused,
                                crate::app::agent_view::BannerSlotParams {
                                    height: banner_height,
                                    announcements: &self.active_announcements,
                                    hidden_ids: &self.hidden_announcement_ids,
                                    privacy_banner,
                                    mouse_pos: agent_mouse_pos,
                                    tip: if show_session_tip {
                                        self.tip.as_deref()
                                    } else {
                                        None
                                    },
                                },
                                &self.bundle_state,
                                overlay_active,
                                overlay_can_cycle,
                                link_spans,
                                AppRenderParams {
                                    voice_available,
                                    voice_listening,
                                    voice_interim: voice_interim.as_deref(),
                                    esc_owned_before_agent,
                                    status_line: status_line_frame.clone(),
                                },
                            );
                            if let Some(modal) = self.import_claude_modal.as_mut() {
                                let theme = crate::theme::Theme::current();
                                crate::views::import_claude_modal::render_import_claude_modal(
                                    f.buffer_mut(),
                                    view_area,
                                    modal,
                                    &theme,
                                    compact,
                                );
                            }
                            if let Some(tutorial) = self.tutorial.as_mut() {
                                crate::views::tutorial::render_tutorial(
                                    f.buffer_mut(),
                                    view_area,
                                    tutorial,
                                    compact,
                                );
                            }
                            if let Some(fps) = &fps_overlay {
                                fps.render(full_area, f.buffer_mut());
                            }
                            if let Some(panel) = &scroll_debug_panel {
                                panel.render(full_area, f.buffer_mut());
                            }
                            let (cursor_pos, post_flush) = result;
                            let has_cloud = false;
                            if has_cloud
                                || self.import_claude_modal.is_some()
                                || self.tutorial.is_some()
                            {
                                link_spans.clear();
                            }
                            let cursor = if has_cloud || self.tutorial.is_some() {
                                None
                            } else {
                                cursor_pos
                            };
                            return (cursor, Self::merge_escapes(notif_escapes, post_flush));
                        }
                    }
                    ActiveView::AgentDashboard => {
                        if let Some(dashboard) = self.dashboard.as_mut() {
                            dashboard.voice_listening = voice_listening;
                            dashboard.voice_interim = voice_interim.clone();
                            if let Some(id) = dashboard.attached_agent
                                && !agents.contains_key(&id)
                            {
                                dashboard.close_popup();
                                if dashboard.error_toast.is_none() {
                                    dashboard.error_toast = Some(format!(
                                        "{} Session closed",
                                        crate::glyphs::check_mark()
                                    ));
                                }
                            }
                            let dashboard_roster: &[crate::app::roster::RosterEntry] =
                                if self.leader_mode {
                                    &self.leader_roster
                                } else {
                                    &self.dashboard_local_sessions
                                };
                            let dash_upgrade_cta = crate::views::announcements::promo_cta(
                                &self.active_announcements,
                                &self.hidden_announcement_ids,
                            )
                            .map(
                                |(owner, label, _)| crate::views::dashboard::HeaderUpgradeCta {
                                    label,
                                    pinned: !crate::views::announcements::is_dismissible(owner),
                                    caption: crate::views::announcements::usable_cta_caption(owner),
                                },
                            );
                            let dash_cursor = crate::views::dashboard::render_dashboard(
                                f.buffer_mut(),
                                view_area,
                                dashboard,
                                agents,
                                registry,
                                pending_hint,
                                dashboard_roster,
                                self.dashboard_sessions_loading,
                                dash_upgrade_cta,
                            );
                            let (popup_cursor, popup_post_flush, drawn_popup_agent) =
                                if let Some(agent_id) = dashboard.attached_agent {
                                    let theme = crate::theme::Theme::current();
                                    let popup_area = crate::views::dashboard::popup_rect(view_area);
                                    let title = agents
                                        .get(&agent_id)
                                        .map(crate::views::session_title::entry_title)
                                        .unwrap_or_else(|| "(session)".to_string());
                                    let bundle_state = &self.bundle_state;
                                    let (cursor, post_flush, drawn) =
                                        crate::views::dashboard::render_popup_overlay(
                                            f.buffer_mut(),
                                            popup_area,
                                            &theme,
                                            &title,
                                            dashboard,
                                            |inner, buf| {
                                                if let Some(agent) = agents.get_mut(&agent_id) {
                                                    agent.draw(
                                                    inner,
                                                    buf,
                                                    registry,
                                                    scratch,
                                                    None,
                                                    false,
                                                    crate::app::agent_view::BannerSlotParams::none(
                                                    ),
                                                    bundle_state,
                                                    false,
                                                    false,
                                                    link_spans,
                                                    AppRenderParams {
                                                        esc_owned_before_agent,
                                                        ..Default::default()
                                                    },
                                                )
                                                } else {
                                                    (None, None)
                                                }
                                            },
                                        );
                                    (cursor, post_flush, drawn.then_some(agent_id))
                                } else {
                                    (None, None, None)
                                };
                            let stale_clears =
                                Self::dashboard_stale_image_clears(agents, drawn_popup_agent);
                            let popup_post_flush =
                                Self::merge_post_flush(stale_clears, popup_post_flush);
                            let tutorial_open = self.tutorial.is_some();
                            if let Some(tutorial) = self.tutorial.as_mut() {
                                crate::views::tutorial::render_tutorial(
                                    f.buffer_mut(),
                                    view_area,
                                    tutorial,
                                    compact,
                                );
                            }
                            if let Some(fps) = &fps_overlay {
                                fps.render(full_area, f.buffer_mut());
                            }
                            if let Some(panel) = &scroll_debug_panel {
                                panel.render(full_area, f.buffer_mut());
                            }
                            let cursor = if tutorial_open {
                                None
                            } else if dashboard.attached_agent.is_some() {
                                popup_cursor
                            } else {
                                dash_cursor
                            };
                            return (cursor, Self::merge_escapes(notif_escapes, popup_post_flush));
                        }
                    }
                }
            }
            if let Some(fps) = &fps_overlay {
                fps.render(full_area, f.buffer_mut());
            }
            if let Some(panel) = &scroll_debug_panel {
                panel.render(full_area, f.buffer_mut());
            }
            (None, Self::merge_escapes(notif_escapes, None))
        });
        if let Some(started) = fps_frame_started {
            self.fps_hud.record(started.elapsed());
        }
        self.log_announcement_cta_impressions();
        self.maybe_evict_offscreen_caches();
    }
    /// Log [`echo_agent_telemetry::events::AnnouncementCtaShown`] for each
    /// surface whose CTA button is painted this frame (armed hit rect, not
    /// covered by a frame occluder — the click/OSC 8 truth the impression
    /// pairs with), once per (announcement, surface) per pager process
    /// (cleared on logout). The owner resolves through the same slot gate as
    /// the click dispatch, so a critical preempting the slot or a hidden
    /// promo emits nothing.
    pub(crate) fn log_announcement_cta_impressions(&mut self) {
        use echo_agent_telemetry::events::AnnouncementCtaSurface;
        let (banner, welcome, header, dashboard) = match self.active_view {
            ActiveView::Welcome => (false, self.welcome_upgrade_cta_rect.is_some(), false, false),
            ActiveView::Agent(agent_id) => match self.agents.get(&agent_id) {
                Some(a) => {
                    let cta_rect = a.hit_announcement_cta.rect;
                    let header_rect = a.hit_upgrade_cta.rect;
                    (
                        cta_rect.is_some_and(|r| !a.rect_occluded(r)),
                        false,
                        header_rect.is_some_and(|r| !a.rect_occluded(r)),
                        false,
                    )
                }
                None => return,
            },
            ActiveView::AgentDashboard => (
                false,
                false,
                false,
                self.dashboard
                    .as_ref()
                    .is_some_and(|d| d.upgrade_cta_hit.rect.is_some()),
            ),
        };
        if !(banner || welcome || header || dashboard) {
            return;
        }
        let Some((owner, _label, _url)) = crate::views::announcements::promo_cta(
            &self.active_announcements,
            &self.hidden_announcement_ids,
        ) else {
            return;
        };
        let key = echo_agent_announcements::announcement_hide_key(owner);
        let id = owner.id.clone();
        let surfaces = [
            (AnnouncementCtaSurface::Banner, banner),
            (AnnouncementCtaSurface::Welcome, welcome),
            (AnnouncementCtaSurface::Header, header),
            (AnnouncementCtaSurface::Dashboard, dashboard),
        ];
        for (surface, _) in surfaces.into_iter().filter(|(_, painted)| *painted) {
            if self
                .announcement_cta_impressions_logged
                .insert((key.clone(), surface))
            {
                echo_agent_telemetry::session_ctx::log_event(
                    echo_agent_telemetry::events::AnnouncementCtaShown {
                        id: id.clone(),
                        source: surface,
                    },
                );
            }
        }
    }
    /// Interval between off-screen render-cache eviction sweeps.
    const CACHE_EVICT_INTERVAL: Duration = Duration::from_secs(5);
    /// Throttled sweep of off-screen render caches for the active view's
    /// scrollback (parent agent, or the open fullscreen subagent child).
    /// A sweep is an O(entries) walk of pointer-sized cache slots — trivial
    /// next to a frame render — but there's no reason to run it per frame.
    fn maybe_evict_offscreen_caches(&mut self) {
        let ActiveView::Agent(id) = self.active_view else {
            return;
        };
        let now = Instant::now();
        if self
            .last_cache_evict_at
            .is_some_and(|t| now.duration_since(t) < Self::CACHE_EVICT_INTERVAL)
        {
            return;
        }
        self.last_cache_evict_at = Some(now);
        if let Some(agent) = self.agents.get_mut(&id) {
            let evicted = if let Some(child_sid) = agent.active_subagent.clone() {
                agent
                    .subagent_views
                    .get(&child_sid)
                    .map(|child| child.scrollback.evict_offscreen_render_caches())
                    .unwrap_or(0)
            } else {
                agent.scrollback.evict_offscreen_render_caches()
            };
            if evicted > 0 {
                tracing::debug!(evicted, "scrollback.evicted_offscreen_render_caches");
            }
        }
    }
}
/// The renderer is the only thing that knows whether the body fitted, and accept is gated on that.
fn record_consent_paint(
    state: &mut ConsentState,
    reported: Option<crate::app::consent::ConsentLegibility>,
) {
    let ConsentState::Pending {
        legibility,
        painted_at,
        ..
    } = state
    else {
        return;
    };
    let Some(painted) = reported else {
        *legibility = crate::app::consent::ConsentLegibility::Illegible;
        return;
    };
    *legibility = painted;
    if painted_at.is_none() {
        *painted_at = Some(Instant::now());
    }
}
impl AppView {
    /// True when any modal that should swallow scroll input is open.
    fn is_scroll_blocking_modal_open(&self) -> bool {
        let cloud_modal_open = false;
        matches!(self.active_view, ActiveView::Agent(id) if self.agents.get(&id).is_some_and(|a| a.extensions_modal.is_some() || a.active_modal.is_some()))
            || self.import_claude_modal.is_some()
            || self.new_worktree_dialog.is_some()
            || self.welcome_doc_viewer.is_some()
            || self.tutorial.is_some()
            || matches!(self.active_view, ActiveView::AgentDashboard
                if self.dashboard.as_ref().is_some_and(|d| d.shortcuts_modal.is_some()))
            || cloud_modal_open
    }
    /// Store the resolved per-tip gates and propagate the prompt-relevant tips
    /// (undo + plan nudge) to every agent's prompt. Reused by startup and the
    /// settings live-apply path so a runtime toggle reaches existing agents.
    pub fn apply_contextual_hints(
        &mut self,
        resolved: echo_agent_runtime::util::config::ResolvedContextualHints,
    ) {
        self.contextual_hints = resolved;
        for agent in self.agents.values_mut() {
            agent
                .prompt
                .set_contextual_hints(resolved.undo, resolved.plan_mode);
        }
    }
    /// One-shot small-screen `/compact-mode` tip trigger, run at the top of
    /// every `draw`. Waits (without consuming the one-shot) until the active
    /// AGENT view has a stable, draw-measured size — so a welcome screen, an
    /// undrawn agent, or a pending post-resize re-measure defer it. An
    /// out-of-band (or user-compact-on) first measure consumes the one-shot,
    /// so later resizes can never re-trigger. An in-band measure whose banner
    /// row is occluded (permission ask, modal, open dropdown, session banner)
    /// defers instead of consuming: the show gate would refuse it anyway, and
    /// spending the run's only evaluation on an invisible frame would kill
    /// the hint for the run.
    pub(crate) fn maybe_trigger_small_screen_tip(&mut self) {
        if self.small_screen_tip_evaluated {
            return;
        }
        let ActiveView::Agent(id) = self.active_view else {
            return;
        };
        let Some(agent) = self.agents.get(&id) else {
            return;
        };
        if agent.terminal_size_stale || agent.last_terminal_size == (0, 0) {
            return;
        }
        if !crate::tips::small_screen::small_screen_band_contains(agent.last_terminal_size.1)
            || self.current_ui.compact_mode
        {
            self.small_screen_tip_evaluated = true;
            return;
        }
        if !agent.ephemeral_tip_can_render() {
            return;
        }
        self.small_screen_tip_evaluated = true;
        super::dispatch::show_small_screen_tip(self);
    }
    /// One-shot SSH `echoagent wrap` tip trigger, run at the top of every `draw`
    /// right after [`Self::maybe_trigger_small_screen_tip`]. The welcome
    /// screen has no ephemeral-tip row, so the first stable agent-view draw
    /// is the earliest surface that can paint a session-load tip. Reads the
    /// live environment (cached statics) and delegates to the injectable
    /// inner so tests never depend on the host's SSH shape.
    pub(crate) fn maybe_trigger_ssh_wrap_tip(&mut self) {
        if self.ssh_wrap_tip_evaluated {
            return;
        }
        static ENV_RECOMMENDS_WRAP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let env_recommends_wrap = *ENV_RECOMMENDS_WRAP.get_or_init(|| {
            let ctx = crate::terminal::terminal_context();
            crate::diagnostics::ssh_wrap_hint(
                ctx.is_ssh,
                crate::diagnostics::probes::osc52_sink_active(),
                ctx.is_official_vscode_remote,
            )
            .is_some()
        });
        self.maybe_trigger_ssh_wrap_tip_inner(env_recommends_wrap);
    }
    /// Inner trigger with the environment verdict injected
    /// (`diagnostics::ssh_wrap_hint` on the live path). Same
    /// defer-vs-consume rules as the small-screen trigger above, with two
    /// deltas: the environment gates are process-constant, so a failing
    /// verdict consumes the one-shot; and a busy tip slot defers instead of
    /// replacing — both session-load tips can qualify on the same first
    /// draw, and replacing would burn the other tip's once-per-session show,
    /// while this one loses nothing by waiting for a later draw.
    pub(crate) fn maybe_trigger_ssh_wrap_tip_inner(&mut self, env_recommends_wrap: bool) {
        if self.ssh_wrap_tip_evaluated {
            return;
        }
        let ActiveView::Agent(id) = self.active_view else {
            return;
        };
        let Some(agent) = self.agents.get(&id) else {
            return;
        };
        if agent.terminal_size_stale || agent.last_terminal_size == (0, 0) {
            return;
        }
        if !env_recommends_wrap {
            self.ssh_wrap_tip_evaluated = true;
            return;
        }
        if !agent.ephemeral_tip_can_render() || agent.ephemeral_tip.is_active() {
            return;
        }
        self.ssh_wrap_tip_evaluated = true;
        super::dispatch::show_ssh_wrap_tip(self);
    }
    /// Whether the clipboard-image tip may poll right now — the single in-window
    /// gate. Outside it the poll touches the pasteboard ZERO times: contextual
    /// hints on, the probe supported (macOS), past the fire cooldown, the
    /// terminal focused, and the active agent eligible (the tip row can paint,
    /// no image chips attached, an image-capable model). Cooldown is part of the
    /// gate so a recently-fired tip suppresses even the cheap changeCount read.
    fn clipboard_tip_in_poll_window(&self, now: std::time::Instant) -> bool {
        self.contextual_hints.image_input
            && crate::clipboard::clipboard_image_probe_supported()
            && !self.clipboard_focus_tip.in_cooldown(now)
            && self.notification_service.focus_tracker.is_focused()
            && match self.active_view {
                ActiveView::Agent(id) => self
                    .agents
                    .get(&id)
                    .is_some_and(AgentView::clipboard_image_tip_eligible),
                _ => false,
            }
    }
    /// Opportunistic, throttled clipboard-image poll. Driven from event-loop
    /// iterations that already run for another reason (input, FocusGained,
    /// resize, an animation tick) — never from a timer and never by forcing
    /// `needs_animation`, so an idle/hibernating/unfocused app polls zero times.
    /// In-window it does at most one cheap `changeCount` read per `POLL_INTERVAL`
    /// and pays for the heavier type classification ONLY on a changeCount delta.
    /// Returns true when the tip was shown (needs redraw).
    pub(crate) fn poll_clipboard_focus_tip(&mut self) -> bool {
        let now = std::time::Instant::now();
        if !self.clipboard_tip_in_poll_window(now) {
            return false;
        }
        let outcome = self.clipboard_focus_tip.poll(
            now,
            crate::clipboard::clipboard_change_count,
            crate::tips::clipboard_focus::run_clipboard_check,
        );
        match outcome {
            Some(outcome) => self.apply_clipboard_probe(outcome, now),
            None => false,
        }
    }
    /// Decide + show for a probe outcome, committing the cooldown/dedup only
    /// when the show actually lands. Split out so the show/commit logic is
    /// unit-testable with a synthetic [`CheckOutcome`] (the native probe reads
    /// real hardware).
    fn apply_clipboard_probe(
        &mut self,
        outcome: crate::tips::clipboard_focus::CheckOutcome,
        now: std::time::Instant,
    ) -> bool {
        if !self.clipboard_focus_tip.should_fire(&outcome, now) {
            return false;
        }
        let ActiveView::Agent(id) = self.active_view else {
            return false;
        };
        let Some(agent) = self.agents.get_mut(&id) else {
            return false;
        };
        if agent.show_ephemeral_tip(
            crate::tips::clipboard_focus::clipboard_image_tip(),
            &mut self.tip_seen_counts,
        ) {
            self.clipboard_focus_tip.note_fired(&outcome, now);
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::ContextualTip {
                    tip: echo_agent_telemetry::events::ContextualTipKind::ImageInput,
                    action: echo_agent_telemetry::events::ContextualTipAction::Shown,
                },
            );
            return true;
        }
        false
    }
    /// Advance animation timers and drain tracing channel.
    ///
    /// Called at a fixed rate (~30fps) from the event loop. Produces
    /// redraws when there are running entries with animated accents,
    /// when a pending action expires (to clear the "press again" hint),
    /// or when new tracing entries arrive via the channel.
    pub fn tick(&mut self) -> bool {
        let mut needs_redraw = false;
        needs_redraw |= self.minimal_state.transcript.is_some();
        needs_redraw |= self.poll_clipboard_focus_tip();
        if matches!(self.active_view, ActiveView::Welcome) {
            self.welcome_tick = self.welcome_tick.wrapping_add(1);
            if let Some(expires_at) = self.welcome_toast.as_ref().map(|(_, at)| *at) {
                if std::time::Instant::now() >= expires_at {
                    self.welcome_toast = None;
                }
                needs_redraw = true;
            }
            if self.session_picker_content_loading
                || crate::views::session_picker::loading_spinner_active(
                    self.session_picker_entries.as_deref(),
                    self.session_picker_source_filter,
                    self.session_picker_loading,
                    &self.session_picker_lanes,
                )
            {
                needs_redraw = true;
            } else {
                let frame = crate::views::welcome::shimmer_frame();
                if frame != self.welcome_shimmer_frame {
                    self.welcome_shimmer_frame = frame;
                    needs_redraw = true;
                }
            }
        }
        if matches!(self.active_view, ActiveView::AgentDashboard)
            && let Some(d) = self.dashboard.as_mut()
        {
            d.spinner_tick = d.spinner_tick.wrapping_add(1);
            needs_redraw = true;
            d.dispatch.poll_file_search();
            d.peek_reply.poll_file_search();
        }
        if let Some(pending) = &self.pending_action
            && pending.expired()
        {
            self.pending_action = None;
            needs_redraw = true;
        }
        if let Some(rx) = &mut self.tracing_rx {
            while rx.try_recv().is_ok() {}
        }
        let mut bootstrap_commands_update: Option<Vec<agent_client_protocol::AvailableCommand>> =
            None;
        for agent in self.agents.values_mut() {
            needs_redraw |= agent.edit_hl_tick();
            for child in agent.subagent_views.values_mut() {
                needs_redraw |= child.edit_hl_tick();
            }
        }
        if let ActiveView::Agent(id) = self.active_view
            && let Some(agent) = self.agents.get_mut(&id)
        {
            needs_redraw |= agent.scrollback.tick();
            needs_redraw |= agent.todo.list_state.tick();
            needs_redraw |= agent.tasks.tick();
            for child_view in agent.subagent_views.values_mut() {
                needs_redraw |= child_view.scrollback.tick();
                needs_redraw |= child_view.tick_toast();
                needs_redraw |= child_view.tick_ephemeral_tip();
                needs_redraw |= child_view.tick_mode_banner();
                needs_redraw |= child_view.tick_selection_highlight();
                needs_redraw |= child_view.tick_drag_autoscroll();
                needs_redraw |= child_view.poll_link_modifier();
                needs_redraw |= child_view.poll_scrollback_search();
                needs_redraw |= child_view.mermaid_tick();
                needs_redraw |= Self::tick_agent_image_load(child_view);
                needs_redraw |= Self::tick_agent_block_viewer(child_view);
            }
            let spinner_frame_tick =
                agent.scrollback.animation_tick() % crate::views::turn_status::SPINNER_DIVISOR == 0;
            needs_redraw |= !agent.session.state.is_idle() && spinner_frame_tick;
            needs_redraw |= agent
                .mcp_init_progress
                .as_ref()
                .is_some_and(McpInitProgress::is_visible)
                && spinner_frame_tick;
            needs_redraw |= matches!(
                agent.btw_state,
                Some(crate::views::btw_overlay::BtwOverlayState::Loading { .. })
            ) && spinner_frame_tick;
            needs_redraw |= matches!(
                agent.active_modal.as_ref(),
                Some(crate::views::modal::ActiveModal::SessionPicker {
                    entries,
                    loading,
                    lanes,
                    source_filter,
                    ..
                }) if crate::views::session_picker::loading_spinner_active(
                    entries.as_deref(),
                    *source_filter,
                    *loading,
                    lanes,
                )
            ) && spinner_frame_tick;
            needs_redraw |= agent.drain_blocked();
            agent.prompt.slash_controller.set_workflows_available(
                agent
                    .session
                    .available_commands
                    .iter()
                    .any(|c| c.name == "workflow")
                    || !agent.workflow_runs.is_empty(),
            );
            agent.prompt.slash_controller.set_workflow_runs(
                agent
                    .workflow_runs
                    .iter()
                    .map(|run| crate::slash::command::WorkflowRunChoice {
                        name: run.name.clone(),
                        status: run.status.clone(),
                        builtin: run.builtin,
                    })
                    .collect(),
            );
            if agent.acp_synced_generation != agent.session.available_commands_generation {
                agent.prompt.sync_acp_commands(
                    &agent.session.available_commands,
                    agent.session.available_tools.as_ref(),
                    &agent.session.models,
                );
                agent.acp_synced_generation = agent.session.available_commands_generation;
                bootstrap_commands_update = Some(agent.session.available_commands.clone());
                needs_redraw = true;
            }
            needs_redraw |= agent.prompt.poll_file_search();
            needs_redraw |= agent.prompt.history_search.poll();
            needs_redraw |= agent.poll_scrollback_search();
            needs_redraw |= agent.tick_toast();
            needs_redraw |= agent.tick_extensions_result_notice();
            needs_redraw |= agent.tick_ephemeral_tip();
            needs_redraw |= agent.tick_mode_banner();
            needs_redraw |= agent.tick_selection_highlight();
            needs_redraw |= agent.tick_drag_autoscroll();
            needs_redraw |= agent.poll_link_modifier();
            needs_redraw |= Self::tick_agent_image_load(agent);
            needs_redraw |= Self::tick_agent_block_viewer(agent);
            if let Some(ref mut viewer) = agent.video_viewer {
                needs_redraw |= viewer.tick();
            }
            if let Some(ref mut gboom) = agent.gboom {
                gboom.tick();
                needs_redraw = true;
            }
            if let Some(ref rx) = agent.video_load_rx
                && let Ok(result) = rx.try_recv()
            {
                agent.video_load_rx = None;
                match result {
                    Some(video) => {
                        agent.replace_inline_video(video);
                        agent.toast = None;
                    }
                    None => {
                        agent.show_toast("Video playback requires ffmpeg");
                    }
                }
                needs_redraw = true;
            }
            needs_redraw |= agent.mermaid_tick();
            if let Some(ref mut video) = agent.inline_video
                && !video.finished
                && !video.frames.is_empty()
            {
                let elapsed = video.last_frame_time.elapsed();
                let frame_dur = std::time::Duration::from_secs_f64(1.0 / video.fps);
                if elapsed >= frame_dur {
                    if video.current_frame + 1 >= video.frames.len() {
                        video.finished = true;
                    } else {
                        video.current_frame += 1;
                        video.last_frame_time = std::time::Instant::now();
                    }
                    needs_redraw = true;
                }
            }
        }
        if let Some(commands) = bootstrap_commands_update {
            self.welcome_prompt
                .sync_acp_commands(&commands, None, &self.models);
            if let Some(d) = self.dashboard.as_mut() {
                d.dispatch.sync_acp_commands(&commands, None, &self.models);
            }
            self.bootstrap_acp_commands = commands;
        }
        self.update_notifications();
        if let Some((_, remaining)) = self.deferred_notification.as_mut() {
            if *remaining == 0 {
                let event = self.deferred_notification.take().unwrap().0;
                self.notification_service.notify(event);
            } else {
                *remaining -= 1;
            }
        }
        needs_redraw |= self.tick_scroll();
        self.update_status_line();
        needs_redraw |= self.status_line.take_changed();
        needs_redraw
    }
    /// Flush pending scroll lines (stream gap detection, redraw cadence).
    /// Without this, stale streams are never finalized after the user stops
    /// scrolling, and sub-line fractional remainders may not be flushed.
    ///
    /// Primarily driven by the event loop's scroll clock, armed from
    /// [`MouseScrollState::scroll_clock_deadline`] while a stream is active so
    /// residuals land on the 16ms redraw cadence (not the animation fps) and
    /// the 80ms stream-gap finalize fires on time. Returns true only when
    /// lines were dispatched — i.e. a draw would show real movement.
    pub(crate) fn tick_scroll(&mut self) -> bool {
        let mut needs_redraw = false;
        let had_scroll_stream = self.scroll_state.has_active_stream();
        let scroll_update = self.scroll_state.on_tick();
        if scroll_update.lines != 0
            && let Some((col, row)) = self.last_scroll_pos
            && !self.is_scroll_blocking_modal_open()
        {
            self.dispatch_scroll(scroll_update.lines, col, row);
            needs_redraw = true;
        }
        if had_scroll_stream && !self.scroll_state.has_active_stream() {
            self.last_scroll_pos = None;
        }
        needs_redraw
    }
    /// Whether the `/gboom` easter egg is open on the active agent view.
    /// While active it owns input, so the event loop preserves key-release
    /// events for it and bypasses paste coalescing.
    pub(crate) fn gboom_active(&self) -> bool {
        matches!(self.active_view, ActiveView::Agent(id)
            if self.agents.get(&id).is_some_and(|a| a.gboom.is_some()))
    }
    /// Un-latch held movement on every open `/gboom` game.
    ///
    /// In release-aware (Kitty) mode a key stays latched until its release
    /// event arrives. On window focus loss the active game's release may be
    /// dropped, so clear all games' holds to stop runaway motion.
    pub(crate) fn gboom_release_all_games(&mut self) {
        for agent in self.agents.values_mut() {
            if let Some(gboom) = agent.gboom.as_mut() {
                gboom.release_all();
            }
        }
    }
    /// Un-latch held movement on every `/gboom` game that is *not* the active
    /// input target. Only the active game receives release events; a key
    /// still held when the user switches agent tabs (or to any other view)
    /// would otherwise leave that backgrounded game walking or turning with
    /// no key down when it is next reopened. Reconciled every event-loop
    /// iteration while a game is open, so it holds regardless of which view
    /// becomes active or whether the shared keyboard layer stays pushed.
    pub(crate) fn gboom_release_backgrounded_games(&mut self) {
        let active = match self.active_view {
            ActiveView::Agent(id) => Some(id),
            _ => None,
        };
        for (id, agent) in self.agents.iter_mut() {
            if Some(*id) != active
                && let Some(gboom) = agent.gboom.as_mut()
            {
                gboom.release_all();
            }
        }
    }
    /// Tick-interval ceiling requested by the current view state, if any.
    ///
    /// The `/gboom` easter egg targets ~30 fps even when the user configured
    /// a lower `animation.fps`; the simulation steps with wall-clock `dt`,
    /// so this only affects smoothness, never game speed.
    pub fn tick_interval_ceiling(&self) -> Option<std::time::Duration> {
        if self.gboom_active() {
            return Some(std::time::Duration::from_millis(33));
        }
        if self.minimal_state.transcript.is_some() {
            return Some(std::time::Duration::from_millis(16));
        }
        None
    }
    /// Deferred image viewer load (background thread). Shared by parent agent
    /// and fullscreen subagent children so gate/tick stay symmetric.
    fn tick_agent_image_load(agent: &mut AgentView) -> bool {
        if let Some(ref mut viewer) = agent.image_viewer
            && viewer.loading
        {
            if agent.image_load_rx.is_none()
                && let Some(path) = viewer.take_source_path()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                agent.image_load_rx = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(crate::prompt_images::load_image_data(&path));
                });
            }
            if let Some(ref rx) = agent.image_load_rx {
                use crate::prompt_images::ImageLoadResult;
                match rx.try_recv() {
                    Ok(ImageLoadResult::Loaded(data)) => {
                        viewer.apply_loaded(data);
                        agent.image_load_rx = None;
                    }
                    Ok(ImageLoadResult::Failed)
                    | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        agent.image_viewer = None;
                        agent.image_load_rx = None;
                        agent.toast = Some(("Couldn't load image preview".into(), 6));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }
            return true;
        }
        false
    }
    /// Block viewer streaming / follow-mode ticks (parent or subagent child).
    fn tick_agent_block_viewer(agent: &mut AgentView) -> bool {
        let mut needs_redraw = false;
        if let Some(ref mut viewer) = agent.block_viewer {
            if viewer.kind == crate::views::block_viewer::ViewerKind::BgTask
                && let Some(ref task_id) = viewer.bg_task_id.clone()
                && let Some(task) = agent.session.bg_tasks.get(task_id)
            {
                let is_running = task.status == crate::app::agent::BgTaskStatus::Running;
                needs_redraw |= viewer.tick_bg_task(&task.stdout, is_running);
            } else if let Some(entry) = agent.scrollback.get_by_id(viewer.entry_id) {
                needs_redraw |= viewer.tick(entry);
            } else {
                agent.block_viewer = None;
                needs_redraw = true;
            }
        }
        needs_redraw
    }
    /// Check if animation ticks should be scheduled.
    pub fn needs_animation(&self) -> bool {
        self.tick_demand() != TickDemand::None
    }
    /// What tick cadence the current view state demands.
    ///
    /// [`TickDemand::Fast`] runs at the configured animation fps (default
    /// 30). [`TickDemand::Slow`] runs at [`SLOW_TICK_INTERVAL`] and is used
    /// when the only reasons to tick are low-frequency by construction —
    /// the ~12fps welcome logo shimmer and the macOS Cmd link-hover poll —
    /// so an app that *looks* idle doesn't spin a 30fps loop for them.
    pub fn tick_demand(&self) -> TickDemand {
        self.view_tick_demand().max(self.status_line_tick_demand())
    }
    fn view_tick_demand(&self) -> TickDemand {
        if self.pending_action.is_some() {
            return TickDemand::Fast;
        }
        if self.minimal_state.transcript.is_some() {
            return TickDemand::Fast;
        }
        if self
            .agents
            .values()
            .any(|a| a.pending_turn_end_reconcile.is_some())
        {
            return TickDemand::Fast;
        }
        if self.agents.values().any(|a| {
            a.pending_cancel_resend.is_some()
                || a.subagent_views
                    .values()
                    .any(|c| c.pending_cancel_resend.is_some())
        }) {
            return TickDemand::Fast;
        }
        if self.deferred_notification.is_some() {
            return TickDemand::Fast;
        }
        if self.voice_listening() {
            return TickDemand::Fast;
        }
        if self.session_picker_content_loading {
            return TickDemand::Fast;
        }
        if self.agents.values().any(|agent| {
            agent.edit_hl_needs_tick()
                || agent
                    .subagent_views
                    .values()
                    .any(|c| c.edit_hl_needs_tick())
        }) {
            return TickDemand::Fast;
        }
        match self.active_view {
            ActiveView::Agent(id) => {
                let Some(agent) = self.agents.get(&id) else {
                    return TickDemand::None;
                };
                let fast = agent.scrollback.needs_animation()
                    || agent.todo.list_state.needs_tick()
                    || agent.tasks.needs_tick()
                    || agent.acp_synced_generation != agent.session.available_commands_generation
                    || !agent.session.state.is_idle()
                    || agent.wake_turn_active()
                    || agent.session.loading_replay
                    || agent
                        .mcp_init_progress
                        .as_ref()
                        .is_some_and(McpInitProgress::is_visible)
                    || agent.plugin_cta.phase.is_spinner()
                    || matches!(
                        agent.btw_state,
                        Some(crate::views::btw_overlay::BtwOverlayState::Loading { .. })
                    )
                    || agent.drain_blocked()
                    || agent.prompt.file_search.context().is_some()
                    || agent.prompt.history_search.is_active()
                    || agent.scrollback_search.is_some()
                    || agent.line_viewer.is_some()
                    || agent.toast.is_some()
                    || agent
                        .extensions_modal
                        .as_ref()
                        .is_some_and(|m| m.result_notice.is_some())
                    || agent.ephemeral_tip_needs_tick()
                    || agent.mode_switch_banner.is_some()
                    || agent.has_drag_autoscroll()
                    || agent.selection_created_at.is_some()
                    || agent.block_viewer.is_some()
                    || agent.image_viewer.as_ref().is_some_and(|v| v.loading)
                    || agent.image_load_rx.is_some()
                    || agent.video_viewer.as_ref().is_some_and(|v| v.playing)
                    || agent.gboom.is_some()
                    || agent.inline_video.as_ref().is_some_and(|v| !v.finished)
                    || agent.video_load_rx.is_some()
                    || agent.mermaid_needs_tick()
                    || !agent.permission_queue.is_empty()
                    || matches!(
                        agent.active_modal.as_ref(),
                        Some(crate::views::modal::ActiveModal::SessionPicker {
                            entries,
                            loading,
                            lanes,
                            source_filter,
                            ..
                        }) if crate::views::session_picker::loading_spinner_active(
                            entries.as_deref(),
                            *source_filter,
                            *loading,
                            lanes,
                        )
                    )
                    || agent.subagent_views.iter().any(|(sid, child)| {
                        child.toast.is_some()
                            || child.ephemeral_tip_needs_tick()
                            || child.mode_switch_banner.is_some()
                            || child.has_drag_autoscroll()
                            || child.selection_created_at.is_some()
                            || (agent.active_subagent.as_deref() == Some(sid.as_str())
                                && child.scrollback.needs_animation())
                            || child.any_cancel_pending()
                            || child.scrollback_search.is_some()
                            || child.block_viewer.is_some()
                            || child.image_viewer.as_ref().is_some_and(|v| v.loading)
                            || child.image_load_rx.is_some()
                            || child.mermaid_needs_tick()
                    });
                if fast {
                    return TickDemand::Fast;
                }
                if cfg!(target_os = "macos")
                    && (agent.needs_link_modifier_poll()
                        || agent
                            .subagent_views
                            .values()
                            .any(|child| child.needs_link_modifier_poll()))
                {
                    return TickDemand::Slow;
                }
                TickDemand::None
            }
            ActiveView::AgentDashboard => {
                let agents_need = self.agents.values().any(|agent| {
                    !agent.session.state.is_idle()
                        || !agent.permission_queue.is_empty()
                        || agent.session.loading_replay
                        || agent
                            .subagent_sessions
                            .values()
                            .any(|info| !info.finished && info.workflow_run_id.is_none())
                        || agent.workflow_runs.iter().any(|run| run.is_active())
                });
                let dash_search = self.dashboard.as_ref().is_some_and(|d| {
                    d.dispatch.file_search.context().is_some()
                        || d.peek_reply.file_search.context().is_some()
                });
                if agents_need || dash_search {
                    TickDemand::Fast
                } else {
                    TickDemand::None
                }
            }
            ActiveView::Welcome => TickDemand::Slow,
        }
    }
    /// Update the terminal tab title and OSC 9;4 progress bar.
    ///
    /// Stores any resulting escape sequences in `pending_notification_escapes`
    /// so that the next `draw()` can pipe them through the frame's
    /// `post_flush_escapes` (inside the synchronized output block).
    ///
    /// Also clears the permission notification flag when no permissions
    /// remain queued, so the next batch fires a fresh bell/popup.
    pub fn update_notifications(&mut self) {
        let (session_name, model, activity, has_perms, turn_elapsed, is_busy) =
            if let ActiveView::Agent(id) = self.active_view
                && let Some(agent) = self.agents.get(&id)
            {
                let name = agent
                    .display_name
                    .as_deref()
                    .or(agent.generated_session_title.as_deref());
                let model = agent.session.models.current_model_name();
                let parked = agent.renders_parked();
                let activity = if parked {
                    None
                } else {
                    agent.resolve_turn_activity()
                };
                let has_perms = !agent.permission_queue.is_empty();
                let elapsed = if parked { None } else { agent.turn_elapsed() };
                let is_busy = agent.session.state.is_busy() && !parked;
                (name, model, activity, has_perms, elapsed, is_busy)
            } else {
                (None, None, None, false, None, false)
            };
        let any_agent_has_perms = self.agents.values().any(|a| !a.permission_queue.is_empty());
        if !any_agent_has_perms {
            self.notification_service.clear_permission_notification();
        }
        let cwd_str = self.cwd.to_string_lossy();
        let title_state = crate::notifications::TitleState {
            session_name,
            model: model.as_deref(),
            activity: activity.as_ref(),
            has_pending_permissions: has_perms,
            cwd: Some(&cwd_str),
            turn_elapsed,
            is_busy,
            focused: self.notification_service.focus_tracker.is_focused(),
        };
        if let Some(esc) = self.notification_service.on_tick(&title_state) {
            self.pending_notification_escapes
                .get_or_insert_with(String::new)
                .push_str(&esc);
        }
    }
}
#[cfg(test)]
#[path = "app_view_tests.rs"]
pub(crate) mod tests;
