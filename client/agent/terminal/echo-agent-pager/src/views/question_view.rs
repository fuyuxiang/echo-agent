//! Question view state and helpers.
//!
//! When the agent calls `AskUserQuestion`, the pager takes over the prompt
//! area and shows a structured question UI. This module contains:
//!
//! - [`QuestionViewState`] — all state for the question overlay
//! - [`QuestionSelection`] — per-question selection tracking
//! - [`QuestionFocus`] — navigation vs input mode
//!
//! No rendering or input handling here — this is pure data and helpers.

use std::collections::HashSet;
use std::time::Instant;

use echo_agent_acp::AcpResult;
use echo_agent_markdown::StreamingMarkdownRenderer;
pub use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
    AskUserQuestionMode, Question, QuestionOption,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use unicode_width::UnicodeWidthStr;

use crate::input::key::RowWalk;
use crate::render::line_utils::{byte_offset_at_width, truncate_line, truncate_str};
use crate::render::wrapping::word_wrap_lines_with_joiners;
use crate::syntax::get_syntect;
use crate::theme::Theme;
use crate::theme::md_style;
use crate::views::prompt_widget::{PromptBg, PromptStyle, StashedPrompt};

/// Maximum description lines shown in the question chrome before truncation.
const DEFAULT_MAX_CHROME_DESC_LINES: u16 = 5;

/// Maximum preview lines shown in the question chrome before truncation.
const DEFAULT_MAX_CHROME_PREVIEW_LINES: u16 = 6;

/// Minimum number of option rows that must be visible before dynamic cap
/// reduction kicks in. Ensures the user always sees at least a few options.
const MIN_VISIBLE_OPTION_ROWS: u16 = 3;

fn hovered_bg(theme: &Theme) -> ratatui::style::Color {
    theme.bg_hover
}

// ── Enums ──────────────────────────────────────────────────────────────

/// Per-question selection state.
#[derive(Debug, Clone)]
pub enum QuestionSelection {
    /// Single-choice: at most one option selected.
    Single(Option<usize>),
    /// Multi-choice: zero or more options toggled on.
    Multi(HashSet<usize>),
}

/// A cursor move within one question's answer rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMotion {
    Next,
    Prev,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    First,
    Last,
}

/// Focus mode within the question view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionFocus {
    /// Cursor is on an option row or the free-form row (not typing).
    /// j/k navigate, Enter selects or enters input mode.
    Navigation,
    /// User is typing in the TextArea (input mode).
    /// @-dropdown may be open. Esc exits back to Navigation.
    InputMode,
}

/// Pager-internal origin for a locally-opened question (one that was NOT
/// driven by an ACP `echo.agent/ask_user_question` request).
///
/// Drives what `submit_question_answers` returns when the user submits.
/// Mutually exclusive with `QuestionViewState.response_tx`: a local
/// question never has an ACP sender.
///
/// Each variant carries the data the local handler needs to translate the
/// submitted selection into an [`crate::app::actions::Action`].
///
/// Not `Clone`: `FeedbackTrace` owns its attachments' staged temp files.
#[derive(Debug)]
pub enum LocalQuestionKind {
    /// Modal opened by `/fork` to resolve the worktree question.
    /// On submit, the selected option index plus the carried directive
    /// are translated into an
    /// [`crate::app::actions::Action::ForkAnswered`].
    Fork {
        /// Optional directive supplied via `/fork <directive>`. Stashed
        /// here so the modal can carry it across the synchronous return
        /// path back to `dispatch_fork_resolved` without a global mailbox.
        directive: Option<String>,
    },
    /// Modal opened by `/new` to resolve the worktree question.
    /// On submit, the selected option index is translated into an
    /// [`crate::app::actions::Action::NewSessionAnswered`].
    NewSession,
    /// Modal shown when the user hits the credit/rate limit (403).
    /// Options map to upsell URLs: upgrade tier or enable on-demand.
    /// `choices` maps each option index to a telemetry choice variant.
    CreditLimitUpsell {
        choices: Vec<echo_agent_telemetry::events::CreditLimitChoice>,
    },
    /// EchoAgent upsell modal: the free-usage paywall (429 +
    /// `subscription:free-usage-exhausted`) or a tier-restricted slash
    /// command invocation. Upgrade options carry their URL in the option
    /// `id`.
    FreeUsageUpsell {
        /// Telemetry source for `EchoAgentUpsellClicked` — distinguishes
        /// the paywall from the restricted-command upsell.
        source: echo_agent_telemetry::events::EchoAgentUpsell,
    },
    /// Modal shown when the shell rejects a model switch due to agent
    /// type incompatibility. Carries the target model + effort so the
    /// answer handler can create a new session with it.
    AgentTypeMismatch {
        model_id: agent_client_protocol::ModelId,
        effort: Option<echo_agent_runtime::sampling::types::ReasoningEffort>,
    },
    DoctorFix {
        target: crate::app::actions::DoctorFixTarget,
        plan: Box<crate::diagnostics::FixPlan>,
    },
    DeleteCurrentSession,
    /// Freeform report modal opened by `/feedback`.
    Feedback,
    /// Second stage of the `/feedback` card: trace consent. Carries the
    /// committed report (text and drained image attachments) so Esc can
    /// skip the question without dropping it.
    FeedbackTrace {
        report: String,
        images: crate::views::prompt_widget::FeedbackImages,
    },
}

/// Bare `/feedback` pane label (first paragraph of the question chrome).
pub const FEEDBACK_QUESTION_LABEL: &str = "How can we improve EchoAgent Build?";

/// Trace-consent question shown after the report is submitted (wording from
/// legal review — discloses retention/training scope, not just debugging).
pub const FEEDBACK_TRACE_QUESTION_LABEL: &str = "Opt-in to provide your trace for debugging \
     purposes. This will also provide EchoAgent the ability to retain and train on coding data, \
     e.g., prompts, traces, & metrics.";

/// Option ids for the trace-consent question; the submit handler maps ids
/// (never positions) back to a [`crate::app::actions::FeedbackTraceChoice`].
pub const FEEDBACK_TRACE_OPTION_OPT_IN: &str = "always_upload";
pub const FEEDBACK_TRACE_OPTION_OPT_OUT: &str = "no_upload";
pub const FEEDBACK_TRACE_OPTION_NEVER_ASK: &str = "never_ask";

// ── State ──────────────────────────────────────────────────────────────

/// Complete state for the question view overlay.
///
/// Created when an `echo.agent/ask_user_question` ext-method request arrives;
/// destroyed on submit, skip, or cancel.
///
/// Not `Clone` because it owns a `oneshot::Sender` for the ACP response.
#[derive(Debug)]
pub struct QuestionViewState {
    /// The tool call ID of the `AskUserQuestion` invocation.
    pub tool_call_id: String,
    /// The questions to present.
    pub questions: Vec<Question>,
    /// Which question is currently shown (0-based index).
    pub active_tab: usize,
    /// Per-question selection state (same length as `questions`).
    pub selections: Vec<QuestionSelection>,
    /// Current focus mode.
    pub focus: QuestionFocus,
    /// Whether fullscreen mode is active (removes height cap).
    pub fullscreen: bool,
    /// Original prompt state, stashed on entry and restored on exit.
    pub stashed_prompt: StashedPrompt,

    /// Cursor position per question (index into options + freeform row).
    pub per_question_cursor: Vec<usize>,
    /// Scroll offset (visual lines) per question.
    pub per_question_scroll: Vec<u16>,
    /// Per-question freeform text (additional context). Each question has
    /// its own text so switching tabs doesn't mix content.
    pub per_question_freeform: Vec<String>,
    /// Whether the per-question freeform answer is "selected" (included in
    /// submission). Toggled by Space, auto-set when exiting InputMode with
    /// text. Independent of the text content — text is preserved on untoggle.
    pub per_question_freeform_selected: Vec<bool>,

    // ── Cached chrome caps (recomputed on resize / question switch) ──
    /// Cached cap on description lines in chrome (capped in non-fullscreen).
    pub cached_desc_cap: u16,
    /// Cached cap on preview lines in chrome (capped in non-fullscreen).
    pub cached_preview_cap: u16,

    // ── ACP response channel (TS-04) ──
    /// Stashed ACP response sender. When the user submits/cancels, the
    /// pager serializes the response and sends it here. `take()` ensures
    /// we never send twice.
    pub response_tx:
        Option<tokio::sync::oneshot::Sender<AcpResult<agent_client_protocol::ExtResponse>>>,
    /// Mode context from the ext-method request. Controls whether the
    /// bottom panel (Chat about this / Skip interview) is shown.
    pub mode: AskUserQuestionMode,
    /// Bottom panel selection index (plan mode only).
    /// `None` = options list has focus, `Some(0)` = Chat about this,
    /// `Some(1)` = Skip interview.
    pub bottom_panel_index: Option<usize>,
    /// `Some` when this question was opened locally (e.g. by `/fork`)
    /// instead of by an ACP `echo.agent/ask_user_question` request. `None` for
    /// ACP questions (preserves today's behaviour).
    ///
    /// Mutually exclusive with `response_tx`: a local question never has
    /// an ACP sender.
    pub local_kind: Option<LocalQuestionKind>,
    /// When this question view was created. Used to pause the turn timer
    /// while the user is answering questions — the time spent in the
    /// question view is subtracted from the turn elapsed display.
    pub opened_at: Instant,
    /// Wall-clock twin of `opened_at` (UTC ms). `Instant` is suspend-blind,
    /// so a pause netted against the wall-anchored turn span must itself be
    /// measured on the wall clock, or a suspend during an open question
    /// would read as worked time.
    pub opened_at_wall_ms: i64,
    /// When `true`, the freeform "Other" input row is hidden. Used by
    /// locally-driven questions (e.g. credit-limit upsell) that only
    /// offer fixed options with no free-text fallback.
    pub no_freeform: bool,

    /// Whether Enter on the report advances to the trace-consent question.
    pub feedback_offer_trace: bool,
    /// Opted-out account: the "Opt in" option also switches coding-data
    /// sharing back on (and says so in its description).
    pub feedback_offer_reenables_sharing: bool,
}

// ── Constructor & basic helpers ────────────────────────────────────────

impl QuestionViewState {
    /// Create a new question view state.
    ///
    /// Initializes per-question vectors (selections, cursors, scroll)
    /// based on each question's type (single vs multi-select).
    pub fn new(
        tool_call_id: String,
        questions: Vec<Question>,
        stashed_prompt: StashedPrompt,
    ) -> Self {
        Self::with_response_tx(
            tool_call_id,
            questions,
            stashed_prompt,
            None,
            AskUserQuestionMode::Default,
        )
    }

    /// Create a new question view state with an ACP response sender.
    ///
    /// Called by the `ExtMethod` handler when a blocking `echo.agent/ask_user_question`
    /// request arrives from the shell coordinator.
    pub fn with_response_tx(
        tool_call_id: String,
        questions: Vec<Question>,
        stashed_prompt: StashedPrompt,
        response_tx: Option<
            tokio::sync::oneshot::Sender<AcpResult<agent_client_protocol::ExtResponse>>,
        >,
        mode: AskUserQuestionMode,
    ) -> Self {
        let n = questions.len();
        let selections: Vec<QuestionSelection> = questions
            .iter()
            .map(|q| {
                if q.multi_select.unwrap_or(false) {
                    QuestionSelection::Multi(HashSet::new())
                } else {
                    QuestionSelection::Single(None)
                }
            })
            .collect();

        Self {
            tool_call_id,
            questions,
            active_tab: 0,
            selections,
            focus: QuestionFocus::Navigation,
            fullscreen: false,
            stashed_prompt,
            per_question_cursor: vec![0; n],
            per_question_scroll: vec![0; n],
            per_question_freeform: vec![String::new(); n],
            per_question_freeform_selected: vec![false; n],
            cached_desc_cap: DEFAULT_MAX_CHROME_DESC_LINES,
            cached_preview_cap: DEFAULT_MAX_CHROME_PREVIEW_LINES,
            response_tx,
            mode,
            bottom_panel_index: None,
            local_kind: None,
            opened_at: Instant::now(),
            opened_at_wall_ms: chrono::Utc::now().timestamp_millis(),
            no_freeform: false,
            feedback_offer_trace: false,
            feedback_offer_reenables_sharing: false,
        }
    }

    /// Builder-style helper to attach a [`LocalQuestionKind`].
    ///
    /// Used by `open_fork_question` to mark a freshly-built
    /// `QuestionViewState` as locally-driven so submit/cancel routes
    /// through the synchronous `Action` path instead of the ACP
    /// `response_tx`. Returns `self` so the call site can chain it on
    /// the constructor.
    pub fn with_local_kind(mut self, kind: LocalQuestionKind) -> Self {
        self.local_kind = Some(kind);
        self
    }

    /// Builder-style helper to hide the freeform "Other" input row.
    pub fn with_no_freeform(mut self) -> Self {
        self.no_freeform = true;
        self
    }

    /// Number of items for a given question: options + 1 free-form row
    /// (unless `no_freeform` is set).
    pub fn total_items(&self, question_idx: usize) -> usize {
        let freeform = if self.no_freeform { 0 } else { 1 };
        self.questions
            .get(question_idx)
            .map(|q| q.options.len() + freeform)
            .unwrap_or(1)
    }

    /// Whether the cursor is on the free-form row for the active question.
    pub fn is_on_freeform_row(&self) -> bool {
        if self.no_freeform {
            return false;
        }
        let idx = self.active_tab;
        self.cursor()
            == self
                .questions
                .get(idx)
                .map(|q| q.options.len())
                .unwrap_or(0)
    }

    /// Current cursor position for the active question.
    pub fn cursor(&self) -> usize {
        self.per_question_cursor
            .get(self.active_tab)
            .copied()
            .unwrap_or(0)
    }

    /// Move the cursor within the active question, clamped at both ends.
    pub fn move_cursor(&mut self, motion: CursorMotion) {
        let last = self.total_items(self.active_tab).saturating_sub(1);
        let cursor = self.cursor();
        let target = match motion {
            CursorMotion::Next => cursor + 1,
            CursorMotion::Prev => cursor.saturating_sub(1),
            CursorMotion::HalfPageDown => cursor + (last / 2).max(1),
            CursorMotion::HalfPageUp => cursor.saturating_sub((last.max(1) / 2).max(1)),
            CursorMotion::PageDown => cursor + last.max(1),
            CursorMotion::PageUp => cursor.saturating_sub(last.max(1)),
            CursorMotion::First => 0,
            CursorMotion::Last => last,
        };
        self.set_cursor(target.min(last));
    }

