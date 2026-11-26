//! Models, announcements, settings, campaigns, tips, and modal PTY coverage.
//!
//! All cases are ignored for ordinary Cargo runs; Bazel opts in and caps this
//! process-heavy family at four concurrent libtest workers.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;

#[path = "pty_e2e/agent_type_mismatch_modal_on_model_switch.rs"]
mod agent_type_mismatch_modal_on_model_switch;
#[path = "pty_e2e/agent_type_mismatch_no_keeps_current_session.rs"]
mod agent_type_mismatch_no_keeps_current_session;
#[path = "pty_e2e/agent_type_mismatch_yes_starts_new_session.rs"]
mod agent_type_mismatch_yes_starts_new_session;
#[path = "pty_e2e/campaign_nudges_default_until_dismissed_by_model_pick.rs"]
mod campaign_nudges_default_until_dismissed_by_model_pick;
#[path = "pty_e2e/campaign_remote_settings_nudge_and_dismiss.rs"]
mod campaign_remote_settings_nudge_and_dismiss;
#[path = "pty_e2e/critical_announcement_session_banner_pty.rs"]
mod critical_announcement_session_banner_pty;
#[path = "pty_e2e/dashboard_overlay_tab_esc_backout_and_ctrl_backslash.rs"]
mod dashboard_overlay_tab_esc_backout_and_ctrl_backslash;
#[path = "pty_e2e/extensions_modal_copy_hints_pty.rs"]
mod extensions_modal_copy_hints_pty;
#[path = "pty_e2e/extensions_modal_workflows_tab_pty.rs"]
mod extensions_modal_workflows_tab_pty;
#[path = "pty_e2e/iterm_readline_editing.rs"]
mod iterm_readline_editing;
#[path = "pty_e2e/prompt_suggestion_ghost_tab_accepts.rs"]
mod prompt_suggestion_ghost_tab_accepts;
#[path = "pty_e2e/reasoning_efforts_fallback_menu_matches_builtin.rs"]
mod reasoning_efforts_fallback_menu_matches_builtin;
#[path = "pty_e2e/reasoning_efforts_from_config_toml_menu.rs"]
mod reasoning_efforts_from_config_toml_menu;
#[path = "pty_e2e/reasoning_efforts_menu_renders_and_remaps_on_wire.rs"]
mod reasoning_efforts_menu_renders_and_remaps_on_wire;
#[path = "pty_e2e/reverse_agent_type_mismatch_cursor_to_default.rs"]
mod reverse_agent_type_mismatch_cursor_to_default;
#[path = "pty_e2e/same_agent_type_switch_no_modal.rs"]
mod same_agent_type_switch_no_modal;
#[path = "pty_e2e/show_thinking_blocks_toggle_hides_existing_pty.rs"]
mod show_thinking_blocks_toggle_hides_existing_pty;
#[path = "pty_e2e/subscription_watch_and_gate_verify_pty.rs"]
mod subscription_watch_and_gate_verify_pty;
#[path = "pty_e2e/undo_tip_resets_each_new_session.rs"]
mod undo_tip_resets_each_new_session;
#[path = "pty_e2e/undo_tip_seen_count_never_persisted.rs"]
mod undo_tip_seen_count_never_persisted;
#[path = "pty_e2e/undo_tip_session_cap_blocks_fourth_show.rs"]
mod undo_tip_session_cap_blocks_fourth_show;
#[path = "pty_e2e/verb_group_fold_expand_collapse_pty.rs"]
mod verb_group_fold_expand_collapse_pty;
#[path = "pty_e2e/verb_group_settings_toggle_pty.rs"]
mod verb_group_settings_toggle_pty;
#[path = "pty_e2e/verb_group_streaming_fold_pty.rs"]
mod verb_group_streaming_fold_pty;
#[path = "pty_e2e/verb_group_thinking_fold_pty.rs"]
mod verb_group_thinking_fold_pty;
#[path = "pty_e2e/zero_turn_model_switch_no_modal.rs"]
mod zero_turn_model_switch_no_modal;
