//! Reusable prompt input widget.
//!
//! [`PromptWidget`] is a text input component built on [`TextArea`] that handles
//! text editing, multiline input, and submit. It does NOT handle focus management
//! (Esc, Tab) or render chrome (accent lines, selection boxes) — those are the
//! caller's responsibility, making this widget reusable across agent view,
//! welcome screen, overlays, etc.
//!
//! ## Visual (as rendered by agent view with chrome)
//!
//! ```text
//!                                            ← top vpad (configurable)
//!  ❯ type here, text wraps                   ← prefix + TextArea
//!    continuation of long input...            ← TextArea continuation
//!  echo-agent-3 · yolo                             ← info line (optional)
//! ```
//!
//! The accent line (┃) and selection box are rendered by the caller.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use echo_agent_ratatui_textarea::{ElementId, ElementKind, TextArea, TextAreaState, TextElement};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::StatefulWidgetRef;

use crate::clipboard::{SystemClipboard, system_clipboard_get};
use crate::input::key::key;
use crate::prompt_images::PastedImage;
use crate::render::{PreviewConfig, PreviewStyle, SafeBuf, render_preview_overlay};
use crate::theme::Theme;
use crate::views::file_search::{FileSearchState, context::normalize_display_path};
use crate::views::history_search::HistorySearchState;
use crate::views::suggestion_controller::{
    AcceptMode, CompletionSplice, SuggestionController, SuggestionSource,
};

/// Element kind tag for pasted multi-line content.
pub const KIND_PASTE: ElementKind = ElementKind(1);
/// Element kind tag for @-file references (future use).
pub const KIND_FILE_REF: ElementKind = ElementKind(2);
/// Element kind tag for pasted image chips (`[Image #N]`).
pub const KIND_IMAGE: ElementKind = ElementKind(3);

/// Byte size at which a single-line paste is chipped (display only — not an offload threshold).
const PASTE_CHIP_DISPLAY_BYTES: usize = 10_000;

pub use crate::prompt_images::PROMPT_IMAGES_TRACING_TARGET;

/// What kind of element interaction occurred when pressing Enter on a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementInteraction {
    /// Paste or file-ref element was inlined (expanded).
    Inlined,
    /// Image chip was activated (caller should open preview).
    ImagePreview,
}

/// What happened after the widget processed an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptEvent {
    /// Text was modified (char inserted, deleted, paste, newline). Redraw needed.
    Edited,
    /// The widget didn't handle this event. Caller should handle it
    /// (Esc, Tab, Ctrl-D, etc.).
    Ignored,
}

/// Result of routing an Enter-family key event through a submittable prompt.
///
/// Used by [`PromptWidget::route_enter`] to centralize the three behaviors
/// that every "bare Enter submits" context needs: Apple Terminal CoreGraphics
/// modifier rescue, backslash continuation, and file-search guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterOutcome {
    /// A newline was inserted (Apple Terminal modifier rescue or backslash
    /// continuation). The caller should return `InputOutcome::Changed`.
    NewlineInserted,
    /// Bare Enter with no active dropdown — the caller should perform its
    /// submit/save/advance action.
    Submit,
    /// Not an Enter key, or the file-search dropdown is open (let
    /// `handle_key()` deal with it).  The caller should fall through
    /// to `prompt.handle_key(key)`.
    PassThrough,
}

/// Result of handling a key event while file search dropdown is visible.
pub(crate) enum FileSearchKeyResult {
    /// Key was handled (navigation). Redraw needed.
    Handled,
    /// User accepted the selected result (Tab/Enter).
    Accepted,
    /// User accepted the result without a trailing space (Right Arrow).
    ///
    /// Used for "drill-down" completion: the path is inserted but the cursor
    /// stays immediately after it (no terminating space), so the user can
    /// continue typing path segments. For directory results, a trailing `/`
    /// is appended so further segments can be typed immediately. For file
    /// results, the path is inserted as an atomic element with no trailing
    /// space.
    AcceptedNoSpace,
    /// User accepted the result and wants to open the line viewer (`:` or Ctrl-L).
    AcceptAndOpenViewer,
    /// User dismissed the dropdown (Esc).
    Dismissed,
    /// Key not handled by file search — fall through to normal handling.
    PassThrough,
}

/// Request to open the line viewer, with optional initial line range.
pub(crate) struct ViewerRequest {
    pub path: std::path::PathBuf,
    /// Optional initial line range (1-based, exclusive end) to scroll to and select.
    pub initial_range: Option<std::ops::Range<usize>>,
}

/// Check if the file search has a valid selection.
fn file_search_has_selection(fs: &FileSearchState) -> bool {
    fs.selected() < fs.result_count()
}

/// Whether the in-app `Cmd+A` "select all in prompt" handler is supported
/// for the given terminal brand.
///
/// Gated to **Ghostty only**. Every other macOS terminal either swallows
/// `Cmd+A` at the terminal layer (Apple Terminal, default iTerm2, default
/// Kitty / WezTerm) or has idiosyncratic in-terminal "Select All"
/// behaviour we don't want to fight. Ghostty users can opt in with a
/// one-line `keybind = cmd+a=unbind` in `~/.config/ghostty/config`, after
/// which this handler fires and selects all text inside the prompt
/// buffer.
///
/// Extracted as a free function so tests can verify the gate per-brand
/// without going through the global terminal context singleton (see
/// `cmd_a_supported_only_for_ghostty` in the tests submodule).
fn cmd_a_select_all_supported(brand: crate::terminal::TerminalName) -> bool {
    matches!(brand, crate::terminal::TerminalName::Ghostty)
}

/// Visual configuration for prompt rendering.
#[derive(Debug, Clone)]
pub struct PromptStyle {
    /// Whether the prompt is focused (affects prefix color, text dimming).
    pub focused: bool,
    /// Whether to show the ❯ prefix character.
    pub show_prefix: bool,
    /// Vertical padding above the text content.
    pub vpad_top: u16,
    /// Whether to render chrome (accent line + hpad + background fill).
    /// When true, the widget renders the full block layout (accent | hpad | content | hpad).
    /// When false, the widget renders only content (for use in overlays).
    pub chrome: bool,
    /// Layout config for chrome mode (accent + padding widths).
    /// Only used when `chrome` is true.
    pub chrome_pad_left: u16,
    pub chrome_pad_right: u16,
    /// Background surface for the prompt; see [`PromptBg`].
    pub bg: PromptBg,
    /// Override the accent line color. When `Some`, uses this color instead
    /// of the default `accent_user` / `gray_dim`. Used for plan mode (golden).
    pub accent_color_override: Option<ratatui::style::Color>,
    /// Override the border color (╭─╮│╰─╯). When `Some`, uses this color
    /// instead of `prompt_border_active` / `prompt_border`. Used for plan mode.
    pub border_color_override: Option<ratatui::style::Color>,
    /// Override the prefix character and its color.
    /// When `Some((str, color))`, replaces the default `❯` prefix.
    /// Used for bash mode (`"! "` in yellow).
    pub prefix_override: Option<(&'static str, ratatui::style::Color)>,
    /// Override the placeholder text shown when the textarea is empty (e.g. remember mode).
    /// When `Some(text)`, uses this instead of the default `"Build anything"`.
    pub placeholder_override: Option<&'static str>,
    /// The main composer hides the empty-textarea placeholder on focus; the feedback box keeps it visible.
    pub placeholder_when_focused: bool,
    /// Compact mode (currently unused for info_block sizing).
    pub compact: bool,
    /// Show the accent line (`┃`) on the left edge of the chrome.
    /// When false the line is hidden and its column reclaimed for content.
    pub show_accent_line: bool,
    /// Draw the prompt's border box (top `╭─╮` divider, side `│` borders, and
    /// bottom info divider). Only consulted when `chrome` is true. Defaults to
    /// `true` (the full-TUI boxed prompt); minimal mode sets it `false` for a
    /// cleaner, border-less input that still keeps the chrome padding.
    pub show_borders: bool,
    /// Session title inlined in the top border (right-aligned, 2-cell inset),
    /// styled like the bottom info line's model name.
    /// None (default) keeps the plain border. Set only by the agent view.
    pub title: Option<String>,
    /// Paint image-chip overlay into `overlay_area` (default true).
    pub image_preview: bool,
}

/// Background for the prompt widget.
///
/// Paste chips bake `theme.paste_bg` — a badge color tuned for the default
/// canvas — into their display `Line` at paste time, so the background says
/// what *kind* of surface the prompt sits on, not just its color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptBg {
    /// The standalone prompt's default fill (`theme.bg_base`).
    #[default]
    Default,
    /// Explicit canvas color for prompts rendered inline within another
    /// widget whose surface matches the main prompt's (dashboard dispatch
    /// box, peek reply). Chips keep their badge background.
    Canvas(ratatui::style::Color),
    /// Inline panel color (question freeform input, permission follow-up).
    /// Chip cells are repainted to blend into the panel.
    Panel(ratatui::style::Color),
}

impl PromptBg {
    /// Effective fill color; `default` is the standalone prompt's.
    fn color(self, default: ratatui::style::Color) -> ratatui::style::Color {
        match self {
            Self::Default => default,
            Self::Canvas(c) | Self::Panel(c) => c,
        }
    }
}

impl Default for PromptStyle {
    fn default() -> Self {
        Self {
            focused: true,
            show_prefix: true,
            vpad_top: 1,
            chrome: true,
            chrome_pad_left: 2,
            chrome_pad_right: 1,
            bg: PromptBg::Default,
            accent_color_override: None,
            border_color_override: None,
            prefix_override: None,
            placeholder_when_focused: false,
            placeholder_override: None,
            compact: false,
            show_accent_line: false,
            show_borders: true,
            title: None,
            image_preview: true,
        }
    }
}

impl PromptStyle {
    /// Style for overlays (no chrome, no vpad).
    pub fn overlay() -> Self {
        Self {
            chrome: false,
            vpad_top: 0,
            ..Default::default()
        }
    }

    /// Style for inline prompts in overlay panels (permission, plan approval,
    /// question). Focused, chromeless, no prefix, no vpad, bg_visual background.
    pub fn inline(bg: ratatui::style::Color) -> Self {
        Self {
            focused: true,
            show_prefix: false,
            vpad_top: 0,
            chrome: false,
            chrome_pad_left: 0,
            chrome_pad_right: 0,
            bg: PromptBg::Panel(bg),
            accent_color_override: None,
            border_color_override: None,
            prefix_override: None,
            placeholder_when_focused: false,
            placeholder_override: None,
            compact: false,
            show_accent_line: false,
            show_borders: false,
            title: None,
            image_preview: true,
        }
    }

    /// Info block height: rows reserved for the bottom divider line.
    pub fn info_block(&self, has_info: bool) -> u16 {
        if has_info { 1 } else { 0 } // bottom divider only
    }

    /// Mode-tinted accent: the override (e.g. plan mode) when set,
    /// else the focus-dependent default. Shared by the accent line and the
    /// prompt prefix so tints stay in lockstep.
    pub fn accent_color(&self, theme: &Theme) -> ratatui::style::Color {
        self.accent_color_override.unwrap_or(if self.focused {
            theme.accent_user
        } else {
            theme.gray_dim
        })
    }
}

/// A flag displayed in the prompt info line (e.g., "plan", "always-approve").
pub struct PromptFlag<'a> {
    /// Display text for the flag.
    pub text: &'a str,
    /// Optional color override. When `Some`, the flag renders in this color
    /// (blended toward bg for subtlety). When `None`, uses the default gray.
    pub color: Option<ratatui::style::Color>,
    /// Whether to render with bold modifier.
    pub bold: bool,
}

/// Optional info line rendered below the prompt text.
///
/// The default is blank: a caller that wants the bottom border without any info text passes it, and [`Self::is_blank`] then skips the text pass.
#[derive(Default)]
pub struct PromptInfo<'a> {
    /// Primary label to display on the info line (left side).
    pub model_name: &'a str,
    /// Flags to display on the left side, joined by " · " (e.g., "plan", "always-approve").
    pub flags: &'a [PromptFlag<'a>],
    /// Whether multiline mode is active (shown right-aligned).
    pub multiline: bool,
    /// Optional usage warning displayed right-aligned (e.g. "5% usage left").
    pub usage_warning: Option<&'a str>,
    /// When true the warning uses the yellow warning color (<=5% left);
    /// when false it uses dim grey text (5-10% left).
    pub usage_warning_critical: bool,
}

impl PromptInfo<'_> {
    pub fn is_blank(&self) -> bool {
        self.model_name.is_empty()
            && self.flags.is_empty()
            && !self.multiline
            && self.usage_warning.is_none()
    }
}

/// Live voice-capture overlay for the prompt.
///
/// Interim STT paints as muted italic ghost text (not in the textarea).
/// Finalized STT is real prompt content and stays editable while the mic is open.
/// Overlay presence (even with no interim) marks voice active for callers that
/// skip empty-state placeholders while capturing.
#[derive(Debug, Clone, Copy)]
pub struct VoicePromptOverlay<'a> {
    /// Latest interim transcript, if any.
    pub interim: Option<&'a str>,
    /// Theme accent associated with this overlay.
    pub color: ratatui::style::Color,
}

