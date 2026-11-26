//! Appearance configuration for the pager.
//!
//! Two-layer design:
//! - `RawAppearanceConfig`: Serde-friendly types for TOML (de)serialization
//! - `AppearanceConfig`: Runtime types with ratatui::Color, BlockBackground, etc.

use documented::{Documented, DocumentedFields};
use echo_agent_shared::ui_config::UiConfig;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, RawString};

// ============================================================================
// Runtime Config (used by render code)
// ============================================================================

/// Background style for block content area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BlockBackground {
    #[default]
    None,
    Light,
    Dark,
}

/// Runtime appearance configuration with resolved types.
#[derive(Debug, Clone)]
pub struct AppearanceConfig {
    pub animation: AnimationConfig,
    pub prompt: PromptViewConfig,
    pub scrollback: ScrollbackConfig,
    pub turn_status: TurnStatusConfig,
    /// Show timestamps on user/agent messages. Toggled via `/timestamps`.
    pub show_timestamps: bool,
    /// Timeline sidebar (per-turn tick rail). Toggled via `/timeline`.
    pub show_timeline: bool,
    /// Whether hooks & plugins UI is disabled (hides /hooks, /plugins commands
    /// and scrollback annotations). `false` by default (plugins enabled).
    pub disable_plugins: bool,
    /// Always show the "plan" chip in the status bar when plan content is
    /// available, even after the user exits plan mode.
    /// `false` by default (chip hidden once plan mode ends).
    pub show_plan_chip: bool,
    /// Alt-screen (fullscreen) policy from the `[terminal]` section.
    pub alt_screen: crate::terminal::AltScreenMode,
    /// Experimental scrollback-native minimal mode (`[terminal] minimal`).
    pub minimal: bool,
    /// Pinned live-region height (rows) in minimal mode. Clamped to
    /// `[3, term_height - 1]` at runtime.
    pub minimal_live_rows: u16,
    /// Maximum rows a single committed block may occupy in minimal mode before
    /// it is truncated with a "… N more lines" footer.
    pub minimal_max_commit_rows: u16,
    /// Resolved `[terminal] minimal_collapse_thinking`.
    pub minimal_collapse_thinking: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        RawAppearanceConfig::default().into()
    }
}

/// Turn status line configuration.
#[derive(Debug, Clone, Copy)]
pub struct TurnStatusConfig {
    /// When true, add a 1-line gap between the turn status line and the prompt
    /// widget. Allows visual separation; also enables future background styling
    /// without merging with the prompt's lighter background.
    pub gap: bool,
}

impl Default for TurnStatusConfig {
    fn default() -> Self {
        Self { gap: true }
    }
}

/// Prompt input view configuration (the editor widget, not the scrollback block).
#[derive(Debug, Clone, Copy)]
pub struct PromptViewConfig {
    /// When true, the prompt collapses to its minimum height (single-line)
    /// when focus is in the scrollback pane. Expands back when focused.
    pub collapse_unfocused: bool,
    /// Show hover highlight box when mousing over the prompt widget.
    pub mouse_hover: bool,
    /// Show the ❯ prefix character in the prompt editor.
    pub show_prefix: bool,
    /// Compact mode: remove top padding and reduce info block padding.
    /// Toggled at runtime via `/compact-mode`. This is the DERIVED render
    /// value, which the app may force on for short terminals (the persisted
    /// user setting is `UiConfig::compact_mode`) — in the pager, write it
    /// only via `AppView::apply_effective_compact`.
    pub compact: bool,
}

impl Default for PromptViewConfig {
    fn default() -> Self {
        Self {
            collapse_unfocused: true,
            mouse_hover: true,
            show_prefix: true,
            compact: false,
        }
    }
}

/// Scrollback pane configuration (layout, scrollbar, scroll, block rendering).
#[derive(Debug, Clone, Default)]
pub struct ScrollbackConfig {
    pub layout: LayoutConfig,
    pub scrollbar: ScrollbarConfig,
    pub scroll: ScrollConfig,
    pub blocks: BlocksConfig,
    pub display: ScrollbackDisplayConfig,
}

/// Scrollback display options (grouping, accents, etc.).
#[derive(Debug, Clone)]
pub struct ScrollbackDisplayConfig {
    /// Render a subtle horizontal line below the last entry.
    /// Visual marker for "end of content".
    pub line_under_last_entry: bool,
    /// Accent character for collapsed groupable blocks (default: "❙").
    /// Used instead of "┃" to prevent adjacent accents from merging visually.
    pub collapsed_accent_char: String,
    /// Blend factor for dimmed accents on collapsed groupable blocks (0.0–1.0).
    /// 0.0 = invisible (fully bg), 1.0 = full accent color. Default: 0.5.
    pub dim_accent: f32,
    /// Group selection box mode.
    /// When `true` (Mode B / "split"): selection box wraps only the contiguous
    /// collapsed sub-group around the selected entry. Expanded blocks within a
    /// group get their own individual selection box.
    /// When `false` (Mode A / "always"): selection box wraps the entire group
    /// regardless of expanded blocks.
    /// Default: `true` (Mode B).
    pub group_selection_split: bool,
    /// When true, the active-block highlight within a group extends over the
    /// selection box border columns (│). When false (default), the highlight is
    /// inset by 1 column on each side so the borders remain uncolored.
    pub highlight_overlays_border: bool,
    /// When true, the bullet character of the selected entry is replaced with
    /// an expand indicator (e.g., "›") if the block is foldable and collapsed.
    /// Helps indicate which entries can be expanded with 'l' or 'e'.
    /// Default: true.
    pub expandable_indicator: bool,
    /// When true, also show the expand indicator on running entries that are
    /// in their minimum fold mode (e.g., Truncated for execute/thinking blocks).
    /// The indicator inherits the block's animated accent style (blinking).
    /// Default: true.
    pub expandable_indicator_running: bool,
    /// Character to use as the expand indicator. Default: "›".
    pub expandable_indicator_char: String,
    /// Show ⧉ (copy) and ↗ (view) buttons on the selection box.
    /// Default: false (opt-in while testing).
    pub selection_buttons: bool,
    /// Pin user prompts as sticky headers when scrolled past.
    /// Default: true.
    pub sticky_headers: bool,
    /// Number of spaces to use when expanding tab characters (\t) in content.
    /// Tabs in model output are replaced with this many spaces before rendering.
    /// Default: 4. Set to 0 to pass through tabs unchanged.
    pub tab_width: u8,
    /// Maximum number of visible entries in a group of consecutive collapsed
    /// tool-call / thinking blocks. Older entries beyond this limit are hidden
    /// behind a compact "╶╶ N more" header. 0 disables group truncation.
    /// Default: 10.
    pub group_max_visible: u16,
    /// Apply UAX #9 bidi reordering in the app before painting scrollback
    /// lines. **Default false** — many terminals already reorder; enabling
    /// both double-flips Arabic/Persian. Turn on only if your terminal does
    /// not handle RTL.
    pub rtl_bidi: bool,
}

impl Default for ScrollbackDisplayConfig {
    fn default() -> Self {
        Self {
            line_under_last_entry: false,
            collapsed_accent_char: crate::glyphs::collapsed_accent().to_string(),
            dim_accent: 0.5,
            group_selection_split: true, // Mode B by default
            highlight_overlays_border: false,
            expandable_indicator: true,
            expandable_indicator_running: true,
            expandable_indicator_char: "›".to_string(),
            selection_buttons: false,
            sticky_headers: true,
            tab_width: 4,
            group_max_visible: 10,
            rtl_bidi: false,
        }
    }
}

/// Layout configuration for viewport padding and block spacing.
#[derive(Debug, Clone, Copy)]
pub struct LayoutConfig {
    /// Vertical padding (top/bottom) for outer viewport.
    pub outer_vpad: u16,
    /// Left horizontal padding for outer viewport.
    pub outer_hpad_left: u16,
    /// Right horizontal padding for outer viewport.
    pub outer_hpad_right: u16,
    /// Padding after accent line, before content (inside block bg).
    pub block_pad_left: u16,
    /// Padding after content, at right edge (inside block bg).
    pub block_pad_right: u16,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            outer_vpad: 1,
            outer_hpad_left: 2,
            outer_hpad_right: 2,
            block_pad_left: 2,
            block_pad_right: 2, // Match left padding for symmetry
        }
    }
}

impl LayoutConfig {
    /// Minimum value for horizontal padding (must have room for selection border).
    pub const MIN_HPAD: u16 = 1;

    /// Effective outer vertical padding (0 in compact mode).
    pub fn eff_outer_vpad(&self, compact: bool) -> u16 {
        if compact { 0 } else { self.outer_vpad }
    }

    /// Effective left horizontal padding (MIN_HPAD in compact mode).
    pub fn eff_hpad_left(&self, compact: bool) -> u16 {
        if compact {
            Self::MIN_HPAD
        } else {
            self.outer_hpad_left
        }
    }

    /// Effective right horizontal padding (MIN_HPAD in compact mode).
    pub fn eff_hpad_right(&self, compact: bool) -> u16 {
        if compact {
            Self::MIN_HPAD
        } else {
            self.outer_hpad_right
        }
    }

    /// Validate and clamp values to valid ranges.
    pub fn validated(self) -> Self {
        Self {
            outer_vpad: self.outer_vpad,
            outer_hpad_left: self.outer_hpad_left.max(Self::MIN_HPAD),
            outer_hpad_right: self.outer_hpad_right.max(Self::MIN_HPAD),
            block_pad_left: self.block_pad_left,
            block_pad_right: self.block_pad_right,
        }
    }
}

/// Scrollbar configuration.
///
/// # Positioning
///
/// The scrollbar position is computed as:
/// - `scrollbar_x = screen_right - gap_right - 1`
/// - Content ends at: `scrollbar_x - gap_left`
///
/// # Content Width Clamping
///
/// Content width is automatically clamped to not extend beyond the outer
/// viewport padding. This means:
/// - With `gap_right=0` (scrollbar at screen edge), the scrollbar is in `outer_hpad_right`
/// - With `gap_left=0`, content extends to just before the scrollbar
/// - But content will never exceed `outer_hpad_right` boundary on the right
///
/// This allows flexible scrollbar positioning without content overflow.
#[derive(Debug, Clone, Copy)]
pub struct ScrollbarConfig {
    /// Whether scrollbar is enabled.
    pub enabled: bool,
    /// Gap between content/selection edge and scrollbar track.
    /// 0 = adjacent to content, 1+ = space between content and scrollbar.
    pub gap_left: u16,
    /// Gap between scrollbar track and screen edge.
    /// 0 = scrollbar at screen edge (in outer_hpad_right if > 0).
    pub gap_right: u16,
    /// Override scrollbar background color (None = use theme default).
    pub scrollbar_bg: Option<Color>,
    /// Override scrollbar foreground/thumb color (None = use theme default).
    pub scrollbar_fg: Option<Color>,
}

