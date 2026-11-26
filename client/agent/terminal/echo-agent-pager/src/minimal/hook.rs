//! Inversion-of-control seam for the optional minimal (scrollback-native)
//! render mode.
//!
//! `echo-agent-pager` cannot depend on `echo-agent-pager-minimal`: that crate reads
//! this crate's view model (`AppView`, the `views::*` widgets, `scrollback`),
//! so a direct dependency would form a cargo cycle. Instead the minimal crate
//! registers its entry points here via [`install`], and this crate dispatches
//! into them through the stored function pointers.
//!
//! The composition-root binary (`echo-agent-pager-bin`) wires it up once at
//! startup by calling `echo_agent_pager_minimal::install()`. When nothing has
//! installed the seam, the pager's `ScreenMode::Minimal` branches are inert
//! (`draw` is a no-op, `/transcript` falls back to the empty case) — the
//! default full-screen and inline render paths never touch this module.

use std::sync::OnceLock;

use crate::app::PagerTerminal;
use crate::app::app_view::AppView;

/// Per-frame minimal render entry point
/// (`echo_agent_pager_minimal::draw`).
pub type MinimalDrawFn = fn(&mut AppView, &mut PagerTerminal);

/// The set of hooks the minimal crate installs.
///
/// Draw-only: minimal's `/transcript` used to hook in here too, but the
/// transcript is now a *state-driven* incremental build
/// (`minimal_api::request_minimal_transcript` arms it; the minimal draw loop
/// pumps a time-budgeted slice per frame), so no second entry point is needed.
#[derive(Clone, Copy)]
pub struct MinimalHooks {
    /// Renders one frame of minimal mode. Called from `AppView::draw`.
    pub draw: MinimalDrawFn,
}

static HOOKS: OnceLock<MinimalHooks> = OnceLock::new();

/// Install the minimal-mode hooks. Idempotent — the first call wins; later
/// calls are ignored.
pub fn install(hooks: MinimalHooks) {
    let _ = HOOKS.set(hooks);
}

/// The installed hooks, if the minimal crate has been wired in.
pub fn hooks() -> Option<&'static MinimalHooks> {
    HOOKS.get()
}