/// Greedy word-wrap the interim transcript into at most `max_rows` lines of
/// `max_w` columns, appending an ellipsis to the last line when truncated.
fn wrap_voice_interim(text: &str, max_w: usize, max_rows: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    if max_w == 0 || max_rows == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in text.split_whitespace() {
        let ww = UnicodeWidthStr::width(word);
        if !cur.is_empty() && cur_w + 1 + ww > max_w {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if cur.is_empty() {
            if ww > max_w {
                cur = crate::render::line_utils::truncate_str(word, max_w);
                cur_w = UnicodeWidthStr::width(cur.as_str());
            } else {
                cur.push_str(word);
                cur_w = ww;
            }
        } else {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_rows {
        lines.truncate(max_rows);
        if let Some(last) = lines.last_mut() {
            let trimmed = crate::render::line_utils::truncate_str(last, max_w.saturating_sub(1));
            *last = format!("{trimmed}\u{2026}");
        }
    }
    lines
}

/// Result of rendering the prompt.
pub struct PromptRenderResult {
    /// Cursor position if the prompt wants a visible cursor.
    /// `None` when unfocused.
    pub cursor_pos: Option<(u16, u16)>,
    /// Terminal escape sequences to write after the ratatui cell flush
    /// (e.g. Kitty/iTerm2 inline image rendering for the image preview).
    pub post_flush_escapes: Option<crate::terminal::overlay::Escapes>,
}

/// Prompt state saved while another input context owns the composer.
#[derive(Debug, Default)]
pub struct StashedPrompt {
    pub text: String,
    pub cursor: usize,
    pub(crate) images: Vec<PastedImage>,
    pub(crate) chip_elements: Vec<crate::app::agent::ChipElement>,
    pub(crate) image_counter: usize,
    pub(crate) image_undo_stash: Vec<PastedImage>,
}

impl Drop for StashedPrompt {
    fn drop(&mut self) {
        crate::prompt_images::drain_and_cleanup(&mut self.images);
        crate::prompt_images::drain_and_cleanup(&mut self.image_undo_stash);
    }
}

/// Feedback attachments in transit between the composer, the trace-consent
/// card, and the send dispatch.
///
/// Owns its staged temp files: dropping without [`take`](Self::take) deletes them.
#[derive(Debug, Default)]
pub struct FeedbackImages(Vec<PastedImage>);

impl From<Vec<PastedImage>> for FeedbackImages {
    fn from(images: Vec<PastedImage>) -> Self {
        Self(images)
    }
}

impl Drop for FeedbackImages {
    fn drop(&mut self) {
        crate::prompt_images::drain_and_cleanup(&mut self.0);
    }
}

impl FeedbackImages {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Read access for encoding; cleanup stays with this owner.
    pub fn as_slice(&self) -> &[PastedImage] {
        &self.0
    }

    /// Hand the records to a consumer that takes over cleanup (the prompt
    /// widget adopting them as live chips).
    pub fn take(&mut self) -> Vec<PastedImage> {
        std::mem::take(&mut self.0)
    }
}

impl StashedPrompt {
    pub(crate) fn from_submission(
        text: String,
        images: Vec<PastedImage>,
        chip_elements: Vec<crate::app::agent::ChipElement>,
    ) -> Self {
        Self {
            cursor: text.len(),
            image_counter: images_high_water(&images),
            text,
            images,
            chip_elements,
            image_undo_stash: Vec::new(),
        }
    }

    /// Clone for freeform prefill while this stash remains the session draft.
    /// Omits `staged_temp_path` so freeform Drop cannot delete session temps;
    /// display/send still use `encoded_bytes` / `session_image_path`.
    pub(crate) fn clone_for_live_prefill(&self) -> Self {
        let strip_temp = |img: &PastedImage| {
            let mut c = img.clone();
            c.staged_temp_path = None;
            c
        };
        Self {
            text: self.text.clone(),
            cursor: self.cursor,
            images: self.images.iter().map(strip_temp).collect(),
            chip_elements: self.chip_elements.clone(),
            image_counter: self.image_counter,
            image_undo_stash: self.image_undo_stash.iter().map(strip_temp).collect(),
        }
    }

    pub(crate) fn is_effectively_empty(&self) -> bool {
        self.text.trim().is_empty()
            && self.images.is_empty()
            && self.chip_elements.is_empty()
            && self.image_undo_stash.is_empty()
    }

    pub(crate) fn into_submission(
        mut self,
    ) -> (
        String,
        Vec<PastedImage>,
        Vec<crate::app::agent::ChipElement>,
    ) {
        (
            std::mem::take(&mut self.text),
            std::mem::take(&mut self.images),
            std::mem::take(&mut self.chip_elements),
        )
    }

    pub(crate) fn with_transformed_text(mut self, text: String) -> Self {
        if self.text == text {
            return self;
        }
        let Some(start) = self.text.rfind(&text) else {
            crate::prompt_images::drain_and_cleanup(&mut self.images);
            crate::prompt_images::drain_and_cleanup(&mut self.image_undo_stash);
            self.text = text;
            self.cursor = self.text.len();
            self.chip_elements.clear();
            self.image_counter = 0;
            return self;
        };
        let end = start + text.len();
        let mut image_numbers = std::collections::HashSet::new();
        self.chip_elements.retain_mut(|chip| {
            if chip.range.start < start || chip.range.end > end {
                return false;
            }
            chip.range = chip.range.start - start..chip.range.end - start;
            if chip.kind == KIND_IMAGE
                && let Some(number) = parse_image_display_number(&text[chip.range.clone()])
            {
                image_numbers.insert(number);
            }
            true
        });
        self.images.retain(|image| {
            if image_numbers.contains(&image.display_number) {
                true
            } else {
                crate::prompt_images::cleanup_temp_file(image);
                false
            }
        });
        crate::prompt_images::drain_and_cleanup(&mut self.image_undo_stash);
        self.text = text;
        self.cursor = self.text.len();
        self.image_counter = images_high_water(&self.images);
        self
    }
}

/// Reusable text prompt component.
///
/// Handles text editing, multiline (Shift/Alt-Enter), submit (Enter),
/// and Ctrl-C (clear if non-empty). Delegates mouse events to TextArea
/// which tracks drag state internally (including drag-beyond-edge).
///
/// Does NOT handle: Esc, Tab (focus management), Ctrl-D (quit).
/// Does NOT render: accent line, selection box (caller's job).
pub struct PromptWidget {
    pub textarea: TextArea,
    textarea_state: TextAreaState,
    /// Cached textarea render area from last draw (for mouse coordinate mapping).
    textarea_area: Rect,
    /// @-completion file search state.
    pub file_search: FileSearchState,
    /// Pending request to open the line viewer.
    /// Set by accept_file_search_for_viewer / Ctrl-L / : triggers, consumed by AgentView.
    pub(crate) pending_viewer_request: Option<ViewerRequest>,
    /// Prompt history panel state: search mode (`/history`) or
    /// browse mode (Up on an empty prompt).
    pub history_search: HistorySearchState,
    /// Slash command controller -- derives completion state from text + cursor.
    /// Owned by the prompt; `AgentView` calls methods, never reaches in directly.
    pub(crate) slash_controller: crate::slash::SlashController,
    /// Slash command completion snapshot (for dropdown rendering).
    pub(crate) slash_state: crate::slash::SlashState,
    /// Mouse-hovered slash dropdown item index (`None` = no hover).
    pub(crate) slash_hovered: Option<usize>,
    /// Last input delta for the flight recorder (read by AgentView after handle_key).
    pub(crate) last_input_delta: crate::input_log::LastInputDelta,
    /// Live preview state for slash commands that support it.
    /// Stores the display text of the previously-active value so we can
    /// revert on Esc. `None` = no preview in progress.
    pub(crate) slash_preview_original: Option<String>,

    /// Shell command suggestion controller (ghost text + progressive matching).
    pub(crate) suggestions: SuggestionController,

    /// Predicted-next-prompt controller (tab autocomplete ghost text).
    pub(crate) prompt_suggestion: crate::views::prompt_suggestion::PromptSuggestionController,
    /// Per-frame gate for the prompt-suggestion ghost, set by `AgentView` before each draw or key dispatch.
    /// False while a turn is running, in bash/remember input modes, or while editing a queued prompt.
    pub(crate) prompt_suggestion_active: bool,

    // -- Image paste state ---------------------------------------------------
    /// Images attached to the current prompt.
    pub images: Vec<PastedImage>,
    /// Images removed during undo that can be restored on redo.
    image_undo_stash: Vec<PastedImage>,
    /// Image element currently hovered by mouse hit-testing.
    hovered_image_element_id: Option<echo_agent_ratatui_textarea::ElementId>,
    /// Image shown immediately after insertion without moving the edit cursor.
    post_insert_image_preview: Option<(echo_agent_ratatui_textarea::ElementId, usize)>,
    /// Monotonic counter for image display numbering (1-based). Reset on
    /// prompt clear. Only increases within a single prompt lifetime.
    pub image_counter: usize,

    /// Compact mode lowers the paste-chip threshold from 4 lines to 2.
    compact: bool,

    /// Whether the in-app Cmd+A handler is enabled (gated to Ghostty).
    ///
    /// Initialised from `terminal_context().brand` at construction. Only
    /// Ghostty is supported because:
    /// - It's the only terminal in current use that can forward `Cmd+A`
    ///   without requiring the user to unbind on the terminal side
    ///   (`keybind = cmd+a=unbind`) — but that one terminal-side step
    ///   is well-documented and we want the binding to behave on Ghostty.
    /// - Other terminals either silently swallow the key (Apple
    ///   Terminal, default iTerm2) or have idiosyncratic in-terminal
    ///   "Select All" behaviour we don't want to fight.
    ///
    /// Tests can mutate this field directly (it's private to the module
    /// and the test submodule has `use super::*`).
    cmd_a_select_all_enabled: bool,

    /// Detects user-initiated wipes of a substantial draft (undo tip).
    clear_detector: crate::tips::clear_detector::ClearDetector,
    /// Gate for the per-keystroke undo-tip clear detector. Defaults off so the
    /// detector never runs until the resolved gate is propagated from `AppView`.
    contextual_hint_undo: bool,
    /// Gate for the per-keystroke plan-nudge keyword scan. Independent of
    /// `contextual_hint_undo` so each tip can be toggled on its own.
    contextual_hint_plan_mode: bool,
    /// One-shot: the last `handle_key` wiped a substantial draft.
    undo_tip_fire: bool,
    /// One-shot: the last `handle_key` crossed the draft into mentioning a
    /// planning keyword (rising edge — see `handle_key`).
    plan_nudge_fire: bool,
    /// One-shot: the last `handle_key` accepted an @-file completion, which
    /// can shrink a long `@query` into a short ref/chip — a big shrink that
    /// is NOT a user wipe, so the clear detector must skip observing it (it
    /// resyncs on the next genuine edit).
    completion_accepted: bool,
}

/// Prefix display width (`"❯ "` or `"> "` — both 2 columns).
const PREFIX_WIDTH: u16 = crate::glyphs::PROMPT_ARROW_WIDTH;

impl PromptWidget {
    /// Create a new empty prompt widget with system clipboard support.
    pub fn new_with_cwd(cwd: &Path) -> Self {
        let theme = Theme::current();
        let mut textarea = TextArea::new();
        textarea.set_clipboard_provider(Box::new(SystemClipboard));
        textarea.set_tab_width(crate::appearance::tab_width());
        // Prompt has a light bg — scrollbar track blends with it,
        // thumb uses the darker base bg for contrast.
        textarea.scrollbar_track_style = Style::default().bg(theme.bg_base);
        textarea.scrollbar_thumb_style = Style::default()
            .fg(theme.selection_border)
            .bg(theme.bg_base);
        textarea.scrollbar_padding = 1;
        Self {
            textarea,
            textarea_state: TextAreaState::default(),
            textarea_area: Rect::default(),
            file_search: FileSearchState::new(cwd),
            pending_viewer_request: None,
            history_search: HistorySearchState::new(),
            slash_controller: crate::slash::SlashController::with_builtins(cwd.to_path_buf()),
            slash_state: crate::slash::SlashState::default(),
            slash_hovered: None,
            last_input_delta: crate::input_log::LastInputDelta::default(),
            slash_preview_original: None,
            suggestions: SuggestionController::new(),
            prompt_suggestion: crate::views::prompt_suggestion::PromptSuggestionController::new(),
            prompt_suggestion_active: false,
            images: Vec::new(),
            image_undo_stash: Vec::new(),
            hovered_image_element_id: None,
            post_insert_image_preview: None,
            image_counter: 0,
            compact: false,
            cmd_a_select_all_enabled: cmd_a_select_all_supported(
                crate::terminal::terminal_context().brand,
            ),
            clear_detector: Default::default(),
            contextual_hint_undo: false,
            contextual_hint_plan_mode: false,
            undo_tip_fire: false,
            plan_nudge_fire: false,
            completion_accepted: false,
        }
    }

    /// Create a new empty prompt widget (no file search — for tests/fallback).
    pub fn new() -> Self {
        Self::new_with_cwd(Path::new("."))
    }

    /// Propagate the resolved per-tip gates (undo tip + plan nudge) from
    /// `AppView`. When both are off, `handle_key` skips the per-keystroke scan
    /// entirely; otherwise each detector runs only when its own gate is on.
    pub fn set_contextual_hints(&mut self, undo: bool, plan_mode: bool) {
        self.contextual_hint_undo = undo;
        self.contextual_hint_plan_mode = plan_mode;
    }

    pub fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
    }

    /// Current compact flag (the derived render value fanned out by
    /// `AppView::apply_effective_compact`).
    pub fn compact(&self) -> bool {
        self.compact
    }

    /// Align textarea tab expansion/display with global appearance (config apply / hot-reload).
    pub fn sync_tab_width_from_appearance(&mut self) {
        self.textarea.set_tab_width(crate::appearance::tab_width());
    }

    /// Set the ghost text for shell command suggestions.
    ///
    /// Source is set to [`SuggestionSource::None`]. Use
    /// `suggestions.set_ghost()` directly for source-aware callers.
    pub fn set_ghost_text(&mut self, text: Option<String>) {
        match text {
            Some(t) if !t.is_empty() => {
                self.suggestions.set_ghost(t, SuggestionSource::None);
            }
            _ => self.suggestions.clear_ghost(),
        }
    }

    /// Whether non-empty ghost text is set and would render.
    pub fn has_ghost_text(&self) -> bool {
        self.suggestions.has_ghost()
    }

    /// Accept ghost text (full or one word). Appends the accepted portion
    /// to the textarea. Returns `None` if no ghost is active or cursor is
    /// not at end-of-text.
    pub fn accept_ghost(&mut self, mode: AcceptMode) -> Option<String> {
        if self.cursor() != self.text().len() {
            return None;
        }
        let accepted = self.suggestions.accept_ghost(mode)?;
        self.textarea.insert_str(&accepted);
        self.update_file_search_context();
        Some(accepted)
    }

    /// Dismiss ghost text without accepting it.
    pub fn clear_ghost(&mut self) {
        self.suggestions.clear_ghost();
    }

    // -- Predicted-next-prompt suggestion (tab autocomplete) -----------------

    /// The prompt-suggestion ghost to render for the current text, if the
    /// per-frame gate is open and no other completion UI owns the row.
    /// Requires the cursor at end-of-text so the ghost visually continues
    /// the typed text.
    pub fn prompt_suggestion_ghost(&self) -> Option<&str> {
        // Cheap checks first — most frames have no suggestion loaded.
        if !self.prompt_suggestion_active
            || !self.prompt_suggestion.has_suggestion()
            || self.suggestions.has_ghost()
            || self.file_search_visible()
            || self.history_search.is_active()
            || self.cursor() != self.text().len()
        {
            return None;
        }
        let slash = self.slash_state.snapshot();
        if slash.active || slash.inline_ghost.is_some() {
            return None;
        }
        self.prompt_suggestion.ghost_for(self.text())
    }

    /// Whether the prompt-suggestion ghost is currently visible (drives the
    /// key intercept and the shortcuts-bar hint).
    pub fn prompt_suggestion_visible(&self) -> bool {
        self.prompt_suggestion_ghost().is_some()
    }

    /// Accept the prompt suggestion: insert the remainder at end-of-text and
    /// move the cursor after it. Returns `true` when text was inserted.
    pub fn accept_prompt_suggestion(&mut self) -> bool {
        if self.prompt_suggestion_ghost().is_none() {
            return false;
        }
        let text = self.text().to_owned();
        let Some(rest) = self.prompt_suggestion.accept(&text) else {
            return false;
        };
        // Accepting rewrites the token — an unrelated highlight must not survive it.
        self.textarea.clear_selection();
        self.textarea.insert_str(&rest);
        self.update_file_search_context();
        true
    }

    /// Try progressive matching against the new text. Returns `true` if
    /// the ghost was trimmed (no network request needed).
    pub fn try_progressive_match(&mut self, new_text: &str) -> bool {
        self.suggestions.try_progressive_match(new_text)
    }

    // -- Completion dropdown ------------------------------------------------

    /// Whether the completion dropdown is currently open.
    pub fn completion_dropdown_open(&self) -> bool {
        self.suggestions.dropdown.open
    }

    /// Whether any prompt-anchored dropdown is open: `@`-file search, slash
    /// command, shell completion, or history search. All four render in the
    /// row directly above the prompt input, so an overlay gated above the
    /// prompt (e.g. the ephemeral tip) must treat an open dropdown as an
    /// occluder. Keep this in sync with the dropdown render arms in
    /// `AgentView::draw`.
    pub fn any_dropdown_open(&self) -> bool {
        self.file_search_visible()
            || self.slash_open()
            || self.completion_dropdown_open()
            || self.history_search.is_active()
    }

    /// Open the completion dropdown if items are available.
    pub fn completion_dropdown_open_if_available(&mut self) -> bool {
        if !self.suggestions.dropdown.items.is_empty() {
            self.suggestions.dropdown.open = true;
            true
        } else {
            false
        }
    }

    /// Close the completion dropdown.
    pub fn completion_dropdown_close(&mut self) {
        self.suggestions.dropdown.close();
    }

    /// Move the completion dropdown selection (Up = -1, Down = +1), wrapping.
    pub fn completion_dropdown_move(&mut self, delta: isize) {
        self.suggestions.dropdown.move_selection(delta);
    }

    /// Scroll the completion dropdown selection by `delta`, clamping at the
    /// ends (no wrap-around). Used for mouse-wheel and page-key scrolling.
    pub fn completion_dropdown_scroll(&mut self, delta: isize) {
        self.suggestions.dropdown.scroll_selection(delta);
    }

    /// Accept the selected completion, resolved against the current draft
    /// (stale generations refuse and close — see
    /// [`SuggestionController::accept_completion`]). Also clears ghost text
    /// since the accepted completion replaces it.
    pub fn completion_dropdown_accept(&mut self) -> Option<CompletionSplice> {
        let result = self.suggestions.accept_completion(self.textarea.text());
        if result.is_some() {
            self.suggestions.clear_ghost();
        }
        result
    }

    /// Write a resolved accept into the textarea. Returns whether text was
    /// written (`Stale` is a draft-preserving no-op); cursor lands after the
    /// insert.
    pub fn apply_completion_splice(&mut self, splice: CompletionSplice) -> bool {
        // Accepting rewrites the token — an unrelated highlight must not survive it.
        self.textarea.clear_selection();
        match splice {
            CompletionSplice::WholeLine(line) => {
                self.set_text(&line);
                self.set_cursor(line.len());
                true
            }
            CompletionSplice::Token(range, replacement) => {
                if self.completion_range_clips_element(&range) {
                    return false;
                }
                let start = range.start;
                self.textarea.replace_range(range, &replacement);
                self.textarea.set_cursor(start + replacement.len());
                self.update_file_search_context();
                true
            }
            CompletionSplice::Stale => false,
        }
    }

    /// Write a `TabAction::Fill` over the typed token (bash's first Tab).
    /// The pair was validated against this draft by `tab_decision` in the
    /// same key/landing event, so it applies directly; cursor lands after
    /// the fill. Returns whether the fill was written — a range clipping an
    /// atomic element is declined, and the caller must NOT treat a declined
    /// fill like typing (no re-fetch: that loop would spin on every Tab).
    pub fn apply_completion_fill(&mut self, range: std::ops::Range<usize>, fill: &str) -> bool {
        if self.completion_range_clips_element(&range) {
            return false;
        }
        let start = range.start;
        self.textarea.replace_range(range, fill);
        self.textarea.set_cursor(start + fill.len());
        self.update_file_search_context();
        true
    }

    /// Whether accepting the selected completion would splice a range that
    /// clips an atomic element — the write path would decline it AFTER the
    /// accept consumed the item, so callers probe first and degrade to
    /// opening the dropdown instead.
    pub fn completion_accept_would_clip_element(&self) -> bool {
        match self
            .suggestions
            .peek_completion_splice(self.textarea.text())
        {
            Some(CompletionSplice::Token(range, _)) => self.completion_range_clips_element(&range),
            _ => false,
        }
    }

    /// Whether a completion range overlaps an atomic prompt element (paste
    /// chip, file ref, image chip). Completion ranges are computed over the
    /// plain request text, but `TextArea::replace_range` expands any
    /// element overlap to the WHOLE element — accepting such a range would
    /// swallow the entire chip (paste data loss). Treated as a stale no-op.
    fn completion_range_clips_element(&self, range: &std::ops::Range<usize>) -> bool {
        self.textarea
            .elements()
            .iter()
            .any(|e| e.range.start < range.end && e.range.end > range.start)
    }

    /// Set the hovered completion dropdown item. Returns `true` if changed.
    pub fn set_completion_hovered(&mut self, index: Option<usize>) -> bool {
        let clamped = index.and_then(|i| {
            if i < self.suggestions.dropdown.items.len() {
                Some(i)
            } else {
                None
            }
        });
        let changed = clamped != self.suggestions.dropdown.hovered;
        self.suggestions.dropdown.hovered = clamped;
        changed
    }

    /// Select the hovered completion item (keyboard selection = hovered).
    pub fn select_completion_hovered(&mut self) -> bool {
        if let Some(idx) = self.suggestions.dropdown.hovered {
            self.suggestions.dropdown.selected = idx;
            true
        } else {
            false
        }
    }

    /// Get the current text.
    pub fn text(&self) -> &str {
        self.textarea.text()
    }

    /// Get the current cursor position (byte offset into text).
    pub fn cursor(&self) -> usize {
        self.textarea.cursor()
    }

    /// Clear the undo/redo history (see [`TextArea::clear_history`]).
    /// Pair with `set_text("")` when reusing the widget for a new
    /// logical context so an undo can't resurrect the previous draft.
    pub fn clear_history(&mut self) {
        self.textarea.clear_history();
    }

    /// Set the cursor position (byte offset), clamped to text length.
    pub fn set_cursor(&mut self, pos: usize) {
        self.post_insert_image_preview = None;
        self.textarea.set_cursor(pos);
    }

    /// Move the current prompt state into a snapshot for later restoration.
    pub fn stash(&mut self) -> StashedPrompt {
        let chip_elements = self
            .textarea
            .elements()
            .iter()
            .map(|element| crate::app::agent::ChipElement {
                range: element.range.clone(),
                kind: element.kind,
                display: element.display.clone(),
            })
            .collect();
        let images = self.drain_images();
        StashedPrompt {
            text: self.text().to_owned(),
            cursor: self.cursor(),
            images,
            chip_elements,
            image_counter: self.image_counter,
            image_undo_stash: std::mem::take(&mut self.image_undo_stash),
        }
    }

    /// Restore a previous [`StashedPrompt`], including image ownership.
    pub fn restore(&mut self, mut stash: StashedPrompt) {
        let text = std::mem::take(&mut stash.text);
        let cursor = stash.cursor;
        let images = std::mem::take(&mut stash.images);
        let chip_elements = std::mem::take(&mut stash.chip_elements);
        let image_counter = stash.image_counter;
        let image_undo_stash = std::mem::take(&mut stash.image_undo_stash);
        if text.is_empty() {
            self.set_text("");
        } else {
            crate::prompt_images::drain_and_cleanup(&mut self.images);
            crate::prompt_images::drain_and_cleanup(&mut self.image_undo_stash);
            crate::prompt_images::reset_counter(&mut self.image_counter);
            self.set_text(&text);
        }
        self.restore_chip_elements(&chip_elements);
        self.set_images(images);
        self.image_counter = self.image_counter.max(image_counter);
        self.image_undo_stash = image_undo_stash;
        self.set_cursor(cursor);
        self.update_file_search_context();
    }

    /// Set the text content.
    ///
    /// When `text` is empty this is a full prompt reset: image-side state,
    /// the image counter, slash completion state, and any in-progress
    /// preview are all cleared so the next prompt starts fresh.
    ///
    /// **Image lifecycle contract.** Non-empty replacements that
    /// don't contain any `[Image #N]` placeholders also clear the
    /// image lists and counter, since orphan `PastedImage` records
    /// without matching chips can't be reached by the user. Callers
    /// that restore an in-flight prompt after `set_text` must also restore
    /// its chip elements and images. Overlay contexts should use [`Self::stash`]
    /// and [`Self::restore`]. The `image_counter` is preserved when chips
    /// remain so numbering stays monotonic within a prompt lifetime.
    pub fn set_text(&mut self, text: &str) {
        self.post_insert_image_preview = None;
        self.hovered_image_element_id = None;
        if text.is_empty() {
            // Revert any in-progress preview while the command text is
            // still in the textarea (parse_invocation needs it).
            self.slash_cancel_preview();
        }
        self.textarea.set_text(text);
        // Suggestion state (ghost, dropdown items, in-flight fetches) is
        // tied to a SPECIFIC draft: every wholesale content swap — empty or
        // not — invalidates it, so nothing stale survives into (or lands
        // over) the new text.
        self.suggestions.invalidate_draft();
        if text.is_empty() {
            // Split "drain Vec" from "reset counter" so the
            // counter-reset is a single explicit line, not two
            // redundant writes hidden in `clear()`.
            crate::prompt_images::drain_and_cleanup(&mut self.images);
            crate::prompt_images::drain_and_cleanup(&mut self.image_undo_stash);
            crate::prompt_images::reset_counter(&mut self.image_counter);
            self.slash_state
                .replace(crate::slash::SlashSnapshot::default());
            // Safety net: ensure preview state is cleared even if
            // slash_cancel_preview was a no-op (e.g. command not
            // found after registry changes).
            self.slash_preview_original = None;
        } else if !chip_placeholder_regex().is_match(text) {
            // Non-empty replacement with no chip placeholder match —
            // any retained `PastedImage` records are now orphan (no
            // chip in the buffer references them). Drop them to
            // keep `drain_images()` honest and to release encoded
            // bytes. Counter is reset since no chips survive.
            crate::prompt_images::drain_and_cleanup(&mut self.images);
            crate::prompt_images::drain_and_cleanup(&mut self.image_undo_stash);
            crate::prompt_images::reset_counter(&mut self.image_counter);
        }
        self.update_file_search_context();
    }

    /// [`Self::set_text`] unless the buffer already holds exactly `text`.
    ///
    /// Skipping the no-op swap keeps chip elements, images, and undo history
    /// intact when a surface reloads an unchanged draft (the question view's
    /// freeform slots); any real content change takes the normal reset path.
    pub fn set_text_preserving(&mut self, text: &str) {
        if self.text() != text {
            self.set_text(text);
        }
    }

    /// Append plain text at the end without replacing existing chip elements.
    pub fn append_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.post_insert_image_preview = None;
        self.hovered_image_element_id = None;
        let end = self.textarea.text().len();
        self.set_cursor(end);
        self.textarea.insert_str(text);
        self.update_file_search_context();
    }

    // -- Slash command state sync -------------------------------------------

    pub fn set_slash_current_title(&mut self, title: Option<String>) {
        self.slash_controller.set_current_title(title);
    }

    pub fn slash_current_title(&self) -> Option<&str> {
        self.slash_controller.current_title()
    }

    /// Refresh the slash snapshot from current text + cursor.
    ///
    /// Called by `AgentView` after every `PromptEvent::Edited`, and when
    /// `current_title` changes while the dropdown is already open (so the
    /// `/rename` ghost is not left stale until the next keystroke).
    /// This is the only way to update the slash snapshot -- `AgentView`
    /// never touches `slash_controller` or `slash_state` directly.
    ///
    pub fn refresh_slash(&mut self, models: &crate::acp::model_state::ModelState) {
        let was_in_args = {
            let snap = self.slash_state.snapshot();
            snap.open && !snap.cursor_in_command && !snap.matches.is_empty()
        };

        // Element placeholder text (e.g. `[Image #1]`) in the buffer
        // confuses the slash system -- spaces inside placeholders break
        // command parsing. Build a clean text with all element content
        // removed so the slash system only sees user-typed text.
        let raw_text = self.textarea.text();
        let (clean_text, clean_cursor) =
            strip_all_elements(raw_text, self.textarea.cursor(), &self.textarea);
        self.slash_controller
            .refresh(&self.slash_state, &clean_text, clean_cursor, models);

        self.slash_state.update(|snap| {
            remap_slash_snapshot_to_raw(snap, raw_text, &self.textarea);
        });

        let snap = self.slash_state.snapshot();
        let now_in_args = snap.open && !snap.cursor_in_command && !snap.matches.is_empty();

        if now_in_args {
            // Trigger preview for the auto-selected first suggestion when
            // transitioning into args mode, or when the suggestion list changes.
            self.slash_preview_current_selection();
        } else if was_in_args && !now_in_args {
            // Left the args phase (e.g., user deleted text) — cancel preview.
            self.slash_cancel_preview();
        }
    }

    /// Sync ACP-advertised commands and the agent toolset into the slash
    /// registry, then refresh the snapshot.
    ///
    /// Called by `AgentView` when `available_commands_generation` changes.
    /// Ordering guarantee: registry sync happens before snapshot refresh
    /// (no stale data in the dropdown). `tools = None` means the toolset
    /// is not yet known; tool-gated commands stay visible.
    pub fn sync_acp_commands(
        &mut self,
        commands: &[agent_client_protocol::AvailableCommand],
        tools: Option<&std::collections::HashSet<String>>,
        models: &crate::acp::model_state::ModelState,
    ) {
        // Single rebuild_triggers() per generation bump: set_acp_state
        // batches both mutations, so we don't pay two trigger rebuilds
        // when the toolset arrived alongside fresh ACP commands.
        let registry = self.slash_controller.registry_mut();
        registry.set_acp_state(commands, tools.cloned());
        self.refresh_slash(models);
    }

    /// Suppress session-scoped slash commands (`/compact`, `/fork`,
    /// `/rewind`, …) from this prompt's completion.
    ///
    /// Used by the agent dashboard's dispatch input, which is
    /// session-less and should only offer pager-global commands.
    pub fn hide_session_scoped_commands(&mut self) {
        self.slash_controller.set_hide_session_scoped(true);
    }

    /// Record the process's effective screen mode so slash visibility gates
    /// (`/minimal`, `/fullscreen`) see it. Injected wherever prompts are
    /// created — the mode is fixed for the process lifetime.
    pub(crate) fn set_screen_mode(&mut self, mode: crate::app::ScreenMode) {
        self.slash_controller.set_screen_mode(mode);
    }

    /// Adopt the shared slash MRU store so this prompt's completion shares
    /// command recency with other agent prompts and the dashboard dispatch.
    /// Injected by `AppView`, which owns the single process store.
    pub(crate) fn adopt_slash_mru(
        &mut self,
        mru: std::rc::Rc<std::cell::RefCell<crate::slash::mru::SlashMru>>,
    ) {
        self.slash_controller.set_mru(mru);
    }

    /// Adopt the shared per-command tag map so this prompt's slash dropdown
    /// renders the same tags as other agent prompts and the dashboard dispatch.
    /// Injected by `AppView`, which owns the single process map.
    pub(crate) fn adopt_command_tags(
        &mut self,
        command_tags: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, String>>>,
    ) {
        self.slash_controller.set_command_tags(command_tags);
    }

    pub(crate) fn set_recap_visible(&mut self, visible: bool) {
        self.slash_controller
            .registry_mut()
            .set_recap_visible(visible);
    }

    pub(crate) fn set_voice_visible(&mut self, visible: bool) {
        self.slash_controller
            .registry_mut()
            .set_voice_visible(visible);
    }

    /// Gate `/auto` on the auto permission-mode feature.
    /// See [`crate::slash::SlashController::set_auto_mode_available`].
    pub(crate) fn set_auto_mode_available(&mut self, available: bool) {
        self.slash_controller.set_auto_mode_available(available);
    }

    /// Replace the restricted slash-command deny list (see
    /// [`crate::slash::registry::CommandRegistry::set_restricted_commands`]).
    pub(crate) fn set_restricted_commands(&mut self, names: &[String]) {
        self.slash_controller
            .registry_mut()
            .set_restricted_commands(names);
    }

    /// Access the current slash snapshot (for dropdown rendering).
    pub fn slash_snapshot(&self) -> crate::slash::SlashSnapshot {
        self.slash_state.snapshot()
    }

    /// Whether the slash dropdown is currently open.
    pub fn slash_open(&self) -> bool {
        self.slash_state.snapshot().open
    }

    /// Close the slash dropdown.
    pub fn slash_close(&mut self) {
        self.slash_state.close();
    }

    /// Move the slash dropdown selection (Up = -1, Down = +1), wrapping around.
    pub fn slash_move_selection(&mut self, delta: isize) {
        self.slash_controller
            .move_selection(&self.slash_state, delta);
    }

    /// Scroll the slash dropdown selection by `delta`, clamping at the ends
    /// (no wrap-around). Used for mouse-wheel and page-key scrolling.
    pub fn slash_scroll_selection(&mut self, delta: isize) {
        self.slash_controller
            .scroll_selection(&self.slash_state, delta);
    }

    /// Accept the currently selected slash completion.
    ///
    /// Replaces the appropriate text range (command or args) with the
    /// selected row's `insert_text`, then refreshes the snapshot.
    /// Returns `true` if a completion was accepted, `false` if nothing to accept.
    pub fn accept_slash_completion(
        &mut self,
        models: &crate::acp::model_state::ModelState,
    ) -> bool {
        let snap = self.slash_state.snapshot();
        let Some(row) = snap.selection() else {
            return false;
        };
        // Accepting rewrites the token — an unrelated highlight must not survive it.
        self.textarea.clear_selection();
        let insert_text = row.insert_text.clone();
        let record_mru = snap.cursor_in_command;
        let mru_record = if record_mru {
            Some((snap.query.clone(), row.command_name().to_string()))
        } else {
            None
        };
        let text_len = self.textarea.text().len();

        let applied = if snap.cursor_in_command {
            // Replace the command range (e.g., "/mo" → "/model ").
            if let Some(ref range) = snap.command_range {
                // Guard: range must be valid for the current text.
                if range.end <= text_len
                    && self.textarea.text().is_char_boundary(range.start)
                    && self.textarea.text().is_char_boundary(range.end)
                {
                    // The row's trailing space is its args separator; absorb
                    // an existing plain-text one so accepting mid-token in
                    // `/mod echo-agent-4` yields `/model echo-agent-4`, not `/model  echo-agent-4`
                    // — absorb (rather than trim the insert) so the cursor
                    // lands after the separator, in the args phase. Never
                    // absorb an element's byte: a chip's leading space is chip
                    // data, and replace_range expands any overlap to the whole
                    // element (the absorb would swallow the chip).
                    let next_is_plain_space = self.textarea.text()[range.end..].starts_with(' ')
                        && !self
                            .textarea
                            .elements()
                            .iter()
                            .any(|e| e.range.contains(&range.end));
                    let end = if insert_text.ends_with(' ') && next_is_plain_space {
                        range.end + 1
                    } else {
                        range.end
                    };
                    self.textarea.replace_range(range.start..end, &insert_text);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            // Replace the args range (e.g., "gr" → "EchoAgent 4 Fast").
            if let Some(ref range) = snap.args_range
                && range.end <= text_len
                && self.textarea.text().is_char_boundary(range.start)
                && self.textarea.text().is_char_boundary(range.end)
            {
                self.textarea.replace_range(range.clone(), &insert_text);
                true
            } else {
                false
            }
        };

        if !applied {
            return false;
        }

        if let Some((prefix, name)) = mru_record {
            self.slash_controller.record_command_use(&prefix, &name);
        }

        // Refresh snapshot after text mutation.
        self.refresh_slash(models);
        true
    }

    /// Set the hovered slash dropdown item index. Returns `true` if changed.
    pub fn set_slash_hovered(&mut self, index: Option<usize>) -> bool {
        let snap = self.slash_state.snapshot();
        let clamped = index.and_then(|i| {
            if i < snap.matches.len() {
                Some(i)
            } else {
                None
            }
        });
        let changed = clamped != self.slash_hovered;
        self.slash_hovered = clamped;
        changed
    }

    /// Get the hovered slash dropdown item index.
    pub fn slash_hovered(&self) -> Option<usize> {
        self.slash_hovered
    }

    /// Select the hovered slash item (set keyboard selection = hovered).
    /// Returns `true` if a valid hover was applied.
    pub fn select_slash_hovered(&mut self) -> bool {
        if let Some(idx) = self.slash_hovered {
            self.slash_state.update(|s| s.selected = idx);
            true
        } else {
            false
        }
    }

    // -- Slash preview -------------------------------------------------------

    /// Trigger live preview for the currently selected slash arg suggestion.
    ///
    /// Called after `slash_move_selection` when the dropdown is in the args
    /// phase of a command that supports preview. On the first call, captures
    /// the current state so it can be reverted on cancel.
    pub fn slash_preview_current_selection(&mut self) {
        let snap = self.slash_state.snapshot();
        if snap.cursor_in_command || snap.matches.is_empty() {
            return;
        }

        // Resolve the command for this input.
        let text = self.textarea.text().to_string();
        let Some(invocation) = crate::slash::parse_invocation(&text) else {
            return;
        };
        let Some(command) = self.slash_controller.registry().get(invocation.token) else {
            return;
        };
        if !command.supports_preview() {
            return;
        }

        // Capture original value on first preview call via the command's
        // preview_state() method (e.g., current theme name for /theme).
        if self.slash_preview_original.is_none() {
            self.slash_preview_original = command.preview_state();
        }

        // Preview the highlighted suggestion.
        if let Some(row) = snap.selection() {
            command.preview_arg(&row.insert_text);
        }
    }

    /// Cancel an in-progress slash preview, reverting to the original state.
    ///
    /// Called when the user presses Esc on the dropdown or clears the input.
    pub fn slash_cancel_preview(&mut self) {
        let Some(original) = self.slash_preview_original.take() else {
            return;
        };

        // Resolve the command to call cancel_preview.
        let text = self.textarea.text().to_string();
        if let Some(invocation) = crate::slash::parse_invocation(&text)
            && let Some(command) = self.slash_controller.registry().get(invocation.token)
            && command.supports_preview()
        {
            command.cancel_preview(&original);
        }
    }

    /// Commit the preview (clear the saved original so cancel is a no-op).
    ///
    /// Called when the user accepts the completion (Enter/Tab) to prevent
    /// revert on subsequent dropdown close.
    pub fn slash_commit_preview(&mut self) {
        self.slash_preview_original = None;
    }

    // -- File search --------------------------------------------------------

    /// Poll the file search daemon for new results. Returns `true` if changed.
    pub fn poll_file_search(&mut self) -> bool {
        self.file_search.poll()
    }

    /// Whether the file search dropdown is currently visible.
    pub fn file_search_visible(&self) -> bool {
        self.file_search.is_visible()
    }

    /// Whether the cursor is at a file ref element boundary (for shortcuts bar hint).
    ///
    /// Matches the same positions where `:` or Ctrl-L would trigger:
    /// at element start, at element end, but NOT after the trailing space.
    pub fn file_ref_near_cursor(&self) -> bool {
        let cursor = self.textarea.cursor();
        self.textarea
            .elements()
            .iter()
            .any(|e| e.kind == KIND_FILE_REF && (cursor == e.range.start || cursor == e.range.end))
    }

    /// Parse a file ref element into (path, optional line range).
    ///
    /// Element text is `@path` or `@path:10` or `@path:10-12`.
    /// Returns `(path, None)` or `(path, Some(10..11))` or `(path, Some(10..13))`.
    fn parse_file_ref_element(text: &str) -> (String, Option<std::ops::Range<usize>>) {
        let text = text.strip_prefix('@').unwrap_or(text);
        if let Some(colon_pos) = text.rfind(':') {
            let path = &text[..colon_pos];
            let range_str = &text[colon_pos + 1..];
            if let Some(range) = parse_line_range(range_str) {
                return (path.to_owned(), Some(range));
            }
        }
        (text.to_owned(), None)
    }

    /// Find a file ref element when cursor is on or immediately after it.
    ///
    /// Used by Ctrl-L: works when cursor is at element start, end, or the
    /// position right after element end (e.g., after the trailing space).
    /// Returns `(path, optional_line_range)`.
    pub fn file_ref_element_at_cursor(&self) -> Option<(String, Option<std::ops::Range<usize>>)> {
        let cursor = self.textarea.cursor();
        for elem in self.textarea.elements() {
            if elem.kind != KIND_FILE_REF {
                continue;
            }
            if cursor >= elem.range.start && cursor <= elem.range.end + 1 {
                let text = &self.textarea.text()[elem.range.clone()];
                return Some(Self::parse_file_ref_element(text));
            }
        }
        None
    }

    /// Find a file ref element when cursor is at an element boundary.
    ///
    /// Used by ':' trigger: fires when cursor is at element.range.start
    /// (on the `@` char) or element.range.end (right after the element).
    /// Returns `(path, optional_line_range)`.
    fn file_ref_at_element_boundary(&self) -> Option<(String, Option<std::ops::Range<usize>>)> {
        let cursor = self.textarea.cursor();
        for elem in self.textarea.elements() {
            if elem.kind != KIND_FILE_REF {
                continue;
            }
            if cursor == elem.range.end || cursor == elem.range.start {
                let text = &self.textarea.text()[elem.range.clone()];
                return Some(Self::parse_file_ref_element(text));
            }
        }
        None
    }

    /// Update file search context from current text and cursor position.
    ///
    /// Suppresses the context if the cursor is on or immediately after a
    /// file reference element — the `@` belongs to the atomic block and
    /// the dropdown can't edit it.
    fn update_file_search_context(&mut self) {
        let cursor = self.textarea.cursor();

        // Check if cursor is adjacent to a file ref element (on it or right after it).
        let on_element =
            self.textarea.elements().iter().any(|e| {
                e.kind == KIND_FILE_REF && cursor >= e.range.start && cursor <= e.range.end
            });

        if on_element {
            self.file_search.clear_context();
        } else {
            self.file_search
                .update_context(self.textarea.text(), cursor);
        }
    }

    /// How tall this widget wants to be, given the available width of the
    /// full prompt area (including chrome if enabled).
    ///
    /// Height = vpad_top + textarea rows + vpad_info(1) + info line.
    /// Clamped to max_height.
    pub fn desired_height(
        &self,
        area_width: u16,
        style: &PromptStyle,
        has_info: bool,
        max_height: u16,
    ) -> u16 {
        let content_width = self.content_width(area_width, style);
        let prefix_w = if style.show_prefix { PREFIX_WIDTH } else { 0 };
        let text_width = content_width.saturating_sub(prefix_w);
        // History browse live-populates the composer per selection move;
        // freeze the box at its pre-open one-row height so stepping through
        // entries of different heights doesn't resize the layout per
        // keypress. The first real edit detaches and the box resizes then.
        let text_height = if self.history_search.is_browse() {
            1
        } else {
            self.textarea.desired_height(text_width).max(1)
        };
        let vpad_top = style.vpad_top;
        let info_block = style.info_block(has_info);
        let total = vpad_top + text_height + info_block;
        let min = vpad_top + 1 + info_block;
        total.clamp(min.min(max_height), max_height)
    }

    /// Current vertical scroll offset of the textarea.
    pub fn scroll(&self) -> u16 {
        self.textarea_state.scroll
    }

    /// Set the vertical scroll offset (e.g., restore after collapsed render).
    pub fn set_scroll(&mut self, scroll: u16) {
        self.textarea_state.scroll = scroll;
    }

    /// Last rendered textarea area for mouse hit-testing.
    pub fn textarea_area(&self) -> Rect {
        self.textarea_area
    }

    /// Compute the content width inside the chrome (if any).
    fn content_width(&self, area_width: u16, style: &PromptStyle) -> u16 {
        if style.chrome {
            let accent_w = if style.show_accent_line { 1u16 } else { 0 };
            area_width.saturating_sub(accent_w + style.chrome_pad_left + style.chrome_pad_right)
        } else {
            area_width
        }
    }
}

impl PromptWidget {
    /// Handle a key event for text editing only.
    ///
    /// This does NOT handle send/submit — that's routed through the action
    /// registry and [`try_send()`]. The widget handles:
    /// - File search navigation (when dropdown is visible)
    /// - File search acceptance (Tab/Enter on dropdown)
    /// - Newline insertion (Shift-Enter, Alt-Enter)
    /// - Clear (Ctrl-C with text)
    /// - All readline-style editing via `textarea.input()` (Ctrl-A/E/W/U/K/Y,
    ///   undo/redo, word movement, etc.)
    ///
    /// Wraps the actual handling to feed the undo-tip clear detector with
    /// user-initiated text transitions: programmatic mutations (`set_text`
    /// from submit/queue flows) never pass through here, which is exactly
    /// what keeps them from firing the tip.
    pub fn handle_key(&mut self, key: &KeyEvent) -> PromptEvent {
        self.post_insert_image_preview = None;
        // One-shot signals describe THIS keypress only; values the caller
        // didn't consume must not leak into the next keypress.
        self.undo_tip_fire = false;
        self.plan_nudge_fire = false;
        self.completion_accepted = false;
        // Both tips off (the default): skip the whole per-keystroke scan — no
        // keyword match, no clear-detector bookkeeping.
        if !self.contextual_hint_undo && !self.contextual_hint_plan_mode {
            return self.handle_key_inner(key);
        }
        let before = self.textarea.text().chars().count();
        let had_images = !self.images.is_empty();
        // Read the pre-edit keyword state fresh every key (the matcher is
        // non-allocating) — only when the plan-nudge detector is on. Reading it
        // here — not from a stored latch — is what keeps the rising edge correct
        // for EVERY text writer: a completion-accept, paste, or programmatic
        // restore that already put a keyword in the buffer leaves
        // before == after == true on the next key, so nothing fires, with no
        // per-writer resync to forget.
        let before_kw = self.contextual_hint_plan_mode
            && crate::tips::plan_nudge::prompt_mentions_planning(self.textarea.text());
        let event = self.handle_key_inner(key);
        // Accepting an @-file completion replaces a long `@query` with a short
        // ref/chip — a big shrink, but a completion, not a wipe. Skip the
        // detector entirely on that keypress; it resyncs on the next genuine
        // edit (`before != last_len`) without firing on the stale peak.
        if event == PromptEvent::Edited && !self.completion_accepted {
            let after = self.textarea.text().chars().count();
            // Undo clear-detector only when the undo tip is on.
            if self.contextual_hint_undo {
                let wiped = self.clear_detector.observe_user_edit(before, after);
                // Honesty guards: never advertise a recovery the textarea can't
                // do, and never one that lost image payloads — ctrl+c's
                // `set_text("")` drains both image lists (undo would restore
                // dead chips), while kill-style wipes stash and restore fully.
                let images_lost =
                    had_images && self.images.is_empty() && self.image_undo_stash.is_empty();
                self.undo_tip_fire = wiped && !images_lost && self.textarea.can_undo();
            }
            // Plan-nudge keyword scan only when the plan-mode tip is on.
            if self.contextual_hint_plan_mode {
                self.plan_nudge_fire = self.plan_nudge_fire_for_edit(key, before_kw);
            }
        }
        event
    }

    /// Whether this just-handled `Edited` keypress should fire the plan-nudge
    /// rising edge: a TYPED edit (not a paste chord) that crossed the draft
    /// into mentioning a planning keyword (`before_kw` is the pre-edit state).
    /// Paste chords are excluded so inline paste matches bracketed paste
    /// (neither fires); slash/bash drafts and an open slash dropdown route to a
    /// command, not a planning prompt.
    fn plan_nudge_fire_for_edit(&self, key: &KeyEvent, before_kw: bool) -> bool {
        if crate::input::key::is_paste_key(key) || crate::input::key::is_inline_paste_key(key) {
            return false;
        }
        let text = self.textarea.text();
        !before_kw
            && crate::tips::plan_nudge::prompt_mentions_planning(text)
            && !text.starts_with('/')
            && !text.starts_with('!')
            && !self.slash_open()
    }

    /// Take the one-shot "substantial draft wiped" signal from the last
    /// `handle_key` call (the undo-tip trigger).
    pub(crate) fn take_undo_tip_fire(&mut self) -> bool {
        std::mem::take(&mut self.undo_tip_fire)
    }

    /// Take the one-shot "draft crossed into a planning keyword" signal from
    /// the last `handle_key` call (the plan-nudge trigger).
    pub(crate) fn take_plan_nudge_fire(&mut self) -> bool {
        std::mem::take(&mut self.plan_nudge_fire)
    }

    /// Key handling body — see [`Self::handle_key`] for the contract.
    fn handle_key_inner(&mut self, key: &KeyEvent) -> PromptEvent {
        // Reset flight recorder delta (overwritten if key reaches textarea).
        self.last_input_delta = crate::input_log::LastInputDelta::default();

        // ── File search key handling (when dropdown is visible) ─────────
        if self.file_search.is_visible() {
            match self.handle_file_search_key(key) {
                FileSearchKeyResult::Handled => return PromptEvent::Edited,
                FileSearchKeyResult::Accepted => {
                    self.accept_file_search_result();
                    // Completion accept, not a wipe — keep the undo tip silent.
                    self.completion_accepted = true;
                    return PromptEvent::Edited;
                }
                FileSearchKeyResult::AcceptedNoSpace => {
                    self.accept_file_search_result_no_space();
                    self.completion_accepted = true;
                    return PromptEvent::Edited;
                }
                FileSearchKeyResult::AcceptAndOpenViewer => {
                    // Accept the result as an element, then signal the caller
                    // (AgentView) to open the line viewer. We return Edited;
                    // the caller checks `wants_line_viewer` afterward.
                    self.accept_file_search_for_viewer();
                    self.completion_accepted = true;
                    return PromptEvent::Edited;
                }
                FileSearchKeyResult::Dismissed => {
                    self.file_search.clear_context();
                    return PromptEvent::Edited;
                }
                FileSearchKeyResult::PassThrough => {
                    // Fall through to normal key handling below.
                }
            }
        }

        // Esc/Tab: drop the highlight but decline the press so it still cancels / switches focus.
        // Modified Esc has no structural consumer — it falls through to the
        // textarea catch-all, which clears the highlight WITH a repaint.
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
            || (key.code == KeyCode::Esc && key.modifiers.is_empty())
        {
            self.textarea.clear_selection();
            return PromptEvent::Ignored;
        }

        // ── Ctrl-L / : on element → open line viewer ────────────────────
        // Ctrl-L when cursor is on or adjacent to a file ref element,
        // or ':' typed right at element boundary → open viewer.
        if key!('l', CONTROL).matches(key)
            && let Some((path, initial_range)) = self.file_ref_element_at_cursor()
        {
            self.pending_viewer_request = Some(ViewerRequest {
                path: std::path::PathBuf::from(path),
                initial_range,
            });
            self.textarea.begin_undo_group();
            return PromptEvent::Edited;
        }
        if key!(':').matches(key)
            && let Some((path, initial_range)) = self.file_ref_at_element_boundary()
        {
            self.pending_viewer_request = Some(ViewerRequest {
                path: std::path::PathBuf::from(path),
                initial_range,
            });
            self.textarea.begin_undo_group();
            return PromptEvent::Edited;
        }
        // Not at element boundary — fall through to type ':' normally.

        // ── Normal key handling ─────────────────────────────────────────

        // Newline: Shift/Alt+Enter, or Apple Terminal bare Enter with a
        // newline modifier held (CoreGraphics rescue inside is_mod_enter).
        if crate::input::is_mod_enter(key) {
            self.insert_replacing_selection("\n");
            return PromptEvent::Edited;
        }

        // Clear: Ctrl-C (empty → Ignored so caller can handle)
        if key!('c', CONTROL).matches(key) {
            return if self.textarea.text().is_empty() {
                PromptEvent::Ignored
            } else {
                self.set_text("");
                PromptEvent::Edited
            };
        }

        // Ctrl-V / Cmd-V: intercept to route through handle_paste() for
        // element creation. Without this, textarea.input() would insert
        // multi-line clipboard content inline instead of creating a paste
        // element. Ghostty passes Cmd+V (SUPER) through as a key event
        // when the clipboard has no text content.
        if crate::input::key::is_paste_key(key) {
            if let Some(text) = system_clipboard_get() {
                if !crate::clipboard::clipboard_text_is_pasteable(Some(&text)) {
                    crate::clipboard::log_paste_key_empty_host_clipboard("prompt_widget");
                }
                // handle_paste calls update_file_search_context internally.
                return self.handle_paste(&text);
            }
            crate::clipboard::log_paste_key_empty_host_clipboard("prompt_widget");
            return PromptEvent::Ignored;
        }

        // Ctrl-Shift-V: inline paste — insert clipboard content directly
        // without creating a [Pasted: N lines] element.
        if crate::input::key::is_inline_paste_key(key) {
            if let Some(text) = system_clipboard_get() {
                let text = normalize_cr(&text);
                if text.is_empty() {
                    crate::clipboard::log_paste_key_empty_host_clipboard("prompt_widget_inline");
                    return PromptEvent::Ignored;
                }
                self.insert_replacing_selection(&text);
                return PromptEvent::Edited;
            }
            crate::clipboard::log_paste_key_empty_host_clipboard("prompt_widget_inline");
            return PromptEvent::Ignored;
        }

        // Cmd+A (macOS, Ghostty only): select all text in the prompt
        // textarea.
        //
        // **Gated to Ghostty.** Every other macOS terminal binds
        // `Cmd+A` to its own in-terminal "Select All" by default, and
        // we don't want to compete with their built-in behaviour or
        // surprise users on terminals where the key doesn't even
        // arrive. Ghostty users can unbind it with a one-line config
        // (`keybind = cmd+a=unbind`) — after that, this handler fires
        // and selects the prompt buffer instead of the entire terminal
        // window. See `docs/user-guide/03-keyboard-shortcuts.md`.
        //
        // The gate is read from a struct field initialised from
        // `terminal_context().brand` at construction (see
        // `cmd_a_select_all_supported`). Tests in this module override
        // the field directly to exercise both branches of the gate.
        //
        // Image / paste chips are textarea elements, and
        // `selection_range()` expands selections to element boundaries
        // automatically, so a full-buffer selection naturally covers
        // every chip. Subsequent Backspace / Delete / typing falls
        // through to the textarea's existing selection-aware handlers
        // (which `delete_selection()` + `sync_images_with_textarea()`
        // keep consistent with `self.images`). The `[Image #N: /path]`
        // text in each chip means a follow-on Ctrl+X (cut) preserves
        // the file path through the system clipboard.
        //
        // Ctrl+A is intentionally NOT remapped: the textarea uses it
        // for the Emacs / readline "move cursor to beginning of line"
        // binding, which terminal users rely on.
        if self.cmd_a_select_all_enabled && key!('a', SUPER).matches(key) {
            let len = self.textarea.text().len();
            if len == 0 {
                return PromptEvent::Ignored;
            }
            self.textarea.set_selection(0, len);
            // Move the cursor to the end of the selection so the
            // visible caret matches the typical GUI "head at end"
            // convention after Cmd+A.
            self.textarea.set_cursor(len);
            return PromptEvent::Edited;
        }

        // Everything else: delegate to textarea.
        // Selection is rendered state: selection-only changes must report Edited or no frame is drawn.
        let old_text = self.textarea.text().to_owned();
        let old_cursor = self.textarea.cursor();
        let old_selection = self.textarea.selection_range();
        self.textarea.input(*key);
        let new_cursor = self.textarea.cursor();
        let new_selection = self.textarea.selection_range();
        let changed = self.textarea.text() != old_text
            || new_cursor != old_cursor
            || new_selection != old_selection;
        self.last_input_delta = crate::input_log::LastInputDelta {
            cursor_before: Some(old_cursor),
            cursor_after: Some(new_cursor),
            text_len_before: Some(old_text.len()),
            text_len_after: Some(self.textarea.text().len()),
            had_selection_before: Some(old_selection.is_some()),
            had_selection_after: Some(new_selection.is_some()),
            textarea_changed: Some(changed),
        };
        if changed {
            // Image chips only change with the text — skip the resync on
            // selection/cursor-only changes (held Shift+arrow extends).
            if self.textarea.text() != old_text {
                self.sync_images_with_textarea();
            }
            self.update_file_search_context();
            PromptEvent::Edited
        } else {
            // Backspace/Delete pressed but produced no effect on non-empty text.
            // This is the telemetry hook for diagnosing the "backspace lock" bug.
            let is_backspace_key = matches!(
                key.code,
                KeyCode::Backspace | KeyCode::Delete | KeyCode::Char('\x08' | '\x7f')
            ) || (key.code == KeyCode::Char('h')
                && key.modifiers.contains(KeyModifiers::CONTROL));
            if is_backspace_key && !old_text.is_empty() {
                use echo_agent_telemetry::events::BackspaceNoEffect;
                use echo_agent_telemetry::session_ctx::log_event;
                let evt = BackspaceNoEffect {
                    terminal: crate::terminal::terminal_context().telemetry_snapshot(),
                    key_code: format!("{:?}", key.code),
                    key_modifiers: format!("{:?}", key.modifiers),
                    key_kind: format!("{:?}", key.kind),
                    cursor_pos: old_cursor,
                    text_len: old_text.len(),
                    has_selection: old_selection.is_some(),
                };
                // Structured warn for the product telemetry pipeline.
                tracing::warn!(
                    terminal.brand = %evt.terminal.brand,
                    terminal.multiplexer = %evt.terminal.multiplexer,
                    terminal.is_ssh = evt.terminal.is_ssh,
                    terminal.term_var = %evt.terminal.term_var,
                    terminal.term_version = %evt.terminal.term_version,
                    terminal.term_version_source = %evt.terminal.term_version_source,
                    key.code = %evt.key_code,
                    key.modifiers = %evt.key_modifiers,
                    key.kind = %evt.key_kind,
                    textarea.cursor_pos = evt.cursor_pos,
                    textarea.text_len = evt.text_len,
                    textarea.has_selection = evt.has_selection,
                    "backspace_no_effect"
                );
                // Product analytics event (when telemetry is enabled).
                log_event(evt);
            }
            PromptEvent::Ignored
        }
    }

    // ── File search key dispatch ────────────────────────────────────────

    /// Handle navigation/selection keys when the file search dropdown is visible.
    fn handle_file_search_key(&mut self, key: &KeyEvent) -> FileSearchKeyResult {
        if key!(Up).matches(key)
            || key!('p', CONTROL).matches(key)
            || key!('k', CONTROL).matches(key)
        {
            // Navigation: up.
            self.file_search.move_selection(-1);
            FileSearchKeyResult::Handled
        } else if key!(Down).matches(key)
            || key!('n', CONTROL).matches(key)
            || key!('j', CONTROL).matches(key)
        {
            // Navigation: down.
            self.file_search.move_selection(1);
            FileSearchKeyResult::Handled
        } else if key!(PageUp).matches(key) || key!('u', CONTROL).matches(key) {
            // Page up.
            self.file_search.page_move(-1, 8);
            FileSearchKeyResult::Handled
        } else if key!(PageDown).matches(key) || key!('d', CONTROL).matches(key) {
            // Page down.
            self.file_search.page_move(1, 8);
            FileSearchKeyResult::Handled
        } else if key!(Tab).matches(key) || key!(Enter).matches(key) {
            // Accept.
            if file_search_has_selection(&self.file_search) {
                FileSearchKeyResult::Accepted
            } else {
                FileSearchKeyResult::PassThrough
            }
        } else if key!(Right).matches(key) {
            // Right Arrow: accept the highlighted suggestion without a
            // terminating space, so the user can continue typing (e.g. drill
            // into a nested directory). Falls through to normal cursor
            // movement when the dropdown has no valid selection.
            //
            // `file_search_has_selection` is `selected < result_count`.
            // The selection invariant is maintained by `FileSearchState`:
            // - On context entry / query change: `selected = 0`
            //   (see FileSearchState::update_context).
            // - On daemon poll with new results: `selected` is clamped to
            //   `result_count - 1` (see FileSearchState::poll).
            // So in production the gate is normally true whenever the
            // dropdown is visible, but the explicit check keeps Right safe
            // even if the invariant ever drifts.
            //
            // Modified Right (Shift/Alt/Ctrl) is not matched by `key!(Right)`
            // (which requires NONE modifiers), so selection extension and
            // word-jump fall through to the textarea unchanged.
            if file_search_has_selection(&self.file_search) {
                FileSearchKeyResult::AcceptedNoSpace
            } else {
                FileSearchKeyResult::PassThrough
            }
        } else if key!(':').matches(key) || key!('l', CONTROL).matches(key) {
            // Accept and open line viewer — only for files (not dirs).
            let is_file = self
                .file_search
                .selected_result()
                .is_some_and(|r| !r.is_dir);
            if file_search_has_selection(&self.file_search) && is_file {
                FileSearchKeyResult::AcceptAndOpenViewer
            } else {
                FileSearchKeyResult::PassThrough
            }
        } else if key!(Esc).matches(key) {
            // Dismiss.
            FileSearchKeyResult::Dismissed
        } else {
            // Everything else: pass through to normal handling.
            FileSearchKeyResult::PassThrough
        }
    }

    /// Accept the currently selected file search result.
    ///
    /// - **Dir mode** (query ends with `/`): plain text replacement, stays in
    ///   completion mode for drill-down.
    /// - **File mode**: creates an atomic TextArea element (`KIND_FILE_REF`)
    ///   wrapped in an undo group with a trailing space. Single undo reverts
    ///   the entire completion.
    pub(crate) fn accept_file_search_result(&mut self) {
        self.accept_file_search_result_inner();
    }

    /// Accept the currently selected file search result via Right Arrow.
    ///
    /// Bound to Right Arrow when the dropdown is visible and a suggestion is
    /// highlighted. This is the "Google search bar" behavior: insert the
    /// suggestion and let the user continue refining.
    ///
    /// Behavior:
    /// - **Selected result is a file**: identical to Tab. Inserts the path
    ///   as an atomic `KIND_FILE_REF` element with a trailing space and
    ///   dismisses the dropdown. There is nothing to drill into beneath a
    ///   file, so Right and Tab are intentionally the same here.
    /// - **Selected result is a directory**: replaces the @-token's path
    ///   portion with the full selected path WITHOUT a trailing `/`. The
    ///   missing `/` keeps (or drops the user out of) non-dir-mode, so
    ///   the dropdown re-populates with both files AND directories under
    ///   the new prefix -- the typical "show me what's inside" intent. If
    ///   the user wants to filter back to directories only, they can type
    ///   `/` themselves. The completion context is kept alive.
    pub(crate) fn accept_file_search_result_no_space(&mut self) {
        // Capture context + result before any state mutation.
        let ctx = self.file_search.context().cloned();
        let result = self.file_search.selected_result().cloned();

        let Some(ctx) = ctx else { return };
        let Some(res) = result else { return };

        // Accepting rewrites the token — an unrelated highlight must not survive it.
        self.textarea.clear_selection();

        // File results behave exactly like Tab (insert + trailing space).
        // "Drill down" only makes sense for directories — there is nothing
        // to nest into beneath a file — so for file selections Right and
        // Tab are intentionally identical.
        if !res.is_dir {
            self.accept_file_search_result_inner();
            return;
        }

        // Directory result: drill down. Replace the path portion of the
        // @-token (preserving the optional `!` hidden-mode prefix) with
        // the full selected path, WITHOUT a trailing `/`. Dropping the
        // slash drops the user out of dir-mode (which filters to dirs
        // only), so the dropdown shows files AND dirs matching the new
        // prefix -- typically what the user wants when drilling. If they
        // want to filter back to directories they can type `/` themselves.
        // The @-context is kept alive so update_file_search_context()
        // below re-detects the @-token and refreshes the dropdown.
        let path_str = res.path.to_string();
        let path = normalize_display_path(&path_str);

        // Replace only the path portion of the @-token (preserving `@`
        // and any hidden-mode `!` marker). See `AtContext::path_range`.
        let path_range = ctx.path_range();
        let replacement = path.to_owned();
        let cursor = path_range.start + replacement.len();

        // Wrap in an undo group so a single Ctrl-Z reverts the entire
        // drill-down (matches the Tab-acceptance convention).
        self.textarea.begin_undo_group();
        self.textarea.replace_range(path_range, &replacement);
        self.textarea.set_cursor(cursor);
        self.textarea.end_undo_group();
        // Anchor the drilled path so a whitespace name keeps the dropdown alive.
        self.file_search.set_drill_prefix(Some(replacement));
        self.update_file_search_context();
    }

    /// Shared body for `accept_file_search_result` (Tab) and
    /// `accept_file_search_result_no_space` (Right) when the selected
    /// result is a file or the user is already in dir mode.
    ///
    /// Right Arrow's directory drill-down is handled directly in
    /// `accept_file_search_result_no_space` before this helper is called,
    /// so by the time we get here either the selection is a file or the
    /// user is in dir mode (typed `/`) and Tab is committing the dir.
    fn accept_file_search_result_inner(&mut self) {
        // Capture context + result before any state mutation.
        let ctx = self.file_search.context().cloned();
        let result = self.file_search.selected_result().cloned();

        let Some(ctx) = ctx else { return };
        let Some(res) = result else { return };

        // Accepting rewrites the token — an unrelated highlight must not survive it.
        self.textarea.clear_selection();

        if ctx.is_dir_mode() && res.is_dir {
            // Descending into a selected directory while navigating (dir-mode):
            // try_replace appends `/` and stays open. A file selected in dir-mode
            // falls through to the ref branch (see FileSearchState::try_replace).
            if let Some(r) = self.file_search.try_replace(self.textarea.text()) {
                self.textarea.replace_range(r.range, &r.text);
                self.textarea.set_cursor(r.cursor);
                if r.dismiss {
                    self.file_search.clear_context();
                } else {
                    // Anchor the drilled child so a whitespace name stays open
                    // (reuse the buffer; only a `./` prefix re-allocs).
                    let mut p = res.path.to_string();
                    if let Some(stripped) = p.strip_prefix("./") {
                        p = stripped.to_owned();
                    }
                    self.file_search.set_drill_prefix(Some(p));
                }
            }
        } else {
            // File accepted: create an atomic `KIND_FILE_REF` element +
            // trailing space. Both Tab and Right Arrow reach this branch
            // for file selections because there is nothing nested under
            // a file to drill into.
            let path_str = res.path.to_string();
            let path = normalize_display_path(&path_str);
            let element_text = format!("@{path}");
            let display = file_ref_display(path);

            self.textarea.begin_undo_group();
            self.textarea.replace_range_with_element(
                ctx.range.clone(),
                &element_text,
                KIND_FILE_REF,
                Some(display),
            );
            self.textarea.insert_str(" ");
            self.textarea.end_undo_group();

            self.file_search.clear_context();
        }
        self.update_file_search_context();
    }

    /// Accept the file search result and request the line viewer to open.
    ///
    /// Creates the element (with undo group begun but NOT ended — the viewer
    /// will end or cancel it), clears the dropdown, and sets `pending_viewer_path`
    /// for AgentView to pick up.
    fn accept_file_search_for_viewer(&mut self) {
        let ctx = self.file_search.context().cloned();
        let result = self.file_search.selected_result().cloned();

        let Some(ctx) = ctx else { return };
        let Some(res) = result else { return };

        // Accepting rewrites the token — an unrelated highlight must not survive it.
        self.textarea.clear_selection();

        let path_str = res.path.to_string();
        let path = normalize_display_path(&path_str);
        let element_text = format!("@{path}");
        let display = file_ref_display(path);

        // Begin undo group — will be ended by viewer confirm or cancelled by Esc.
        self.textarea.begin_undo_group();
        self.textarea.replace_range_with_element(
            ctx.range.clone(),
            &element_text,
            KIND_FILE_REF,
            Some(display),
        );

        self.file_search.clear_context();
        self.update_file_search_context();

        // Signal AgentView to open the viewer.
        self.pending_viewer_request = Some(ViewerRequest {
            path: std::path::PathBuf::from(path),
            initial_range: None,
        });
    }

    /// If the character immediately before the cursor is `\`, replace it
    /// with a newline and return `true` (continuation applied).  Returns
    /// `false` when no continuation is needed.
    fn apply_backslash_continuation(&mut self) -> bool {
        let cursor = self.textarea.cursor();
        if cursor > 0 && self.textarea.text().as_bytes().get(cursor - 1) == Some(&b'\\') {
            self.textarea.delete_backward(1);
            self.textarea.insert_str("\n");
            return true;
        }
        false
    }

    /// Shared Enter-routing for any context where bare Enter submits.
    ///
    /// Centralizes three behaviors that every submittable prompt needs:
    /// 1. File-search guard (Enter accepts dropdown selection, not submit)
    /// 2. Apple Terminal CoreGraphics modifier rescue (Shift+Enter arrives
    ///    as bare Enter on terminals without Kitty protocol)
    /// 3. Backslash continuation (`\` + Enter → newline)
    ///
    /// Callers match on the result:
    /// - `NewlineInserted` → return `InputOutcome::Changed`
    /// - `Submit` → perform the context-specific submit action
    /// - `PassThrough` → fall through to `self.handle_key(key)`
    pub fn route_enter(&mut self, key: &KeyEvent) -> EnterOutcome {
        // File-search dropdown is open — Enter accepts the selection,
        // not submit.  Let handle_key() deal with it.
        if self.file_search_visible() {
            return EnterOutcome::PassThrough;
        }

        // Apple Terminal: Shift+Enter arrives as bare Enter because there's
        // no Kitty keyboard protocol.  Poll CoreGraphics for the real
        // modifier state — if Shift/Option/Cmd is physically held, insert
        // a newline instead of submitting.
        if key.code == KeyCode::Enter && crate::input::is_apple_terminal_newline_modifier_held() {
            self.insert_replacing_selection("\n");
            return EnterOutcome::NewlineInserted;
        }

        // Backslash continuation: if the character before the cursor is `\`,
        // replace it with a newline.
        if key.code == KeyCode::Enter
            && key.modifiers.is_empty()
            && self.apply_backslash_continuation()
        {
            return EnterOutcome::NewlineInserted;
        }

        // Bare Enter → submit.
        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
            return EnterOutcome::Submit;
        }

        // Not Enter, or Enter with modifiers (Shift/Alt) — let handle_key()
        // process it (Shift+Enter inserts newline there).
        EnterOutcome::PassThrough
    }

    /// Attempt to send the prompt text.
    ///
    /// Called when the `SendPrompt` action is triggered (via action registry).
    /// Returns `Some(text)` if the prompt was submitted, `None` if rejected
    /// (empty text, backslash continuation).
    ///
    /// Handles:
    /// - Empty/whitespace rejection → returns None
    /// - Backslash continuation: trailing `\` before cursor → replaces with newline
    /// - On success: returns the text and clears the widget
    pub fn try_send(&mut self) -> Option<String> {
        if self.apply_backslash_continuation() {
            return None;
        }

        let text = self.textarea.text().to_string();

        // Reject empty/whitespace-only
        if text.trim().is_empty() {
            return None;
        }

        Some(text)
    }

    /// Whether the prompt is in a state where send would succeed.
    ///
    /// Used for dynamic hint visibility (show "Enter:send" only when sendable).
    pub fn can_send(&self) -> bool {
        let text = self.textarea.text();
        if text.trim().is_empty() {
            return false;
        }
        // Check backslash continuation — trailing `\` means send would do continuation, not send
        let cursor = self.textarea.cursor();
        if cursor > 0 && text.as_bytes().get(cursor - 1) == Some(&b'\\') {
            return false;
        }
        true
    }

    /// Handle a mouse event.
    ///
    /// Forwards to TextArea which handles click-to-place, drag-to-select,
    /// double/triple click, and element interactions. The caller should forward
    /// ALL mouse events (not just those in the prompt area) because TextArea
    /// tracks drag state internally and handles drag-beyond-edge.
    pub fn handle_mouse(&mut self, mouse: &crossterm::event::MouseEvent) -> PromptEvent {
        use echo_agent_ratatui_textarea::{MouseAction, TextElementEventKind};

        self.post_insert_image_preview = None;
        let action = self
            .textarea
            .handle_mouse(*mouse, self.textarea_area, self.textarea_state);

        while let Some(event) = self.textarea.poll_element_event() {
            match event.kind {
                TextElementEventKind::HoverEnter => {
                    let is_image = self
                        .textarea
                        .elements()
                        .iter()
                        .any(|e| e.id == event.id && e.kind == KIND_IMAGE);
                    if is_image {
                        self.hovered_image_element_id = Some(event.id);
                    }
                }
                TextElementEventKind::HoverLeave => {
                    if self.hovered_image_element_id == Some(event.id) {
                        self.hovered_image_element_id = None;
                    }
                }
                TextElementEventKind::Click => {}
            }
        }

        match action {
            MouseAction::CursorPlaced
            | MouseAction::SelectionUpdated
            | MouseAction::SelectionFinished
            | MouseAction::Scrolled => {
                // Cursor moved — update file search context so the dropdown
                // appears/disappears based on the new cursor position.
                self.update_file_search_context();
                PromptEvent::Edited
            }
            MouseAction::Nothing => PromptEvent::Edited,
        }
    }

    /// Handle a paste event.
    ///
    /// Short pastes are inserted inline.  At or above the chip threshold
    /// (4 lines, or 2 in compact mode), or larger than
    /// [`PASTE_CHIP_DISPLAY_BYTES`] bytes, an atomic `[Pasted: N lines]`
    /// element is created.  Trailing newlines in the inline path are
    /// preserved so that split batches (Windows) accumulate correctly.
    /// Repasting a chip's exact content while the cursor is on or right
    /// after it expands that chip instead of inserting a duplicate.
    pub fn handle_paste(&mut self, text: &str) -> PromptEvent {
        if text.is_empty() {
            return PromptEvent::Ignored;
        }
        self.post_insert_image_preview = None;

        let text = normalize_cr(text);
        let text = &text;
        let replacing_selection = self.textarea.selection_range().is_some();

        // Repaste-to-expand: "paste didn't do what I want? paste again."
        // Requires exact byte equality after the same canonicalization the
        // original insertion applied (normalize_cr above + the textarea's
        // tab expansion) — e.g. a trailing-newline difference is a
        // different paste and takes the normal path below.
        if !replacing_selection
            && let Some(elem) = self.paste_element_near_cursor()
            && self.textarea.text()[elem.range.clone()] == *self.textarea.expand_tabs(text)
        {
            let id = elem.id;
            self.expand_element(id);
            return PromptEvent::Edited;
        }

        if replacing_selection {
            self.textarea.begin_undo_group();
            self.textarea.delete_selection();
        }

        // Count actual content lines. `.lines()` treats a trailing newline as
        // optional, so "hello\n" counts as 1 line — exactly what we want.
        let line_count = text.lines().count();

        let chip_min_lines = if self.compact { 2 } else { 4 };
        // Chip when the paste is multi-line past the threshold, or large by byte
        // size. The label prefers size for large pastes (a 1 MB paste reads
        // better as "1.0 MB" than "N lines", regardless of line count); smaller
        // multi-line pastes keep the line count.
        let by_lines = line_count >= chip_min_lines;
        let by_bytes = text.len() > PASTE_CHIP_DISPLAY_BYTES;
        if by_lines || by_bytes {
            let display = if by_bytes {
                paste_chip_display_bytes(text.len())
            } else {
                paste_chip_display(line_count)
            };
            self.textarea
                .insert_element(text, KIND_PASTE, Some(display));
        } else {
            self.textarea.insert_str(text);
        }
        if replacing_selection {
            self.textarea.end_undo_group();
            self.sync_images_with_textarea();
        }

        // Update file search context so @-completion reacts to pasted text.
        // Without this, bracketed paste (terminal-level paste) skips the
        // per-keystroke update_file_search_context() that handle_key does,
        // leaving the fuzzy matcher query stale until the next keystroke.
        self.update_file_search_context();
        PromptEvent::Edited
    }

    // ── Image chip support ──────────────────────────────────────────

    /// Maximum number of image chips allowed in a single prompt (v1).
    pub const IMAGE_CAP: usize = 10;

    /// Toast text shown when an image insertion is rejected because the
    /// prompt already holds [`Self::IMAGE_CAP`] images.
    #[allow(dead_code)]
    pub(crate) fn cap_reached_toast() -> String {
        format!("Image limit reached (max {})", Self::IMAGE_CAP)
    }

    /// Insert a pasted image as an atomic `[Image #N]` chip.
    ///
    /// Returns `Err` with a user-facing message if the image is rejected
    /// (cap reached or too small). The caller should show a toast.
    pub fn insert_image(&mut self, image: PastedImage) -> Result<(), String> {
        if self.images.len() >= Self::IMAGE_CAP {
            return Err(Self::cap_reached_toast());
        }
        // Backend APIs reject images with either side < 8 px.
        const MIN_SIDE: u32 = 8;
        if let Some((w, h)) = image.preview_dimensions()
            && (w < MIN_SIDE || h < MIN_SIDE)
        {
            return Err(format!(
                "Image too small ({w}×{h}). Must be at least {MIN_SIDE}×{MIN_SIDE} pixels."
            ));
        }

        self.image_counter += 1;
        let display_number = self.image_counter;
        let placeholder = crate::prompt_images::display_text(display_number);
        let display_line = chip_line(format!("Image #{display_number}"));

        let buf_len_before = self.textarea.text().len();
        self.textarea.begin_undo_group();
        let element_id = self
            .textarea
            .insert_element(&placeholder, KIND_IMAGE, Some(display_line));
        self.textarea.insert_str(" ");
        self.textarea.end_undo_group();
        self.post_insert_image_preview = Some((element_id, self.textarea.cursor()));
        let buf_len_after = self.textarea.text().len();

        tracing::debug!(
            target: PROMPT_IMAGES_TRACING_TARGET,
            display_number,
            element_id = ?element_id,
            images_len_after = self.images.len() + 1,
            image_counter_after = self.image_counter,
            buf_len_before,
            buf_len_after,
            "prompt_widget: insert_image",
        );

        let mut img = image;
        img.element_id = element_id;
        img.display_number = display_number;
        self.images.push(img);

        Ok(())
    }

    /// Insert text replacing the selection; resyncs images (a selection can swallow chips).
    pub(crate) fn insert_replacing_selection(&mut self, text: &str) {
        let replaced_selection = self.textarea.selection_range().is_some();
        self.textarea.insert_str_replacing_selection(text);
        if replaced_selection {
            self.sync_images_with_textarea();
        }
        self.update_file_search_context();
    }

    fn sync_images_with_textarea(&mut self) {
        use std::collections::HashMap;
        use std::collections::hash_map::Entry;

        let live_image_elements: Vec<(ElementId, std::ops::Range<usize>)> = self
            .textarea
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_IMAGE)
            .map(|e| (e.id, e.range.clone()))
            .collect();
        let stored_len_before = self.images.len();
        let counter_before = self.image_counter;
        let stash_len_before = self.image_undo_stash.len();

        // Primary lookup keyed by `ElementId` (unique by construction).
        // Keying by `display_number` would silently drop one record when two
        // images share a number; the warn branch below makes a future
        // duplicate-id regression visible instead of hiding it.
        let mut stored_by_id: HashMap<ElementId, PastedImage> =
            HashMap::with_capacity(self.images.len());
        for img in self.images.drain(..) {
            let id = img.element_id;
            let display_number = img.display_number;
            match stored_by_id.entry(id) {
                Entry::Vacant(v) => {
                    v.insert(img);
                }
                Entry::Occupied(_) => {
                    tracing::warn!(
                        target: PROMPT_IMAGES_TRACING_TARGET,
                        element_id = ?id,
                        display_number,
                        "sync_images_with_textarea: duplicate element_id \
                         in stored images — dropping later entry to \
                         preserve the first (invariant violation)",
                    );
                }
            }
        }

        // Fallback for elements restored by an undo/redo cycle: the textarea
        // may have re-issued a fresh `ElementId` for the restored chip, so an
        // `element_id` lookup misses. Keying by `display_number` is safe here
        // because the monotonic counter never recycles numbers within a
        // prompt lifetime; a collision-warn keeps any future regression visible.
        let mut stash_by_number: HashMap<usize, PastedImage> =
            HashMap::with_capacity(self.image_undo_stash.len());
        for img in self.image_undo_stash.drain(..) {
            let display_number = img.display_number;
            let element_id = img.element_id;
            match stash_by_number.entry(display_number) {
                Entry::Vacant(v) => {
                    v.insert(img);
                }
                Entry::Occupied(_) => {
                    tracing::warn!(
                        target: PROMPT_IMAGES_TRACING_TARGET,
                        display_number,
                        element_id = ?element_id,
                        "sync_images_with_textarea: duplicate display_number \
                         in undo stash — dropping later entry",
                    );
                }
            }
        }

        let mut synced = Vec::with_capacity(live_image_elements.len());

        for (id, range) in &live_image_elements {
            // Primary: match by element_id (collision-free, stable
            // for non-undo/redo edits).
            if let Some(mut img) = stored_by_id.remove(id) {
                // Refresh display_number from the live buffer text in
                // case the chip was renumbered externally (sanity).
                let parsed = parse_image_display_number(&self.textarea.text()[range.clone()])
                    .unwrap_or(img.display_number);
                img.element_id = *id;
                img.display_number = parsed;
                synced.push(img);
                continue;
            }
            // Fallback: redo path — match by display_number from the
            // stash, then rebind to the freshly-issued `element_id`.
            let display_number =
                parse_image_display_number(&self.textarea.text()[range.clone()]).unwrap_or(0);
            if let Some(mut img) = stash_by_number.remove(&display_number) {
                img.element_id = *id;
                img.display_number = display_number;
                synced.push(img);
            }
        }

        // Images whose elements are gone move to the undo stash so they
        // can be recovered if the user redoes. Explicit resets (set_text,
        // Ctrl-C) clear the stash separately.
        //
        // Cap the stash at `IMAGE_CAP * 2` so a long edit session
        // can't unboundedly grow it. Evicted entries get their temp
        // files cleaned up (mirrors what `clear()` does on submit).
        // Oldest-first eviction by `display_number` keeps the most
        // recent deletions recoverable, since redo typically reverses
        // the most recent undo.
        let mut new_stash: Vec<PastedImage> = stored_by_id
            .into_values()
            .chain(stash_by_number.into_values())
            .collect();
        let stash_cap = Self::IMAGE_CAP * 2;
        if new_stash.len() > stash_cap {
            // Oldest = lowest `display_number` (monotonic within a
            // prompt lifetime). Sort ascending, then drop the front.
            new_stash.sort_by_key(|img| img.display_number);
            let evict_count = new_stash.len() - stash_cap;
            for img in new_stash.drain(..evict_count) {
                // stash eviction
                crate::prompt_images::cleanup_temp_file(&img);
            }
        }
        self.image_undo_stash = new_stash;
        self.images = synced;
        if self
            .hovered_image_element_id
            .is_some_and(|id| !self.images.iter().any(|img| img.element_id == id))
        {
            self.hovered_image_element_id = None;
        }
        if self
            .post_insert_image_preview
            .is_some_and(|(id, _)| !self.images.iter().any(|img| img.element_id == id))
        {
            self.post_insert_image_preview = None;
        }
        // Monotonic high-water mark within a prompt lifetime: the counter only
        // ever advances upward, so deleting a chip never lets a later insert
        // reuse a number that already appeared. The counter is reset only by
        // explicit prompt clears (`set_text("")`, Ctrl+C) which call
        // [`crate::prompt_images::clear`].
        self.image_counter = self.image_counter.max(images_high_water(&self.images));

        // Only log when the stored count or stash size actually changed,
        // otherwise every post-paste keystroke would emit a debug line.
        let stash_len_after = self.image_undo_stash.len();
        let stored_changed = self.images.len() != stored_len_before;
        let stash_changed = stash_len_after != stash_len_before;
        if stored_changed || stash_changed {
            let stashed_numbers: Vec<usize> = self
                .image_undo_stash
                .iter()
                .map(|img| img.display_number)
                .collect();
            tracing::debug!(
                target: PROMPT_IMAGES_TRACING_TARGET,
                live_count = live_image_elements.len(),
                stored_len_before,
                synced_len = self.images.len(),
                counter_before,
                counter_after = self.image_counter,
                stash_len_before,
                stash_len_after,
                ?stashed_numbers,
                "prompt_widget: sync_images_with_textarea",
            );
        }
    }

    /// Look up the image record for the element the cursor is currently on.
    ///
    /// Uses `element_at_cursor()` which matches when `cursor_pos` falls
    /// within `[range.start, range.end)`. For atomic elements the cursor
    /// sits at `range.start` when the user navigates onto the chip (LEFT
    /// arrow jumps there). Positions at or past `range.end` (the trailing
    /// separator space and beyond) are intentionally excluded so the
    /// overlay dismisses as soon as the cursor leaves the chip.
    pub fn image_at_cursor(&self) -> Option<&PastedImage> {
        let elem = self.textarea.element_at_cursor()?;
        if elem.kind != KIND_IMAGE {
            return None;
        }
        self.images.iter().find(|img| img.element_id == elem.id)
    }

    /// Image element the cursor is on, or exactly at its right edge.
    ///
    /// An on-chip match of any kind wins (the chip under the cursor owns
    /// the position), so a cursor sitting on a non-image chip yields
    /// `None` even when an image chip ends at the cursor.
    fn image_element_near_cursor(&self) -> Option<&echo_agent_ratatui_textarea::TextElement> {
        if let Some(elem) = self.textarea.element_at_cursor() {
            return (elem.kind == KIND_IMAGE).then_some(elem);
        }
        let cursor = self.textarea.cursor();
        // Right edge only — not the spacer after (`range.end + 1`).
        self.textarea
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_IMAGE)
            .find(|e| e.range.end == cursor)
    }

    /// Image record for the preview overlay: cursor, hover, or fresh insertion.
    fn image_for_preview(&self) -> Option<&PastedImage> {
        if let Some(elem) = self.image_element_near_cursor() {
            return self.images.iter().find(|img| img.element_id == elem.id);
        }
        if let Some(id) = self.hovered_image_element_id {
            return self.images.iter().find(|img| img.element_id == id);
        }
        let (id, cursor) = self.post_insert_image_preview?;
        if self.cursor() != cursor {
            return None;
        }
        self.images.iter().find(|img| img.element_id == id)
    }

    /// Drain all prompt-side images for submission.
    ///
    /// Reconciles against live `TextArea` elements first so deleted chips
    /// are never included. Returns the drained images; the prompt-side
    /// storage is left empty.
    pub fn drain_images(&mut self) -> Vec<PastedImage> {
        self.post_insert_image_preview = None;
        let live_ids: std::collections::HashSet<_> = self
            .textarea
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_IMAGE)
            .map(|e| e.id)
            .collect();
        crate::prompt_images::reconcile(&mut self.images, &live_ids);
        std::mem::take(&mut self.images)
    }

    /// Buffer text with `[Image #N]` chip placeholders removed, for
    /// text-only surfaces (e.g. question/permission feedback) that must
    /// not leak image tokens onto the wire.
    pub(crate) fn text_without_image_chips(&self) -> String {
        let text = self.textarea.text();
        let mut out = String::with_capacity(text.len());
        let mut prev_end = 0usize;
        for elem in self.textarea.elements() {
            if elem.kind != KIND_IMAGE {
                continue;
            }
            out.push_str(&text[prev_end..elem.range.start]);
            prev_end = elem.range.end;
        }
        out.push_str(&text[prev_end..]);
        out
    }

    /// Replace the prompt-side image list and restore `image_counter`.
    /// The caller must call `restore_chip_elements` beforehand to
    /// re-register the `KIND_IMAGE` textarea elements (with stored byte
    /// ranges) so hover/click behaviour works.
    ///
    /// **Pairing**: each `PastedImage` is bound to the live textarea
    /// element whose chip text parses to the same `display_number`.
    /// Identity-based pairing is robust against the two ways the
    /// `images` vector and the textarea element list can diverge:
    ///
    /// 1. `textarea.elements()` is sorted by buffer position
    ///    (`TextArea::add_element` sorts on every insert), whereas
    ///    `self.images` is populated by `insert_image::push` in
    ///    chronological insertion order. Inserting a second image at
    ///    the start of the buffer (cursor-at-Home) is enough to make
    ///    a positional zip mis-bind both records.
    /// 2. Two chips with identical placeholder widths (e.g.
    ///    `[Image #1]` and `[Image #2]`, both 10 bytes) would
    ///    collide under a byte-length match.
    ///
    /// `display_number` is unique within a prompt lifetime (the
    /// monotonic counter never recycles within one prompt), so the
    /// identity lookup is collision-free.
    ///
    /// Excess `images` past `IMAGE_CAP` are dropped with a warn:
    /// defence-in-depth for sessions that somehow accumulated more
    /// than the cap.
    pub fn set_images(&mut self, mut images: Vec<PastedImage>) {
        if images.len() > Self::IMAGE_CAP {
            tracing::warn!(
                target: PROMPT_IMAGES_TRACING_TARGET,
                count = images.len(),
                cap = Self::IMAGE_CAP,
                "set_images: input exceeds IMAGE_CAP; truncating",
            );
            images.truncate(Self::IMAGE_CAP);
        }

        // Build a (display_number → ElementId) lookup over the live
        // KIND_IMAGE elements. Linear scan (cap ≤ 10 in practice).
        //
        // Defence in depth: the monotonic-counter contract
        // guarantees `display_number` is unique within a prompt
        // lifetime, so duplicates here imply a contract violation
        // upstream (e.g. a future refactor that recycles numbers).
        // Surface a `tracing::warn!` with the existing image
        // tracing target so the regression is visible without
        // crashing the TUI; the lookup below still picks the first
        // match so the visible binding is at least deterministic.
        let buf = self.textarea.text();
        let mut by_number: Vec<(usize, ElementId)> = Vec::new();
        for elem in self.textarea.elements() {
            if elem.kind != KIND_IMAGE {
                continue;
            }
            if let Some(dn) = parse_image_display_number(&buf[elem.range.clone()]) {
                if by_number.iter().any(|(seen_dn, _)| *seen_dn == dn) {
                    tracing::warn!(
                        target: PROMPT_IMAGES_TRACING_TARGET,
                        display_number = dn,
                        element_id = ?elem.id,
                        "set_images: duplicate display_number across live KIND_IMAGE \
                         elements — monotonic-counter contract violated; first match \
                         wins the binding",
                    );
                    continue;
                }
                by_number.push((dn, elem.id));
            }
        }

        for img in &mut images {
            if let Some(&(_, eid)) = by_number.iter().find(|(dn, _)| *dn == img.display_number) {
                img.element_id = eid;
            } else {
                tracing::warn!(
                    target: PROMPT_IMAGES_TRACING_TARGET,
                    display_number = img.display_number,
                    "set_images: no live element matched display_number; \
                     PastedImage remains dangling until the next sync",
                );
            }
        }
        // Align with the monotonic contract in `sync_images_with_textarea`:
        // the counter only ever advances upward within a prompt lifetime.
        self.image_counter = self.image_counter.max(images_high_water(&images));
        self.images = images;
    }

    /// Re-register all chip elements (paste blocks, @-file refs, image
    /// chips) after a `set_text` restore. Uses the byte ranges stored at
    /// capture time — no buffer re-scanning.
    pub fn restore_chip_elements(&mut self, elems: &[crate::app::agent::ChipElement]) {
        self.textarea.restore_elements(
            elems
                .iter()
                .map(|e| (e.range.clone(), e.kind, e.display.clone())),
        );
    }

    /// Adopt previously drained images by binding them to the `[Image #N]`
    /// placeholders already present in the buffer, matched by display
    /// number. The `/feedback` pane uses this to turn an inline command's
    /// pasted attachments back into live, removable chips inside the
    /// prefill text. Images without a matching placeholder are cleaned up:
    /// nothing in the buffer references them, so they could never be
    /// removed by the user nor drained for send.
    pub fn adopt_images(&mut self, mut images: Vec<PastedImage>) {
        if images.is_empty() {
            return;
        }
        let text = self.textarea.text().to_owned();
        let mut chips: Vec<crate::app::agent::ChipElement> = Vec::new();
        let mut adopted: Vec<PastedImage> = Vec::new();
        for m in chip_placeholder_regex().find_iter(&text) {
            // The `:`-suffixed regex arm never parses (no closing bracket
            // in the match), which is correct: only the canonical
            // `[Image #N]` emitter form carries an exact chip range.
            let Some(number) = parse_image_display_number(m.as_str()) else {
                continue;
            };
            let Some(pos) = images.iter().position(|img| img.display_number == number) else {
                continue;
            };
            chips.push(crate::app::agent::ChipElement {
                range: m.range(),
                kind: KIND_IMAGE,
                display: Some(chip_line(format!("Image #{number}"))),
            });
            adopted.push(images.remove(pos));
        }
        crate::prompt_images::drain_and_cleanup(&mut images);
        if adopted.is_empty() {
            return;
        }
        self.restore_chip_elements(&chips);
        self.set_images(adopted);
    }

    /// Expose the underlying textarea for element access.
    pub fn textarea(&self) -> &echo_agent_ratatui_textarea::TextArea {
        &self.textarea
    }

    /// Buffer text of `elem` if it is a paste chip (`KIND_PASTE`).
    fn paste_text(&self, elem: &TextElement) -> Option<&str> {
        (elem.kind == KIND_PASTE).then(|| &self.textarea.text()[elem.range.clone()])
    }

    /// Check if the cursor is currently on a paste element (strict on-chip
    /// match, mirroring the Enter-to-expand targeting in
    /// [`Self::try_element_interaction`]).
    ///
    /// Returns the element's buffer text if so.
    pub fn paste_element_at_cursor(&self) -> Option<&str> {
        self.paste_text(self.textarea.element_at_cursor()?)
    }

    /// Paste element the cursor is on or immediately to the right of —
    /// where [`TextArea::insert_element`] leaves it right after a paste.
    ///
    /// An on-chip match of any kind wins (the chip under the cursor owns
    /// the position), so a cursor sitting on a non-paste chip yields
    /// `None` even when a paste chip ends at the cursor.
    fn paste_element_near_cursor(&self) -> Option<&TextElement> {
        let elem = self.textarea.element_at_cursor().or_else(|| {
            let cursor = self.textarea.cursor();
            self.textarea
                .elements()
                .iter()
                .find(|e| e.range.end == cursor)
        })?;
        (elem.kind == KIND_PASTE).then_some(elem)
    }

    /// Paste element text for the preview overlay.
    ///
    /// Shows the moment a chip is created (cursor right after it) as well
    /// as with the cursor on it. Display-only: Enter handling keeps the
    /// strict on-chip match, so Enter right after a chip keeps its normal
    /// behavior (submit or newline) instead of expanding the chip.
    fn paste_element_for_preview(&self) -> Option<&str> {
        self.paste_text(self.paste_element_near_cursor()?)
    }

    /// Expand hint for the paste preview overlay, honest per position:
    /// ON the chip Enter expands it; right after the chip Enter submits,
    /// so the affordance advertised there is pasting the content again.
    /// Double-click expands from either position (a click moves the
    /// cursor onto the chip first).
    fn paste_preview_hint(&self, theme: &Theme) -> Line<'static> {
        let dim = Style::default().fg(theme.gray);
        // Chord deliberately deviates from the tips' text_secondary to a
        // real accent: oscura/rosepine alias text_secondary and gray to
        // the box's own content/border colors, so only an accent pops.
        let chord = Style::default()
            .fg(theme.fuzzy_accent)
            .add_modifier(Modifier::BOLD);
        let action = if self.paste_element_at_cursor().is_some() {
            "enter"
        } else {
            "paste again"
        };
        Line::from(vec![
            Span::styled(action, chord),
            Span::styled(" or ", dim),
            Span::styled("double-click", chord),
            Span::styled(" to expand", dim),
        ])
    }

    /// Inline (expand) element `id` into plain buffer text and refresh the
    /// @-completion context. A single undoable step (see
    /// [`TextArea::inline_element`]).
    fn expand_element(&mut self, id: ElementId) {
        self.textarea.inline_element(id);
        self.update_file_search_context();
    }

    /// Expand the paste chip under the cursor (mouse double-click path).
    ///
    /// Strict on-chip match: a cursor merely adjacent to a chip (e.g.
    /// right after it) must not expand it. Returns `true` if a paste
    /// chip was expanded.
    pub fn expand_paste_element_at_cursor(&mut self) -> bool {
        let Some(elem) = self.textarea.element_at_cursor() else {
            return false;
        };
        if elem.kind != KIND_PASTE {
            return false;
        }
        let id = elem.id;
        self.expand_element(id);
        true
    }

    /// Try to interact with an element at the cursor.
    ///
    /// - Enter on a paste/file-ref element → inline (expand) it.
    /// - Enter on an image chip → signal the caller to open preview
    ///   (does NOT inline the placeholder text).
    ///
    /// Returns `Some(interaction)` if handled, `None` otherwise.
    pub fn try_element_interaction(&mut self, key: &KeyEvent) -> Option<ElementInteraction> {
        let elem = self.textarea.element_at_cursor()?;
        if !key!(Enter).matches(key) {
            return None;
        }
        match elem.kind {
            k if k == KIND_PASTE || k == KIND_FILE_REF => {
                let id = elem.id;
                self.expand_element(id);
                Some(ElementInteraction::Inlined)
            }
            k if k == KIND_IMAGE => Some(ElementInteraction::ImagePreview),
            _ => None,
        }
    }
}