    /// Walk one answer row of the active question, wrapping at both ends.
    pub fn walk_cursor(&mut self, walk: RowWalk) {
        let target = walk.step(self.cursor(), self.total_items(self.active_tab));
        self.set_cursor(target);
    }

    pub fn clear_selection(&mut self, q_idx: usize) {
        match self.selections.get_mut(q_idx) {
            Some(QuestionSelection::Multi(selected)) => selected.clear(),
            Some(QuestionSelection::Single(selected)) => *selected = None,
            None => {}
        }
        if let Some(freeform_selected) = self.per_question_freeform_selected.get_mut(q_idx) {
            *freeform_selected = false;
        }
    }

    /// Set cursor position for the active question, clamped to valid range.
    pub fn set_cursor(&mut self, pos: usize) {
        let max = self.total_items(self.active_tab).saturating_sub(1);
        let clamped = pos.min(max);
        if let Some(c) = self.per_question_cursor.get_mut(self.active_tab) {
            *c = clamped;
        }
    }

    /// Adjust scroll so the cursor row is visible within `visible_h` lines.
    ///
    /// Call this after every cursor change. `content_w` is needed to compute
    /// per-option visual heights for stacked layout.
    pub fn ensure_cursor_visible(&mut self, visible_h: u16, content_w: usize) {
        let q_idx = self.active_tab;
        let Some(question) = self.questions.get(q_idx) else {
            return;
        };

        let cursor = self.cursor();
        let heights = option_heights(question, content_w, cursor);
        let scroll = self.per_question_scroll.get(q_idx).copied().unwrap_or(0);

        // Compute the visual Y range of the cursor item.
        let cursor_top: u16 = heights[..cursor].iter().sum();
        let cursor_bottom = cursor_top + heights.get(cursor).copied().unwrap_or(1);

        let mut new_scroll = scroll;

        // If cursor is above the visible window, scroll up.
        if cursor_top < new_scroll {
            new_scroll = cursor_top;
        }

        // If cursor is below the visible window, scroll down.
        if cursor_bottom > new_scroll + visible_h {
            new_scroll = cursor_bottom.saturating_sub(visible_h);
        }

        if let Some(s) = self.per_question_scroll.get_mut(q_idx) {
            *s = new_scroll;
        }
    }

    /// Clamp the active question's scroll offset to the current viewport.
    pub fn clamp_scroll(&mut self, visible_h: u16, content_w: usize) {
        let q_idx = self.active_tab;
        let Some(question) = self.questions.get(q_idx) else {
            return;
        };

        let max_scroll = total_options_height(question, content_w, self.cursor())
            .saturating_sub(self.phantom_freeform_h())
            .saturating_sub(visible_h);
        if let Some(s) = self.per_question_scroll.get_mut(q_idx) {
            *s = (*s).min(max_scroll);
        }
    }

    /// Height of the freeform line that [`option_heights`] /
    /// [`total_options_height`] always include but which is never rendered
    /// when `no_freeform` is set. Subtract this from those totals wherever
    /// they feed layout or scroll limits.
    pub fn phantom_freeform_h(&self) -> u16 {
        if self.no_freeform { 1 } else { 0 }
    }
}

/// Visual heights for each item in a question: all options, then freeform.
pub fn option_heights(question: &Question, content_w: usize, cursor: usize) -> Vec<u16> {
    let prefix_w = option_prefix_w(question);
    let max_lw = compute_max_label_w(&question.options, content_w);

    question
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| option_visual_height(o, content_w, prefix_w, max_lw, i == cursor))
        .chain(std::iter::once(1u16))
        .collect()
}

/// Total visual height of all option rows plus the freeform row.
pub fn total_options_height(question: &Question, content_w: usize, cursor: usize) -> u16 {
    option_heights(question, content_w, cursor)
        .into_iter()
        .sum()
}

/// Map a visual line offset within the scrolled options list to an item index.
pub fn item_index_at_visual_line(
    question: &Question,
    content_w: usize,
    visual_line: u16,
    cursor: usize,
) -> usize {
    let heights = option_heights(question, content_w, cursor);
    let mut top = 0u16;
    for (idx, height) in heights.iter().copied().enumerate() {
        if visual_line < top + height {
            return idx;
        }
        top += height;
    }
    heights.len().saturating_sub(1)
}

pub fn item_top_offset(
    question: &Question,
    content_w: usize,
    item_index: usize,
    cursor: usize,
) -> u16 {
    let heights = option_heights(question, content_w, cursor);
    let clamped = item_index.min(heights.len());
    heights[..clamped].iter().sum()
}

pub fn scroll_offset_for_item_delta(
    question: &Question,
    content_w: usize,
    current_scroll: u16,
    delta: i32,
    viewport_height: u16,
    cursor: usize,
    phantom_freeform_h: u16,
) -> u16 {
    // Line-based scrolling: add delta directly to the scroll offset,
    // clamped to [0, max_scroll].  Each visual line (including wrapped
    // description lines) is independent, so we scroll by individual lines
    // instead of jumping whole items.
    //
    // `phantom_freeform_h` (see [`QuestionViewState::phantom_freeform_h`])
    // removes the never-rendered freeform line from the scrollable total
    // for `no_freeform` questions.
    let max_scroll = total_options_height(question, content_w, cursor)
        .saturating_sub(phantom_freeform_h)
        .saturating_sub(viewport_height);
    ((current_scroll as i32) + delta).clamp(0, max_scroll as i32) as u16
}

/// Visible option rows height within a rendered question area.
pub fn visible_options_height(
    question: &Question,
    area_height: u16,
    content_w: usize,
    preview: Option<&str>,
    fullscreen: bool,
    desc_cap: u16,
    preview_cap: u16,
) -> u16 {
    area_height.saturating_sub(chrome_height(
        question,
        content_w,
        preview,
        fullscreen,
        desc_cap,
        preview_cap,
    ))
}

/// Maximum scroll offset for the option rows in a rendered question area.
#[allow(clippy::too_many_arguments)]
pub fn max_scroll_offset(
    question: &Question,
    area_height: u16,
    content_w: usize,
    preview: Option<&str>,
    fullscreen: bool,
    desc_cap: u16,
    preview_cap: u16,
    cursor: usize,
) -> u16 {
    total_options_height(question, content_w, cursor).saturating_sub(visible_options_height(
        question,
        area_height,
        content_w,
        preview,
        fullscreen,
        desc_cap,
        preview_cap,
    ))
}

/// Resolve the item index for a screen row within the rendered question area.
#[allow(clippy::too_many_arguments)]
pub fn item_index_at_screen_row(
    question: &Question,
    area: Rect,
    content_w: usize,
    scroll: u16,
    row: u16,
    preview: Option<&str>,
    fullscreen: bool,
    desc_cap: u16,
    preview_cap: u16,
    cursor: usize,
) -> Option<usize> {
    let options_start_y = area.y
        + chrome_height(
            question,
            content_w,
            preview,
            fullscreen,
            desc_cap,
            preview_cap,
        );
    let options_end_y = area.y + area.height;
    if row < options_start_y || row >= options_end_y {
        return None;
    }

    let visual_line = (row - options_start_y) + scroll;
    Some(item_index_at_visual_line(
        question,
        content_w,
        visual_line,
        cursor,
    ))
}

// ── Layout helpers ─────────────────────────────────────────────────────

/// Compute the aligned label column width.
///
/// The column fits the longest label, capped at 60% of the available
/// width so labels are always visible while the collapsed description
/// (with its `…` affordance) keeps the remaining space. Labels longer
/// than the cap are truncated with `…` on unfocused rows and get
/// stacked/wrapped layout when focused.
pub fn compute_max_label_w(options: &[QuestionOption], content_w: usize) -> usize {
    let cap = content_w * 3 / 5;
    options
        .iter()
        .map(|o| normalize_label(&o.label).width())
        .max()
        .unwrap_or(0)
        .min(cap)
}

/// Visual height of a single option row.
///
/// - Unfocused: always 1 line (collapsed `label  description…`).
/// - Focused: full description. The label shares the first description line when
///   it fits the column (description wrapped in the column to its right); an
///   overflowing label wraps full-width with the description stacked below it,
///   both indented at `prefix_w`.
pub fn option_visual_height(
    option: &QuestionOption,
    content_w: usize,
    prefix_w: usize,
    max_label_w: usize,
    focused: bool,
) -> u16 {
    if !focused {
        return 1;
    }
    let gap = 2;
    let norm_label = normalize_label(&option.label);
    if norm_label.width() > max_label_w {
        let wide_w = content_w.saturating_sub(prefix_w).max(1);
        let label_lines = wrap_label_chunks(&norm_label, wide_w).len() as u16;
        let desc_lines = rendered_option_description_lines(option, wide_w).len() as u16;
        (label_lines + desc_lines).max(1)
    } else {
        let indent = prefix_w + max_label_w + gap;
        let desc_w = content_w.saturating_sub(indent).max(1);
        let desc_lines = rendered_option_description_lines(option, desc_w).len() as u16;
        desc_lines.max(1)
    }
}

/// Inner chrome-height computation with explicit description/preview caps.
///
/// Same logic as [`chrome_height`] but accepts caps as parameters instead of
/// branching on `fullscreen`. Used by the dynamic-cap fallback in
/// [`question_view_height`].
fn chrome_height_with_dynamic_caps(
    question: &Question,
    content_w: usize,
    preview: Option<&str>,
    desc_cap: u16,
    preview_cap: u16,
) -> u16 {
    let wrap_w = content_w.max(1);
    let (label, desc) = split_question_label_desc(&question.question);
    let raw_line = Line::from(vec![Span::raw(label.to_string())]);
    let label_lines = crate::render::wrapping::word_wrap_line(&raw_line, wrap_w)
        .len()
        .max(1) as u16;
    let desc_lines = if desc.is_empty() {
        0u16
    } else {
        rendered_option_description_lines(
            &QuestionOption {
                label: String::new(),
                description: desc.to_string(),
                preview: None,
                id: None,
            },
            wrap_w,
        )
        .len() as u16
    };
    let preview_lines: u16 = match preview {
        Some(p) if !p.is_empty() => p
            .lines()
            .map(|l| {
                let raw = Line::from(vec![Span::raw(l.to_string())]);
                crate::render::wrapping::word_wrap_line(&raw, wrap_w)
                    .len()
                    .max(1) as u16
            })
            .sum(),
        _ => 0,
    };

    let desc_lines = desc_lines.min(desc_cap);
    let preview_lines = preview_lines.min(preview_cap);

    let preview_gap = if preview_lines > 0 { 1 } else { 0 };
    let label_gap = if label_gap_suppressed(question) { 0 } else { 1 };

    // vpad(1) + label + label_gap(1) + description (if any)
    //   + [preview_gap(1) + preview_lines if preview exists] + gap(1)
    1 + label_lines + label_gap + desc_lines + preview_gap + preview_lines + 1
}

/// A card with no description and no options has nothing under its label, so the blank line meant to separate them would leave it unevenly padded.
/// [`chrome_height_with_dynamic_caps`] and [`render_question_chrome`] must agree on it.
fn label_gap_suppressed(question: &Question) -> bool {
    let (_, desc) = split_question_label_desc(&question.question);
    desc.is_empty() && question.options.is_empty()
}

/// Chrome height for a question: vpad + label lines + gap + [description lines] + gap.
///
/// The label (first paragraph of the question text) word-wraps across
/// multiple lines. If the question contains a paragraph break (`\n\n`),
/// the remaining text is rendered as a description below the label.
/// Must match `render_question_chrome`.
///
/// When `fullscreen` is false, description and preview lines are capped to
/// `desc_cap` / `preview_cap` respectively (with room for a truncation
/// indicator). When `fullscreen` is true the caps are ignored and all
/// lines are counted.
pub fn chrome_height(
    question: &Question,
    content_w: usize,
    preview: Option<&str>,
    fullscreen: bool,
    desc_cap: u16,
    preview_cap: u16,
) -> u16 {
    if fullscreen {
        chrome_height_with_dynamic_caps(question, content_w, preview, u16::MAX, u16::MAX)
    } else {
        chrome_height_with_dynamic_caps(question, content_w, preview, desc_cap, preview_cap)
    }
}

/// Split question text into a label (first paragraph) and description (rest).
///
/// A paragraph break is `\n\n`. If no break exists, the full text is the
/// label and the description is empty.
fn split_question_label_desc(text: &str) -> (&str, &str) {
    if let Some(pos) = text.find("\n\n") {
        (text[..pos].trim(), text[pos + 2..].trim())
    } else {
        (text.trim(), "")
    }
}

// ── Selection helpers ──────────────────────────────────────────────────

impl QuestionViewState {
    /// Toggle an option for a question.
    ///
    /// - Multi: toggle in/out of the HashSet.
    /// - Single: set to `Some(option_idx)`, or `None` if already selected
    ///   (deselect).
    pub fn toggle_option(&mut self, question_idx: usize, option_idx: usize) {
        let Some(sel) = self.selections.get_mut(question_idx) else {
            return;
        };
        match sel {
            QuestionSelection::Multi(set) => {
                if !set.remove(&option_idx) {
                    set.insert(option_idx);
                }
            }
            QuestionSelection::Single(current) => {
                if *current == Some(option_idx) {
                    *current = None;
                } else {
                    *current = Some(option_idx);
                }
            }
        }
    }

    /// Select an option (no toggle — always selects).
    ///
    /// - Single: set to `Some(option_idx)`.
    /// - Multi: add to set.
    pub fn select_option(&mut self, question_idx: usize, option_idx: usize) {
        let Some(sel) = self.selections.get_mut(question_idx) else {
            return;
        };
        match sel {
            QuestionSelection::Multi(set) => {
                set.insert(option_idx);
            }
            QuestionSelection::Single(current) => {
                *current = Some(option_idx);
            }
        }
    }

    /// Activate freeform input for the active question.
    ///
    /// Marks the freeform row as selected, clears the option selection
    /// (single-select exclusivity), sets focus to `InputMode`, and returns
    /// the current freeform text so the caller can load it into the prompt.
    ///
    /// No-op returning an empty string when `no_freeform` is set: such
    /// questions (e.g. the EchoAgent upsell) have no freeform row, so
    /// `InputMode` must be unreachable. Callers gate on `no_freeform` /
    /// [`Self::is_on_freeform_row`] too; this is defense in depth.
    pub fn activate_freeform_input(&mut self) -> String {
        if self.no_freeform {
            return String::new();
        }
        let idx = self.active_tab;
        if let Some(sel) = self.per_question_freeform_selected.get_mut(idx) {
            *sel = true;
        }
        if let Some(QuestionSelection::Single(sel)) = self.selections.get_mut(idx) {
            *sel = None;
        }
        self.focus = QuestionFocus::InputMode;
        self.per_question_freeform
            .get(idx)
            .cloned()
            .unwrap_or_default()
    }