impl Default for ScrollbarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            gap_left: 0,  // Content adjacent to scrollbar
            gap_right: 0, // Scrollbar at screen edge
            scrollbar_bg: None,
            scrollbar_fg: None,
        }
    }
}

impl ScrollbarConfig {
    /// Total width reserved for scrollbar (gap_left + track + gap_right).
    pub fn total_width(&self) -> u16 {
        if self.enabled {
            self.gap_left + 1 + self.gap_right
        } else {
            0
        }
    }

    /// Whether scrollbar fits entirely within outer_hpad_right.
    pub fn is_outside(&self, outer_hpad_right: u16) -> bool {
        self.gap_right < outer_hpad_right
    }
}

/// Scroll behavior configuration.
#[derive(Debug, Clone, Copy)]
pub struct ScrollConfig {
    /// Minimum lines of context to keep above/below selected entry.
    /// When navigating, ensure at least this many lines of adjacent entries
    /// remain visible. 0 = scroll to edge (default).
    pub margin: u16,
    /// Minimum scroll as a fraction of viewport height (0-100).
    /// If a scroll would be less than this percentage of the viewport,
    /// scroll by this amount instead. 0 = minimal scroll (default).
    /// 100 = always scroll by full page.
    pub min_page_fraction: u8,
    /// Follow indicator style in the gap row below scrollback.
    pub follow_indicator: FollowIndicator,
    /// When follow mode scrolls to new content, auto-select the latest entry.
    pub follow_auto_select: bool,
    /// Scrolling past the bottom (j, Ctrl-D, page-down, mousewheel) engages follow mode.
    pub follow_by_overscroll: bool,
    /// When true (default), expanding/collapsing a block adjusts scroll_offset so
    /// the block's header line stays at the same screen position. When false, uses
    /// ensure_selected_visible (the block may shift on screen).
    pub anchor_on_fold: bool,
    pub respect_manual_folds: bool,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            margin: 0,
            min_page_fraction: 0,
            follow_indicator: FollowIndicator::Center,
            follow_auto_select: true,
            follow_by_overscroll: true,
            anchor_on_fold: true,
            respect_manual_folds: false,
        }
    }
}

/// Scroll indicator display mode: the ▼ jump-to-bottom arrow below
/// scrollback and its ▲ jump-to-response-top mirror under the sticky
/// prompt header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FollowIndicator {
    /// No scroll indicators.
    None,
    /// Show ▼ centered in the gap row below scrollback when not following
    /// and there's content below the viewport, and ▲ centered under the
    /// sticky prompt header while the answer being read starts above the
    /// viewport top.
    #[default]
    Center,
}

impl ScrollConfig {
    /// Compute the minimum scroll amount in lines for a given viewport height.
    pub fn min_scroll_lines(&self, viewport_height: u16) -> u16 {
        if self.min_page_fraction == 0 {
            0
        } else {
            let fraction =
                (self.min_page_fraction.min(100) as u32) * (viewport_height as u32) / 100;
            fraction as u16
        }
    }
}

/// Animation configuration.
#[derive(Debug, Clone, Copy)]
pub struct AnimationConfig {
    /// Animation frame rate (ticks per second).
    /// Higher = smoother but more CPU. Default: 30.
    pub fps: u8,
    /// Rows per wave cycle for accent line animation.
    /// Lower = faster wave, higher = slower/smoother wave. Default: 32.
    pub wave_rows: u16,
    /// Show an FPS counter overlay in the top-right corner (debug/dev builds only).
    /// Also enabled by the `ECHO_AGENT_FPS=1` env var. Default: false.
    pub show_fps: bool,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            wave_rows: 32,
            show_fps: false,
        }
    }
}

impl AnimationConfig {
    /// Get the tick interval as a Duration.
    pub fn tick_interval(&self) -> std::time::Duration {
        let fps = self.fps.max(1) as u64;
        std::time::Duration::from_millis(1000 / fps)
    }
}

#[derive(Debug, Clone, Default)]
pub struct BlocksConfig {
    pub edit: EditBlockConfig,
    pub prompt: PromptConfig,
    pub thinking: ThinkingConfig,
    pub tool: ToolConfig,
    pub list_dir: ListDirConfig,
    pub execute: ExecuteConfig,
}

/// Runtime config for EditBlock with resolved ratatui types.
#[derive(Debug, Clone)]
pub struct EditBlockConfig {
    pub indent: bool,
    pub vpad: bool,
    pub bg: BlockBackground,
    pub accent_bg: bool,
    pub accent: Option<Color>,
    pub gutter_bg: bool,
    pub indent_bg: bool,
    /// Show the +N/-M line summary in the collapsed header. `None` (default)
    /// follows the shell-owned `collapsed_edit_blocks` flag; an explicit
    /// pager.toml value pins the shape regardless of the flag.
    pub line_summary: Option<bool>,
    /// When true, Edit blocks start in Expanded mode showing the diff; when
    /// false, they start Collapsed (one-line summary). `None` (default)
    /// follows the shell-owned `collapsed_edit_blocks` flag; an explicit
    /// pager.toml value pins the shape regardless of the flag.
    pub expanded_by_default: Option<bool>,
    /// Separator between diff hunks.
    /// Options: "…" (ellipsis, default), "───" (line), "⋯" (midline), "" (none).
    pub hunk_separator: String,
    /// Show two line-number columns (old + new) like GitHub's unified diff.
    /// When false (default), show a single column with the new-file line number.
    pub dual_line_numbers: bool,
}

impl Default for EditBlockConfig {
    fn default() -> Self {
        Self {
            indent: true,
            vpad: false,
            bg: BlockBackground::None,
            accent_bg: false,
            accent: None,
            gutter_bg: false,
            indent_bg: false,
            line_summary: None,
            expanded_by_default: None,
            hunk_separator: "…".to_string(),
            dual_line_numbers: false,
        }
    }
}

impl EditBlockConfig {
    /// Effective "Edit blocks start expanded" default. The single policy
    /// point pairing the two owners: an explicit pager.toml value wins;
    /// unset defers to the shell-owned `collapsed_edit_blocks` flag
    /// (flag on = collapsed one-liner, off = legacy expanded diff).
    pub fn effective_expanded(&self, collapsed_edit_blocks: bool) -> bool {
        self.expanded_by_default.unwrap_or(!collapsed_edit_blocks)
    }

    /// Effective collapsed-header `+N/-M` diffstat toggle. Same pairing as
    /// [`Self::effective_expanded`]: explicit value wins; unset shows the
    /// diffstat exactly when the flag collapses Edits (the one-liner view
    /// is what the summary exists for).
    pub fn effective_line_summary(&self, collapsed_edit_blocks: bool) -> bool {
        self.line_summary.unwrap_or(collapsed_edit_blocks)
    }
}

/// Runtime config for user prompt block (rendered inside scrollback).
#[derive(Debug, Clone)]
pub struct PromptConfig {
    /// Whether to apply vertical padding (blank lines above/below).
    pub vpad: bool,
    /// Block background color.
    pub bg: BlockBackground,
    /// Whether accent column gets block's background.
    pub accent_bg: bool,
    /// Minimum content lines to show in truncated/sticky header mode.
    /// This is the number of actual content lines, not including vpad.
    pub min_lines: u16,
    /// Show the ❯ prefix character before the prompt text.
    pub show_prefix: bool,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            vpad: true,
            bg: BlockBackground::Light,
            accent_bg: false,
            min_lines: 2,
            show_prefix: true,
        }
    }
}

/// Runtime config for thinking/reasoning block.
#[derive(Debug, Clone)]
pub struct ThinkingConfig {
    /// Accent color for the thinking block.
    pub accent: Color,
    /// Whether accent line is enabled. When false, no accent in any mode.
    pub accent_enabled: bool,
    /// How much to blend markdown colors with background (0.0-1.0).
    /// 0.8 means 80% original color, 20% background.
    pub bg_blend: f32,
    /// Number of visual lines to show in truncated mode (before and after ellipsis).
    pub truncated_lines: u16,
    /// Whether the accent line animates (traveling wave) while thinking is active.
    pub animate: bool,
    /// Show header line ("Thinking..." / "Thought for Xs") in all display modes.
    /// When false (default), the header only appears in collapsed mode.
    /// When true, it appears as the first line in truncated and expanded modes too.
    pub header: bool,
    /// When true, the header uses brighter styling in non-collapsed modes
    /// (matching tool block title style), and respects muted_collapsed when collapsed.
    /// When false (default), the header is always dim/muted gray.
    pub header_bright: bool,
    /// Render the reasoning body de-emphasized (SGR dim + italic) on top of the
    /// `bg_blend` fade, for surfaces where the fade alone cannot separate
    /// reasoning from the answer. **Not a TOML key** — minimal mode sets it;
    /// see the minimal-mode design doc §6.16.
    pub body_dim_italic: bool,
    /// Append a dim "(ctrl+e to expand)" affordance to the *collapsed* header
    /// when it fits on the same row (never adds a row). **Not a TOML key** —
    /// minimal mode sets it, being the only surface where a folded block cannot
    /// be unfolded in place.
    pub collapsed_expand_hint: bool,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            accent: crate::theme::Theme::current().gray_dim,
            accent_enabled: true,
            bg_blend: 0.7,
            truncated_lines: 3,
            animate: true,
            header: true,
            header_bright: false,
            body_dim_italic: false,
            collapsed_expand_hint: false,
        }
    }
}

/// Runtime config for tool call blocks (Read, Search, ListDir, etc).
#[derive(Debug, Clone)]
pub struct ToolConfig {
    /// When true, collapsed tool calls render entirely in muted gray.
    /// When false, collapsed tool calls show normal colors (paths, patterns, etc).
    pub muted_collapsed: bool,
    /// When true, parenthetical details use gray_dim (dimmest gray):
    /// Read "(1-50)", Search "(N matches)", Edit "(N edits)", Thinking "for Xs".
    /// When false, they use the normal muted gray.
    pub dim_details: bool,
    /// Bullet/icon character rendered before tool call headers.
    pub bullet: ToolBullet,
    // Note: bullet_accent and bullet_color were removed in the scrollback-v2 refactor.
    // Bullet color is now determined by BlockContent::bullet() — each block type
    // decides its own bullet color based on state (accent color, error, default).
    // Dimming for collapsed+groupable blocks is handled by EntryRenderer.
    // TODO(dim_muted): add a dim factor for collapsed text styling (not just bullet/accent).
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            muted_collapsed: true,
            dim_details: true,
            bullet: ToolBullet::Diamond,
        }
    }
}

/// Bullet/icon style for tool call headers.
///
/// Rendered before the tool title, e.g. `⊙ Read src/main.rs`.
/// Respects `muted_collapsed`: when the tool is collapsed and muting is
/// enabled, the bullet color blends with the muted palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolBullet {
    /// No bullet (default).
    #[default]
    None,
    /// `·` (middle dot — smallest).
    Dot,
    /// `•` (bullet — between dot and circle).
    SmallCircle,
    /// `●` (filled circle).
    Circle,
    /// `▸` (right-pointing small triangle).
    SmallTriangle,
    /// `▶` (right-pointing triangle).
    Triangle,
    /// `◆` (filled diamond).
    Diamond,
}

