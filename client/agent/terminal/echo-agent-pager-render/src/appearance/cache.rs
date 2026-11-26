//! Thread-local caches for the pager's UI settings.
//!
//! Read on every render frame — must be cheaper than re-loading from
//! `config.toml`. Call [`prime`] at startup so the first frame never
//! hits disk; the lazy fallback seeds from `load_effective_config()`
//! on first read as a safety net.
//!
//! Disk writes live in `echo_agent_runtime::util::config::set_<field>()`.
//! This module is a pager-side in-memory cache only.
//!
//! Default consts are hardcoded for `Cell::new` (`const` required) and
//! asserted in tests to match `UiConfig::default()`.
//!
//! Thread-local `Cell<bool>` is safe because TUI render + dispatch run
//! on a single thread. Multi-thread use would require `AtomicBool`.

use std::cell::Cell;

use echo_agent_shared::ui_config::UiConfig;

use super::follow_up_behavior::FollowUpBehavior;
use super::render_mermaid::RenderMermaid;
use super::scroll_mode::ScrollMode;
use super::text_selection::TextSelection;

// -- Defaults (asserted in tests to match UiConfig::default()) --------------

const COMPACT_DEFAULT: bool = false;
const TIMESTAMPS_DEFAULT: bool = true;
/// Timeline sidebar (per-turn tick rail): single source of truth is
/// [`UiConfig::SHOW_TIMELINE_DEFAULT`]; aliased here for the `Cell::new`
/// const context and the effective-config fallback read.
const TIMELINE_DEFAULT: bool = UiConfig::SHOW_TIMELINE_DEFAULT;
const PAGE_FLIP_ON_SEND_DEFAULT: bool = UiConfig::PAGE_FLIP_ON_SEND_DEFAULT;
/// Combine-queued-prompts rollout flag defaults OFF (opt-in).
const COMBINE_QUEUED_PROMPTS_DEFAULT: bool = false;
const FOLLOW_UP_BEHAVIOR_DEFAULT: FollowUpBehavior = FollowUpBehavior::Queue;
const SIMPLE_MODE_DEFAULT: bool = true;
/// Vim-mode scrollback default — matches the previous on-disk default.
const VIM_MODE_DEFAULT: bool = false;
const SHOW_THINKING_BLOCKS_DEFAULT: bool = true;
const GROUP_TOOL_VERBS_DEFAULT: bool = true;
/// Collapsed-Edit-blocks rollout flag defaults OFF (legacy expanded diffs).
const COLLAPSED_EDIT_BLOCKS_DEFAULT: bool = false;
/// Next-prompt suggestions (tab autocomplete ghost text) default ON.
const PROMPT_SUGGESTIONS_DEFAULT: bool = true;
const KEEP_TEXT_SELECTION_DEFAULT: TextSelection = TextSelection::Flash;
/// Scroll speed default (1-100 scale, matches the legacy `[ui].scroll_speed`).
const SCROLL_SPEED_DEFAULT: u8 = 50;
const SCROLL_SPEED_MIN: u8 = 1;
const SCROLL_SPEED_MAX: u8 = 100;
const SCROLL_MODE_DEFAULT: ScrollMode = ScrollMode::Auto;
const INVERT_SCROLL_DEFAULT: bool = false;
/// `scroll_lines` sentinel for "never configured": keep the per-terminal
/// profile's lines-per-tick instead of forcing a user value.
const SCROLL_LINES_UNSET: u8 = 0;
const SCROLL_LINES_MIN: u8 = 1;
const SCROLL_LINES_MAX: u8 = 10;

// -- Compact mode ------------------------------------------------------------

