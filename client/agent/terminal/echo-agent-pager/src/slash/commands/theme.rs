//! `/theme` (alias `/t`) -- switch the color theme.
//!
//! Toggles between available themes or switches to a named theme.
//! Selecting `auto` enables system-appearance-driven theme switching.
//! Selecting an explicit theme disengages auto mode.
//!
//! `run` dispatches `Action::SetTheme(<canonical>)` — the dispatcher
//! handles mutation + persistence + toast. `preview_arg` /
//! `cancel_preview` call `Theme::apply_kind` directly for non-persisting
//! visual previews (no toast/disk writes per keystroke).

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};
use crate::slash::{ModeSupport, Remedy};
use crate::theme::{Theme, ThemeKind, cache as theme_cache};

/// Switch the pager color theme.
pub struct ThemeCommand;

impl SlashCommand for ThemeCommand {
    fn name(&self) -> &str {
        "theme"
    }

    fn aliases(&self) -> &[&str] {
        &["t"]
    }

    fn description(&self) -> &str {
        "Switch the color theme"
    }

    fn mode_support(&self) -> ModeSupport {
        ModeSupport::FullscreenOnly(Remedy::SwitchMode {
            why: "minimal renders with your terminal's own palette",
        })
    }

    fn usage(&self) -> &str {
        "/theme <name>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<theme>")
    }

    fn supports_preview(&self) -> bool {
        true
    }

    fn preview_state(&self) -> Option<String> {
        Some(Theme::current_kind().display_name().to_string())
    }

    fn preview_arg(&self, arg: &str) {
        if let Some(kind) = ThemeKind::from_name(arg) {
            if kind.is_auto() {
                // Preview the theme that auto mode would resolve to.
                let resolved = theme_cache::resolve_auto();
                Theme::apply_kind(resolved);
            } else {
                Theme::apply_kind(kind);
            }
        }
    }

    fn cancel_preview(&self, previous: &str) {
        if let Some(kind) = ThemeKind::from_name(previous) {
            Theme::apply_kind(kind);
        }
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        let current = Theme::current_kind();
        let is_auto = theme_cache::is_auto_mode();
        let available = ThemeKind::available();

        // Prepend "auto" (follow system appearance) as the first option.
        let auto_active = if is_auto { " (active)" } else { "" };
        let mut items = vec![ArgItem {
            display: "auto".to_string(),
            match_text: "auto".to_string(),
            insert_text: "auto".to_string(),
            description: format!("auto (follow system){auto_active}"),
        }];

        // Concrete themes — only show "(active)" when not in auto mode.
        items.extend(available.iter().map(|kind| {
            let active = if *kind == current && !is_auto {
                " (active)"
            } else {
                ""
            };
            ArgItem {
                display: kind.display_name().to_string(),
                match_text: kind.display_name().to_string(),
                insert_text: kind.display_name().to_string(),
                description: format!("{}{active}", kind.display_name()),
            }
        }));

        Some(items)
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        let available = ThemeKind::available();

        // No args: toggle between available themes.
        if trimmed.is_empty() {
            let current = Theme::current_kind();
            let current_idx = available.iter().position(|k| *k == current).unwrap_or(0);
            let next = available[(current_idx + 1) % available.len()];

            return CommandResult::Action(Action::SetTheme(next.display_name().to_string()));
        }

        // Named theme (including "auto"): parse and dispatch.
        // Truecolor-only themes are accepted regardless of terminal —
        // `Theme::apply_kind` clamps the live visual as needed.
        match ThemeKind::from_name(trimmed) {
            Some(kind) => {
                // Normalise alias to canonical display_name.
                CommandResult::Action(Action::SetTheme(kind.display_name().to_string()))
            }
            None => {
                let all_names: Vec<&str> =
                    ThemeKind::ALL.iter().map(|k| k.display_name()).collect();
                CommandResult::Error(format!(
                    "Unknown theme: {}. Available: auto, {}",
                    trimmed,
                    all_names.join(", ")
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{cache as theme_cache, system_appearance};

    /// Run a test with a clean in-memory state. Prevents disk reads by
    /// pre-loading the theme state.
    fn with_test_env(f: impl FnOnce()) {
        let _guard = theme_cache::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        theme_cache::reset_for_test();
        theme_cache::seed_auto_theme_defaults_for_test();
        system_appearance::clear_mock();
        // Set LOADED=true so current_kind() doesn't try to read from disk.
        theme_cache::set(ThemeKind::EchoAgentNight);
        f();
        system_appearance::clear_mock();
        theme_cache::reset_for_test();
    }

    // -- suggest_args ---------------------------------------------------------

    #[test]
    fn suggest_args_prepends_auto_option() {
        with_test_env(|| {
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let ctx = AppCtx {
                models: &models,
                cwd: std::path::Path::new("."),
                has_session_announcements: false,
                billing_surface_visible: true,
                usage_command_visible: true,
                workflows_available: true,
                saved_workflows: &[],
                workflow_runs: &[],
                screen_mode: crate::app::ScreenMode::Fullscreen,
                current_title: None,
            };
            let items = cmd.suggest_args(&ctx, "").expect("should return items");
            assert_eq!(items[0].insert_text, "auto");
            assert!(items[0].description.contains("follow system"));
            // auto + all available concrete themes
            assert_eq!(items.len(), ThemeKind::available().len() + 1);
        });
    }

    #[test]
    fn suggest_args_auto_active_when_auto_mode() {
        with_test_env(|| {
            theme_cache::set_auto_mode(true);
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let ctx = AppCtx {
                models: &models,
                cwd: std::path::Path::new("."),
                has_session_announcements: false,
                billing_surface_visible: true,
                usage_command_visible: true,
                workflows_available: true,
                saved_workflows: &[],
                workflow_runs: &[],
                screen_mode: crate::app::ScreenMode::Fullscreen,
                current_title: None,
            };
            let items = cmd.suggest_args(&ctx, "").expect("should return items");
            assert!(
                items[0].description.contains("(active)"),
                "auto should show (active), got: {}",
                items[0].description
            );
        });
    }

    #[test]
    fn suggest_args_auto_not_active_when_explicit() {
        with_test_env(|| {
            theme_cache::set_auto_mode(false);
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let ctx = AppCtx {
                models: &models,
                cwd: std::path::Path::new("."),
                has_session_announcements: false,
                billing_surface_visible: true,
                usage_command_visible: true,
                workflows_available: true,
                saved_workflows: &[],
                workflow_runs: &[],
                screen_mode: crate::app::ScreenMode::Fullscreen,
                current_title: None,
            };
            let items = cmd.suggest_args(&ctx, "").expect("should return items");
            assert!(
                !items[0].description.contains("(active)"),
                "auto should not show (active), got: {}",
                items[0].description
            );
        });
    }

    #[test]
    fn suggest_args_explicit_active_when_not_auto() {
        with_test_env(|| {
            theme_cache::set_auto_mode(false);
            theme_cache::set(ThemeKind::EchoAgentNight);
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let ctx = AppCtx {
                models: &models,
                cwd: std::path::Path::new("."),
                has_session_announcements: false,
                billing_surface_visible: true,
                usage_command_visible: true,
                workflows_available: true,
                saved_workflows: &[],
                workflow_runs: &[],
                screen_mode: crate::app::ScreenMode::Fullscreen,
                current_title: None,
            };
            let items = cmd.suggest_args(&ctx, "").expect("should return items");
            let echo_agent_night = items
                .iter()
                .find(|i| i.insert_text == "echo-agent-night")
                .expect("echo-agent-night should be in list");
            assert!(
                echo_agent_night.description.contains("(active)"),
                "explicit theme should show (active), got: {}",
                echo_agent_night.description
            );
        });
    }

    #[test]
    fn suggest_args_no_explicit_active_when_auto() {
        with_test_env(|| {
            theme_cache::set_auto_mode(true);
            theme_cache::set(ThemeKind::EchoAgentNight);
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let ctx = AppCtx {
                models: &models,
                cwd: std::path::Path::new("."),
                has_session_announcements: false,
                billing_surface_visible: true,
                usage_command_visible: true,
                workflows_available: true,
                saved_workflows: &[],
                workflow_runs: &[],
                screen_mode: crate::app::ScreenMode::Fullscreen,
                current_title: None,
            };
            let items = cmd.suggest_args(&ctx, "").expect("should return items");
            // No concrete theme should show "(active)" in auto mode.
            for item in items.iter().skip(1) {
                assert!(
                    !item.description.contains("(active)"),
                    "{} should not show (active) in auto mode",
                    item.insert_text
                );
            }
        });
    }

    // -- run (dispatches Action::SetTheme) ------------------------------------

    /// `/theme <name>` returns `Action::SetTheme(<canonical>)` —
    /// the dispatcher handles in-memory state + disk write + toast.
    #[test]
    fn run_explicit_dispatches_set_theme_action() {
        with_test_env(|| {
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let bundle = crate::app::bundle::BundleState::default();
            let mut ctx = CommandExecCtx {
                models: &models,
                session_id: None,
                bundle_state: &bundle,
                screen_mode: crate::app::ScreenMode::Inline,
                billing_surface_visible: true,
                usage_command_visible: true,
                pager_state: crate::settings::PagerLocalSnapshot {
                    multiline_mode: false,
                    yolo_mode: false,
                    ..crate::settings::PagerLocalSnapshot::default()
                },
            };
            let result = cmd.run(&mut ctx, "echo-agent-night");
            match result {
                CommandResult::Action(Action::SetTheme(name)) => {
                    assert_eq!(name, "echo-agent-night");
                }
                other => panic!("expected Action::SetTheme(\"echo-agent-night\"), got {other:?}"),
            }
        });
    }

    /// `/theme` (no args) toggles by dispatching `Action::SetTheme(<next>)`.
    /// Precondition-assert that `ThemeKind::available()` has ≥2 entries;
    /// otherwise the previous `unwrap_or` masked a broken upstream
    /// invariant.
    #[test]
    fn run_toggle_dispatches_set_theme_action() {
        with_test_env(|| {
            theme_cache::set(ThemeKind::EchoAgentNight);
            // Hard-fail with a clear message if the precondition
            // breaks — `(0 + 1) % 0` in `run` would otherwise panic
            // with `attempt to calculate the remainder with a
            // divisor of zero`, which is a worse error message.
            assert!(
                ThemeKind::available().len() >= 2,
                "toggle test requires ≥2 available themes, got {}",
                ThemeKind::available().len(),
            );
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let bundle = crate::app::bundle::BundleState::default();
            let mut ctx = CommandExecCtx {
                models: &models,
                session_id: None,
                bundle_state: &bundle,
                screen_mode: crate::app::ScreenMode::Inline,
                billing_surface_visible: true,
                usage_command_visible: true,
                pager_state: crate::settings::PagerLocalSnapshot {
                    multiline_mode: false,
                    yolo_mode: false,
                    ..crate::settings::PagerLocalSnapshot::default()
                },
            };
            let result = cmd.run(&mut ctx, "");
            match result {
                CommandResult::Action(Action::SetTheme(name)) => {
                    // available[0] = EchoAgentNight; next is available[1].
                    let expected = ThemeKind::available()[1].display_name();
                    assert_eq!(name, expected);
                }
                other => panic!("expected Action::SetTheme(...), got {other:?}"),
            }
        });
    }

    /// `/theme auto` dispatches `SetTheme("auto")`.
    #[test]
    fn run_auto_dispatches_set_theme_auto() {
        with_test_env(|| {
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let bundle = crate::app::bundle::BundleState::default();
            let mut ctx = CommandExecCtx {
                models: &models,
                session_id: None,
                bundle_state: &bundle,
                screen_mode: crate::app::ScreenMode::Inline,
                billing_surface_visible: true,
                usage_command_visible: true,
                pager_state: crate::settings::PagerLocalSnapshot {
                    multiline_mode: false,
                    yolo_mode: false,
                    ..crate::settings::PagerLocalSnapshot::default()
                },
            };
            let result = cmd.run(&mut ctx, "auto");
            match result {
                CommandResult::Action(Action::SetTheme(name)) => {
                    assert_eq!(name, "auto");
                }
                other => panic!("expected Action::SetTheme(\"auto\"), got {other:?}"),
            }
        });
    }

    /// Aliases normalise to canonical `display_name` before dispatch.
    #[test]
    fn run_alias_normalises_to_canonical() {
        with_test_env(|| {
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let bundle = crate::app::bundle::BundleState::default();
            let mut ctx = CommandExecCtx {
                models: &models,
                session_id: None,
                bundle_state: &bundle,
                screen_mode: crate::app::ScreenMode::Inline,
                billing_surface_visible: true,
                usage_command_visible: true,
                pager_state: crate::settings::PagerLocalSnapshot {
                    multiline_mode: false,
                    yolo_mode: false,
                    ..crate::settings::PagerLocalSnapshot::default()
                },
            };
            // "dark" is an alias for EchoAgentNight.
            let result = cmd.run(&mut ctx, "dark");
            match result {
                CommandResult::Action(Action::SetTheme(name)) => {
                    assert_eq!(
                        name, "echo-agent-night",
                        "alias must normalise to canonical"
                    );
                }
                other => panic!("expected Action::SetTheme(\"echo-agent-night\"), got {other:?}"),
            }
        });
    }

    // -- preview_arg ----------------------------------------------------------

    #[test]
    fn preview_auto_applies_resolved_theme() {
        with_test_env(|| {
            system_appearance::set_mock(Some(system_appearance::SystemAppearance::Light));
            let cmd = ThemeCommand;
            cmd.preview_arg("auto");
            // Default auto config maps Light -> EchoAgentDay.
            assert_eq!(Theme::current_kind(), ThemeKind::EchoAgentDay);
        });
    }

    /// `preview_arg` applies the named theme directly.
    #[test]
    fn preview_explicit_theme_applies_directly() {
        with_test_env(|| {
            theme_cache::set(ThemeKind::EchoAgentNight);
            let cmd = ThemeCommand;
            cmd.preview_arg("echo-agent-day");
            assert_eq!(Theme::current_kind(), ThemeKind::EchoAgentDay);
        });
    }

    /// `preview_arg` with unknown theme is a no-op.
    #[test]
    fn preview_unknown_theme_is_no_op() {
        with_test_env(|| {
            theme_cache::set(ThemeKind::EchoAgentNight);
            let cmd = ThemeCommand;
            cmd.preview_arg("nonexistent-theme");
            assert_eq!(
                Theme::current_kind(),
                ThemeKind::EchoAgentNight,
                "unknown theme name must NOT change Theme::current_kind",
            );
        });
    }

    // -- cancel_preview -------------------------------------------------------

    /// `cancel_preview` restores the previously-applied theme.
    #[test]
    fn cancel_preview_restores_previous_kind() {
        with_test_env(|| {
            theme_cache::set(ThemeKind::EchoAgentNight);
            let cmd = ThemeCommand;
            // Simulate user navigating into a different theme during preview.
            cmd.preview_arg("echo-agent-day");
            assert_eq!(Theme::current_kind(), ThemeKind::EchoAgentDay);

            // Then Escape (or arg picker dismissal): restore.
            cmd.cancel_preview("echo-agent-night");
            assert_eq!(
                Theme::current_kind(),
                ThemeKind::EchoAgentNight,
                "cancel_preview must restore the previous canonical",
            );
        });
    }

    /// `cancel_preview` with unknown theme is a no-op.
    #[test]
    fn cancel_preview_unknown_theme_is_no_op() {
        with_test_env(|| {
            theme_cache::set(ThemeKind::EchoAgentDay);
            let cmd = ThemeCommand;
            cmd.cancel_preview("nonexistent-theme");
            assert_eq!(
                Theme::current_kind(),
                ThemeKind::EchoAgentDay,
                "unknown previous must NOT change Theme::current_kind",
            );
        });
    }

    // -- error handling -------------------------------------------------------

    #[test]
    fn run_unknown_lists_auto_in_available() {
        with_test_env(|| {
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let bundle = crate::app::bundle::BundleState::default();
            let mut ctx = CommandExecCtx {
                models: &models,
                session_id: None,
                bundle_state: &bundle,
                screen_mode: crate::app::ScreenMode::Inline,
                billing_surface_visible: true,
                usage_command_visible: true,
                pager_state: crate::settings::PagerLocalSnapshot {
                    multiline_mode: false,
                    yolo_mode: false,
                    ..crate::settings::PagerLocalSnapshot::default()
                },
            };
            let result = cmd.run(&mut ctx, "nonexistent");
            if let CommandResult::Error(msg) = result {
                assert!(msg.contains("auto"), "error should list auto: {msg}");
            } else {
                panic!("expected Error, got: {result:?}");
            }
        });
    }

    /// Truecolor-only themes are accepted; clamping happens downstream.
    #[test]
    fn run_truecolor_theme_dispatches_set_theme_action() {
        with_test_env(|| {
            let cmd = ThemeCommand;
            let models = crate::acp::model_state::ModelState::default();
            let bundle = crate::app::bundle::BundleState::default();
            let mut ctx = CommandExecCtx {
                models: &models,
                session_id: None,
                bundle_state: &bundle,
                screen_mode: crate::app::ScreenMode::Inline,
                billing_surface_visible: true,
                usage_command_visible: true,
                pager_state: crate::settings::PagerLocalSnapshot {
                    multiline_mode: false,
                    yolo_mode: false,
                    ..crate::settings::PagerLocalSnapshot::default()
                },
            };
            let result = cmd.run(&mut ctx, "tokyonight");
            match result {
                CommandResult::Action(Action::SetTheme(name)) => {
                    assert_eq!(
                        name, "tokyonight",
                        "truecolor themes must be accepted; clamping happens \
                         downstream in `Theme::apply_kind`",
                    );
                }
                other => panic!("expected Action::SetTheme(\"tokyonight\"), got {other:?}"),
            }
        });
    }
}