impl ToolBullet {
    /// The display character for this bullet, or `None` for no bullet.
    pub fn char(&self) -> Option<&'static str> {
        match self {
            Self::None => Option::None,
            Self::Dot => Some("·"),
            Self::SmallCircle => Some("•"),
            Self::Circle => Some(crate::glyphs::filled_dot()),
            Self::SmallTriangle => Some("▸"),
            Self::Triangle => Some("▶"),
            // Routed through `glyphs` so the default scrollback bullet
            // (used by tool calls, thinking, the running-subagent block,
            // etc.) degrades to the CP437 `♦` on legacy Windows consoles
            // that can't render U+25C6.
            Self::Diamond => Some(crate::glyphs::diamond_filled()),
        }
    }
}

/// Runtime config for ListDir block.
#[derive(Debug, Clone)]
pub struct ListDirConfig {
    /// When true, output has terminal-style dark background.
    /// When false, output has no background (default).
    pub terminal_bg: bool,
}

impl Default for ListDirConfig {
    fn default() -> Self {
        Self {
            terminal_bg: true, // Default: dark background for output
        }
    }
}

/// Header display style for execute blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecuteHeaderStyle {
    /// Shell style: `$ command` (default).
    /// The `$` prompt is dim/muted, command may or may not be colored.
    #[default]
    Shell,
    /// Label style: `Run command` (like Edit/Search blocks).
    /// "Run" is bold (muted when collapsed, primary when expanded).
    Label,
}

/// Runtime config for Execute tool call block.
#[derive(Debug, Clone)]
pub struct ExecuteConfig {
    /// Number of output lines to show at the start in truncated mode.
    pub first_lines: u16,
    /// Number of output lines to show at the end in truncated mode.
    pub last_lines: u16,
    /// Whether accent line is enabled. When false, no accent (running/success/error).
    pub accent_enabled: bool,
    /// Accent color for running execute blocks (animated).
    pub running_accent: Color,
    /// Header display style (shell `$` vs label `Run`).
    pub header_style: ExecuteHeaderStyle,
    /// When true, command text is muted/uncolored when collapsed.
    pub muted_command_collapsed: bool,
}

impl Default for ExecuteConfig {
    fn default() -> Self {
        Self {
            first_lines: 2,
            last_lines: 3,
            accent_enabled: true,
            running_accent: crate::theme::Theme::current().accent_running,
            header_style: ExecuteHeaderStyle::Label,
            muted_command_collapsed: true,
        }
    }
}

// ============================================================================
// Raw Config (for TOML serde)
// ============================================================================
//
// ╔═══════════════════════════════════════════════════════════════════════════╗
// ║ MAINTAINER NOTE: When adding/changing fields or sections:                 ║
// ║                                                                           ║
// ║ 1. Add doc comments (///) to ALL fields in Raw* structs - they become    ║
// ║    TOML comments via the `DocumentedFields` derive macro.                 ║
// ║                                                                           ║
// ║ 2. If adding a new section (e.g., RawNewBlockConfig):                     ║
// ║    - Add it to RawBlocksConfig (or appropriate parent)                    ║
// ║    - Add corresponding runtime config (NewBlockConfig)                    ║
// ║    - Add From<RawNewBlockConfig> for NewBlockConfig conversion            ║
// ║    - Add annotate_table call in to_toml_with_comments() below!            ║
// ║                                                                           ║
// ║ 3. The to_toml_with_comments() method generates the default config file   ║
// ║    with comments. Update it when adding new sections.                     ║
// ╚═══════════════════════════════════════════════════════════════════════════╝

/// Root appearance configuration (TOML format).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawAppearanceConfig {
    /// Terminal behavior settings (fullscreen policy).
    pub terminal: RawTerminalConfig,
    /// Animation settings.
    pub animation: RawAnimationConfig,
    /// Prompt input view settings (collapse, hover).
    pub prompt: RawPromptViewConfig,
    /// Scrollback pane settings (layout, scrollbar, scroll, blocks).
    pub scrollback: RawScrollbackConfig,
    /// Disable hooks & plugins UI (/hooks and /plugins commands, scrollback annotations).
    /// Defaults to false (plugins enabled).
    pub disable_plugins: bool,
    /// Always show the "plan" chip in the status bar when plan content is
    /// available, even after the user exits plan mode.
    /// Defaults to false (chip hidden once plan mode ends).
    pub show_plan_chip: bool,
}

/// Terminal behavior configuration (TOML format).
///
/// Controls fullscreen (alternate screen) policy and related terminal
/// interaction settings.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawTerminalConfig {
    /// Alt-screen (fullscreen) policy.
    /// "auto" — fullscreen in plain terminals and normal tmux, inline in
    ///          tmux control mode and Zellij. (default)
    /// "always" — always enter fullscreen, even in control mode / Zellij.
    /// "never" — never enter fullscreen; run inline in main scrollback.
    pub alt_screen: RawAltScreenMode,
    /// Experimental scrollback-native rendering mode. Finalized blocks are
    /// printed into the terminal's native scrollback. Default false.
    pub minimal: bool,
    /// Pinned live-region height (rows) for minimal mode. Default 10.
    pub minimal_live_rows: Option<u16>,
    /// Maximum rows for a single committed block in minimal mode. Default 2000.
    pub minimal_max_commit_rows: Option<u16>,
    /// Commit reasoning ("Thought for Xs") to native scrollback COLLAPSED to
    /// its one-line header instead of in full. Default false — minimal
    /// deliberately keeps the whole reasoning body in the transcript (K9); this
    /// is the opt-out for a terser scrollback. The body stays reachable with
    /// `Ctrl+E` / `/expand` and `/transcript`.
    pub minimal_collapse_thinking: bool,
}

impl Default for RawTerminalConfig {
    fn default() -> Self {
        Self {
            alt_screen: RawAltScreenMode::Auto,
            minimal: false,
            minimal_live_rows: None,
            minimal_max_commit_rows: None,
            minimal_collapse_thinking: false,
        }
    }
}

/// Raw alt-screen mode for TOML (de)serialization.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RawAltScreenMode {
    /// Automatic: fullscreen in healthy environments, inline in degraded ones.
    #[default]
    Auto,
    /// Always enter the alternate screen.
    Always,
    /// Never enter the alternate screen.
    Never,
}

impl From<RawAltScreenMode> for crate::terminal::AltScreenMode {
    fn from(raw: RawAltScreenMode) -> Self {
        match raw {
            RawAltScreenMode::Auto => crate::terminal::AltScreenMode::Auto,
            RawAltScreenMode::Always => crate::terminal::AltScreenMode::Always,
            RawAltScreenMode::Never => crate::terminal::AltScreenMode::Never,
        }
    }
}
/// Prompt input view configuration (TOML format).
///
/// This configures the prompt editor widget at the bottom of the screen,
/// NOT the user prompt block rendered inside the scrollback.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawPromptViewConfig {
    /// When true, the prompt collapses to its minimum height (single-line)
    /// when focus is in the scrollback pane. Expands back when focused.
    pub collapse_unfocused: bool,
    /// Show hover highlight box when mousing over the prompt widget.
    pub mouse_hover: bool,
    /// Show the ❯ prefix character in the prompt editor.
    pub show_prefix: bool,
}

impl Default for RawPromptViewConfig {
    fn default() -> Self {
        Self {
            collapse_unfocused: true,
            mouse_hover: true,
            show_prefix: true,
        }
    }
}

/// Scrollback pane configuration (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Default, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawScrollbackConfig {
    /// Layout settings (padding, spacing).
    pub layout: RawLayoutConfig,
    /// Scrollbar settings.
    pub scrollbar: RawScrollbarConfig,
    /// Scroll behavior settings.
    pub scroll: RawScrollConfig,
    /// Block rendering settings.
    pub blocks: RawBlocksConfig,
    /// Miscellaneous display options.
    pub display: RawScrollbackDisplayConfig,
}

/// Scrollback display options (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawScrollbackDisplayConfig {
    /// Render a subtle horizontal line below the last entry ("end of content" marker).
    pub line_under_last_entry: bool,
    /// Accent character for collapsed groupable blocks. Default: "❙".
    pub collapsed_accent_char: Option<String>,
    /// Blend factor for dimmed accents on collapsed groupable blocks (0.0–1.0). Default: 0.5.
    pub dim_accent: Option<f32>,
    /// Group selection box mode. true = "split" (Mode B), false = "always" (Mode A). Default: true.
    pub group_selection_split: Option<bool>,
    /// Whether active-block highlight overlays selection box borders. Default: false.
    pub highlight_overlays_border: Option<bool>,
    /// Show expand indicator (e.g., "›") on selected foldable collapsed entries. Default: true.
    pub expandable_indicator: Option<bool>,
    /// Show expand indicator on running entries in their minimum fold mode. Default: true.
    pub expandable_indicator_running: Option<bool>,
    /// Character for the expand indicator. Default: "›".
    pub expandable_indicator_char: Option<String>,
    /// Show ⧉/↗ buttons on the selection box. Default: false.
    pub selection_buttons: Option<bool>,
    /// Pin user prompts as sticky headers when scrolled past. Default: true.
    pub sticky_headers: Option<bool>,
    /// Number of spaces to use when expanding tab characters (\t) in content.
    /// Tabs in model output are replaced with this many spaces before rendering.
    /// Default: 4. Set to 0 to pass through tabs unchanged.
    pub tab_width: Option<u8>,
    /// Maximum visible entries in a consecutive group of collapsed tool/thinking blocks.
    /// Older entries are hidden behind a compact header. 0 = disable. Default: 10.
    pub group_max_visible: Option<u16>,
    /// App-side RTL bidi reordering for scrollback. Default false (terminals often
    /// already reorder; enabling both double-flips Arabic/Persian).
    pub rtl_bidi: Option<bool>,
}

impl Default for RawScrollbackDisplayConfig {
    fn default() -> Self {
        Self {
            line_under_last_entry: false,
            collapsed_accent_char: Some(crate::glyphs::collapsed_accent().to_string()),
            dim_accent: Some(0.5),
            group_selection_split: Some(true),
            highlight_overlays_border: Some(false),
            expandable_indicator: Some(true),
            expandable_indicator_running: Some(true),
            expandable_indicator_char: Some("›".to_string()),
            selection_buttons: Some(false),
            sticky_headers: Some(true),
            tab_width: Some(4),
            group_max_visible: Some(10),
            rtl_bidi: Some(false),
        }
    }
}