/// Paint slash/skill accent color on a byte range, using textarea screen coords
/// so highlighting works on any visual row — including the continuation rows
/// of a token that soft-wraps at the line end.
fn paint_slash_token_highlight(
    textarea: &TextArea,
    state: TextAreaState,
    ta_area: Rect,
    buf: &mut Buffer,
    range: std::ops::Range<usize>,
    fg: ratatui::style::Color,
) {
    // One row-span per visual row, from the textarea's own wrap ranges (the
    // source of truth for where it broke the text); invalid ranges yield no
    // spans.
    for span in textarea.screen_spans_of_range(range, ta_area, state) {
        for x in span.left()..span.right() {
            if let Some(cell) = buf.cell_mut((x, span.y))
                && !cell.symbol().trim().is_empty()
            {
                cell.set_fg(fg);
            }
        }
    }
}

impl PromptWidget {
    /// Render the prompt widget.
    ///
    /// When `style.chrome` is true, renders the full block layout:
    /// `accent(┃) | hpad | content | hpad` with background fill.
    /// When false, renders only the content (prefix + textarea + info).
    pub fn draw(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        overlay_area: Option<Rect>,
        style: &PromptStyle,
        info: Option<&PromptInfo>,
        voice: Option<VoicePromptOverlay>,
    ) -> PromptRenderResult {
        if area.height == 0 || area.width < 4 {
            return PromptRenderResult {
                cursor_pos: None,
                post_flush_escapes: None,
            };
        }

        let theme = Theme::current();
        let bg = style.bg.color(theme.bg_base);

        let border_color = style.border_color_override.unwrap_or(if style.focused {
            theme.prompt_border_active
        } else {
            theme.prompt_border
        });

        // Fill entire area with fg + bg so every cell has RGB colors (needed for blending)
        buf.set_style(area, Style::default().fg(theme.text_primary).bg(bg));

        // Compute content area (inside chrome if enabled).
        let content_area = if style.chrome {
            let accent_w = if style.show_accent_line { 1u16 } else { 0 };
            if style.show_accent_line {
                let accent_color = style.accent_color(&theme);
                for y in area.y..area.y + area.height {
                    if let Some(cell) = buf.cell_mut((area.x, y)) {
                        cell.set_symbol(crate::glyphs::accent_bar());
                        cell.set_style(Style::default().fg(accent_color).bg(bg));
                    }
                }
            }
            Rect {
                x: area.x + accent_w + style.chrome_pad_left,
                y: area.y,
                width: area
                    .width
                    .saturating_sub(accent_w + style.chrome_pad_left + style.chrome_pad_right),
                height: area.height,
            }
        } else {
            area
        };

        // Split content: vpad_top + text + info_block
        let vpad_top = style.vpad_top;
        let info_block = style.info_block(info.is_some());
        let chunks = Layout::vertical([
            Constraint::Length(vpad_top),
            Constraint::Min(1),
            Constraint::Length(info_block),
        ])
        .split(content_area);

        let text_area_rect = chunks[1];

        // Top divider: ╭──────────╮
        if vpad_top > 0 && style.chrome && style.show_borders {
            let div_style = Style::default().fg(border_color).bg(bg);
            let div_y = chunks[0].y;
            let left_x = area.x;
            let right_x = area.x + area.width.saturating_sub(1);
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, div_y)) {
                    let ch = if x == left_x {
                        '\u{256d}' // ╭
                    } else if x == right_x {
                        '\u{256e}' // ╮
                    } else {
                        '\u{2500}' // ─
                    };
                    cell.set_char(ch);
                    cell.set_style(div_style);
                }
            }

            // Caption inlined in the divider, right-aligned ending 2 cells before ╮.
            // The pad spaces blank the adjacent `─`; corners plus 2-cell insets stay plain border.
            let caption = style
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let max_w = area.width.saturating_sub(6);
            if let Some(caption) = caption
                && max_w >= 6
            {
                let label = format!(" {caption} ");
                let trunc = crate::render::line_utils::truncate_str(&label, max_w as usize);
                let label_w = unicode_width::UnicodeWidthStr::width(trunc.as_str()) as u16;
                buf.set_string(
                    area.x + area.width.saturating_sub(3 + label_w),
                    div_y,
                    &trunc,
                    Self::chrome_caption_style(bg, &theme, style.focused),
                );
            }
        }

        // Render prefix on first text row: search icon when history search is active, else ❯.
        let prefix_w = if style.show_prefix { PREFIX_WIDTH } else { 0 };
        if style.show_prefix && text_area_rect.width > PREFIX_WIDTH {
            let (prefix_str, accent_color) =
                if self.history_search.is_active() && !self.history_search.is_browse() {
                    // Search-mode indicator — highest prefix priority. Browse
                    // mode keeps the normal prefix: the composer holds the
                    // populated entry (a real draft), not a filter query.
                    ("? ", theme.accent_user)
                } else if let Some((p, c)) = style.prefix_override {
                    // Caller override (e.g., bash mode `! ` in yellow).
                    (p, c)
                } else {
                    (crate::glyphs::prompt_arrow(), style.accent_color(&theme))
                };
            buf.set_string(
                text_area_rect.x,
                text_area_rect.y,
                prefix_str,
                Style::default().fg(accent_color).bg(bg),
            );
        }

        // TextArea content
        let ta_area = Rect {
            x: text_area_rect.x + prefix_w,
            y: text_area_rect.y,
            width: text_area_rect.width.saturating_sub(prefix_w),
            height: text_area_rect.height,
        };
        self.textarea_area = ta_area;

        (&self.textarea).render_ref(ta_area, buf, &mut self.textarea_state);

        // Chip bg remap (see `PromptBg::Panel`): chip `Line`s bake in
        // `paste_bg` at paste time and the same element can render on
        // multiple surfaces, so restyle at paint time.
        if matches!(style.bg, PromptBg::Panel(_)) && bg != theme.paste_bg {
            for y in ta_area.top()..ta_area.bottom() {
                for x in ta_area.left()..ta_area.right() {
                    if let Some(cell) = buf.cell_mut((x, y))
                        && cell.bg == theme.paste_bg
                    {
                        cell.bg = bg;
                    }
                }
            }
        }

        // Slash overlays: teal command name + args ghost text. Both use the
        // same snapshot, so clone once. Capture flags for later ghost text
        // suppression to avoid a second clone.
        let (slash_active, slash_has_inline_ghost) = {
            let snap = self.slash_state.snapshot();

            // Slash command-name highlight color. Minimal mode is monochrome, so
            // recolor to primary text (no visible accent) — typed `/commands`
            // then match the rest of the input.
            let slash_hl = if crate::views::modal_window::embedded() {
                theme.text_primary
            } else {
                theme.accent_skill
            };

            // Teal coloring: recolor the command name cells (not args) when the
            // command is recognized or has visible autocomplete suggestions.
            if snap.active
                && (snap.open || snap.command_recognized)
                && let Some(cmd_range) = &snap.command_range
            {
                paint_slash_token_highlight(
                    &self.textarea,
                    self.textarea_state,
                    ta_area,
                    buf,
                    cmd_range.clone(),
                    slash_hl,
                );
            }

            // Teal for the partial mid-text token under the cursor while typing.
            if let Some(ref ghost) = snap.inline_ghost {
                paint_slash_token_highlight(
                    &self.textarea,
                    self.textarea_state,
                    ta_area,
                    buf,
                    ghost.token_range.clone(),
                    slash_hl,
                );
            }

            // Args ghost text: show expected arguments after the cursor for
            // skills and commands that declare an arg_placeholder.
            if snap.args_query_is_empty
                && let Some(ref placeholder) = snap.args_placeholder
                && let Some((start_x, row_y)) = self.textarea.screen_position_of(
                    self.textarea.cursor(),
                    ta_area,
                    self.textarea_state,
                )
            {
                let avail = (ta_area.x + ta_area.width).saturating_sub(start_x) as usize;
                if avail > 0 {
                    let truncated = crate::render::line_utils::truncate_str(placeholder, avail);
                    buf.set_string(
                        start_x,
                        row_y,
                        &truncated,
                        Style::default().fg(theme.gray).bg(bg),
                    );
                }
            }

            // Mid-text inline ghost text: show completion suffix for a
            // partial `/command` token under the cursor.
            if let Some(ref ghost) = snap.inline_ghost
                && ghost.token_range.end <= self.textarea.text().len()
                && let Some((screen_x, screen_y)) = self.textarea.screen_position_of(
                    ghost.token_range.end,
                    ta_area,
                    self.textarea_state,
                )
            {
                let avail = (ta_area.x + ta_area.width).saturating_sub(screen_x) as usize;
                if avail > 0 {
                    let truncated = crate::render::line_utils::truncate_str(&ghost.text, avail);
                    buf.set_string(
                        screen_x,
                        screen_y,
                        &truncated,
                        Style::default().fg(theme.gray).bg(bg),
                    );
                }
            }

            // Teal highlighting for recognized mid-text `/command` tokens.
            for token_range in &snap.recognized_tokens {
                paint_slash_token_highlight(
                    &self.textarea,
                    self.textarea_state,
                    ta_area,
                    buf,
                    token_range.clone(),
                    slash_hl,
                );
            }

            (snap.active, snap.inline_ghost.is_some())
        };

        // Interim STT: muted italic overlay (not in the textarea). Finalized
        // text remains the real, editable draft.
        let voice_interim_shown = if let Some(v) = voice
            && let Some(interim) = v.interim.filter(|t| !t.trim().is_empty())
            && ta_area.width > 0
            && ta_area.height > 0
        {
            let interim_fg = crate::render::color::blend_color(bg, theme.text_secondary, 0.7)
                .unwrap_or(theme.gray);
            let interim_style = Style::default()
                .fg(interim_fg)
                .bg(bg)
                .add_modifier(Modifier::ITALIC);
            if self.textarea.text().is_empty() {
                let lines =
                    wrap_voice_interim(interim, ta_area.width as usize, ta_area.height as usize);
                for (i, line) in lines.iter().enumerate() {
                    buf.set_string(ta_area.x, ta_area.y + i as u16, line, interim_style);
                }
            } else {
                // Ghost suffix after the finalized draft (not at the caret).
                let end = self.textarea.text().len();
                if let Some((start_x, row_y)) =
                    self.textarea
                        .screen_position_of(end, ta_area, self.textarea_state)
                {
                    let display = format!(" {interim}");
                    let avail = (ta_area.x + ta_area.width).saturating_sub(start_x) as usize;
                    if avail > 0 {
                        let truncated = crate::render::line_utils::truncate_str(&display, avail);
                        buf.set_string(start_x, row_y, &truncated, interim_style);
                    }
                }
            }
            true
        } else {
            false
        };

        // Placeholder text when empty (unfocused, or opted in while focused).
        if self.textarea.text().is_empty()
            && ta_area.width > 0
            && (!style.focused || style.placeholder_when_focused)
            && !voice_interim_shown
        {
            let placeholder = style.placeholder_override.unwrap_or("Build anything");
            // `set_string` clips at the buffer edge, not at the textarea, so a placeholder longer than the box would paint over its border.
            let truncated =
                crate::render::line_utils::truncate_str(placeholder, ta_area.width as usize);
            buf.set_string(
                ta_area.x,
                ta_area.y,
                &truncated,
                Style::default().fg(theme.gray).bg(bg),
            );
        }

        // Side borders: │ on left and right of each text row.
        if style.chrome && style.show_borders && area.width >= 2 {
            let div_style = Style::default().fg(border_color).bg(bg);
            let left_x = area.x;
            let right_x = area.x + area.width.saturating_sub(1);
            for y in text_area_rect.y..text_area_rect.y + text_area_rect.height {
                if let Some(cell) = buf.cell_mut((left_x, y)) {
                    cell.set_char('\u{2502}'); // │
                    cell.set_style(div_style);
                }
                if let Some(cell) = buf.cell_mut((right_x, y)) {
                    cell.set_char('\u{2502}'); // │
                    cell.set_style(div_style);
                }
            }
        }

        // Bottom divider: ╰──────────echo-agent-3 · flags──╯
        // Guard on actual allocated height, not requested `info_block`: during
        // resize the layout may squeeze the info block to 0 rows, leaving
        // chunks[2].y past the buffer boundary.
        if info_block > 0 && style.chrome && style.show_borders && chunks[2].height > 0 {
            let div_y = chunks[2].y;
            let div_style = Style::default().fg(border_color).bg(bg);
            let left_x = area.x;
            let right_x = area.x + area.width.saturating_sub(1);
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, div_y)) {
                    let ch = if x == left_x {
                        '\u{2570}' // ╰
                    } else if x == right_x {
                        '\u{256f}' // ╯
                    } else {
                        '\u{2500}' // ─
                    };
                    cell.set_char(ch);
                    cell.set_style(div_style);
                }
            }
            // A blank info line still writes its padding spaces, which would punch holes in the divider it sits on.
            if let Some(info) = info.filter(|i| !i.is_blank()) {
                let info_rect = Rect {
                    x: content_area.x,
                    y: div_y,
                    width: content_area.width,
                    height: 1,
                };
                self.render_info_line(buf, info_rect, info, bg, &theme, style.focused);
            }
        }

        // Unfocused dimming: blend fg toward bg (bg already precomputed above).
        // Skip when the bg is overridden — the prompt is inline in another widget.
        if !style.focused && style.bg == PromptBg::Default {
            // Dim only the content inside the box (skip all border chars).
            let dim_area = Rect {
                x: area.x + 1,
                y: content_area.y + vpad_top,
                width: area.width.saturating_sub(2),
                height: content_area.height.saturating_sub(vpad_top + info_block),
            };
            crate::render::color::blend_area(buf, dim_area, Some((bg, 0.66)), None);
        }

        // Finalized draft stays editable during voice; hide the caret only when
        // the box is empty and interim is standing in for it.
        let hide_caret_for_empty_interim = self.textarea.text().is_empty()
            && voice.is_some_and(|v| v.interim.is_some_and(|t| !t.trim().is_empty()));
        let cursor_pos = if style.focused && !hide_caret_for_empty_interim {
            self.textarea
                .cursor_pos_with_state(ta_area, self.textarea_state)
        } else {
            None
        };

        // Ghost suffixes (shell completion / predicted prompt). Voice interim
        // owns the end-of-text cells when shown, so skip both ghosts then.
        if !voice_interim_shown {
            if let Some(ghost) = self.suggestions.ghost_text()
                && self.textarea.cursor() == self.textarea.text().len()
                && !slash_active
                && !slash_has_inline_ghost
                && let Some((cx, cy)) = cursor_pos
            {
                let avail = (ta_area.x + ta_area.width).saturating_sub(cx) as usize;
                if avail > 0 {
                    let truncated = crate::render::line_utils::truncate_str(ghost, avail);
                    buf.set_string(cx, cy, &truncated, theme.ghost_text_style().bg(bg));
                }
            }

            if let Some(ghost) = self.prompt_suggestion_ghost()
                && let Some((cx, cy)) = cursor_pos
            {
                let avail = (ta_area.x + ta_area.width).saturating_sub(cx) as usize;
                if avail > 0 {
                    let truncated = crate::render::line_utils::truncate_str(ghost, avail);
                    buf.set_string(cx, cy, &truncated, theme.ghost_text_style().bg(bg));
                }
            }
        }

        // Paste preview overlay: show when cursor is on or right after a paste element
        if style.focused
            && let Some(overlay) = overlay_area
            && let Some(paste_text) = self.paste_element_for_preview()
        {
            let preview_style = PreviewStyle::new(theme.paste_bg, theme.paste_fg, theme.paste_dim);
            let config = PreviewConfig {
                hint: Some(self.paste_preview_hint(&theme)),
                ..Default::default()
            };
            render_preview_overlay(buf, overlay, paste_text, preview_style, config);
        }

        let mut post_flush_escapes = None;
        let preview_image = if style.image_preview && style.focused && overlay_area.is_some() {
            self.image_for_preview()
        } else {
            None
        };

        if let Some(image) = preview_image {
            let Some(overlay) = overlay_area else {
                return PromptRenderResult {
                    cursor_pos,
                    post_flush_escapes: None,
                };
            };
            post_flush_escapes = crate::render::render_image_overlay(
                buf,
                overlay,
                image,
                theme.paste_bg,
                theme.paste_fg,
                theme.paste_dim,
            );
        }

        // Clear ID 1 whenever this frame did not replace it.
        if post_flush_escapes.is_none() {
            post_flush_escapes = crate::terminal::overlay::clear();
        }

        PromptRenderResult {
            cursor_pos,
            post_flush_escapes,
        }
    }

    /// Caption style for text embedded in the prompt's border chrome — the
    /// info line's model name on `╰─╯` and the session title on `╭─╮`:
    /// dimmed secondary text over the prompt bg, fading further when
    /// unfocused, so both borders read as one chrome.
    fn chrome_caption_style(bg: ratatui::style::Color, theme: &Theme, focused: bool) -> Style {
        let opacity = if focused { 0.6 } else { 0.4 };
        let fg = crate::render::color::blend_color(bg, theme.text_secondary, opacity)
            .unwrap_or(theme.gray);
        Style::default().fg(fg).bg(bg)
    }

    /// Render the info line: left-aligned `model_name · flag1 · flag2`, right-aligned `multiline`.
    ///
    /// `pub(crate)` so the dashboard's dispatch box (which draws its own
    /// chrome) can paint an identical model + mode indicator on its bottom
    /// border. Does not read `self`.
    pub(crate) fn render_info_line(
        &self,
        buf: &mut Buffer,
        area: Rect,
        info: &PromptInfo,
        bg: ratatui::style::Color,
        theme: &Theme,
        focused: bool,
    ) {
        if area.height == 0 {
            return;
        }

        // When the prompt is unfocused, fade info-line content further toward
        // bg so it follows the same focused/unfocused dimming as the prompt
        // border. Model name and flag color use a higher opacity than the
        // separator so they remain readable.
        let sep_opacity = if focused { 1.0 } else { 0.6 };
        let flag_opacity = if focused { 0.75 } else { 0.5 };

        let model_style = Self::chrome_caption_style(bg, theme, focused);
        let sep_fg = if focused {
            theme.gray_dim
        } else {
            crate::render::color::blend_color(bg, theme.gray_dim, sep_opacity)
                .unwrap_or(theme.gray_dim)
        };
        let sep_style = Style::default().fg(sep_fg).bg(bg);
        let flag_style = Style::default().fg(theme.gray).bg(bg);

        // Left side: model name + flags. Wrap with leading/trailing spaces
        // so the rendered cells adjacent to the corner borders (╰ / ╯) are
        // blanked out instead of showing the underlying `─` glyphs from the
        // bottom-border fill — giving 1 cell of visual padding on each side.
        let pad_style = Style::default().bg(bg);
        let mut left_spans = vec![Span::styled(" ", pad_style)];
        if let Some(warning) = info.usage_warning {
            let fg = if info.usage_warning_critical {
                theme.warning
            } else {
                sep_fg
            };
            let warning_style = Style::default().fg(fg).bg(bg);
            left_spans.push(Span::styled(warning.to_owned(), warning_style));
            left_spans.push(Span::styled(" · ", sep_style));
        }
        left_spans.push(Span::styled(info.model_name, model_style));
        for flag in info.flags {
            left_spans.push(Span::styled(" · ", sep_style));
            let mut style = if let Some(color) = flag.color {
                if flag.bold {
                    // Bold flags use full color for visibility.
                    Style::default().fg(color).bg(bg)
                } else {
                    let dimmed = crate::render::color::blend_color(bg, color, flag_opacity)
                        .unwrap_or(theme.gray);
                    Style::default().fg(dimmed).bg(bg)
                }
            } else if flag.bold {
                Style::default().fg(theme.text_primary).bg(bg)
            } else if focused {
                flag_style
            } else {
                let dimmed = crate::render::color::blend_color(bg, theme.gray, flag_opacity)
                    .unwrap_or(theme.gray);
                Style::default().fg(dimmed).bg(bg)
            };
            if flag.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            left_spans.push(Span::styled(flag.text, style));
        }
        // Trailing pad mirrors the leading pad above.
        left_spans.push(Span::styled(" ", pad_style));

        // Build right-side spans: "multiline" indicator.
        let mut right_spans: Vec<Span<'static>> = Vec::new();
        if info.multiline {
            right_spans.push(Span::styled("multiline", flag_style));
        }

        if !right_spans.is_empty() {
            // Pad the right side so it doesn't sit flush against the ╯ corner.
            right_spans.push(Span::styled(" ", pad_style));
            let right_line = Line::from(right_spans);
            let right_w = right_line.width() as u16;
            let left_line = Line::from(left_spans);
            let left_w = (left_line.width() as u16).min(area.width.saturating_sub(right_w + 1));
            // Right-align both parts: [left][gap][right]
            let total_w = left_w + 1 + right_w; // 1 for gap
            let x = area.x + area.width.saturating_sub(total_w);
            buf.set_line_safe(x, area.y, &left_line, left_w);
            let rx = area.x + area.width.saturating_sub(right_w);
            buf.set_line_safe(rx, area.y, &right_line, right_w);
        } else {
            // Right-align: clamp to area width so text doesn't overflow borders.
            let line = Line::from(left_spans);
            let text_w = (line.width() as u16).min(area.width);
            let x = area.x + area.width.saturating_sub(text_w);
            buf.set_line_safe(x, area.y, &line, text_w);
        }
    }
}

