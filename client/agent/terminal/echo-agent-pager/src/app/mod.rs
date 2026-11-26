//! Application entry point and terminal management.
//!
//! Submodule overview:
//! - [`actions`] — Action, Effect, TaskResult enums
//! - [`agent`] — AgentSession, AgentId, TurnState (business types)
//! - [`agent_view`] — AgentView (per-agent view-model: input + draw)
//! - [`app_view`] — AppView (root component: input routing + draw)
//! - [`dispatch`] — Action → state mutation + Vec<Effect> (sync, testable)
//! - [`effects`] — Effect → async task spawning
//! - [`acp_handler`] — ACP notification routing
//! - [`event_loop`] — biased tokio::select! loop
pub mod actions;
pub mod agent;
pub mod agent_view;
pub mod app_view;
pub mod bundle;
pub(crate) mod cancel_latency;
pub mod cli;
pub mod consent;
pub use crate::link_opener;
/// Off-thread full-file syntax highlight upgrade for edit diffs.
pub mod edit_highlight_worker;
/// Off-thread Mermaid diagram render worker (out of process) + per-session cache.
pub mod mermaid_worker;
pub use echo_agent_prompt_queue as prompt_queue;
mod acp_handler;
mod connect_timeout;
mod csi_filter;
mod dispatch;
/// Display-refresh probe + motion cadence + terminal telemetry at startup.
mod display_refresh_startup;
mod effects;
pub(crate) mod error_display;
pub mod roster;
pub mod session_startup;
pub(crate) mod session_title_resolve;
pub mod status_blocks;
pub(crate) mod status_line;
mod status_line_policy;
pub mod subagent;
pub mod subscription;
pub(crate) use effects::sanitize_user_error;
mod event_loop;
mod event_loop_stall;
mod exit_timeout;
pub(crate) mod external_editor;
mod foreign_sessions;
mod inline_edit;
#[cfg(all(test, unix))]
mod leader_cluster;
mod modals;
pub(crate) mod mode_switch;
mod mouse;
mod queue_edit;
pub(crate) mod screen_mode_relaunch;
mod session_load_barrier;
pub mod signal_handler;
mod startup_failure;
mod turn_completion;
mod xt_filter;
pub(crate) use crate::terminal::{kitty_flags_pushed, kitty_releases_reported};
pub use cli::{
    AgentArgs, AgentCmd, Command, HeadlessArgs, LeaderArgs, LeaderMgmtArgs, LeaderMgmtCommand,
    LeaderTargetArgs, OutputFormat, PagerArgs, ServeArgs, WrapArgs,
};
pub use cli::{WorkspaceMgmtArgs, WorkspaceMgmtCommand, WorkspaceStartArgs};
use crossterm::cursor::{self, SetCursorStyle};
use crossterm::event;
use crossterm::execute;
use crossterm::terminal::{
    self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use echo_agent_runtime::util::config;
pub use foreign_sessions::ForeignScanCoordinator;
pub(crate) use foreign_sessions::{
    badge_for_picker_source, foreign_tool_display_label, is_foreign_picker_source,
};
use ratatui::backend::CrosstermBackend;
pub use startup_failure::StartupFailure;
use std::io::{self, Write};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;
pub(crate) use turn_completion::CANCELLATION_CATEGORY_KEY;
/// Tracks the extra Kitty keyboard layer pushed while the `/gboom` game is
/// open (see [`push_gboom_keyboard_flags`]). Kept separate from the base layer
/// (`terminal::kitty_keyboard`) so teardown pops both, in LIFO order.
static GBOOM_KEYBOARD_PUSHED: AtomicBool = AtomicBool::new(false);
/// While the `/gboom` game owns input, additionally request
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES` so plain letter keys (WASD) emit
/// release events — required to track several keys held at once. No-op
/// unless the Kitty keyboard protocol is active. Balanced by
/// [`pop_gboom_keyboard_flags`] (and by `restore_terminal` on teardown).
pub(crate) fn push_gboom_keyboard_flags() {
    if !kitty_flags_pushed() || GBOOM_KEYBOARD_PUSHED.swap(true, Ordering::AcqRel) {
        return;
    }
    let flags = event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        let _ = execute!(stderr, event::PushKeyboardEnhancementFlags(flags));
    });
}
/// Pop the extra keyboard layer pushed by [`push_gboom_keyboard_flags`].
pub(crate) fn pop_gboom_keyboard_flags() {
    if GBOOM_KEYBOARD_PUSHED.swap(false, Ordering::AcqRel) {
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            let _ = execute!(stderr, event::PopKeyboardEnhancementFlags);
        });
    }
}
/// Tracks whether mouse capture (the five DEC modes enabled by
/// crossterm `EnableMouseCapture` + bracketed paste) is currently active.
pub(crate) static MOUSE_CAPTURE_ENABLED: AtomicBool = AtomicBool::new(false);
/// Whether minimal was auto-selected solely because the terminal leaks mouse
/// reports as raw text (JediTerm/Windows) and the user expressed no preference.
/// Gates the idle-hint "auto-set" note so it never misleads users who chose
/// minimal themselves.
static MINIMAL_AUTO_SET_FOR_MOUSE_LEAK: AtomicBool = AtomicBool::new(false);
/// See [`MINIMAL_AUTO_SET_FOR_MOUSE_LEAK`].
pub fn minimal_auto_set_for_mouse_leak() -> bool {
    MINIMAL_AUTO_SET_FOR_MOUSE_LEAK.load(Ordering::Acquire)
}
/// Set after a `/minimal` re-exec that actually stayed minimal (idle-status cue).
static MINIMAL_SHOW_SWITCH_BACK_TO_FULLSCREEN: AtomicBool = AtomicBool::new(false);
pub fn minimal_show_switch_back_to_fullscreen() -> bool {
    MINIMAL_SHOW_SWITCH_BACK_TO_FULLSCREEN.load(Ordering::Acquire)
}
#[cfg(any(test, feature = "test-support"))]
pub fn set_minimal_show_switch_back_to_fullscreen_for_test(on: bool) {
    MINIMAL_SHOW_SWITCH_BACK_TO_FULLSCREEN.store(on, Ordering::Release);
}
/// Whether startup actually applied a forced cursor style. Teardown (and the
/// panic hook, which can't thread parameters) resets the style only when
/// true: under inherit, `0 q` would clobber a shell-chosen style.
pub(crate) static CURSOR_STYLE_FORCED: AtomicBool = AtomicBool::new(false);
/// The screen the terminal is ACTUALLY on, for teardown paths that cannot
/// thread parameters (panic hook, signal handler, post-loop restore); updated
/// eagerly at every screen flip so mid-switch failures tear down correctly.
static CURRENT_SCREEN_MODE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(ScreenMode::INITIAL_U8);
pub(crate) fn set_current_screen_mode(mode: ScreenMode) {
    CURRENT_SCREEN_MODE.store(mode.to_u8(), Ordering::Release);
    signal_handler::set_mode(mode);
}
pub(crate) fn current_screen_mode() -> ScreenMode {
    ScreenMode::from_u8(CURRENT_SCREEN_MODE.load(Ordering::Acquire))
}
/// Whether this process runs the minimal (scrollback-native) screen mode.
/// Set once by [`apply_screen_mode_globals`] from the *effective* mode.
///
/// Exists for the few places that need minimal-mode **behavior** (input
/// semantics, state mutations) but sit below `AppView` and cannot see
/// `AppView::screen_mode` (e.g. `AgentView::handle_input`). Do NOT use the
/// styling globals (`modal_window::embedded()`, `scrollbar hidden`, …) for
/// behavior gating: those are deliberately mode-agnostic render toggles, and a
/// future embedded host flipping them must not inherit minimal's key remaps or
/// scrollback writes.
static MINIMAL_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Whether the process runs in minimal (scrollback-native) mode. See
/// [`MINIMAL_MODE_ACTIVE`]; prefer `AppView::screen_mode.is_minimal()` wherever
/// the screen mode is already in reach.
pub(crate) fn minimal_mode_active() -> bool {
    MINIMAL_MODE_ACTIVE.load(Ordering::Acquire)
}
/// Test-only override for [`minimal_mode_active`] (unit tests exercising
/// minimal-gated input paths without a terminal). Save/restore around use —
/// this is process-global state.
#[cfg(test)]
pub(crate) fn set_minimal_mode_active_for_test(on: bool) {
    MINIMAL_MODE_ACTIVE.store(on, Ordering::Release);
}
/// Whether a bare Esc cancels a running turn: minimal mode and non-vim
/// fullscreen get the single-Esc cancel; fullscreen vim mode keeps the
/// mid-turn swallow (Ctrl+C stays the cancel gesture there).
///
/// Pure over its inputs — production callers pass the agent's injected
/// effective screen mode (`AgentView::is_minimal_mode`, seeded by
/// `apply_app_scoped_gates`; never the [`minimal_mode_active`] process
/// global) and tests pass explicit booleans. `vim_mode` is the
/// scrollback-nav setting (`[ui].vim_mode` / `/vim-mode`), not the prompt
/// `simple_mode`.
pub(crate) fn esc_cancels_turn(is_minimal: bool, vim_mode: bool) -> bool {
    is_minimal || !vim_mode
}
/// Whether the opt-in mouse-reporting toggle feature is enabled
/// (`[ui] mouse_reporting_toggle` / `ECHO_AGENT_MOUSE_REPORTING_TOGGLE`). Seeded once
/// at startup; gates both the `Ctrl+R` shortcut registration and the
/// `/toggle-mouse-reporting` slash command's visibility/execution.
pub(crate) static MOUSE_REPORTING_TOGGLE_ENABLED: AtomicBool = AtomicBool::new(false);
/// Read the cached opt-in mouse-reporting toggle flag (see
/// [`MOUSE_REPORTING_TOGGLE_ENABLED`]). Set once at startup from layered config.
pub(crate) fn mouse_reporting_toggle_enabled() -> bool {
    MOUSE_REPORTING_TOGGLE_ENABLED.load(Ordering::Acquire)
}
/// Process-global voice gate for view code without an `AppView`.
/// Written only by [`crate::app::app_view::AppView::apply_voice_mode_enabled`].
pub(crate) static VOICE_MODE_ENABLED: AtomicBool = AtomicBool::new(false);
pub(crate) fn voice_mode_enabled() -> bool {
    VOICE_MODE_ENABLED.load(Ordering::Acquire)
}
/// Test helper for the process-global voice gate.
pub fn set_voice_mode_enabled_for_test(on: bool) {
    VOICE_MODE_ENABLED.store(on, Ordering::Release);
}
/// Process-global gate for the Ctrl+Space / F8 voice chord, for key-routing
/// and view code without an `AppView` (`resolve_action`, the cheatsheet).
/// Default ON. Seeded at startup from `[ui].voice_keybind_enabled` and
/// updated live by the settings setter; unlike [`VOICE_MODE_ENABLED`] it only
/// silences the keybinding — `/voice` and the other voice surfaces stay up.
pub(crate) static VOICE_KEYBIND_ENABLED: AtomicBool = AtomicBool::new(true);
pub(crate) fn voice_keybind_enabled() -> bool {
    VOICE_KEYBIND_ENABLED.load(Ordering::Acquire)
}
/// Test helper for the process-global voice-keybind gate.
pub fn set_voice_keybind_enabled_for_test(on: bool) {
    VOICE_KEYBIND_ENABLED.store(on, Ordering::Release);
}
fn voice_mode_in(layer: &toml::Value) -> Option<bool> {
    layer
        .get("features")?
        .get(echo_agent_runtime::agent::config::Feature::VoiceMode.key())?
        .as_bool()
}
/// `[features] voice_mode` from merged `requirements.toml`.
pub(crate) fn voice_mode_requirement_pin() -> Option<bool> {
    voice_mode_in(&echo_agent_config::load_merged_requirements()?)
}
/// `[features] voice_mode` from effective config (user + managed).
pub(crate) fn voice_mode_config_value() -> Option<bool> {
    voice_mode_in(&echo_agent_runtime::config::load_effective_config().ok()?)
}
/// The registry owns the precedence and the default. One rule has no row there:
/// with `is_api_key`, a remote-only off is forced back on. A requirement, env,
/// or config `false` still wins.
pub(crate) fn resolve_voice_mode_enabled(
    requirement: Option<bool>,
    config: Option<bool>,
    remote: Option<bool>,
    is_api_key: bool,
) -> bool {
    use echo_agent_runtime::agent::config::{ConfigSource, Feature, FeatureSources};
    let resolved = Feature::VoiceMode.resolve(FeatureSources {
        pin: requirement,
        config,
        remote,
        ..FeatureSources::from_process_env(Feature::VoiceMode)
    });
    if resolved.value {
        return true;
    }
    is_api_key && resolved.source == ConfigSource::Remote
}
/// Resolve from live policy + env + remote + API-key state.
pub(crate) fn resolve_voice_mode_live(remote: Option<bool>, is_api_key: bool) -> bool {
    resolve_voice_mode_enabled(
        voice_mode_requirement_pin(),
        voice_mode_config_value(),
        remote,
        is_api_key,
    )
}
#[cfg(test)]
mod voice_gate_tests {
    use super::resolve_voice_mode_enabled;
    #[test]
    fn api_key_force_on_over_remote_kill_only() {
        assert!(resolve_voice_mode_enabled(None, None, Some(false), true));
        assert!(!resolve_voice_mode_enabled(None, None, Some(false), false));
    }
    #[test]
    fn policy_false_outranks_api_key_force_on() {
        assert!(!resolve_voice_mode_enabled(
            Some(false),
            Some(true),
            Some(true),
            true
        ));
        assert!(!resolve_voice_mode_enabled(
            None,
            Some(false),
            Some(false),
            true
        ));
    }
}
/// Sticky banner shown while mouse reporting is off, telling the user how to
/// turn it back on. The advertised invocation depends on focus: `Ctrl+R` only
/// works from scrollback, so the prompt-focused variant points at the
/// `/toggle-mouse-reporting` command (which toggles from any pane). The banner
/// is stored in the scrollback form; `AgentView::active_toast_message` swaps to
/// the prompt form at render time when the prompt is focused.
pub(crate) const MOUSE_OFF_HINT_SCROLLBACK: &str =
    "Ctrl+r to enable mouse reporting and restore TUI features";
pub(crate) const MOUSE_OFF_HINT_PROMPT: &str =
    "/toggle-mouse-reporting to enable mouse reporting and restore TUI features";
/// Terminal type for the pager.
///
/// Uses [`echo_agent_ratatui_inline::Terminal`] instead of stock `ratatui::Terminal`
/// because our `flush()` returns `bool` indicating whether any cells actually
/// changed. This lets [`crate::render::draw::draw_frame`] skip cursor escape
/// sequences on frames with empty diffs (e.g., off-screen animation ticks),
/// preserving the cursor blink timer. See [`crate::render::draw`] for details.
///
/// The backend writes to a [`TermWriter`](crate::render::draw::TermWriter)
/// that buffers frame data in memory and sends it to a dedicated writer
/// thread via a channel. The writer thread performs the actual blocking
/// `write()` to stderr / the pty fd, keeping the tokio event loop free
/// from pty back-pressure (e.g. when Ghostty is busy with another pane).
pub use crate::render::draw::PagerTerminal;
/// Whether the pager uses the alternate screen (fullscreen) or stays inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScreenMode {
    Fullscreen,
    Inline,
    /// Scrollback-native (experimental, `--minimal`): finalized blocks are
    /// printed into the terminal's native scrollback via `insert_before`, with
    /// a small pinned live region for the prompt, status, and running turn.
    ///
    /// All minimal-mode rendering lives in the sibling `echo-agent-pager-minimal`
    /// crate. This crate only holds the seam: `crate::minimal_hook` (dispatch
    /// into minimal's `draw`/transcript), `crate::minimal_api` (the read surface
    /// minimal consumes), and `AppView::minimal_state`. If you don't work on
    /// minimal, treat this variant as opaque — the fullscreen/inline paths are
    /// unaffected.
    Minimal,
}
impl ScreenMode {
    const INITIAL_U8: u8 = 1;
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            Self::Fullscreen => 0,
            Self::Inline => 1,
            Self::Minimal => 2,
        }
    }
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Fullscreen,
            2 => Self::Minimal,
            _ => Self::Inline,
        }
    }
    pub(crate) fn is_fullscreen(self) -> bool {
        matches!(self, Self::Fullscreen)
    }
    /// Whether this is the experimental scrollback-native minimal mode.
    pub(crate) fn is_minimal(self) -> bool {
        matches!(self, Self::Minimal)
    }
    /// Stable wire label for the `_meta.screenMode` prompt-telemetry field
    /// (headless sends `"headless"`). Values are pinned by the telemetry
    /// allowlist (`echo-agent-telemetry`'s `KNOWN_SCREEN_MODES`); renaming one
    /// silently collapses it to `"other"` on the external stream.
    pub(crate) fn meta_label(self) -> &'static str {
        match self {
            Self::Fullscreen => "fullscreen",
            Self::Inline => "inline",
            Self::Minimal => "minimal",
        }
    }
}
/// Install the process-wide render globals that depend on the screen mode.
///
/// Consolidates every "minimal behaves differently here" toggle into one place
/// so the rest of startup (and any future contributor) doesn't have to sprinkle
/// `is_minimal()` checks through `run`. All of these globals are no-ops outside
/// minimal (they default to the full-TUI behavior), so calling this for every
/// mode is safe and keeps the effective-mode source of truth singular.
fn apply_screen_mode_globals(screen_mode: ScreenMode) {
    let minimal = screen_mode.is_minimal();
    set_current_screen_mode(screen_mode);
    MINIMAL_MODE_ACTIVE.store(minimal, Ordering::Release);
    crate::terminal::image::set_inline_overlay_force_off(minimal);
    crate::views::modal_window::set_embedded(minimal);
    crate::render::scrollbar::set_scrollbars_hidden(minimal);
    crate::theme::cache::set_terminal_native_lock(minimal);
}
/// Startup theme state for the *requested* screen mode — step 1 of the
/// two-phase startup theme handshake (step 2: [`finish_theme_after_probe`]).
/// Must run before `init_terminal`, whose `apply_cursor_color()` reads the
/// state installed here.
fn engage_startup_theme(screen_mode: ScreenMode) {
    if screen_mode.is_minimal() {
        crate::theme::cache::set_terminal_native_lock(true);
    } else {
        let initial_theme = crate::theme::cache::resolve_initial_theme();
        crate::theme::cache::set(initial_theme);
        mode_switch::mark_theme_resolved();
    }
}
/// Step 2 of the startup theme handshake: if a `--minimal` start was
/// downgraded to Inline by `init_terminal`'s probe, resolve the regular
/// theme that [`engage_startup_theme`] skipped. No-op otherwise.
fn finish_theme_after_probe(requested_minimal: bool, effective_mode: ScreenMode) {
    if requested_minimal && !effective_mode.is_minimal() {
        let late_theme = crate::theme::cache::resolve_initial_theme_no_osc11();
        crate::theme::cache::set(late_theme);
        crate::theme::apply_cursor_color();
        mode_switch::mark_theme_resolved();
        tracing::info!(?late_theme, "minimal downgrade: resolved regular theme");
    }
}
/// Info about the active session at exit time, used for the resume hint.
///
/// Wrapped in a struct so additional fields (e.g., cwd, model) can be added
/// without changing the return type.
pub(crate) struct ExitInfo {
    pub session_id: String,
    pub minimal: bool,
    /// Glanceable session tail; `Some` exactly when it should print. The
    /// presence policy lives at the sole construction site, `finish_run`.
    pub summary: Option<ExitSummary>,
}
/// Session tail printed above the resume command on fullscreen quits.
///
/// Invariant: every field is a pre-sanitized single line (built from the
/// `views::session_title` helpers), so the printer only width-truncates.
pub(crate) struct ExitSummary {
    /// Display title (rename > generated > first prompt).
    pub title: String,
    pub last_prompt: Option<String>,
    /// `None` when the newest prompt is still unanswered.
    pub last_response: Option<String>,
}
/// Resolve leader mode, reporting both why it is off and what turned it off.
///
/// Precedence (highest first): `--no-leader` → `--leader` → eligibility → local
/// config `use_leader` → remote `leader_mode` (release-dist) → default off.
/// `requested_confinement` then vetoes leader use when `Some` (in-process tools
/// stay under the OS sandbox) without reclaiming a shared leader on its own.
///
/// `policy_disable_reason` is `Some("config"|"remote")` only when leader mode is
/// *definitively* off by policy (local `use_leader = false`, or remote
/// `leader_mode` fetched as `false`). Unknown remote state (`None` / prefetch
/// timeout), the default, `--no-leader`, and ineligibility are `None` — never
/// reclaim a leader on an unknown signal.
pub fn resolve_leader_mode<'p>(
    leader_flag: bool,
    no_leader_flag: bool,
    raw_config: &toml::Value,
    _remote_settings: Option<&echo_agent_runtime::util::config::RemoteSettings>,
    eligible: bool,
    requested_confinement: Option<&'p str>,
) -> LeaderMode<'p> {
    let (use_leader, policy_disable_reason) = 'policy: {
        if no_leader_flag {
            break 'policy (false, None);
        }
        if leader_flag {
            break 'policy (true, None);
        }
        if !eligible {
            break 'policy (false, None);
        }
        if let Some(v) = config::use_leader_from_toml_opt(raw_config) {
            break 'policy (v, (!v).then_some("config"));
        }
        #[cfg(feature = "release-dist")]
        if let Some(remote_val) = _remote_settings.and_then(|s| s.leader_mode) {
            break 'policy (remote_val, (!remote_val).then_some("remote"));
        }
        (false, None)
    };
    if let Some(profile) = requested_confinement {
        return LeaderMode {
            use_leader: false,
            policy_disable_reason,
            disabled_by_confinement: use_leader.then_some(profile),
        };
    }
    LeaderMode {
        use_leader,
        policy_disable_reason,
        disabled_by_confinement: None,
    }
}
/// Leader mode as resolved, plus the sandbox profile that overrode it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaderMode<'p> {
    pub use_leader: bool,
    /// `Some` only when leader mode is *definitively* off by policy, which is
    /// what licenses reclaiming a leftover leader.
    pub policy_disable_reason: Option<&'static str>,
    /// The profile that turned leader mode off, set only when leader mode was
    /// otherwise on — the case worth telling the user about.
    pub disabled_by_confinement: Option<&'p str>,
}
/// The leader-mode decision alone, for callers with nothing to report.
///
/// See [`resolve_leader_mode`] for the precedence chain and the
/// `policy_disable_reason` contract.
pub fn resolve_use_leader(
    leader_flag: bool,
    no_leader_flag: bool,
    raw_config: &toml::Value,
    remote_settings: Option<&echo_agent_runtime::util::config::RemoteSettings>,
    eligible: bool,
    requested_confinement: Option<&str>,
) -> (bool, Option<&'static str>) {
    let resolved = resolve_leader_mode(
        leader_flag,
        no_leader_flag,
        raw_config,
        remote_settings,
        eligible,
        requested_confinement,
    );
    (resolved.use_leader, resolved.policy_disable_reason)
}
/// How long the sandbox note stays uncovered before a fullscreen TUI opens over
/// it. Paid only when the note was printed and the screen is about to hide it.
const SANDBOX_NOTICE_LINGER: std::time::Duration = std::time::Duration::from_millis(1_200);
/// Tell the user at startup that the sandbox turned leader mode off.
///
/// Writes to the dup'd terminal stderr, which survives the TUI's fd-2 redirect
/// (`redirect_native_stderr`). A fullscreen TUI still paints over it, leaving
/// the line to be read on exit; `leader_disabled_by_sandbox` on the
/// leader-mode decision log is the durable record.
pub fn warn_leader_disabled_by_sandbox(profile: &str) {
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        print_leader_disabled_by_sandbox(profile, stderr)
    });
}
/// Says only that the profile was *requested*: enforcement can still fail
/// (`apply_sandbox` warns and continues) while the leader is refused either way.
///
/// Write errors are dropped — `eprintln!` would panic on a closed stderr.
fn print_leader_disabled_by_sandbox(profile: &str, w: &mut impl Write) {
    let _ = writeln!(
        w,
        "note: sandbox profile '{profile}' was requested, so leader mode is off for this \
         session and tool calls stay in this process instead of the shared leader. \
         Disable the profile at the source that selected it (CLI, env, config, or a \
         managed requirement) to use the leader."
    );
}
/// Join early prefetch to get remote settings (with timeout).
///
/// Remote settings come from the product settings API and contain `leader_mode`,
/// announcements, etc.  Waits up to 2 s for the background thread.
pub fn join_early_prefetch(
    handle: Option<echo_agent_runtime::agent::models::EarlyPrefetchHandle>,
) -> Option<echo_agent_runtime::util::config::RemoteSettings> {
    let handle = handle?;
    if handle.is_finished() {
        return match handle.join() {
            Ok(r) => r.settings,
            Err(_) => None,
        };
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(r)) => r.settings,
        _ => None,
    }
}
/// First non-blank of CLI > env > config (precedence + blank-skip). `None` →
/// nothing set; `acp::initialize` canonicalizes and applies the default.
fn resolve_hunk_tracker_mode(
    cli: Option<&str>,
    env: Option<&str>,
    config: Option<&str>,
) -> Option<String> {
    [cli, env, config]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}
/// A failed connect attempt, classified for telemetry at the point of failure
/// rather than by parsing the error message.
struct ConnectFailure {
    outcome: crate::acp::StartupOutcome,
    error: anyhow::Error,
    timeout_secs: Option<u64>,
    longest_step: Option<crate::acp::StartupPhase>,
}
/// Bound connect so a hung leader/spawn cannot blank-screen forever.
async fn bounded_connect(
    cancel: &CancellationToken,
    timeout: std::time::Duration,
    target: crate::acp::AgentKind,
    attempt: startup_failure::ConnectAttempt,
    timer: &crate::acp::StartupTimer,
    connect: impl std::future::Future<Output = anyhow::Result<crate::acp::AcpConnection>>,
) -> Result<crate::acp::AcpConnection, ConnectFailure> {
    use crate::acp::StartupOutcome;
    let context = || startup_failure::Context {
        target,
        attempt,
        version: echo_agent_version::display_version_with_commit(
            echo_agent_version::full_version(),
            echo_agent_update::channel_label(),
        ),
        log_path: echo_agent_telemetry::unified_log::path(),
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ConnectFailure {
            outcome: StartupOutcome::Cancelled,
            error: anyhow::Error::new(startup_failure::StartupFailure::cancelled(context())),
            timeout_secs: None,
            longest_step: None,
        }),
        connected = connect => connected.map_err(|error| ConnectFailure {
            outcome: StartupOutcome::Error,
            error,
            timeout_secs: None,
            longest_step: None,
        }),
        () = tokio::time::sleep(timeout) => {
            let timings = timer.phase_snapshot();
            let longest_step = timings.longest_step();
            // `connect_target`: tracing reserves bare `target=`.
            tracing::error!(
                connect_target = target.label(),
                stuck_in = timings.stuck_in(),
                phases = %timings.summary(),
                timeout_secs = timeout.as_secs(),
                "connect timed out"
            );
            Err(ConnectFailure {
                outcome: StartupOutcome::Timeout,
                error: anyhow::Error::new(startup_failure::StartupFailure::timed_out(
                    context(),
                    // Measured, not the budget: a synchronous step can overrun it.
                    timer.elapsed(),
                    timings,
                )),
                timeout_secs: Some(timeout.as_secs()),
                longest_step,
            })
        }
    }
}
/// Main entry point: connect to agent, init terminal, run event loop, restore.
///
/// If a session ID is provided via `--resume` / `--load` / `--continue`, the
/// pager skips the welcome screen and immediately loads that session (replaying
/// its history). Sessions not found locally are restored from remote storage.
///
/// Returns `Ok(true)` when the user accepted a pending update. The caller
/// should print a message telling the user to relaunch `echoagent`.
pub async fn run(
    args: PagerArgs,
    bg_update_rx: Option<
        tokio::sync::oneshot::Receiver<Option<echo_agent_update::auto_update::UpdateAvailable>>,
    >,
) -> anyhow::Result<bool> {
    echo_agent_tty_utils::redirect_native_stderr();
    let screen_mode_override = screen_mode_relaunch::take_screen_mode_env_override();
    let cancel = CancellationToken::new();
    let startup_start = std::time::Instant::now();
    let raw_config = echo_agent_runtime::config::load_effective_config()
        .map_err(|e| anyhow::anyhow!("Failed to load config: {e}"))?;
    let echo_agent_com_config = match echo_agent_runtime::agent::config::Config::new_from_toml_cfg(
        &raw_config,
    ) {
        Ok(c) => c.echo_agent_com_config,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse config for auth refresh, using defaults");
            echo_agent_runtime::auth::EchoAgentComConfig::default()
        }
    };
    let refreshed_auth = tokio::time::timeout(
        echo_agent_runtime::http::STARTUP_AUTH_REFRESH_TIMEOUT,
        echo_agent_runtime::auth::try_ensure_fresh_auth(&echo_agent_com_config),
    )
    .await
    .unwrap_or(None);
    let early_prefetch = match refreshed_auth {
        Some(auth) => echo_agent_runtime::agent::models::start_early_prefetch_with_auth(Some(auth)),
        None => echo_agent_runtime::agent::models::start_early_prefetch(Some(
            echo_agent_com_config.clone(),
        )),
    };
    echo_agent_runtime::agent::mvp_agent::warm_async_http_client();
    tokio::task::spawn_blocking(|| {});
    if let Ok(cwd) = std::env::current_dir() {
        crate::git_info::populate_from_cwd_async(cwd);
    }
    let prefetch_wait_started = std::time::Instant::now();
    let had_prefetch = early_prefetch.is_some();
    let remote_settings = join_early_prefetch(early_prefetch);
    if had_prefetch {
        echo_agent_telemetry::startup::record_prefetch_wait(prefetch_wait_started.elapsed());
    }
    echo_agent_runtime::util::config::cache_remote_auto_mode(
        remote_settings.as_ref().and_then(|s| s.auto_mode.clone()),
    );
    echo_agent_runtime::util::config::set_remote_campaigns_from_settings(remote_settings.as_ref());
    let raw_config = echo_agent_runtime::config::load_effective_config()
        .map_err(|e| anyhow::anyhow!("Failed to load config: {e}"))?;
    let prefetch_elapsed = startup_start.elapsed();
    let requested_confinement = echo_agent_sandbox::requested_confinement_profile();
    let LeaderMode {
        use_leader,
        policy_disable_reason,
        disabled_by_confinement,
    } = resolve_leader_mode(
        args.leader,
        args.no_leader,
        &raw_config,
        remote_settings.as_ref(),
        true,
        requested_confinement,
    );
    tracing::info!(
        use_leader,
        ?policy_disable_reason,
        sandbox_profile = ?requested_confinement,
        // The other fields cannot distinguish this from leader mode being off
        // already while a sandbox is on.
        leader_disabled_by_sandbox = disabled_by_confinement.is_some(),
        prefetch_ms = prefetch_elapsed.as_millis() as u64,
        "pager TUI leader mode resolved"
    );
    if let Some(profile) = disabled_by_confinement {
        warn_leader_disabled_by_sandbox(profile);
    }
    if session_startup::chat_mode_conflicts_with_leader(args.chat(), use_leader) {
        anyhow::bail!("{}", session_startup::CHAT_MODE_LEADER_CONFLICT);
    }
    if args.trust {
        match std::env::current_dir() {
            Ok(cwd) => echo_agent_runtime::agent::folder_trust::grant_folder_trust(&cwd),
            Err(e) => {
                tracing::warn!(error = %e, "--trust: failed to resolve cwd; folder not trusted")
            }
        }
    }
    if let Some(reason) = policy_disable_reason {
        tokio::spawn(echo_agent_runtime::leader::kill_stale_reachable_leaders(
            reason,
        ));
    }
    if let Some(err) =
        session_startup::chat_mode_flag_conflict(args.chat(), args.fork_session, args.restore_code)
    {
        anyhow::bail!("{err}");
    }
    #[cfg(feature = "local-workspace")]
    {
        let lw = session_startup::resolve_local_workspace_config(
            args.chat(),
            args.local_workspace(),
            args.local_workspace_attach(),
            args.local_workspace_cwd(),
        )?;
        if let Some(ref cfg) = lw {
            session_startup::emit_local_workspace_startup_ux(cfg)?;
        }
        session_startup::set_active_local_workspace(lw)?;
    }
    let intent = args
        .session_startup_intent()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut materialize_ctx = session_startup::MaterializeCtx::from_pager_args(&args);
    materialize_ctx.restore_progress_on_stdout =
        std::io::IsTerminal::is_terminal(&std::io::stdout());
    let materialized = session_startup::materialize_startup(materialize_ctx, intent).await?;
    if args.chat()
        && let session_startup::MaterializedStartup::Resume { session_id, .. } = &materialized
    {
        let cwd = std::env::current_dir().unwrap_or_default();
        if session_startup::chat_mode_refuses_local_build_load(true, false, session_id, &cwd) {
            anyhow::bail!(
                "{} (session id: {session_id})",
                session_startup::CHAT_MODE_LOCAL_BUILD_REFUSAL
            );
        }
    }
    let mut session_title = match &materialized {
        session_startup::MaterializedStartup::Resume { title, .. }
        | session_startup::MaterializedStartup::Fork {
            parent_title: title,
            ..
        } => title.clone(),
        _ => None,
    };
    let title_lookup_id = match &materialized {
        session_startup::MaterializedStartup::Resume { session_id, .. } => {
            Some(session_id.as_str())
        }
        session_startup::MaterializedStartup::Fork {
            parent_session_id, ..
        } => Some(parent_session_id.as_str()),
        _ => None,
    };
    if session_title.is_none()
        && !args.chat()
        && let Some(id) = title_lookup_id
    {
        let summaries = echo_agent_runtime::session::persistence::list_summaries(None).await?;
        if let Some(s) = summaries.iter().find(|s| s.info.id.0.as_ref() == id)
            && let Some(title) = s.display_title_opt()
        {
            session_title = Some(title);
        }
    }
    let session_cwd = match &materialized {
        session_startup::MaterializedStartup::Resume { original_cwd, .. }
        | session_startup::MaterializedStartup::Fork {
            parent_cwd: original_cwd,
            ..
        } => original_cwd.clone(),
        _ => None,
    };
    let env_hunk_tracker_mode = std::env::var("ECHO_AGENT_HUNK_TRACKER").ok();
    let config_hunk_tracker_mode = raw_config
        .get("ui")
        .and_then(|ui| ui.get("hunk_tracker_mode"))
        .and_then(|v| v.as_str());
    let hunk_tracker_mode = resolve_hunk_tracker_mode(
        args.hunk_tracker_mode.as_deref(),
        env_hunk_tracker_mode.as_deref(),
        config_hunk_tracker_mode,
    );
    let remote_permission_mode = remote_settings
        .as_ref()
        .and_then(|s| s.permission_mode.as_deref());
    let launch_yolo = echo_agent_runtime::util::config::effective_yolo_for_launch(
        args.yolo,
        args.permission_mode_flag.as_deref(),
        remote_permission_mode,
    );
    let launch_auto = echo_agent_runtime::util::config::effective_auto_for_launch_interactive(
        args.yolo,
        args.permission_mode_flag.as_deref(),
        remote_permission_mode,
    );
    let mut connect_flags = crate::acp::ConnectFlags {
        subagents: !args.no_subagents,
        memory_enabled_override: args.memory_enabled_override(),
        memory_override_flag: args.memory_override_flag(),
        disable_web_search: args.disable_web_search,
        todo_gate: args.todo_gate,
        laziness_debug_log: None,
        storage_mode: args.storage_mode.clone(),
        client_identifier: args.client_identifier.clone(),
        hunk_tracker_mode,
        terminal: args.terminal,
        fs_read: args.fs_read,
        fs_write: args.fs_write,
        installer: args.installer.clone(),
        remote_settings: remote_settings.clone(),
        system_prompt_override: args.system_prompt_override.clone(),
        rules: args.rules.clone(),
        reasoning_effort_override: args
            .reasoning_effort
            .as_deref()
            .and_then(echo_agent_runtime::sampling::types::parse_canonical_effort_token),
        permission_rules: crate::headless::parse_permission_rules_lenient(
            &args.allow_rules,
            &args.deny_rules,
        ),
        default_yolo_mode: launch_yolo.yolo,
        default_auto_mode: launch_auto && !launch_yolo.yolo,
        status_line: false,
    };
    let mut config_watcher = crate::appearance::ConfigWatcher::start().await?;
    let alt_screen_config_mode = config_watcher.current().alt_screen;
    let term_ctx = crate::terminal::terminal_context();
    let is_control_mode = crate::terminal::detect_tmux_control_mode(term_ctx);
    let alt_screen_wants_fullscreen = crate::terminal::determine_alt_screen_policy(
        args.no_alt_screen,
        alt_screen_config_mode,
        term_ctx,
        is_control_mode,
    );
    let config_screen_mode = raw_config
        .get("ui")
        .and_then(|ui| ui.get("screen_mode"))
        .and_then(|v| v.as_str());
    let auto_minimal_mouse_leak = term_ctx.mouse_reporting_leaks_as_raw_text();
    let explicit_minimal = screen_mode_relaunch::effective_minimal_preference(
        args.minimal,
        args.fullscreen,
        config_screen_mode,
        config_watcher.current().minimal,
    );
    let screen_mode = screen_mode_relaunch::resolve_screen_mode(
        screen_mode_override,
        explicit_minimal.unwrap_or(auto_minimal_mouse_leak),
        alt_screen_wants_fullscreen,
    );
    MINIMAL_AUTO_SET_FOR_MOUSE_LEAK.store(
        screen_mode.is_minimal() && explicit_minimal.is_none() && screen_mode_override.is_none(),
        Ordering::Release,
    );
    let minimal = screen_mode.is_minimal();
    connect_flags.status_line = event_loop::load_initial_ui_config()
        .status_line
        .reserves_a_row();
    let relaunched_into_minimal = screen_mode_override == Some(ScreenMode::Minimal);
    let relaunched_into_fullscreen = screen_mode_override == Some(ScreenMode::Fullscreen);
    tracing::info!(
        use_alt_screen = screen_mode.is_fullscreen(),
        minimal = screen_mode.is_minimal(),
        mouse_capture = !screen_mode.is_minimal(),
        minimal_live_rows = config_watcher.current().minimal_live_rows,
        is_control_mode,
        no_alt_screen_cli = args.no_alt_screen,
        minimal_cli = args.minimal,
        fullscreen_cli = args.fullscreen,
        config_screen_mode = ?config_screen_mode,
        auto_minimal_mouse_leak,
        config_mode = ?alt_screen_config_mode,
        multiplexer = ?term_ctx.multiplexer,
        "resolved fullscreen policy"
    );
    if disabled_by_confinement.is_some() && screen_mode.is_fullscreen() {
        tokio::time::sleep(SANDBOX_NOTICE_LINGER).await;
    }
    engage_startup_theme(screen_mode);
    let minimal_live_rows = config_watcher.current().minimal_live_rows;
    let (frame_tx, writer_sync, writer_event_rx, writer_thread) =
        crate::render::draw::spawn_writer_thread();
    let cursor_blink = event_loop::load_initial_ui_config().cursor_blink;
    let TerminalInit {
        mut terminal,
        screen_mode,
        startup_typeahead,
    } = init_terminal(
        screen_mode,
        minimal_live_rows,
        relaunched_into_minimal,
        frame_tx,
        writer_sync,
        cursor_blink,
    )?;
    MINIMAL_SHOW_SWITCH_BACK_TO_FULLSCREEN.store(
        relaunched_into_minimal && screen_mode.is_minimal(),
        Ordering::Release,
    );
    apply_screen_mode_globals(screen_mode);
    finish_theme_after_probe(minimal, screen_mode);
    if let Some(ref t) = session_title {
        set_terminal_title(t);
    }
    let connect_ui_timeout_env = std::env::var(connect_timeout::CONNECT_UI_TIMEOUT_ENV).ok();
    let connect_ui_timeout = connect_timeout::resolve(connect_ui_timeout_env.as_deref());
    if let Some(raw) = connect_ui_timeout_env {
        crate::unified_log::write_direct_info(
            "startup connect budget from env",
            Some(serde_json::json!({
                "raw": raw,
                "timeout_secs": connect_ui_timeout.as_secs(),
            })),
        );
    }
    let fallback_flags = use_leader.then(|| connect_flags.clone());
    let primary_target = if use_leader {
        crate::acp::AgentKind::Leader
    } else {
        crate::acp::AgentKind::Embedded
    };
    echo_agent_telemetry::external::init(
        echo_agent_runtime::agent::config::resolve_external_otel_config(
            echo_agent_telemetry::external::config::ExternalClientInfo {
                service_version: echo_agent_version::full_version().to_owned(),
                client_version: echo_agent_version::VERSION.to_owned(),
                app_entrypoint: "tui".to_owned(),
            },
        ),
    );
    let pending_startup = echo_agent_telemetry::startup::PendingStartup::new();
    let timer = echo_agent_telemetry::startup::begin(crate::acp::Owner::Client);
    let primary_started = std::time::Instant::now();
    let connect_result = bounded_connect(
        &cancel,
        connect_ui_timeout,
        primary_target,
        startup_failure::ConnectAttempt::First,
        &timer,
        async {
            if use_leader {
                crate::acp::connect_via_leader(&cancel, connect_flags, &raw_config).await
            } else {
                crate::acp::connect(&cancel, connect_flags).await
            }
        },
    )
    .await;
    let (connect_result, embedded_fallback, timer, connect_target) = match connect_result {
        Err(f) if use_leader && !cancel.is_cancelled() => {
            tracing::warn!(error = %f.error, "leader connect failed; falling back to embedded agent");
            timer.emit_telemetry(primary_target, f.outcome, f.timeout_secs, false);
            let flags = fallback_flags.expect("set on the use_leader path");
            let timer = echo_agent_telemetry::startup::begin(crate::acp::Owner::Client);
            let target = crate::acp::AgentKind::Embedded;
            let fallback = bounded_connect(
                &cancel,
                connect_ui_timeout,
                target,
                startup_failure::ConnectAttempt::AfterFallback(startup_failure::EarlierAttempt {
                    target: primary_target,
                    wait: primary_started.elapsed(),
                    outcome: f.outcome,
                    longest_step: f.longest_step,
                }),
                &timer,
                async { crate::acp::connect(&cancel, flags).await },
            )
            .await;
            (fallback, true, timer, target)
        }
        other => (other, false, timer, primary_target),
    };
    let mut connection = match connect_result {
        Ok(conn) => {
            tracing::info!(
                elapsed_ms = startup_start.elapsed().as_millis() as u64,
                use_leader = use_leader && !embedded_fallback,
                embedded_fallback,
                phases = %timer.summary(),
                "Connected"
            );
            timer.emit_telemetry(
                connect_target,
                crate::acp::StartupOutcome::Ok,
                None,
                embedded_fallback,
            );
            conn
        }
        Err(f) => {
            timer.emit_telemetry(connect_target, f.outcome, f.timeout_secs, embedded_fallback);
            if f.outcome == crate::acp::StartupOutcome::Cancelled {
                pending_startup.abandon();
            } else {
                pending_startup.finish(f.outcome);
            }
            crate::unified_log::flush_blocking().await;
            let _ = restore_terminal(terminal, writer_thread, screen_mode);
            cancel.cancel();
            return Err(f.error);
        }
    };
    let agent_guard =
        crate::acp::spawn::AgentShutdownGuard::new(cancel.clone(), connection.agent_thread.take());
    let effective_args = PagerArgs {
        resume_session: None,
        load_session: None,
        continue_last_session: false,
        session_id: None,
        fork_session: false,
        ..args
    };
    let term_state = event_loop::TerminalState {
        is_control_mode,
        screen_mode,
        relaunched_into_minimal,
        relaunched_into_fullscreen,
        initial_theme: crate::theme::cache::current_kind(),
        startup_typeahead,
    };
    let result = event_loop::run(
        &mut terminal,
        connection,
        pending_startup,
        &mut config_watcher,
        &effective_args,
        session_cwd,
        remote_settings,
        term_state,
        materialized,
        bg_update_rx,
        writer_event_rx,
    )
    .await;
    signal_handler::clear_quit_notify();
    let forced_exit_code = match &result {
        Ok(run_result) if run_result.quit_for_update || run_result.relaunch.is_some() => None,
        Ok(_) => Some(0),
        Err(_) => Some(1),
    };
    if let Some(code) = forced_exit_code {
        exit_timeout::arm(code);
        exit_timeout::hold_teardown_for_test();
    }
    crate::unified_log::flush_blocking().await;
    let restore_result = restore_terminal(terminal, writer_thread, current_screen_mode());
    drop(agent_guard);
    echo_agent_telemetry::session_ctx::drain_at_process_exit().await;
    echo_agent_tty_utils::global_process_scope().kill_all();
    crate::app::status_line::metrics::global().report_health();
    if let Err(cleanup_error) = restore_result {
        match &result {
            Ok(_) => {
                tracing::warn!(
                    error = %cleanup_error,
                    "terminal cleanup failed after successful event loop"
                )
            }
            Err(run_error) => {
                tracing::warn!(
                    error = %cleanup_error,
                    run_error = %run_error,
                    "terminal cleanup also failed"
                )
            }
        }
    }
    match result {
        Ok(run_result) => {
            if run_result.quit_for_update {
                return Ok(true);
            }
            if let Some(relaunch) = run_result.relaunch.as_ref() {
                if let Err(e) = screen_mode_relaunch::exec_screen_mode_relaunch(
                    &relaunch.session_id,
                    relaunch.minimal,
                ) {
                    tracing::error!(error = %e, "screen-mode relaunch failed");
                    print_relaunch_failure_hint(
                        &e,
                        &relaunch.session_id,
                        relaunch.minimal,
                        &mut io::stderr(),
                    );
                }
                return Ok(false);
            }
            if let Some(info) = run_result.exit_info {
                let width = crossterm::terminal::size().map_or(80, |(cols, _)| cols as usize);
                print_exit_resume_hint(&info, width, &mut io::stderr());
            }
            Ok(false)
        }
        Err(run_error) => Err(run_error),
    }
}
/// Plain-quit "Resume this session with…" lines (after terminal restore).
///
/// A summary, when present — title, last prompt, last response, one line
/// each, width-truncated — precedes the command so a glance at the pane
/// shows which session lives there and where it left off.
/// Best-effort: closed-pane EIO/BrokenPipe must not panic (`panic = "abort"`).
fn print_exit_resume_hint(info: &ExitInfo, max_width: usize, w: &mut impl Write) {
    use crate::render::line_utils::truncate_str;
    let _ = writeln!(w);
    if let Some(summary) = &info.summary {
        let _ = writeln!(w, "{}", truncate_str(&summary.title, max_width));
        if let Some(prompt) = summary.last_prompt.as_deref() {
            let _ = writeln!(w, "> {}", truncate_str(prompt, max_width.saturating_sub(2)));
        }
        if let Some(response) = summary.last_response.as_deref() {
            let _ = writeln!(
                w,
                "  {}",
                truncate_str(response, max_width.saturating_sub(2))
            );
        }
        let _ = writeln!(w);
    }
    let _ = writeln!(w, "Resume this session with:");
    if info.minimal {
        let _ = writeln!(w, "  echoagent --minimal --resume {}", info.session_id);
    } else {
        let _ = writeln!(w, "  echoagent --resume {}", info.session_id);
    }
}
/// Screen-mode relaunch failure fallback (same quit tail as plain resume).
fn print_relaunch_failure_hint(
    error: &impl std::fmt::Display,
    session_id: &str,
    want_minimal: bool,
    w: &mut impl Write,
) {
    let _ = writeln!(w, "Failed to relaunch in requested mode: {error}");
    let _ = writeln!(w, "Resume this session with:");
    let _ = writeln!(
        w,
        "  {}",
        screen_mode_relaunch::screen_mode_relaunch_resume_hint(session_id, want_minimal),
    );
}
/// Write raw CSI sequences to disable mouse tracking and bracketed paste.
///
/// Best-effort: failures are silently ignored since this runs on teardown
/// and panic paths where stderr may already be broken.
fn disable_mouse_paste_raw() {
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        let _ = stderr.write_all(echo_agent_crash_handler::terminal::MOUSE_PASTE_RESET);
        let _ = stderr.flush();
    });
}
/// Set the console output code page to UTF-8 and enable
/// `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on the stderr console handle.
///
/// **Code page** — The pager outputs UTF-8 (Braille art in the logo, Powerline
/// icons, box-drawing characters). On Windows the default console code page is
/// a legacy OEM page (e.g. CP437), so multi-byte UTF-8 sequences are
/// misinterpreted as individual single-byte characters, producing garbled
/// output. Setting the output code page to 65001 (UTF-8) fixes this.
///
/// **VTP on stderr** — Each console handle (stdin, stdout, stderr) has
/// independent mode flags. `crossterm::enable_raw_mode()` sets flags on stdin
/// only. Since the pager renders to stderr (via `TermWriter`), ANSI sequences
/// for background colors (SGR 48;2;R;G;B), alternate screen, and cursor
/// control must be processed by the stderr handle. Without the VTP flag the
/// console silently drops background-color sequences while foreground colors
/// work, producing the "text renders but backgrounds are missing" symptom.
///
/// Best-effort: if any call fails (e.g. stderr is redirected to a file),
/// the pager continues — rendering may be degraded but the TUI is still usable.
#[cfg(windows)]
fn configure_windows_console() {
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4u32;
    const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const CP_UTF8: u32 = 65001;
    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
        fn GetConsoleMode(hConsoleHandle: *mut core::ffi::c_void, lpMode: *mut u32) -> i32;
        fn SetConsoleMode(hConsoleHandle: *mut core::ffi::c_void, dwMode: u32) -> i32;
        fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
    }
    unsafe {
        SetConsoleOutputCP(CP_UTF8);
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        if handle.is_null() || handle == -1_isize as *mut _ {
            return;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return;
        }
        let _ = SetConsoleMode(
            handle,
            mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        );
    }
}
/// Native drag-to-select on legacy conhost windows (classic `powershell.exe` /
/// `cmd.exe` console host — not Windows Terminal / Warp) is conhost's
/// **QuickEdit** mode, controlled by console-input mode flags on the *stdin*
/// handle, not by DEC private-mode escapes.
///
/// Minimal mode's contract is "the terminal owns the mouse" (design K7), and
/// on conhost merely *skipping* `EnableMouseCapture` is not enough:
///
/// - crossterm's `EnableMouseCapture` is winapi-only on Windows
///   (`is_ansi_code_supported() == false`): it **replaces** the stdin mode
///   with `ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT`.
///   `ENABLE_EXTENDED_FLAGS` without `ENABLE_QUICK_EDIT_MODE` turns QuickEdit
///   *off*.
/// - `SetConsoleMode` state **outlives the process** for the console window,
///   and teardown historically reset mouse state with ANSI sequences only —
///   so one fullscreen/inline run left the window with QuickEdit off and
///   `ENABLE_MOUSE_INPUT` on, breaking native drag-select for every later
///   `--minimal` run in that same window ("works in a fresh cmd window but
///   not in my PowerShell window").
/// - Some PowerShell shortcuts ship QuickEdit disabled per window title
///   (`HKCU\Console\<title>`), so even a pristine window may need it asserted.
///
/// Modern terminals (Windows Terminal, Warp) select host-side and decide "app
/// owns the mouse" from the DEC `?100x` escapes — which minimal never emits —
/// so these conhost flags are inert there and asserting them is harmless.
/// Everything is best-effort: if stdin is not a console, calls are no-ops.
#[cfg(any(windows, test))]
pub(crate) mod win_native_selection {
    const ENABLE_WINDOW_INPUT: u32 = 0x0008;
    const ENABLE_MOUSE_INPUT: u32 = 0x0010;
    const ENABLE_QUICK_EDIT_MODE: u32 = 0x0040;
    const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
    /// Stdin console mode for "terminal owns the mouse": QuickEdit on (with
    /// the extended-flags gate that makes it effective), app-side mouse
    /// reporting off, and window-resize events on (parity with the capture
    /// path — `WINDOW_BUFFER_SIZE_EVENT` is how resize reaches crossterm on
    /// conhost). All other bits are preserved.
    pub(crate) fn native_selection_mode(mode: u32) -> u32 {
        (mode & !ENABLE_MOUSE_INPUT)
            | ENABLE_EXTENDED_FLAGS
            | ENABLE_QUICK_EDIT_MODE
            | ENABLE_WINDOW_INPUT
    }
    #[cfg(windows)]
    pub(crate) use imp::{enable_native_selection, restore_stdin_mode};
    #[cfg(windows)]
    mod imp {
        use std::sync::atomic::{AtomicU64, Ordering};
        const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6u32;
        /// Stdin mode before the first `enable_native_selection`; `u64::MAX`
        /// means "never touched" (same sentinel scheme crossterm uses for its
        /// own capture snapshot). First writer wins, so repeated enables (e.g.
        /// `/mouse` toggles) keep the true original for teardown.
        static ORIGINAL_STDIN_MODE: AtomicU64 = AtomicU64::new(u64::MAX);
        unsafe extern "system" {
            fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
            fn GetConsoleMode(hConsoleHandle: *mut core::ffi::c_void, lpMode: *mut u32) -> i32;
            fn SetConsoleMode(hConsoleHandle: *mut core::ffi::c_void, dwMode: u32) -> i32;
        }
        /// Read the stdin console handle + its current mode. `None` when
        /// stdin is redirected / not a console.
        fn stdin_console_mode() -> Option<(*mut core::ffi::c_void, u32)> {
            unsafe {
                let handle = GetStdHandle(STD_INPUT_HANDLE);
                if handle.is_null() || handle == -1_isize as *mut _ {
                    return None;
                }
                let mut mode: u32 = 0;
                if GetConsoleMode(handle, &mut mode) == 0 {
                    return None;
                }
                Some((handle, mode))
            }
        }
        /// Assert the native-selection stdin mode (QuickEdit on, app mouse
        /// reporting off, resize events on), snapshotting the original mode
        /// once for [`restore_stdin_mode`].
        pub(crate) fn enable_native_selection() {
            let Some((handle, mode)) = stdin_console_mode() else {
                return;
            };
            let _ = ORIGINAL_STDIN_MODE.compare_exchange(
                u64::MAX,
                u64::from(mode),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            let new_mode = super::native_selection_mode(mode);
            if new_mode != mode {
                unsafe {
                    let _ = SetConsoleMode(handle, new_mode);
                }
            }
        }
        /// Restore the mode captured by the first `enable_native_selection`
        /// (no-op if it never ran). Teardown-only; consumes the snapshot so
        /// concurrent teardown paths (panic hook + restore_terminal) restore
        /// at most once.
        pub(crate) fn restore_stdin_mode() {
            let saved = ORIGINAL_STDIN_MODE.swap(u64::MAX, Ordering::AcqRel);
            let Ok(saved) = u32::try_from(saved) else {
                return;
            };
            if let Some((handle, current)) = stdin_console_mode() {
                if current != saved {
                    unsafe {
                        let _ = SetConsoleMode(handle, saved);
                    }
                }
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
        const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
        #[test]
        fn asserts_quick_edit_and_resize_clears_mouse_input() {
            let mode = native_selection_mode(ENABLE_MOUSE_INPUT);
            assert_eq!(mode & ENABLE_MOUSE_INPUT, 0, "app mouse reporting off");
            assert_ne!(mode & ENABLE_QUICK_EDIT_MODE, 0, "QuickEdit on");
            assert_ne!(mode & ENABLE_EXTENDED_FLAGS, 0, "extended-flags gate on");
            assert_ne!(mode & ENABLE_WINDOW_INPUT, 0, "resize events on");
        }
        #[test]
        fn preserves_unrelated_bits() {
            let input = ENABLE_PROCESSED_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT;
            let mode = native_selection_mode(input);
            assert_eq!(mode & input, input);
        }
        #[test]
        fn idempotent() {
            let once = native_selection_mode(ENABLE_MOUSE_INPUT | ENABLE_PROCESSED_INPUT);
            assert_eq!(native_selection_mode(once), once);
        }
        /// The crossterm capture mode (what a crashed prior run leaves behind)
        /// maps to a QuickEdit-on, reporting-off mode.
        #[test]
        fn recovers_from_stale_crossterm_capture_mode() {
            const CROSSTERM_ENABLE_MOUSE_MODE: u32 =
                ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT;
            let mode = native_selection_mode(CROSSTERM_ENABLE_MOUSE_MODE);
            assert_eq!(mode & ENABLE_MOUSE_INPUT, 0);
            assert_ne!(mode & ENABLE_QUICK_EDIT_MODE, 0);
        }
    }
}
/// Startup cursor-style policy from `[ui].cursor_blink`: `Inherit` (the
/// `None` default) emits no style escapes, so the terminal's configured
/// cursor shape/blink survives; forcing one was reported as cursor flicker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorStylePolicy {
    /// Leave the terminal's cursor style untouched (default).
    Inherit,
    /// Legacy: `EnableBlinking` + `SetCursorStyle::BlinkingBlock`.
    ForceBlinking,
    /// `DisableBlinking` + `SetCursorStyle::SteadyBlock`.
    ForceSteady,
}
/// Map the `[ui].cursor_blink` tri-state onto the startup policy.
fn cursor_style_policy(cursor_blink: Option<bool>) -> CursorStylePolicy {
    match cursor_blink {
        None => CursorStylePolicy::Inherit,
        Some(true) => CursorStylePolicy::ForceBlinking,
        Some(false) => CursorStylePolicy::ForceSteady,
    }
}
/// Outcome of [`init_terminal`]: the live terminal, the effective screen mode,
/// and any startup type-ahead captured after raw mode was enabled.
pub(crate) struct TerminalInit {
    pub terminal: PagerTerminal,
    /// The *effective* screen mode, which may differ from the requested one (see
    /// [`init_terminal`]).
    pub screen_mode: ScreenMode,
    /// Keystrokes the user typed while the app was still loading, captured by the
    /// post-raw-mode drains; replayed into the composer by [`event_loop::run`].
    pub startup_typeahead: Vec<event_loop::TimedInputEvent>,
}
/// Initialize the terminal for `mode`. Returns the live terminal handle and the
/// *effective* screen mode, which may differ from the requested one: a
/// `Minimal` request downgrades to `Inline` if the inline-viewport probe fails
/// (its `insert_before` / `set_viewport_height` commit pipeline is a no-op on
/// the `Viewport::Fixed` fallback, so minimal cannot function there). Also
/// returns any startup type-ahead captured by the post-raw-mode drains.
fn init_terminal(
    mode: ScreenMode,
    minimal_live_rows: u16,
    clear_main_screen: bool,
    frame_tx: crate::render::draw::WriterSender,
    writer_sync: crate::render::draw::WriterSync,
    cursor_blink: Option<bool>,
) -> io::Result<TerminalInit> {
    echo_agent_crash_handler::enable_terminal_escape_restore();
    terminal::enable_raw_mode()?;
    #[cfg(windows)]
    configure_windows_console();
    let want_minimal = mode.is_minimal();
    let mut startup_typeahead: Vec<event_loop::TimedInputEvent> = Vec::new();
    let (terminal, screen_mode) = (|| -> io::Result<(PagerTerminal, ScreenMode)> {
        startup_typeahead.extend(event_loop::capture_startup_typeahead(
            std::time::Duration::from_millis(0),
        ));
        set_terminal_title("");
        if want_minimal && clear_main_screen {
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                execute!(
                    stderr,
                    Clear(ClearType::All),
                    Clear(ClearType::Purge),
                    cursor::MoveTo(0, 0),
                )
            })?;
        }
        if mode.is_fullscreen() {
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                execute!(stderr, EnterAlternateScreen)
            })?;
        }
        #[cfg(windows)]
        if want_minimal {
            win_native_selection::enable_native_selection();
        }
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            if !want_minimal {
                execute!(stderr, event::EnableMouseCapture)?;
            } else if crate::terminal::terminal_context().mouse_reporting_leaks_as_raw_text() {
                let _ = stderr.write_all(echo_agent_crash_handler::terminal::MOUSE_TRACKING_RESET);
            }
            execute!(
                stderr,
                event::EnableFocusChange,
                event::EnableBracketedPaste,
                cursor::Hide,
            )?;
            let policy = cursor_style_policy(cursor_blink);
            match policy {
                CursorStylePolicy::Inherit => {}
                CursorStylePolicy::ForceBlinking => {
                    execute!(
                        stderr,
                        cursor::EnableBlinking,
                        SetCursorStyle::BlinkingBlock
                    )?;
                }
                CursorStylePolicy::ForceSteady => {
                    execute!(stderr, cursor::DisableBlinking, SetCursorStyle::SteadyBlock)?;
                }
            }
            CURSOR_STYLE_FORCED.store(policy != CursorStylePolicy::Inherit, Ordering::Release);
            io::Result::Ok(())
        })?;
        MOUSE_CAPTURE_ENABLED.store(!want_minimal, Ordering::Release);
        set_current_screen_mode(mode);
        set_panic_hook();
        signal_handler::install(mode);
        let drain_timeout = if crate::terminal::terminal_context().vte_version.is_some() {
            std::time::Duration::from_millis(20)
        } else {
            std::time::Duration::ZERO
        };
        startup_typeahead.extend(event_loop::capture_startup_typeahead(drain_timeout));
        crate::theme::apply_cursor_color();
        let ctx = crate::terminal::terminal_context();
        let skip_reason: Option<&str> =
            ctx.kitty_skip_reason()
                .or_else(|| match terminal::supports_keyboard_enhancement() {
                    Ok(true) => None,
                    _ => Some("unsupported"),
                });
        crate::terminal::da2::probe_at_startup();
        let flags = crate::terminal::negotiated_kitty_flags(
            skip_reason,
            crate::terminal::da2::detected_packed(),
        );
        if flags.is_empty() {
            tracing::info!(
                kitty.flags = "none",
                kitty.skipped_reason = skip_reason.unwrap_or("unknown"),
                "kitty keyboard protocol skipped"
            );
        } else {
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                let _ = execute!(stderr, event::PushKeyboardEnhancementFlags(flags));
            });
            tracing::info!(
                kitty.flags = ?flags,
                kitty.disambiguate = true,
                kitty.report_event_types =
                    flags.contains(event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES),
                kitty.report_all_keys = false,
                "kitty keyboard protocol pushed"
            );
        }
        crate::terminal::set_pushed_kitty_flags(flags);
        if mode.is_fullscreen() {
            let backend = CrosstermBackend::new(
                crate::render::draw::TermWriter::new(frame_tx, writer_sync)
                    .map_err(io::Error::other)?,
            );
            Ok((
                echo_agent_ratatui_inline::Terminal::new(backend)?,
                ScreenMode::Fullscreen,
            ))
        } else {
            let (cols, rows) = crossterm::terminal::size()?;
            let viewport_rows = if want_minimal {
                minimal_live_rows.clamp(3, rows.saturating_sub(1).max(3))
            } else {
                rows
            };
            let probe_backend = CrosstermBackend::new(
                crate::render::draw::TermWriter::new(frame_tx.clone(), writer_sync.clone())
                    .map_err(io::Error::other)?,
            );
            if let Ok(term) = echo_agent_ratatui_inline::Terminal::with_options(
                probe_backend,
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Inline(viewport_rows),
                },
            ) {
                return Ok((
                    term,
                    if want_minimal {
                        ScreenMode::Minimal
                    } else {
                        ScreenMode::Inline
                    },
                ));
            }
            if want_minimal {
                tracing::warn!(
                    "minimal: inline viewport probe failed; downgrading to full-height inline"
                );
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    execute!(stderr, event::EnableMouseCapture)
                })?;
                MOUSE_CAPTURE_ENABLED.store(true, Ordering::Release);
                let retry_backend = CrosstermBackend::new(
                    crate::render::draw::TermWriter::new(frame_tx.clone(), writer_sync.clone())
                        .map_err(io::Error::other)?,
                );
                if let Ok(term) = echo_agent_ratatui_inline::Terminal::with_options(
                    retry_backend,
                    ratatui::TerminalOptions {
                        viewport: ratatui::Viewport::Inline(rows),
                    },
                ) {
                    return Ok((term, ScreenMode::Inline));
                }
            } else {
                tracing::error!("inline viewport probe failed, using Viewport::Fixed");
            }
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                execute!(
                    stderr,
                    crossterm::terminal::ScrollUp(rows),
                    cursor::MoveTo(0, 0),
                )
            })?;
            let backend = CrosstermBackend::new(
                crate::render::draw::TermWriter::new(frame_tx, writer_sync)
                    .map_err(io::Error::other)?,
            );
            let term = echo_agent_ratatui_inline::Terminal::with_options(
                backend,
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(
                        0, 0, cols, rows,
                    )),
                },
            )?;
            Ok((term, ScreenMode::Inline))
        }
    })()
    .inspect_err(|_| {
        emit_terminal_teardown_sequences(mode, None);
        let _ = terminal::disable_raw_mode();
        signal_handler::mark_restored();
        echo_agent_crash_handler::disable_terminal_escape_restore();
    })?;
    Ok(TerminalInit {
        terminal,
        screen_mode,
        startup_typeahead,
    })
}
/// Drop the terminal (closing the writer mpsc channel) and join the
/// writer thread. After this returns, subsequent direct stderr writes
/// are guaranteed to land strictly after every queued frame.
fn drain_writer_thread_before_teardown(
    terminal: PagerTerminal,
    writer_thread: crate::render::draw::WriterThread,
) -> io::Result<()> {
    drop(terminal);
    writer_thread.join()
}
/// Inline teardown escape sequences in the canonical order, shared by
/// `restore_terminal` and `set_panic_hook` so the on-wire byte order is
/// defined exactly once.
///
/// Order: EndSynchronizedUpdate -> reset_cursor_color ->
/// disable_mouse_paste_raw -> DisableFocusChange -> pop kitty (if pushed)
/// -> mode-specific final block. EndSynchronizedUpdate is emitted first so multiplexers
/// (zellij/tmux) stop buffering before the resets arrive. Does NOT call
/// `disable_raw_mode`. Callers should drain queued writer-thread frames
/// first when possible; the panic hook can't (would deadlock).
fn emit_terminal_teardown_sequences(mode: ScreenMode, inline_cursor_row: Option<u16>) {
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        let _ = stderr.write_all(crate::notifications::progress::OSC_CLEAR.as_bytes());
        let _ = stderr.flush();
    });
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        let _ = execute!(stderr, crossterm::terminal::EndSynchronizedUpdate);
    });
    crate::theme::reset_cursor_color();
    disable_mouse_paste_raw();
    if MOUSE_CAPTURE_ENABLED.swap(false, Ordering::AcqRel) {
        #[cfg(windows)]
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            let _ = execute!(stderr, event::DisableMouseCapture);
        });
    }
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        let _ = execute!(stderr, event::DisableFocusChange);
    });
    pop_gboom_keyboard_flags();
    if crate::terminal::take_kitty_flags_pushed() {
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            let _ = execute!(stderr, event::PopKeyboardEnhancementFlags);
        });
    }
    let restore_style = CURSOR_STYLE_FORCED.load(Ordering::Acquire);
    if mode.is_fullscreen() {
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            if restore_style {
                let _ = execute!(stderr, SetCursorStyle::DefaultUserShape);
            }
            let _ = execute!(stderr, cursor::Show, LeaveAlternateScreen);
        });
    } else {
        let rows = crossterm::terminal::size().map(|(_, r)| r).unwrap_or(24);
        let last = rows.saturating_sub(1);
        let target = inline_cursor_row.unwrap_or(last).min(last);
        echo_agent_runtime::util::with_locked_stderr(|stderr| {
            if restore_style {
                let _ = execute!(stderr, SetCursorStyle::DefaultUserShape);
            }
            let _ = execute!(stderr, cursor::MoveTo(0, target), cursor::Show);
            let _ = writeln!(stderr);
            let _ = stderr.flush();
        });
    }
    #[cfg(windows)]
    win_native_selection::restore_stdin_mode();
}
/// Consumes `terminal` and `writer_thread`: queues a final fullscreen clear,
/// drains every accepted frame, then emits teardown sequences. Teardown still
/// runs if draining fails, so terminal state is restored before returning that
/// error. Draining first prevents a late frame after `LeaveAlternateScreen`.
fn restore_terminal_with(
    mut terminal: PagerTerminal,
    writer_thread: crate::render::draw::WriterThread,
    mode: ScreenMode,
    drain: impl FnOnce(PagerTerminal, crate::render::draw::WriterThread) -> io::Result<()>,
    teardown: impl FnOnce(ScreenMode, Option<u16>),
) -> io::Result<()> {
    if mode.is_fullscreen() && !writer_thread.writer_sync().failed() {
        let _ = terminal.clear();
        {
            use std::io::Write;
            let _ = terminal.backend_mut().flush();
        }
    }
    let inline_cursor_row = (!mode.is_fullscreen()).then(|| terminal.viewport_area().bottom());
    let drain_result = drain(terminal, writer_thread);
    teardown(mode, inline_cursor_row);
    let _ = event_loop::drain_pending_events(std::time::Duration::from_millis(10), |_| false);
    let _ = terminal::disable_raw_mode();
    signal_handler::mark_restored();
    echo_agent_crash_handler::disable_terminal_escape_restore();
    echo_agent_tty_utils::restore_native_stderr();
    drain_result
}
fn restore_terminal(
    terminal: PagerTerminal,
    writer_thread: crate::render::draw::WriterThread,
    mode: ScreenMode,
) -> io::Result<()> {
    restore_terminal_with(
        terminal,
        writer_thread,
        mode,
        drain_writer_thread_before_teardown,
        emit_terminal_teardown_sequences,
    )
}
pub(crate) fn set_terminal_title(title: &str) {
    let full = terminal_title_string(title);
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        let _ = execute!(stderr, SetTitle(full));
    });
}
/// Sanitized/truncated window title. Strips control characters: crossterm's
/// `SetTitle` emits the string raw inside an OSC sequence, so an embedded
/// BEL/ESC (titles can arrive from cloud.echo-agent.invalid conversation metadata) would
/// terminate the OSC early and let the remainder inject arbitrary escape
/// sequences into the terminal.
fn terminal_title_string(title: &str) -> String {
    let sanitized: String = title.chars().filter(|c| !c.is_control()).collect();
    if sanitized.is_empty() {
        "echoagent".into()
    } else {
        let truncated: String = sanitized.chars().take(80 - 6).collect();
        format!("{} - echoagent", truncated)
    }
}
/// Reads [`current_screen_mode`] at panic time — never capture a mode here,
/// or an in-process mode switch tears down the wrong screen.
fn set_panic_hook() {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        emit_terminal_teardown_sequences(current_screen_mode(), None);
        let _ = terminal::disable_raw_mode();
        signal_handler::mark_restored();
        echo_agent_crash_handler::disable_terminal_escape_restore();
        echo_agent_tty_utils::restore_native_stderr();
        echo_agent_tty_utils::global_process_scope().kill_all();
        crate::memory_trace::record_crash_sample();
        hook(info);
    }));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_runs_teardown_even_when_writer_failed() {
        use ratatui::{TerminalOptions, Viewport};
        let (tx, _rx) = std::sync::mpsc::channel::<crate::render::draw::WriterPayload>();
        let sync = crate::render::draw::WriterSync::new();
        let backend = CrosstermBackend::new(
            crate::render::draw::TermWriter::new(tx, sync).expect("single test writer"),
        );
        let terminal = echo_agent_ratatui_inline::Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
            },
        )
        .expect("test terminal");
        let (writer_tx, _writer_sync, _events, writer_thread) =
            crate::render::draw::spawn_writer_thread();
        drop(writer_tx);
        let teardown_called = std::cell::Cell::new(false);
        let result = restore_terminal_with(
            terminal,
            writer_thread,
            ScreenMode::Inline,
            |terminal, writer_thread| {
                drop(terminal);
                drop(writer_thread);
                Err(io::Error::other("injected drain failure"))
            },
            |_, _| teardown_called.set(true),
        );
        assert!(result.is_err());
        assert!(teardown_called.get());
    }
    /// `[ui].cursor_blink` tri-state → startup cursor policy; the `None`
    /// default must be Inherit (emit nothing).
    #[test]
    fn cursor_blink_config_maps_to_policy() {
        assert_eq!(cursor_style_policy(None), CursorStylePolicy::Inherit);
        assert_eq!(
            cursor_style_policy(Some(true)),
            CursorStylePolicy::ForceBlinking
        );
        assert_eq!(
            cursor_style_policy(Some(false)),
            CursorStylePolicy::ForceSteady
        );
    }
    fn empty_config() -> toml::Value {
        toml::Value::Table(Default::default())
    }
    fn config_with_leader(enabled: bool) -> toml::Value {
        let toml_str = format!("[cli]\nuse_leader = {enabled}");
        toml::from_str(&toml_str).unwrap()
    }
    #[test]
    fn terminal_title_strips_control_characters() {
        assert_eq!(
            terminal_title_string("evil\x07\x1b]52;c;payload\x07title"),
            "evil]52;c;payloadtitle - echoagent"
        );
        assert_eq!(terminal_title_string("\x07\x1b\x00"), "echoagent");
        assert_eq!(terminal_title_string(""), "echoagent");
        assert_eq!(terminal_title_string("My chat"), "My chat - echoagent");
    }
    #[test]
    fn hunk_tracker_mode_nothing_set_is_none() {
        assert_eq!(resolve_hunk_tracker_mode(None, None, None), None);
    }
    #[test]
    fn hunk_tracker_mode_empty_env_is_none() {
        assert_eq!(resolve_hunk_tracker_mode(None, Some(""), None), None);
        assert_eq!(resolve_hunk_tracker_mode(None, Some("   "), None), None);
        assert_eq!(resolve_hunk_tracker_mode(None, None, Some("")), None);
    }
    #[test]
    fn hunk_tracker_mode_precedence_cli_over_env_over_config() {
        assert_eq!(
            resolve_hunk_tracker_mode(Some("off"), Some("all_dirty"), Some("agent_only")),
            Some("off".to_string()),
        );
        assert_eq!(
            resolve_hunk_tracker_mode(Some(""), Some("all_dirty"), Some("agent_only")),
            Some("all_dirty".to_string()),
        );
        assert_eq!(
            resolve_hunk_tracker_mode(Some("  "), Some(""), Some("agent_only")),
            Some("agent_only".to_string()),
        );
    }
    #[test]
    fn hunk_tracker_mode_trims_and_passes_off_through() {
        assert_eq!(
            resolve_hunk_tracker_mode(Some(" off "), None, None),
            Some("off".to_string()),
        );
        assert_eq!(
            resolve_hunk_tracker_mode(None, Some("disabled"), None),
            Some("disabled".to_string()),
        );
    }
    #[test]
    fn no_leader_flag_wins_over_leader_flag_and_config() {
        let cfg = config_with_leader(true);
        let (use_leader, reason) = resolve_use_leader(true, true, &cfg, None, true, None);
        assert!(!use_leader);
        assert_eq!(reason, None);
    }
    #[test]
    fn leader_flag_enables() {
        let (use_leader, reason) =
            resolve_use_leader(true, false, &empty_config(), None, true, None);
        assert!(use_leader);
        assert_eq!(reason, None);
    }
    #[test]
    fn not_eligible_returns_false() {
        let cfg = config_with_leader(true);
        let (use_leader, reason) = resolve_use_leader(false, false, &cfg, None, false, None);
        assert!(!use_leader);
        assert_eq!(reason, None);
    }
    #[test]
    fn config_toml_enables() {
        let cfg = config_with_leader(true);
        let (use_leader, reason) = resolve_use_leader(false, false, &cfg, None, true, None);
        assert!(use_leader);
        assert_eq!(reason, None);
    }
    #[test]
    fn config_toml_disables() {
        let cfg = config_with_leader(false);
        let (use_leader, reason) = resolve_use_leader(false, false, &cfg, None, true, None);
        assert!(!use_leader);
        assert_eq!(reason, Some("config"));
    }
    #[test]
    fn default_is_false() {
        let (use_leader, reason) =
            resolve_use_leader(false, false, &empty_config(), None, true, None);
        assert!(!use_leader);
        assert_eq!(reason, None);
    }
    #[test]
    fn cli_flag_overrides_config() {
        let cfg = config_with_leader(false);
        let (use_leader, reason) = resolve_use_leader(true, false, &cfg, None, true, None);
        assert!(use_leader);
        assert_eq!(reason, None);
    }
    #[test]
    fn sandbox_confinement_refuses_leader_even_with_leader_flag_and_config_on() {
        let cfg = config_with_leader(true);
        let (use_leader, reason) =
            resolve_use_leader(true, false, &cfg, None, true, Some("strict"));
        assert!(!use_leader);
        assert_eq!(reason, None);
    }
    /// `disabled_by_confinement` for the four leader × sandbox cells, driven by
    /// every input that can decide leader mode — not just `[cli] use_leader`.
    #[test]
    fn matrix_reports_the_profile_only_when_the_sandbox_takes_leader_mode_away() {
        let on = config_with_leader(true);
        let off = config_with_leader(false);
        let sandbox = Some("strict");
        for (label, leader_flag, cfg) in [
            ("config on", false, &on),
            ("--leader", true, &empty_config()),
            ("--leader over config off", true, &off),
        ] {
            let resolved = resolve_leader_mode(leader_flag, false, cfg, None, true, sandbox);
            assert!(!resolved.use_leader, "{label}: leader must be vetoed");
            assert_eq!(
                resolved.disabled_by_confinement,
                Some("strict"),
                "{label}: the profile that took leader mode away must be named"
            );
        }
        for (label, cfg, expect_leader) in [("leader on", &on, true), ("leader off", &off, false)] {
            let resolved = resolve_leader_mode(false, false, cfg, None, true, None);
            assert_eq!(resolved.use_leader, expect_leader, "{label}");
            assert_eq!(resolved.disabled_by_confinement, None, "{label}");
        }
        for (label, leader_flag, no_leader_flag, cfg, eligible) in [
            ("config off", false, false, &off, true),
            ("--no-leader over config on", false, true, &on, true),
            ("default", false, false, &empty_config(), true),
            ("ineligible mode with config on", false, false, &on, false),
        ] {
            let resolved =
                resolve_leader_mode(leader_flag, no_leader_flag, cfg, None, eligible, sandbox);
            assert!(!resolved.use_leader, "{label}");
            assert_eq!(
                resolved.disabled_by_confinement, None,
                "{label}: the sandbox took nothing away, so it must stay silent"
            );
        }
    }
    #[test]
    fn sandbox_notice_names_the_profile_without_promising_enforcement() {
        let mut out = Vec::new();
        print_leader_disabled_by_sandbox("strict", &mut out);
        let msg = String::from_utf8(out).expect("utf-8");
        assert!(msg.contains("'strict'"), "must name the profile: {msg}");
        assert!(
            msg.contains("was requested"),
            "must describe the request, not enforcement: {msg}"
        );
        assert!(
            !msg.contains("is active"),
            "must not claim the profile is enforced: {msg}"
        );
        assert!(
            msg.contains("Disable the profile at the source"),
            "must say how to get leader mode back: {msg}"
        );
        assert_eq!(msg.lines().count(), 1, "single line: {msg}");
    }
    #[test]
    fn sandbox_confinement_preserves_config_off_reclaim_reason() {
        let cfg = config_with_leader(false);
        let (use_leader, reason) =
            resolve_use_leader(false, false, &cfg, None, true, Some("strict"));
        assert!(!use_leader);
        assert_eq!(reason, Some("config"));
    }
    fn try_parse_pager(args: &[&str]) -> Result<PagerArgs, clap::Error> {
        use clap::Parser;
        PagerArgs::try_parse_from(args)
    }
    #[test]
    fn cli_leader_and_no_leader_conflict() {
        let result = try_parse_pager(&["echo-agent-pager", "--leader", "--no-leader"]);
        assert!(result.is_err());
    }
    #[test]
    fn cli_leader_flag_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "--leader"]).unwrap();
        assert!(args.leader);
        assert!(!args.no_leader);
    }
    #[test]
    fn cli_no_leader_flag_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "--no-leader"]).unwrap();
        assert!(!args.leader);
        assert!(args.no_leader);
    }
    #[test]
    fn cli_hidden_memory_compat_flags_parse_and_collapse() {
        let enabled = try_parse_pager(&["echo-agent-pager", "--experimental-memory"]).unwrap();
        assert_eq!(enabled.memory_enabled_override(), Some(true));
        assert_eq!(
            enabled.memory_override_flag(),
            Some("--experimental-memory")
        );
        let disabled = try_parse_pager(&["echo-agent-pager", "--no-memory"]).unwrap();
        assert_eq!(disabled.memory_enabled_override(), Some(false));
        assert_eq!(disabled.memory_override_flag(), Some("--no-memory"));
        let deferred = try_parse_pager(&["echo-agent-pager"]).unwrap();
        assert_eq!(deferred.memory_enabled_override(), None);
        assert_eq!(deferred.memory_override_flag(), None);
    }
    #[test]
    fn cli_hidden_memory_compat_flags_conflict() {
        assert!(
            try_parse_pager(&["echo-agent-pager", "--experimental-memory", "--no-memory"]).is_err()
        );
    }
    #[test]
    fn cli_neither_leader_flag_defaults_false() {
        let args = try_parse_pager(&["echo-agent-pager"]).unwrap();
        assert!(!args.leader);
        assert!(!args.no_leader);
    }
    #[test]
    fn no_leader_flag_overrides_config_for_tui_fallback() {
        let cfg = config_with_leader(true);
        let (use_leader, reason) = resolve_use_leader(false, true, &cfg, None, true, None);
        assert!(!use_leader);
        assert_eq!(reason, None);
    }
    /// clap accepts top-level --leader with agent subcommand, so
    /// main() must reject the combination at runtime.
    #[test]
    fn cli_top_level_leader_with_agent_subcommand_parses_flag() {
        let args = try_parse_pager(&["echo-agent-pager", "--leader", "agent"]).unwrap();
        assert!(args.leader);
        assert!(matches!(args.command, Some(Command::Agent(_))));
    }
    #[test]
    fn cli_top_level_no_leader_with_agent_subcommand_parses_flag() {
        let args = try_parse_pager(&["echo-agent-pager", "--no-leader", "agent"]).unwrap();
        assert!(args.no_leader);
        assert!(matches!(args.command, Some(Command::Agent(_))));
    }
    #[test]
    fn remote_settings_none_falls_through_to_default() {
        let (use_leader, reason) =
            resolve_use_leader(false, false, &empty_config(), None, true, None);
        assert!(!use_leader);
        assert_eq!(reason, None);
    }
    #[cfg(feature = "release-dist")]
    #[test]
    fn remote_settings_leader_mode_true_enables_leader() {
        let rs = echo_agent_runtime::util::config::RemoteSettings {
            leader_mode: Some(true),
            ..Default::default()
        };
        let (use_leader, reason) =
            resolve_use_leader(false, false, &empty_config(), Some(&rs), true, None);
        assert!(use_leader);
        assert_eq!(reason, None);
    }
    #[cfg(feature = "release-dist")]
    #[test]
    fn remote_settings_leader_mode_false_disables_leader() {
        let rs = echo_agent_runtime::util::config::RemoteSettings {
            leader_mode: Some(false),
            ..Default::default()
        };
        let (use_leader, reason) =
            resolve_use_leader(false, false, &empty_config(), Some(&rs), true, None);
        assert!(!use_leader);
        assert_eq!(reason, Some("remote"));
    }
    #[cfg(feature = "release-dist")]
    #[test]
    fn remote_settings_unknown_leader_mode_is_not_policy_disable() {
        let rs = echo_agent_runtime::util::config::RemoteSettings {
            leader_mode: None,
            ..Default::default()
        };
        let (use_leader, reason) =
            resolve_use_leader(false, false, &empty_config(), Some(&rs), true, None);
        assert!(!use_leader);
        assert_eq!(reason, None);
    }
    #[cfg(feature = "release-dist")]
    #[test]
    fn config_toml_overrides_remote_settings() {
        let rs = echo_agent_runtime::util::config::RemoteSettings {
            leader_mode: Some(true),
            ..Default::default()
        };
        let cfg = config_with_leader(false);
        let (use_leader, reason) = resolve_use_leader(false, false, &cfg, Some(&rs), true, None);
        assert!(!use_leader);
        assert_eq!(reason, Some("config"));
    }
    #[test]
    fn cli_resume_parses_session_id() {
        let args = try_parse_pager(&["echo-agent-pager", "--resume", "abc-123"]).unwrap();
        assert_eq!(args.session_to_resume(), Some("abc-123"));
    }
    #[test]
    fn cli_short_r_parses_session_id() {
        let args = try_parse_pager(&["echo-agent-pager", "-r", "abc-123"]).unwrap();
        assert_eq!(args.session_to_resume(), Some("abc-123"));
    }
    #[test]
    fn cli_load_alias_parses_session_id() {
        let args = try_parse_pager(&["echo-agent-pager", "--load", "abc-123"]).unwrap();
        assert_eq!(args.session_to_resume(), Some("abc-123"));
    }
    #[test]
    fn cli_resume_preferred_over_load() {
        let mut args = try_parse_pager(&["echo-agent-pager", "--resume", "from-resume"]).unwrap();
        args.load_session = Some("from-load".into());
        assert_eq!(args.session_to_resume(), Some("from-resume"));
    }
    #[test]
    fn cli_continue_flag_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "--continue"]).unwrap();
        assert!(args.continue_last_session);
        assert_eq!(args.session_to_resume(), None);
    }
    #[test]
    fn cli_continue_short_c_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "-c"]).unwrap();
        assert!(args.continue_last_session);
    }
    #[test]
    fn cli_resume_no_id_sets_empty_sentinel() {
        let args = try_parse_pager(&["echo-agent-pager", "--resume"]).unwrap();
        assert_eq!(args.resume_session.as_deref(), Some(""));
        assert!(args.resume_most_recent());
        assert_eq!(args.session_to_resume(), None);
    }
    #[test]
    fn cli_short_r_no_id_sets_empty_sentinel() {
        let args = try_parse_pager(&["echo-agent-pager", "-r"]).unwrap();
        assert_eq!(args.resume_session.as_deref(), Some(""));
        assert!(args.resume_most_recent());
    }
    #[test]
    fn cli_resume_with_id_is_not_most_recent() {
        let args = try_parse_pager(&["echo-agent-pager", "--resume", "abc-123"]).unwrap();
        assert!(!args.resume_most_recent());
        assert_eq!(args.session_to_resume(), Some("abc-123"));
    }
    #[test]
    fn cli_no_resume_is_not_most_recent() {
        let args = try_parse_pager(&["echo-agent-pager"]).unwrap();
        assert!(!args.resume_most_recent());
    }
    #[test]
    fn cli_continue_conflicts_with_resume() {
        let result = try_parse_pager(&["echo-agent-pager", "--continue", "--resume", "abc"]);
        assert!(result.is_err());
    }
    #[test]
    fn cli_continue_conflicts_with_load() {
        let result = try_parse_pager(&["echo-agent-pager", "--continue", "--load", "abc"]);
        assert!(result.is_err());
    }
    #[test]
    fn cli_no_session_flags_defaults() {
        let args = try_parse_pager(&["echo-agent-pager"]).unwrap();
        assert!(!args.continue_last_session);
        assert!(args.worktree.is_none());
        assert_eq!(args.session_to_resume(), None);
        assert!(!args.chat());
    }
    /// Without the optional feature the flag must not exist at all: a stable
    /// binary given that flag fails clap parsing instead of silently ignoring.
    #[test]
    fn cli_chat_flag_rejected_without_feature() {
        assert!(try_parse_pager(&["echo-agent-pager", "--chat"]).is_err());
    }
    #[cfg(feature = "local-workspace")]
    #[test]
    fn cli_local_workspace_attach_requires_chat() {
        assert!(
            try_parse_pager(&["echo-agent-pager", "--local-workspace-attach=srv"]).is_err(),
            "attach without --chat must clap-error"
        );
        let args = try_parse_pager(&["echo-agent-pager", "--chat", "--local-workspace-attach=srv"])
            .unwrap();
        assert_eq!(args.local_workspace_attach(), Some("srv"));
    }
    #[cfg(feature = "local-workspace")]
    #[test]
    fn cli_local_workspace_own_conflicts_with_attach() {
        assert!(
            try_parse_pager(&[
                "echo-agent-pager",
                "--chat",
                "--local-workspace=/tmp/a",
                "--local-workspace-attach=srv",
            ])
            .is_err(),
            "own + attach must clap-conflict"
        );
    }
    #[cfg(feature = "local-workspace")]
    #[test]
    fn cli_local_workspace_cwd_requires_chat() {
        assert!(try_parse_pager(&["echo-agent-pager", "--local-workspace-cwd=/tmp/a"]).is_err());
        let args = try_parse_pager(&[
            "echo-agent-pager",
            "--chat",
            "--local-workspace-attach=srv",
            "--local-workspace-cwd=/tmp/repo",
        ])
        .unwrap();
        assert_eq!(
            args.local_workspace_cwd(),
            Some(std::path::Path::new("/tmp/repo"))
        );
    }
    #[test]
    fn cli_local_workspace_flags_rejected_without_feature() {
        assert!(try_parse_pager(&["echo-agent-pager", "--local-workspace-attach=srv"]).is_err());
        assert!(try_parse_pager(&["echo-agent-pager", "--local-workspace"]).is_err());
        assert!(try_parse_pager(&["echo-agent-pager", "--local-workspace-cwd=/tmp"]).is_err());
    }
    #[test]
    fn chat_mode_leader_guard_truth_table() {
        assert!(session_startup::chat_mode_conflicts_with_leader(true, true));
        assert!(!session_startup::chat_mode_conflicts_with_leader(
            true, false
        ));
        assert!(!session_startup::chat_mode_conflicts_with_leader(
            false, true
        ));
        assert!(!session_startup::chat_mode_conflicts_with_leader(
            false, false
        ));
    }
    #[test]
    fn cli_worktree_flag_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "--worktree"]).unwrap();
        assert_eq!(args.worktree.as_deref(), Some(""));
    }
    #[test]
    fn cli_worktree_short_w_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "-w"]).unwrap();
        assert_eq!(args.worktree.as_deref(), Some(""));
    }
    #[test]
    fn cli_worktree_with_label() {
        let args = try_parse_pager(&["echo-agent-pager", "-w", "my-label"]).unwrap();
        assert_eq!(args.worktree.as_deref(), Some("my-label"));
    }
    #[test]
    fn cli_worktree_long_with_label() {
        let args = try_parse_pager(&["echo-agent-pager", "--worktree", "fix-bug"]).unwrap();
        assert_eq!(args.worktree.as_deref(), Some("fix-bug"));
    }
    #[test]
    fn cli_worktree_with_empty_string() {
        let args = try_parse_pager(&["echo-agent-pager", "-w", ""]).unwrap();
        assert_eq!(args.worktree.as_deref(), Some(""));
    }
    #[test]
    fn cli_worktree_with_resume_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "-w", "--resume", "abc"]).unwrap();
        assert_eq!(args.worktree.as_deref(), Some(""));
        assert_eq!(args.session_to_resume(), Some("abc"));
    }
    #[test]
    fn cli_worktree_label_with_resume() {
        let args =
            try_parse_pager(&["echo-agent-pager", "-w", "my-label", "--resume", "abc"]).unwrap();
        assert_eq!(args.worktree.as_deref(), Some("my-label"));
        assert_eq!(args.session_to_resume(), Some("abc"));
    }
    #[test]
    fn cli_worktree_default_none() {
        let args = try_parse_pager(&["echo-agent-pager"]).unwrap();
        assert!(args.worktree.is_none());
    }
    #[test]
    fn cli_session_id_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "--session-id", "my-id"]).unwrap();
        assert_eq!(args.session_id.as_deref(), Some("my-id"));
        assert!(matches!(
            args.session_startup_intent().unwrap(),
            crate::app::session_startup::SessionStartupIntent::NewWithId { .. }
        ));
    }
    #[test]
    fn cli_session_id_short_s_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "-s", "my-id"]).unwrap();
        assert_eq!(args.session_id.as_deref(), Some("my-id"));
    }
    #[test]
    fn cli_session_id_with_resume_requires_fork() {
        let args = try_parse_pager(&["echo-agent-pager", "-s", "a", "--resume", "b"]).unwrap();
        assert!(args.session_startup_intent().is_err());
    }
    #[test]
    fn cli_session_id_with_continue_requires_fork() {
        let args = try_parse_pager(&["echo-agent-pager", "-s", "a", "--continue"]).unwrap();
        assert!(args.session_startup_intent().is_err());
    }
    #[test]
    fn cli_session_id_with_resume_and_fork_ok() {
        let args = try_parse_pager(&[
            "echo-agent-pager",
            "-s",
            "a",
            "--resume",
            "b",
            "--fork-session",
        ])
        .unwrap();
        assert!(args.session_startup_intent().is_ok());
    }
    #[test]
    fn cli_session_id_default_none() {
        let args = try_parse_pager(&["echo-agent-pager"]).unwrap();
        assert!(args.session_id.is_none());
    }
    #[test]
    fn cli_no_alt_screen_flag_parses() {
        let args = try_parse_pager(&["echo-agent-pager", "--no-alt-screen"]).unwrap();
        assert!(args.no_alt_screen);
    }
    #[test]
    fn cli_no_alt_screen_default_false() {
        let args = try_parse_pager(&["echo-agent-pager"]).unwrap();
        assert!(!args.no_alt_screen);
    }
    #[test]
    fn cli_command_name_is_echo_agent() {
        use clap::CommandFactory;
        assert_eq!(PagerArgs::command().get_name(), "echoagent");
    }
    #[test]
    fn cli_help_output_header() {
        use clap::CommandFactory;
        let help = PagerArgs::command().render_long_help().to_string();
        let first_5: Vec<&str> = help.lines().take(5).collect();
        assert_eq!(
            first_5,
            vec![
                "EchoAgent Build TUI",
                "",
                "Usage: echoagent [OPTIONS] [PROMPT] [COMMAND]",
                "",
                "Arguments:",
            ]
        );
        assert!(help.find("Arguments:\n").unwrap() < help.find("Options:\n").unwrap());
        assert!(help.find("Options:\n").unwrap() < help.find("Commands:\n").unwrap());
    }
    #[test]
    fn cli_completions_parses() {
        use clap_complete::Shell;
        let args = try_parse_pager(&["echo-agent-pager", "completions", "zsh"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Completions { shell: Shell::Zsh })
        ));
        let args = try_parse_pager(&["echo-agent-pager", "completions", "bash"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Completions { shell: Shell::Bash })
        ));
    }
    /// Always fails writes with EIO (os error 5) — closed-pane stderr.
    struct AlwaysFailWrite;
    impl Write for AlwaysFailWrite {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(5))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(5))
        }
    }
    /// [`ExitInfo`] with no summary, as built for inline/minimal quits.
    fn bare_exit_info(session_id: &str, minimal: bool) -> ExitInfo {
        ExitInfo {
            session_id: session_id.to_string(),
            minimal,
            summary: None,
        }
    }
    #[test]
    fn print_exit_resume_hint_writes_expected_lines() {
        let mut buf = Vec::new();
        print_exit_resume_hint(&bare_exit_info("sess-abc", false), 80, &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "\nResume this session with:\n  echoagent --resume sess-abc\n"
        );
    }
    #[test]
    fn print_exit_resume_hint_includes_minimal_flag() {
        let mut buf = Vec::new();
        print_exit_resume_hint(&bare_exit_info("sess-abc", true), 80, &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "\nResume this session with:\n  echoagent --minimal --resume sess-abc\n"
        );
    }
    #[test]
    fn print_exit_resume_hint_includes_session_summary() {
        let info = ExitInfo {
            session_id: "sess-abc".to_string(),
            minimal: false,
            summary: Some(ExitSummary {
                title: "Fix flaky CI test".to_string(),
                last_prompt: Some("make the suite deterministic".to_string()),
                last_response: Some("Pinned the seed; 200 consecutive green runs.".to_string()),
            }),
        };
        let mut buf = Vec::new();
        print_exit_resume_hint(&info, 80, &mut buf);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            concat!(
                "\n",
                "Fix flaky CI test\n",
                "> make the suite deterministic\n",
                "  Pinned the seed; 200 consecutive green runs.\n",
                "\n",
                "Resume this session with:\n",
                "  echoagent --resume sess-abc\n",
            )
        );
    }
    #[test]
    fn print_exit_resume_hint_truncates_summary_to_width() {
        let info = ExitInfo {
            session_id: "sess-abc".to_string(),
            minimal: false,
            summary: Some(ExitSummary {
                title: "t".repeat(50),
                last_prompt: Some("p".repeat(50)),
                last_response: Some("r".repeat(50)),
            }),
        };
        let mut buf = Vec::new();
        print_exit_resume_hint(&info, 20, &mut buf);
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains(&format!("\n{}…\n", "t".repeat(19))));
        assert!(out.contains(&format!("\n> {}…\n", "p".repeat(17))));
        assert!(out.contains(&format!("\n  {}…\n", "r".repeat(17))));
        assert!(out.contains("  echoagent --resume sess-abc\n"));
    }
    #[test]
    fn print_relaunch_failure_hint_writes_expected_lines() {
        let mut buf = Vec::new();
        print_relaunch_failure_hint(&"exec failed", "sess-xyz", false, &mut buf);
        let hint = screen_mode_relaunch::screen_mode_relaunch_resume_hint("sess-xyz", false);
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            format!(
                "Failed to relaunch in requested mode: exec failed\n\
                 Resume this session with:\n  {hint}\n"
            )
        );
    }
    /// [`ExitInfo`] with a full summary, for the failing-writer tests.
    fn full_exit_info(session_id: &str) -> ExitInfo {
        ExitInfo {
            summary: Some(ExitSummary {
                title: "title".to_string(),
                last_prompt: Some("prompt".to_string()),
                last_response: Some("response".to_string()),
            }),
            ..bare_exit_info(session_id, false)
        }
    }
    #[test]
    fn print_hints_survive_eio() {
        let mut w = AlwaysFailWrite;
        print_exit_resume_hint(&bare_exit_info("sess-abc", false), 80, &mut w);
        print_exit_resume_hint(&bare_exit_info("sess-abc", true), 80, &mut w);
        print_exit_resume_hint(&full_exit_info("sess-abc"), 80, &mut w);
        print_relaunch_failure_hint(&"exec failed", "sess-xyz", true, &mut w);
        print_leader_disabled_by_sandbox("strict", &mut w);
    }
    /// Close the *read* end so writes on the write end get EPIPE
    /// (SIGPIPE is SIG_IGN → BrokenPipe, not process death).
    #[cfg(unix)]
    #[test]
    fn print_hints_survive_closed_pipe() {
        use std::os::unix::io::FromRawFd;
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        unsafe {
            libc::close(fds[0]);
        }
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        print_exit_resume_hint(&bare_exit_info("pipe-sid", false), 80, &mut writer);
        print_exit_resume_hint(&bare_exit_info("pipe-sid", true), 80, &mut writer);
        print_exit_resume_hint(&full_exit_info("pipe-sid"), 80, &mut writer);
        print_relaunch_failure_hint(&"exec failed", "pipe-sid", false, &mut writer);
        print_leader_disabled_by_sandbox("strict", &mut writer);
    }
}
