//! In-process `/minimal` ⇄ `/fullscreen` switch: terminal transition plus
//! re-seeding of every mode-derived piece of state startup decides once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};

use super::{PagerTerminal, ScreenMode};
use crate::app::agent_view::AgentView;
use crate::app::app_view::AppView;

// Resolve the regular theme at most once: re-resolving on a later round-trip
// would clobber an in-session /theme choice held in the cache.
static THEME_RESOLVED_FOR_FULL_TUI: AtomicBool = AtomicBool::new(false);

pub(super) fn mark_theme_resolved() {
    THEME_RESOLVED_FOR_FULL_TUI.store(true, Ordering::Release);
}

/// Re-seed every mode-derived piece of app state (pure state, no terminal
/// escapes); keep in lockstep with the startup seeding sites.
pub(crate) fn reseed_screen_mode(app: &mut AppView, mode: ScreenMode) {
    super::apply_screen_mode_globals(mode);

    if !mode.is_minimal() && !THEME_RESOLVED_FOR_FULL_TUI.swap(true, Ordering::AcqRel) {
        let late_theme = crate::theme::cache::resolve_initial_theme_no_osc11();
        crate::theme::cache::set(late_theme);
        tracing::info!(?late_theme, "mode switch: resolved regular theme");
    }

    app.screen_mode = mode;
    app.registry = crate::actions::ActionRegistry::defaults_with_config_for(
        mode,
        super::mouse_reporting_toggle_enabled(),
    );
    app.welcome_prompt.set_screen_mode(mode);
    for agent in app.agents.values_mut() {
        reseed_agent_screen_mode(agent, mode);
    }
}

fn reseed_agent_screen_mode(agent: &mut AgentView, mode: ScreenMode) {
    agent.prompt.set_screen_mode(mode);
    for child in agent.subagent_views.values_mut() {
        reseed_agent_screen_mode(child, mode);
    }
}

/// Result of [`transition_terminal`].
#[derive(Debug)]
pub(crate) enum ModeSwitchOutcome {
    /// Terminal live in the target mode; caller must reseed + full repaint.
    Switched,
    /// Rolled back; terminal still in the previous mode, app state untouched.
    Aborted(String),
    /// Terminal state unknown; caller must fall back to the exec relaunch.
    NeedsExecFallback(String),
}

/// Switch the live terminal between screen modes in place (escapes + viewport
/// only; app state is the caller's `reseed_screen_mode`).
pub(crate) fn transition_terminal(
    terminal: &mut PagerTerminal,
    from: ScreenMode,
    to: ScreenMode,
    minimal_live_rows: u16,
    input_paused: &AtomicBool,
    reader_parked: &AtomicBool,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<super::event_loop::TimedInputEvent>,
) -> ModeSwitchOutcome {
    if !super::event_loop::park_input_reader(
        input_paused,
        reader_parked,
        Duration::from_millis(500),
    ) {
        input_paused.store(false, Ordering::Release);
        return ModeSwitchOutcome::Aborted(
            "terminal input reader did not park before the mode switch".to_owned(),
        );
    }
    let writer_sync = terminal.backend_mut().writer_mut().writer_sync().clone();
    let drained = match writer_sync.wait_drained(Duration::from_millis(750)) {
        Ok(crate::render::draw::WriterDrain::Drained) => Ok(()),
        Ok(crate::render::draw::WriterDrain::TimedOut) => {
            Err("terminal writer did not drain before the mode switch".to_owned())
        }
        Err(error) => Err(format!("terminal writer failed: {error}")),
    };
    if let Err(reason) = drained {
        input_paused.store(false, Ordering::Release);
        return ModeSwitchOutcome::Aborted(reason);
    }

    let outcome = perform_screen_transition(terminal, from, to, minimal_live_rows);

    // Only the pre-park race can reach this channel; later input stays in the tty.
    while input_rx.try_recv().is_ok() {}
    input_paused.store(false, Ordering::Release);
    outcome
}