/// Layout configuration (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawLayoutConfig {
    /// Vertical padding (top/bottom) for outer viewport.
    pub outer_vpad: u16,
    /// Left horizontal padding for outer viewport (min 1).
    pub outer_hpad_left: u16,
    /// Right horizontal padding for outer viewport (min 1).
    pub outer_hpad_right: u16,
    /// Padding after accent line, before content.
    pub block_pad_left: u16,
    /// Padding after content, at right edge.
    pub block_pad_right: u16,
}

impl Default for RawLayoutConfig {
    fn default() -> Self {
        Self {
            outer_vpad: 1,
            outer_hpad_left: 2,
            outer_hpad_right: 2,
            block_pad_left: 2,
            block_pad_right: 2,
        }
    }
}

/// Scrollbar configuration (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawScrollbarConfig {
    /// Whether scrollbar is enabled.
    pub enabled: bool,
    /// Gap between content/selection edge and scrollbar track.
    /// 0 = adjacent to content, 1+ = space between.
    /// Note: Content width is clamped to outer_hpad boundaries.
    pub gap_left: u16,
    /// Gap between scrollbar track and screen edge.
    /// 0 = scrollbar at screen edge (uses outer_hpad_right if available).
    pub gap_right: u16,
    /// Override scrollbar background color.
    /// Use "none" to use theme default, or a color value.
    pub scrollbar_bg: OptionalColor,
    /// Override scrollbar foreground/thumb color.
    /// Use "none" to use theme default, or a color value.
    pub scrollbar_fg: OptionalColor,
}

impl Default for RawScrollbarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            gap_left: 0,
            gap_right: 0,
            scrollbar_bg: OptionalColor::None,
            scrollbar_fg: OptionalColor::None,
        }
    }
}

/// Scroll behavior configuration (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawScrollConfig {
    /// Minimum lines of context to keep above/below selected entry.
    /// When navigating, ensure at least this many lines of adjacent entries
    /// remain visible. 0 = scroll to edge (default).
    pub margin: u16,
    /// Minimum scroll as percentage of viewport height (0-100).
    /// If a scroll would be less than this percentage, scroll by this amount instead.
    /// 0 = minimal scroll (default), 25 = quarter page, 100 = full page.
    pub min_page_fraction: u8,
    /// Scroll indicators: the ▼ below scrollback and the ▲ under the sticky
    /// prompt header. "none" = hidden, "center" = centered arrows.
    pub follow_indicator: RawFollowIndicator,
    /// When follow mode scrolls to new content, auto-select the latest entry.
    pub follow_auto_select: bool,
    /// Scrolling past the bottom (j, Ctrl-D, page-down, mousewheel) engages follow mode.
    pub follow_by_overscroll: bool,
    /// Anchor scroll on fold: keep the toggled block's header at the same screen y. Default: true.
    pub anchor_on_fold: bool,
    /// Opt-in: keep manually folded blocks as-is during streaming and when they finish,
    /// and stop auto-scroll when a fold expands a block while following. Default: false.
    pub respect_manual_folds: bool,
}

impl Default for RawScrollConfig {
    fn default() -> Self {
        Self {
            margin: 0,
            min_page_fraction: 0,
            follow_indicator: RawFollowIndicator::Center,
            follow_auto_select: true,
            follow_by_overscroll: true,
            anchor_on_fold: true,
            respect_manual_folds: false,
        }
    }
}

/// Scroll indicator display mode (TOML format).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RawFollowIndicator {
    /// No scroll indicators.
    None,
    /// Show ▼ centered below scrollback and ▲ under the sticky prompt header.
    #[default]
    Center,
}

/// Tool bullet style (TOML format).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RawToolBullet {
    /// No bullet.
    #[default]
    None,
    /// `·` (middle dot — smallest).
    Dot,
    /// `•` (bullet — between dot and circle).
    SmallCircle,
    /// `●` (filled circle).
    Circle,
    /// `▸` (right-pointing small triangle).
    SmallTriangle,
    /// `▶` (right-pointing triangle).
    Triangle,
    /// `◆` (filled diamond).
    Diamond,
}

/// Animation configuration (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawAnimationConfig {
    /// Animation frame rate (ticks per second).
    /// Higher = smoother but more CPU. Range: 1-60. Default: 30.
    pub fps: u8,
    /// Rows per wave cycle for accent line animation.
    /// Lower = faster wave, higher = slower/smoother wave. Default: 32.
    pub wave_rows: u16,
    /// Show an FPS counter overlay in the top-right corner.
    /// Requires a debug build. Also enabled by ECHO_AGENT_FPS=1 env var. Default: false.
    pub show_fps: bool,
}

impl Default for RawAnimationConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            wave_rows: 32,
            show_fps: false,
        }
    }
}

/// Configuration for all block types (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Default, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawBlocksConfig {
    /// Edit block settings.
    pub edit: RawEditBlockConfig,
    /// User prompt block settings.
    pub prompt: RawPromptConfig,
    /// Thinking/reasoning block settings.
    pub thinking: RawThinkingConfig,
    /// Tool call block settings (Read, Search, ListDir, etc).
    pub tool: RawToolConfig,
    /// ListDir block settings.
    pub list_dir: RawListDirConfig,
    /// Execute tool call settings.
    pub execute: RawExecuteConfig,
}

/// Configuration for EditBlock rendering (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawEditBlockConfig {
    /// Whether to apply 2-char indent before line numbers.
    pub indent: bool,
    /// Whether to apply vertical padding.
    pub vpad: bool,
    /// Block background: "none", "dark", or "light".
    pub bg: RawBlockBackground,
    /// Whether to show background behind accent.
    pub accent_bg: bool,
    /// Accent color for vertical line.
    /// Use "none" to disable, or a color value.
    /// Formats: [r, g, b], "#rrggbb", "#rgb", or named color.
    /// Named: BLUE, CYAN, GREEN, YELLOW, ORANGE, RED, MAGENTA, COMMENT, etc.
    pub accent: OptionalColor,
    /// Whether diff line background extends to include gutter (line numbers).
    pub gutter_bg: bool,
    /// Whether to skip indent columns in diff line background (keep them clean).
    /// true = skip indent, false = include indent in background.
    pub indent_bg: bool,
    /// Show the +N/-M line summary in the collapsed header.
    /// Commented out (unset), it follows the `[ui] collapsed_edit_blocks`
    /// flag in config.toml; uncomment to pin either way.
    pub line_summary: Option<bool>,
    /// Start Edit blocks expanded (showing the diff) instead of as a
    /// collapsed one-line summary. Commented out (unset), it follows the
    /// `[ui] collapsed_edit_blocks` flag in config.toml (flag on =
    /// collapsed); uncomment to pin either way.
    pub expanded_by_default: Option<bool>,
    /// Separator between diff hunks. Options: "…" (default), "───", "⋯", "" (none).
    pub hunk_separator: Option<String>,
    /// Show two line-number columns (old + new) like GitHub's unified diff.
    /// When false (default), show a single column with the new-file line number.
    pub dual_line_numbers: bool,
}

impl Default for RawEditBlockConfig {
    fn default() -> Self {
        Self {
            indent: true,
            vpad: false,
            bg: RawBlockBackground::None,
            accent_bg: false,
            accent: OptionalColor::None,
            gutter_bg: false,
            indent_bg: false,
            line_summary: None,
            expanded_by_default: None,
            hunk_separator: Some("…".to_string()),
            dual_line_numbers: false,
        }
    }
}

/// Configuration for user prompt block rendering (TOML format).
///
/// This configures how user prompts appear inside the scrollback,
/// NOT the prompt editor widget (see [prompt] section).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawPromptConfig {
    /// Whether to apply vertical padding (blank lines above/below).
    pub vpad: bool,
    /// Block background: "none", "dark", or "light".
    pub bg: RawBlockBackground,
    /// Whether accent column gets block's background.
    pub accent_bg: bool,
    /// Minimum content lines to show in truncated/sticky header mode.
    /// This is the number of actual content lines, not including vpad.
    pub min_lines: u16,
    /// Show the ❯ prefix character before the prompt text.
    pub show_prefix: bool,
}

impl Default for RawPromptConfig {
    fn default() -> Self {
        Self {
            vpad: true,
            bg: RawBlockBackground::Light,
            accent_bg: false,
            min_lines: 2,
            show_prefix: true,
        }
    }
}

/// Configuration for thinking/reasoning block rendering (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawThinkingConfig {
    /// Accent color for the vertical line.
    /// Use "default" (or "none") for theme default, or a named/hex/RGB color.
    pub accent: OptionalColor,
    /// Whether accent line is shown. Set to false to hide accent in all modes.
    pub accent_enabled: bool,
    /// Blend factor for markdown colors with background (0-100).
    /// Lower values = more faded. 70 = 70% original color, 30% background.
    pub bg_blend: u8,
    /// Number of visual lines to show at start and end in truncated mode.
    /// If content exceeds this, shows first N lines, ellipsis, last N lines.
    pub truncated_lines: u16,
    /// Whether the accent line animates (traveling wave) while thinking is active.
    /// When false, the accent line is static.
    pub animate: bool,
    /// Show header line in truncated/expanded modes.
    /// When true: "Thinking..." (running) or "Thought for Xs" (done) appears
    /// as the first line above thinking content, with accent line and bullet.
    pub header: bool,
    /// When true, header uses brighter styling in non-collapsed modes (like tool titles).
    /// Respects muted_collapsed when collapsed. When false, header is always dim gray.
    pub header_bright: bool,
}

impl Default for RawThinkingConfig {
    fn default() -> Self {
        Self {
            accent: OptionalColor::None,
            accent_enabled: true,
            bg_blend: 70,
            truncated_lines: 3,
            animate: true,
            header: true,
            header_bright: false,
        }
    }
}

/// Configuration for tool call blocks (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawToolConfig {
    /// When true, collapsed tool calls render entirely in muted gray.
    /// When false, collapsed tool calls show normal colors.
    pub muted_collapsed: bool,
    /// When true, parenthetical details use the dimmest gray (gray_dim):
    /// Read "(1-50)", Search "(N matches)", Edit "(N edits)", Thinking "for Xs".
    /// When false, they use the normal muted gray.
    pub dim_details: bool,
    /// Bullet/icon before tool call headers.
    /// "none", "dot" (·), "small-circle" (•), "circle" (●),
    /// "small-triangle" (▸), "triangle" (▶), "diamond" (◆).
    pub bullet: RawToolBullet,
    // Note: bullet_accent and bullet_color removed — see ToolConfig comment.
}

impl Default for RawToolConfig {
    fn default() -> Self {
        Self {
            muted_collapsed: true,
            dim_details: true,
            bullet: RawToolBullet::Diamond,
        }
    }
}

/// Configuration for ListDir block (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawListDirConfig {
    /// When true, output has terminal-style dark background.
    /// When false, output has no background.
    pub terminal_bg: bool,
}

impl Default for RawListDirConfig {
    fn default() -> Self {
        Self {
            terminal_bg: true, // Default: dark background for output
        }
    }
}