    /// Preview text for the currently focused option, if any.
    ///
    /// Returns `Some(preview)` when the cursor is on an option (not freeform)
    /// and that option has a `preview` field set.
    pub fn focused_preview(&self) -> Option<&str> {
        let q = self.questions.get(self.active_tab)?;
        let cursor = self.cursor();
        let option = q.options.get(cursor)?;
        option.preview.as_deref()
    }

    /// Either stage of the `/feedback` card (report or trace consent).
    pub fn is_feedback(&self) -> bool {
        matches!(
            self.local_kind,
            Some(LocalQuestionKind::Feedback | LocalQuestionKind::FeedbackTrace { .. })
        )
    }

    /// The freeform report stage of the `/feedback` card.
    pub fn is_feedback_report(&self) -> bool {
        matches!(self.local_kind, Some(LocalQuestionKind::Feedback))
    }

    /// The trace-consent stage of the `/feedback` card.
    pub fn is_feedback_trace(&self) -> bool {
        matches!(
            self.local_kind,
            Some(LocalQuestionKind::FeedbackTrace { .. })
        )
    }

    pub fn feedback_report(&self) -> String {
        self.per_question_freeform
            .first()
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Swap the report card for the trace-consent question, keeping the
    /// stashed prompt. Built through the constructor so the per-question
    /// vector-length invariant lives in exactly one place.
    pub fn begin_feedback_trace_stage(
        &mut self,
        report: String,
        images: Vec<crate::prompt_images::PastedImage>,
    ) {
        // "Opt in" is a persistent grant, so its description names what it
        // turns on beyond this one upload.
        let opt_in_description = if self.feedback_offer_reenables_sharing {
            "Turns on trace upload for future sessions on this machine and switches coding \
             data sharing back on for this account."
        } else {
            "Turns on trace upload for future sessions on this machine (change any time with \
             [telemetry] trace_upload in config.toml)."
        };
        let question = Question {
            question: FEEDBACK_TRACE_QUESTION_LABEL.to_string(),
            options: vec![
                QuestionOption {
                    label: "Opt in".into(),
                    description: opt_in_description.into(),
                    preview: None,
                    id: Some(FEEDBACK_TRACE_OPTION_OPT_IN.into()),
                },
                QuestionOption {
                    label: "Opt out this time".into(),
                    description: String::new(),
                    preview: None,
                    id: Some(FEEDBACK_TRACE_OPTION_OPT_OUT.into()),
                },
                QuestionOption {
                    label: "Opt out and don't ask again".into(),
                    description: String::new(),
                    preview: None,
                    id: Some(FEEDBACK_TRACE_OPTION_NEVER_ASK.into()),
                },
            ],
            multi_select: Some(false),
            id: None,
        };
        let mut next = QuestionViewState::new(
            std::mem::take(&mut self.tool_call_id),
            vec![question],
            std::mem::take(&mut self.stashed_prompt),
        );
        next.selections = vec![QuestionSelection::Single(Some(0))];
        next.no_freeform = true;
        next.fullscreen = self.fullscreen;
        // Card-open time spans both stages (pause accounting).
        next.opened_at = self.opened_at;
        next.opened_at_wall_ms = self.opened_at_wall_ms;
        next.feedback_offer_trace = self.feedback_offer_trace;
        next.feedback_offer_reenables_sharing = self.feedback_offer_reenables_sharing;
        next.local_kind = Some(LocalQuestionKind::FeedbackTrace {
            report,
            images: images.into(),
        });
        *self = next;
    }

    /// Labels of the selected options for a given question.
    pub fn selected_labels(&self, question_idx: usize) -> Vec<String> {
        let Some(sel) = self.selections.get(question_idx) else {
            return Vec::new();
        };
        let Some(q) = self.questions.get(question_idx) else {
            return Vec::new();
        };
        match sel {
            QuestionSelection::Multi(set) => {
                let mut indices: Vec<_> = set.iter().copied().collect();
                indices.sort_unstable();
                indices
                    .into_iter()
                    .filter_map(|i| q.options.get(i).map(|o| o.label.clone()))
                    .collect()
            }
            QuestionSelection::Single(Some(idx)) => q
                .options
                .get(*idx)
                .map(|o| vec![o.label.clone()])
                .unwrap_or_default(),
            QuestionSelection::Single(None) => Vec::new(),
        }
    }

    /// True when the active tab has any option selected, or its free-form
    /// answer marked selected.
    pub fn active_tab_has_selection(&self) -> bool {
        let q_idx = self.active_tab;
        let option_selected = !self.selected_labels(q_idx).is_empty();
        let freeform_selected = self
            .per_question_freeform_selected
            .get(q_idx)
            .copied()
            .unwrap_or(false);
        option_selected || freeform_selected
    }
}

// ── ACP response builders (TS-05) ─────────────────────────────────────

impl QuestionViewState {
    /// Build the `Accepted` ext-method response from the current state.
    ///
    /// Rules:
    /// - Only answered questions appear in `answers` (unanswered omitted).
    /// - Multi-select: labels joined with `, `.
    /// - Freeform-only (no option, only typed text): label = `"Other"`,
    ///   typed text in `annotations[q].notes`.
    /// - Preview included for single-select only, verbatim from the option.
    /// - Notes included when freeform text is non-empty and selected.
    pub fn build_accepted_response(
        &self,
    ) -> echo_agent_tools::implementations::echo_agent_build::ask_user_question::AskUserQuestionExtResponse
    {
        use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
            AskUserQuestionExtResponse, QuestionAnnotation,
        };
        use indexmap::IndexMap;
        use std::collections::HashMap;

        let mut answers = IndexMap::new();
        let mut annotations: HashMap<String, QuestionAnnotation> = HashMap::new();

        for (i, q) in self.questions.iter().enumerate() {
            let labels = self.selected_labels(i);
            let freeform_selected = self
                .per_question_freeform_selected
                .get(i)
                .copied()
                .unwrap_or(false);
            let freeform_text = self
                .per_question_freeform
                .get(i)
                .cloned()
                .unwrap_or_default();
            let has_freeform = freeform_selected && !freeform_text.trim().is_empty();

            if labels.is_empty() && !has_freeform {
                // Unanswered — omit from answers.
                continue;
            }

            // Build the per-question label vec: one element for
            // single-select, multiple for multi-select, or `["Other"]` when
            // only the freeform input was used. The wire format carries
            // these as separate elements so downstream cursor-shape
            // resolvers do not have to re-split a comma-joined string.
            let label_vec: Vec<String> = if labels.is_empty() && has_freeform {
                vec!["Other".to_string()]
            } else {
                labels
            };

            answers.insert(q.question.clone(), label_vec);

            // Build annotation if there's preview or notes.
            let is_single = !q.multi_select.unwrap_or(false);
            let preview = if is_single {
                // Preview from selected option (single-select only).
                match &self.selections[i] {
                    QuestionSelection::Single(Some(idx)) => {
                        q.options.get(*idx).and_then(|o| o.preview.clone())
                    }
                    _ => None,
                }
            } else {
                None
            };

            let notes = if has_freeform {
                Some(freeform_text)
            } else {
                None
            };

            if preview.is_some() || notes.is_some() {
                annotations.insert(q.question.clone(), QuestionAnnotation { preview, notes });
            }
        }

        let annotations = if annotations.is_empty() {
            None
        } else {
            Some(annotations)
        };

        AskUserQuestionExtResponse::Accepted {
            answers,
            annotations,
        }
    }

    /// Send the ACP ext-method response and return `true` if the response
    /// was actually sent (i.e. `response_tx` was present).
    ///
    /// After sending, `response_tx` is consumed (set to `None`) to prevent
    /// double-send.
    pub fn send_ext_response(
        &mut self,
        response: echo_agent_tools::implementations::echo_agent_build::ask_user_question::AskUserQuestionExtResponse,
    ) -> bool {
        let Some(tx) = self.response_tx.take() else {
            return false;
        };
        let raw = serde_json::value::to_raw_value(&response)
            .expect("AskUserQuestionExtResponse serialization should not fail");
        tx.send(Ok(agent_client_protocol::ExtResponse::new(raw.into())))
            .ok();
        true
    }
}

// ── Tab cycling ────────────────────────────────────────────────────────

impl QuestionViewState {
    /// Advance to the next question (clamped, no wrap).
    pub fn next_question(&mut self) {
        if self.active_tab + 1 < self.questions.len() {
            self.active_tab += 1;
        }
    }