impl Default for PromptWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a line range string like "10" or "10-12" into a Range<usize> (1-based, exclusive end).
fn parse_line_range(s: &str) -> Option<std::ops::Range<usize>> {
    if let Some((start_s, end_s)) = s.split_once('-') {
        let start: usize = start_s.parse().ok()?;
        let end: usize = end_s.parse().ok()?;
        if start > 0 && end >= start {
            Some(start..end + 1)
        } else {
            None
        }
    } else {
        let line: usize = s.parse().ok()?;
        if line > 0 { Some(line..line + 1) } else { None }
    }
}

// ── Element display helpers ────────────────────────────────────────────

/// Build the styled display `Line` for a file reference element.
///
/// Renders as: `@foo/bar.rs` or `@foo/bar.rs:10-12`
/// Style: `@` and `:` in gray, path in theme.path, numbers in text_primary.
pub fn file_ref_display(path: &str) -> Line<'static> {
    let theme = Theme::current();
    let (file_part, line_part) = if let Some(colon_pos) = path.rfind(':') {
        (&path[..colon_pos], Some(&path[colon_pos + 1..]))
    } else {
        (path, None)
    };
    crate::views::file_search::styled_file_ref(file_part, line_part, &theme, true)
}

/// Extract the display number from an `[Image #N]` or
/// `[Image #N: <path>]` chip placeholder. Returns `None` when the
/// input doesn't match either form.
///
/// Path-suffix support is required because chips with a `source_path`
/// (file-path pastes / Finder Cmd+C) appear in the buffer as
/// `[Image #3: /path/to/file.png]`. Without it, the undo/redo
/// recovery path in `sync_images_with_textarea` falls back to
/// `stash_by_number.remove(0)` and never finds the original entry.
///
/// Permissive parsing: a literal space after `#` (e.g. `[Image # 1]`)
/// is tolerated via `trim()`. The canonical emitter never produces
/// that form; the permissiveness is intentional defence in depth.
fn parse_image_display_number(placeholder: &str) -> Option<usize> {
    let inner = placeholder.strip_prefix("[Image #")?.strip_suffix(']')?;
    // Split at the first `:` (path-suffix form) or use the whole string.
    let number_str = inner.split_once(':').map(|(n, _)| n).unwrap_or(inner);
    number_str.trim().parse().ok()
}

