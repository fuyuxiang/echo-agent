//! Hot-reloadable appearance configuration.
//!
//! Two unrelated concerns live under this module name:
//!
//! - **`config` + `watcher`**: dev-only `~/.echo-agent/pager.toml` RenderConfig
//!   (200+ fields for terminal rendering tuning). Hot-reloaded in dev mode,
//!   static defaults in prod.
//! - **`cache`**: thread-local in-memory caches for the user-facing UI bool
//!   settings (`compact_mode`, `show_timestamps`, `simple_mode`). Disk
//!   writes happen in `echo_agent_runtime::util::config::set_<field>()` via
//!   `Effect::PersistSetting`, NOT here — this is a read-cache only.
//! - **`permission_cursor`**: the `default_selected_permission` value type
//!   plus the caches and resolution logic for which row a permission prompt
//!   preselects.

pub mod cache;
mod config;
pub mod follow_up_behavior;
pub mod permission_cursor;
pub mod render_mermaid;
pub mod scroll_mode;
pub mod text_selection;
mod watcher;

pub use config::{
    AnimationConfig, AppearanceConfig, BlockBackground, BlocksConfig, EditBlockConfig,
    ExecuteHeaderStyle, FollowIndicator, LayoutConfig, PromptConfig, PromptViewConfig,
    RawAltScreenMode, RawAppearanceConfig, RawTerminalConfig, ScrollConfig, ScrollbackConfig,
    ScrollbarConfig, ToolBullet, ToolConfig, persist_respect_manual_folds,
};
pub use follow_up_behavior::FollowUpBehavior;
pub use render_mermaid::RenderMermaid;
pub use scroll_mode::ScrollMode;
pub use text_selection::TextSelection;
pub use watcher::ConfigWatcher;

// -- Global tab_width --------------------------------------------------------
//
// Stored as an atomic so MarkdownContent can read the current value
// without needing the AppearanceConfig threaded through its API.
// Updated by the event loop whenever pager.toml is (re)loaded.

use std::sync::atomic::{AtomicU8, Ordering};

static TAB_WIDTH: AtomicU8 = AtomicU8::new(4);

/// Current tab expansion width (number of spaces per `\t`).
pub fn tab_width() -> u8 {
    TAB_WIDTH.load(Ordering::Relaxed)
}

/// Update the global tab width (called when config is loaded/reloaded).
pub fn set_tab_width(w: u8) {
    TAB_WIDTH.store(w, Ordering::Relaxed);
}

/// Tabs as spaces at [`tab_width`]. Beside the width because every caller that
/// paints a tab has to agree on it: ratatui drops a cluster holding a control
/// character, so a tab left in the text is deleted rather than drawn.
pub fn expand_tabs(text: &str) -> std::borrow::Cow<'_, str> {
    let width = tab_width();
    if width == 0 || !text.contains('\t') {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(text.replace('\t', &" ".repeat(usize::from(width))))
}