/// Configuration for Execute tool call block (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default)]
pub struct RawExecuteConfig {
    /// Number of output lines to show at the start in truncated mode.
    /// In truncated mode: first_lines, then "…", then last_lines.
    pub first_lines: u16,
    /// Number of output lines to show at the end in truncated mode.
    pub last_lines: u16,
    /// Whether accent line is shown. Set to false to hide all accents
    /// (running/success/error).
    pub accent_enabled: bool,
    /// Accent color for running execute blocks (animated wave).
    /// Default: MAGENTA. Use named colors, hex, or RGB array.
    pub running_accent: OptionalColor,
    /// Header display style: "shell" = `$ command` (default), "label" = `Run command`.
    /// Shell style shows a dim `$` prompt. Label style shows bold "Run" like Edit/Search.
    pub header_style: RawExecuteHeaderStyle,
    /// When true, command text is muted/uncolored when collapsed.
    /// When false, command text keeps its color when collapsed.
    pub muted_command_collapsed: bool,
}

impl Default for RawExecuteConfig {
    fn default() -> Self {
        Self {
            first_lines: 2,
            last_lines: 3,
            accent_enabled: true,
            running_accent: OptionalColor::None,
            header_style: RawExecuteHeaderStyle::Label,
            muted_command_collapsed: true,
        }
    }
}

/// Raw header style for execute blocks (TOML format).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RawExecuteHeaderStyle {
    /// Shell style: `$ command` (default).
    #[default]
    Shell,
    /// Label style: `Run command` (like Edit/Search blocks).
    Label,
}

impl From<RawExecuteHeaderStyle> for ExecuteHeaderStyle {
    fn from(raw: RawExecuteHeaderStyle) -> Self {
        match raw {
            RawExecuteHeaderStyle::Shell => ExecuteHeaderStyle::Shell,
            RawExecuteHeaderStyle::Label => ExecuteHeaderStyle::Label,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RawBlockBackground {
    #[default]
    None,
    Dark,
    Light,
}

impl From<RawBlockBackground> for BlockBackground {
    fn from(raw: RawBlockBackground) -> Self {
        match raw {
            RawBlockBackground::None => BlockBackground::None,
            RawBlockBackground::Dark => BlockBackground::Dark,
            RawBlockBackground::Light => BlockBackground::Light,
        }
    }
}

// ============================================================================
// Raw → Runtime Conversion
// ============================================================================

impl From<RawAppearanceConfig> for AppearanceConfig {
    fn from(raw: RawAppearanceConfig) -> Self {
        Self {
            animation: raw.animation.into(),
            prompt: PromptViewConfig {
                collapse_unfocused: raw.prompt.collapse_unfocused,
                mouse_hover: raw.prompt.mouse_hover,
                show_prefix: raw.prompt.show_prefix,
                compact: false, // runtime-only, not persisted in TOML
            },
            scrollback: ScrollbackConfig {
                layout: raw.scrollback.layout.into(),
                scrollbar: raw.scrollback.scrollbar.into(),
                scroll: raw.scrollback.scroll.into(),
                blocks: raw.scrollback.blocks.into(),
                display: ScrollbackDisplayConfig {
                    line_under_last_entry: raw.scrollback.display.line_under_last_entry,
                    collapsed_accent_char: raw
                        .scrollback
                        .display
                        .collapsed_accent_char
                        .unwrap_or_else(|| crate::glyphs::collapsed_accent().to_string()),
                    dim_accent: raw.scrollback.display.dim_accent.unwrap_or(0.5),
                    group_selection_split: raw
                        .scrollback
                        .display
                        .group_selection_split
                        .unwrap_or(true),
                    highlight_overlays_border: raw
                        .scrollback
                        .display
                        .highlight_overlays_border
                        .unwrap_or(false),
                    expandable_indicator: raw
                        .scrollback
                        .display
                        .expandable_indicator
                        .unwrap_or(true),
                    expandable_indicator_running: raw
                        .scrollback
                        .display
                        .expandable_indicator_running
                        .unwrap_or(true),
                    expandable_indicator_char: raw
                        .scrollback
                        .display
                        .expandable_indicator_char
                        .unwrap_or_else(|| "›".to_string()),
                    selection_buttons: raw.scrollback.display.selection_buttons.unwrap_or(false),
                    sticky_headers: raw.scrollback.display.sticky_headers.unwrap_or(true),
                    tab_width: raw.scrollback.display.tab_width.unwrap_or(4),
                    group_max_visible: raw.scrollback.display.group_max_visible.unwrap_or(10),
                    rtl_bidi: raw.scrollback.display.rtl_bidi.unwrap_or(false),
                },
            },
            turn_status: TurnStatusConfig::default(),
            show_timestamps: true, // runtime-only, loaded from config.toml via persist
            // Single source: UiConfig::SHOW_TIMELINE_DEFAULT (loaded from config.toml via persist).
            show_timeline: UiConfig::SHOW_TIMELINE_DEFAULT,
            disable_plugins: raw.disable_plugins,
            show_plan_chip: raw.show_plan_chip,
            alt_screen: raw.terminal.alt_screen.into(),
            minimal: raw.terminal.minimal,
            minimal_live_rows: raw.terminal.minimal_live_rows.unwrap_or(10),
            minimal_max_commit_rows: raw.terminal.minimal_max_commit_rows.unwrap_or(2000),
            minimal_collapse_thinking: raw.terminal.minimal_collapse_thinking,
        }
    }
}

impl From<RawAnimationConfig> for AnimationConfig {
    fn from(raw: RawAnimationConfig) -> Self {
        Self {
            fps: raw.fps.clamp(1, 60),
            wave_rows: raw.wave_rows.max(1),
            show_fps: raw.show_fps,
        }
    }
}

impl From<RawFollowIndicator> for FollowIndicator {
    fn from(raw: RawFollowIndicator) -> Self {
        match raw {
            RawFollowIndicator::None => Self::None,
            RawFollowIndicator::Center => Self::Center,
        }
    }
}

impl From<RawToolBullet> for ToolBullet {
    fn from(raw: RawToolBullet) -> Self {
        match raw {
            RawToolBullet::None => Self::None,
            RawToolBullet::Dot => Self::Dot,
            RawToolBullet::SmallCircle => Self::SmallCircle,
            RawToolBullet::Circle => Self::Circle,
            RawToolBullet::SmallTriangle => Self::SmallTriangle,
            RawToolBullet::Triangle => Self::Triangle,
            RawToolBullet::Diamond => Self::Diamond,
        }
    }
}

impl From<RawScrollConfig> for ScrollConfig {
    fn from(raw: RawScrollConfig) -> Self {
        Self {
            margin: raw.margin,
            min_page_fraction: raw.min_page_fraction.min(100),
            follow_indicator: raw.follow_indicator.into(),
            follow_auto_select: raw.follow_auto_select,
            follow_by_overscroll: raw.follow_by_overscroll,
            anchor_on_fold: raw.anchor_on_fold,
            respect_manual_folds: raw.respect_manual_folds,
        }
    }
}

impl From<RawLayoutConfig> for LayoutConfig {
    fn from(raw: RawLayoutConfig) -> Self {
        Self {
            outer_vpad: raw.outer_vpad,
            outer_hpad_left: raw.outer_hpad_left,
            outer_hpad_right: raw.outer_hpad_right,
            block_pad_left: raw.block_pad_left,
            block_pad_right: raw.block_pad_right,
        }
        .validated()
    }
}

impl From<RawScrollbarConfig> for ScrollbarConfig {
    fn from(raw: RawScrollbarConfig) -> Self {
        Self {
            enabled: raw.enabled,
            gap_left: raw.gap_left,
            gap_right: raw.gap_right,
            scrollbar_bg: raw.scrollbar_bg.to_option(),
            scrollbar_fg: raw.scrollbar_fg.to_option(),
        }
    }
}

impl From<RawBlocksConfig> for BlocksConfig {
    fn from(raw: RawBlocksConfig) -> Self {
        Self {
            edit: raw.edit.into(),
            prompt: raw.prompt.into(),
            thinking: raw.thinking.into(),
            tool: raw.tool.into(),
            list_dir: raw.list_dir.into(),
            execute: raw.execute.into(),
        }
    }
}

impl From<RawToolConfig> for ToolConfig {
    fn from(raw: RawToolConfig) -> Self {
        Self {
            muted_collapsed: raw.muted_collapsed,
            dim_details: raw.dim_details,
            bullet: raw.bullet.into(),
        }
    }
}

impl From<RawListDirConfig> for ListDirConfig {
    fn from(raw: RawListDirConfig) -> Self {
        Self {
            terminal_bg: raw.terminal_bg,
        }
    }
}

impl From<RawExecuteConfig> for ExecuteConfig {
    fn from(raw: RawExecuteConfig) -> Self {
        // Default accent is accent_running from theme — already quantized.
        let default_accent = crate::theme::Theme::current().accent_running;
        let accent = raw.running_accent.to_option().unwrap_or(default_accent);
        Self {
            first_lines: raw.first_lines.max(1),
            last_lines: raw.last_lines.max(1),
            accent_enabled: raw.accent_enabled,
            running_accent: accent,
            header_style: raw.header_style.into(),
            muted_command_collapsed: raw.muted_command_collapsed,
        }
    }
}

impl From<RawEditBlockConfig> for EditBlockConfig {
    fn from(raw: RawEditBlockConfig) -> Self {
        Self {
            indent: raw.indent,
            vpad: raw.vpad,
            bg: raw.bg.into(),
            accent_bg: raw.accent_bg,
            accent: raw.accent.to_option(),
            gutter_bg: raw.gutter_bg,
            indent_bg: raw.indent_bg,
            line_summary: raw.line_summary,
            expanded_by_default: raw.expanded_by_default,
            hunk_separator: raw.hunk_separator.unwrap_or_else(|| "…".to_string()),
            dual_line_numbers: raw.dual_line_numbers,
        }
    }
}

impl From<RawPromptConfig> for PromptConfig {
    fn from(raw: RawPromptConfig) -> Self {
        Self {
            vpad: raw.vpad,
            bg: raw.bg.into(),
            accent_bg: raw.accent_bg,
            min_lines: raw.min_lines,
            show_prefix: raw.show_prefix,
        }
    }
}

impl From<RawThinkingConfig> for ThinkingConfig {
    fn from(raw: RawThinkingConfig) -> Self {
        // Default accent is gray_dim from theme — already quantized.
        let default_accent = crate::theme::Theme::current().gray_dim;
        let accent = raw.accent.to_option().unwrap_or(default_accent);
        // Convert 0-100 integer to 0.0-1.0 float
        let bg_blend = (raw.bg_blend.min(100) as f32) / 100.0;
        Self {
            accent,
            accent_enabled: raw.accent_enabled,
            bg_blend,
            truncated_lines: raw.truncated_lines.max(1),
            animate: raw.animate,
            header: raw.header,
            header_bright: raw.header_bright,
            body_dim_italic: false,
            collapsed_expand_hint: false,
        }
    }
}

// ============================================================================
// Color Parsing
// ============================================================================

/// An optional color that can be "none" or a color value.
/// This allows TOML to represent None values explicitly.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum OptionalColor {
    #[default]
    None,
    Some(Color),
}

impl OptionalColor {
    /// Convert to `Option<Color>`, quantizing to the terminal's color level.
    ///
    /// User-configured colors arrive as raw RGB from TOML. Unlike theme
    /// colors (which are pre-quantized by [`Theme::current()`]), these
    /// need quantization here to work correctly on 256-color terminals.
    pub fn to_option(&self) -> Option<Color> {
        match self {
            OptionalColor::None => None,
            OptionalColor::Some(c) => Some(crate::theme::quantize(*c)),
        }
    }

    /// Convert to `Option<Color>` without quantization.
    ///
    /// Returns the raw parsed color value. Used for serialization
    /// round-trip tests and anywhere the original RGB is needed.
    pub fn to_option_raw(&self) -> Option<Color> {
        match self {
            OptionalColor::None => None,
            OptionalColor::Some(c) => Some(*c),
        }
    }
}

impl Serialize for OptionalColor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            OptionalColor::None => serializer.serialize_str("none"),
            OptionalColor::Some(Color::Rgb(r, g, b)) => [*r, *g, *b].serialize(serializer),
            OptionalColor::Some(Color::Indexed(n)) => {
                // Convert indexed to RGB for serialization so the TOML round-trips.
                let (r, g, b) = crate::render::color::indexed_to_rgb(*n);
                [r, g, b].serialize(serializer)
            }
            OptionalColor::Some(_) => serializer.serialize_str("unknown"),
        }
    }
}