/// Regex matching a canonical image chip placeholder anywhere in a
/// buffer string: `[Image #<digits>]` or `[Image #<digits>:`.
///
/// Used by `set_text` to detect whether a non-empty replacement
/// preserves any chips. A literal-substring scan
/// (`text.contains("[Image #")`) has two foot-guns:
/// - false positive on the user pasting prose like `"[Image #" as a
///   placeholder convention`,
/// - false negative on near-matches like `[Image#1]` (missing space)
///   that the parser would reject anyway.
///
/// The regex pins to the canonical emitter format so the heuristic
/// matches what `display_text(...)` actually produces.
fn chip_placeholder_regex() -> &'static regex::Regex {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\[Image #\d+(?::|\])").unwrap());
    &RE
}

/// Build a copy of the buffer text with all element placeholder content
/// removed, and compute the adjusted cursor position.
///
/// This gives the slash system a clean view of just user-typed text,
/// so element placeholders like `[Image #1]` (which contain spaces)
/// don't break command parsing.
fn strip_all_elements(
    text: &str,
    cursor: usize,
    textarea: &echo_agent_ratatui_textarea::TextArea,
) -> (String, usize) {
    let elements = textarea.elements();
    if elements.is_empty() {
        return (text.to_owned(), cursor);
    }

    let mut result = String::with_capacity(text.len());
    let mut adjusted_cursor = cursor;
    let mut prev_end: usize = 0;

    for elem in elements {
        // Append text between the previous element and this one.
        let gap = &text[prev_end..elem.range.start];
        result.push_str(gap);

        // Adjust cursor: if cursor is after this element, subtract
        // the element's length from the cursor position.
        let elem_len = elem.range.end - elem.range.start;
        if cursor > elem.range.end {
            adjusted_cursor -= elem_len;
        } else if cursor > elem.range.start {
            // Cursor is inside the element — clamp to element start.
            adjusted_cursor -= cursor - elem.range.start;
        }

        prev_end = elem.range.end;
    }

    // Append any text after the last element.
    result.push_str(&text[prev_end..]);

    let len = result.len();
    (result, adjusted_cursor.min(len))
}