    /// Go to the previous question (clamped, no wrap).
    pub fn prev_question(&mut self) {
        self.active_tab = self.active_tab.saturating_sub(1);
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

/// Desired height for the question view overlay.
///
/// Height cap: 33% of `screen_h`, clamped to min 8, max 80%.
/// Fullscreen mode removes the cap.
///
/// When the chrome (label + description + preview) would leave fewer than
/// [`MIN_VISIBLE_OPTION_ROWS`] visible option rows, the description and
/// preview caps are dynamically reduced so at least that many rows remain
/// when the terminal is large enough for the fixed chrome overhead. On
/// extremely small terminals this is best-effort — the guarantee may not
/// hold when even zero desc/preview lines cannot free enough space.
/// The effective caps are written to `state.cached_desc_cap` /
/// `state.cached_preview_cap` so the renderer uses matching values.
pub fn question_view_height(state: &mut QuestionViewState, screen_h: u16, content_w: usize) -> u16 {
    let q_idx = state.active_tab;
    let Some(question) = state.questions.get(q_idx) else {
        return 0;
    };

    // `total_options_height` unconditionally counts a 1-line freeform row;
    // when `no_freeform` is set that row is never rendered, so subtract it
    // from the totals below — otherwise the panel keeps a clickable dead
    // row under the last option.
    let phantom_freeform = state.phantom_freeform_h();
    let freeform_h: u16 = 1 - phantom_freeform;
    let min_options_space = MIN_VISIBLE_OPTION_ROWS + freeform_h;

    if state.fullscreen {
        // desc_cap/preview_cap are ignored when fullscreen=true (chrome_height
        // routes to u16::MAX internally), but pass MAX for clarity.
        let chrome_h = chrome_height(
            question,
            content_w,
            state.focused_preview(),
            true,
            u16::MAX,
            u16::MAX,
        );
        let total = chrome_h
            + total_options_height(question, content_w, state.cursor())
                .saturating_sub(phantom_freeform);
        state.cached_desc_cap = u16::MAX;
        state.cached_preview_cap = u16::MAX;
        return total.min(screen_h);
    }

    let cap = (screen_h as u32 * 33 / 100)
        .max(8)
        .min(screen_h as u32 * 80 / 100) as u16;

    let mut effective_desc_cap = DEFAULT_MAX_CHROME_DESC_LINES;
    let mut effective_preview_cap = DEFAULT_MAX_CHROME_PREVIEW_LINES;

    let mut chrome_h = chrome_height_with_dynamic_caps(
        question,
        content_w,
        state.focused_preview(),
        effective_desc_cap,
        effective_preview_cap,
    );

    if chrome_h + min_options_space > cap {
        // Compute fixed overhead (vpad + label + gaps).
        let (label, desc) = split_question_label_desc(&question.question);
        let raw_line = Line::from(vec![Span::raw(label.to_string())]);
        let label_lines = crate::render::wrapping::word_wrap_line(&raw_line, content_w.max(1))
            .len()
            .max(1) as u16;
        let fixed_overhead = 1 + label_lines + 1 + 1; // vpad + label + blank + bottom gap

        // Compute actual description line count so unused desc budget can
        // be reallocated to preview instead of being wasted.
        let actual_desc_lines = if desc.is_empty() {
            0u16
        } else {
            rendered_option_description_lines(
                &QuestionOption {
                    label: String::new(),
                    description: desc.to_string(),
                    preview: None,
                    id: None,
                },
                content_w.max(1),
            )
            .len() as u16
        };

        let content_budget = cap
            .saturating_sub(fixed_overhead)
            .saturating_sub(min_options_space);
        effective_desc_cap = content_budget
            .min(DEFAULT_MAX_CHROME_DESC_LINES)
            .min(actual_desc_lines);
        let remaining = content_budget.saturating_sub(effective_desc_cap);
        // Reserve 1 row for the preview gap (blank separator) when preview
        // text exists and there is any remaining budget for preview lines.
        let preview_gap_allowance = if state.focused_preview().is_some() && remaining > 0 {
            1u16
        } else {
            0
        };
        effective_preview_cap = remaining
            .saturating_sub(preview_gap_allowance)
            .min(DEFAULT_MAX_CHROME_PREVIEW_LINES);

        // Recompute chrome with reduced caps.
        chrome_h = chrome_height_with_dynamic_caps(
            question,
            content_w,
            state.focused_preview(),
            effective_desc_cap,
            effective_preview_cap,
        );
    }

    state.cached_desc_cap = effective_desc_cap;
    state.cached_preview_cap = effective_preview_cap;

    let total = chrome_h
        + total_options_height(question, content_w, state.cursor())
            .saturating_sub(phantom_freeform);
    total.min(cap)
}

/// Shortcut label for an option index: 1-9 then a-z.
///
/// Returns `'1'`..`'9'` for indices 0..8, `'a'`..`'z'` for 9..34.
/// Returns `None` for indices ≥ 35.
pub fn option_shortcut_label(idx: usize) -> Option<char> {
    match idx {
        0..=8 => Some((b'1' + idx as u8) as char),
        9..=34 => Some((b'a' + (idx - 9) as u8) as char),
        _ => None,
    }
}

/// Map a pressed key character to an option index.
///
/// `'1'`..`'9'` → 0..8, `'a'`..`'f'` → 9..14.
/// Only a-f are mapped as shortcuts to avoid conflicts with navigation keys
/// (g=top, h=prev-question, j=down, k=up, l=next-question, n=next, s=skip).
pub fn option_index_for_key(c: char) -> Option<usize> {
    match c {
        '1'..='9' => Some((c as usize) - ('1' as usize)),
        'a'..='f' => Some(9 + (c as usize) - ('a' as usize)),
        _ => None,
    }
}

/// Horizontal padding: 3 left (accent + pad) + 2 right (scrollbar gutter).
pub const QUESTION_VIEW_HPAD: u16 = 5;

/// Prefix width for option rows.
///
/// The shortcut column is always 1 character wide (1-9, a-z), followed by a
/// space, then the marker/radio/checkbox, then a space:
///   Multi:  `X [✓] ` = 1 + 1 + 3 + 1 = 6
///   Single: `X (●) ` = 1 + 1 + 3 + 1 = 6
pub fn option_prefix_w(_question: &Question) -> usize {
    6 // both multi and single use 3-char markers now
}

/// Report area of the bare `/feedback` card: a multi-line box standing in for the option rows, shared by the full TUI and minimal renderers.
/// `draw` needs a blank [`crate::views::prompt_widget::PromptInfo`] to put the bottom rule in place.
pub mod feedback_input {
    use super::{PromptBg, PromptStyle, QUESTION_VIEW_HPAD, Theme};

    /// Rows at rest: top rule, five text rows, bottom rule. The box grows with the report up to the caller's cap.
    pub const HEIGHT: u16 = 7;

    /// Rows of that height spent on the outline rather than text.
    pub const CHROME_H: u16 = 2;

    /// Smallest box that can still carry its outline: the two rules plus one row of text. Below this the renderers drop to [`flat_style`].
    pub const MIN_HEIGHT: u16 = CHROME_H + 1;

    /// Shown while the box is empty, including while it has focus.
    pub const PLACEHOLDER: &str = "Please provide as much detail as possible.";

    /// The card's content column, so the box lines up under the label.
    pub fn width(area_width: u16) -> u16 {
        area_width.saturating_sub(QUESTION_VIEW_HPAD)
    }

    pub fn style(theme: &Theme) -> PromptStyle {
        PromptStyle {
            // Sits on the card, so it takes the card's surface rather than the composer's, and pads symmetrically inside its own rules.
            bg: PromptBg::Panel(theme.bg_light),
            chrome_pad_right: 2,
            placeholder_when_focused: true,
            placeholder_override: Some(PLACEHOLDER),
            ..PromptStyle::default()
        }
    }

    /// Unoutlined variant for a panel too short to spare the two rows the rules cost.
    pub fn flat_style(theme: &Theme) -> PromptStyle {
        PromptStyle {
            vpad_top: 0,
            chrome: false,
            show_borders: false,
            ..style(theme)
        }
    }
}

/// Width available for inline prompt text given the full area width.
///
/// Subtracts left padding (accent col + 2 = 3), the option prefix
/// (`"z [x] "` = 6 chars), and the prompt indicator (`"❯ "` = 2 chars).
/// Matches the `text_w` computed during rendering so `desired_height`
/// wraps at the same width as the draw area.
pub fn inline_text_width(area_width: u16) -> u16 {
    const LEFT_PAD: u16 = 3; // accent column + 2 padding
    const OPTION_PREFIX_W: u16 = 6; // shortcut + marker ("z [x] ")
    const PROMPT_INDICATOR_W: u16 = 2; // "❯ "
    area_width.saturating_sub(LEFT_PAD + OPTION_PREFIX_W + PROMPT_INDICATOR_W)
}

/// Normalize a label for single-line display: replace newlines with spaces.
pub(crate) fn normalize_label(label: &str) -> String {
    label.replace('\n', " ").replace("  ", " ")
}

fn rendered_option_label(option: &QuestionOption, max_label_w: usize) -> String {
    if max_label_w == 0 {
        String::new()
    } else {
        truncate_str(&normalize_label(&option.label), max_label_w)
    }
}

fn rendered_option_description_lines(option: &QuestionOption, width: usize) -> Vec<Line<'static>> {
    if option.description.trim().is_empty() {
        return Vec::new();
    }

    let mut renderer = StreamingMarkdownRenderer::new(md_style::style(), true);
    renderer.push(&option.description);
    // finish() (not render()) so the LaTeX-delimiter normalizer flushes any
    // trailing held-back bytes for this complete, one-shot description.
    renderer.finish(Some(get_syntect()));
    let view = renderer.view();
    let lines_owned: Vec<Line<'static>> = view
        .lines
        .iter()
        .map(crate::render::line_utils::line_to_static)
        .collect();
    let (wrapped, _) = word_wrap_lines_with_joiners(lines_owned, width.max(1));

    let mut compact = Vec::new();
    let mut prev_blank = true;
    for line in wrapped {
        let is_blank = line.spans.iter().all(|s| s.content.trim().is_empty());
        if is_blank {
            if prev_blank {
                continue;
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;
        compact.push(line);
    }
    compact
}

fn styled_description_lines(
    option: &QuestionOption,
    width: usize,
    row_bg: ratatui::style::Color,
    desc_fg: ratatui::style::Color,
) -> Vec<Line<'static>> {
    rendered_option_description_lines(option, width)
        .into_iter()
        .map(|mut line| {
            line.style = line.style.patch(Style::default().fg(desc_fg).bg(row_bg));
            for span in &mut line.spans {
                span.style = span.style.patch(Style::default().fg(desc_fg).bg(row_bg));
            }
            line
        })
        .collect()
}

/// Build a flat list of styled lines for all option rows and the freeform row.
///
/// Each visual line — including wrapped description continuation lines — is a
/// separate `Line<'static>`.  The caller can render a scrolled window by
/// simply slicing `[scroll .. scroll + visible_h]`, which makes scrolling
/// smooth and line-granular.
#[allow(clippy::too_many_arguments)]
pub fn build_flat_option_lines(
    question: &Question,
    content_w: usize,
    cursor: usize,
    hovered: Option<usize>,
    selections: &QuestionSelection,
    theme: &Theme,
    show_freeform: bool,
    freeform_text: &str,
    freeform_selected: bool,
    panel_focused: bool,
) -> Vec<Line<'static>> {
    let prefix_w = option_prefix_w(question);
    let max_lw = compute_max_label_w(&question.options, content_w);
    let is_multi = question.multi_select.unwrap_or(false);

    let mut all_lines = Vec::new();

    let hover_bg = hovered_bg(theme);

    for (i, option) in question.options.iter().enumerate() {
        let is_cursor_item = i == cursor;
        let is_hovered_item = hovered == Some(i);
        let is_selected = match selections {
            QuestionSelection::Multi(set) => set.contains(&i),
            QuestionSelection::Single(sel) => *sel == Some(i),
        };
        let embed =
            crate::views::modal_window::embedded_row_style(theme, is_cursor_item && panel_focused);
        // Full TUI: focused (keyboard cursor) → distinct selection bg, but
        // only when the panel itself owns focus. When unfocused, drop the
        // cursor-row bg so it reads as "no active selection".
        // Hovered (mouse) → subtle blend.
        // Normal → dark bg.
        let row_bg = match embed {
            Some(e) => e.bg,
            None if is_cursor_item && panel_focused => theme.bg_visual,
            None if is_hovered_item => hover_bg,
            None => theme.bg_light,
        };

        build_single_option_lines(
            &mut all_lines,
            i,
            option,
            is_multi,
            is_selected,
            max_lw,
            prefix_w,
            content_w,
            row_bg,
            embed,
            theme,
            is_cursor_item,
        );
    }

    // Freeform row — hidden in InputMode (prompt widget below replaces it).
    if show_freeform {
        let freeform_idx = question.options.len();
        all_lines.push(build_freeform_line(
            freeform_idx == cursor,
            hovered == Some(freeform_idx),
            freeform_text,
            freeform_selected,
            is_multi,
            theme,
            panel_focused,
        ));
    }

    all_lines
}

/// Build an indented description continuation line.
fn build_indented_desc_line(
    indent: usize,
    desc_line: &Line<'static>,
    row_bg: ratatui::style::Color,
) -> Line<'static> {
    Line::from(
        std::iter::once(Span::styled(
            " ".repeat(indent),
            Style::default().bg(row_bg),
        ))
        .chain(desc_line.spans.iter().cloned())
        .collect::<Vec<_>>(),
    )
    .style(Style::default().bg(row_bg))
}

/// A collapsed description: one visual line that still shows a trailing `…`
/// affordance whenever content is hidden.
fn collapsed_description_spans(
    option: &QuestionOption,
    width: usize,
    row_bg: ratatui::style::Color,
    desc_fg: ratatui::style::Color,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let lines = styled_description_lines(option, width, row_bg, desc_fg);
    let Some(first) = lines.first().cloned() else {
        return Vec::new();
    };
    let has_more = lines.len() > 1;
    let first_w: usize = first.spans.iter().map(|s| s.content.width()).sum();
    if first_w <= width && !has_more {
        return first.spans;
    }
    let budget = if first_w > width {
        width
    } else {
        width.saturating_sub(1)
    };
    let mut spans = truncate_line(first, budget).spans;
    let ends_ellipsis = spans
        .last()
        .is_some_and(|s| s.content.ends_with('\u{2026}'));
    if !ends_ellipsis {
        spans.push(Span::styled(
            "\u{2026}",
            Style::default().fg(desc_fg).bg(row_bg),
        ));
    }
    spans
}

/// Word-wrap an overflowing label into chunks of at most `width` columns.
fn wrap_label_chunks(label: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut remaining = label;
    while !remaining.is_empty() {
        if remaining.width() <= width {
            out.push(remaining.to_string());
            break;
        }
        let byte_end = byte_offset_at_width(remaining, width);
        let break_at = remaining[..byte_end]
            .rfind(' ')
            .map(|i| i + 1)
            .unwrap_or(byte_end);
        let break_at = if break_at == 0 {
            remaining
                .char_indices()
                .nth(1)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len())
        } else {
            break_at
        };
        out.push(remaining[..break_at].to_string());
        remaining = remaining[break_at..].trim_start();
    }
    out
}