impl<'de> Deserialize<'de> for OptionalColor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = RawColorValue::deserialize(deserializer)?;
        match value {
            RawColorValue::Rgb([r, g, b]) => Ok(OptionalColor::Some(Color::Rgb(r, g, b))),
            RawColorValue::Text(s) => {
                let s = s.trim().to_lowercase();
                if s == "none" || s == "null" {
                    Ok(OptionalColor::None)
                } else {
                    parse_color_string(&s)
                        .map(OptionalColor::Some)
                        .map_err(serde::de::Error::custom)
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawColorValue {
    Rgb([u8; 3]),
    Text(String),
}

fn parse_color_string(s: &str) -> Result<Color, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    lookup_named_color(s)
}

fn parse_hex_color(hex: &str) -> Result<Color, String> {
    let hex = hex.trim_start_matches('#');
    let (r, g, b) = match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).map_err(|e| e.to_string())? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).map_err(|e| e.to_string())? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).map_err(|e| e.to_string())? * 17;
            (r, g, b)
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
            (r, g, b)
        }
        _ => return Err(format!("invalid hex color: #{hex}")),
    };
    Ok(Color::Rgb(r, g, b))
}

fn lookup_named_color(name: &str) -> Result<Color, String> {
    // Named colors use the EchoAgentNight RGB palette. They are quantized via
    // `parse_color_string` → `quantize()` to match the terminal's capabilities.
    let color = match name.to_uppercase().as_str() {
        // Background colors
        "BG" | "BG_BASE" => Color::Rgb(20, 20, 20), // #141414
        "BG_LIGHT" | "BG_HIGHLIGHT" => Color::Rgb(30, 30, 30), // #1e1e1e
        "BG_DARK" => Color::Rgb(17, 17, 17),        // #111111
        "BG_TERMINAL" | "BG_NIGHT" => Color::Rgb(10, 10, 10), // #0a0a0a
        "BG_VISUAL" => Color::Rgb(30, 32, 45),      // blue-tinted selection
        "BG_SEARCH" => Color::Rgb(48, 48, 52),      // #303034

        // Accent colors (TokyoNight Night)
        "BLUE" => Color::Rgb(77, 121, 255),          // #4D79FF
        "BLUE0" => Color::Rgb(61, 89, 161),          // #3d59a1
        "BLUE1" => Color::Rgb(42, 195, 222),         // #2ac3de
        "BLUE2" => Color::Rgb(13, 185, 215),         // #0db9d7
        "BLUE5" => Color::Rgb(137, 221, 255),        // #89ddff
        "BLUE6" => Color::Rgb(180, 249, 248),        // #b4f9f8
        "BLUE7" => Color::Rgb(57, 75, 112),          // #394b70
        "CYAN" => Color::Rgb(125, 207, 255),         // #7dcfff
        "GREEN" => Color::Rgb(36, 196, 116),         // #24C474
        "GREEN1" => Color::Rgb(115, 218, 202),       // #73daca
        "GREEN2" => Color::Rgb(65, 166, 181),        // #41a6b5
        "YELLOW" => Color::Rgb(255, 219, 141),       // #FFDB8D
        "ORANGE" => Color::Rgb(255, 158, 100),       // #ff9e64
        "RED" => Color::Rgb(248, 114, 122),          // #F8727A
        "RED1" => Color::Rgb(219, 75, 75),           // #db4b4b
        "MAGENTA" => Color::Rgb(187, 154, 247),      // #bb9af7
        "PURPLE" => Color::Rgb(131, 113, 211),       // #8371D3
        "MAGENTA2" => Color::Rgb(255, 0, 124),       // #ff007c
        "TEAL" | "HINT" => Color::Rgb(26, 188, 156), // #1abc9c

        // Text colors
        "FG" | "TEXT" | "TEXT_PRIMARY" => Color::Rgb(243, 243, 243), // #f3f3f3
        "FG_DARK" | "TEXT_SECONDARY" => Color::Rgb(200, 200, 200),   // #c8c8c8
        "FG_GUTTER" => Color::Rgb(65, 65, 65),                       // #414141
        "COMMENT" | "MUTED" | "TEXT_MUTED" => Color::Rgb(98, 98, 98), // #626262
        "DARK3" => Color::Rgb(90, 90, 90),                           // #5a5a5a
        "DARK5" | "TOOL" => Color::Rgb(120, 120, 120),               // #787878

        // Semantic colors
        "ERROR" => Color::Rgb(247, 118, 142),   // RED
        "SUCCESS" => Color::Rgb(158, 206, 106), // GREEN
        "WARNING" => Color::Rgb(224, 175, 104), // YELLOW
        "INFO" => Color::Rgb(125, 207, 255),    // CYAN

        // Basic colors
        "BLACK" => Color::Black,
        "WHITE" => Color::White,
        "GRAY" | "GREY" => Color::Gray,

        _ => return Err(format!("unknown color name: {name}")),
    };
    Ok(color)
}

// ============================================================================
// TOML Generation with Comments
// ============================================================================

impl RawAppearanceConfig {
    pub fn to_toml_with_comments() -> String {
        let mut config = Self::default();
        // Template-only materialization: these default to None (the shell's
        // `[ui] collapsed_edit_blocks` flag decides), which the serializer
        // would omit entirely. Show the flag-off shape as commented lines so
        // the keys stay discoverable; commenting-out below keeps them inert.
        config.scrollback.blocks.edit.expanded_by_default = Some(true);
        config.scrollback.blocks.edit.line_summary = Some(false);
        let toml_str = toml_edit::ser::to_string_pretty(&config).expect("serialize default");
        let mut doc: DocumentMut = toml_str.parse().expect("parse toml");

        let pager_path = crate::util::display_user_echo_agent_path("pager.toml");
        let header = format!(
            "\
# EchoAgent Pager Appearance Configuration ({pager_path})
# Every value below is a commented-out built-in default: uncomment a line and
# save to override it. Values left commented track future default changes.
# Delete the file to regenerate this template.
#
# ═══════════════════════════════════════════════════════════════════════════════
# TOKYO NIGHT STORM COLOR PALETTE
# ═══════════════════════════════════════════════════════════════════════════════
#
# Colors can be specified as:
#   - Named color: \"BLUE\", \"cyan\", \"Comment\" (case-insensitive)
#   - Hex string:  \"#7aa2f7\" or \"#f7f\" (3 or 6 digits)
#   - RGB array:   [122, 162, 247]
#
# Background colors:
#   BG, BG_BASE       #141414  (neutral background)
#   BG_LIGHT          #1c1c1c  (highlighted background)
#   BG_DARK           #111111  (darker background)
#   BG_TERMINAL       #0e0e0e  (terminal background)
#   BG_VISUAL         #262628  (visual selection background)
#   BG_SEARCH         #303034  (search highlight background)
#
# Primary colors:
#   BLUE              #7aa2f7
#   BLUE0             #3d59a1  (dark blue, used for search/visual)
#   BLUE1             #2ac3de  (bright cyan-blue)
#   BLUE2             #0db9d7  (teal-blue)
#   BLUE5             #89ddff  (light cyan)
#   BLUE6             #b4f9f8  (pale cyan)
#   BLUE7             #394b70  (dark muted blue)
#   CYAN              #7dcfff
#   GREEN             #9ece6a
#   GREEN1            #73daca  (teal-green)
#   GREEN2            #41a6b5  (dark teal)
#   YELLOW            #e0af68
#   ORANGE            #ff9e64
#   RED               #f7768e
#   RED1              #db4b4b  (dark red)
#   MAGENTA           #bb9af7
#   PURPLE            #9d7cd8
#   MAGENTA2          #ff007c  (hot pink)
#   TEAL, HINT        #1abc9c
#
# Text colors:
#   FG, TEXT          #c8c8c8  (primary text)
#   FG_DARK           #b2b2b2  (secondary text)
#   FG_GUTTER         #414141  (line number gutter)
#   COMMENT, MUTED    #5f5f5f  (muted/comment text)
#   DARK3             #5a5a5a  (medium gray)
#   DARK5, TOOL       #787878  (tool accent, system prompt)
#
# Semantic colors:
#   ERROR             #f7768e  (same as RED)
#   SUCCESS           #9ece6a  (same as GREEN)
#   WARNING           #e0af68  (same as YELLOW)
#   INFO              #7dcfff  (same as CYAN)
#
# ═══════════════════════════════════════════════════════════════════════════════

"
        );

        if let Some(terminal) = doc.get_mut("terminal").and_then(Item::as_table_mut) {
            annotate_table::<RawTerminalConfig>(terminal);
        }

        if let Some(animation) = doc.get_mut("animation").and_then(Item::as_table_mut) {
            annotate_table::<RawAnimationConfig>(animation);
        }

        if let Some(prompt) = doc.get_mut("prompt").and_then(Item::as_table_mut) {
            annotate_table::<RawPromptViewConfig>(prompt);
        }

        if let Some(scrollback) = doc.get_mut("scrollback").and_then(Item::as_table_mut) {
            if let Some(display) = scrollback.get_mut("display").and_then(Item::as_table_mut) {
                annotate_table::<RawScrollbackDisplayConfig>(display);
            }
            if let Some(layout) = scrollback.get_mut("layout").and_then(Item::as_table_mut) {
                annotate_table::<RawLayoutConfig>(layout);
            }
            if let Some(scrollbar) = scrollback.get_mut("scrollbar").and_then(Item::as_table_mut) {
                annotate_table::<RawScrollbarConfig>(scrollbar);
            }
            if let Some(scroll) = scrollback.get_mut("scroll").and_then(Item::as_table_mut) {
                annotate_table::<RawScrollConfig>(scroll);
            }
            if let Some(blocks) = scrollback.get_mut("blocks").and_then(Item::as_table_mut) {
                if let Some(edit) = blocks.get_mut("edit").and_then(Item::as_table_mut) {
                    annotate_table::<RawEditBlockConfig>(edit);
                }
                if let Some(prompt) = blocks.get_mut("prompt").and_then(Item::as_table_mut) {
                    annotate_table::<RawPromptConfig>(prompt);
                }
                if let Some(thinking) = blocks.get_mut("thinking").and_then(Item::as_table_mut) {
                    annotate_table::<RawThinkingConfig>(thinking);
                }
                if let Some(tool) = blocks.get_mut("tool").and_then(Item::as_table_mut) {
                    annotate_table::<RawToolConfig>(tool);
                }
                if let Some(list_dir) = blocks.get_mut("list_dir").and_then(Item::as_table_mut) {
                    annotate_table::<RawListDirConfig>(list_dir);
                }
                if let Some(execute) = blocks.get_mut("execute").and_then(Item::as_table_mut) {
                    annotate_table::<RawExecuteConfig>(execute);
                }
            }
        }

        format!("{header}{}", comment_out_values(&doc.to_string()))
    }
}