/// Map a byte offset in [`strip_all_elements`] output back to the raw buffer.
fn map_clean_offset_to_raw(
    text: &str,
    clean_offset: usize,
    elements: &[echo_agent_ratatui_textarea::TextElement],
) -> usize {
    if elements.is_empty() {
        return clean_offset.min(text.len());
    }

    let mut clean_pos = 0usize;
    let mut prev_end = 0usize;
    for elem in elements {
        let gap_len = elem.range.start.saturating_sub(prev_end);
        if clean_offset < clean_pos + gap_len {
            return (prev_end + (clean_offset - clean_pos)).min(text.len());
        }
        clean_pos += gap_len;
        prev_end = elem.range.end;
    }
    (prev_end + (clean_offset - clean_pos)).min(text.len())
}

/// Map a byte range in clean text to the corresponding raw buffer range.
fn map_clean_range_to_raw(
    text: &str,
    clean_range: std::ops::Range<usize>,
    textarea: &TextArea,
) -> std::ops::Range<usize> {
    let elements = textarea.elements();
    if elements.is_empty() {
        return clean_range;
    }
    let start = map_clean_offset_to_raw(text, clean_range.start, elements);
    let end = map_clean_offset_to_raw(text, clean_range.end, elements);
    start..end
}

/// Rewrite slash snapshot ranges from clean-text coordinates to raw buffer
/// coordinates (paste/image chips are omitted from clean text).
fn remap_slash_snapshot_to_raw(
    snap: &mut crate::slash::SlashSnapshot,
    text: &str,
    textarea: &TextArea,
) {
    if textarea.elements().is_empty() {
        return;
    }
    if let Some(ref range) = snap.command_range {
        snap.command_range = Some(map_clean_range_to_raw(text, range.clone(), textarea));
    }
    if let Some(ref range) = snap.args_range {
        snap.args_range = Some(map_clean_range_to_raw(text, range.clone(), textarea));
    }
    snap.recognized_tokens = snap
        .recognized_tokens
        .iter()
        .map(|r| map_clean_range_to_raw(text, r.clone(), textarea))
        .collect();
    if let Some(ref mut ghost) = snap.inline_ghost {
        ghost.token_range = map_clean_range_to_raw(text, ghost.token_range.clone(), textarea);
    }
}