thread_local! {
    static COMPACT_CURRENT: Cell<bool> = const { Cell::new(COMPACT_DEFAULT) };
    static COMPACT_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `compact_mode`, seeding from disk on first call.
pub fn load() -> bool {
    COMPACT_LOADED.with(|loaded| {
        if !loaded.get() {
            COMPACT_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "compact_mode",
                    COMPACT_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    COMPACT_CURRENT.with(|c| c.get())
}

/// Replace cached `compact_mode` (optimistic update or rollback).
pub fn set(enabled: bool) {
    COMPACT_CURRENT.with(|c| c.set(enabled));
    COMPACT_LOADED.with(|l| l.set(true));
}

// -- Timestamps --------------------------------------------------------------

thread_local! {
    static TIMESTAMPS_CURRENT: Cell<bool> = const { Cell::new(TIMESTAMPS_DEFAULT) };
    static TIMESTAMPS_LOADED: Cell<bool> = const { Cell::new(false) };
}

pub fn load_timestamps() -> bool {
    TIMESTAMPS_LOADED.with(|loaded| {
        if !loaded.get() {
            TIMESTAMPS_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "show_timestamps",
                    TIMESTAMPS_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    TIMESTAMPS_CURRENT.with(|c| c.get())
}

pub fn set_timestamps(enabled: bool) {
    TIMESTAMPS_CURRENT.with(|c| c.set(enabled));
    TIMESTAMPS_LOADED.with(|l| l.set(true));
}

// -- Timeline sidebar ----------------------------------------------------------

thread_local! {
    static TIMELINE_CURRENT: Cell<bool> = const { Cell::new(TIMELINE_DEFAULT) };
    static TIMELINE_LOADED: Cell<bool> = const { Cell::new(false) };
}

pub fn load_show_timeline() -> bool {
    TIMELINE_LOADED.with(|loaded| {
        if !loaded.get() {
            TIMELINE_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "show_timeline",
                    TIMELINE_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    TIMELINE_CURRENT.with(|c| c.get())
}

pub fn set_show_timeline(enabled: bool) {
    TIMELINE_CURRENT.with(|c| c.set(enabled));
    TIMELINE_LOADED.with(|l| l.set(true));
}

// -- Page-flip on send ---------------------------------------------------------

thread_local! {
    static PAGE_FLIP_ON_SEND_CURRENT: Cell<bool> = const { Cell::new(PAGE_FLIP_ON_SEND_DEFAULT) };
    static PAGE_FLIP_ON_SEND_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Cached `page_flip_on_send`, seeding from `[ui]` on first call.
pub fn load_page_flip_on_send() -> bool {
    PAGE_FLIP_ON_SEND_LOADED.with(|loaded| {
        if !loaded.get() {
            PAGE_FLIP_ON_SEND_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "page_flip_on_send",
                    PAGE_FLIP_ON_SEND_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    PAGE_FLIP_ON_SEND_CURRENT.with(|c| c.get())
}

pub fn set_page_flip_on_send(enabled: bool) {
    PAGE_FLIP_ON_SEND_CURRENT.with(|c| c.set(enabled));
    PAGE_FLIP_ON_SEND_LOADED.with(|l| l.set(true));
}

// -- Combine queued prompts ---------------------------------------------------

thread_local! {
    static COMBINE_QUEUED_PROMPTS_CURRENT: Cell<bool> =
        const { Cell::new(COMBINE_QUEUED_PROMPTS_DEFAULT) };
    static COMBINE_QUEUED_PROMPTS_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Cached `combine_queued_prompts`, seeding from `[ui]` on first call.
pub fn load_combine_queued_prompts() -> bool {
    COMBINE_QUEUED_PROMPTS_LOADED.with(|loaded| {
        if !loaded.get() {
            COMBINE_QUEUED_PROMPTS_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "combine_queued_prompts",
                    COMBINE_QUEUED_PROMPTS_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    COMBINE_QUEUED_PROMPTS_CURRENT.with(|c| c.get())
}

pub fn set_combine_queued_prompts(enabled: bool) {
    COMBINE_QUEUED_PROMPTS_CURRENT.with(|c| c.set(enabled));
    COMBINE_QUEUED_PROMPTS_LOADED.with(|l| l.set(true));
}

thread_local! {
    static FOLLOW_UP_BEHAVIOR_CURRENT: Cell<FollowUpBehavior> =
        const { Cell::new(FOLLOW_UP_BEHAVIOR_DEFAULT) };
    static FOLLOW_UP_BEHAVIOR_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `follow_up_behavior`, seeding from disk on first call.
pub fn load_follow_up_behavior() -> FollowUpBehavior {
    FOLLOW_UP_BEHAVIOR_LOADED.with(|loaded| {
        if !loaded.get() {
            let value = load_str_from_effective_config("follow_up_behavior")
                .as_deref()
                .and_then(FollowUpBehavior::from_canonical)
                .unwrap_or(FOLLOW_UP_BEHAVIOR_DEFAULT);
            FOLLOW_UP_BEHAVIOR_CURRENT.with(|c| c.set(value));
            loaded.set(true);
        }
    });
    FOLLOW_UP_BEHAVIOR_CURRENT.with(|c| c.get())
}

/// True when follow-ups should promote as mid-turn interjections (Steer).
pub fn load_follow_up_steer() -> bool {
    load_follow_up_behavior().is_steer()
}

/// Replace cached `follow_up_behavior`.
pub fn set_follow_up_behavior(value: FollowUpBehavior) {
    FOLLOW_UP_BEHAVIOR_CURRENT.with(|c| c.set(value));
    FOLLOW_UP_BEHAVIOR_LOADED.with(|l| l.set(true));
}

// -- Simple mode --------------------------------------------------------------

thread_local! {
    static SIMPLE_MODE_CURRENT: Cell<bool> = const { Cell::new(SIMPLE_MODE_DEFAULT) };
    static SIMPLE_MODE_LOADED: Cell<bool> = const { Cell::new(false) };
}

pub fn load_simple_mode() -> bool {
    SIMPLE_MODE_LOADED.with(|loaded| {
        if !loaded.get() {
            SIMPLE_MODE_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "simple_mode",
                    SIMPLE_MODE_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    SIMPLE_MODE_CURRENT.with(|c| c.get())
}

pub fn set_simple_mode(enabled: bool) {
    SIMPLE_MODE_CURRENT.with(|c| c.set(enabled));
    SIMPLE_MODE_LOADED.with(|l| l.set(true));
}

// -- Vim mode (scrollback) ---------------------------------------------------

thread_local! {
    static VIM_MODE_CURRENT: Cell<bool> = const { Cell::new(VIM_MODE_DEFAULT) };
    static VIM_MODE_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `vim_mode`, seeding from disk on first call.
///
/// `vim_mode` is pager-owned ephemeral state in the new settings
/// registry: the cache acts as the process-wide source of truth so
/// newly-spawned agents read a consistent value, but no `Effect`
/// writes to disk. Restored from `[ui].vim_mode` on first read so
/// historical configs continue to work.
pub fn load_vim_mode() -> bool {
    VIM_MODE_LOADED.with(|loaded| {
        if !loaded.get() {
            VIM_MODE_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "vim_mode",
                    VIM_MODE_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    VIM_MODE_CURRENT.with(|c| c.get())
}

/// Replace cached `vim_mode`.
pub fn set_vim_mode(enabled: bool) {
    VIM_MODE_CURRENT.with(|c| c.set(enabled));
    VIM_MODE_LOADED.with(|l| l.set(true));
}

// -- Show thinking blocks ----------------------------------------------------

thread_local! {
    static SHOW_THINKING_BLOCKS_CURRENT: Cell<bool> =
        const { Cell::new(SHOW_THINKING_BLOCKS_DEFAULT) };
    static SHOW_THINKING_BLOCKS_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `show_thinking_blocks`, seeding from `[ui]` on first call.
/// Default ON when unset. Startup may override via resolve.
pub fn load_show_thinking_blocks() -> bool {
    SHOW_THINKING_BLOCKS_LOADED.with(|loaded| {
        if !loaded.get() {
            SHOW_THINKING_BLOCKS_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "show_thinking_blocks",
                    SHOW_THINKING_BLOCKS_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    SHOW_THINKING_BLOCKS_CURRENT.with(|c| c.get())
}

/// Replace cached `show_thinking_blocks`.
pub fn set_show_thinking_blocks(enabled: bool) {
    SHOW_THINKING_BLOCKS_CURRENT.with(|c| c.set(enabled));
    SHOW_THINKING_BLOCKS_LOADED.with(|l| l.set(true));
}

// -- Group tool verbs ---------------------------------------------------------

thread_local! {
    static GROUP_TOOL_VERBS_CURRENT: Cell<bool> =
        const { Cell::new(GROUP_TOOL_VERBS_DEFAULT) };
    static GROUP_TOOL_VERBS_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `group_tool_verbs`, seeding from `[ui]` on first call.
/// Default ON when unset. Startup may override via resolve.
pub fn load_group_tool_verbs() -> bool {
    GROUP_TOOL_VERBS_LOADED.with(|loaded| {
        if !loaded.get() {
            GROUP_TOOL_VERBS_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "group_tool_verbs",
                    GROUP_TOOL_VERBS_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    GROUP_TOOL_VERBS_CURRENT.with(|c| c.get())
}

/// Replace cached `group_tool_verbs`.
pub fn set_group_tool_verbs(enabled: bool) {
    GROUP_TOOL_VERBS_CURRENT.with(|c| c.set(enabled));
    GROUP_TOOL_VERBS_LOADED.with(|l| l.set(true));
}

// -- Collapsed edit blocks -----------------------------------------------------

thread_local! {
    static COLLAPSED_EDIT_BLOCKS_CURRENT: Cell<bool> =
        const { Cell::new(COLLAPSED_EDIT_BLOCKS_DEFAULT) };
    static COLLAPSED_EDIT_BLOCKS_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `collapsed_edit_blocks`, seeding from `[ui]` on first call.
/// Default OFF when unset. Startup may override via resolve. Consulted only
/// when the pager.toml `[scrollback.blocks.edit]` shape keys are unset (see
/// `EditBlockConfig::effective_expanded` / `effective_line_summary`).
pub fn load_collapsed_edit_blocks() -> bool {
    COLLAPSED_EDIT_BLOCKS_LOADED.with(|loaded| {
        if !loaded.get() {
            COLLAPSED_EDIT_BLOCKS_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "collapsed_edit_blocks",
                    COLLAPSED_EDIT_BLOCKS_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    COLLAPSED_EDIT_BLOCKS_CURRENT.with(|c| c.get())
}

/// Replace cached `collapsed_edit_blocks`.
pub fn set_collapsed_edit_blocks(enabled: bool) {
    COLLAPSED_EDIT_BLOCKS_CURRENT.with(|c| c.set(enabled));
    COLLAPSED_EDIT_BLOCKS_LOADED.with(|l| l.set(true));
}

// -- Prompt suggestions (tab autocomplete) -----------------------------------

thread_local! {
    static PROMPT_SUGGESTIONS_CURRENT: Cell<bool> =
        const { Cell::new(PROMPT_SUGGESTIONS_DEFAULT) };
    static PROMPT_SUGGESTIONS_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `prompt_suggestions`, seeding from `[ui]` on first call.
/// Default ON when unset. The `ECHO_AGENT_PROMPT_SUGGESTIONS` env var overrides
/// this at the feature gate (see the pager's `prompt_suggestion` module).
pub fn load_prompt_suggestions() -> bool {
    PROMPT_SUGGESTIONS_LOADED.with(|loaded| {
        if !loaded.get() {
            PROMPT_SUGGESTIONS_CURRENT.with(|c| {
                c.set(load_bool_from_effective_config(
                    "prompt_suggestions",
                    PROMPT_SUGGESTIONS_DEFAULT,
                ))
            });
            loaded.set(true);
        }
    });
    PROMPT_SUGGESTIONS_CURRENT.with(|c| c.get())
}

/// Replace cached `prompt_suggestions`.
pub fn set_prompt_suggestions(enabled: bool) {
    PROMPT_SUGGESTIONS_CURRENT.with(|c| c.set(enabled));
    PROMPT_SUGGESTIONS_LOADED.with(|l| l.set(true));
}

// -- keep_text_selection (`flash` | `hold`) ----------------------------------

thread_local! {
    static KEEP_TEXT_SELECTION_CURRENT: Cell<TextSelection> =
        const { Cell::new(KEEP_TEXT_SELECTION_DEFAULT) };
    static KEEP_TEXT_SELECTION_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `keep_text_selection`, seeding from disk on first call.
pub fn load_keep_text_selection() -> TextSelection {
    KEEP_TEXT_SELECTION_LOADED.with(|loaded| {
        if !loaded.get() {
            KEEP_TEXT_SELECTION_CURRENT
                .with(|c| c.set(load_keep_text_selection_from_effective_config()));
            loaded.set(true);
        }
    });
    KEEP_TEXT_SELECTION_CURRENT.with(|c| c.get())
}

/// Replace cached `keep_text_selection`.
pub fn set_keep_text_selection(value: TextSelection) {
    KEEP_TEXT_SELECTION_CURRENT.with(|c| c.set(value));
    KEEP_TEXT_SELECTION_LOADED.with(|l| l.set(true));
}

/// Apply the remote soft default for `keep_text_selection` on top of the locally-resolved
/// [`load_keep_text_selection`].
///
/// When the server supplies a recognized value (`flash` / `hold` / `word_select`) and the
/// user has expressed no text-selection preference (no `keep_text_selection`, no legacy
/// `selection_highlight_duration_ms`, no retired `double_click_action = "word_select"`),
/// adopt it as the default. Any explicit local choice always wins, so this only sets the
/// untouched default. An absent or unrecognized value leaves the compile-time default in
/// place. Call once at startup after [`prime`].
pub fn apply_remote_keep_text_selection_default(remote_default: Option<&str>, ui: &UiConfig) {
    let Some(value) = remote_default.and_then(TextSelection::from_canonical) else {
        return;
    };
    let user_expressed_preference = ui.keep_text_selection.is_some()
        || ui.selection_highlight_duration_ms.is_some()
        || ui.double_click_action.as_deref() == Some("word_select");
    if user_expressed_preference {
        return;
    }
    set_keep_text_selection(value);
}

// -- Scroll speed ------------------------------------------------------------

thread_local! {
    static SCROLL_SPEED_CURRENT: Cell<u8> = const { Cell::new(SCROLL_SPEED_DEFAULT) };
    static SCROLL_SPEED_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached scroll speed (1..=100), seeding from disk + env var on
/// first call. `ECHO_AGENT_SCROLL_SPEED` overrides config.toml — matches the
/// legacy `appearance::persist::load_scroll_speed` behaviour.
pub fn load_scroll_speed() -> u8 {
    SCROLL_SPEED_LOADED.with(|loaded| {
        if !loaded.get() {
            let from_env = std::env::var("ECHO_AGENT_SCROLL_SPEED")
                .ok()
                .and_then(|v| v.parse::<u8>().ok());
            let raw = from_env.unwrap_or_else(|| {
                load_u8_from_effective_config("scroll_speed", SCROLL_SPEED_DEFAULT)
            });
            SCROLL_SPEED_CURRENT.with(|c| c.set(raw.clamp(SCROLL_SPEED_MIN, SCROLL_SPEED_MAX)));
            loaded.set(true);
        }
    });
    SCROLL_SPEED_CURRENT.with(|c| c.get())
}

/// Replace cached scroll speed. Input clamped to `[1, 100]` to match
/// the registry's `Int { min: 1, max: 100 }` bounds.
pub fn set_scroll_speed(speed: u8) {
    SCROLL_SPEED_CURRENT.with(|c| c.set(speed.clamp(SCROLL_SPEED_MIN, SCROLL_SPEED_MAX)));
    SCROLL_SPEED_LOADED.with(|l| l.set(true));
}

// -- Scroll mode (auto | wheel | trackpad) -----------------------------------

thread_local! {
    static SCROLL_MODE_CURRENT: Cell<ScrollMode> = const { Cell::new(SCROLL_MODE_DEFAULT) };
    static SCROLL_MODE_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `scroll_mode`, seeding from disk + env var on first call.
/// `ECHO_AGENT_SCROLL_MODE` overrides `[ui].scroll_mode` (the `ECHO_AGENT_SCROLL_SPEED`
/// contract); unrecognized values from either source fall back to `auto`.
pub fn load_scroll_mode() -> ScrollMode {
    SCROLL_MODE_LOADED.with(|loaded| {
        if !loaded.get() {
            let from_env = std::env::var("ECHO_AGENT_SCROLL_MODE")
                .ok()
                .and_then(|v| ScrollMode::from_canonical(v.trim()));
            let value = from_env.unwrap_or_else(|| {
                load_str_from_effective_config("scroll_mode")
                    .as_deref()
                    .and_then(ScrollMode::from_canonical)
                    .unwrap_or(SCROLL_MODE_DEFAULT)
            });
            SCROLL_MODE_CURRENT.with(|c| c.set(value));
            loaded.set(true);
        }
    });
    SCROLL_MODE_CURRENT.with(|c| c.get())
}

/// Replace cached `scroll_mode`.
pub fn set_scroll_mode(value: ScrollMode) {
    SCROLL_MODE_CURRENT.with(|c| c.set(value));
    SCROLL_MODE_LOADED.with(|l| l.set(true));
}

// -- Invert scroll ------------------------------------------------------------

thread_local! {
    static INVERT_SCROLL_CURRENT: Cell<bool> = const { Cell::new(INVERT_SCROLL_DEFAULT) };
    static INVERT_SCROLL_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `invert_scroll`, seeding from disk + env var on first call.
/// `ECHO_AGENT_INVERT_SCROLL` (`1`/`true`/`0`/`false`) overrides `[ui].invert_scroll`.
pub fn load_invert_scroll() -> bool {
    INVERT_SCROLL_LOADED.with(|loaded| {
        if !loaded.get() {
            let from_env = std::env::var("ECHO_AGENT_INVERT_SCROLL")
                .ok()
                .and_then(|v| match v.trim() {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => None,
                });
            let value = from_env.unwrap_or_else(|| {
                load_bool_from_effective_config("invert_scroll", INVERT_SCROLL_DEFAULT)
            });
            INVERT_SCROLL_CURRENT.with(|c| c.set(value));
            loaded.set(true);
        }
    });
    INVERT_SCROLL_CURRENT.with(|c| c.get())
}

/// Replace cached `invert_scroll`.
pub fn set_invert_scroll(enabled: bool) {
    INVERT_SCROLL_CURRENT.with(|c| c.set(enabled));
    INVERT_SCROLL_LOADED.with(|l| l.set(true));
}

// -- Scroll lines ------------------------------------------------------------

thread_local! {
    static SCROLL_LINES_CURRENT: Cell<u8> = const { Cell::new(SCROLL_LINES_UNSET) };
    static SCROLL_LINES_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read cached `scroll_lines` (1..=10), seeding from disk + env var on first
/// call. `None` = never configured, so the per-terminal scroll profile keeps
/// its own lines-per-tick. `ECHO_AGENT_SCROLL_LINES` overrides `[ui].scroll_lines`.
pub fn load_scroll_lines() -> Option<u8> {
    SCROLL_LINES_LOADED.with(|loaded| {
        if !loaded.get() {
            let from_env = std::env::var("ECHO_AGENT_SCROLL_LINES")
                .ok()
                .and_then(|v| v.trim().parse::<u8>().ok());
            let raw = from_env.unwrap_or_else(|| {
                load_u8_from_effective_config("scroll_lines", SCROLL_LINES_UNSET)
            });
            let value = if raw == SCROLL_LINES_UNSET {
                SCROLL_LINES_UNSET
            } else {
                raw.clamp(SCROLL_LINES_MIN, SCROLL_LINES_MAX)
            };
            SCROLL_LINES_CURRENT.with(|c| c.set(value));
            loaded.set(true);
        }
    });
    SCROLL_LINES_CURRENT.with(|c| match c.get() {
        SCROLL_LINES_UNSET => None,
        lines => Some(lines),
    })
}

/// Replace cached `scroll_lines`. Input clamped to `[1, 10]` to match the
/// registry's `Int { min: 1, max: 10 }` bounds.
pub fn set_scroll_lines(lines: u8) {
    SCROLL_LINES_CURRENT.with(|c| c.set(lines.clamp(SCROLL_LINES_MIN, SCROLL_LINES_MAX)));
    SCROLL_LINES_LOADED.with(|l| l.set(true));
}

// -- Render mermaid (auto | on | off) ---------------------------------------

thread_local! {
    static RENDER_MERMAID_CURRENT: Cell<RenderMermaid> = const { Cell::new(RenderMermaid::Auto) };
    static RENDER_MERMAID_LOADED: Cell<bool> = const { Cell::new(false) };
}

/// Read the cached `render_mermaid` preference, seeding from
/// `[ui].render_mermaid` on first call.
///
/// Mirrors `vim_mode`: the cache is the process-wide render-path source of
/// truth, modal commits update it optimistically, and the value is persisted to
/// disk (via `PersistSetting`) so it survives restarts. The thread-local is
/// coherent because scrollback render + dispatch run on the one main thread
/// (see the module docs); a render on any other thread would lazy-seed from
/// disk and would not observe an in-session modal override.
pub fn load_render_mermaid() -> RenderMermaid {
    RENDER_MERMAID_LOADED.with(|loaded| {
        if !loaded.get() {
            let value = render_mermaid_from_config_str(
                load_str_from_effective_config("render_mermaid").as_deref(),
            );
            RENDER_MERMAID_CURRENT.with(|c| c.set(value));
            loaded.set(true);
        }
    });
    RENDER_MERMAID_CURRENT.with(|c| c.get())
}

/// Parse a `[ui].render_mermaid` config value into the preference, defaulting to
/// `Auto` for an absent or unrecognized value (the seed-path fallback).
fn render_mermaid_from_config_str(value: Option<&str>) -> RenderMermaid {
    value
        .and_then(RenderMermaid::from_canonical)
        .unwrap_or_default()
}

/// Replace cached `render_mermaid` (optimistic update from the settings modal).
pub fn set_render_mermaid(value: RenderMermaid) {
    RENDER_MERMAID_CURRENT.with(|c| c.set(value));
    RENDER_MERMAID_LOADED.with(|l| l.set(true));
}

// -- Prime + read path ------------------------------------------------------

/// Seed all caches from the live `UiConfig` at startup so subsequent
/// `load*()` calls never hit disk on the render hot path.
pub fn prime(ui: &UiConfig) {
    set(ui.compact_mode);
    set_timestamps(ui.show_timestamps.unwrap_or(TIMESTAMPS_DEFAULT));
    set_show_timeline(ui.show_timeline_enabled());
    set_page_flip_on_send(ui.page_flip_on_send_enabled());
    set_combine_queued_prompts(
        ui.combine_queued_prompts
            .unwrap_or(COMBINE_QUEUED_PROMPTS_DEFAULT),
    );
    set_follow_up_behavior(
        ui.follow_up_behavior
            .as_deref()
            .and_then(FollowUpBehavior::from_canonical)
            .unwrap_or(FOLLOW_UP_BEHAVIOR_DEFAULT),
    );
    set_simple_mode(ui.simple_mode.unwrap_or(SIMPLE_MODE_DEFAULT));
    set_keep_text_selection(text_selection_from_ui(ui));
    // Layered-config keys (not the `UiConfig` arg) — seed so the first frame
    // skips disk. `load_*` is a no-op when already set (e.g. resolve at startup).
    let _ = load_vim_mode();
    let _ = load_scroll_speed();
    let _ = load_scroll_mode();
    let _ = load_invert_scroll();
    let _ = load_scroll_lines();
    let _ = load_render_mermaid();
    let _ = load_show_thinking_blocks();
    let _ = load_group_tool_verbs();
    let _ = load_collapsed_edit_blocks();
    let _ = load_prompt_suggestions();
    // `default_selected_permission` owns its own cache in `permission_cursor`.
    crate::appearance::permission_cursor::prime();
}

/// Read a `[ui].<key>` boolean from the shell's layered effective config
/// (managed → user → defaults). Falls back to `default` on any error.
fn load_bool_from_effective_config(key: &str, default: bool) -> bool {
    let root = match echo_agent_config::load_effective_config_disk_only() {
        Ok(r) => r,
        Err(_) => return default,
    };
    root.get("ui")
        .and_then(|ui| ui.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// Resolve the unified text-selection mode from a parsed `UiConfig`.
///
/// Precedence:
/// 1. Explicit canonical `[ui].keep_text_selection` (`flash` | `hold` |
///    `word_select`) always wins — Settings and hand-edits must stick even if a
///    retired `double_click_action` key is still on disk.
/// 2. Else retired `double_click_action = "word_select"` migrates to
///    `word_select` (pre-unification / hand-edited configs only; Settings
///    clears it on any write via `set_keep_text_selection`).
/// 3. Else a legacy `selection_highlight_duration_ms` maps `0 → hold`,
///    non-zero `→ flash` (old configs that predate the unified key).
/// 4. Else nothing is configured, so it resolves to
///    [`KEEP_TEXT_SELECTION_DEFAULT`] (`flash`).
///
/// The remote soft default is layered on top of this at pager startup via
/// [`apply_remote_keep_text_selection_default`]; it does not change this local
/// resolution.
fn text_selection_from_ui(ui: &UiConfig) -> TextSelection {
    if let Some(kind) = ui
        .keep_text_selection
        .as_deref()
        .and_then(TextSelection::from_canonical)
    {
        return kind;
    }
    // Legacy only when keep_text_selection is unset/non-canonical.
    if ui.double_click_action.as_deref() == Some("word_select") {
        return TextSelection::WordSelect;
    }
    match ui.selection_highlight_duration_ms {
        Some(0) => TextSelection::Hold,
        Some(_) => TextSelection::Flash,
        None => KEEP_TEXT_SELECTION_DEFAULT,
    }
}

fn load_keep_text_selection_from_effective_config() -> TextSelection {
    let root = match echo_agent_config::load_effective_config_disk_only() {
        Ok(r) => r,
        Err(_) => return KEEP_TEXT_SELECTION_DEFAULT,
    };
    let Some(ui_val) = root.get("ui") else {
        return KEEP_TEXT_SELECTION_DEFAULT;
    };
    match ui_val.clone().try_into::<UiConfig>() {
        Ok(ui) => text_selection_from_ui(&ui),
        Err(_) => KEEP_TEXT_SELECTION_DEFAULT,
    }
}

/// Read a `[ui].<key>` u8 from the shell's layered effective config.
fn load_u8_from_effective_config(key: &str, default: u8) -> u8 {
    let root = match echo_agent_config::load_effective_config_disk_only() {
        Ok(r) => r,
        Err(_) => return default,
    };
    root.get("ui")
        .and_then(|ui| ui.get(key))
        .and_then(|v| v.as_integer())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(default)
}

/// Read a `[ui].<key>` string from the shell's layered effective config.
/// `None` on any error or when the key is absent.
fn load_str_from_effective_config(key: &str) -> Option<String> {
    let root = echo_agent_config::load_effective_config_disk_only().ok()?;
    root.get("ui")
        .and_then(|ui| ui.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Hardcoded `Cell::new` defaults must match `UiConfig::default()`.
    #[test]
    fn cache_initial_defaults_match_ui_config_default() {
        let ui = UiConfig::default();
        assert_eq!(COMPACT_DEFAULT, ui.compact_mode);
        assert_eq!(TIMESTAMPS_DEFAULT, ui.show_timestamps.unwrap_or(true));
        assert_eq!(TIMELINE_DEFAULT, ui.show_timeline_enabled());
        assert_eq!(PAGE_FLIP_ON_SEND_DEFAULT, ui.page_flip_on_send_enabled());
        assert_eq!(
            COMBINE_QUEUED_PROMPTS_DEFAULT,
            ui.combine_queued_prompts
                .unwrap_or(COMBINE_QUEUED_PROMPTS_DEFAULT)
        );
        assert_eq!(
            FOLLOW_UP_BEHAVIOR_DEFAULT.as_canonical(),
            ui.follow_up_behavior()
        );
        assert_eq!(SIMPLE_MODE_DEFAULT, ui.simple_mode.unwrap_or(true));
        assert_eq!(VIM_MODE_DEFAULT, ui.vim_mode.unwrap_or(false));
        assert_eq!(
            SHOW_THINKING_BLOCKS_DEFAULT,
            ui.show_thinking_blocks
                .unwrap_or(SHOW_THINKING_BLOCKS_DEFAULT)
        );
        assert_eq!(
            GROUP_TOOL_VERBS_DEFAULT,
            ui.group_tool_verbs.unwrap_or(GROUP_TOOL_VERBS_DEFAULT)
        );
        assert_eq!(
            COLLAPSED_EDIT_BLOCKS_DEFAULT,
            ui.collapsed_edit_blocks
                .unwrap_or(COLLAPSED_EDIT_BLOCKS_DEFAULT)
        );
        // Deliberate constant pin: the rollout contract is default-OFF.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                !COLLAPSED_EDIT_BLOCKS_DEFAULT,
                "collapsed_edit_blocks must default OFF (rollout flag)"
            );
        }
        assert_eq!(
            PROMPT_SUGGESTIONS_DEFAULT,
            ui.prompt_suggestions.unwrap_or(PROMPT_SUGGESTIONS_DEFAULT)
        );
        assert_eq!(KEEP_TEXT_SELECTION_DEFAULT, text_selection_from_ui(&ui));
        assert_eq!(
            SCROLL_MODE_DEFAULT,
            ui.scroll_mode
                .as_deref()
                .and_then(ScrollMode::from_canonical)
                .unwrap_or_default()
        );
        assert_eq!(INVERT_SCROLL_DEFAULT, ui.invert_scroll.unwrap_or(false));
        // None on disk = the SCROLL_LINES_UNSET sentinel (profile default).
        assert_eq!(
            SCROLL_LINES_UNSET,
            ui.scroll_lines.unwrap_or(SCROLL_LINES_UNSET)
        );
    }

    /// `set` then `load` round-trips. Runs in a fresh thread because
    /// `Cell<bool>` thread-locals are sticky across `#[test]`s.
    #[test]
    fn set_then_load_round_trips_compact() {
        std::thread::spawn(|| {
            set(true);
            assert!(load(), "set(true) → load() must return true");
            set(false);
            assert!(!load(), "set(false) → load() must return false");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_timestamps() {
        std::thread::spawn(|| {
            set_timestamps(false);
            assert!(!load_timestamps());
            set_timestamps(true);
            assert!(load_timestamps());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_page_flip_on_send() {
        std::thread::spawn(|| {
            set_page_flip_on_send(true);
            assert!(load_page_flip_on_send());
            set_page_flip_on_send(false);
            assert!(!load_page_flip_on_send());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_combine_queued_prompts() {
        std::thread::spawn(|| {
            set_combine_queued_prompts(true);
            assert!(load_combine_queued_prompts());
            set_combine_queued_prompts(false);
            assert!(!load_combine_queued_prompts());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_simple_mode() {
        std::thread::spawn(|| {
            set_simple_mode(false);
            assert!(!load_simple_mode());
            set_simple_mode(true);
            assert!(load_simple_mode());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_vim_mode() {
        std::thread::spawn(|| {
            set_vim_mode(true);
            assert!(load_vim_mode());
            set_vim_mode(false);
            assert!(!load_vim_mode());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_show_thinking_blocks() {
        std::thread::spawn(|| {
            set_show_thinking_blocks(false);
            assert!(!load_show_thinking_blocks());
            set_show_thinking_blocks(true);
            assert!(load_show_thinking_blocks());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_group_tool_verbs() {
        std::thread::spawn(|| {
            set_group_tool_verbs(false);
            assert!(!load_group_tool_verbs());
            set_group_tool_verbs(true);
            assert!(load_group_tool_verbs());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_collapsed_edit_blocks() {
        std::thread::spawn(|| {
            set_collapsed_edit_blocks(true);
            assert!(load_collapsed_edit_blocks());
            set_collapsed_edit_blocks(false);
            assert!(!load_collapsed_edit_blocks());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_prompt_suggestions() {
        std::thread::spawn(|| {
            set_prompt_suggestions(false);
            assert!(!load_prompt_suggestions());
            set_prompt_suggestions(true);
            assert!(load_prompt_suggestions());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_scroll_speed() {
        std::thread::spawn(|| {
            set_scroll_speed(25);
            assert_eq!(load_scroll_speed(), 25);
            set_scroll_speed(75);
            assert_eq!(load_scroll_speed(), 75);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_scroll_mode() {
        std::thread::spawn(|| {
            set_scroll_mode(ScrollMode::Wheel);
            assert_eq!(load_scroll_mode(), ScrollMode::Wheel);
            set_scroll_mode(ScrollMode::Trackpad);
            assert_eq!(load_scroll_mode(), ScrollMode::Trackpad);
            set_scroll_mode(ScrollMode::Auto);
            assert_eq!(load_scroll_mode(), ScrollMode::Auto);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_invert_scroll() {
        std::thread::spawn(|| {
            set_invert_scroll(true);
            assert!(load_invert_scroll());
            set_invert_scroll(false);
            assert!(!load_invert_scroll());
        })
        .join()
        .unwrap();
    }

    /// `set_scroll_lines` clamps to `[1, 10]` (the registry bounds); a set
    /// value always reads back as `Some` — only the never-configured seed
    /// path can yield `None` (profile default).
    #[test]
    fn set_scroll_lines_round_trips_and_clamps_to_bounds() {
        std::thread::spawn(|| {
            set_scroll_lines(4);
            assert_eq!(load_scroll_lines(), Some(4));
            set_scroll_lines(0);
            assert_eq!(load_scroll_lines(), Some(SCROLL_LINES_MIN));
            set_scroll_lines(200);
            assert_eq!(load_scroll_lines(), Some(SCROLL_LINES_MAX));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_render_mermaid() {
        std::thread::spawn(|| {
            set_render_mermaid(RenderMermaid::Off);
            assert_eq!(load_render_mermaid(), RenderMermaid::Off);
            set_render_mermaid(RenderMermaid::On);
            assert_eq!(load_render_mermaid(), RenderMermaid::On);
            set_render_mermaid(RenderMermaid::Auto);
            assert_eq!(load_render_mermaid(), RenderMermaid::Auto);
        })
        .join()
        .unwrap();
    }

    /// The first-read seed parse: a valid `[ui].render_mermaid` value maps to
    /// the preference; an absent or unrecognized value falls back to `Auto`.
    /// (Exercises the parse path `load_render_mermaid` runs on its disk seed;
    /// the disk read itself goes through the shell's layered config and is
    /// covered by integration use.)
    #[test]
    fn render_mermaid_config_seed_parse() {
        assert_eq!(
            render_mermaid_from_config_str(Some("off")),
            RenderMermaid::Off
        );
        assert_eq!(
            render_mermaid_from_config_str(Some("on")),
            RenderMermaid::On
        );
        assert_eq!(
            render_mermaid_from_config_str(Some("auto")),
            RenderMermaid::Auto
        );
        // Invalid / absent → default Auto (the #5-class fallback).
        assert_eq!(
            render_mermaid_from_config_str(Some("garbage")),
            RenderMermaid::Auto
        );
        assert_eq!(
            render_mermaid_from_config_str(Some("")),
            RenderMermaid::Auto
        );
        assert_eq!(render_mermaid_from_config_str(None), RenderMermaid::Auto);
    }

    /// `set_scroll_speed` clamps to `[1, 100]` to match the registry bounds.
    #[test]
    fn set_scroll_speed_clamps_to_bounds() {
        std::thread::spawn(|| {
            set_scroll_speed(0);
            assert_eq!(load_scroll_speed(), SCROLL_SPEED_MIN);
            set_scroll_speed(250);
            assert_eq!(load_scroll_speed(), SCROLL_SPEED_MAX);
        })
        .join()
        .unwrap();
    }

    /// Toggling one cache must not affect the others.
    #[test]
    fn caches_are_independent() {
        std::thread::spawn(|| {
            // ── compact independent (the other two stay true) ──
            set(false);
            set_timestamps(true);
            set_simple_mode(true);
            assert!(!load(), "compact toggled");
            assert!(
                load_timestamps(),
                "timestamps must NOT toggle when compact changed"
            );
            assert!(
                load_simple_mode(),
                "simple_mode must NOT toggle when compact changed"
            );

            // ── timestamps independent ──
            set(true);
            set_timestamps(false);
            set_simple_mode(true);
            assert!(load(), "compact must NOT toggle when timestamps changed");
            assert!(!load_timestamps(), "timestamps toggled");
            assert!(
                load_simple_mode(),
                "simple_mode must NOT toggle when timestamps changed"
            );

            // ── simple_mode independent ──
            set(true);
            set_timestamps(true);
            set_simple_mode(false);
            assert!(load(), "compact must NOT toggle when simple_mode changed");
            assert!(
                load_timestamps(),
                "timestamps must NOT toggle when simple_mode changed"
            );
            assert!(!load_simple_mode(), "simple_mode toggled");
        })
        .join()
        .unwrap();
    }

    /// First `load()` seeds from disk, subsequent calls return the
    /// same value (cache stabilises). Fresh thread = un-seeded locals.
    #[test]
    fn first_load_seeds_then_subsequent_loads_are_stable() {
        std::thread::spawn(|| {
            // First reads — exercise the lazy-seed branch.
            let c1 = load();
            let t1 = load_timestamps();
            let s1 = load_simple_mode();

            // Second reads must be stable.
            assert_eq!(load(), c1, "load() must be stable after first seed");
            assert_eq!(
                load_timestamps(),
                t1,
                "load_timestamps() must be stable after first seed"
            );
            assert_eq!(
                load_simple_mode(),
                s1,
                "load_simple_mode() must be stable after first seed"
            );

            let _ = (c1, t1, s1);
        })
        .join()
        .unwrap();
    }

    /// `prime` populates all caches so `load*` calls skip disk.
    #[test]
    fn prime_seeds_all_caches() {
        std::thread::spawn(|| {
            let ui = UiConfig {
                compact_mode: true,
                show_timestamps: Some(false),
                simple_mode: Some(false),
                keep_text_selection: Some("hold".into()),
                ..UiConfig::default()
            };
            prime(&ui);
            assert!(load());
            assert!(!load_timestamps());
            assert!(!load_simple_mode());
            assert_eq!(load_keep_text_selection(), TextSelection::Hold);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn set_then_load_round_trips_keep_text_selection() {
        std::thread::spawn(|| {
            set_keep_text_selection(TextSelection::Hold);
            assert_eq!(load_keep_text_selection(), TextSelection::Hold);
            set_keep_text_selection(TextSelection::Flash);
            assert_eq!(load_keep_text_selection(), TextSelection::Flash);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn prime_honors_legacy_duration_zero_via_ui_config() {
        std::thread::spawn(|| {
            let ui = UiConfig {
                selection_highlight_duration_ms: Some(0),
                ..UiConfig::default()
            };
            prime(&ui);
            assert_eq!(load_keep_text_selection(), TextSelection::Hold);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn prime_resolves_word_select_from_keep_text_selection() {
        std::thread::spawn(|| {
            let ui = UiConfig {
                keep_text_selection: Some("word_select".into()),
                ..UiConfig::default()
            };
            prime(&ui);
            let ts = load_keep_text_selection();
            assert_eq!(ts, TextSelection::WordSelect);
            assert!(ts.holds(), "word_select must hold the highlight");
            assert!(ts.selects_word(), "word_select must enable word selection");
        })
        .join()
        .unwrap();
    }

    /// Backward-compat: the retired hidden `double_click_action = "word_select"`
    /// flag migrates onto the unified `word_select` mode when
    /// `keep_text_selection` is unset.
    #[test]
    fn prime_migrates_legacy_double_click_action_flag() {
        std::thread::spawn(|| {
            let ui = UiConfig {
                double_click_action: Some("word_select".into()),
                ..UiConfig::default()
            };
            prime(&ui);
            assert_eq!(load_keep_text_selection(), TextSelection::WordSelect);
        })
        .join()
        .unwrap();
    }

    /// Explicit `keep_text_selection` must beat a leftover
    /// `double_click_action = "word_select"` so Settings/hand-edits stick and
    /// the legacy key cannot re-override on every prime/load.
    #[test]
    fn explicit_keep_text_selection_wins_over_legacy_double_click() {
        std::thread::spawn(|| {
            for (paired, want) in [
                ("flash", TextSelection::Flash),
                ("hold", TextSelection::Hold),
                ("word_select", TextSelection::WordSelect),
            ] {
                let ui = UiConfig {
                    keep_text_selection: Some(paired.into()),
                    double_click_action: Some("word_select".into()),
                    ..UiConfig::default()
                };
                prime(&ui);
                assert_eq!(
                    load_keep_text_selection(),
                    want,
                    "explicit keep_text_selection={paired} must win over double_click_action"
                );
            }
        })
        .join()
        .unwrap();
    }

    /// The remote `keep_text_selection_default` sets the untouched default, but
    /// never overrides an explicit local choice, and ignores unknown values.
    #[test]
    fn remote_keep_text_selection_default_sets_only_untouched_default() {
        std::thread::spawn(|| {
            // Absent or unrecognized: the compile-time default (flash) stands.
            set_keep_text_selection(TextSelection::Flash);
            apply_remote_keep_text_selection_default(None, &UiConfig::default());
            assert_eq!(load_keep_text_selection(), TextSelection::Flash);
            apply_remote_keep_text_selection_default(Some("nonsense"), &UiConfig::default());
            assert_eq!(load_keep_text_selection(), TextSelection::Flash);

            // Recognized value, no user preference: adopted (word_select and hold).
            set_keep_text_selection(TextSelection::Flash);
            apply_remote_keep_text_selection_default(Some("word_select"), &UiConfig::default());
            assert_eq!(load_keep_text_selection(), TextSelection::WordSelect);
            set_keep_text_selection(TextSelection::Flash);
            apply_remote_keep_text_selection_default(Some("hold"), &UiConfig::default());
            assert_eq!(load_keep_text_selection(), TextSelection::Hold);

            // The user explicitly chose flash: the user wins over a remote value.
            set_keep_text_selection(TextSelection::Flash);
            let ui = UiConfig {
                keep_text_selection: Some("flash".into()),
                ..UiConfig::default()
            };
            apply_remote_keep_text_selection_default(Some("word_select"), &ui);
            assert_eq!(load_keep_text_selection(), TextSelection::Flash);
        })
        .join()
        .unwrap();
    }
}