/// Comment out every key-value line, keeping section headers and comments.
/// The generated template documents defaults without pinning them, so a
/// future built-in default change reaches installs holding an old file
/// (active values would freeze the defaults of the day forever).
fn comment_out_values(toml: &str) -> String {
    let mut out = String::with_capacity(toml.len() + 256);
    for line in toml.lines() {
        let trimmed = line.trim_start();
        if !(trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[')) {
            out.push_str("# ");
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Serializes the pager.toml read-modify-write so two rapid settings
/// toggles can't interleave and clobber each other (mirrors the shell's
/// `save_config` `SAVE_LOCK`).
static PAGER_TOML_SAVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn persist_respect_manual_folds(enabled: bool) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    if echo_agent_config::user_echo_agent_home().is_none() {
        return Err(Error::new(
            ErrorKind::NotFound,
            "no user echoagent home resolved; refusing to write a cwd-relative pager.toml \
             that startup would never read",
        ));
    }
    let _guard = PAGER_TOML_SAVE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let path = crate::util::pager_toml_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let updated = upsert_respect_manual_folds(&content, enabled)
        .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    #[cfg(unix)]
    let prior_mode: Option<u32> = std::fs::metadata(&path).ok().map(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode()
    });

    let suffix = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("toml.tmp.{}.{}", std::process::id(), nanos)
    };
    let tmp = path.with_extension(suffix);
    std::fs::write(&tmp, updated)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = prior_mode {
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
        }
    }
    std::fs::rename(&tmp, &path)
}

fn upsert_respect_manual_folds(content: &str, enabled: bool) -> Result<String, String> {
    let mut doc: DocumentMut = content
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let scrollback = doc
        .entry("scrollback")
        .or_insert_with(implicit_table)
        .as_table_mut()
        .ok_or_else(|| "pager.toml `scrollback` is not a table".to_string())?;
    let scroll = scrollback
        .entry("scroll")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| "pager.toml `scrollback.scroll` is not a table".to_string())?;
    scroll.insert("respect_manual_folds", toml_edit::value(enabled));
    Ok(doc.to_string())
}

fn implicit_table() -> Item {
    let mut table = toml_edit::Table::new();
    table.set_implicit(true);
    Item::Table(table)
}