fn perform_screen_transition(
    terminal: &mut PagerTerminal,
    from: ScreenMode,
    to: ScreenMode,
    minimal_live_rows: u16,
) -> ModeSwitchOutcome {
    match to {
        ScreenMode::Minimal => {
            let mouse_was_captured = super::MOUSE_CAPTURE_ENABLED.swap(false, Ordering::AcqRel);
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                if mouse_was_captured {
                    let _ = execute!(stderr, event::DisableMouseCapture);
                }
                // JediTerm/Windows: DisableMouseCapture is winapi-only, so send
                // the ANSI reset too (same self-heal as init_terminal).
                if crate::terminal::terminal_context().mouse_reporting_leaks_as_raw_text() {
                    use std::io::Write as _;
                    let _ =
                        stderr.write_all(echo_agent_crash_handler::terminal::MOUSE_TRACKING_RESET);
                }
            });
            #[cfg(windows)]
            super::win_native_selection::enable_native_selection();
            if from.is_fullscreen() {
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    let _ = execute!(stderr, LeaveAlternateScreen);
                });
            } else {
                // Clear(All) only — Purge would destroy the user's real scrollback.
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    let _ = execute!(
                        stderr,
                        Clear(ClearType::All),
                        crossterm::cursor::MoveTo(0, 0)
                    );
                });
            }
            // Teardown paths must track the screen the terminal is ACTUALLY on.
            super::set_current_screen_mode(ScreenMode::Minimal);
            let rows = crossterm::terminal::size().map(|(_, r)| r).unwrap_or(24);
            let viewport_rows = minimal_live_rows.clamp(3, rows.saturating_sub(1).max(3));
            // CPR query: the parked reader must be the only stdin consumer.
            match terminal.set_viewport(ratatui::Viewport::Inline(viewport_rows)) {
                Ok(()) => ModeSwitchOutcome::Switched,
                Err(error) => {
                    echo_agent_runtime::util::with_locked_stderr(|stderr| {
                        if from.is_fullscreen() {
                            let _ = execute!(stderr, EnterAlternateScreen);
                        }
                        // Restore the observed state; a runtime toggle may have had it off.
                        if mouse_was_captured {
                            let _ = execute!(stderr, event::EnableMouseCapture);
                        }
                    });
                    super::MOUSE_CAPTURE_ENABLED.store(mouse_was_captured, Ordering::Release);
                    super::set_current_screen_mode(from);
                    let rollback_viewport = if from.is_fullscreen() {
                        ratatui::Viewport::Fullscreen
                    } else {
                        ratatui::Viewport::Inline(rows)
                    };
                    match terminal.set_viewport(rollback_viewport) {
                        Ok(()) => ModeSwitchOutcome::Aborted(format!(
                            "inline viewport probe failed: {error}"
                        )),
                        Err(rollback_error) => ModeSwitchOutcome::NeedsExecFallback(format!(
                            "inline viewport probe failed ({error}); rollback also failed \
                             ({rollback_error})"
                        )),
                    }
                }
            }
        }
        ScreenMode::Fullscreen => {
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                let _ = execute!(stderr, EnterAlternateScreen);
                let _ = execute!(stderr, event::EnableMouseCapture);
            });
            super::MOUSE_CAPTURE_ENABLED.store(true, Ordering::Release);
            // Set before the fallible viewport swap: the exec-fallback teardown
            // must leave the alt screen that is already live.
            super::set_current_screen_mode(ScreenMode::Fullscreen);
            match terminal.set_viewport(ratatui::Viewport::Fullscreen) {
                Ok(()) => ModeSwitchOutcome::Switched,
                Err(error) => ModeSwitchOutcome::NeedsExecFallback(format!(
                    "fullscreen viewport rebuild failed: {error}"
                )),
            }
        }
        ScreenMode::Inline => {
            ModeSwitchOutcome::Aborted("inline is not a switch target".to_owned())
        }
    }
}

/// Append `block` without landing after a live agent stream: a non-thinking
/// sibling behind a streaming message trips minimal's print-once commit
/// (`commit.rs::agent_message_stream_closed`), freezing the partial reply.
pub(crate) fn push_block_behind_live_stream(
    sb: &mut crate::scrollback::state::ScrollbackState,
    block: crate::scrollback::block::RenderBlock,
) {
    let anchor = (0..sb.len()).find_map(|i| {
        sb.entry(i)
            .and_then(|e| (e.is_running && !sb.is_committed(e.id)).then_some(e.id))
    });
    match anchor {
        Some(id) => {
            sb.insert_block_before(id, block);
        }
        None => {
            sb.push_block(block);
        }
    }
}

