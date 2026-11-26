//! Per-agent view component.
//!
//! [`AgentView`] is a view-model that owns both business state (session,
//! entries) and UI state (scroll, selection, focus, mode). It handles
//! input routing to the active pane and renders the agent layout.
//!
//! ## Input flow (three-level bubbling)
//!
//! ```text
//! key press
//!   → overlays / modals / dropdowns / voice / search / selection steal Esc first
//!   → 1. pane level (exact context match):
//!       prompt focused:
//!         prompt.handle_key(key) → Submit / Edited / Ignored
//!         if Ignored → registry.lookup(key, PromptFocused) → pane-specific actions
//!         if still unmatched → Tab structural FocusScrollback
//!       scrollback focused:
//!         Space/i → FocusPrompt
//!         registry.lookup(key, ScrollbackFocused) → navigation, view, etc.
//!   → 2. agent level (if pane returned Unchanged):
//!       registry.lookup(key, AgentScreen) → CancelTurn (Ctrl+C), ToggleYolo, NextModel
//!       CancelTurn has runtime guards (Running→cancel, Cancelling→quit on Ctrl+C).
//!       From the prompt pane, Ctrl+C CancelTurn is a two-step gesture:
//!       a non-empty prompt skips the AgentScreen promotion so the key
//!       falls through to the widget's clear path; the next Ctrl+C (now
//!       on an empty prompt) re-enters this level and runs CancelTurn.
//!   → 3. Esc policy (try_handle_esc_policy) on Prompt or Scrollback only,
//!       after overlays/dropdowns/selection returned Changed / stole Esc:
//!       turn running, gate ON (`esc_cancels_turn`: minimal mode OR
//!         `[ui].vim_mode` off) → CancelTurn (even with a draft; the draft
//!         is preserved, unlike Ctrl+C's clear-first gesture)
//!       turn running, gate OFF (fullscreen vim mode) → Changed (swallow)
//!       turn cancelling → CancelTurn in every mode (retry lost ack;
//!         Ctrl+C escalates to Quit)
//!       idle + non-empty prompt, prompt pane only → ArmPending ClearPrompt (2× within 800ms, hint)
//!       idle + empty + messages, either pane (Normal composer mode, no
//!         needs-input overlay pending, no open history search, and not
//!         within ESC_CANCEL_REWIND_GRACE of an Esc-fired cancel) →
//!         ArmPending RewindShowPicker (2×, silent)
//!       idle otherwise (scrollback-pane draft / latent mode / pending overlay /
//!         open history search / post-cancel grace, or empty + no messages) →
//!         Changed (swallow Esc; not FocusScrollback)
//!   → 4. return Unchanged → bubbles to app_view for global actions (quit)
//! ```
//!
//! The mid-turn cancel is the only Esc-policy branch gated on `[ui].vim_mode`
//! (scrollback nav); everything else — and all of it with respect to
//! `[ui].simple_mode` (prompt editor) — is mode-independent. Tab remains
//! leave-prompt in both modes.
//!
//! ## Future: data/view split
//!
//! When multi-agent views arrive (swarm overview, aggregate stats), we'll
//! need views that read from MULTIPLE agents. At that point, split into:
//! - `AgentData` — entries, session, tracker (shared, view-agnostic)
//! - `AgentViewState` — scroll, selection, folds (per-view instance)
//!
//! See `plan/app-reorg.md` for details.
//!
//! ## Module organization
//!
//! The `AgentView` struct, its supporting types, and shared free functions
//! live here; the `impl AgentView` method clusters live in the per-domain
//! child modules declared below (input routing, rendering, selection, panes,
//! modals, ...). Child modules import from `super::` with at most one hop;
//! their `#[cfg(test)]` mods share `test_fixtures` below.
use crate::actions::ActionId;
use crate::key;
use crate::render::SafeBuf;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
/// Hit areas for inline media buttons, rebuilt each frame.
///
/// All hit areas are cleared at the start of inline media rendering and
/// repopulated only when the media is visible. This ensures scroll
/// resilience — stale hit areas are never left around.
#[derive(Debug, Clone, Default)]
pub(crate) struct InlineMediaHitAreas {
    /// Inline image areas (image overlay): clicking opens the file natively.
    pub media_areas: Vec<(ratatui::layout::Rect, std::path::PathBuf)>,
    /// Video poster areas: clicking starts/restarts inline playback.
    pub video_play_areas: Vec<(ratatui::layout::Rect, std::path::PathBuf)>,
    /// `[Play]` button rects: clicking starts/replays inline video.
    pub play_buttons: Vec<(ratatui::layout::Rect, std::path::PathBuf)>,
    /// `[Open]` button rects (overlay and text fallback): open the file natively.
    pub open_buttons: Vec<(ratatui::layout::Rect, std::path::PathBuf)>,
    /// `[Copy]` button rects (images only): clicking copies the image.
    pub copy_image_buttons: Vec<(ratatui::layout::Rect, std::path::PathBuf)>,
    /// Filepath line rects: clicking copies the path to the clipboard.
    pub filepath_areas: Vec<(ratatui::layout::Rect, std::path::PathBuf)>,
    /// Mermaid affordance-row button rects: `(rect, kind, source_idx)` where
    /// `source_idx` indexes [`mermaid_sources`](Self::mermaid_sources). `[Open]`/
    /// `[Copy path]` render the source lazily on click; `[Copy source]` copies
    /// it. Indexing keeps each diagram's source cloned once per frame, not per
    /// button.
    pub mermaid_buttons: Vec<(
        ratatui::layout::Rect,
        crate::scrollback::blocks::mermaid_content::AffordanceKind,
        usize,
    )>,
    /// Diagram sources for the visible affordance rows; [`mermaid_buttons`] index
    /// into this (one entry per visible diagram).
    ///
    /// [`mermaid_buttons`]: Self::mermaid_buttons
    pub mermaid_sources: Vec<String>,
}
/// Inline video playback state for scrollback media entries.
///
/// Created when the user clicks or presses Enter on a video poster frame.
/// Frames are extracted via ffmpeg in a background thread. Playback
/// advances one frame per tick, stops on the last frame (no loop).
#[derive(Debug)]
pub(crate) struct InlineVideoState {
    /// Video file path (used to match against visible placements).
    pub path: std::path::PathBuf,
    /// Pre-extracted frames, protocol-prepared (PNG for Kitty, JPEG for iTerm2).
    pub frames: Vec<Vec<u8>>,
    /// Current frame index (0-based).
    pub current_frame: usize,
    /// Timestamp of last frame advance (for fps pacing).
    pub last_frame_time: std::time::Instant,
    /// Target playback frame rate.
    pub fps: f64,
    /// True after the last frame has been displayed.
    pub finished: bool,
}
use super::actions::Action;
use super::agent::AgentSession;
use super::app_view::InputOutcome;
use super::cancel_latency::CancelLatency;
use crate::scrollback::EntryId;
use crate::scrollback::ScrollbackSearchState;
use crate::scrollback::state::ScrollbackState;
use crate::scrollback::text_selection::{
    ActiveBlockDrag, ActiveTextDrag, DragAutoScrollState, PendingBlockDrag, PendingTextDrag,
    PersistentTextSelection, ResolvedSelectionBoundaries, ResolvedSelectionModel,
    TableSelectionGeometry,
};
use crate::theme::Theme;
pub use crate::views::agent::{ActivePane, AgentViewLayout, InputMode, PaneAreas};
use crate::views::block_viewer::BlockViewerPane;
use crate::views::elicitation_view::ElicitationViewState;
use crate::views::extensions_modal::ExtensionsModalState;
use crate::views::file_search::line_viewer::LineViewerState;
use crate::views::modal::{self, ActiveModal, ModalButtonHit};
use crate::views::permission_view::{PermissionViewState, SubagentInfo};
use crate::views::plan_approval_view::{PlanApprovalViewState, PlanComment};
use crate::views::prompt_widget::{PromptWidget, StashedPrompt};
use crate::views::question_view::QuestionViewState;
use crate::views::queue_pane::QueuePane;
use crate::views::subagent_catalog_pane::SubagentCatalogPane;
use crate::views::tasks_pane::TasksPane;
use crate::views::todo_pane::TodoPane;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
mod cta;
mod elicitation;
mod input;
pub(crate) use input::ExternalPromptEditorAccess;
mod interactions;
mod jump;
mod key_owner;
pub(crate) use key_owner::{BlockingCard, EscStep, KeyOwner};
mod links;
mod media;
mod modals;
mod notices;
mod panes;
mod paste;
mod plan;
mod prompt;
mod prompt_stash;
pub(in crate::app) use prompt_stash::prompt_history_text;
pub use prompt_stash::{PromptStashEntry, StashCause};
mod queue;
mod render;
pub use render::AppRenderParams;
mod rewind;
mod selection;
mod session;
mod shell_completion;
#[cfg(test)]
mod task_status_tests;
mod viewer;
mod workflows_overlay;
use super::actions;
use super::dispatch;
pub(super) fn active_contexts_for_pane(pane: ActivePane) -> Vec<crate::actions::When> {
    use crate::actions::When;
    match pane {
        ActivePane::Prompt => vec![When::PromptFocused, When::AgentScreen, When::Always],
        ActivePane::Scrollback => {
            vec![When::ScrollbackFocused, When::AgentScreen, When::Always]
        }
        _ => vec![When::AgentScreen, When::Always],
    }
}
/// Pane focus within the agent view.
///
/// This will grow as we add more panes (tasks, review files, etc.).
pub type AgentPane = ActivePane;
/// Per-agent view-model.
///
/// Owns both business state (session, entries) and UI state (scroll,
/// selection, pane focus). See module docs for future split plans.
/// MCP server initialization progress, received from the shell.
#[derive(Debug, Clone)]
pub struct McpInitProgress {
    pub total: u32,
    pub connected: u32,
    pub started_at: Instant,
}
impl McpInitProgress {
    /// Max age for a `total == 0` seed before it auto-expires.
    pub const SEED_EXPIRE: std::time::Duration = std::time::Duration::from_secs(30);
    /// Whether the progress indicator should be visible in the UI.
    ///
    /// - `total > 0` (real servers): always visible until
    ///   `echo.agent/mcp_initialized` clears the progress.
    /// - `total == 0` (seed / 0-server): visible for at most
    ///   [`SEED_EXPIRE`] seconds, then auto-expires as
    ///   defense-in-depth against the shell failing to send
    ///   `mcp_initialized`.
    pub fn is_visible(&self) -> bool {
        self.total > 0 || self.started_at.elapsed() < Self::SEED_EXPIRE
    }
}
#[cfg(test)]
mod mcp_init_progress_tests {
    use super::McpInitProgress;
    #[test]
    fn is_visible_requires_servers_or_fresh_seed() {
        let real = McpInitProgress {
            total: 3,
            connected: 1,
            started_at: std::time::Instant::now(),
        };
        assert!(real.is_visible(), "real progress must be visible");
        let fresh = McpInitProgress {
            total: 0,
            connected: 0,
            started_at: std::time::Instant::now(),
        };
        assert!(fresh.is_visible(), "fresh seed must be visible");
        let expired = McpInitProgress {
            total: 0,
            connected: 0,
            started_at: std::time::Instant::now()
                - McpInitProgress::SEED_EXPIRE
                - std::time::Duration::from_secs(1),
        };
        assert!(!expired.is_visible(), "expired seed must not be visible");
    }
}
/// Current voice record-dot pulse: `(filled, brightness)`.
///
/// A smooth sine "breathing" on a fixed ~0.7s wall-clock period (not the
/// animation tick), so the dot animates like a studio recording light and
/// never speeds up or syncs with streaming-text redraws. `filled` picks the
/// FISHEYE/BULLSEYE glyph; `brightness` (0.4–1.0) fades the red color.
fn record_dot_pulse() -> (bool, f32) {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    let phase = (epoch.elapsed().as_secs_f32() / 0.7).fract();
    let s = (phase * std::f32::consts::TAU).sin();
    let brightness = 0.4 + 0.6 * (0.5 + 0.5 * s);
    (s >= 0.0, brightness)
}
/// A clickable/hoverable screen region.
///
/// Tracks an optional screen rect (set during render) and whether the
/// mouse is currently hovering over it. Consolidates the repeated
/// `area: Option<Rect>` + `hovered: bool` pattern.
#[derive(Debug, Clone, Copy, Default)]
pub struct HitArea {
    pub rect: Option<Rect>,
    pub hovered: bool,
}
/// Privacy upsell banner state on the agent view: whether the banner owns
/// the banner slot this frame (`active`, set at draw start like
/// `session_banner_active`; persists until acted on, so it is a tip
/// occluder AND a tip-tick freezer) plus the four click targets.
#[derive(Debug, Default)]
pub struct PrivacyBannerState {
    pub(crate) active: bool,
    /// `[Opt in]` (opt in; ack only after ACP success).
    pub(crate) hit_opt_in: HitArea,
    /// `[Opt out]` (ack now; record the decline).
    pub(crate) hit_opt_out: HitArea,
    /// "Terms" link (opens the terms of service).
    pub(crate) hit_terms: HitArea,
    /// "Privacy Policy" link (opens the privacy policy).
    pub(crate) hit_policy: HitArea,
}
impl PrivacyBannerState {
    /// Drop all click targets (slot not painted this frame).
    pub fn clear_hits(&mut self) {
        self.hit_opt_in.clear();
        self.hit_opt_out.clear();
        self.hit_terms.clear();
        self.hit_policy.clear();
    }
}
/// Banner-slot inputs to [`AgentView::draw`]. Slot precedence is computed
/// by the caller (`AppView::draw`).
pub struct BannerSlotParams<'a> {
    /// Reserved slot height (0 = no slot this frame).
    pub(crate) height: u16,
    pub(crate) announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    pub(crate) hidden_ids: &'a std::collections::BTreeSet<String>,
    /// Privacy upsell banner owns the slot (highest banner precedence
    /// below critical announcements; gated by the caller).
    pub(crate) privacy_banner: bool,
    /// Last mouse position, for mouse-pos-driven hover styling.
    pub(crate) mouse_pos: Option<(u16, u16)>,
    /// Session tip, only when it owns the slot.
    pub(crate) tip: Option<&'a str>,
}
impl BannerSlotParams<'static> {
    /// No banner slot this frame.
    pub fn none() -> Self {
        static EMPTY_IDS: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        Self {
            height: 0,
            announcements: &[],
            hidden_ids: &EMPTY_IDS,
            privacy_banner: false,
            mouse_pos: None,
            tip: None,
        }
    }
}
impl HitArea {
    /// Update hover state for a mouse position. Returns `true` if changed.
    pub fn update_hover(&mut self, col: u16, row: u16) -> bool {
        let new = self.rect.is_some_and(|r| r.contains((col, row).into()));
        let changed = new != self.hovered;
        self.hovered = new;
        changed
    }
    /// Check if a position is inside the rect.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.rect.is_some_and(|r| r.contains((col, row).into()))
    }
    /// Set the rect (called during render).
    pub fn set(&mut self, rect: Option<Rect>) {
        self.rect = rect;
    }
    /// Like [`Self::set`], but drops the rect while a dropdown is open: dropdowns paint over these rows post-arm.
    pub fn set_unless_dropdown(&mut self, rect: Option<Rect>, dropdown_open: bool) {
        self.set(if dropdown_open { None } else { rect });
    }
    /// Clear rect and hover.
    pub fn clear(&mut self) {
        self.rect = None;
        self.hovered = false;
    }
}
pub use super::queue_edit::PromptMode;
/// Which special input mode the prompt is currently in.
///
/// These modes are **mutually exclusive**: only one can be active at a time.
/// `Normal` is the default. Each variant changes the prompt's visual appearance
/// (accent color, prefix, placeholder) and the action dispatched on Enter.
///
/// Orthogonal to `multiline_mode` ... and `PromptMode` ... and `InputMode`...
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptInputMode {
    /// Standard prompt: Enter sends `Action::SendPrompt`.
    #[default]
    Normal,
    /// Bash mode (`!` prefix): Enter sends `Action::SendBashCommand`.
    Bash,
    /// Remember mode (`#` prefix): Enter sends `Action::SendRememberNote`.
    Remember,
}
impl PromptInputMode {
    pub fn accent_color(self, theme: &Theme) -> Option<ratatui::style::Color> {
        match self {
            PromptInputMode::Normal => None,
            PromptInputMode::Bash => Some(theme.command),
            PromptInputMode::Remember => Some(theme.accent_remember),
        }
    }
    pub fn prefix_override(self, theme: &Theme) -> Option<(&'static str, ratatui::style::Color)> {
        match self {
            PromptInputMode::Normal => None,
            PromptInputMode::Bash => Some(("! ", theme.command)),
            PromptInputMode::Remember => Some(("# ", theme.accent_remember)),
        }
    }
    pub fn placeholder_override(self, multiline: bool) -> Option<&'static str> {
        match self {
            PromptInputMode::Normal | PromptInputMode::Bash => None,
            PromptInputMode::Remember => {
                if multiline {
                    Some("Save a memory note... (Enter for newline, Shift+Enter to save)")
                } else {
                    Some("Save a memory note... (Shift+Enter for multiline)")
                }
            }
        }
    }
    pub fn prompt_info_override(self) -> Option<&'static str> {
        match self {
            PromptInputMode::Normal => None,
            PromptInputMode::Bash => Some("Run shell command"),
            PromptInputMode::Remember => Some("Save memory note"),
        }
    }
    pub fn send_action(self, text: String) -> Action {
        match self {
            PromptInputMode::Normal => Action::SendPrompt(text),
            PromptInputMode::Bash => Action::SendBashCommand(text),
            PromptInputMode::Remember => Action::SendRememberNote(text),
        }
    }
    pub fn is_exit_key(self, key: &KeyEvent) -> bool {
        match self {
            PromptInputMode::Normal => false,
            PromptInputMode::Bash | PromptInputMode::Remember => {
                let ctrl_w = key!('w', CONTROL).matches(key);
                let ctrl_u = key!('u', CONTROL).matches(key);
                let ctrl_c = key!('c', CONTROL).matches(key);
                key.code == KeyCode::Backspace
                    || key.code == KeyCode::Esc
                    || ctrl_w
                    || ctrl_u
                    || ctrl_c
            }
        }
    }
}
/// Multi-click state for text-level selection (word/line).
///
/// Unlike block-level `last_click` (which tracks by entry_idx),
/// this tracks by (entry_idx, range_id, block_line_idx) to ensure
/// double-click word selection only triggers when clicking the same text region.
#[derive(Debug, Clone)]
pub struct TextClickState {
    pub time: Instant,
    pub entry_idx: usize,
    pub range_id: u16,
    pub block_line_idx: usize,
    pub col_within_range: u16,
    pub click_count: u8,
}
/// Maximum time (ms) between consecutive clicks to count as a multi-click.
pub(super) const MULTI_CLICK_TIMEOUT_MS: u128 = 300;
/// Minimum interval (ms) between clipboard toasts for rapid word/line
/// selections. Drag completions always show the toast regardless.
const CLIPBOARD_TOAST_DEBOUNCE_MS: u128 = 500;
/// Minimum interval (ms) between consecutive context-bar clicks. Each click
/// fires an async ACP `session/info` request, so without this debounce a
/// double/triple-click would spawn redundant backend round-trips and reopen
/// the modal multiple times.
pub(super) const CONTEXT_CLICK_DEBOUNCE_MS: u128 = 300;
/// Default highlight TTL when `keep_text_selection` is `flash`.
const DEFAULT_SELECTION_HIGHLIGHT_DURATION_MS: u64 = 150;
/// Duration of the transient mode-switch banner (shown above prompt on Shift+Tab).
/// 2 s full visibility + 0.3 s fade out @ 30 fps.
const MODE_BANNER_TOTAL_TICKS: u8 = 69;
/// Final portion of the banner lifetime spent fading out (full → invisible).
const MODE_BANNER_FADE_TICKS: u8 = 9;
/// Whether `Event::Paste(text)` should probe the clipboard for image
/// bytes / a file reference. See [`crate::clipboard::paste_payload_needs_clipboard_attachment_probe`].
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn bracketed_paste_should_probe(text: &str) -> bool {
    crate::clipboard::paste_payload_needs_clipboard_attachment_probe(text)
}
/// Check if the platform link-action modifier is held.
/// macOS: Cmd (via CoreGraphics). Linux/Windows: Ctrl (from mouse modifiers).
#[cfg(target_os = "macos")]
pub(super) fn is_link_modifier_held(_mouse_modifiers: KeyModifiers) -> bool {
    crate::input::macos_modifiers::snapshot().command
}
#[cfg(not(target_os = "macos"))]
pub(super) fn is_link_modifier_held(mouse_modifiers: KeyModifiers) -> bool {
    mouse_modifiers.contains(KeyModifiers::CONTROL)
}
/// Determine whether the link modifier is held during a key event.
///
/// On macOS, polls CoreGraphics directly (independent of the event's modifier bits).
/// On Linux/Windows, derives from the key event's modifier flags, with a
/// special case for Ctrl-key release events (where the modifier bit is still
/// set in the event but the physical key is no longer held).
fn is_link_modifier_for_key(key: &KeyEvent) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = key;
        crate::input::macos_modifiers::snapshot().command
    }
    #[cfg(not(target_os = "macos"))]
    {
        if key.kind == crossterm::event::KeyEventKind::Release
            && matches!(
                key.code,
                KeyCode::Modifier(
                    crossterm::event::ModifierKeyCode::LeftControl
                        | crossterm::event::ModifierKeyCode::RightControl
                )
            )
        {
            false
        } else {
            key.modifiers.contains(KeyModifiers::CONTROL)
        }
    }
}
fn supports_osc22() -> bool {
    crate::terminal::terminal_context()
        .hyperlink_capabilities()
        .osc22_cursor
}
pub(super) fn has_native_link_hover() -> bool {
    crate::terminal::terminal_context()
        .hyperlink_capabilities()
        .native_link_hover
}
/// Whether app Cmd/Ctrl+click should open this link (vs terminal bare-URL open).
pub(super) fn app_should_open_link_on_click(link: &crate::scrollback::VisibleLink) -> bool {
    app_should_open_link_on_click_with(
        crate::terminal::terminal_context()
            .hyperlink_capabilities()
            .native_plain_url_open,
        link,
    )
}
/// Test form of [`app_should_open_link_on_click`]. Yields only Standard-scheme
/// bare-URL-text hits when `native_plain_url_open` (labels/citations/file stay).
pub(super) fn app_should_open_link_on_click_with(
    native_plain_url_open: bool,
    link: &crate::scrollback::VisibleLink,
) -> bool {
    if !native_plain_url_open {
        return true;
    }
    let Some(url) = crate::render::osc8::resolve_link_target(&link.target)
        .and_then(|resolved| resolved.osc8_url)
    else {
        return true;
    };
    if !crate::app::link_opener::is_safe_to_open(
        &url,
        crate::terminal::hyperlinks::SchemeFilter::Standard,
    ) {
        return true;
    }
    !link.looks_like_bare_url_text()
}
/// Whether double/triple-click performs terminal-like word/paragraph text
/// selection (and copy) instead of toggling a fold.
///
/// Unified into the `keep_text_selection` setting (the `word_select` mode):
/// reads the live appearance cache, so a Settings-panel change applies without
/// a restart and can never drift from the highlight-persistence behavior. The
/// compile-time default (`flash`) keeps double-click as a fold-toggle;
/// `word_select` is a staged rollout via a remote flag.
pub(super) fn is_text_selection_on_double_click() -> bool {
    crate::appearance::cache::load_keep_text_selection().selects_word()
}
/// A driver-side turn-end broadcast awaiting its `session/prompt` RPC
/// response. See [`AgentView::pending_turn_end_reconcile`].
#[derive(Debug, Clone)]
pub(crate) struct PendingTurnEnd {
    /// The prompt whose turn the broadcast declared ended.
    pub prompt_id: String,
    /// `stopReason` from the broadcast (`"cancelled"`, `"end_turn"`, …).
    pub stop_reason: Option<String>,
    /// `agentResult` detail from the broadcast (error text, when present).
    pub agent_result: Option<String>,
    /// `_meta.cancellationCategory` from the broadcast (`"HookDenied"` picks
    /// the "blocked by a hook" marker over "cancelled by user"). `None` on
    /// older shells or plain user cancels.
    pub cancellation_category: Option<String>,
    /// `_meta.cancelTrigger` from the broadcast (`"send_now"` marks a
    /// cancel-and-send whose "Turn cancelled" marker is suppressed). `None`
    /// on older shells / non-cancel ends.
    pub cancel_trigger: Option<String>,
    /// When the broadcast arrived; the reconcile fires after
    /// [`super::dispatch::TURN_END_RECONCILE_GRACE`].
    pub received_at: std::time::Instant,
}
/// The wake turn currently streaming on this session. A wake turn is a turn
/// the shell starts on its own when background work finishes, to tell the
/// model about it; the pager never adopts one (`AgentState` stays `Idle`),
/// so this is the only record that one is in flight. Drives the [stop]/Esc/Ctrl+C cancel
/// affordance and the status-row chrome; lifecycle on
/// [`AgentView::note_streaming_wake_turn`].
#[derive(Debug, Clone)]
pub(crate) struct RunningWakeTurn {
    /// The wake turn's synthetic prompt id (`task-completed-…` family).
    pub prompt_id: String,
    /// True once a cancel was sent for it: the status row reads Cancelling
    /// and the cancel-resend reconcile stays armed until the terminal lands.
    pub cancel_sent: bool,
}
/// A cancel sent while the pane is in a cancelling state, awaiting proof the
/// shell received it. See [`AgentView::pending_cancel_resend`].
#[derive(Debug, Clone)]
pub(crate) struct PendingCancelResend {
    /// The turn the cancel targeted (the wake prompt id or the adopted
    /// prompt id; `None` for cancels with no adopted prompt, such as
    /// `/compact`); a record from another target is never reused.
    pub prompt_id: Option<String>,
    /// When the cancel was (last) sent.
    pub sent_at: std::time::Instant,
    /// Sends so far, capped at [`crate::app::dispatch::turn::CANCEL_RESEND_MAX_ATTEMPTS`].
    pub attempts: u8,
    /// The turn-end broadcast arrived, proving the cancel landed: the
    /// auto-resend stops, but the record stays so a manual retry can reuse
    /// the recorded subagent choice.
    pub confirmed: bool,
    /// The first cancel's subagent decision; retries replay it instead of
    /// escalating past a one-shot "Continue to run".
    pub cancel_subagents: bool,
    /// Replayed so a resend still arms the shell's task-wake barrier.
    pub trigger: crate::app::actions::CancelTrigger,
}
/// Turn-end hook runs held for the live turn's marker. See [`AgentView::pending_stop_hooks`].
#[derive(Debug, Clone, Default)]
pub(crate) struct PendingStopHooks {
    /// The turn the stash belongs to; a stash that can't be matched to the
    /// ending turn flushes standalone instead of attaching to its marker.
    pub prompt_id: Option<String>,
    /// `(event_name, runs)` per hook batch, in arrival order
    /// (`stop_failure` before `stop` on error turns).
    pub groups: Vec<(String, Vec<crate::scrollback::blocks::tool::HookRunEntry>)>,
}
/// Components for the deferred fork banner. Stored by
/// `dispatch_fork_resolved` and formatted into the final banner text
/// in `TaskResult::SessionLoaded` once the child's session id is known.
#[derive(Debug, Clone)]
pub(crate) struct PendingForkBanner {
    /// Full session id of the parent session.
    pub parent_sid: String,
    /// Whether the fork created a new worktree.
    pub worktree: bool,
}
/// A finish held until its spawn arrives. Output is stripped at insert.
#[derive(Debug, Clone)]
pub(crate) struct DeferredSubagentFinish {
    pub notification: echo_agent_runtime::extensions::notification::SessionNotification,
    pub inserted_at: std::time::Instant,
}
/// In-flight reconnect session reload.
///
/// Opened by [`AgentView::begin_session_reload`]: the pre-outage scrollback
/// and tracker are stashed here while the live fields point at fresh state
/// the `session/load` replay streams into. [`AgentView::finish_session_reload`]
/// then keeps the replayed state, merges a cursor-resolved live tail onto the
/// stash, or restores the stash wholesale on failure — so a failed or
/// superseded reload can never leave the transcript blank.
///
/// Restore scope: the stash covers the transcript-critical state below (plus
/// the todo list). Satellite state the replay also mutates —
/// `subagent_sessions`/`subagent_views`, bg/scheduled tasks,
/// `available_commands`, context usage — is NOT restored on failure: live
/// updates keep routing through those maps during the window, so stashing
/// them would break mid-window routing, and they re-converge on the next
/// successful reload. A `SubagentInfo::scrollback_entry_id` replayed during a
/// failed window dangles into the discarded staging state (harmless no-op
/// lookups; never aliased, thanks to the shared `EntryId` space).
pub(crate) struct SessionReload {
    /// Reconnect generation (from `ConnectionStatus::Connected`) this reload
    /// was opened for; finalization is rejected for any other generation.
    generation: u64,
    /// Pre-outage transcript, tracker, todo, and workflow state: the same
    /// [`ReplayRebuiltState`] every replay detaches, stashed for
    /// restore-on-failure.
    stash: ReplayRebuiltState,
    /// Reconnect cursor as of window open, restored with the stash so a
    /// later reload doesn't skip events the restored transcript never got.
    last_seen_event_id: Option<String>,
    /// Parsed counter of [`Self::last_seen_event_id`] (same restore rationale).
    last_seen_event_seq: Option<u64>,
    /// Live dedup highwaters (ACP + EchoAgent) as of window open (same restore
    /// rationale).
    last_applied_event_seq: Option<u64>,
    last_applied_echo_agent_event_seq: Option<u64>,
    /// Whether any `isReplay` update applied during this window. False means
    /// the agent resolved the cursor and sent only a live post-cursor tail.
    saw_replay: bool,
    /// Whether a Plan update applied during this window: the cursor-merge
    /// outcome then keeps the staging todo list (newer) instead of the stash.
    saw_todo_update: bool,
    /// Expiry notices staged by replayed tombstones during this window. The keep-stash finalize
    /// drops staged copies of a line only up to the count the stash already shows (two tasks can
    /// share identical notice text).
    replayed_expiry_notices: Vec<crate::scrollback::entry::EntryId>,
}
/// The `AgentView` state a session replay rebuilds from disk, detached by
/// [`AgentView::take_replay_rebuilt_state`] (see its doc for the contract).
pub(crate) struct ReplayRebuiltState {
    pub(crate) scrollback: ScrollbackState,
    pub(crate) tracker: crate::acp::tracker::AcpUpdateTracker,
    pub(crate) todo: TodoPane,
    pub(crate) workflow_blocks:
        std::collections::HashMap<String, crate::scrollback::entry::EntryId>,
    pub(crate) workflow_runs: Vec<crate::views::workflows::WorkflowRunSnapshot>,
    pub(crate) workflow_run_revisions: std::collections::HashMap<String, u64>,
    pub(crate) cleared_workflow_runs: std::collections::HashSet<String>,
}
/// Lifecycle of the inline plugin CTA. `Hidden`/`Matched` cover the idle and
/// prompt-matched states; `Installing`/`Installed`/`Error` cover an in-TUI
/// install triggered from the CTA. `AwaitingReload`/`AwaitingMcps` cover the
/// post-install branch (reload plugins, then read MCP servers); a needs-auth
/// result hands the user into the Extensions modal and settles back to `Hidden`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CtaPhase {
    #[default]
    Hidden,
    Matched {
        plugin_relative_path: String,
        name: String,
    },
    Installing {
        plugin_relative_path: String,
        name: String,
    },
    AwaitingReload {
        name: String,
    },
    AwaitingMcps {
        name: String,
    },
    Installed {
        name: String,
    },
    Error {
        plugin_relative_path: String,
        name: String,
        message: String,
    },
}
impl CtaPhase {
    /// True while the CTA shows an animated spinner (install or post-install
    /// setup in progress). The frame loop keeps ticking in these phases so the
    /// braille spinner advances.
    pub fn is_spinner(&self) -> bool {
        matches!(
            self,
            Self::Installing { .. } | Self::AwaitingReload { .. } | Self::AwaitingMcps { .. }
        )
    }
}
#[derive(Default)]
pub struct PluginCtaState {
    /// Not-installed candidate plugins for CTA matching, from the CTA source
    /// (EchoAgent Official, or the configured `plugin_cta_marketplace` override).
    pub candidates: Vec<echo_agent_hooks_plugins_types::MarketplacePluginEntry>,
    /// URL/path of the CTA source the candidates came from — the install
    /// target (the shell resolves marketplace sources by URL/path identity).
    /// `None` = no CTA source (official by default, the
    /// `plugin_cta_marketplace` source when configured) in the last catalog
    /// scan, which keeps the CTA hidden and blocks installs.
    pub source_url_or_path: Option<String>,
    /// Current CTA phase (recomputed when the prompt debounce expires).
    pub phase: CtaPhase,
    /// Generation counter for prompt-change debouncing (mirrors suggestions).
    pub debounce_generation: u64,
    /// `[Install]`/`[Retry]` affordance rect, rebuilt each frame the CTA is visible.
    pub hit_connect: HitArea,
    /// `[x]` dismiss affordance rect, rebuilt each frame the CTA is visible.
    pub hit_dismiss: HitArea,
    /// Whether the plugin being installed ships MCP servers (`has_mcp` of the
    /// matched candidate, captured at Connect time). Gates the post-install
    /// MCP-init settle poll: skills-only plugins settle immediately.
    pub expects_mcp: bool,
    /// Post-install MCP-list re-probe counter, reset on each `AwaitingMcps`
    /// entry and bounded by the poll budget.
    pub mcp_attempt: u32,
    /// Dismissed plugin ids, cached from `config.toml` on catalog load so the
    /// matched-debounce recompute never reads the config from disk on the UI
    /// thread. Updated in-memory when the user dismisses via `[x]`.
    pub dismissed: std::collections::HashSet<String>,
}
/// Follow-up suggestion chips for the latest assistant response
/// (`echo.agent/follow_ups`). Streaming-only: never persisted, does not survive a
/// session reload. Keyed by the assistant `response_id` (the newest-wins key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FollowUps {
    /// `params.response_id` these chips belong to (the newest-wins key).
    pub(crate) response_id: String,
    /// Suggestion labels, already sanitized (control + bidi/format chars
    /// stripped) and length-bounded at ingestion. Non-empty whenever
    /// `AgentView::follow_ups` is `Some`.
    pub(crate) suggestions: Vec<String>,
}
/// A composer action held back while a clipboard attachment probe is off-thread.
///
/// Kind-only: the payload is re-derived at resume (see [`AgentView::resume_deferred_send`]), so the freshly attached image chip travels with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentDeferredSend {
    /// Enter: a normal prompt send.
    SendPrompt,
    /// Ctrl+Enter: a mid-turn interjection.
    Interject,
    /// Enter on the `/feedback` pane — a feedback submit.
    SubmitFeedback,
    /// Ctrl+S / Alt+S: set the draft aside once its image lands.
    Stash,
}
pub struct AgentView {
    pub session: AgentSession,
    pub(crate) session_binding_epoch: u32,
    pub scrollback: ScrollbackState,
    pub prompt: PromptWidget,
    /// Sticky: once the user types in the prompt, hide the tip for the session.
    pub tip_typing_dismissed: bool,
    pub todo: TodoPane,
    pub tasks: TasksPane,
    pub catalog: SubagentCatalogPane,
    pub queue: QueuePane,
    /// Per-agent mirror of the server-authoritative shared prompt queue
    /// (`AppView::shared_prompt_queues[sid]`), kept in sync by
    /// `handle_queue_changed` and the immediate-send path. The queue
    /// pane renders the union of this and the local `pending_prompts`; the
    /// edit handlers read it to route remove/reorder by origin. Empty unless a
    /// plain prompt was queued server-side while a turn was running.
    pub shared_queue: Vec<crate::app::prompt_queue::QueueEntryWire>,
    /// True when this session was opened via `session/load` (session picker
    /// resume, `/resume`, or a leader dashboard roster attach) rather than
    /// created locally — i.e. this client is *viewing* a session it did not
    /// start. While set, the ACP gate adopts the prompt id of incoming live
    /// `session/update` deltas (the driver's turn) instead of dropping them,
    /// so the viewer renders the in-flight (and subsequent) turns live. This
    /// must NOT be applied to a locally-created driver, whose post-rewind
    /// stale-chunk drop semantics rely on the strict prompt-id match. Cleared
    /// in `maybe_drain_queue` the moment this client sends its own prompt
    /// ("takes the wheel"), and re-derived per turn from
    /// [`Self::self_originated_prompt_ids`] in the ACP gate / turn-start shim:
    /// a client that has driven a turn can still go on to VIEW a turn another
    /// client drives (e.g. a `/loop` cron, or a plain prompt typed in another
    /// pane), so this flag is no longer a one-way latch.
    pub attached_as_viewer: bool,
    /// Prompt ids of turns THIS client originated (sent to the agent as the
    /// turn driver). The ACP gate consults this to keep `attached_as_viewer`
    /// per-turn accurate: a prompt id present here is this client's own turn
    /// (drive it — and drop a stale post-rewind chunk on a mismatch), while one
    /// that is absent is another client's (or a server-initiated) turn (adopt +
    /// render it as a viewer). Without this, the flag latched false after the
    /// first local prompt and the gate dropped every later turn a different
    /// pane drove. Bounded FIFO — only recent ids matter (a stale chunk arrives
    /// right after its turn ends).
    pub self_originated_prompt_ids: VecDeque<String>,
    pub rewound_prompt_ids: VecDeque<String>,
    /// Highwater of the largest `eventId` counter applied to this session's
    /// scrollback (see `acp::meta::NotificationMeta::event_seq`). Incoming
    /// `session/update`s with a counter `<=` this are duplicates (replay/live
    /// overlap, a re-emit after the reconnect gate, or duplicate routing) and
    /// are dropped so each event renders exactly once. `None` until the first
    /// `eventId`-bearing update.
    ///
    /// ACP stream only — the EchoAgent stream keeps its own highwater
    /// ([`Self::last_applied_echo_agent_event_seq`]) because the two streams are not
    /// delivered in one id order: ACP lines ride the agent's FIFO event
    /// pipeline while EchoAgent lines are emitted direct-to-gateway, so a fresh EchoAgent
    /// id arriving ahead of queued lower-id ACP chunks must not make the
    /// chunks look stale (silent live-text loss).
    pub last_applied_event_seq: Option<u64>,
    /// EchoAgent-stream sibling of [`Self::last_applied_event_seq`] (see there for
    /// why the highwaters are split). Same drop rule, replay-exempt. Durable
    /// subagent lifecycle events bypass that drop check but still lift this
    /// highwater via `max` so later ordinary updates on the cursor tail stay
    /// deduped.
    pub last_applied_echo_agent_event_seq: Option<u64>,
    /// Raw `eventId` of the most recent update APPLIED to this root session —
    /// replay or live, on both the ACP and EchoAgent paths; dropped updates (dedup,
    /// promptId gate, unexpected replay) don't move it. Sent as `_meta.cursor`
    /// on a reconnect `session/load` so the agent replays only the post-cursor
    /// tail. Why the full string: see
    /// [`crate::acp::meta::NotificationMeta::event_id`]. Forward-only: a later
    /// applied lower-ID lifecycle update must not regress this cursor. All
    /// writers go through [`Self::advance_last_seen_event_id`].
    pub last_seen_event_id: Option<String>,
    /// Parsed counter for [`Self::last_seen_event_id`], kept in lockstep by
    /// [`Self::advance_last_seen_event_id`] so forward-only compares do not
    /// re-parse the id string at every writer.
    pub last_seen_event_seq: Option<u64>,
    /// Terminal lifecycle updates that arrived before their spawn. Bounded,
    /// output-stripped, and cleared on session rebind; the spawn path drains
    /// an entry before a later reconnect cursor can strand the finish.
    pub(crate) deferred_subagent_finishes: HashMap<String, DeferredSubagentFinish>,
    /// Open reconnect reload window, if any. See [`SessionReload`].
    pub(crate) session_reload: Option<SessionReload>,
    /// Unexpected-replay drops since the last reload window opened. Gates the
    /// drop log to one `warn!` per incident (a late replay is one line per
    /// event — thousands for a large transcript).
    pub(crate) unexpected_replay_drops: u32,
    /// After `SessionLoaded` clears `loading_replay`, keep accepting this-session
    /// `isReplay` until this instant (or the first this-session live update).
    /// The load barrier may release on an Unrelated firehose timeout while
    /// remaining replay still sits behind the ACP peek.
    pub(crate) late_replay_until: Option<std::time::Instant>,
    /// Prompt ids whose durable `TurnCompleted` terminal arrived during THIS
    /// load's replay window (`loading_replay`). The running turn is not adopted
    /// until replay finishes, so a terminal seen mid-replay can't be finalized
    /// yet — it is recorded here and consulted by
    /// [`Self::should_adopt_running_prompt`] so the post-replay adoption skips a
    /// turn that already ended (otherwise the viewer re-strands on "Waiting…").
    /// Reset at the start of every load so it never leaks across loads.
    pub(crate) replayed_terminal_prompts: HashSet<String>,
    /// Wake prompt id whose failure marker already rendered — a re-delivered
    /// errored wake terminal must not stack a second "Turn failed" row (the
    /// output-epoch dedupe only covers chatty closes; failures bypass it).
    pub(crate) failed_wake_marker_for: Option<String>,
    /// Wake prompts whose terminals landed; a late delta for one must not
    /// revive the stop affordance (see `note_streaming_wake_turn`). Cleared
    /// at replay-window entry; a queue broadcast naming one as running again
    /// removes that entry.
    pub(crate) finished_wake_prompts: std::collections::HashSet<String>,
    /// The wake turn currently streaming, if any. See [`RunningWakeTurn`].
    pub(crate) running_wake_turn: Option<RunningWakeTurn>,
    pub active_pane: AgentPane,
    /// Current mode of the prompt widget (normal vs editing a queued prompt).
    pub prompt_mode: PromptMode,
    /// Current special prompt input mode (Normal/Bash/Remember).
    pub prompt_input_mode: PromptInputMode,
    /// Multiline input mode: swap Enter (insert newline) and Shift+Enter (send).
    /// Toggled by `Ctrl+M` or `/multiline`. Not persisted across sessions.
    pub multiline_mode: bool,
    /// Vim-mode scrollback keybindings. When `false` (default), bare-letter
    /// and Shift+letter scrollback bindings (j/k, h/l, g/G, y/Y, o/O, r,
    /// x, e/E, L/H, plus the `i` FocusPrompt alt) are suppressed and the
    /// pressed letter forwards to the prompt textarea via
    /// `Action::FocusPrompt` + `ActionThenForward`. Arrows / Tab / Esc /
    /// Space / PgUp / PgDn and all `Ctrl+letter` bindings remain active
    /// regardless of this flag. Pager-owned ephemeral field — reset each session.
    pub vim_mode: bool,
    /// Runtime InputMode synced from persisted `simple_mode` bool (false=Vim). Pane reconcile only on Vim+empty prompt. Subagent children are forced to Vim after new() (no prompt UI).
    pub input_mode: InputMode,
    /// Whether the current/last turn was a bash-mode command.
    /// Set when a bash command starts, cleared on next non-bash turn.
    /// Used to auto-focus scrollback after bash turn completion.
    pub bash_turn: bool,
    /// The task ID of the currently running cron turn, if any.
    /// Set when a cron prompt is drained, cleared on turn completion.
    pub cron_task_id: Option<String>,
    /// Stashed normal prompt state while editing a queued prompt.
    /// Restored when editing ends.
    pub stashed_prompt: Option<StashedPrompt>,
    /// One draft set aside for later; see [`prompt_stash`].
    pub prompt_stash: Option<PromptStashEntry>,
    /// Set by the send that consumed the user's draft this dispatch; see `note_draft_consumed`.
    pub(crate) draft_consumed: bool,
    /// Complete prompt stashed from a credit-limit-blocked turn. Used by
    /// `CreditLimitRecheckComplete` to retry the prompt after a tier
    /// upgrade instead of showing a stale upsell.
    pub credit_limit_stashed_prompt: Option<crate::app::agent::InFlightPrompt>,
    /// Complete prompt stashed from a turn that failed because the login
    /// expired (401 / re-auth). Used by the `AuthComplete` handler to
    /// auto-resubmit the prompt after a successful mid-session re-auth so
    /// the user doesn't have to retype it.
    pub reauth_stashed_prompt: Option<crate::app::agent::InFlightPrompt>,
    /// Currently active modal dialog (blocks all other input).
    pub active_modal: Option<ActiveModal>,
    /// Hit areas for modal buttons (from last render).
    pub(crate) modal_buttons: Vec<ModalButtonHit>,
    /// Currently hovered modal button key (for highlight).
    pub(crate) modal_hovered_key: Option<char>,
    /// Cached server-reported context state.
    pub context_state: Option<echo_agent_runtime::session::ContextInfo>,
    pub status_context: Option<echo_agent_status_line::StatusLineContext>,
    /// Held across a frame that clamps the row away, so a script keeps the size
    /// it last painted at.
    pub last_status_line_size: Option<crate::views::status_line::RowSize>,
    /// Gateway light-frontend session (`kind: "chat"` / `--chat` / conversation
    /// resume). Suppresses Build credits / local sampler context telemetry so the
    /// status bar and prompt never imply remote usage from wrong metrics.
    /// OR'd with sticky `--chat` for UI/focus matching — not the ACP rename
    /// `kind` bit; see [`Self::conversation_entry`].
    pub chat_kind: bool,
    /// Whether this session opened on the chat lane (ACP `kind=chat`).
    /// True for conversation-entry loads, `/chat` create, and sticky
    /// `--chat` gateway resumes. False for history-bypass local-disk
    /// Build rows (those keep [`Self::chat_kind`] for UI only).
    pub conversation_entry: bool,
    /// Process-wide `--chat` (mirrors `AppView::chat_mode`; set via
    /// [`Self::apply_app_scoped_gates`]). UI policy only: hides picker
    /// source filter / delete / deep search on a conversations-only list.
    /// Unlike `chat_kind`, stays `false` for a `/chat` one-shot session in
    /// a Build process, whose picker still lists local sessions.
    pub app_chat_mode: bool,
    /// Durable workspace mode for the in-session status indicator (`--chat`).
    #[cfg(feature = "local-workspace")]
    pub workspace_mode: crate::views::welcome::WelcomeWorkspaceMode,
    /// True when CLI/env locked local workspace at startup for this session.
    #[cfg(feature = "local-workspace")]
    pub workspace_mode_cli_locked: bool,
    /// Mocked credit balance for the status bar indicator.
    pub credit_balance: Option<crate::views::credit_bar::CreditBalance>,
    /// Auto top-up rule paired with `credit_balance` for the prompt warning.
    pub auto_topup: Option<crate::views::credit_bar::AutoTopupInfo>,
    /// Current goal orchestration state. Set by `GoalUpdated` session
    /// notifications, cleared when a new session starts.
    pub goal_state: Option<super::agent::GoalDisplayState>,
    pub workflow_blocks: std::collections::HashMap<String, crate::scrollback::entry::EntryId>,
    pub workflow_runs: Vec<crate::views::workflows::WorkflowRunSnapshot>,
    pub workflow_run_revisions: std::collections::HashMap<String, u64>,
    pub cleared_workflow_runs: std::collections::HashSet<String>,
    pub show_workflows: bool,
    pub workflows_view: crate::views::workflows::WorkflowsViewState,
    /// Turn-end hook runs waiting for the turn's marker, which they race. Consumed or flushed
    /// by `push_turn_terminal_marker`; dropped on every replay-window entry.
    pub(crate) pending_stop_hooks: Option<PendingStopHooks>,
    /// Goal id of the most recently cleared goal, captured from the dropped
    /// state (the `cleared` event itself carries an empty id). Drops a late
    /// in-flight `GoalUpdated` that would otherwise resurrect the cleared
    /// chip/modal. Single slot: goal ids are unique, so only the latest clear
    /// can race a stale update.
    pub last_cleared_goal_id: Option<String>,
    /// Whether the expanded goal detail overlay is visible.
    /// Toggled by `Action::ToggleGoalDetail`. Only shown when
    /// `goal_state` is `Some`.
    pub show_goal_detail: bool,
    /// UTC ms when the current turn started (`turnStartMs` from notification meta).
    /// Used for turn elapsed display.
    pub turn_start_ms: Option<i64>,
    /// Prompt id the stored `turn_start_ms` belongs to (stamped together from
    /// the same delta meta): wake markers may only claim an elapsed whose
    /// anchor is provably their own turn's.
    pub turn_start_ms_prompt: Option<String>,
    /// Local wall-clock time when the current turn started.
    /// Set by `maybe_drain_queue` when a prompt is sent. Used to compute
    /// elapsed time for "Worked for Xm Ys" system messages.
    pub turn_started_at: Option<Instant>,
    /// Turn-start anchor a `turn.first_activity` log was already emitted for (fire-once-per-turn guard).
    pub first_activity_logged_for: Option<Instant>,
    /// Accumulated duration the turn timer was paused (while the user was
    /// answering questions via `AskUserQuestion`). Reset when the turn ends.
    pub turn_paused_duration: std::time::Duration,
    /// Wall-clock twin of `turn_paused_duration`: the same pauses measured on
    /// the wall clock, which keeps counting through OS suspend while `Instant`
    /// does not. Netted against the wall-anchored turn span so a suspend
    /// during an open question isn't reported as worked time.
    pub turn_paused_wall: std::time::Duration,
    /// IDs of interjections this client sent and already rendered locally
    /// (optimistic echo). The shell broadcasts `echo.agent/session/interjection` to
    /// every attached pane; when our own broadcast echoes back carrying an id
    /// in this set, `handle_interjection` drops it (we already showed it) and
    /// removes the id. Other panes (which lack the id) render it. This is the
    /// queue's optimistic-echo + reconcile-by-id pattern, applied so the
    /// originator gets instant feedback AND viewers stay in sync.
    pub self_interjection_ids: std::collections::HashSet<String>,
    /// Local wall-clock time when the most recent turn finished
    /// (success, failure, or cancellation). Used by the dashboard
    /// modal to display "Nm ago" idle markers. Initialised to the
    /// agent-creation time in [`AgentView::new`] so newly-created
    /// agents that have never run a turn still show a sensible
    /// relative time.
    pub last_active_at: Option<Instant>,
    pub current_branch: Option<String>,
    pub is_worktree: bool,
    pub main_repo: Option<String>,
    /// Human-readable worktree label from the worktree metadata DB, when
    /// this agent's cwd is a managed worktree. Refreshed off the git
    /// caches when the dashboard opens; drives the dashboard row's
    /// worktree-name subtitle. `None` for non-worktree agents.
    pub worktree_label: Option<String>,
    /// Local wall-clock time when the current activity phase started.
    /// Reset on each activity transition (thinking → responding → tool, etc.).
    /// Used for the `(5s)` phase timer in the turn status line.
    pub activity_started_at: Option<Instant>,
    /// Last observed [`TurnActivity`] — used to detect phase transitions
    /// and reset `activity_started_at`.
    pub(crate) last_activity: Option<crate::acp::tracker::TurnActivity>,
    /// Cached pane areas from last render, for mouse hit-testing.
    pub pane_areas: PaneAreas,
    /// Entry index currently hovered by the mouse (for dimmed selection box).
    pub hovered_entry: Option<usize>,
    /// Pending markdown text drag before the pointer crosses the drag threshold.
    pub pending_text_drag: Option<PendingTextDrag>,
    /// Active markdown text drag selection.
    pub drag_selection: Option<ActiveTextDrag>,
    /// Pending whole-block drag before the pointer crosses the drag threshold.
    pub pending_block_drag: Option<PendingBlockDrag>,
    /// Active whole-block drag selection.
    pub block_drag_selection: Option<ActiveBlockDrag>,
    /// Deferred text-drag anchor, armed (with the press position, for
    /// tracing) by a scrollback press that hit no selectable text — chrome,
    /// vpad, and gap rows all count, so any scrollback point can start a
    /// selection gesture — or by a press on the passive strips between the
    /// scrollback pane and the prompt box (turn status, banner, gap rows;
    /// interactive controls there consume their presses first). While set,
    /// the first drag motion that lands on selectable text anchors an
    /// [`ActiveTextDrag`] THERE (the entry point, not the press point) and
    /// cancels any block-drag state; the conversion is one-way. A gesture
    /// that never enters text keeps whatever the press
    /// armed alongside (the in-pane whole-block drag; strips arm nothing
    /// else), and a release while still deferred falls back to the press's
    /// native semantics (in-pane: the plain click cascade; strips: a no-op —
    /// no click latch is set there). Btw presses never arm it, and no source
    /// arms while the block viewer is open.
    pub deferred_text_press: Option<(u16, u16)>,
    /// Persistent text selection (survives mouse-up). Set after drag
    /// completion, double-click, or triple-click. Cleared on next click
    /// elsewhere, Escape, or navigation.
    pub persistent_text_selection: Option<PersistentTextSelection>,
    /// Table geometry backing a table-shaped drag / persistent selection,
    /// keyed to that selection; ignored when the key doesn't match.
    pub table_selection_geometry: Option<TableSelectionGeometry>,
    /// Timestamp when the current persistent selection was created.
    /// Used for auto-dismissal after a configurable timeout.
    pub selection_created_at: Option<Instant>,
    /// Last mouse position during an active drag (for autoscroll re-hit-testing).
    pub last_drag_mouse: Option<(u16, u16)>,
    /// Active drag autoscroll state (scrolls on each tick while set).
    pub drag_autoscroll: Option<DragAutoScrollState>,
    /// Whether the primary mouse button is currently held down.
    pub(crate) left_mouse_down: bool,
    /// Whether a left-button drag that started in the feedback prompt is in
    /// progress while the plan preview (line viewer) is open. Used to keep
    /// forwarding `Drag`/`Up` events to the prompt so text selection in the
    /// input works even though the line viewer otherwise intercepts them.
    pub(crate) plan_prompt_mouse_drag: bool,
    /// Last resolved scrollback selection model from the most recent draw.
    pub last_scrollback_selection_model: ResolvedSelectionModel,
    /// Edit-only hidden source boundaries paired with the last scrollback model.
    pub(crate) last_scrollback_selection_boundaries: ResolvedSelectionBoundaries,
    /// Link overlay from the last frame (scrollback + optional `/btw` merge;
    /// for OSC 8 emission).
    pub last_link_overlay: crate::render::osc8::LinkOverlay,
    /// Rects of overlays drawn over the scrollback content this frame (dropdowns,
    /// goal-detail). Used to exclude covered links from both OSC 8 emission and
    /// mouse hover/click hit-testing. Rebuilt each `draw()`.
    pub frame_occluder_rects: Vec<Rect>,
    /// Per-frame map of clickable link regions (populated after scrollback render).
    pub visible_link_map: crate::scrollback::link_map::VisibleLinkMap,
    /// Number of links in `visible_link_map` that came from the scrollback
    /// rebuild (citations included). Overlay-only sources (e.g. `/btw`) are
    /// appended after this prefix each frame and truncated back on the next.
    scrollback_visible_link_count: usize,
    /// Index of the currently highlighted link in `visible_link_map` for
    /// keyboard link navigation (o/O cycling). `None` when not in link-nav mode.
    pub highlighted_link_idx: Option<usize>,
    /// Link index under the mouse cursor (for hover highlight).
    pub hovered_link_idx: Option<usize>,
    /// Last emitted OSC 22 pointer state (avoids re-emitting every frame).
    pub last_pointer_on_link: bool,
    /// Selection model for the /btw overlay panel (populated each frame).
    pub last_btw_selection_model: ResolvedSelectionModel,
    /// Cached screen rect of the /btw overlay panel from the last render.
    pub last_btw_area: Rect,
    /// Pending plain scrollback click that should dispatch on mouse-up if no drag starts.
    pub pending_scrollback_click: Option<(u16, u16)>,
    /// Pending link click: (col, row, target). Set on Down(Left) when a link is hit,
    /// consumed on Up(Left) at the same position, cleared on drag.
    pub pending_link_click: Option<(u16, u16, crate::render::osc8::LinkTarget)>,
    /// Absolute paths of media generated in this transcript, used to resolve the
    /// short relative paths the model prints (`images/1.jpg`) to clickable
    /// links. Rebuilt from scrollback only when its generation changes.
    pub media_link_paths: Vec<std::path::PathBuf>,
    /// Scrollback generation [`Self::media_link_paths`] was built for.
    pub media_link_paths_gen: Option<u64>,
    /// Last mouse position (column, row) for hover hit-testing.
    pub last_mouse_pos: (u16, u16),
    /// When the pointer last moved (any mouse event). Bounds the macOS
    /// Cmd-key link-hover poll: a pointer merely *resting* over content must
    /// not keep the ~30fps animation tick (and its per-tick CoreGraphics
    /// query) alive indefinitely — see [`Self::needs_link_modifier_poll`].
    pub last_mouse_moved_at: Option<Instant>,
    /// Last click info for multi-click detection: (timestamp, entry_index, click_count).
    pub last_click: Option<(Instant, usize, u8)>,
    /// Text-level multi-click state (finer-grained than block-level `last_click`).
    /// Mutually exclusive with `last_click`: one is cleared when the other is set.
    pub last_text_click: Option<TextClickState>,
    /// Timestamp of the last clipboard toast shown for word/line selection.
    /// Used to debounce rapid toasts; drag completions bypass this.
    pub last_clipboard_toast_at: Option<Instant>,
    /// Timestamp of the last accepted context-bar click. Used to debounce
    /// rapid clicks so we don't spawn a redundant `session/info` request
    /// per double-click.
    pub last_context_click_at: Option<Instant>,
    /// Whether the mouse is hovering over the prompt widget.
    pub hovered_prompt: bool,
    pub hit_context: HitArea,
    pub hit_credits: HitArea,
    pub hit_todo_close: HitArea,
    pub hit_bg_close: HitArea,
    pub hit_subagent_close: HitArea,
    pub hit_catalog_close: HitArea,
    pub hit_bg_status: HitArea,
    pub hit_goal_status: HitArea,
    pub hit_goal_close: HitArea,
    pub hit_bg_button: HitArea,
    #[allow(dead_code)]
    pub(crate) last_bg_click: Option<Instant>,
    pub hit_queue_close: HitArea,
    pub hit_plan_button: HitArea,
    pub hit_plan_approval_status: HitArea,
    pub hit_follow_indicator: HitArea,
    /// ▲ jump-to-response-top indicator in the sticky header's gap row
    /// (click snaps the answer's first line to the top, same as `K`).
    pub hit_response_top_indicator: HitArea,
    /// CWD / worktree path in the status bar (click to copy).
    pub hit_cwd: HitArea,
    /// Cancel button in turn status line (`[stop]`).
    pub hit_cancel_button: HitArea,
    /// Still-running watcher cue on the turn-status row (click opens the
    /// tasks pane, same as `Ctrl+G`).
    pub hit_watching_cue: HitArea,
    /// One-time Ctrl+G toast already fired for a watching-cue click.
    pub(crate) watching_cue_toast_shown: bool,
    /// `[hide]` button on the announcement banner (click == `/announcements hide`).
    pub hit_announcement_hide: HitArea,
    /// `[label]` CTA button on the promo banner row (click opens its link).
    pub hit_announcement_cta: HitArea,
    /// Privacy upsell banner state: slot ownership + click targets
    /// (packaged like [`Self::plugin_cta`]).
    pub privacy_banner: PrivacyBannerState,
    /// `[label]` upgrade CTA appended after the cwd path in the status bar
    /// (click opens its link; nulled under dropdowns / occluders like the
    /// banner CTA).
    pub hit_upgrade_cta: HitArea,
    /// Stop button in the voice record indicator row (`[stop]`), far right.
    pub hit_voice_stop_button: HitArea,
    /// Scrollbar track for the scrollback pane (for click-to-jump / drag).
    pub hit_scrollbar: HitArea,
    /// Whether a scrollbar drag is in progress on the scrollback scrollbar.
    pub scrollbar_dragging: bool,
    /// Cached screen area of the file search dropdown items (for mouse hit-testing).
    /// Excludes border rows — only the clickable item rows.
    pub(crate) dropdown_items_area: Option<Rect>,
    /// Cached screen area of the slash dropdown items (for mouse hit-testing).
    pub(crate) slash_dropdown_items_area: Option<Rect>,
    /// Slash dropdown hit-test state from the renderer (see
    /// [`crate::views::slash_dropdown::RenderedDropdown`]).
    pub(crate) slash_dropdown_hit: crate::views::slash_dropdown::RenderedDropdown,
    /// Cached screen area of the completion dropdown items (for mouse hit-testing).
    pub(crate) completion_dropdown_items_area: Option<Rect>,
    /// Cached screen area of the history search dropdown items (for mouse hit-testing).
    pub(crate) history_dropdown_area: Option<Rect>,
    /// Last prompt click timestamp (for double-click detection).
    pub(crate) last_prompt_click_ms: Option<Instant>,
    /// Active line viewer popup (Phase 3). When `Some`, the viewer intercepts
    /// all input and renders as a centered overlay.
    pub(crate) line_viewer: Option<LineViewerState>,
    /// Active image viewer popup. When `Some`, shows an image preview that
    /// intercepts input (Esc to close).
    pub(crate) image_viewer: Option<crate::prompt_images::ImageViewerState>,
    /// Receiver for background image loading. Set when a deferred image
    /// viewer spawns a load thread; polled each tick via `try_recv()`.
    pub(crate) image_load_rx:
        Option<std::sync::mpsc::Receiver<crate::prompt_images::ImageLoadResult>>,
    /// Active video viewer popup (Esc to close, Space to pause).
    pub(crate) video_viewer: Option<crate::prompt_images::VideoViewerState>,
    /// Active `/gboom` easter-egg game modal.
    pub(crate) gboom: Option<crate::gboom::GboomState>,
    /// Protocol-prepared image bytes keyed by file path. Used for dimension
    /// decoding and iTerm2 re-sends. Kitty transmits once and re-places.
    pub(crate) inline_media_cache: std::collections::HashMap<std::path::PathBuf, Vec<u8>>,
    /// Paths that failed to decode/extract, keyed by the file stamp at
    /// failure: skips per-frame decode/ffmpeg retries while the file is
    /// unchanged, and self-heals when it changes (e.g. caught mid-write).
    /// Cleared with the byte cache on eviction.
    pub(crate) inline_media_load_failed:
        std::collections::HashMap<std::path::PathBuf, media::MediaFileStamp>,
    /// Kitty GPU image IDs per media path. Each path gets a unique ID (2+)
    /// so switching between images is a cheap re-place (~80 bytes) instead
    /// of a full re-transmit. ID 1 is reserved for modal overlays.
    pub(crate) inline_media_ids: std::collections::HashMap<std::path::PathBuf, u32>,
    /// Paths whose iTerm2 inline data has already been emitted this placement
    /// cycle. Avoids re-sending full base64 image data every TUI frame.
    /// Last iTerm2 placement per path — re-emit when `screen_rect` changes.
    pub(crate) inline_media_iterm_emitted:
        std::collections::HashMap<std::path::PathBuf, ratatui::layout::Rect>,
    /// Counter for allocating the next Kitty image ID.
    pub(crate) next_inline_media_id: u32,
    /// Active inline video playback (user-initiated via click/Enter).
    pub(crate) inline_video: Option<InlineVideoState>,
    /// Receiver for background video frame extraction.
    pub(crate) video_load_rx: Option<std::sync::mpsc::Receiver<Option<InlineVideoState>>>,
    /// Off-thread Mermaid render runtime (worker channels + the on-click renders
    /// awaiting their result). Lazily created on the first *cache-missing*
    /// `[Open]`/`[Copy path]` click; `None` until then.
    pub(crate) mermaid: Option<crate::app::mermaid_worker::MermaidRuntime>,
    /// Off-thread edit-diff full-file syntax highlight upgrade. Lazily created
    /// on the first successful Edit tool completion that has hunks.
    pub(crate) edit_hl: Option<crate::app::edit_highlight_worker::EditHlRuntime>,
    /// Whether any inline media is currently placed on screen.
    pub(crate) inline_media_active: bool,
    /// Image IDs that were placed on screen last frame. Used to detect
    /// images that scrolled off and need their Kitty placements cleared.
    pub(crate) last_placed_ids: HashSet<u32>,
    /// Previous terminal dimensions — used to detect resize and invalidate
    /// Kitty IDs (terminals clear GPU data on resize).
    ///
    /// This is the size of the rect this view last painted into, which can
    /// be smaller than the terminal (dashboard overlay header band/popup,
    /// dev tracing split). Only `note_terminal_size` (draw) writes it.
    pub(crate) last_terminal_size: (u16, u16),
    /// Set on every `Event::Resize` (see `AppView::handle_input`), cleared
    /// by the next draw's re-measure. While set, `last_terminal_size` is
    /// known-invalidated and the ephemeral-tip show gate refuses, so a
    /// trigger racing the (debounced) resize draw can never burn a seen
    /// count against a height the new layout may not be able to paint.
    pub(crate) terminal_size_stale: bool,
    /// Hit areas for inline media buttons (cleared and rebuilt each frame).
    pub(crate) inline_media_hits: InlineMediaHitAreas,
    /// Active hooks/plugins modal popup. When `Some`, blocks all input and
    /// renders as a centered overlay. Opened by `/hooks`, `/plugins`, or `/mcps`.
    pub(crate) extensions_modal: Option<ExtensionsModalState>,
    /// Active agents modal popup. When `Some`, blocks all input and
    /// renders as a centered overlay. Opened by `/config-agents` or `/agents`.
    pub(crate) agents_modal: Option<crate::views::agents_modal::AgentsModalState>,
    pub(crate) persona_detail: Option<crate::views::persona_detail::PersonaDetailState>,
    /// Active /btw side question overlay. When `Some`, renders as a dismissible
    /// overlay and captures keyboard input (Esc/Enter/Space to dismiss).
    pub btw_state: Option<crate::views::btw_overlay::BtwOverlayState>,
    /// Minimal-only ownership/correlation for `btw_state`; absent in fullscreen.
    pub(crate) minimal_btw_lifecycle: Option<crate::minimal_api::MinimalBtwLifecycle>,
    /// Whether the /btw panel holds keyboard focus. The panel is non-blocking,
    /// so Up/Down/PgUp/PgDn scroll it when focused and otherwise reach the
    /// prompt. Set on a `Done` answer; cleared when the user types in or clicks
    /// the prompt.
    pub(crate) btw_focused: bool,
    /// Hit area for the [Esc] close button in the /btw panel title.
    pub(crate) hit_btw_close: HitArea,
    /// Toast message to display briefly (e.g., "Copied!" after y).
    /// Tuple of (message, remaining_ticks). Decremented each tick, removed at 0.
    /// Does **not** carry sticky status banners — see [`Self::sticky_toast`].
    pub(crate) toast: Option<(String, u8)>,
    /// Single-slot ephemeral tip shown in the banner rect above the prompt.
    /// Unlike `toast`, survives typing; cleared by TTL, any prompt-box
    /// submit (prompt/interject/bash/feedback/remember), or explicit clear.
    /// Show via `show_ephemeral_tip` (renderability-gated), never `.show()`.
    pub(crate) ephemeral_tip: crate::tips::EphemeralTipState,
    /// Prompt text snapshot taken when the word-select tip was shown. Any
    /// divergence (typed, pasted, dropped — every edit path, no per-helper
    /// hooks) means the user moved past the double-click moment: the Ctrl+Y
    /// intercept refuses and the tick path retires the tip, so the long TTL
    /// can never shadow yank mid-edit. `None` while the tip is not showing.
    pub(crate) word_select_tip_prompt_snapshot: Option<String>,
    /// When the last fold/nav double-click landed on assistant text (a
    /// word-select probe). A second probe within the repeat window is the
    /// repeated-selection-attempt signal that fires the word-select tip —
    /// lone double-clicks (habitual folders) never tip.
    pub(crate) last_word_select_probe: Option<Instant>,
    /// Persistent status line (e.g. mouse reporting off). Survives transient
    /// toasts, keypress dismissal, and subagent open/close when propagated
    /// via [`Self::set_sticky_toast_recursive`].
    pub(crate) sticky_toast: Option<String>,
    /// Transient "Switched to mode: X" banner shown above the prompt after
    /// Shift+Tab. (message, remaining_ticks). Full brightness for 2 s, then
    /// fades out over the final 0.3 s.
    pub(crate) mode_switch_banner: Option<(String, u8)>,
    /// Session announcement banner (critical or promo) is showing (set at
    /// start of `draw`). Ephemeral-tip occluder — unlike short-lived
    /// mode-switch, an announcement can last the session, so tips must not
    /// burn TTL/seen counts while hidden.
    pub(crate) session_banner_active: bool,
    /// A pinned (non-dismissible) promo upgrade CTA is live this frame (set at
    /// the start of `draw` from the same slot gate as the header CTA). When
    /// true, `Ctrl+O` opens that CTA instead of toggling YOLO; the dispatch
    /// re-resolves through the gate so a stale-by-one-frame value stays safe.
    pub(crate) pinned_upgrade_cta_live: bool,
    /// Fullscreen block viewer. When `Some`, replaces the scrollback area.
    pub(crate) block_viewer: Option<BlockViewerPane>,
    /// Active scrollback search session. When `Some`, vim `/` (or `/find`) is
    /// searching the scrollback. Inert until input wiring opens it.
    pub(crate) scrollback_search: Option<ScrollbackSearchState>,
    /// Hit area for scrollback selection box copy button.
    pub(crate) hit_sb_copy: HitArea,
    /// Hit area for scrollback selection box view button.
    pub(crate) hit_sb_view: HitArea,
    /// Active question view (from `AskUserQuestion` tool). When `Some`, the
    /// prompt area shows a structured question UI and input is modal.
    pub(crate) question_view: Option<QuestionViewState>,
    pub(crate) elicitation_view: Option<ElicitationViewState>,
    pub(crate) pending_elicitation: Option<(
        echo_agent_tools::mcp_elicitation::McpElicitExtRequest,
        tokio::sync::oneshot::Sender<echo_agent_acp::AcpResult<agent_client_protocol::ExtResponse>>,
    )>,
    pub(crate) elicit_hits: Vec<(
        crate::views::elicitation_view::ElicitHit,
        ratatui::layout::Rect,
    )>,
    /// Scrollbar hit area for the question view (set during render).
    pub(crate) hit_question_scrollbar: HitArea,
    /// Hovered question item index (visual highlight only).
    pub(crate) hovered_question_item: Option<usize>,
    /// Whether a scrollbar drag is in progress on the question scrollbar.
    pub(crate) question_scrollbar_dragging: bool,
    /// Last question-view option click: (timestamp, item_index) for double-click detection.
    pub(crate) last_question_click: Option<(Instant, usize)>,
    /// Screen area of the inline prompt (label + textarea) in InputMode.
    /// Used for mouse scroll forwarding — scrolls over this region go to
    /// the textarea instead of the options list.
    pub(crate) inline_prompt_area: Option<Rect>,
    /// Clickable button regions on the question nav bar (key → rect).
    pub(crate) question_nav_buttons: Vec<(char, Rect)>,
    /// Currently hovered question nav button key (for highlight).
    pub(crate) hovered_question_button: Option<char>,
    /// Y-range of the scrollable options area (set during render).
    /// Scroll events outside this range are ignored.
    pub(crate) question_scroll_region: Option<(u16, u16)>,
    /// Whether plan mode is currently active. Set when `enter_plan_mode`
    /// tool completes, cleared when `exit_plan_mode` tool completes.
    /// Controls prompt accent color and shortcut bar hints.
    pub(crate) plan_mode_active: bool,
    /// Optimistic plan-mode state set immediately on Shift+Tab.
    /// Cleared to `None` when `detect_plan_mode_change()` confirms real state.
    /// The cycle logic uses `plan_mode_pending.unwrap_or(plan_mode_active)`
    /// so rapid Shift+Tab presses advance correctly without waiting for ACP.
    pub(crate) plan_mode_pending: Option<bool>,
    /// Session mode to apply once this agent's ACP session exists. Set when
    /// the agent is spawned from the dashboard with `/plan` active (the
    /// session does not exist yet, so the mode can't be sent immediately).
    /// Consumed in the `SessionCreated` / `WorktreeSessionCreated` handlers,
    /// mirroring `AgentSession.deferred_model_switch`.
    pub(crate) deferred_session_mode: Option<echo_agent_tools::types::SessionMode>,
    pub(crate) pending_extensions_fetch: bool,
    /// Whether this view was last rendered inside the dashboard's session
    /// overlay. Updated every frame by `draw`; read when building the
    /// shortcuts cheatsheet so the overlay-scoped shortcuts
    /// (`When::DashboardOverlay`) are lit in the overlay and dimmed elsewhere.
    pub(crate) in_dashboard_overlay: bool,
    /// Whether that overlay's cycle order holds more than one agent, i.e.
    /// whether the header shows its `[‹]`/`[›]` chips. Updated every frame by
    /// `draw` beside [`Self::in_dashboard_overlay`], and read from the same
    /// place: the shortcuts bar builds the pane's hints once for both the bar
    /// and the cheatsheet, neither of which can see `draw`'s arguments.
    pub(crate) overlay_can_cycle: bool,
    /// MCP server init progress. Set when the shell starts connecting
    /// MCP servers, cleared when `echo.agent/mcp_initialized` arrives.
    /// Shown in the turn status line while the agent is idle.
    pub(crate) mcp_init_progress: Option<McpInitProgress>,
    /// Last synced ACP command generation. When this differs from
    /// `session.available_commands_generation`, `sync_acp_commands()`
    /// is called on the prompt. Starts at 0 so bootstrap (generation 1)
    /// triggers an initial sync.
    pub(crate) acp_synced_generation: u64,
    /// Hovered permission option index (visual highlight only, like question view).
    pub(crate) hovered_permission_item: Option<usize>,
    pub(crate) last_permission_click: Option<(Instant, usize)>,
    /// Queue of pending permission requests. Only the front request is rendered
    /// and interactive. Subsequent requests wait until the front is resolved.
    /// Matches the TUI's `VecDeque<PermissionRequest>` semantics.
    pub permission_queue: VecDeque<PermissionViewState>,
    /// Monotonic counter for permission request IDs.
    pub next_perm_req_id: usize,
    /// Original prompt text stashed when the permission queue became non-empty.
    /// Restored when the queue drains to empty. This is queue-level state, NOT
    /// per-request — stashing happens on the `empty -> non-empty` transition
    /// and restoring on the `non-empty -> empty` transition.
    pub permission_stashed_prompt: Option<StashedPrompt>,
    /// `exit_plan_mode` deferred freeform prefill because permission owned the
    /// keyboard. Cleared when `restore_permission_stashes` applies it (or plan
    /// review ends). Must not run on unrelated restore calls (e.g. YOLO).
    pub plan_freeform_prefill_deferred: bool,
    /// Scrollback focus stolen for a permission prompt; restored when the queue empties.
    pub permission_stashed_pane: Option<AgentPane>,
    /// Free-form "Always allow" pattern editor buffer for the front request.
    /// `Some` only in `PermissionFocus::PatternEdit`; cleared when the request
    /// resolves or the edit is cancelled.
    pub permission_pattern_edit: Option<crate::views::permission_view::PatternEditState>,
    /// Active plan approval view (from `exit_plan_mode` ext_method). When `Some`,
    /// the prompt area shows the plan approval overlay and input is modal.
    pub(crate) plan_approval_view: Option<PlanApprovalViewState>,
    pub(crate) latest_inline_plan_content: Option<String>,
    pub(crate) plan_comments: Vec<PlanComment>,
    /// Monotonic counter for casual plan comment IDs.
    pub(crate) plan_next_comment_id: u64,
    /// Line range for the casual comment being composed (1-based).
    pub(crate) casual_commenting_range: Option<std::ops::Range<usize>>,
    /// Comment being edited in casual mode (if any).
    pub(crate) casual_editing_comment_id: Option<u64>,
    /// Prompt text stashed when entering casual commenting — restored
    /// on save/cancel. Mirrors `plan_approval_view.stashed_prompt` so
    /// the casual plan modal can share the same prompt-input UX.
    pub(crate) casual_stashed_prompt: Option<StashedPrompt>,
    /// Non-blocking cancel-turn panel (QA-style, shown when cancelling with running subagents).
    pub(crate) cancel_turn_view: Option<modal::CancelTurnViewState>,
    /// Clickable rects for cancel-turn option rows, populated by
    /// `render_cancel_turn_panel`.
    pub(crate) cancel_turn_buttons: Vec<Rect>,
    /// Per-agent mirror of cancel-subagents preference (`Some(true)` = always
    /// stop, `Some(false)` = always continue). Always choices set this on every
    /// agent and persist to `[ui].cancel_subagents_on_turn_cancel`; when unset,
    /// cancel falls back to that UI/config field, then the prompt panel.
    pub(crate) cancel_subagents_preference: Option<bool>,
    /// What gesture triggered the pending turn-cancel (Ctrl+C / mouse; Esc
    /// via the mid-turn cancel in minimal / non-vim mode and the cancel-retry
    /// path while TurnCancelling).
    /// Set by the key/mouse handler, consumed by `do_cancel_turn` / the
    /// cancel-retry path so `session/cancel` carries `_meta.cancelTrigger`.
    pub(crate) cancel_trigger_hint: Option<crate::app::actions::CancelTrigger>,
    pub(crate) rewind_state: Option<crate::views::rewind::RewindState>,
    pub(crate) rewind_points: Option<Vec<crate::views::rewind::RewindPointInfo>>,
    /// In-place edit of a previous user prompt. See `inline_edit.rs`.
    pub(crate) inline_edit: Option<crate::app::inline_edit::InlineEditState>,
    /// Edited text awaiting its rewind; `dispatch_rewind_success` resubmits it.
    /// Set only when the rewind flow emits `Effect::RewindExecute` while the
    /// inline editor is open (see `stash_inline_resubmit_if_editing`).
    pub(crate) pending_inline_resubmit: Option<String>,
    /// `/jump` picker overlay (pure client-side turn navigation).
    pub(crate) jump_state: Option<crate::views::jump::JumpState>,
    /// Timeline sidebar rail geometry for the current frame (`None` =
    /// hidden). Set by the renderer, consumed by mouse hit-testing.
    pub(crate) timeline_rail: Option<crate::views::timeline::TimelineRail>,
    /// Rail part under the mouse — drives hover styling + the tick
    /// preview popup.
    pub(crate) timeline_hover: Option<crate::views::timeline::TimelineHit>,
    /// Cached tick-hover preview `(turn_idx, text)`. Filled when hover
    /// lands on a tick; borrowed during render so streaming redraws don't
    /// rescan/allocate the prompt every frame.
    pub(crate) timeline_hover_preview: Option<(usize, String)>,
    /// Running agent definition for this session (`echo.agent/session/info` `agentName`).
    pub session_agent_name: Option<String>,
    /// Map of child session IDs to subagent metadata. Populated on
    /// `SubagentSpawned` notifications, used for permission routing
    /// (which agent owns a session) and provenance display.
    pub subagent_sessions: HashMap<String, SubagentInfo>,
    /// Child subagent views. Keyed by child_session_id.
    /// Created eagerly on SubagentSpawned so updates are tracked from the start.
    pub subagent_views: HashMap<String, Box<AgentView>>,
    /// Currently open subagent view (child_session_id). When Some, the
    /// scrollback area is replaced by the subagent's framed view.
    pub active_subagent: Option<String>,
    /// When true, this AgentView is rendering as a subagent (read-only):
    /// - Prompt is hidden
    /// - Cancel turn / demote to bg shortcuts are disabled
    /// - Shortcuts bar shows subagent-specific hints
    pub is_subagent_view: bool,
    /// Hit area for the [✗] close button in the subagent frame title bar.
    pub hit_subagent_frame_close: HitArea,
    /// Whether the `/share` slash command is available (mirrors
    /// `AppView::sharing_enabled`). Used to gate palette entries.
    pub sharing_enabled: bool,
    /// Whether THIS session's scheduled fires run as detached background
    /// subagents, as resolved by the shell when the session's actor spawned and
    /// delivered on the `session/new` / `session/load` response. `/loop` reads
    /// it to describe the runtime a fire will get. `None` until that response
    /// lands (or against a shell that predates the key), where readers fall
    /// back to `AppView::scheduler_background_loops_seed`. Deliberately NOT
    /// refreshed by `echo.agent/settings/update`: the fire side is pinned for the
    /// session's lifetime, so a live mirror would drift out of agreement.
    pub scheduler_background_loops: Option<bool>,
    /// Mirrors `AppView::usage_visible` (credit warning + `/usage manage`).
    pub billing_surface_visible: bool,
    /// Whether `/usage` is offered. Mirrors `!AppView::has_external_auth_provider`.
    pub usage_command_visible: bool,
    /// Input flight recorder — rolling buffer of recent key events.
    /// Dumped to file via Esc→d combo for debugging.
    pub(crate) input_log: crate::input_log::InputRingBuffer,
    /// Timestamp of the last Esc press, used for Esc→d combo (dump input log).
    /// Cleared on any non-`d` key press, after 500ms expiry, or once
    /// `try_handle_esc_policy` consumes the Esc. `pub(crate)` for policy tests.
    pub(crate) esc_pressed_at: Option<std::time::Instant>,
    /// Post-cancel grace deadline: while `now` is before it, the Esc policy
    /// holds the idle rewind ARM so Esc-mashing past a cancel cannot
    /// silently arm the rewind picker. Set (`now + ESC_CANCEL_REWIND_GRACE`)
    /// by `suppress_rewind_arm` on every Esc-fired cancel, consumed and
    /// retired-on-expiry by `rewind_arm_suppressed`. `pub(crate)` for policy
    /// tests.
    pub(crate) rewind_suppress_deadline: Option<std::time::Instant>,
    /// First prompt to enqueue once the session finishes loading replay.
    /// Set by `/fork` when a directive is provided; drained in the
    /// `TaskResult::SessionLoaded` arm via `enqueue_prompt_front` so the
    /// directive runs ahead of any prompts the user typed during the
    /// placeholder window.
    pub(crate) pending_first_prompt: Option<String>,
    /// Deferred fork banner to push at the bottom of the scrollback once
    /// the fork session finishes loading (in `TaskResult::SessionLoaded`).
    /// Set by `dispatch_fork_resolved`; stores the parent session id and
    /// worktree flag so the banner can be formatted with the child's
    /// session id (not known until `SessionLoaded`). `None` for non-fork
    /// sessions.
    ///
    /// Cleared on all failure paths: `SessionLoadFailed`,
    /// `WorktreeSessionFailed` (non-orphan branch), and
    /// `ForkSessionFailed`.
    pub(crate) pending_fork_banner: Option<PendingForkBanner>,
    /// Entry ID of the "Loading session ..." placeholder block pushed
    /// by `dispatch_load_session_inner`. Cleared by the `SessionLoaded`
    /// handler so the placeholder doesn't linger on screen when the
    /// loaded session has no replay content.
    pub(crate) loading_placeholder_id: Option<EntryId>,
    /// Entry ID of the in-flight manual `/recap` loading block (rendered with
    /// the animated "running" sidebar). Set when `/recap` is dispatched and
    /// taken by the `SessionRecap` handler, which fills the block with the
    /// summary and stops the animation. `None` when no manual recap is
    /// pending (auto recaps never show a loading block).
    pub(crate) pending_recap_entry: Option<EntryId>,
    /// The manually-chosen session title (`/rename` or the dashboard
    /// rename flow), as distinct from the auto-generated
    /// `generated_session_title` below. Set optimistically at dispatch,
    /// persisted by the shell as `Summary.title_is_manual`, and restored
    /// from disk on resume (`TaskResult::SessionMetaFromDisk`). Drives the
    /// prompt-border inline title and wins precedence for the dashboard
    /// modal label and the OSC terminal title. The on-disk write is
    /// best-effort (failure surfaces a system block through the existing
    /// `RenameSessionFailed` arm).
    pub display_name: Option<String>,
    /// Short title from shell `SessionSummaryGenerated` or `summary.json` on load/resume.
    /// Precedence in the dashboard title is below `display_name`, above first-prompt text.
    pub generated_session_title: Option<String>,
    /// Shell already fanned out `titleIsManual: false` for this unpin. A
    /// dropped RPC then surfaces `ResetSessionTitleFailed`; restoring the
    /// pin would stick a manual title that later absent-meta auto titles
    /// refuse to clear.
    pub title_unpin_committed: bool,
    /// Ultra-short summary of the most recent successful turn (shell
    /// `LastTurnSummary`), preferred over the last-message preview for the
    /// idle dashboard row's secondary line. Shown until replaced by the next
    /// successful turn's summary; cleared only by a conversation rewind
    /// (which removes the work it describes).
    pub last_turn_summary: Option<String>,
    /// Bumped on every live mutation of [`Self::last_turn_summary`] (notification
    /// apply or rewind clear). Disk hydration captures this at enqueue and
    /// applies only when it still matches, so a rewind that cleared the field
    /// while the read was in flight is not undone by a stale disk value.
    pub last_turn_summary_gen: u64,
    /// Effects queued by input handlers that cannot return `InputOutcome::Action`.
    /// Drained by `AppView.handle_input` after each event.
    pub(crate) pending_effects: Vec<super::actions::Effect>,
    /// In-flight deferred clipboard attachment probes for this prompt. A send
    /// while `> 0` is stashed (see `deferred_send`) so a
    /// paste-then-immediate-send never builds content blocks before the image
    /// attaches.
    pub(crate) paste_probe_in_flight: usize,
    /// A prompt send / interject deferred until the in-flight paste probe(s)
    /// complete. Kind-only: the payload is re-derived from the widget on
    /// reissue so the freshly attached image chip travels with it.
    pub(crate) deferred_send: Option<AgentDeferredSend>,
    /// Armed when an `echo.agent/session/prompt_complete` broadcast arrives for the
    /// turn THIS client drives while it is still awaiting that turn's
    /// `session/prompt` RPC response. The RPC normally lands milliseconds
    /// later and disarms this; if it never does (lost in leader response
    /// routing / reconnect races), the event loop reconciles turn state from
    /// the broadcast after [`super::dispatch::TURN_END_RECONCILE_GRACE`] so
    /// the pane cannot stay latched in `TurnRunning`/`TurnCancelling` forever
    /// (a lost response would otherwise leave the TUI on "Cancelling…" with
    /// Esc and the input dead until a restart).
    pub(crate) pending_turn_end_reconcile: Option<PendingTurnEnd>,
    /// Armed whenever `Effect::CancelTurn` leaves the pane cancelling:
    /// `session/cancel` is fire-and-forget with known loss windows, so the
    /// event loop re-sends the idempotent cancel after
    /// [`super::dispatch::CANCEL_RESEND_GRACE`] while still cancelling.
    pub(crate) pending_cancel_resend: Option<PendingCancelResend>,
    pub(crate) cancel_latency: Option<CancelLatency>,
    /// Send-now cancel expectation: the client-minted id of an explicit
    /// cancel-and-send this client dispatched into a running turn (send-now
    /// chord / `SendPromptNow`, or queue-row "Send now"). The running turn's
    /// imminent cancel is the silent half of cancel-and-send, so the turn-end
    /// rails suppress the "Turn cancelled by user …" marker.
    ///
    /// Compat fallback only: a wire `_meta.cancelTrigger` on the turn end is
    /// trusted over this flag (`"send_now"` suppresses, anything else
    /// renders). Consumed at every driver turn end. Kept across the matching
    /// send-now prompt's turn start (so the outgoing turn's cancel
    /// PromptResponse can still suppress the marker when it races behind the
    /// adopt), but cleared on a non-matching turn start / interactive cancel /
    /// replay-window entry, so a stale expectation can never eat a later real
    /// Ctrl+C marker.
    pub(crate) expect_send_now_cancel: Option<String>,
    /// Cleared at turn start; set on the first live non-echo update. Defaults true.
    pub(crate) front_message_committed: bool,
    /// Send-now promote: skip `scroll_to_entry_top` on next matching adoption.
    /// Survives cancel-rail `take()` of [`Self::expect_send_now_cancel`].
    pub(crate) follow_without_jump_prompt_id: Option<String>,
    /// Ids of THIS client's server-queue rows that are still optimistic
    /// echoes — the `session/prompt` RPC is in flight and no
    /// `echo.agent/queue/changed` broadcast has confirmed the row yet. Inserted by
    /// the echo push, drained when a broadcast lists the id (queued or
    /// running) or the RPC resolves without the row landing.
    pub(crate) optimistic_queue_ids: std::collections::HashSet<String>,
    /// A queue-row send-now the user fired while the row was still an
    /// optimistic echo. Firing `echo.agent/queue/interject` then would race the
    /// row's own in-flight `session/prompt` and silently no-op shell-side
    /// (a rapid double-Enter on a queued bash command could "disappear" — the
    /// interject overtook the row, the no-op dropped the send-now, and the
    /// armed cancel expectation hid the still-queued row).
    /// Parked here and fired from the confirming `echo.agent/queue/changed`
    /// broadcast with the row's authoritative version.
    pub(crate) send_now_awaiting_confirm: Option<String>,
    /// User blocks painted at send-now dispatch, keyed by prompt id; the
    /// turn-start adoption consumes an entry to reuse its block. The flag
    /// marks an edit-interject override (fresher than the mirror text the
    /// adoption captures). Cleared on session reload.
    pub(crate) send_now_painted_blocks:
        std::collections::HashMap<String, (crate::scrollback::EntryId, bool)>,
    /// Cached official-marketplace candidates for the plugin CTA, populated on
    /// session start independently of the Extensions modal.
    pub plugin_cta: PluginCtaState,
    /// Follow-up suggestion chips for the latest assistant response
    /// (`echo.agent/follow_ups`). `None` when no chips are shown. Set by
    /// [`AgentView::apply_follow_ups`]; cleared at each turn start.
    pub(crate) follow_ups: Option<FollowUps>,
    /// `promptId` (turn identity) of the currently-shown `follow_ups`, when the
    /// delivery that displayed them carried one. Tracked separately because
    /// [`FollowUps`] is keyed by `response_id` and does not carry the turn id.
    /// Used by [`AgentView::reset_follow_ups_for_reload_preserving`] to tell
    /// whether the on-screen chips belong to the running turn a reload is about
    /// to adopt — so a reload preserves chips that RENDERED during replay, not
    /// only those still sitting in the pending buffer. `None` when no chips are
    /// shown or the delivery had no stamped `promptId` (legacy/newest-wins).
    pub(crate) follow_up_shown_prompt_id: Option<String>,
    /// Clickable screen rect of each rendered follow-up chip, index-aligned
    /// with the rendered prefix of `follow_ups.suggestions` (chips that do
    /// not fit the row are omitted). Rebuilt every frame by the renderer;
    /// hit-tested by [`AgentView::follow_up_chip_at`].
    pub(crate) follow_up_chips: Vec<Rect>,
    /// Chip under the mouse (hover highlight). Cleared when chips clear or
    /// the pointer leaves the follow-up row.
    pub(crate) hovered_follow_up_chip: Option<usize>,
    /// Assistant `response_id`s the pager has accepted follow-up chips for, in
    /// strictly-increasing acceptance order (`follow_up_next_gen`). Makes
    /// newest-wins correct on EVERY turn-boundary path without depending on a
    /// clear being wired: an id already accepted is, by construction, older than
    /// the current one, so a re-delivered (buffer-replay/duplicate) chunk for it
    /// is rejected, while a never-seen id is strictly newer and supersedes.
    /// Never evicted — eviction is what would let a stale id masquerade as new.
    /// Bounded in practice by the number of follow-up-bearing turns in a
    /// session. See [`AgentView::apply_follow_ups`].
    pub(crate) follow_up_seen: HashMap<String, u64>,
    /// Monotonic generation assigned to the next newly-accepted `response_id`.
    /// The ordering key for newest-wins: a fresh id takes the next value (the
    /// new high-water), so every previously-seen id is strictly lower.
    pub(crate) follow_up_next_gen: u64,
    /// Stamped `echo.agent/follow_ups` that arrived for a turn that is NOT yet the
    /// currently-adopted one, keyed by `promptId`. Ext notifications and
    /// `session/update` travel on separate channels, so a turn's follow_ups can
    /// land BEFORE the `session/update` that adopts it. Rather than drop such a
    /// delivery (chips would never appear if it was the only one), it is buffered
    /// here and flushed by [`AgentView::flush_pending_follow_ups`] when that
    /// `promptId` becomes current. A `promptId` that is already a prior turn and
    /// never becomes current again is never flushed (so stale chips are not
    /// revived); the buffer is FIFO-bounded by [`MAX_PENDING_FOLLOW_UPS`] via
    /// `follow_up_pending_order`.
    pub(crate) follow_up_pending: HashMap<String, FollowUps>,
    /// Insertion order of `follow_up_pending` keys, so an overflow evicts ONLY
    /// the OLDEST buffered entry (never the whole map).
    pub(crate) follow_up_pending_order: VecDeque<String>,
    /// Live `session/update`s buffered for the stashed pending running
    /// adoption: in the FIFO handoff window an instant turn emits its whole
    /// stream before the previous turn's PromptResponse applies the adoption.
    pub(crate) pending_adoption_updates: Vec<(
        String,
        agent_client_protocol::SessionUpdate,
        crate::acp::meta::NotificationMeta,
    )>,
}
/// Cap on [`AgentView::self_originated_prompt_ids`]. Only recent ids matter (a
/// stale post-rewind chunk arrives right after its turn ends), so a small
/// bounded ring is plenty and keeps a long-lived session from growing the set
/// without bound.
const SELF_ORIGINATED_PROMPT_CAP: usize = 64;
const REWOUND_PROMPT_ID_CAP: usize = 64;
/// Cap on [`AgentView::follow_up_pending`]. Only a handful of turns can ever be
/// "buffered but not-yet-adopted" at once (the ext/`session/update` race window
/// is tiny), so a small bounded map is plenty; an overflow evicts the oldest
/// buffered entry (FIFO) rather than the whole map.
const MAX_PENDING_FOLLOW_UPS: usize = 16;
/// Cap on [`AgentView::pending_adoption_updates`]. Overflow drops the NEWEST
/// entry (unlike the follow-up buffer's oldest-first eviction): a coherent
/// prefix (user echo + tool-call start) renders sanely, a headless tail would not.
pub(crate) const MAX_PENDING_ADOPTION_UPDATES: usize = 128;
/// Outcome of [`AgentView::dashboard_answer_question`] — tells the
/// dashboard dispatcher whether the whole ask form was submitted (close
/// the peek), the form advanced to the next question (keep the peek open
/// but reset its per-question draft), or nothing happened.
pub(crate) enum PeekAnswerOutcome {
    Submitted,
    Advanced,
    NoOp,
}
/// Test-only re-export of [`translate_local_submit`] so dispatch tests
/// can verify the local-question -> Action mapping without spinning up
/// a full agent view.
#[cfg(test)]
pub(crate) fn translate_local_submit_for_test(
    qv: &crate::views::question_view::QuestionViewState,
    kind: crate::views::question_view::LocalQuestionKind,
    skipped: bool,
) -> InputOutcome {
    translate_local_submit(qv, kind, skipped)
}
/// Translate a local-question submission into an [`InputOutcome`].
///
/// Returns `InputOutcome::Action(...)` so the event loop dispatches the
/// action through the normal channel, mirroring the way ACP-driven
/// questions complete via `response_tx.send(..)`. Cancel / skip /
/// invalid-selection paths return `InputOutcome::Changed` and the
/// directive (if one was supplied) is silently dropped -- matching the
/// "no UI for cancellation" stance.
/// Map a worktree-question option index to `(use_worktree, persist_mode)`.
///
/// Indices 0-3 correspond to the four options presented in
/// `open_fork_question` / `open_new_session_question`. Returns `None`
/// for out-of-range indices.
fn worktree_choice_from_index(
    idx: usize,
) -> Option<(bool, Option<crate::app::app_view::WorktreeMode>)> {
    use crate::app::app_view::WorktreeMode;
    match idx {
        0 => Some((true, None)),
        1 => Some((false, None)),
        2 => Some((true, Some(WorktreeMode::Always))),
        3 => Some((false, Some(WorktreeMode::Never))),
        _ => None,
    }
}
fn translate_local_submit(
    qv: &crate::views::question_view::QuestionViewState,
    kind: crate::views::question_view::LocalQuestionKind,
    skipped: bool,
) -> InputOutcome {
    use crate::views::question_view::{LocalQuestionKind, QuestionSelection};
    if skipped {
        return InputOutcome::Changed;
    }
    let Some(QuestionSelection::Single(Some(idx))) = qv.selections.first() else {
        if let LocalQuestionKind::FeedbackTrace { report, images } = kind {
            return InputOutcome::Action(Action::SendFeedback {
                text: report,
                images,
                trace: Some(crate::app::actions::FeedbackTraceChoice::NoUpload),
            });
        }
        return InputOutcome::Changed;
    };
    match kind {
        LocalQuestionKind::Fork { directive } => {
            let Some((worktree, persist_mode)) = worktree_choice_from_index(*idx) else {
                return InputOutcome::Changed;
            };
            InputOutcome::Action(Action::ForkAnswered {
                worktree,
                directive,
                persist_mode,
            })
        }
        LocalQuestionKind::NewSession => {
            let Some((worktree, persist_mode)) = worktree_choice_from_index(*idx) else {
                return InputOutcome::Changed;
            };
            InputOutcome::Action(Action::NewSessionAnswered {
                worktree,
                persist_mode,
            })
        }
        LocalQuestionKind::CreditLimitUpsell { choices } => {
            let q = qv.questions.first();
            let url = q
                .and_then(|q| q.options.get(*idx))
                .and_then(|o| o.id.as_deref())
                .unwrap_or(super::dispatch::UPSELL_URL_PAYG);
            let choice = choices
                .get(*idx)
                .copied()
                .unwrap_or(echo_agent_telemetry::events::CreditLimitChoice::PayAsYouGo);
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::CreditLimitUpsellClicked {
                    surface: echo_agent_telemetry::events::CreditLimitUpsellSurface::QuestionModal,
                    choice,
                },
            );
            InputOutcome::Action(Action::OpenUrl(url.to_string()))
        }
        LocalQuestionKind::FreeUsageUpsell { source } => {
            let url = qv
                .questions
                .first()
                .and_then(|q| q.options.get(*idx))
                .and_then(|o| o.id.as_deref())
                .unwrap_or(super::dispatch::UPSELL_URL_UPGRADE);
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::EchoAgentUpsellClicked {
                    source,
                    auth_method: None,
                },
            );
            InputOutcome::Action(Action::OpenUrl(url.to_string()))
        }
        LocalQuestionKind::AgentTypeMismatch { model_id, effort } => {
            let start_new = *idx == 0;
            InputOutcome::Action(Action::AgentTypeMismatchAnswered {
                start_new,
                model_id: model_id.clone(),
                effort,
            })
        }
        LocalQuestionKind::DoctorFix { target, plan } => {
            if *idx == 0 {
                InputOutcome::Action(Action::DoctorFixConfirmed { target, plan })
            } else {
                InputOutcome::Action(Action::DoctorFixCancelled(target))
            }
        }
        LocalQuestionKind::DeleteCurrentSession => {
            InputOutcome::Action(Action::DeleteCurrentSessionAnswered {
                confirmed: *idx == 0,
            })
        }
        LocalQuestionKind::Feedback => {
            unreachable!(
                "feedback report submits through submit_feedback_pane, which returns first"
            )
        }
        LocalQuestionKind::FeedbackTrace { report, images } => {
            use crate::app::actions::FeedbackTraceChoice;
            use crate::views::question_view::{
                FEEDBACK_TRACE_OPTION_NEVER_ASK, FEEDBACK_TRACE_OPTION_OPT_IN,
                FEEDBACK_TRACE_OPTION_OPT_OUT,
            };
            let id = qv
                .questions
                .first()
                .and_then(|q| q.options.get(*idx))
                .and_then(|o| o.id.as_deref());
            let trace = match id {
                Some(FEEDBACK_TRACE_OPTION_OPT_IN) => FeedbackTraceChoice::AlwaysUpload,
                Some(FEEDBACK_TRACE_OPTION_NEVER_ASK) => FeedbackTraceChoice::NeverAsk,
                Some(FEEDBACK_TRACE_OPTION_OPT_OUT) => FeedbackTraceChoice::NoUpload,
                other => {
                    debug_assert!(false, "trace-consent option without a known id: {other:?}");
                    FeedbackTraceChoice::NoUpload
                }
            };
            InputOutcome::Action(Action::SendFeedback {
                text: report,
                images,
                trace: Some(trace),
            })
        }
    }
}
/// Convert an [`OverlayAction`] to an [`InputOutcome`].
fn overlay_action_to_outcome(action: crate::views::overlay::OverlayAction) -> InputOutcome {
    use crate::views::overlay::OverlayAction;
    match action {
        OverlayAction::Ignored => InputOutcome::Unchanged,
        OverlayAction::Changed => InputOutcome::Changed,
        OverlayAction::FocusScrollback => InputOutcome::Action(Action::FocusScrollback),
        OverlayAction::FocusPrompt => InputOutcome::Action(Action::FocusPrompt),
    }
}
/// Render dropdown chrome (borders, count hint) anchored to the prompt and
/// return the inner items area. Returns `None` when geometry doesn't fit.
///
/// `below = false` anchors the panel *above* the prompt (full-TUI default);
/// `below = true` anchors it *below* the prompt (minimal mode, common CLI
/// style, to reduce layout shift).
///
/// Shared by slash dropdown and completion dropdown to avoid duplicated chrome code.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_dropdown_chrome(
    buf: &mut Buffer,
    item_count: usize,
    item_rows: u16,
    inline_prompt_area: Option<Rect>,
    layout_prompt: Rect,
    area: Rect,
    layout_cfg: &crate::appearance::LayoutConfig,
    compact: bool,
    below: bool,
    theme: &Theme,
) -> Option<DropdownChrome> {
    let mut panel_height = item_rows + 2;
    let (top_border_y, bottom_border_y) = if below {
        let anchor = inline_prompt_area.unwrap_or(layout_prompt);
        let top = anchor.y + anchor.height;
        (top, top + panel_height - 1)
    } else {
        let bottom = if let Some(ipa) = inline_prompt_area {
            ipa.y.saturating_sub(1)
        } else {
            layout_prompt.y.saturating_sub(1)
        };
        let avail = bottom.saturating_sub(area.y).saturating_add(1);
        panel_height = panel_height.min(avail);
        if panel_height < 3 {
            return None;
        }
        (bottom.saturating_sub(panel_height - 1), bottom)
    };
    let embedded = crate::views::modal_window::embedded();
    let (hpad_left, hpad_right) = if embedded {
        (0, 0)
    } else {
        (
            layout_cfg.eff_hpad_left(compact),
            layout_cfg.eff_hpad_right(compact),
        )
    };
    let panel_x = area.x + hpad_left;
    let panel_width = area.width.saturating_sub(hpad_left + hpad_right);
    if top_border_y >= bottom_border_y || panel_width <= 4 {
        return None;
    }
    if below && bottom_border_y > area.y + area.height.saturating_sub(1) {
        return None;
    }
    let panel_area = Rect {
        x: panel_x,
        y: top_border_y,
        width: panel_width,
        height: panel_height,
    };
    if panel_area.bottom() > buf.area.bottom()
        || panel_area.right() > buf.area.right()
        || panel_area.y < buf.area.y
    {
        return None;
    }
    ratatui::widgets::Clear.render(panel_area, buf);
    if embedded {
        let reset = ratatui::style::Color::Reset;
        let divider_style = Style::default().fg(theme.gray_dim).bg(reset);
        let divider = Line::styled("\u{2500}".repeat(panel_width as usize), divider_style);
        buf.set_line_safe(panel_x, top_border_y, &divider, panel_width);
        let footer = "\u{2191}/\u{2193} navigate \u{00b7} enter confirm \u{00b7} esc cancel";
        let footer_line = Line::styled(
            footer.to_string(),
            Style::default().fg(theme.gray_dim).bg(reset),
        );
        buf.set_line_safe(
            panel_x + 1,
            bottom_border_y,
            &footer_line,
            panel_width.saturating_sub(1),
        );
    } else {
        buf.set_style(
            panel_area,
            Style::default().fg(theme.text_primary).bg(theme.bg_light),
        );
        let border_style = Style::default().fg(theme.bg_highlight).bg(theme.bg_base);
        let border_line = Line::styled("\u{2500}".repeat(panel_width as usize), border_style);
        buf.set_line_safe(panel_x, top_border_y, &border_line, panel_width);
        buf.set_line_safe(panel_x, bottom_border_y, &border_line, panel_width);
        let hint = format!("{}", item_count);
        let hint_w = hint.len() as u16;
        if hint_w + 2 <= panel_width {
            let hint_x = panel_x + panel_width - hint_w - 1;
            let hint_line = Line::styled(hint, Style::default().fg(theme.gray).bg(theme.bg_base));
            buf.set_line_safe(hint_x, top_border_y, &hint_line, hint_w);
        }
    }
    let content_inset = dropdown_content_inset();
    let items_x = layout_prompt.x + content_inset;
    let items_width = layout_prompt.width.saturating_sub(content_inset);
    Some(DropdownChrome {
        items: Rect {
            x: items_x,
            y: top_border_y + 1,
            height: panel_height - 2,
            width: items_width,
        },
        panel: panel_area,
    })
}
/// Left inset of dropdown item rows inside the panel (see the comment in
/// [`render_dropdown_chrome`]).
pub(crate) fn dropdown_content_inset() -> u16 {
    if crate::views::modal_window::embedded() {
        0
    } else {
        2
    }
}
/// Width of the dropdown item rows [`render_dropdown_chrome`] will produce for
/// `layout_prompt` — for sizing the row count *before* drawing the chrome.
pub(crate) fn dropdown_items_width(layout_prompt: Rect) -> u16 {
    layout_prompt.width.saturating_sub(dropdown_content_inset())
}
/// Geometry returned by [`render_dropdown_chrome`]: the inset `items` area for
/// rendering rows and the full `panel` rect (borders + padding) used as an
/// occluder for hyperlink hit-testing.
pub(crate) struct DropdownChrome {
    pub(crate) items: Rect,
    pub(crate) panel: Rect,
}
/// Render a row of 1-char buttons right-aligned, returning their hit-test rects.
///
/// Buttons are rendered right-to-left starting from `right_x`. Each button
/// is 1 cell wide, separated by `gap` cells. The `base_style` is used for
/// non-hovered buttons; `hover_style` for hovered ones.
///
/// Returns one `Rect` per button, in the same order as the input iterator.
fn render_char_buttons<const N: usize>(
    buf: &mut Buffer,
    right_x: u16,
    y: u16,
    buttons: [(&str, bool); N],
    base_style: Style,
    hover_style: Style,
    gap: u16,
) -> [Rect; N] {
    let mut areas = [Rect::default(); N];
    let mut x = right_x;
    for i in (0..N).rev() {
        let (sym, hovered) = buttons[i];
        let style = if hovered { hover_style } else { base_style };
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(sym);
            cell.set_style(style);
        }
        areas[i] = Rect::new(x, y, 1, 1);
        x = x.saturating_sub(1 + gap);
    }
    areas
}
/// Whether this key event represents `!` (bang).
///
/// Most terminals report `KeyCode::Char('!')` directly. Under the Kitty
/// keyboard protocol the terminal sends the base key `1` with SHIFT
/// modifier instead of the produced character (crossterm#968).
fn is_bang_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('!')
        || (key.code == KeyCode::Char('1') && key.modifiers.contains(KeyModifiers::SHIFT))
}
/// Translate a `SettingsKeyOutcome` into an `InputOutcome`.
pub(super) fn apply_settings_outcome(
    agent: &mut AgentView,
    outcome: crate::views::settings_modal::SettingsKeyOutcome,
) -> InputOutcome {
    use crate::views::settings_modal::SettingsKeyOutcome;
    match outcome {
        SettingsKeyOutcome::Close => {
            agent.active_modal = None;
            InputOutcome::Changed
        }
        SettingsKeyOutcome::Action(a) => InputOutcome::Action(a),
        SettingsKeyOutcome::ActionPair(a, b) => InputOutcome::ActionPair(a, b),
        SettingsKeyOutcome::ActionThenClose(a) => {
            agent.active_modal = None;
            InputOutcome::Action(a)
        }
        SettingsKeyOutcome::Changed => InputOutcome::Changed,
        SettingsKeyOutcome::Unchanged => InputOutcome::Unchanged,
    }
}
/// Whether this key event represents `#` (hash).
///
/// Most terminals report `KeyCode::Char('#')` directly. Under the Kitty
/// keyboard protocol the terminal sends the base key `3` with SHIFT
/// modifier instead of the produced character (crossterm#968).
fn is_hash_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('#')
        || (key.code == KeyCode::Char('3') && key.modifiers.contains(KeyModifiers::SHIFT))
}
/// Check `[features] remember_mode` in config.toml. Defaults to `false`.
fn remember_mode_enabled() -> bool {
    let path = echo_agent_tools::util::echo_agent_home::echo_agent_home().join("config.toml");
    let Some(doc) = crate::config_toml_edit::read_config_document_for_edit(&path) else {
        return false;
    };
    doc.get("features")
        .and_then(|f| f.get("remember_mode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}
/// Mouse reporting toggle chord (Ctrl+R on scrollback), for unified-log diagnostics.
fn is_mouse_reporting_toggle_chord(key: &KeyEvent) -> bool {
    crate::key!('r', CONTROL).matches(key)
}
fn format_key_for_log(key: &KeyEvent) -> serde_json::Value {
    serde_json::json!({
        "code": format!("{:?}", key.code),
        "modifiers": format!("{:?}", key.modifiers),
        "kind": format!("{:?}", key.kind),
    })
}
fn resolve_action(action_id: Option<ActionId>) -> Option<InputOutcome> {
    let action = match action_id? {
        ActionId::SendPrompt => return None,
        ActionId::SelectNext => Action::SelectNext,
        ActionId::SelectPrev => Action::SelectPrev,
        ActionId::NextTurn => Action::NextTurn,
        ActionId::PrevTurn => Action::PrevTurn,
        ActionId::NextResponse => Action::NextResponse,
        ActionId::PrevResponse => Action::PrevResponse,
        ActionId::GotoTop => Action::GotoTop,
        ActionId::GotoBottom => Action::GotoBottom,
        ActionId::ScrollUp => Action::ScrollUp(1),
        ActionId::ScrollDown => Action::ScrollDown(1),
        ActionId::HalfPageUp => Action::HalfPageUp,
        ActionId::HalfPageDown => Action::HalfPageDown,
        ActionId::PageUp => Action::PageUp,
        ActionId::PageDown => Action::PageDown,
        ActionId::Collapse => Action::Collapse,
        ActionId::Expand => Action::Expand,
        ActionId::ToggleFold => Action::ToggleFold,
        ActionId::ToggleExpandAll => Action::ToggleExpandAll,
        ActionId::ExpandAllThinking => Action::ExpandAllThinking,
        ActionId::ToggleRaw => Action::ToggleRaw,
        ActionId::ToggleMouseCapture => Action::ToggleMouseCapture,
        ActionId::CopyBlockContent => Action::CopyBlockContent,
        ActionId::CopyBlockMeta => Action::CopyBlockMeta,
        ActionId::OpenBlockViewer => Action::OpenBlockViewer,
        ActionId::OpenNextLink => Action::OpenNextLink,
        ActionId::OpenPrevLink => Action::OpenPrevLink,
        ActionId::FocusPrompt => Action::FocusPrompt,
        ActionId::FocusScrollback => Action::FocusScrollback,
        ActionId::NextModel => Action::NextModel,
        ActionId::CycleMode => Action::CycleMode,
        ActionId::CancelTurn
        | ActionId::Quit
        | ActionId::ExitSession
        | ActionId::NewSession
        | ActionId::NewSessionInWorktree
        | ActionId::CommandPalette
        | ActionId::ModelPicker => return None,
        ActionId::DumpInputLog => return None,
        ActionId::ToggleYolo => return None,
        ActionId::ToggleMultiline => return None,
        ActionId::InterjectPrompt => return None,
        ActionId::StashPrompt => return None,
        ActionId::EnableVoiceMode => Action::EnableVoiceMode,
        ActionId::VoiceToggle => {
            if !crate::app::voice_keybind_enabled() {
                return None;
            }
            Action::VoiceToggle
        }
        ActionId::ShortcutsHelp => return None,
        ActionId::OpenSettings => return None,
        ActionId::ToggleTodos
        | ActionId::ToggleTasks
        | ActionId::EditPromptExternal
        | ActionId::ToggleQueue
        | ActionId::OpenSessions
        | ActionId::OpenExtensions
        | ActionId::SendToBackground
        | ActionId::BashMode
        | ActionId::Rewind
        | ActionId::KillBgTask
        | ActionId::OpenDashboard
        | ActionId::DashboardSelectNext
        | ActionId::DashboardSelectPrev
        | ActionId::DashboardTogglePin
        | ActionId::DashboardBeginRename
        | ActionId::DashboardStop
        | ActionId::DashboardCycleMode
        | ActionId::DashboardToggleGrouping
        | ActionId::DashboardReorderUp
        | ActionId::DashboardReorderDown
        | ActionId::DashboardShortcutsHelp
        | ActionId::DashboardExit
        | ActionId::DashboardOverlayExit
        | ActionId::DashboardOverlayPrev
        | ActionId::DashboardOverlayNext
        | ActionId::DashboardOverlayStop
        | ActionId::DashboardToggleAutoApprove
        | ActionId::DashboardOpenLocationPicker
        | ActionId::DashboardToggleWorktree => return None,
    };
    Some(InputOutcome::Action(action))
}
/// Visible height of the scrollable options area, from the render-computed
/// scroll region or a fallback estimate (footer=3, sticky freeform=1).
/// `sticky_freeform_h` is 0 for `no_freeform` questions (no sticky row is
/// rendered) and 1 otherwise.
#[allow(clippy::too_many_arguments)]
fn question_visible_h(
    scroll_region: Option<(u16, u16)>,
    prompt_height: u16,
    question: &echo_agent_tools::implementations::echo_agent_build::ask_user_question::Question,
    content_w: usize,
    preview: Option<&str>,
    fullscreen: bool,
    desc_cap: u16,
    preview_cap: u16,
    sticky_freeform_h: u16,
) -> u16 {
    if let Some((top, bottom)) = scroll_region {
        bottom.saturating_sub(top)
    } else {
        let question_area_h = prompt_height.saturating_sub(3);
        crate::views::question_view::visible_options_height(
            question,
            question_area_h,
            content_w,
            preview,
            fullscreen,
            desc_cap,
            preview_cap,
        )
        .saturating_sub(sticky_freeform_h)
    }
}
/// Collect citation URLs from visible WebSearch and WebFetch tool blocks.
///
/// Returns `VisibleLink` entries with the entry's rendered content area.
/// Only visible blocks (from the selection model) are scanned.
fn collect_citation_links(
    scrollback: &ScrollbackState,
    selection_model: &ResolvedSelectionModel,
) -> Vec<crate::scrollback::link_map::VisibleLink> {
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::blocks::tool::ToolCallBlock;
    use crate::scrollback::link_map::VisibleLink;
    use std::sync::Arc;
    let mut links = Vec::new();
    for block_geom in &selection_model.visible_blocks {
        let Some(entry) = scrollback.entry(block_geom.entry_idx) else {
            continue;
        };
        match &entry.block {
            RenderBlock::ToolCall(ToolCallBlock::WebSearch(ws)) => {
                for url in &ws.citations {
                    links.push(VisibleLink {
                        rects: vec![block_geom.content_area],
                        target: crate::render::osc8::LinkTarget::Url(Arc::from(url.as_str())),
                        id: None,
                    });
                }
            }
            RenderBlock::ToolCall(ToolCallBlock::WebFetch(wf)) => {
                if !wf.url.is_empty() {
                    links.push(VisibleLink {
                        rects: vec![block_geom.content_area],
                        target: crate::render::osc8::LinkTarget::Url(Arc::from(wf.url.as_str())),
                        id: None,
                    });
                }
            }
            _ => {}
        }
    }
    links
}
/// Shared fixtures for the queue-routing tests here, the queued-prompt
/// editing tests in `queue_edit.rs`, and the parked-wait tests in
/// `dispatch/queue.rs` / `acp_handler.rs`.
#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::{AgentPane, AgentView};
    use crate::acp::model_state::ModelState;
    use crate::actions::ActionRegistry;
    use crate::app::agent::{AgentId, AgentSession, AgentState};
    use crate::app::prompt_queue::QueueEntryWire;
    use crate::scrollback::state::ScrollbackState;
    use agent_client_protocol as acp;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    pub(crate) fn make_followup_permission_state()
    -> crate::views::permission_view::PermissionViewState {
        let (response_tx, _rx) = tokio::sync::oneshot::channel();
        let request = agent_client_protocol::RequestPermissionRequest::new(
            agent_client_protocol::SessionId::new(std::sync::Arc::from("test")),
            agent_client_protocol::ToolCallUpdate::new(
                agent_client_protocol::ToolCallId::new(std::sync::Arc::from("call-1")),
                agent_client_protocol::ToolCallUpdateFields::default(),
            ),
            vec![],
        );
        let perm = echo_agent_acp::AcpArgs {
            request,
            response_tx,
        };
        crate::views::permission_view::PermissionViewState {
            request: perm,
            id: 0,
            focus: crate::views::permission_view::PermissionFocus::FollowupInput,
            options: vec![],
            active_idx: 0,
            bash_highlights: None,
            bash_selection_count: 0,
            bash_deny_selection_count: 0,
            bash_command_raw: None,
            mcp_scope: None,
            title: String::new(),
            description: vec![],
            args_expanded: false,
            desc_scroll: 0,
            subagent_label: None,
            options_area_height: 0,
            options_scroll_offset: 0,
        }
    }
    pub(crate) fn make_plan_approval_view_state()
    -> crate::views::plan_approval_view::PlanApprovalViewState {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "test-session".into(),
            tool_call_id: "call-1".into(),
            plan_content: Some("# Plan\n\n## Step 1\nDo something".into()),
        };
        crate::views::plan_approval_view::PlanApprovalViewState::new(
            request,
            crate::views::prompt_widget::StashedPrompt {
                text: String::new(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
            tx,
        )
    }
    /// Drive the agent's tracker into a task-output wait via the real update
    /// path. `timeout_ms > 0` advertises a blocking (sendable/parked) wait;
    /// `0` is an instant poll that must NOT advertise one.
    pub fn simulate_task_output_wait_ms(agent: &mut AgentView, task_id: &str, timeout_ms: u64) {
        simulate_task_output_wait_call(agent, "wait-1", task_id, timeout_ms);
    }
    /// [`simulate_task_output_wait_ms`] with an explicit tool-call id.
    pub fn simulate_task_output_wait_call(
        agent: &mut AgentView,
        tool_call_id: &str,
        task_id: &str,
        timeout_ms: u64,
    ) {
        use crate::acp::meta::NotificationMeta;
        use crate::acp::tracker::{TurnActivity, WaitingReason};
        use std::sync::Arc;
        agent.front_message_committed = true;
        let meta = NotificationMeta::default();
        agent.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(
                    acp::ToolCallId::new(Arc::from(tool_call_id)),
                    "get_command_or_subagent_output",
                )
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .content(vec![])
                .locations(vec![]),
            ),
            &meta,
            &mut agent.scrollback,
        );
        agent.session.handle_update(
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new().raw_input(Some(serde_json::json!({
                    "task_ids": [task_id],
                    "timeout_ms": timeout_ms,
                }))),
            )),
            &meta,
            &mut agent.scrollback,
        );
        let activity = agent.resolve_turn_activity();
        if timeout_ms > 0 {
            assert!(
                matches!(
                    activity,
                    Some(TurnActivity::Waiting(WaitingReason::TaskOutput {
                        waits: true,
                        ..
                    }))
                ),
                "expected TaskOutput wait, got {activity:?}"
            );
        } else {
            assert!(
                !matches!(
                    activity,
                    Some(TurnActivity::Waiting(WaitingReason::TaskOutput { .. }))
                ),
                "poll must not advertise a task-output wait, got {activity:?}"
            );
        }
    }
    /// Complete a wait tool call registered by
    /// [`simulate_task_output_wait_call`], releasing its blocking-wait entry.
    pub fn complete_task_output_wait_call(agent: &mut AgentView, tool_call_id: &str) {
        use crate::acp::meta::NotificationMeta;
        use std::sync::Arc;
        let meta = NotificationMeta::default();
        agent.session.handle_update(
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(tool_call_id)),
                acp::ToolCallUpdateFields::new().status(Some(acp::ToolCallStatus::Completed)),
            )),
            &meta,
            &mut agent.scrollback,
        );
    }
    /// Blocking-wait shorthand for [`simulate_task_output_wait_ms`].
    pub fn simulate_task_output_wait(agent: &mut AgentView, task_id: &str) {
        simulate_task_output_wait_ms(agent, task_id, 30_000);
    }
    /// Drive the agent's tracker into a wait-all
    /// (`WaitingReason::TasksComplete`) blocking wait via the real update
    /// path; the tracker classifies on the title alone.
    pub fn simulate_wait_all(agent: &mut AgentView) {
        use crate::acp::meta::NotificationMeta;
        use crate::acp::tracker::{TurnActivity, WaitingReason};
        use std::sync::Arc;
        agent.front_message_committed = true;
        let meta = NotificationMeta::default();
        agent.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(
                    acp::ToolCallId::new(Arc::from("waitall-1")),
                    "wait_commands_or_subagents",
                )
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .content(vec![])
                .locations(vec![]),
            ),
            &meta,
            &mut agent.scrollback,
        );
        let activity = agent.resolve_turn_activity();
        assert!(
            matches!(
                activity,
                Some(TurnActivity::Waiting(WaitingReason::TasksComplete))
            ),
            "expected TasksComplete wait, got {activity:?}"
        );
    }
    /// Drive the agent's tracker into a foreground-subagent wait via the real
    /// update path: a pending `task` tool call (no `run_in_background`)
    /// registers a blocking [`WaitingReason::Subagent`]
    /// (`crate::acp::tracker`). The shell aborts that await the moment the
    /// user sends (send-now), so it must read as a sendable/parked wait.
    pub fn simulate_subagent_wait(agent: &mut AgentView) {
        use crate::acp::meta::NotificationMeta;
        use crate::acp::tracker::{TurnActivity, WaitingReason};
        use std::sync::Arc;
        agent.front_message_committed = true;
        let meta = NotificationMeta::default();
        agent.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(Arc::from("task-tc-1")), "task")
                    .kind(acp::ToolKind::Other)
                    .status(acp::ToolCallStatus::Pending)
                    .content(vec![])
                    .locations(vec![]),
            ),
            &meta,
            &mut agent.scrollback,
        );
        let activity = agent.resolve_turn_activity();
        assert!(
            matches!(
                activity,
                Some(TurnActivity::Waiting(WaitingReason::Subagent { .. }))
            ),
            "expected Subagent wait, got {activity:?}"
        );
    }
    /// A minimal running (foreground) subagent registry row, so tests can
    /// count it in `watchers()` snapshots.
    pub fn running_subagent_info(child_sid: &str) -> crate::app::subagent::SubagentInfo {
        use std::sync::Arc;
        use std::time::Instant;
        crate::app::subagent::SubagentInfo {
            subagent_id: Arc::from(format!("sa-{child_sid}")),
            child_session_id: Arc::from(child_sid),
            description: Arc::from("test"),
            subagent_type: Arc::from("general-purpose"),
            persona: None,
            role: None,
            model: None,
            context_source: None,
            resumed_from: None,
            capability_mode: None,
            workflow_run_id: None,
            context_normalized: false,
            parent_prompt_id: None,
            started_at: Instant::now(),
            last_progress_at: Instant::now(),
            finished: false,
            status: None,
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
            transcript: Default::default(),
        }
    }
    /// Count of "Worked for X" (`TurnCompleted`) marker blocks in the
    /// agent's scrollback.
    pub fn count_turn_markers(agent: &AgentView) -> usize {
        use crate::scrollback::block::RenderBlock;
        use crate::scrollback::blocks::SessionEvent;
        (0..agent.scrollback.len())
            .filter(|i| {
                matches!(
                    agent.scrollback.get(*i).map(|e| &e.block),
                    Some(RenderBlock::SessionEvent(b))
                        if matches!(b.event, SessionEvent::TurnCompleted { .. })
                )
            })
            .count()
    }
    pub fn raw_ctrl_b_event() -> crossterm::event::Event {
        crossterm::event::Event::Key(KeyEvent::new(KeyCode::Char('\u{0002}'), KeyModifiers::NONE))
    }
    pub fn add_running_bg_task(agent: &mut AgentView) {
        agent.session.bg_tasks.insert(
            "task-1".into(),
            crate::app::agent::BgTaskState {
                task_id: "task-1".into(),
                tool_call_id: "tool-1".into(),
                command: "sleep 5".into(),
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
                is_monitor: false,
                restored_from_replay: false,
            },
        );
        agent.tasks.sync(
            &agent.session.bg_tasks,
            &agent.subagent_sessions,
            &agent.session.scheduled_tasks,
            None,
            &std::collections::HashSet::new(),
            &agent.workflow_runs,
        );
    }
    pub fn add_running_execute(agent: &mut AgentView) {
        use crate::acp::meta::NotificationMeta;
        use std::sync::Arc;
        agent.session.state = AgentState::TurnRunning;
        agent.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(Arc::from("exec-1")), "sleep 5")
                    .kind(acp::ToolKind::Execute)
                    .status(acp::ToolCallStatus::InProgress)
                    .content(vec![])
                    .locations(vec![]),
            ),
            &NotificationMeta::default(),
            &mut agent.scrollback,
        );
    }
    pub fn make_running_agent() -> AgentView {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = AgentSession {
            id: AgentId(0),
            acp_tx: tx,
            session_id: Some(acp::SessionId::new("test-session")),
            models: ModelState::default(),
            state: AgentState::TurnRunning,
            tracker: crate::acp::tracker::AcpUpdateTracker::new(),
            cwd: std::path::PathBuf::from("/tmp"),
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
        session.enqueue_prompt("local one".to_string());
        let mut agent = AgentView::new(session, ScrollbackState::new());
        agent.shared_queue = vec![QueueEntryWire {
            id: "p1".into(),
            version: 2,
            owner: None,
            last_editor: None,
            kind: "prompt".into(),
            text: "server one".into(),
            position: 0,
            combined_texts: None,
        }];
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        agent.queue.overlay.visible = true;
        agent.queue.overlay.focused = true;
        agent
    }
    /// Minimal idle agent (no queue, no session id) shared by input tests.
    pub fn make_agent() -> AgentView {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        AgentView::new(
            AgentSession {
                id: AgentId(0),
                acp_tx: tx,
                session_id: None,
                models: ModelState::default(),
                state: AgentState::Idle,
                tracker: crate::acp::tracker::AcpUpdateTracker::new(),
                cwd: std::path::PathBuf::from("/tmp"),
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
            },
            ScrollbackState::new(),
        )
    }
    /// Interject chord for non–VS Code family tests (`Ctrl+Enter`).
    pub fn force_interject_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
    }
    /// Interject chord for VS Code family tests (`Ctrl+L`).
    pub fn vscode_interject_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)
    }
    /// Host-independent registry for queue/prompt interject tests (Ctrl+Enter).
    pub fn non_vscode_registry() -> ActionRegistry {
        ActionRegistry::non_vscode_for_test()
    }
    /// Host-independent VS family registry (Ctrl+L interject, OpenExtensions Null).
    pub fn vscode_family_registry() -> ActionRegistry {
        ActionRegistry::vscode_family_for_test()
    }
    #[test]
    fn apply_follow_ups_renders_chips_for_a_response() {
        let mut agent = make_agent();
        let changed =
            agent.apply_follow_ups("resp-1".into(), vec!["Tell me more".into(), "Sum".into()]);
        assert!(changed, "first chips for a response warrant a redraw");
        let fu = agent.follow_ups.as_ref().expect("chips must be set");
        assert_eq!(fu.response_id, "resp-1");
        assert_eq!(fu.suggestions, vec!["Tell me more", "Sum"]);
    }
    #[test]
    fn apply_follow_ups_newer_response_supersedes() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        let changed = agent.apply_follow_ups("resp-2".into(), vec!["b".into()]);
        assert!(changed, "a newer response must take over");
        let fu = agent.follow_ups.as_ref().unwrap();
        assert_eq!(fu.response_id, "resp-2");
        assert_eq!(fu.suggestions, vec!["b"]);
    }
    #[test]
    fn apply_follow_ups_ignores_superseded_redelivery() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        agent.apply_follow_ups("resp-2".into(), vec!["b".into()]);
        let changed = agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        assert!(!changed, "a superseded response's re-delivery is ignored");
        let fu = agent.follow_ups.as_ref().unwrap();
        assert_eq!(fu.response_id, "resp-2");
        assert_eq!(fu.suggestions, vec!["b"]);
    }
    #[test]
    fn apply_follow_ups_same_response_is_idempotent() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        let changed = agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        assert!(!changed);
        assert_eq!(agent.follow_ups.as_ref().unwrap().suggestions, vec!["a"]);
    }
    #[test]
    fn apply_follow_ups_empty_clears_current_response_chips() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        let changed = agent.apply_follow_ups("resp-1".into(), Vec::new());
        assert!(
            changed,
            "retracting the shown response's chips warrants redraw"
        );
        assert!(agent.follow_ups.is_none());
    }
    #[test]
    fn apply_follow_ups_empty_for_different_response_supersedes_and_clears() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        let changed = agent.apply_follow_ups("resp-2".into(), Vec::new());
        assert!(changed);
        assert!(agent.follow_ups.is_none());
    }
    #[test]
    fn apply_follow_ups_empty_for_unseen_id_does_not_poison_ring() {
        let mut agent = make_agent();
        assert!(!agent.apply_follow_ups("resp-1".into(), Vec::new()));
        assert!(agent.follow_ups.is_none());
        assert!(agent.apply_follow_ups("resp-1".into(), vec!["a".into()]));
        assert_eq!(agent.follow_ups.as_ref().unwrap().suggestions, vec!["a"]);
        let mut agent = make_agent();
        agent.apply_follow_ups("shown".into(), vec!["x".into()]);
        agent.apply_follow_ups("resp-2".into(), Vec::new());
        let changed = agent.apply_follow_ups("resp-2".into(), vec!["b".into()]);
        assert!(
            changed,
            "non-empty for the previously-empty resp-2 must render"
        );
        assert_eq!(agent.follow_ups.as_ref().unwrap().suggestions, vec!["b"]);
    }
    #[test]
    fn apply_follow_ups_empty_retract_allows_same_id_redelivery() {
        let mut agent = make_agent();
        assert!(agent.apply_follow_ups("resp-1".into(), vec!["a".into()]));
        assert_eq!(agent.follow_ups.as_ref().unwrap().suggestions, vec!["a"]);
        assert!(agent.apply_follow_ups("resp-1".into(), Vec::new()));
        assert!(agent.follow_ups.is_none());
        assert!(
            !agent.follow_up_seen.contains_key("resp-1"),
            "an empty retraction of the shown chips must drop the id from the seen-ring"
        );
        let changed = agent.apply_follow_ups("resp-1".into(), vec!["b".into()]);
        assert!(
            changed,
            "non-empty re-delivery for a retracted id must render"
        );
        assert_eq!(agent.follow_ups.as_ref().unwrap().suggestions, vec!["b"]);
        assert!(agent.apply_follow_ups("resp-2".into(), vec!["c".into()]));
        let changed_old = agent.apply_follow_ups("resp-1".into(), vec!["b".into()]);
        assert!(!changed_old, "a superseded older id stays rejected");
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-2");
    }
    #[test]
    fn clear_then_superseded_redelivery_is_ignored() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        agent.clear_follow_ups();
        let changed = agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        assert!(!changed, "a cleared response's re-delivery is ignored");
        assert!(agent.follow_ups.is_none());
    }
    #[test]
    fn apply_follow_ups_old_response_rejected_after_many_newer() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-0".into(), vec!["first".into()]);
        for i in 1..=200 {
            agent.apply_follow_ups(format!("resp-{i}"), vec![format!("s{i}")]);
        }
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-200");
        let changed = agent.apply_follow_ups("resp-0".into(), vec!["first".into()]);
        assert!(!changed, "a long-superseded response stays rejected");
        let fu = agent.follow_ups.as_ref().unwrap();
        assert_eq!(fu.response_id, "resp-200");
        assert_eq!(fu.suggestions, vec!["s200"]);
    }
    #[test]
    fn apply_follow_ups_generation_is_monotonic() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        assert_eq!(agent.follow_up_seen.get("resp-1"), Some(&0));
        assert_eq!(agent.follow_up_next_gen, 1);
        agent.apply_follow_ups("resp-1".into(), vec!["a2".into()]);
        assert_eq!(agent.follow_up_next_gen, 1);
        agent.apply_follow_ups("resp-2".into(), vec!["b".into()]);
        assert_eq!(agent.follow_up_seen.get("resp-2"), Some(&1));
        assert_eq!(agent.follow_up_next_gen, 2);
        assert!(!agent.apply_follow_ups("resp-1".into(), vec!["a".into()]));
        assert_eq!(agent.follow_up_next_gen, 2);
    }
    #[test]
    fn reconnect_reload_finalize_clears_follow_up_chips() {
        let mut agent = make_agent();
        assert!(agent.apply_follow_ups("resp-1".into(), vec!["a".into(), "b".into()]));
        assert!(agent.follow_ups.is_some(), "chips shown before the reload");
        assert!(agent.follow_up_seen.contains_key("resp-1"));
        assert!(agent.follow_up_next_gen > 0);
        agent.session.prompt_history_loading = true;
        agent.begin_session_reload(1);
        assert!(agent.finish_session_reload(1, true));
        assert!(!agent.session.prompt_history_loading);
        assert!(
            agent.follow_ups.is_none(),
            "reconnect reload must clear the shown chips"
        );
        assert!(agent.follow_up_chips.is_empty());
        assert!(
            agent.follow_up_seen.is_empty(),
            "the seen map must clear so chips streamed after the reload are not suppressed"
        );
        assert_eq!(
            agent.follow_up_next_gen, 0,
            "the acceptance generation resets with the reload"
        );
    }
    /// A model switch stuck across a reconnect must not jam the drain, but a
    /// switch started DURING the reload window must keep its model-switch hold. The
    /// reload START (`begin_session_reload`) releases the hold (the disconnect
    /// dropped the in-flight RPC); finalize (`apply_reload_outcome`) must NOT —
    /// a window switch is live on the reconnected link.
    #[test]
    fn reconnect_reload_clears_stuck_model_switch_pending() {
        for success in [false, true] {
            let mut agent = make_agent();
            agent.session.model_switch_pending = true;
            agent.begin_session_reload(1);
            assert!(
                !agent.session.model_switch_pending,
                "reload start must release the hold for the lost pre-outage switch"
            );
            agent.session.model_switch_pending = true;
            assert!(agent.finish_session_reload(1, success));
            assert!(
                agent.session.model_switch_pending,
                "finalize (success={success}) must NOT clear a switch started \
                 during the reload window"
            );
        }
    }
    #[test]
    fn reconnect_reload_failure_also_clears_follow_up_chips() {
        let mut agent = make_agent();
        assert!(agent.apply_follow_ups("resp-1".into(), vec!["a".into()]));
        assert!(agent.follow_ups.is_some());
        agent.session.prompt_history_loading = true;
        agent.begin_session_reload(1);
        assert!(agent.finish_session_reload(1, false));
        assert!(!agent.session.prompt_history_loading);
        assert!(
            agent.follow_ups.is_none(),
            "a failed reconnect reload must still clear stale chips"
        );
        assert!(agent.follow_up_seen.is_empty());
        assert_eq!(agent.follow_up_next_gen, 0);
    }
    fn wf_snapshot(run_id: &str, status: &str) -> crate::views::workflows::WorkflowRunSnapshot {
        crate::views::workflows::WorkflowRunSnapshot {
            run_id: run_id.to_string(),
            name: "deep-research".to_string(),
            objective: "obj".to_string(),
            status: status.to_string(),
            management_available: true,
            builtin: false,
            phases: Vec::new(),
            current_phase: None,
            agents: Vec::new(),
            agent_budget: None,
            agents_used: 0,
            agents_reserved: 0,
            agents_remaining: None,
            agent_usage_incomplete: false,
            active_agents: 0,
            elapsed_ms: 1_000,
            received_at: std::time::Instant::now(),
            pause_message: None,
            result_summary: None,
        }
    }
    #[test]
    fn reconnect_reload_failure_restores_workflow_projection() {
        let mut agent = make_agent();
        let block =
            crate::scrollback::blocks::WorkflowBlock::started("wf-1", "deep-research", "obj");
        let block_id = agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::Workflow(block));
        agent.workflow_blocks.insert("wf-1".to_string(), block_id);
        agent.workflow_runs.push(wf_snapshot("wf-1", "active"));
        agent.workflow_run_revisions.insert("wf-1".to_string(), 4);
        agent.cleared_workflow_runs.insert("wf-old".to_string());
        agent.begin_session_reload(1);
        assert!(
            agent.workflow_runs.is_empty(),
            "staging clears the run list"
        );
        assert!(
            agent.workflow_blocks.is_empty(),
            "staging clears the block map"
        );
        assert!(agent.workflow_run_revisions.is_empty());
        assert!(agent.cleared_workflow_runs.is_empty());
        assert!(agent.finish_session_reload(1, false));
        assert_eq!(
            agent.workflow_runs.len(),
            1,
            "run list restored on failed reload"
        );
        assert_eq!(agent.workflow_runs[0].run_id, "wf-1");
        assert_eq!(
            agent.workflow_run_revisions.get("wf-1").copied(),
            Some(4),
            "revision highwater restored so a stale re-delivery is still deduped"
        );
        assert!(
            agent.cleared_workflow_runs.contains("wf-old"),
            "cleared tombstone set restored"
        );
        assert_eq!(
            agent.workflow_blocks.get("wf-1").copied(),
            Some(block_id),
            "block map restored"
        );
        assert!(
            agent.scrollback.get_by_id(block_id).is_some(),
            "the restored block map points at a block that is back in the scrollback"
        );
    }
    #[test]
    fn reconnect_reload_success_drops_stashed_workflow_projection() {
        let mut agent = make_agent();
        agent.workflow_runs.push(wf_snapshot("wf-1", "active"));
        agent.workflow_run_revisions.insert("wf-1".to_string(), 2);
        agent.cleared_workflow_runs.insert("wf-old".to_string());
        agent.begin_session_reload(1);
        agent.mark_reload_replay_seen();
        assert!(agent.finish_session_reload(1, true));
        assert!(
            agent.workflow_runs.is_empty(),
            "success keeps the rebuilt (here empty) run list, not the stash"
        );
        assert!(agent.workflow_run_revisions.is_empty());
        assert!(agent.cleared_workflow_runs.is_empty());
    }
    /// Drives the production `finalize_reload_and_maybe_adopt` that the
    /// `event_loop.rs` reconnect loop also calls (so a future reorder of the
    /// finalize-before-adopt gate fails here). A synthetic non-scheduler running
    /// id leaves the agent `Idle` (reload still finalized), while a `/loop` or
    /// user id IS adopted.
    #[test]
    fn reconnect_reload_adopts_only_for_prompt_with_completion_exit() {
        let mut synthetic = make_agent();
        synthetic.begin_session_reload(1);
        assert!(
            synthetic.finalize_reload_and_maybe_adopt(
                1,
                true,
                Some("task-completed-abc-123".into())
            ),
            "the reload must finalize even when adoption is skipped"
        );
        assert!(synthetic.session_reload.is_none());
        assert!(synthetic.session.current_prompt_id.is_none());
        assert!(
            synthetic.session.state.is_idle(),
            "a synthetic non-scheduler running id must not strand the viewer in TurnRunning"
        );
        let mut cron = make_agent();
        cron.begin_session_reload(1);
        assert!(cron.finalize_reload_and_maybe_adopt(
            1,
            true,
            Some("scheduler-fired-019e51a3-abcd-1234".into()),
        ));
        assert_eq!(
            cron.session.current_prompt_id.as_deref(),
            Some("scheduler-fired-019e51a3-abcd-1234"),
            "a /loop fire has a prompt_complete exit, so it is adopted on reconnect"
        );
        assert!(cron.session.state.is_turn_running());
        let mut user = make_agent();
        user.begin_session_reload(1);
        assert!(user.finalize_reload_and_maybe_adopt(1, true, Some("p-user".into())));
        assert_eq!(user.session.current_prompt_id.as_deref(), Some("p-user"));
        assert!(user.session.state.is_turn_running());
    }
    /// Resolving a reload window purges iff a heavy transient dropped: the
    /// stash (success + full replay), or the staged partial replay (failure /
    /// abort / supersede). The common cursor-resolve outcome reuses the stash
    /// — nothing multi-MB drops — and must NOT purge. The counter is
    /// thread-local, so parallel tests cannot interfere with the deltas.
    #[test]
    fn reload_finalize_and_abort_release_retained_memory() {
        use crate::memory_release::test_support;
        test_support::install_counting_hook();
        let mut agent = make_agent();
        agent.begin_session_reload(1);
        agent.mark_reload_replay_seen();
        let before = test_support::calls();
        assert!(agent.finish_session_reload(1, true));
        assert_eq!(
            test_support::calls(),
            before + 1,
            "full-replay finalize must purge after the reload stash drops"
        );
        let mut cursor = make_agent();
        cursor.begin_session_reload(1);
        let before = test_support::calls();
        assert!(cursor.finish_session_reload(1, true));
        assert_eq!(
            test_support::calls(),
            before,
            "cursor-resolve finalize drops nothing heavy and must not purge"
        );
        let mut stale = make_agent();
        stale.begin_session_reload(2);
        let before = test_support::calls();
        assert!(!stale.finish_session_reload(1, true));
        assert_eq!(
            test_support::calls(),
            before,
            "a superseded finalize drops nothing and must not purge"
        );
        let before = test_support::calls();
        stale.begin_session_reload(3);
        assert_eq!(
            test_support::calls(),
            before + 1,
            "supersede must purge the discarded prior staging"
        );
        let before = test_support::calls();
        stale.abort_session_reload();
        assert_eq!(
            test_support::calls(),
            before + 1,
            "abort must purge after the discarded staging drops"
        );
        let before = test_support::calls();
        stale.abort_session_reload();
        assert_eq!(
            test_support::calls(),
            before,
            "abort without an open window must not purge"
        );
    }
    #[test]
    fn apply_follow_ups_empty_clears_hit_areas() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        agent.follow_up_chips = vec![ratatui::layout::Rect::new(0, 0, 5, 1)];
        agent.apply_follow_ups("resp-1".into(), Vec::new());
        assert!(agent.follow_ups.is_none());
        assert!(
            agent.follow_up_chips.is_empty(),
            "hit areas must be cleared"
        );
    }
    #[test]
    fn clear_follow_ups_drops_chips_and_hit_areas() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
        agent.follow_up_chips = vec![ratatui::layout::Rect::new(0, 0, 5, 1)];
        agent.clear_follow_ups();
        assert!(agent.follow_ups.is_none());
        assert!(agent.follow_up_chips.is_empty());
    }
    /// FIX 4 (a): a re-delivery of the CURRENTLY-ADOPTED turn's follow_ups
    /// re-renders even after its chips were cleared by turn adoption — the
    /// stamped `promptId` matches the active `current_prompt_id`, so the
    /// (already-seen) response is re-rendered rather than rejected.
    #[test]
    fn apply_follow_ups_current_turn_redelivery_rerenders_after_clear() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p1".into());
        assert!(agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p1"), vec!["a".into()]));
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-1");
        agent.clear_follow_ups();
        assert!(agent.follow_ups.is_none());
        assert!(
            agent.follow_up_seen.contains_key("resp-1"),
            "adoption keeps the seen ring (no un-record)"
        );
        let changed =
            agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p1"), vec!["a".into()]);
        assert!(
            changed,
            "re-delivery of the adopted turn's follow_ups must re-render"
        );
        assert_eq!(agent.follow_ups.as_ref().unwrap().suggestions, vec!["a"]);
    }
    /// FIX 4 (b): after adopting a NEW turn, a buffer-replayed `echo.agent/follow_ups`
    /// for a PRIOR turn's response_id must NOT revive stale chips — its
    /// `promptId` is not the active turn and it is already in the seen ring.
    #[test]
    fn apply_follow_ups_prior_turn_replay_does_not_revive() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p1".into());
        assert!(agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p1"), vec!["a".into()]));
        agent.session.current_prompt_id = Some("p2".into());
        agent.clear_follow_ups();
        assert!(agent.follow_ups.is_none());
        let changed =
            agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p1"), vec!["a".into()]);
        assert!(
            !changed,
            "a prior turn's replay must not revive stale chips"
        );
        assert!(agent.follow_ups.is_none(), "no stale chips were revived");
        assert!(agent.apply_follow_ups_with_prompt("resp-2".into(), Some("p2"), vec!["b".into()]));
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-2");
    }
    /// FINDING B (stamped path): a LATE FIRST-TIME (never-seen) `echo.agent/follow_ups`
    /// for a PRIOR turn — arriving while a newer turn is active — must NOT
    /// render. Before the fix it slipped through the "strictly newer" branch
    /// (never recorded in `follow_up_seen`, so the seen-reject didn't catch it).
    #[test]
    fn apply_follow_ups_late_prior_turn_first_time_rejected() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p2".into());
        assert!(agent.apply_follow_ups_with_prompt("resp-2".into(), Some("p2"), vec!["b".into()]));
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-2");
        let changed =
            agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p1"), vec!["a".into()]);
        assert!(
            !changed,
            "a never-seen prior-turn follow_ups must not render over the active turn"
        );
        assert_eq!(
            agent.follow_ups.as_ref().unwrap().response_id,
            "resp-2",
            "the active turn's chips must remain untouched"
        );
        assert!(
            !agent.follow_up_seen.contains_key("resp-1"),
            "a rejected non-current first-time arrival must not poison the seen ring"
        );
    }
    /// Regression guard for the FINDING B fix: a first-time follow_ups for the
    /// CURRENTLY-ADOPTED turn (promptId == current) still renders.
    #[test]
    fn apply_follow_ups_current_turn_first_time_renders() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p1".into());
        assert!(
            agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p1"), vec!["a".into()]),
            "the active turn's first follow_ups must render"
        );
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-1");
    }
    /// Regression guard: trailing follow_ups that arrive AFTER the turn finished
    /// (`current_prompt_id` cleared to None) are NOT treated as a mismatch and
    /// still render — `None` current is not "another active turn".
    #[test]
    fn apply_follow_ups_trailing_after_turn_complete_still_renders() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = None;
        assert!(
            agent.apply_follow_ups_with_prompt("resp-1".into(), Some("p1"), vec!["a".into()]),
            "a stamped follow_ups with no active turn must still render (turn just completed)"
        );
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-1");
    }
    /// None-fallback (older shells / no promptId): with no turn identity on the
    /// notification AND a newer turn active, a late first-time arrival cannot be
    /// distinguished from the new turn's first follow_ups, so it follows the
    /// legacy newest-wins (renders). This path is not reachable for current
    /// shells (which always stamp `promptId`) or for buffer-replays (suppressed
    /// upstream by the `_meta["echo.agent/replayed"]` gate); it is pinned here so the
    /// stamped-path fix above is understood to be the deterministic guard.
    #[test]
    fn apply_follow_ups_none_prompt_first_time_follows_legacy_newest_wins() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p2".into());
        assert!(
            agent.apply_follow_ups_with_prompt("resp-x".into(), None, vec!["a".into()]),
            "a None-promptId first-time arrival follows legacy newest-wins"
        );
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-x");
    }
    /// FIX (buffer-before-adoption): a stamped `echo.agent/follow_ups` for a turn that
    /// is NOT yet current (its `session/update` adoption raced behind the ext
    /// channel) must be BUFFERED, not dropped — and then RENDER when that turn
    /// becomes current and is flushed.
    #[test]
    fn apply_follow_ups_buffered_before_adoption_flushes_on_adoption() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p1".into());
        let changed =
            agent.apply_follow_ups_with_prompt("resp-2".into(), Some("p2"), vec!["b".into()]);
        assert!(
            !changed,
            "a not-yet-current turn's follow_ups must not render immediately"
        );
        assert!(
            agent.follow_ups.is_none(),
            "nothing rendered while buffered"
        );
        assert!(
            agent.follow_up_pending.contains_key("p2"),
            "the delivery is buffered keyed by its promptId"
        );
        agent.session.current_prompt_id = Some("p2".into());
        let flushed = agent.flush_pending_follow_ups("p2");
        assert!(flushed, "adoption flushes the buffered follow_ups");
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-2");
        assert_eq!(agent.follow_ups.as_ref().unwrap().suggestions, vec!["b"]);
        assert!(
            !agent.follow_up_pending.contains_key("p2"),
            "the buffered entry is consumed on flush"
        );
        assert!(agent.follow_up_pending_order.is_empty());
    }
    /// FIX (no stale revival): a buffered entry for a `promptId` that is
    /// SUPERSEDED by a newer turn (and never becomes current) must NOT revive.
    #[test]
    fn apply_follow_ups_buffered_superseded_turn_does_not_revive() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p1".into());
        assert!(!agent.apply_follow_ups_with_prompt("resp-2".into(), Some("p2"), vec!["b".into()]));
        assert!(agent.follow_up_pending.contains_key("p2"));
        agent.session.current_prompt_id = Some("p3".into());
        assert!(
            !agent.flush_pending_follow_ups("p3"),
            "no buffered entry for the adopted turn p3"
        );
        assert!(
            agent.follow_ups.is_none(),
            "the never-adopted p2 buffer must not revive on a different adoption"
        );
        assert!(agent.apply_follow_ups_with_prompt("resp-3".into(), Some("p3"), vec!["c".into()]));
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-3");
        assert!(
            agent.follow_up_pending.contains_key("p2"),
            "p2 remains buffered-but-inert (bounded by the cap), never revived"
        );
    }
    /// The pending buffer is FIFO-bounded: an overflow evicts ONLY the oldest
    /// entry, never the whole map (so other not-yet-adopted turns survive).
    #[test]
    fn pending_follow_ups_buffer_evicts_oldest_on_overflow() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("cur".into());
        let total = super::MAX_PENDING_FOLLOW_UPS + 1;
        for i in 0..total {
            assert!(!agent.apply_follow_ups_with_prompt(
                format!("resp-{i}"),
                Some(&format!("p{i}")),
                vec!["x".into()],
            ));
        }
        assert_eq!(agent.follow_up_pending.len(), super::MAX_PENDING_FOLLOW_UPS);
        assert!(
            !agent.follow_up_pending.contains_key("p0"),
            "the OLDEST buffered entry is evicted on overflow"
        );
        assert!(
            agent.follow_up_pending.contains_key("p1"),
            "a still-buffered (non-oldest) entry survives the overflow"
        );
        agent.session.current_prompt_id = Some("p1".into());
        assert!(agent.flush_pending_follow_ups("p1"));
        assert_eq!(agent.follow_ups.as_ref().unwrap().response_id, "resp-1");
    }
    /// FIX (reload must not wipe adopted chips): follow_ups that arrive during
    /// `loading_replay` for the running turn are BUFFERED (the turn is not
    /// current yet). On `SessionLoaded` the reset must PRESERVE that buffer (drop
    /// only stale pre-reload state) so adoption flushes + renders them.
    #[test]
    fn reload_preserves_running_turn_follow_ups_and_renders_on_adoption() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p-stale".into());
        agent.session.loading_replay = true;
        assert!(!agent.apply_follow_ups_with_prompt(
            "resp-run".into(),
            Some("p-run"),
            vec!["go".into()],
        ));
        assert!(!agent.apply_follow_ups_with_prompt(
            "resp-old".into(),
            Some("p-old"),
            vec!["stale".into()],
        ));
        assert!(agent.follow_up_pending.contains_key("p-run"));
        assert!(agent.follow_up_pending.contains_key("p-old"));
        agent.reset_follow_ups_for_reload_preserving(Some("p-run"));
        assert!(
            agent.follow_up_pending.contains_key("p-run"),
            "the running turn's buffered follow_ups survive the reload reset"
        );
        assert!(
            !agent.follow_up_pending.contains_key("p-old"),
            "stale pre-reload buffers are still dropped"
        );
        agent.adopt_running_prompt("p-run".into());
        assert_eq!(
            agent.follow_ups.as_ref().unwrap().response_id,
            "resp-run",
            "the running turn's follow_ups render after adoption"
        );
    }
    /// FIX (reload must not wipe DISPLAYED chips): when the running turn's
    /// follow_ups already RENDERED during `loading_replay` (because
    /// `current_prompt_id` was unset or already equalled the running turn, so the
    /// delivery took the render path, not the buffer), the reload reset must also
    /// preserve those on-screen chips — re-buffering them so adoption re-renders
    /// them WITHOUT the server resending. A stale OTHER turn is still dropped.
    #[test]
    fn reload_preserves_running_turn_displayed_chips_and_rerenders_on_adoption() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("p-run".into());
        agent.session.loading_replay = true;
        assert!(agent.apply_follow_ups_with_prompt(
            "resp-run".into(),
            Some("p-run"),
            vec!["go".into()],
        ));
        assert_eq!(
            agent.follow_ups.as_ref().unwrap().response_id,
            "resp-run",
            "the running turn's chips are on screen (displayed, not buffered)"
        );
        assert_eq!(agent.follow_up_shown_prompt_id.as_deref(), Some("p-run"));
        assert!(
            agent.follow_up_pending.is_empty(),
            "displayed chips are NOT in the pending buffer"
        );
        assert!(!agent.apply_follow_ups_with_prompt(
            "resp-old".into(),
            Some("p-old"),
            vec!["stale".into()],
        ));
        assert!(agent.follow_up_pending.contains_key("p-old"));
        agent.reset_follow_ups_for_reload_preserving(Some("p-run"));
        assert!(
            agent.follow_ups.is_none(),
            "the reset clears the on-screen chips"
        );
        assert!(
            agent.follow_up_pending.contains_key("p-run"),
            "the running turn's DISPLAYED chips are re-buffered so adoption can restore them"
        );
        assert!(
            !agent.follow_up_pending.contains_key("p-old"),
            "the stale OTHER turn is still dropped"
        );
        agent.adopt_running_prompt("p-run".into());
        assert_eq!(
            agent.follow_ups.as_ref().unwrap().response_id,
            "resp-run",
            "the running turn's chips re-render after adoption without a server resend"
        );
    }
    /// A full reload reset (no running turn to preserve) clears the pending
    /// buffer too — the reconnect-reload finalize path.
    #[test]
    fn reset_for_reload_clears_pending_buffer() {
        let mut agent = make_agent();
        agent.session.current_prompt_id = Some("cur".into());
        assert!(!agent.apply_follow_ups_with_prompt("r".into(), Some("future"), vec!["a".into()],));
        assert!(agent.follow_up_pending.contains_key("future"));
        agent.reset_follow_ups_for_reload();
        assert!(
            agent.follow_up_pending.is_empty(),
            "a full reload reset clears the pending buffer"
        );
        assert!(agent.follow_up_pending_order.is_empty());
    }
    #[test]
    fn follow_up_chip_click_maps_to_suggestion_text() {
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["First".into(), "Second".into()]);
        let area = ratatui::layout::Rect::new(0, 0, 60, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let theme = crate::theme::Theme::current();
        let suggestions = agent.follow_ups.as_ref().unwrap().suggestions.clone();
        agent.follow_up_chips =
            crate::views::agent::render_follow_ups(area, &mut buf, &theme, &suggestions, None);
        assert_eq!(agent.follow_up_chips.len(), 2, "both chips fit");
        let r = agent.follow_up_chips[1];
        let idx = agent
            .follow_up_chip_at(r.x + 1, r.y)
            .expect("click inside a chip hits it");
        assert_eq!(idx, 1);
        assert_eq!(
            agent.follow_ups.as_ref().unwrap().suggestions[idx],
            "Second"
        );
        assert_eq!(agent.follow_up_chip_at(area.width - 1, 0), None);
    }
    /// `make_running_agent` reduced to a single focused local row: empty server
    /// mirror, no in-flight prompt. The shared setup for the pane-hide paths.
    pub fn running_agent_local_only() -> AgentView {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        agent.shared_queue.clear();
        agent.session.current_prompt_id = None;
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );
        agent.queue.overlay.visible = true;
        agent.queue.overlay.focused = true;
        agent
    }
    /// Test image record attached to queued-prompt rows in the carry tests.
    pub fn test_pasted_image() -> crate::prompt_images::PastedImage {
        crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
            data: vec![1, 2, 3],
            mime_type: "image/png".into(),
        })
    }
}
/// Build a minimal [`AgentView`] for tests with an explicit session identity, so
/// the lazy Mermaid glue (which needs a session dir) can be exercised from the
/// `mermaid_worker` test module without duplicating the large `AgentSession`
/// literal.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn test_agent_view(session_id: Option<&str>, cwd: std::path::PathBuf) -> AgentView {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AgentView::new(
        crate::app::agent::AgentSession {
            id: crate::app::agent::AgentId(0),
            acp_tx: tx,
            session_id: session_id.map(agent_client_protocol::SessionId::new),
            models: crate::acp::model_state::ModelState::default(),
            state: crate::app::agent::AgentState::Idle,
            tracker: crate::acp::tracker::AcpUpdateTracker::new(),
            cwd,
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
        },
        crate::scrollback::state::ScrollbackState::new(),
    )
}
#[cfg(test)]
mod dropdown_chrome_tests {
    use super::*;
    use ratatui::buffer::Buffer;
    /// A panel taller than the space above the prompt (wrapped rows on a
    /// short screen) must clamp, not hang past the buffer: an unclamped
    /// `top` saturates at 0 and ratatui's `Clear` panics on the overhang.
    #[test]
    fn above_anchor_clamps_to_short_screen() {
        let theme = crate::theme::Theme::current();
        let layout_cfg = crate::appearance::LayoutConfig::default();
        let area = Rect::new(0, 0, 100, 6);
        let prompt = Rect::new(0, 4, 100, 2);
        let mut buf = Buffer::empty(area);
        let chrome = render_dropdown_chrome(
            &mut buf,
            2,
            6,
            None,
            prompt,
            area,
            &layout_cfg,
            false,
            false,
            &theme,
        );
        if let Some(chrome) = chrome {
            assert!(chrome.panel.bottom() <= area.bottom());
            assert!(chrome.items.height >= 1);
            assert_eq!(chrome.items.height, chrome.panel.height - 2);
        }
        let prompt_top = Rect::new(0, 0, 100, 2);
        let chrome = render_dropdown_chrome(
            &mut buf,
            2,
            6,
            None,
            prompt_top,
            area,
            &layout_cfg,
            false,
            false,
            &theme,
        );
        assert!(chrome.is_none());
        for h in 1..=8u16 {
            for prompt_y in 0..h {
                let area = Rect::new(0, 0, 40, h);
                let mut buf = Buffer::empty(area);
                let prompt = Rect::new(0, prompt_y, 40, 1);
                let _ = render_dropdown_chrome(
                    &mut buf,
                    2,
                    6,
                    None,
                    prompt,
                    area,
                    &layout_cfg,
                    false,
                    false,
                    &theme,
                );
            }
        }
    }
}
#[cfg(test)]
mod voice_keybind_gate_tests {
    use super::*;
    /// The per-pane chord route drops `VoiceToggle` while the Voice shortcut
    /// setting is off (the event-loop intercept skips the chord in that state,
    /// so this route is what would otherwise leak it through).
    #[test]
    fn resolve_action_honors_voice_keybind_gate() {
        let prev = crate::app::voice_keybind_enabled();
        crate::app::set_voice_keybind_enabled_for_test(false);
        assert!(resolve_action(Some(ActionId::VoiceToggle)).is_none());
        crate::app::set_voice_keybind_enabled_for_test(true);
        assert!(matches!(
            resolve_action(Some(ActionId::VoiceToggle)),
            Some(InputOutcome::Action(Action::VoiceToggle))
        ));
        crate::app::set_voice_keybind_enabled_for_test(prev);
    }
}
#[cfg(test)]
mod prompt_input_mode_tests {
    use super::*;
    use crate::app::actions::Action;
    use crate::theme::Theme;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    #[test]
    fn default_is_normal() {
        assert_eq!(PromptInputMode::default(), PromptInputMode::Normal);
    }
    #[test]
    fn accent_color_returns_expected_for_each_variant() {
        let theme = Theme::current();
        assert_eq!(PromptInputMode::Normal.accent_color(&theme), None);
        assert_eq!(
            PromptInputMode::Bash.accent_color(&theme),
            Some(theme.command)
        );
        assert_eq!(
            PromptInputMode::Remember.accent_color(&theme),
            Some(theme.accent_remember)
        );
    }
    #[test]
    fn prefix_override_returns_expected_for_each_variant() {
        let theme = Theme::current();
        assert_eq!(PromptInputMode::Normal.prefix_override(&theme), None);
        assert_eq!(
            PromptInputMode::Bash.prefix_override(&theme),
            Some(("! ", theme.command))
        );
        assert_eq!(
            PromptInputMode::Remember.prefix_override(&theme),
            Some(("# ", theme.accent_remember))
        );
    }
    #[test]
    fn placeholder_override_returns_expected_for_each_variant() {
        assert_eq!(PromptInputMode::Normal.placeholder_override(false), None);
        assert_eq!(PromptInputMode::Normal.placeholder_override(true), None);
        assert_eq!(PromptInputMode::Bash.placeholder_override(false), None);
        assert_eq!(PromptInputMode::Bash.placeholder_override(true), None);
        assert_eq!(
            PromptInputMode::Remember.placeholder_override(false),
            Some("Save a memory note... (Shift+Enter for multiline)")
        );
        assert_eq!(
            PromptInputMode::Remember.placeholder_override(true),
            Some("Save a memory note... (Enter for newline, Shift+Enter to save)")
        );
    }
    #[test]
    fn prompt_info_override_returns_expected_for_each_variant() {
        assert_eq!(PromptInputMode::Normal.prompt_info_override(), None);
        assert_eq!(
            PromptInputMode::Bash.prompt_info_override(),
            Some("Run shell command")
        );
        assert_eq!(
            PromptInputMode::Remember.prompt_info_override(),
            Some("Save memory note")
        );
    }
    #[test]
    fn send_action_maps_to_correct_action_variant() {
        let t1 = "hello world".to_string();
        assert!(matches!(
            PromptInputMode::Normal.send_action(t1.clone()),
            Action::SendPrompt(t) if t == t1
        ));
        let t2 = "ls -l".to_string();
        assert!(matches!(
            PromptInputMode::Bash.send_action(t2.clone()),
            Action::SendBashCommand(t) if t == t2
        ));
        let t4 = "remember this".to_string();
        assert!(matches!(
            PromptInputMode::Remember.send_action(t4.clone()),
            Action::SendRememberNote(t) if t == t4
        ));
    }
    #[test]
    fn is_exit_key_normal_never_exits() {
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let back = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!PromptInputMode::Normal.is_exit_key(&esc));
        assert!(!PromptInputMode::Normal.is_exit_key(&back));
        assert!(!PromptInputMode::Normal.is_exit_key(&ctrl_c));
        assert!(!PromptInputMode::Normal.is_exit_key(&enter));
    }
    #[test]
    fn is_exit_key_bash_and_remember_share_full_exit_set() {
        for mode in [PromptInputMode::Bash, PromptInputMode::Remember] {
            assert!(mode.is_exit_key(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
            assert!(mode.is_exit_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
            assert!(mode.is_exit_key(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
            assert!(mode.is_exit_key(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
            assert!(mode.is_exit_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
            assert!(!mode.is_exit_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
            assert!(!mode.is_exit_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
        }
    }
}
