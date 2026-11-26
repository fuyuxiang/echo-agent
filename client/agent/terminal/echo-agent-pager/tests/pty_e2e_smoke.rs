//! Basic PTY coverage: startup, input, rendering, permission-mode keys, and
//! `echoagent wrap` command routing.
//!
//! All cases are ignored for ordinary Cargo runs; Bazel opts in and caps this
//! process-heavy family at four concurrent libtest workers.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;

#[path = "pty_e2e/agent_response.rs"]
mod agent_response;
#[path = "pty_e2e/auto_compact_top_row.rs"]
mod auto_compact_top_row;
#[path = "pty_e2e/basename_path_demo_pty.rs"]
mod basename_path_demo_pty;
#[path = "pty_e2e/connect_ui_timeout_env_override.rs"]
mod connect_ui_timeout_env_override;
#[path = "pty_e2e/doubled_lines_out_of_band_repro.rs"]
mod doubled_lines_out_of_band_repro;
#[path = "pty_e2e/embedded_mode_boots_without_hanging_on_blocked_backend.rs"]
mod embedded_mode_boots_without_hanging_on_blocked_backend;
#[path = "pty_e2e/feedback_slash_opens_descriptive_pane.rs"]
mod feedback_slash_opens_descriptive_pane;
#[path = "pty_e2e/fullscreen_external_editor_round_trip.rs"]
mod fullscreen_external_editor_round_trip;
#[path = "pty_e2e/initial_prompt_positional_auto_submits.rs"]
mod initial_prompt_positional_auto_submits;
#[path = "pty_e2e/input_echoes_at_idle_prompt.rs"]
mod input_echoes_at_idle_prompt;
#[path = "pty_e2e/plan_revise_empty_enter_does_not_approve.rs"]
mod plan_revise_empty_enter_does_not_approve;
#[path = "pty_e2e/question_tab_cycles_answers.rs"]
mod question_tab_cycles_answers;
#[path = "pty_e2e/renders_on_action.rs"]
mod renders_on_action;
#[path = "pty_e2e/requirements_version_failure_exits_2_with_guidance.rs"]
mod requirements_version_failure_exits_2_with_guidance;
#[path = "pty_e2e/shift_selection_key_encodings.rs"]
mod shift_selection_key_encodings;
#[path = "pty_e2e/shift_tab_in_session_cycles_mode.rs"]
mod shift_tab_in_session_cycles_mode;
#[path = "pty_e2e/shift_tab_on_welcome_starts_session_in_plan_mode.rs"]
mod shift_tab_on_welcome_starts_session_in_plan_mode;
#[path = "pty_e2e/small_screen_tip_survives_slow_turn.rs"]
mod small_screen_tip_survives_slow_turn;
#[path = "pty_e2e/tab_focuses_scrollback_in_vim_and_default_modes.rs"]
mod tab_focuses_scrollback_in_vim_and_default_modes;
#[path = "pty_e2e/waiting_for_model_label.rs"]
mod waiting_for_model_label;
#[path = "pty_e2e/welcome_screen.rs"]
mod welcome_screen;
#[path = "pty_e2e/welcome_screen_braille_logo_renders_correctly.rs"]
mod welcome_screen_braille_logo_renders_correctly;
#[path = "pty_e2e/wrap_appearance_env_advertised_through_shell.rs"]
mod wrap_appearance_env_advertised_through_shell;
#[path = "pty_e2e/wrap_child_killed_with_latched_modes_restores_terminal.rs"]
mod wrap_child_killed_with_latched_modes_restores_terminal;
#[path = "pty_e2e/wrap_clean_exit_stays_byte_transparent.rs"]
mod wrap_clean_exit_stays_byte_transparent;
#[path = "pty_e2e/wrap_echo_passthrough_and_exit_code.rs"]
mod wrap_echo_passthrough_and_exit_code;
#[path = "pty_e2e/wrap_explicit_path_not_found_fails_fast.rs"]
mod wrap_explicit_path_not_found_fails_fast;
#[path = "pty_e2e/wrap_not_found_alias_routes_via_shell_contract.rs"]
mod wrap_not_found_alias_routes_via_shell_contract;
#[path = "pty_e2e/wrap_osc52_sink_env_advertised_through_shell.rs"]
mod wrap_osc52_sink_env_advertised_through_shell;
#[path = "pty_e2e/wrap_sigterm_restores_terminal_and_exit_code.rs"]
mod wrap_sigterm_restores_terminal_and_exit_code;
#[path = "pty_e2e/wrap_single_string_routes_via_shell.rs"]
mod wrap_single_string_routes_via_shell;