fn annotate_table<T: DocumentedFields>(table: &mut toml_edit::Table) {
    for (mut key, _value) in table.iter_mut() {
        let field_name = key.get();
        if let Ok(docs) = T::get_field_docs(field_name) {
            let comment: String = docs
                .lines()
                .map(|l| {
                    if l.is_empty() {
                        "#\n".to_string()
                    } else {
                        format!("# {l}\n")
                    }
                })
                .collect();
            let decor = key.leaf_decor_mut();
            let prefix = decor.prefix().and_then(RawString::as_str).unwrap_or("");
            decor.set_prefix(format!("{prefix}{comment}"));
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_color() {
        assert_eq!(parse_hex_color("ff8800"), Ok(Color::Rgb(255, 136, 0)));
        assert_eq!(parse_hex_color("f80"), Ok(Color::Rgb(255, 136, 0)));
        assert_eq!(parse_hex_color("000"), Ok(Color::Rgb(0, 0, 0)));
        assert_eq!(parse_hex_color("fff"), Ok(Color::Rgb(255, 255, 255)));
    }

    #[test]
    fn test_parse_named_color() {
        assert_eq!(lookup_named_color("BLUE"), Ok(Color::Rgb(77, 121, 255)));
        assert_eq!(lookup_named_color("blue"), Ok(Color::Rgb(77, 121, 255)));
        assert!(lookup_named_color("NOTACOLOR").is_err());
    }

    #[test]
    fn test_deserialize_rgb_array() {
        let toml = r#"color = [255, 128, 0]"#;
        #[derive(Deserialize)]
        struct Test {
            color: OptionalColor,
        }
        let t: Test = toml::from_str(toml).unwrap();
        assert_eq!(t.color.to_option_raw(), Some(Color::Rgb(255, 128, 0)));
    }

    #[test]
    fn test_deserialize_hex_string() {
        let toml = r##"color = "#ff8800""##;
        #[derive(Deserialize)]
        struct Test {
            color: OptionalColor,
        }
        let t: Test = toml::from_str(toml).unwrap();
        assert_eq!(t.color.to_option_raw(), Some(Color::Rgb(255, 136, 0)));
    }

    #[test]
    fn test_deserialize_named_color() {
        let toml = r#"color = "BLUE""#;
        #[derive(Deserialize)]
        struct Test {
            color: OptionalColor,
        }
        let t: Test = toml::from_str(toml).unwrap();
        assert_eq!(t.color.to_option_raw(), Some(Color::Rgb(77, 121, 255)));
    }

    #[test]
    fn test_deserialize_none_color() {
        let toml = r#"color = "none""#;
        #[derive(Deserialize)]
        struct Test {
            color: OptionalColor,
        }
        let t: Test = toml::from_str(toml).unwrap();
        assert_eq!(t.color.to_option(), None);
    }

    #[test]
    fn test_deserialize_full_config() {
        let toml = r#"
[scrollback.blocks.edit]
indent = false
vpad = true
bg = "dark"
accent_bg = true
accent = "CYAN"
gutter_bg = true
"#;
        let raw: RawAppearanceConfig = toml::from_str(toml).unwrap();
        let cfg: AppearanceConfig = raw.into();
        assert!(!cfg.scrollback.blocks.edit.indent);
        assert!(cfg.scrollback.blocks.edit.vpad);
        assert_eq!(cfg.scrollback.blocks.edit.bg, BlockBackground::Dark);
        assert!(cfg.scrollback.blocks.edit.accent_bg);
        // CYAN from lookup_named_color — quantized by OptionalColor::to_option().
        // In non-TTY test environments, quantize() may downgrade to ANSI 16,
        // so compare against the quantized value.
        assert!(cfg.scrollback.blocks.edit.accent.is_some());
        assert!(cfg.scrollback.blocks.edit.gutter_bg);
    }

    #[test]
    fn test_default_config() {
        let cfg = AppearanceConfig::default();
        assert!(cfg.scrollback.blocks.edit.indent);
        assert!(!cfg.scrollback.blocks.edit.vpad);
        assert_eq!(cfg.scrollback.blocks.edit.bg, BlockBackground::None);
    }

    /// Legacy configs still contain the removed `invert` key (it was written
    /// by old generated `pager.toml`s); parsing must keep ignoring it.
    #[test]
    fn removed_prompt_invert_key_is_ignored() {
        let raw: RawAppearanceConfig =
            toml::from_str("[scrollback.blocks.prompt]\ninvert = true\nvpad = false").unwrap();
        let cfg: AppearanceConfig = raw.into();
        assert!(!cfg.scrollback.blocks.prompt.vpad);
    }

    #[test]
    fn test_respect_manual_folds_defaults_off_and_round_trips() {
        let cfg = AppearanceConfig::default();
        assert!(!cfg.scrollback.scroll.respect_manual_folds);

        let raw: RawAppearanceConfig =
            toml::from_str("[scrollback.scroll]\nrespect_manual_folds = true").unwrap();
        let cfg: AppearanceConfig = raw.into();
        assert!(cfg.scrollback.scroll.respect_manual_folds);
    }

    #[test]
    fn test_upsert_respect_manual_folds_writes_key_and_preserves_siblings() {
        let updated = upsert_respect_manual_folds("", true).unwrap();
        assert!(
            updated.contains("[scrollback.scroll]"),
            "missing section must render in section form, not as inline tables:\n{updated}"
        );
        let raw: RawAppearanceConfig = toml::from_str(&updated).unwrap();
        let cfg: AppearanceConfig = raw.into();
        assert!(
            cfg.scrollback.scroll.respect_manual_folds,
            "empty pager.toml: upserted key must parse back as true:\n{updated}"
        );

        let existing = "[scrollback.scroll]\nanchor_on_fold = false\nrespect_manual_folds = true\n";
        let updated = upsert_respect_manual_folds(existing, false).unwrap();
        let raw: RawAppearanceConfig = toml::from_str(&updated).unwrap();
        let cfg: AppearanceConfig = raw.into();
        assert!(
            !cfg.scrollback.scroll.respect_manual_folds,
            "key updated in place"
        );
        assert!(
            !cfg.scrollback.scroll.anchor_on_fold,
            "sibling keys must be preserved:\n{updated}"
        );

        assert!(
            upsert_respect_manual_folds("scrollback = 5", true).is_err(),
            "non-table `scrollback` must be a hard error, not a panic"
        );
        assert!(
            upsert_respect_manual_folds("[scrollback]\nscroll = 5", true).is_err(),
            "non-table `scrollback.scroll` must be a hard error, not a panic"
        );
    }

    #[test]
    fn test_to_toml_with_comments() {
        let toml = RawAppearanceConfig::to_toml_with_comments();
        // Check scrollback sections — check for key names rather than section
        // headers since toml_edit may format subtables differently when the
        // parent table has inline keys (like line_under_last_entry).
        assert!(
            toml.contains("margin = "),
            "Missing scroll margin in:\n{toml}"
        );
        assert!(
            toml.contains("min_page_fraction = "),
            "Missing scroll min_page_fraction in:\n{toml}"
        );
        assert!(
            toml.contains("follow_indicator = "),
            "Missing follow_indicator in:\n{toml}"
        );
        assert!(
            toml.contains("follow_auto_select = "),
            "Missing follow_auto_select in:\n{toml}"
        );
        assert!(
            toml.contains("follow_by_overscroll = "),
            "Missing follow_by_overscroll in:\n{toml}"
        );
        assert!(
            toml.contains("respect_manual_folds = "),
            "Missing respect_manual_folds in:\n{toml}"
        );
        assert!(
            toml.contains("# Opt-in: keep manually folded blocks as-is"),
            "Missing respect_manual_folds comment in:\n{toml}"
        );
        assert!(
            toml.contains("line_under_last_entry"),
            "Missing line_under_last_entry in:\n{toml}"
        );
        // Check tool bullet fields
        assert!(
            toml.contains("bullet = "),
            "Missing tool bullet in:\n{toml}"
        );
        // Note: bullet_color and bullet_accent removed in scrollback-v2 refactor.
        assert!(
            toml.contains("outer_vpad = "),
            "Missing layout outer_vpad in:\n{toml}"
        );
        assert!(
            toml.contains("enabled = "),
            "Missing scrollbar enabled in:\n{toml}"
        );
        // Check animation section
        assert!(
            toml.contains("[animation]"),
            "Missing [animation] section in:\n{toml}"
        );
        assert!(toml.contains("fps = "), "Missing animation fps in:\n{toml}");
        assert!(
            toml.contains("wave_rows = "),
            "Missing animation wave_rows in:\n{toml}"
        );
        // Check edit block comments
        assert!(toml.contains("# Whether to apply 2-char indent"));
        assert!(toml.contains("indent = true"));
        // Check prompt block comments
        assert!(
            toml.contains("# Whether to apply vertical padding"),
            "Missing prompt vpad comment in:\n{toml}"
        );
        assert!(
            toml.contains("# Minimum content lines to show"),
            "Missing prompt min_lines comment in:\n{toml}"
        );
        // Check prompt view section
        assert!(
            toml.contains("[prompt]"),
            "Missing [prompt] section in:\n{toml}"
        );
        assert!(
            toml.contains("collapse_unfocused = "),
            "Missing collapse_unfocused in:\n{toml}"
        );
        assert!(
            toml.contains("mouse_hover = "),
            "Missing mouse_hover in:\n{toml}"
        );
        // Check blocks are under scrollback
        assert!(
            toml.contains("[scrollback.blocks.edit]"),
            "Missing [scrollback.blocks.edit] section in:\n{toml}"
        );
        assert!(
            toml.contains("[scrollback.blocks.execute]"),
            "Missing [scrollback.blocks.execute] section in:\n{toml}"
        );
        // Check execute block has running_accent
        assert!(
            toml.contains("running_accent = "),
            "Missing execute running_accent in:\n{toml}"
        );
        // Ensure old top-level sections are gone
        assert!(
            !toml.contains("\n[scroll]\n"),
            "Old [scroll] section should not exist:\n{toml}"
        );
        assert!(
            !toml.contains("[prompt_input]"),
            "Old [prompt_input] section should not exist:\n{toml}"
        );
        assert!(
            !toml.contains("\n[mouse]\n"),
            "Old [mouse] section should not exist:\n{toml}"
        );
        assert!(
            !toml.contains("\n[blocks."),
            "Old top-level [blocks.*] should not exist:\n{toml}"
        );
    }

    /// The generated template must be inert: parsing it (and an empty file)
    /// yields exactly the built-in defaults, so old dev-generated files can
    /// never pin a superseded default again.
    #[test]
    fn template_parses_to_builtin_defaults() {
        let serialize = |cfg: &RawAppearanceConfig| toml_edit::ser::to_string_pretty(cfg).unwrap();
        let defaults = serialize(&RawAppearanceConfig::default());

        let template = RawAppearanceConfig::to_toml_with_comments();
        let parsed: RawAppearanceConfig = toml::from_str(&template).expect("template must parse");
        assert_eq!(
            serialize(&parsed),
            defaults,
            "inert template must round-trip to built-in defaults"
        );

        let empty: RawAppearanceConfig = toml::from_str("").expect("empty config must parse");
        assert_eq!(serialize(&empty), defaults);
    }

    /// Every value line in the template is commented out; only section
    /// headers (empty tables) and comments are active.
    #[test]
    fn template_has_no_active_value_lines() {
        let template = RawAppearanceConfig::to_toml_with_comments();
        for line in template.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
                continue;
            }
            panic!("active value line in generated template: {line:?}");
        }
    }

    /// The single policy point pairing the pager.toml shape keys with the
    /// shell-owned `collapsed_edit_blocks` flag: unset keys follow the flag
    /// (on = collapsed one-liner with diffstat, off = legacy expanded diff
    /// without it); explicit values pin the shape in both directions.
    #[test]
    fn effective_edit_shape_follows_flag_unless_pinned() {
        let unset = EditBlockConfig::default();
        assert!(unset.effective_expanded(false), "flag off: expanded");
        assert!(
            !unset.effective_line_summary(false),
            "flag off: no diffstat"
        );
        assert!(!unset.effective_expanded(true), "flag on: collapsed");
        assert!(unset.effective_line_summary(true), "flag on: diffstat");

        let pinned = EditBlockConfig {
            expanded_by_default: Some(true),
            line_summary: Some(true),
            ..EditBlockConfig::default()
        };
        assert!(
            pinned.effective_expanded(true),
            "explicit expanded beats the flag"
        );
        assert!(
            pinned.effective_line_summary(false),
            "explicit diffstat beats the flag"
        );
        let pinned = EditBlockConfig {
            expanded_by_default: Some(false),
            line_summary: Some(false),
            ..EditBlockConfig::default()
        };
        assert!(
            !pinned.effective_expanded(false),
            "explicit collapse beats the flag"
        );
        assert!(
            !pinned.effective_line_summary(true),
            "explicit no-diffstat beats the flag"
        );
    }

    /// The two flag-deferred edit keys default to `None` (omitted by the
    /// serializer), but the template must still document them as commented
    /// lines showing the flag-off shape.
    #[test]
    fn template_documents_flag_deferred_edit_keys() {
        let template = RawAppearanceConfig::to_toml_with_comments();
        assert!(
            template.contains("# expanded_by_default = true"),
            "expanded_by_default missing from template:\n{template}"
        );
        assert!(
            template.contains("# line_summary = false"),
            "line_summary missing from template:\n{template}"
        );
        assert!(
            template.contains("collapsed_edit_blocks"),
            "doc comments must point at the [ui] flag:\n{template}"
        );
    }

    /// The `/settings` writer must insert an ACTIVE key into the inert
    /// template (whose keys are all comments) rather than edit a commented
    /// line or fail.
    #[test]
    fn upsert_into_inert_template_inserts_active_key() {
        let template = RawAppearanceConfig::to_toml_with_comments();
        let updated = upsert_respect_manual_folds(&template, true).expect("upsert");
        let parsed: RawAppearanceConfig =
            toml::from_str(&updated).expect("updated template must parse");
        assert!(
            parsed.scrollback.scroll.respect_manual_folds,
            "inserted key must be active:\n{updated}"
        );
    }

    // ── Terminal config (alt_screen) parsing ─────────────────────

    #[test]
    fn terminal_alt_screen_auto_default() {
        let raw: RawAppearanceConfig = toml::from_str("").unwrap();
        assert_eq!(raw.terminal.alt_screen, RawAltScreenMode::Auto);
    }

    #[test]
    fn terminal_alt_screen_never() {
        let raw: RawAppearanceConfig =
            toml::from_str("[terminal]\nalt_screen = \"never\"").unwrap();
        assert_eq!(raw.terminal.alt_screen, RawAltScreenMode::Never);
    }

    #[test]
    fn terminal_alt_screen_always() {
        let raw: RawAppearanceConfig =
            toml::from_str("[terminal]\nalt_screen = \"always\"").unwrap();
        assert_eq!(raw.terminal.alt_screen, RawAltScreenMode::Always);
    }

    #[test]
    fn terminal_alt_screen_auto_explicit() {
        let raw: RawAppearanceConfig = toml::from_str("[terminal]\nalt_screen = \"auto\"").unwrap();
        assert_eq!(raw.terminal.alt_screen, RawAltScreenMode::Auto);
    }

    #[test]
    fn terminal_config_to_runtime_conversion() {
        use crate::terminal::AltScreenMode;

        let raw = RawAltScreenMode::Auto;
        assert_eq!(AltScreenMode::from(raw), AltScreenMode::Auto);

        let raw = RawAltScreenMode::Always;
        assert_eq!(AltScreenMode::from(raw), AltScreenMode::Always);

        let raw = RawAltScreenMode::Never;
        assert_eq!(AltScreenMode::from(raw), AltScreenMode::Never);
    }

    #[test]
    fn terminal_section_appears_in_generated_toml() {
        let toml = RawAppearanceConfig::to_toml_with_comments();
        assert!(
            toml.contains("alt_screen = "),
            "Missing alt_screen in generated config:\n{toml}"
        );
    }

    /// A config written before the key existed must still parse and keep K9.
    #[test]
    fn minimal_collapse_thinking_defaults_off_and_old_configs_parse() {
        let empty: RawAppearanceConfig = toml::from_str("").expect("empty config must parse");
        assert!(!empty.terminal.minimal_collapse_thinking);
        assert!(!AppearanceConfig::from(empty).minimal_collapse_thinking);

        let legacy: RawAppearanceConfig =
            toml::from_str("[terminal]\nminimal = true\nminimal_live_rows = 12\n")
                .expect("legacy config must parse");
        let cfg: AppearanceConfig = legacy.into();
        assert!(cfg.minimal);
        assert_eq!(cfg.minimal_live_rows, 12);
        assert!(
            !cfg.minimal_collapse_thinking,
            "a config written before the key existed must keep the K9 default"
        );

        assert!(!AppearanceConfig::default().minimal_collapse_thinking);
    }

    #[test]
    fn minimal_collapse_thinking_opt_in_parses() {
        let raw: RawAppearanceConfig =
            toml::from_str("[terminal]\nminimal_collapse_thinking = true\n").unwrap();
        assert!(AppearanceConfig::from(raw).minimal_collapse_thinking);
    }

    /// The reasoning-legibility toggles must stay un-settable from pager.toml.
    #[test]
    fn thinking_body_treatment_is_off_by_default_and_not_a_toml_key() {
        let cfg = AppearanceConfig::default();
        assert!(!cfg.scrollback.blocks.thinking.body_dim_italic);
        assert!(!cfg.scrollback.blocks.thinking.collapsed_expand_hint);

        let template = RawAppearanceConfig::to_toml_with_comments();
        assert!(!template.contains("body_dim_italic"));
        assert!(!template.contains("collapsed_expand_hint"));
    }
}