/// Close user-opened surfaces minimal never paints (they would become
/// invisible input owners); agent-initiated prompts are left alone.
pub(crate) fn dismiss_fullscreen_only_surfaces(app: &mut AppView) {
    for agent in app.agents.values_mut() {
        dismiss_agent_surfaces(agent);
    }
}

fn dismiss_agent_surfaces(agent: &mut AgentView) {
    if agent.gboom.take().is_some() {
        // Pop the game's kitty layer or later keys carry unexpected release events.
        super::pop_gboom_keyboard_flags();
    }
    agent.image_viewer = None;
    agent.video_viewer = None;
    agent.line_viewer = None;
    agent.block_viewer = None;
    agent.persona_detail = None;
    agent.agents_modal = None;
    agent.show_goal_detail = false;
    for child in agent.subagent_views.values_mut() {
        dismiss_agent_surfaces(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reseed_round_trip_flips_every_mode_derived_gate() {
        // Serializes process-global mutation with other theme-touching tests.
        let _guard = crate::theme::cache::pin_theme();
        // Pretend startup resolved a theme so reseed never reads the host config.
        mark_theme_resolved();

        let mut app = crate::app::app_view::tests::test_app_with_agent();
        let agent_id = *app.agents.keys().next().expect("agent present");
        let child =
            crate::app::agent_view::test_agent_view(Some("sess-1"), std::path::PathBuf::from("."));
        app.agents
            .get_mut(&agent_id)
            .expect("agent present")
            .subagent_views
            .insert("sub-1".to_string(), Box::new(child));

        for &(mode, minimal) in &[
            (ScreenMode::Minimal, true),
            (ScreenMode::Fullscreen, false),
            (ScreenMode::Minimal, true),
        ] {
            reseed_screen_mode(&mut app, mode);

            assert_eq!(app.screen_mode, mode);
            assert_eq!(crate::app::minimal_mode_active(), minimal);
            assert_eq!(crate::app::current_screen_mode(), mode);
            assert_eq!(crate::views::modal_window::embedded(), minimal);
            assert_eq!(crate::render::scrollbar::scrollbars_hidden(), minimal);
            assert_eq!(crate::theme::cache::terminal_native_locked(), minimal);
            assert_eq!(
                crate::terminal::image::scrollback_inline_overlay_forced_off(),
                minimal
            );
            assert_eq!(
                app.welcome_prompt.slash_controller.screen_mode(),
                mode,
                "welcome prompt slash gate"
            );
            let agent = &app.agents[&agent_id];
            assert_eq!(agent.is_minimal_mode(), minimal, "agent gate");
            let child = agent
                .subagent_views
                .values()
                .next()
                .expect("subagent present");
            assert_eq!(child.is_minimal_mode(), minimal, "subagent gate");
        }

        // Leave the fullscreen baseline other tests expect.
        reseed_screen_mode(&mut app, ScreenMode::Fullscreen);
    }

    #[test]
    fn push_behind_live_stream_anchors_before_first_uncommitted_running_entry() {
        use crate::scrollback::block::RenderBlock;
        use crate::scrollback::entry::ScrollbackEntry;
        use crate::scrollback::state::ScrollbackState;

        let mut sb = ScrollbackState::new();
        sb.push_block(RenderBlock::system("done"));
        // Committed running entry (the BgTask shape): must be skipped as anchor.
        sb.push(ScrollbackEntry::running(RenderBlock::system("bg task")));
        sb.mark_committed(0);
        sb.mark_committed(1);
        let stream = sb.push(ScrollbackEntry::running(RenderBlock::system("streaming")));

        push_block_behind_live_stream(&mut sb, RenderBlock::system("marker"));
        assert_eq!(
            sb.index_of_id(stream),
            Some(3),
            "marker must land before the live stream, not after it"
        );

        // No uncommitted running entry left: plain append.
        let mut idle = ScrollbackState::new();
        let a = idle.push_block(RenderBlock::system("a"));
        push_block_behind_live_stream(&mut idle, RenderBlock::system("marker"));
        assert_eq!(idle.index_of_id(a), Some(0));
        assert_eq!(idle.len(), 2);
    }
}