/// Build a `[<label>]` chip line with the shared chip styling
/// (dim brackets, paste foreground/background).
fn chip_line(label: String) -> Line<'static> {
    let theme = Theme::current();
    let bracket = Style::default().fg(theme.paste_dim).bg(theme.paste_bg);
    let text = Style::default().fg(theme.paste_fg).bg(theme.paste_bg);
    Line::from(vec![
        Span::styled("[", bracket),
        Span::styled(label, text),
        Span::styled("]", bracket),
    ])
}

/// Normalize bare `\r` to `\n`, leaving `\r\n` pairs intact.
///
/// Some terminals send bare `\r` for line breaks in bracketed-paste content.
/// Rust's `str::lines()` only splits on `\n` and `\r\n`, so without this
/// normalization multi-line pastes would be treated as a single line.
fn normalize_cr(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() != Some(&'\n') {
            s.push('\n');
        } else {
            s.push(c);
        }
    }
    s
}

/// Build the styled display `Line` for a paste element chip.
///
/// Renders as: `[Pasted: N lines]`
fn paste_chip_display(line_count: usize) -> Line<'static> {
    chip_line(format!(
        "Pasted: {} line{}",
        line_count,
        if line_count != 1 { "s" } else { "" }
    ))
}

/// Display `Line` for a byte-triggered paste chip, e.g. `[Pasted: 12 KB]` or
/// `[Pasted: 1.0 MB]` (size, not a line count). Decimal (1000-based) units.
fn paste_chip_display_bytes(byte_len: usize) -> Line<'static> {
    let size = if byte_len >= 1_000_000 {
        format!("{:.1} MB", byte_len as f64 / 1_000_000.0)
    } else if byte_len >= 1000 {
        format!("{} KB", byte_len / 1000)
    } else {
        format!("{byte_len} bytes")
    };
    chip_line(format!("Pasted: {size}"))
}

/// Highest `display_number` present in `images`, or `0` when the slice is empty.
fn images_high_water(images: &[PastedImage]) -> usize {
    images.iter().map(|i| i.display_number).max().unwrap_or(0)
}

#[cfg(test)]
mod tests;