/// Build the visual lines for a single option and append them to `out`.
#[allow(clippy::too_many_arguments)]
fn build_single_option_lines(
    out: &mut Vec<Line<'static>>,
    idx: usize,
    option: &QuestionOption,
    is_multi: bool,
    is_selected: bool,
    max_label_w: usize,
    prefix_w: usize,
    content_w: usize,
    row_bg: ratatui::style::Color,
    embed: Option<crate::views::modal_window::EmbeddedRowStyle>,
    theme: &Theme,
    focused: bool,
) {
    let fg = |normal| embed.map_or(normal, |e| e.fg(normal));
    let shortcut_ch = option_shortcut_label(idx).unwrap_or(' ');
    let num_str = format!("{shortcut_ch}");
    let num_style = Style::default().fg(fg(theme.accent_user)).bg(row_bg);
    let label_style = Style::default()
        .fg(fg(theme.text_primary))
        .bg(row_bg)
        .add_modifier(if focused {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

    // Build prefix spans (number + marker/checkbox)
    let prefix_spans: Vec<Span<'static>> = if is_multi {
        let (checkbox, cb_style) = if is_selected {
            (
                "[x]".to_string(),
                Style::default()
                    .fg(fg(theme.text_primary))
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "[ ]".to_string(),
                Style::default().fg(fg(theme.gray)).bg(row_bg),
            )
        };
        vec![
            Span::styled(format!("{num_str} "), num_style),
            Span::styled(format!("{checkbox} "), cb_style),
        ]
    } else {
        // Single-select: radio buttons (●) / (○)
        let (radio, radio_style) = if is_selected {
            (
                format!("({})", crate::glyphs::filled_dot()), // (●) → (•) on legacy ConHost
                Style::default()
                    .fg(fg(theme.text_primary))
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "(\u{25cb})".to_string(), // (○)
                Style::default().fg(fg(theme.gray)).bg(row_bg),
            )
        };
        vec![
            Span::styled(format!("{num_str} "), num_style),
            Span::styled(format!("{radio} "), radio_style),
        ]
    };

    let gap = 2usize;
    let indent = prefix_w + max_label_w + gap;
    let desc_w = content_w.saturating_sub(indent).max(1);

    if !focused {
        let mut spans = prefix_spans;
        let label = rendered_option_label(option, max_label_w);
        let padded_label = format!("{label:<width$}", width = max_label_w);
        spans.push(Span::styled(padded_label, label_style));
        let desc_spans = collapsed_description_spans(option, desc_w, row_bg, fg(theme.gray));
        if !desc_spans.is_empty() {
            spans.push(Span::styled(" ".repeat(gap), Style::default().bg(row_bg)));
            spans.extend(desc_spans);
        }
        out.push(Line::from(spans).style(Style::default().bg(row_bg)));
        return;
    }

    let norm_label = normalize_label(&option.label);
    if norm_label.width() > max_label_w {
        let wide_w = content_w.saturating_sub(prefix_w).max(1);
        let chunks = wrap_label_chunks(&norm_label, wide_w);
        for (li, chunk) in chunks.into_iter().enumerate() {
            if li == 0 {
                let mut spans = prefix_spans.clone();
                spans.push(Span::styled(chunk, label_style));
                out.push(Line::from(spans).style(Style::default().bg(row_bg)));
            } else {
                let spans = vec![
                    Span::styled(" ".repeat(prefix_w), Style::default().bg(row_bg)),
                    Span::styled(chunk, label_style),
                ];
                out.push(Line::from(spans).style(Style::default().bg(row_bg)));
            }
        }
        for line in styled_description_lines(option, wide_w, row_bg, fg(theme.gray)) {
            out.push(build_indented_desc_line(prefix_w, &line, row_bg));
        }
    } else {
        let label = rendered_option_label(option, max_label_w);
        let padded_label = format!("{label:<width$}", width = max_label_w);
        let mut label_spans = prefix_spans;
        label_spans.push(Span::styled(padded_label, label_style));
        let desc_lines = styled_description_lines(option, desc_w, row_bg, fg(theme.gray));
        if let Some(first_desc) = desc_lines.first()
            && !first_desc.spans.is_empty()
        {
            label_spans.push(Span::styled(" ".repeat(gap), Style::default().bg(row_bg)));
            label_spans.extend(first_desc.spans.iter().cloned());
        }
        out.push(Line::from(label_spans).style(Style::default().bg(row_bg)));
        for line in desc_lines.iter().skip(1) {
            out.push(build_indented_desc_line(indent, line, row_bg));
        }
    }
}

/// Build the freeform row line.
///
/// `prefix_w` is the total prefix width used by option rows (number + marker),
/// so the freeform row aligns with the option labels.
/// `freeform_text` is the per-question freeform text — when non-empty the row
/// shows as ticked with a preview of the answer.
fn build_freeform_line(
    is_cursor: bool,
    is_hovered: bool,
    freeform_text: &str,
    is_selected: bool,
    is_multi: bool,
    theme: &Theme,
    panel_focused: bool,
) -> Line<'static> {
    // Whitespace-only freeform is treated as empty — never shown as selected.
    let is_selected = is_selected && !freeform_text.trim().is_empty();

    let embed = crate::views::modal_window::embedded_row_style(theme, is_cursor && panel_focused);
    let fg = |normal| embed.map_or(normal, |e| e.fg(normal));
    let row_bg = match embed {
        Some(e) => e.bg,
        None if is_cursor && panel_focused => theme.bg_visual,
        None if is_hovered => hovered_bg(theme),
        None => theme.bg_light,
    };

    // Multi-select: [x]/[ ] checkboxes.  Single-select: (●)/(○) radio buttons.
    // Both are 3 display cells — same as option rows.
    let marker: String = if is_multi {
        (if is_selected { "[x]" } else { "[ ]" }).to_string()
    } else if is_selected {
        format!("({})", crate::glyphs::filled_dot())
    } else {
        "(\u{25cb})".to_string()
    };
    let _marker_display_w: usize = 3;
    let marker_style = if is_selected {
        Style::default()
            .fg(fg(theme.text_primary))
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else if is_cursor {
        Style::default().fg(fg(theme.accent_user)).bg(row_bg)
    } else {
        Style::default().fg(fg(theme.gray)).bg(row_bg)
    };
    // Stable shortcut "z" — always 1 character, matching option labels.
    let num_str = "z".to_string();
    let num_style = Style::default().fg(fg(theme.accent_user)).bg(row_bg);
    let marker_with_space = format!("{marker} ");

    let has_text = !freeform_text.trim().is_empty();
    let prompt_indicator = Style::default().fg(fg(theme.accent_user)).bg(row_bg);
    let (label, label_style) = if is_selected && has_text {
        // Show a truncated preview of the typed answer.
        let first_line = freeform_text.lines().next().unwrap_or("");
        let preview = truncate_str(first_line, 50);
        (
            preview,
            Style::default().fg(fg(theme.text_primary)).bg(row_bg),
        )
    } else if has_text {
        // Has text but not selected — show dimmed preview.
        let first_line = freeform_text.lines().next().unwrap_or("");
        let preview = truncate_str(first_line, 50);
        (preview, Style::default().fg(fg(theme.gray)).bg(row_bg))
    } else {
        // Empty — show placeholder.
        (
            "Type your answer here".to_string(),
            Style::default().fg(fg(theme.gray)).bg(row_bg),
        )
    };

    let mut spans = vec![
        Span::styled(format!("{num_str} "), num_style),
        Span::styled(marker_with_space, marker_style),
    ];
    // Show ❯ prompt indicator only when there's text (not on placeholder).
    if has_text {
        spans.push(Span::styled(
            crate::glyphs::prompt_arrow(),
            prompt_indicator,
        ));
    }
    spans.push(Span::styled(label, label_style));

    Line::from(spans).style(Style::default().bg(row_bg))
}

/// Render the complete question view into the given area.
///
/// `area` is the region above the textarea allocated for the question chrome +
/// option rows. The accent `┃` line and background are rendered here.
/// Return value from [`render_question_view`] with layout info for mouse handling.
pub struct QuestionViewRenderResult {
    /// Y coordinate where the scrollable options area starts (after chrome header).
    pub options_start_y: u16,
    /// Y coordinate where the scrollable options area ends (before freeform/inline prompt).
    pub options_end_y: u16,
}

pub fn render_question_view(
    buf: &mut Buffer,
    area: Rect,
    state: &QuestionViewState,
    hovered_item: Option<usize>,
    theme: &Theme,
    focused: bool,
) -> QuestionViewRenderResult {
    if area.height == 0 || area.width == 0 {
        return QuestionViewRenderResult {
            options_start_y: area.y,
            options_end_y: area.y,
        };
    }

    let q_idx = state.active_tab;
    let Some(question) = state.questions.get(q_idx) else {
        return QuestionViewRenderResult {
            options_start_y: area.y,
            options_end_y: area.y,
        };
    };

    let content_w = area.width.saturating_sub(QUESTION_VIEW_HPAD) as usize;

    // Fill background — same as the focused prompt (bg_light).
    let bg = Style::default().bg(theme.bg_light);
    buf.set_style(area, bg);

    // Accent line ┃ on the left column — blue to match the shortcut key color
    let accent_style = Style::default().fg(theme.accent_user);
    for row in area.y..area.y + area.height {
        if let Some(cell) = buf.cell_mut((area.x, row)) {
            cell.set_symbol(crate::glyphs::accent_bar()); // ┃ → │ on legacy ConHost
            cell.set_style(accent_style);
        }
    }

    // Content area (left: accent + 2-char pad, right: 2-char pad for scrollbar)
    let content_x = area.x + 3;
    let content_width = area.width.saturating_sub(QUESTION_VIEW_HPAD);
    let mut y = area.y;

    // Vertical padding at the top.
    y += 1;

    // ── Question chrome (label + counter + description) ──
    // Clip to the panel bottom: when the accounted height disagrees with the
    // rendered height (wrap-width drift, stale caps), the chrome must degrade
    // to truncation instead of writing past the area — set_line past the
    // buffer bottom aborts the TUI.
    y = render_question_chrome(
        buf,
        content_x,
        y,
        content_width,
        area.y + area.height,
        state,
        question,
        theme,
        state.fullscreen,
        state.cached_desc_cap,
        state.cached_preview_cap,
    );

    // ── Gap ──
    y += 1;

    let options_start_y = y;

    // ── Option rows (scrollable) + sticky freeform row ──
    let visible_bottom = area.y + area.height;
    let scroll = state.per_question_scroll.get(q_idx).copied().unwrap_or(0) as usize;
    let cursor = state.cursor();
    let is_input_mode = state.focus == QuestionFocus::InputMode;

    let freeform_text = state
        .per_question_freeform
        .get(q_idx)
        .map(|s| s.as_str())
        .unwrap_or("");
    let freeform_selected = state
        .per_question_freeform_selected
        .get(q_idx)
        .copied()
        .unwrap_or(false);

    // Freeform row is always rendered sticky at the bottom (not in the
    // scrollable list), unless in InputMode where the inline prompt replaces it.
    // When `no_freeform` is set the row is hidden entirely.
    let sticky_freeform = !is_input_mode && !state.no_freeform;
    let freeform_h: u16 = if sticky_freeform { 1 } else { 0 };

    // Build option lines WITHOUT the freeform row (it's sticky or inline).
    let all_lines = build_flat_option_lines(
        question,
        content_w,
        cursor,
        hovered_item,
        &state.selections[q_idx],
        theme,
        false, // never in scroll list
        freeform_text,
        freeform_selected,
        focused,
    );

    let visible_h = visible_bottom.saturating_sub(y).saturating_sub(freeform_h) as usize;
    for line in all_lines.iter().skip(scroll).take(visible_h) {
        if y >= visible_bottom.saturating_sub(freeform_h) {
            break;
        }
        let row_rect = Rect {
            x: content_x,
            y,
            width: content_width,
            height: 1,
        };
        buf.set_style(row_rect, line.style);
        buf.set_line(content_x, y, line, content_width);
        y += 1;
    }

    // ── Sticky freeform row at the bottom ──
    if sticky_freeform {
        let freeform_y = visible_bottom.saturating_sub(1);
        if freeform_y >= y {
            let freeform_idx = question.options.len();
            let is_multi = question.multi_select.unwrap_or(false);
            let _prefix_w = option_prefix_w(question);
            let freeform_line = build_freeform_line(
                freeform_idx == cursor,
                hovered_item == Some(freeform_idx),
                freeform_text,
                freeform_selected,
                is_multi,
                theme,
                focused,
            );
            let row_rect = Rect {
                x: content_x,
                y: freeform_y,
                width: content_width,
                height: 1,
            };
            buf.set_style(row_rect, freeform_line.style);
            buf.set_line(content_x, freeform_y, &freeform_line, content_width);
        }
    }

    // Unfocus dim: when this overlay is rendered while the user has
    // navigated to the scrollback (or any other pane), blend foregrounds
    // toward `bg_light` so the panel visually recedes. Mirrors the
    // unfocused prompt widget pattern (`prompt_widget.rs:1948`).
    if !focused {
        crate::render::color::blend_area(buf, area, Some((theme.bg_light, 0.66)), None);
    }

    let options_end_y = visible_bottom.saturating_sub(freeform_h);
    QuestionViewRenderResult {
        options_start_y,
        options_end_y,
    }
}

/// Render the question view scrollbar. Call this AFTER `render_prompt_chrome`.
///
/// `scrollbar_x` is the column to render the scrollbar in — should be
/// outside the selection box border (same column as the scrollback scrollbar).
/// Returns the scrollbar track rect if one was rendered (for mouse hit-testing).
pub fn render_question_scrollbar(
    buf: &mut Buffer,
    scrollbar_x: u16,
    state: &QuestionViewState,
    theme: &Theme,
    scroll_region: (u16, u16),
) -> Option<Rect> {
    let q_idx = state.active_tab;
    let question = state.questions.get(q_idx)?;

    let (scroll_top, scroll_bottom) = scroll_region;
    let visible_options_h = scroll_bottom.saturating_sub(scroll_top);
    // Content width for height computation — use a reasonable estimate.
    let total_option_h = {
        let cw = buf.area.width.saturating_sub(QUESTION_VIEW_HPAD) as usize;
        total_options_height(question, cw, state.cursor())
            .saturating_sub(state.phantom_freeform_h())
    };

    let scroll = state.per_question_scroll.get(q_idx).copied().unwrap_or(0);

    if total_option_h > visible_options_h && visible_options_h > 0 {
        let scrollbar_area = Rect {
            x: scrollbar_x,
            y: scroll_top,
            width: 1,
            height: visible_options_h,
        };
        // Use visible colors against bg_light: dim track, bright thumb.
        let track_style = Style::default().fg(theme.gray_dim).bg(theme.bg_light);
        let thumb_style = Style::default().fg(theme.gray).bg(theme.bg_light);
        crate::render::scrollbar::render_scrollbar_styled(
            buf,
            Some(scrollbar_area),
            total_option_h,
            visible_options_h,
            scroll,
            track_style,
            thumb_style,
        );
        return Some(scrollbar_area);
    }
    None
}

/// Render a truncation indicator line: `... Ctrl-F to expand`.
fn render_truncation_indicator(buf: &mut Buffer, x: u16, y: u16, width: u16, theme: &Theme) {
    let style = Style::default().fg(theme.gray).bg(theme.bg_light);
    let indicator = Line::from(vec![
        Span::styled("... ", style),
        Span::styled(
            "Ctrl-F",
            Style::default().fg(theme.accent_user).bg(theme.bg_light),
        ),
        Span::styled(" to expand", style),
    ]);
    buf.set_line(x, y, &indicator, width);
}

/// Render question chrome: label line + description.
///
/// Returns the Y position after the rendered chrome.
///
/// All writes are clipped to `max_y` (exclusive): the accounted chrome height
/// (`chrome_height`) and the rendered height can drift (e.g. wrap-width
/// differences), and an unclipped `set_line` below the buffer bottom panics
/// inside ratatui. Clipping degrades to truncation instead.
#[allow(clippy::too_many_arguments)]
fn render_question_chrome(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    max_y: u16,
    state: &QuestionViewState,
    question: &Question,
    theme: &Theme,
    fullscreen: bool,
    desc_cap: u16,
    preview_cap: u16,
) -> u16 {
    let mut cur_y = y;
    let w = width as usize;
    // Never write below the panel or the buffer (belt and braces: the area
    // itself should already be inside the buffer, but a mis-sized area must
    // degrade to truncation, not an abort).
    let max_y = max_y.min(buf.area.bottom());

    // Split into label (first paragraph) and description (rest).
    let (label_text, desc_text) = split_question_label_desc(&question.question);

    // ── Label (bold, primary text, word-wrapped) ──
    let label_style = Style::default()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD);

    let raw_line = Line::from(vec![Span::styled(label_text.to_string(), label_style)]);
    let wrapped = crate::render::wrapping::word_wrap_line(&raw_line, w);
    for line in &wrapped {
        if cur_y >= max_y {
            return cur_y;
        }
        buf.set_line(x, cur_y, line, width);
        cur_y += 1;
    }

    // Blank line separating the label from what follows it.
    if !label_gap_suppressed(question) {
        cur_y += 1;
    }

    // ── Description (dimmed, markdown-rendered) ──
    if !desc_text.is_empty() {
        let desc_lines = styled_description_lines(
            &QuestionOption {
                label: String::new(),
                description: desc_text.to_string(),
                preview: None,
                id: None,
            },
            w,
            theme.bg_light,
            theme.gray,
        );
        let raw_desc_count = desc_lines.len() as u16;
        let is_truncated = !fullscreen && raw_desc_count > desc_cap;
        for (desc_rendered, line) in desc_lines.into_iter().enumerate() {
            let desc_rendered = desc_rendered as u16;
            if !fullscreen && desc_rendered >= desc_cap {
                break;
            }
            if cur_y >= max_y {
                return cur_y;
            }
            // Always render the real content line first. When truncated and
            // there is room for both content and an indicator (cap >= 2),
            // append the indicator after the second-to-last real line and
            // break. When cap == 1 we show the single content line without
            // an indicator — there is no room for both.
            buf.set_line(x, cur_y, &line, width);
            cur_y += 1;
            if is_truncated && desc_cap >= 2 && desc_rendered == desc_cap.saturating_sub(2) {
                if cur_y >= max_y {
                    return cur_y;
                }
                render_truncation_indicator(buf, x, cur_y, width, theme);
                cur_y += 1;
                break;
            }
        }
    }

    // ── Preview for focused option (dimmed, word-wrapped) ──
    if let Some(preview_text) = state.focused_preview()
        && !preview_text.is_empty()
    {
        let preview_style = Style::default().fg(theme.gray).bg(theme.bg_light);

        // Count total preview lines first to determine truncation.
        // Uses Span::raw to match chrome_height (style doesn't affect wrapping).
        let mut total_preview_count = 0u16;
        for text_line in preview_text.lines() {
            let raw = Line::from(vec![Span::raw(text_line.to_string())]);
            total_preview_count += crate::render::wrapping::word_wrap_line(&raw, w)
                .len()
                .max(1) as u16;
        }

        // Cap to match chrome_height accounting.
        let capped_count = if fullscreen {
            total_preview_count
        } else {
            total_preview_count.min(preview_cap)
        };

        // Only emit the preview gap if there will be visible preview lines.
        if capped_count > 0 {
            cur_y += 1;
        }

        let is_truncated = !fullscreen && total_preview_count > preview_cap;
        let mut preview_rendered = 0u16;
        'preview_done: for text_line in preview_text.lines() {
            let raw = Line::from(vec![Span::styled(text_line.to_string(), preview_style)]);
            let wrapped = crate::render::wrapping::word_wrap_line(&raw, w);
            for line in &wrapped {
                if !fullscreen && preview_rendered >= preview_cap {
                    break 'preview_done;
                }
                if cur_y >= max_y {
                    return cur_y;
                }
                // Always render the real content line first. Append the
                // truncation indicator only when cap >= 2 so at least one
                // real preview line is visible above it.
                buf.set_line(x, cur_y, line, width);
                cur_y += 1;
                preview_rendered += 1;
                if is_truncated
                    && preview_cap >= 2
                    && preview_rendered == preview_cap.saturating_sub(1)
                {
                    if cur_y >= max_y {
                        return cur_y;
                    }
                    render_truncation_indicator(buf, x, cur_y, width, theme);
                    cur_y += 1;
                    break 'preview_done;
                }
            }
        }
    }

    cur_y
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic long multi-line `ask_user_question` payload for layout
    /// regression tests (wide wrap + multi-line option previews). Content is
    /// fictional and not from a real session.
    fn gb3747_question() -> Question {
        Question {
            question: "When renaming the shared helper module used by both the \
                       CLI and the desktop client, which compatibility approach \
                       should we take for the public config keys?"
                .to_string(),
            options: vec![
                QuestionOption {
                    label: "Keep old keys as aliases for one release".to_string(),
                    description: "Ship dual-read of the previous key names, log a \
                                  deprecation notice once per session, and remove \
                                  the aliases in the following minor version."
                        .to_string(),
                    preview: Some(
                        "config.legacy_keys: dual-read for one release\n\
                         deprecation notice: once per session\n\
                         remove aliases: next minor\n\
                         tests: load both old and new keys"
                            .to_string(),
                    ),
                    id: None,
                },
                QuestionOption {
                    label: "Break now with a clear migration note".to_string(),
                    description: "Drop the old keys immediately, document the rename \
                                  in the changelog, and print a one-line hint when an \
                                  unknown legacy key is present."
                        .to_string(),
                    preview: Some(
                        "config.legacy_keys: removed\n\
                         changelog: document rename\n\
                         unknown legacy key: one-line hint\n\
                         tests: reject old keys"
                            .to_string(),
                    ),
                    id: None,
                },
            ],
            multi_select: None,
            id: None,
        }
    }

    /// Regression: `draw()` used to size the question panel by
    /// wrapping at the full inner width while the renderer wraps at
    /// `width - QUESTION_VIEW_HPAD`. The under-allocated panel let the
    /// unclipped chrome walk past the buffer bottom and abort in ratatui
    /// (`index outside of buffer: ... but index is (5, H)`).
    ///
    /// Recreates that exact under-allocation (height computed at `w`,
    /// render at `w - HPAD`) across terminal sizes: rendering must clip,
    /// never panic. Fails on pre-fix code at e.g. 31x14 with `(5, 14)`.
    #[test]
    fn gb3747_regression_mis_sized_area_never_panics() {
        let theme = Theme::default();
        for w in 20u16..=140 {
            for h in 10u16..=60 {
                let mut state = QuestionViewState::new(
                    "tc".into(),
                    vec![gb3747_question()],
                    StashedPrompt::default(),
                );
                let inner_width = w.saturating_sub(4); // hpad_left 2 + hpad_right 2
                // Pre-fix draw() bug: full inner width (no HPAD subtraction).
                let qv_h = question_view_height(&mut state, h, inner_width as usize);
                let question_footer_h: u16 = 3;
                let reserved = 1 + 5 + 1 + 3; // draw()'s overcommit clamp
                let prompt_height = (qv_h + question_footer_h)
                    .max(3)
                    .min(h.saturating_sub(reserved));
                // The prompt slot sits directly above the shortcuts row.
                let question_area = Rect {
                    x: 2,
                    y: h.saturating_sub(1).saturating_sub(prompt_height),
                    width: inner_width,
                    height: prompt_height.saturating_sub(question_footer_h),
                };
                let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
                let _ = render_question_view(&mut buf, question_area, &state, None, &theme, true);
            }
        }
    }

    /// Companion to the mis-sized-area regression: with the *fixed* accounting
    /// (heights computed at the same `content_w` the renderer wraps at),
    /// the chrome must fit the allocation exactly — rendering into a
    /// generous buffer, the rendered chrome height equals `chrome_height`.
    #[test]
    fn gb3747_chrome_accounting_matches_render() {
        let theme = Theme::default();
        for content_w in [20usize, 35, 60, 90, 120] {
            let mut state = QuestionViewState::new(
                "tc".into(),
                vec![gb3747_question()],
                StashedPrompt::default(),
            );
            // Fixed convention: accounting at the render wrap width.
            let _ = question_view_height(&mut state, 200, content_w);
            let question = &state.questions[0];
            let expected_chrome = chrome_height(
                question,
                content_w,
                state.focused_preview(),
                false,
                state.cached_desc_cap,
                state.cached_preview_cap,
            );

            let area_w = content_w as u16 + QUESTION_VIEW_HPAD;
            let area = Rect::new(0, 0, area_w, 200);
            let mut buf = Buffer::empty(area);
            let result = render_question_view(&mut buf, area, &state, None, &theme, true);
            // chrome_height counts vpad(1) + label + gap + desc + preview
            // + bottom gap(1); options_start_y sits after exactly that.
            assert_eq!(
                result.options_start_y - area.y,
                expected_chrome,
                "chrome accounting vs render drift at content_w={content_w}"
            );
        }
    }

    #[test]
    fn begin_feedback_trace_stage_swaps_report_for_consent_options() {
        let mut state = QuestionViewState::new(
            "fb".into(),
            vec![Question {
                question: FEEDBACK_QUESTION_LABEL.into(),
                options: vec![],
                multi_select: Some(false),
                id: None,
            }],
            StashedPrompt::default(),
        )
        .with_local_kind(LocalQuestionKind::Feedback);
        state.per_question_freeform[0] = "clipboard is broken over ssh".into();

        state.begin_feedback_trace_stage(state.feedback_report(), vec![]);

        assert!(
            state.is_feedback(),
            "trace stage is still the feedback card"
        );
        assert!(state.is_feedback_trace());
        assert!(!state.is_feedback_report());
        assert_eq!(state.questions.len(), 1);
        assert_eq!(state.questions[0].question, FEEDBACK_TRACE_QUESTION_LABEL);
        assert_eq!(state.questions[0].options.len(), 3);
        assert_eq!(
            state.questions[0].options[2].label,
            "Opt out and don't ask again"
        );
        assert_eq!(
            state.questions[0]
                .options
                .iter()
                .map(|o| o.id.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some(FEEDBACK_TRACE_OPTION_OPT_IN),
                Some(FEEDBACK_TRACE_OPTION_OPT_OUT),
                Some(FEEDBACK_TRACE_OPTION_NEVER_ASK),
            ],
            "consent maps from ids, so every option must carry one"
        );
        assert!(
            matches!(state.selections[0], QuestionSelection::Single(Some(0))),
            "turning trace upload on is the default"
        );
        assert!(state.no_freeform, "consent card has no free-text row");
        assert_eq!(state.focus, QuestionFocus::Navigation);
        let Some(LocalQuestionKind::FeedbackTrace { report, .. }) = &state.local_kind else {
            panic!("local kind must carry the report");
        };
        assert_eq!(report, "clipboard is broken over ssh");
    }

    /// Helper: build a question with N options.
    fn make_question(text: &str, labels: &[&str], multi: bool) -> Question {
        Question {
            question: text.to_string(),
            options: labels
                .iter()
                .map(|l| QuestionOption {
                    label: l.to_string(),
                    description: format!("Desc for {l}"),
                    preview: None,
                    id: None,
                })
                .collect(),
            multi_select: Some(multi),
            id: None,
        }
    }

    /// Regression: on the terminal-native palette (`bg_visual = Reset`) the
    /// embedded cursor row used to be indistinguishable except for a bold
    /// label.
    #[test]
    #[serial_test::serial]
    fn embedded_cursor_row_takes_selection_accent() {
        use ratatui::style::Color;

        struct EmbedReset;
        impl Drop for EmbedReset {
            fn drop(&mut self) {
                crate::views::modal_window::set_embedded(false);
            }
        }
        let _reset = EmbedReset;
        crate::views::modal_window::set_embedded(true);

        let theme = Theme::terminal_default();
        let q = make_question("Pick one?", &["Alpha", "Beta"], false);
        let lines = build_flat_option_lines(
            &q,
            80,
            0,
            None,
            &QuestionSelection::Single(None),
            &theme,
            true,
            "",
            false,
            true,
        );

        // Cursor row (option 0): every colored span carries the accent, and
        // the row stays transparent.
        let cursor_line = &lines[0];
        assert!(
            cursor_line
                .spans
                .iter()
                .filter(|s| !s.content.trim().is_empty())
                .all(|s| s.style.fg == Some(theme.fuzzy_accent)),
            "cursor row must recolor all text with the selection accent, got {:?}",
            cursor_line
                .spans
                .iter()
                .map(|s| (s.content.clone(), s.style.fg))
                .collect::<Vec<_>>()
        );
        assert!(
            cursor_line
                .spans
                .iter()
                .all(|s| s.style.bg == Some(Color::Reset) || s.style.bg.is_none()),
            "embedded rows must not paint a background band"
        );

        // Non-cursor row keeps normal colors (label = text_primary).
        let other_line = &lines[1];
        assert!(
            other_line
                .spans
                .iter()
                .any(|s| s.content.contains("Beta") && s.style.fg == Some(theme.text_primary)),
            "non-cursor row keeps the normal label color"
        );

        // Freeform row on cursor: same accent treatment.
        let freeform_cursor = build_freeform_line(true, false, "", false, false, &theme, true);
        assert!(
            freeform_cursor
                .spans
                .iter()
                .filter(|s| !s.content.trim().is_empty())
                .all(|s| s.style.fg == Some(theme.fuzzy_accent)),
            "freeform cursor row must take the accent"
        );
    }

    #[test]
    #[serial_test::serial]
    fn full_tui_cursor_row_keeps_bg_visual_band() {
        crate::views::modal_window::set_embedded(false);
        let theme = Theme::default();
        let q = make_question("Pick one?", &["Alpha", "Beta"], false);
        let lines = build_flat_option_lines(
            &q,
            80,
            0,
            None,
            &QuestionSelection::Single(None),
            &theme,
            true,
            "",
            false,
            true,
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.bg == Some(theme.bg_visual)),
            "full TUI cursor row paints the bg_visual band"
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.content.contains("Alpha") && s.style.fg == Some(theme.text_primary)),
            "full TUI label keeps text_primary (no accent recolor)"
        );
    }

    // ── new() ──────────────────────────────────────────────────────────

    #[test]
    fn new_initializes_vectors_correctly() {
        let q1 = make_question("Pick one?", &["A", "B", "C"], false);
        let q2 = make_question("Pick many?", &["X", "Y"], true);
        let state = QuestionViewState::new(
            "tc-1".into(),
            vec![q1, q2],
            StashedPrompt {
                text: "stashed".into(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
        );

        assert_eq!(state.questions.len(), 2);
        assert_eq!(state.selections.len(), 2);
        assert_eq!(state.per_question_cursor.len(), 2);
        assert_eq!(state.per_question_scroll.len(), 2);
        assert_eq!(state.active_tab, 0);
        assert_eq!(state.stashed_prompt.text, "stashed");

        // Single-choice initialized to None
        assert!(matches!(
            state.selections[0],
            QuestionSelection::Single(None)
        ));
        // Multi-choice initialized to empty set
        assert!(matches!(
            state.selections[1],
            QuestionSelection::Multi(ref s) if s.is_empty()
        ));

        // Cursors all start at 0
        assert!(state.per_question_cursor.iter().all(|&c| c == 0));
        assert!(state.per_question_scroll.iter().all(|&s| s == 0));
    }

    // ── toggle_option ──────────────────────────────────────────────────

    #[test]
    fn toggle_option_multi_toggles_in_out() {
        let q = make_question("Pick?", &["A", "B", "C"], true);
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());

        // Toggle on
        state.toggle_option(0, 1);
        assert_eq!(state.selected_labels(0), vec!["B"]);

        // Toggle another on
        state.toggle_option(0, 0);
        let mut labels = state.selected_labels(0);
        labels.sort();
        assert_eq!(labels, vec!["A", "B"]);

        // Toggle first one off
        state.toggle_option(0, 1);
        assert_eq!(state.selected_labels(0), vec!["A"]);
    }

    // ── select_option ──────────────────────────────────────────────────

    #[test]
    fn select_option_single_replaces_previous() {
        let q = make_question("Pick?", &["A", "B", "C"], false);
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());

        state.select_option(0, 0);
        assert_eq!(state.selected_labels(0), vec!["A"]);

        state.select_option(0, 2);
        assert_eq!(state.selected_labels(0), vec!["C"]);
    }

    // ── selected_labels ────────────────────────────────────────────────

    #[test]
    fn selected_labels_mixed_selections() {
        let q1 = make_question("Single?", &["X", "Y"], false);
        let q2 = make_question("Multi?", &["P", "Q", "R"], true);
        let mut state = QuestionViewState::new("tc".into(), vec![q1, q2], StashedPrompt::default());

        state.select_option(0, 1); // Y
        state.toggle_option(1, 0); // P
        state.toggle_option(1, 2); // R

        assert_eq!(state.selected_labels(0), vec!["Y"]);
        let mut multi = state.selected_labels(1);
        multi.sort();
        assert_eq!(multi, vec!["P", "R"]);
    }

    // ── next_question / prev_question ──────────────────────────────────

    #[test]
    fn question_cycling_clamps_at_boundaries() {
        let qs = vec![
            make_question("Q1?", &["A"], false),
            make_question("Q2?", &["B"], false),
            make_question("Q3?", &["C"], false),
        ];
        let mut state = QuestionViewState::new("tc".into(), qs, StashedPrompt::default());

        assert_eq!(state.active_tab, 0);
        state.next_question();
        assert_eq!(state.active_tab, 1);
        state.next_question();
        assert_eq!(state.active_tab, 2);
        state.next_question();
        assert_eq!(state.active_tab, 2); // clamped at end

        state.prev_question();
        assert_eq!(state.active_tab, 1);
        state.prev_question();
        assert_eq!(state.active_tab, 0);
        state.prev_question();
        assert_eq!(state.active_tab, 0); // clamped at start
    }

    // ── compute_max_label_w ────────────────────────────────────────────

    #[test]
    fn compute_max_label_w_caps_long_labels_at_60_percent() {
        let options = vec![
            QuestionOption {
                label: "A very long label that is way too wide for half the width here".into(),
                description: String::new(),
                preview: None,
                id: None,
            },
            QuestionOption {
                label: "Medium label".into(),
                description: String::new(),
                preview: None,
                id: None,
            },
            QuestionOption {
                label: "Short".into(),
                description: String::new(),
                preview: None,
                id: None,
            },
        ];
        // content_w=80 → cap = 48. Longest label is 63 → capped at 48.
        assert_eq!(compute_max_label_w(&options, 80), 48);
    }

    #[test]
    fn compute_max_label_w_uses_longest_when_all_fit() {
        let options = vec![
            QuestionOption {
                label: "Hello".into(),
                description: String::new(),
                preview: None,
                id: None,
            },
            QuestionOption {
                label: "World!".into(),
                description: String::new(),
                preview: None,
                id: None,
            },
        ];
        // content_w=80 → cap = 48. Both fit. Longest = "World!" = 6.
        assert_eq!(compute_max_label_w(&options, 80), 6);
    }

    #[test]
    fn compute_max_label_w_never_zero_when_all_labels_long() {
        let options = vec![
            QuestionOption {
                label: "Lorem ipsum dolor sit amet consectetur adipiscing!".into(),
                description: "Lorem ipsum dolor sit ame".into(),
                preview: None,
                id: None,
            },
            QuestionOption {
                label: "Lorem ipsum dolor sit amet elit sed tempor".into(),
                description: "Lorem ipsum dolor sit amet elit s".into(),
                preview: None,
                id: None,
            },
        ];
        // content_w=100 → cap = 60. Longest label = 50 → fits, column = 50.
        assert_eq!(compute_max_label_w(&options, 100), 50);
    }

    #[test]
    fn unfocused_row_with_all_long_labels_still_shows_label() {
        let opt = QuestionOption {
            label: "Lorem ipsum dolor sit amet consectetur adipiscing!".into(),
            description: "Lorem ipsum dolor sit amet, consectetur adipiscing.".into(),
            preview: None,
            id: None,
        };
        let content_w = 100usize;
        let max_label_w = compute_max_label_w(std::slice::from_ref(&opt), content_w);
        assert!(max_label_w > 0);

        let theme = Theme::default();
        let mut lines = Vec::new();
        build_single_option_lines(
            &mut lines,
            0,
            &opt,
            false,
            false,
            max_label_w,
            6,
            content_w,
            theme.bg_light,
            None,
            &theme,
            false,
        );
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("Lorem ipsum dolor sit amet consectetur adipiscing!"),
            "unfocused row must show the label, got: {text:?}"
        );
        assert!(
            text.contains("Lorem ipsum dolor sit amet, consectetur"),
            "unfocused row should still show the collapsed description, got: {text:?}"
        );
    }

    #[test]
    fn focused_stacked_description_wraps_at_prefix_indent_not_label_column() {
        let opt = QuestionOption {
            label: "This is an intentionally very long option label designed to test how the \
                    question view handles label visibility when the text greatly exceeds the cap"
                .into(),
            description: "This description exists purely to stress test the option description \
                          rendering in the question view. It should be long enough to force \
                          wrapping across multiple lines on most terminal widths."
                .into(),
            preview: None,
            id: None,
        };
        let content_w = 100usize;
        let prefix_w = 6usize;
        let max_label_w = compute_max_label_w(std::slice::from_ref(&opt), content_w);
        assert!(normalize_label(&opt.label).width() > max_label_w);

        let theme = Theme::default();
        let mut lines = Vec::new();
        build_single_option_lines(
            &mut lines,
            0,
            &opt,
            false,
            false,
            max_label_w,
            prefix_w,
            content_w,
            theme.bg_light,
            None,
            &theme,
            true,
        );

        let label_line_count =
            wrap_label_chunks(&normalize_label(&opt.label), content_w - prefix_w).len();
        let desc_lines = &lines[label_line_count..];
        assert!(!desc_lines.is_empty());
        let mut texts = Vec::new();
        for line in desc_lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let leading = text.len() - text.trim_start().len();
            assert_eq!(
                leading, prefix_w,
                "stacked description must be indented at prefix_w, not the label column: {text:?}"
            );
            texts.push(text);
        }
        assert!(
            texts
                .iter()
                .any(|t| t.trim_end().len() > prefix_w + max_label_w),
            "stacked description should use the full row width, got: {texts:?}"
        );

        let heights = option_visual_height(&opt, content_w, prefix_w, max_label_w, true);
        assert_eq!(heights as usize, lines.len());
    }

    // ── is_on_freeform_row ─────────────────────────────────────────────

    #[test]
    fn is_on_freeform_row_returns_true_at_end() {
        let q = make_question("Pick?", &["A", "B"], false);
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());

        // Cursor at 0 → on option A, not freeform
        assert!(!state.is_on_freeform_row());

        // Cursor at 2 → options.len() == 2, so this is the freeform row
        state.set_cursor(2);
        assert!(state.is_on_freeform_row());
    }

    // ── cursor / set_cursor ────────────────────────────────────────────

    #[test]
    fn set_cursor_clamps_to_valid_range() {
        let q = make_question("Pick?", &["A", "B"], false);
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());

        // total_items = 3 (A, B, freeform), so max cursor = 2
        state.set_cursor(100);
        assert_eq!(state.cursor(), 2);

        state.set_cursor(0);
        assert_eq!(state.cursor(), 0);
    }

    // ── total_items ────────────────────────────────────────────────────

    #[test]
    fn total_items_counts_options_plus_freeform() {
        let q = make_question("Pick?", &["A", "B", "C"], false);
        let state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());
        assert_eq!(state.total_items(0), 4); // 3 options + 1 freeform
    }

    // ── no_freeform ────────────────────────────────────────────────────

    /// `no_freeform` questions (e.g. the EchoAgent upsell) have no "Other"
    /// row, so activating freeform input must be impossible: focus stays in
    /// Navigation and nothing gets marked selected. Regression test for the
    /// upsell modal letting the user type after clicking under the last
    /// option.
    #[test]
    fn activate_freeform_input_is_noop_when_no_freeform() {
        let q = make_question("Pick?", &["A", "B"], false);
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default())
            .with_no_freeform();
        state.selections[0] = QuestionSelection::Single(Some(1));

        let text = state.activate_freeform_input();

        assert_eq!(text, "");
        assert_eq!(state.focus, QuestionFocus::Navigation);
        assert!(!state.per_question_freeform_selected[0]);
        assert!(
            matches!(state.selections[0], QuestionSelection::Single(Some(1))),
            "option selection must survive"
        );
    }

    /// The panel height for a `no_freeform` question must not reserve the
    /// (never rendered) freeform row — that dead row was clickable and
    /// activated freeform input on the upsell modal.
    #[test]
    fn question_view_height_excludes_freeform_row_when_no_freeform() {
        let q = make_question("Pick?", &["A", "B", "C"], false);
        let mut with_freeform =
            QuestionViewState::new("tc".into(), vec![q.clone()], StashedPrompt::default());
        let mut without_freeform =
            QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default())
                .with_no_freeform();

        let h_with = question_view_height(&mut with_freeform, 50, 80);
        let h_without = question_view_height(&mut without_freeform, 50, 80);
        assert_eq!(
            h_with,
            h_without + 1,
            "no_freeform panel must be exactly one row shorter"
        );

        // Fullscreen path too.
        with_freeform.fullscreen = true;
        without_freeform.fullscreen = true;
        let h_with = question_view_height(&mut with_freeform, 50, 80);
        let h_without = question_view_height(&mut without_freeform, 50, 80);
        assert_eq!(h_with, h_without + 1);
    }

    // ── option_visual_height ───────────────────────────────────────────

    #[test]
    fn option_visual_height_unfocused_always_1() {
        let opt = QuestionOption {
            label: "Short".into(),
            description: "A description that is longer than the available width".into(),
            preview: None,
            id: None,
        };
        assert_eq!(option_visual_height(&opt, 30, 6, 5, false), 1);
    }

    #[test]
    fn option_visual_height_focused_wraps() {
        let opt = QuestionOption {
            label: "Short".into(),
            description: "A description that is longer than the available width".into(),
            preview: None,
            id: None,
        };
        // content_w=30, prefix_w=6, max_label_w=5, gap=2 → indent=13, desc_w=17
        let h = option_visual_height(&opt, 30, 6, 5, true);
        assert!(h >= 3, "expected >= 3, got {h}");
    }

    // ── chrome_height ──────────────────────────────────────────────────

    #[test]
    fn split_question_label_desc_no_break() {
        let (label, desc) = split_question_label_desc("Which database engine?");
        assert_eq!(label, "Which database engine?");
        assert_eq!(desc, "");
    }

    #[test]
    fn split_question_label_desc_with_break() {
        let (label, desc) =
            split_question_label_desc("Which database?\n\nPick the engine for the backend.");
        assert_eq!(label, "Which database?");
        assert_eq!(desc, "Pick the engine for the backend.");
    }

    #[test]
    fn chrome_height_short_question() {
        // Short question, no description: vpad(1) + label(1) + gap(1) + gap(1) = 4.
        let q = make_question("Which database engine?", &["A"], false);
        assert_eq!(
            chrome_height(
                &q,
                80,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            4
        );
    }

    #[test]
    fn chrome_height_option_less_question_drops_the_label_gap() {
        // Nothing under the label to separate it from, so the gap goes: vpad(1) + label(1) + gap(1) = 3. This is the bare `/feedback` card.
        let q = make_question("How can we improve EchoAgent Build?", &[], false);
        assert_eq!(
            chrome_height(
                &q,
                80,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            3
        );
    }

    #[test]
    fn chrome_height_with_description() {
        // Question with paragraph break: label + gap + description + gap.
        let q = make_question(
            "Which database?\n\nChoose the primary data store for the backend service.",
            &["A"],
            false,
        );
        let desc_part = "Choose the primary data store for the backend service.";
        // vpad(1) + label(1) + gap(1) + desc lines + gap(1)
        let desc_lines = desc_part.len().div_ceil(80).max(1) as u16; // 1 line at width 80
        assert_eq!(
            chrome_height(
                &q,
                80,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            1 + 1 + 1 + desc_lines + 1
        );
    }

    #[test]
    fn chrome_height_wraps_long_question() {
        // 60-char question at width 40 wraps to 2 lines: vpad(1) + label(2) + gap(1) + gap(1) = 5.
        let q = make_question(
            "Which database engine should we use for the backend service?",
            &["A"],
            false,
        );
        assert_eq!(
            chrome_height(
                &q,
                40,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            5
        );
        // Same question at width 80 fits on 1 line: 4.
        assert_eq!(
            chrome_height(
                &q,
                80,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            4
        );
    }

    #[test]
    fn chrome_height_wraps_extra_long_question() {
        // 150-char question wraps across multiple lines depending on width.
        //
        // At width 75 (typical terminal with chrome):
        //   ┃  Given the requirements for high availability, horizontal
        //   ┃  scaling, and strict ACID compliance, which database engine
        //   ┃  and replication topology should we adopt for the user
        //   ┃  accounts microservice?
        //   ┃
        //   ┃  1 [ ] PostgreSQL   ...
        //
        // At width 40 (narrow):
        //   ┃  Given the requirements for high
        //   ┃  availability, horizontal scaling,
        //   ┃  and strict ACID compliance, which
        //   ┃  database engine and replication
        //   ┃  topology should we adopt for the
        //   ┃  user accounts microservice?
        //   ┃
        //   ┃  1 [ ] PostgreSQL   ...
        let q = make_question(
            "Given the requirements for high availability, horizontal scaling, \
             and strict ACID compliance, which database engine and replication \
             topology should we adopt for the user accounts microservice?",
            &["PostgreSQL", "CockroachDB", "TiDB"],
            false,
        );
        // Word-wrap at width 75: 3 lines → vpad(1) + label(3) + gap(1) + gap(1) = 6
        assert_eq!(
            chrome_height(
                &q,
                75,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            6
        );
        // Word-wrap at width 40: 6 lines (word boundaries prevent mid-word splits)
        // → vpad(1) + label(6) + gap(1) + gap(1) = 9
        assert_eq!(
            chrome_height(
                &q,
                40,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            9
        );
        // Word-wrap at width 200: 1 line → vpad(1) + label(1) + gap(1) + gap(1) = 4
        assert_eq!(
            chrome_height(
                &q,
                200,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            4
        );
    }

    #[test]
    fn chrome_height_with_preview() {
        // Short question + preview: vpad(1) + label(1) + gap(1) + preview_gap(1) + preview(1) + gap(1) = 6.
        let q = make_question("Which database?", &["A"], false);
        let preview = "commit abc123: fix the bug";
        assert_eq!(
            chrome_height(
                &q,
                80,
                Some(preview),
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            6
        );
    }

    #[test]
    fn chrome_height_with_long_preview() {
        // Preview that wraps across 2 lines at width 40.
        let q = make_question("Confirm?", &["A"], false);
        let preview = "fix(auth): resolve token refresh race condition in middleware";
        // Word-wrap at width 40: 2 lines → vpad(1) + label(1) + gap(1) + preview_gap(1) + preview(2) + gap(1) = 7
        assert_eq!(
            chrome_height(
                &q,
                40,
                Some(preview),
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            7
        );
    }

    #[test]
    fn chrome_height_with_multiline_preview() {
        // Multi-line preview: each \n-separated line is wrapped independently.
        let q = make_question("Confirm?", &["A"], false);
        let preview =
            "fix(auth): token refresh\n\nResolves the race condition\nin the middleware layer";
        // .lines() yields 4 segments (including one empty line).
        // word_wrap_line returns 1 line for each, so 4 preview lines total.
        // vpad(1) + label(1) + gap(1) + preview_gap(1) + preview(4) + gap(1) = 9
        assert_eq!(
            chrome_height(
                &q,
                80,
                Some(preview),
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            9
        );
    }

    #[test]
    fn chrome_height_with_empty_preview() {
        // Empty preview should be the same as None.
        let q = make_question("Pick?", &["A"], false);
        assert_eq!(
            chrome_height(
                &q,
                80,
                Some(""),
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
            chrome_height(
                &q,
                80,
                None,
                false,
                DEFAULT_MAX_CHROME_DESC_LINES,
                DEFAULT_MAX_CHROME_PREVIEW_LINES
            ),
        );
    }

    // ── focused_preview ──────────────────────────────────────────────

    #[test]
    fn focused_preview_returns_preview_when_on_option() {
        let q = Question {
            question: "Pick?".into(),
            options: vec![
                QuestionOption {
                    label: "A".into(),
                    description: "desc A".into(),
                    preview: Some("preview content".into()),
                    id: None,
                },
                QuestionOption {
                    label: "B".into(),
                    description: "desc B".into(),
                    preview: None,
                    id: None,
                },
            ],
            multi_select: Some(false),
            id: None,
        };
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());

        // Cursor at 0 → option A has preview.
        assert_eq!(state.focused_preview(), Some("preview content"));

        // Cursor at 1 → option B has no preview.
        state.set_cursor(1);
        assert_eq!(state.focused_preview(), None);

        // Cursor at 2 → freeform row, no preview.
        state.set_cursor(2);
        assert_eq!(state.focused_preview(), None);
    }

    // ── toggle on Single ───────────────────────────────────────────────

    #[test]
    fn toggle_option_single_deselects_when_same() {
        let q = make_question("Pick?", &["A", "B"], false);
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());

        state.toggle_option(0, 1);
        assert_eq!(state.selected_labels(0), vec!["B"]);

        // Toggle same → deselect
        state.toggle_option(0, 1);
        assert!(state.selected_labels(0).is_empty());
    }

    #[test]
    fn item_index_at_visual_line_accounts_for_wrapped_rows() {
        let q = Question {
            question: "Pick one".into(),
            options: vec![
                QuestionOption {
                    label: "Alpha".into(),
                    description: "one two three four five six seven eight nine ten eleven".into(),
                    preview: None,
                    id: None,
                },
                QuestionOption {
                    label: "Beta".into(),
                    description: "short desc".into(),
                    preview: None,
                    id: None,
                },
            ],
            multi_select: Some(true),
            id: None,
        };

        let content_w = 20;
        let cursor = 0; // focus first option so it gets full height
        let heights = option_heights(&q, content_w, cursor);
        assert!(heights[0] > 1);

        for line in 0..heights[0] {
            assert_eq!(item_index_at_visual_line(&q, content_w, line, cursor), 0);
        }
        assert_eq!(
            item_index_at_visual_line(&q, content_w, heights[0], cursor),
            1
        );
    }

    #[test]
    fn clamp_scroll_limits_offset_to_viewport() {
        let q = make_question("Pick?", &["A", "B", "C", "D"], true);
        let mut state =
            QuestionViewState::new("tc".into(), vec![q.clone()], StashedPrompt::default());
        state.per_question_scroll[0] = 100;

        let visible_h = 2;
        let content_w = 80;
        let expected_max =
            total_options_height(&q, content_w, state.cursor()).saturating_sub(visible_h);
        state.clamp_scroll(visible_h, content_w);

        assert_eq!(state.per_question_scroll[0], expected_max);
    }

    // ── truncation cap tests ───────────────────────────────────────────

    #[test]
    fn chrome_height_caps_long_description() {
        // 10-line description using CommonMark hard breaks (`  \n`) so each
        // logical line renders as its own visual line; bare `\n` between
        // text lines is a soft break and collapses to a space.
        let q = make_question(
            "Q?\n\nline1  \nline2  \nline3  \nline4  \nline5  \nline6  \nline7  \nline8  \nline9  \nline10",
            &["A"],
            false,
        );
        let uncapped = chrome_height(&q, 80, None, true, 5, 6);
        let capped = chrome_height(&q, 80, None, false, 5, 6);
        assert!(
            uncapped > capped,
            "fullscreen ({uncapped}) should exceed capped ({capped})",
        );
        // capped: vpad(1) + label(1) + gap(1) + desc(5) + gap(1) = 9
        assert_eq!(capped, 9);
    }

    #[test]
    fn chrome_height_caps_long_preview() {
        // 8-line preview at preview_cap=3 should be capped.
        let q = make_question("Pick?", &["A"], false);
        let preview = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8";
        let uncapped = chrome_height(&q, 80, Some(preview), true, 5, 3);
        let capped = chrome_height(&q, 80, Some(preview), false, 5, 3);
        assert!(
            uncapped > capped,
            "fullscreen ({uncapped}) should exceed capped ({capped})",
        );
        // capped: vpad(1) + label(1) + gap(1) + preview_gap(1) + preview(3) + gap(1) = 8
        assert_eq!(capped, 8);
    }

    #[test]
    fn chrome_height_preview_cap_zero_no_gap() {
        // When preview_cap=0 and !fullscreen, preview contributes 0 lines and
        // no gap — matching render_question_chrome which guards the gap on
        // capped_count > 0.
        let q = make_question("Pick?", &["A"], false);
        let preview = "some preview text";
        let with_preview = chrome_height(&q, 80, Some(preview), false, 5, 0);
        let without = chrome_height(&q, 80, None, false, 5, 0);
        assert_eq!(with_preview, without);
    }

    #[test]
    fn chrome_height_desc_cap_one() {
        // Edge case: desc_cap=1 should still show exactly 1 description line.
        let q = make_question("Q?\n\nline1\nline2\nline3", &["A"], false);
        let h = chrome_height(&q, 80, None, false, 1, 6);
        // vpad(1) + label(1) + gap(1) + desc(1) + gap(1) = 5
        assert_eq!(h, 5);
    }

    // ── question_view_height / minimum visible option rows ─────────────

    /// Helper: build a QuestionViewState for height tests.
    fn make_state_for_height(
        question_text: &str,
        labels: &[&str],
        fullscreen: bool,
    ) -> QuestionViewState {
        let q = make_question(question_text, labels, false);
        let mut state = QuestionViewState::new("tc".into(), vec![q], StashedPrompt::default());
        state.fullscreen = fullscreen;
        state
    }

    #[test]
    fn question_view_height_large_terminal_uses_static_caps() {
        // On a big terminal (80 rows), static caps work and at least
        // MIN_VISIBLE_OPTION_ROWS options are visible.
        let mut state = make_state_for_height(
            "Which database?",
            &["PostgreSQL", "MySQL", "SQLite", "CockroachDB", "TiDB"],
            false,
        );
        let content_w = 75;
        let h = question_view_height(&mut state, 80, content_w);

        let chrome_h = chrome_height(
            &state.questions[0],
            content_w,
            state.focused_preview(),
            false,
            state.cached_desc_cap,
            state.cached_preview_cap,
        );
        let visible_h = h.saturating_sub(chrome_h);
        assert!(
            visible_h >= MIN_VISIBLE_OPTION_ROWS,
            "visible_h={visible_h} < MIN_VISIBLE_OPTION_ROWS={MIN_VISIBLE_OPTION_ROWS}",
        );
        // Static caps should be unchanged.
        assert_eq!(state.cached_desc_cap, DEFAULT_MAX_CHROME_DESC_LINES);
        assert_eq!(state.cached_preview_cap, DEFAULT_MAX_CHROME_PREVIEW_LINES);
    }

    #[test]
    fn question_view_height_small_terminal_reduces_caps() {
        // On a small terminal (24 rows) with a long description, dynamic
        // fallback should reduce the desc/preview caps.
        let mut state = make_state_for_height(
            "Which database?\n\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8",
            &["PostgreSQL", "MySQL", "SQLite", "CockroachDB", "TiDB"],
            false,
        );
        let content_w = 75;
        let h = question_view_height(&mut state, 24, content_w);

        // At least one cap should have been reduced from the default.
        let caps_reduced = state.cached_desc_cap < DEFAULT_MAX_CHROME_DESC_LINES
            || state.cached_preview_cap < DEFAULT_MAX_CHROME_PREVIEW_LINES;
        assert!(
            caps_reduced,
            "expected dynamic cap reduction on small terminal, desc_cap={} preview_cap={}",
            state.cached_desc_cap, state.cached_preview_cap,
        );

        let chrome_h = chrome_height(
            &state.questions[0],
            content_w,
            state.focused_preview(),
            false,
            state.cached_desc_cap,
            state.cached_preview_cap,
        );
        let visible_h = h.saturating_sub(chrome_h);
        assert!(
            visible_h >= MIN_VISIBLE_OPTION_ROWS,
            "visible_h={visible_h} < MIN_VISIBLE_OPTION_ROWS={MIN_VISIBLE_OPTION_ROWS} on 24-row terminal",
        );
    }

    #[test]
    fn question_view_height_fullscreen_uses_max_caps() {
        let mut state = make_state_for_height(
            "Which database?\n\nline1\nline2\nline3",
            &["A", "B", "C"],
            true,
        );
        let _ = question_view_height(&mut state, 80, 75);
        assert_eq!(state.cached_desc_cap, u16::MAX);
        assert_eq!(state.cached_preview_cap, u16::MAX);
    }

    // ── focus-driven option height ─────────────────────────────────────

    #[test]
    fn unfocused_option_is_one_line_focused_is_full() {
        let opt = QuestionOption {
            label: "Opt".into(),
            description: "line1  \nline2  \nline3  \nline4  \nline5  \nline6".into(),
            preview: None,
            id: None,
        };
        let content_w = 40;
        let prefix_w = 6;
        let max_label_w = 5;
        let focused_h = option_visual_height(&opt, content_w, prefix_w, max_label_w, true);
        let unfocused_h = option_visual_height(&opt, content_w, prefix_w, max_label_w, false);
        assert_eq!(unfocused_h, 1, "unfocused should collapse to a single line");
        assert!(
            focused_h >= 6,
            "focused ({focused_h}) should show all six description lines",
        );
    }

    #[test]
    fn build_flat_option_lines_count_matches_option_heights_sum() {
        // Verify that the number of lines produced by build_flat_option_lines
        // equals the sum of option_heights (consistency check).
        let q = Question {
            question: "Pick one".into(),
            options: vec![
                QuestionOption {
                    label: "Alpha".into(),
                    description: "short".into(),
                    preview: None,
                    id: None,
                },
                QuestionOption {
                    label: "Beta".into(),
                    description: "a longer description that should wrap at narrow width and produce multiple lines of text".into(),
                    preview: None,
                    id: None,
                },
                QuestionOption {
                    label: "Gamma".into(),
                    description: "desc".into(),
                    preview: None,
                    id: None,
                },
            ],
            multi_select: Some(false),
            id: None,
        };
        let content_w = 30;
        let cursor = 1; // focus Beta
        let theme = Theme::default();
        let sel = QuestionSelection::Single(None);

        let lines = build_flat_option_lines(
            &q, content_w, cursor, None, &sel, &theme, true, // show freeform
            "", false, true, // panel focused
        );
        let heights = option_heights(&q, content_w, cursor);
        let expected_total: u16 = heights.iter().sum();
        assert_eq!(
            lines.len(),
            expected_total as usize,
            "flat lines ({}) should equal sum of option_heights ({expected_total})",
            lines.len(),
        );
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn single_option_lines(desc: &str, content_w: usize, focused: bool) -> Vec<Line<'static>> {
        let q = Question {
            question: "Pick".into(),
            options: vec![QuestionOption {
                label: "Yes".into(),
                description: desc.into(),
                preview: None,
                id: None,
            }],
            multi_select: Some(false),
            id: None,
        };
        let theme = Theme::default();
        let sel = QuestionSelection::Single(None);
        let cursor = if focused { 0 } else { 1 };
        build_flat_option_lines(
            &q, content_w, cursor, None, &sel, &theme, false, "", false, true,
        )
    }

    #[test]
    fn unfocused_long_description_collapses_to_one_line_with_ellipsis() {
        let lines = single_option_lines(
            "a very long single line description that will not fit on the row and must be \
             truncated to a single line ending with an ellipsis",
            40,
            false,
        );
        assert_eq!(lines.len(), 1, "unfocused option must be a single line");
        let text = line_text(&lines[0]);
        assert!(text.contains('\u{2026}'), "expected ellipsis, got {text:?}");
    }

    #[test]
    fn unfocused_multiline_description_shows_ellipsis_even_when_first_line_short() {
        let lines = single_option_lines("short  \nthen a second line of content", 60, false);
        assert_eq!(lines.len(), 1);
        let text = line_text(&lines[0]);
        assert!(text.contains("short"), "first line should show: {text:?}");
        assert!(
            text.contains('\u{2026}'),
            "expected ellipsis for more content: {text:?}",
        );
    }

    #[test]
    fn unfocused_short_description_has_no_ellipsis() {
        let lines = single_option_lines("tiny", 60, false);
        assert_eq!(lines.len(), 1);
        let text = line_text(&lines[0]);
        assert!(text.contains("tiny"));
        assert!(
            !text.contains('\u{2026}'),
            "short fully-shown description should not get an ellipsis: {text:?}",
        );
    }

    #[test]
    fn focused_long_description_expands_to_multiple_lines() {
        let lines = single_option_lines("line1  \nline2  \nline3  \nline4", 40, true);
        assert!(
            lines.len() >= 4,
            "focused option should show all lines, got {}",
            lines.len(),
        );
    }

    #[test]
    fn no_navigate_and_expand_hint_in_rendered_lines() {
        let lines = single_option_lines("l1  \nl2  \nl3  \nl4  \nl5", 40, false);
        for line in &lines {
            let text = line_text(line);
            assert!(
                !text.contains("navigate & expand"),
                "removed hint should not appear: {text:?}",
            );
        }
    }

    #[test]
    fn flat_lines_match_heights_with_overflowing_focused_label() {
        let q = Question {
            question: "Pick".into(),
            options: vec![
                QuestionOption {
                    label: "A really long label that overflows the aligned column width".into(),
                    description: "line1  \nline2  \nline3".into(),
                    preview: None,
                    id: None,
                },
                QuestionOption {
                    label: "Short".into(),
                    description: "desc".into(),
                    preview: None,
                    id: None,
                },
            ],
            multi_select: Some(false),
            id: None,
        };
        let content_w = 30;
        let cursor = 0;
        let theme = Theme::default();
        let sel = QuestionSelection::Single(None);
        let lines = build_flat_option_lines(
            &q, content_w, cursor, None, &sel, &theme, false, "", false, true,
        );
        let heights = option_heights(&q, content_w, cursor);
        let expected: u16 = heights[..q.options.len()].iter().sum();
        assert_eq!(lines.len(), expected as usize);
    }

    #[test]
    fn collapsed_description_spans_respects_zero_width() {
        let opt = QuestionOption {
            label: "x".into(),
            description: "some long description that definitely has content".into(),
            preview: None,
            id: None,
        };
        let theme = Theme::default();
        let spans = collapsed_description_spans(&opt, 0, theme.bg_light, theme.gray);
        let w: usize = spans.iter().map(|s| s.content.width()).sum();
        assert_eq!(w, 0, "zero-width budget must produce no spans");
    }
}
