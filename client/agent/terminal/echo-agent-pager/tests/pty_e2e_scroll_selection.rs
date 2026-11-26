//! Scroll, mouse, drag-selection, folding, and viewport PTY coverage.
//!
//! All cases are ignored for ordinary Cargo runs; Bazel opts in and caps this
//! process-heavy family at four concurrent libtest workers.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;
#[path = "pty_e2e/scroll.rs"]
mod scroll;
#[path = "pty_e2e/scroll_anchor_holds_parked_marker_during_live_stream.rs"]
mod scroll_anchor_holds_parked_marker_during_live_stream;

#[path = "pty_e2e/agent_execute_full_output_double_click_fold_pty.rs"]
mod agent_execute_full_output_double_click_fold_pty;
#[path = "pty_e2e/bash_full_output_double_click_fold_pty.rs"]
mod bash_full_output_double_click_fold_pty;
#[path = "pty_e2e/drag_autoscroll_no_bounce_pty.rs"]
mod drag_autoscroll_no_bounce_pty;
#[path = "pty_e2e/drag_enters_content_from_gap_pty.rs"]
mod drag_enters_content_from_gap_pty;
#[path = "pty_e2e/drag_from_above_prompt_strip_pty.rs"]
mod drag_from_above_prompt_strip_pty;
#[path = "pty_e2e/drag_from_chrome_stays_block_pty.rs"]
mod drag_from_chrome_stays_block_pty;
#[path = "pty_e2e/drag_over_gap_rows_does_not_freeze_head_pty.rs"]
mod drag_over_gap_rows_does_not_freeze_head_pty;
#[path = "pty_e2e/drag_select_autoscroll_full_scrollout_copy_pty.rs"]
mod drag_select_autoscroll_full_scrollout_copy_pty;
#[path = "pty_e2e/drag_select_wheel_scroll_extends_pty.rs"]
mod drag_select_wheel_scroll_extends_pty;
#[path = "pty_e2e/forced_wheel_mode_env_scrolls_exact_rows.rs"]
mod forced_wheel_mode_env_scrolls_exact_rows;
#[path = "pty_e2e/keep_text_selection_settings_visible_pty.rs"]
mod keep_text_selection_settings_visible_pty;
#[path = "pty_e2e/misclassified_wheel_flood_does_not_teleport_viewport.rs"]
mod misclassified_wheel_flood_does_not_teleport_viewport;
#[path = "pty_e2e/mouse_reporting_toggle_inactive_without_config_pty.rs"]
mod mouse_reporting_toggle_inactive_without_config_pty;
#[path = "pty_e2e/mouse_reporting_toggle_sticky_persists_pty.rs"]
mod mouse_reporting_toggle_sticky_persists_pty;
#[path = "pty_e2e/nested_quote_drag_copy_excludes_bars_pty.rs"]
mod nested_quote_drag_copy_excludes_bars_pty;
#[path = "pty_e2e/page_flip_on_send_pty.rs"]
mod page_flip_on_send_pty;
#[path = "pty_e2e/plan_scrollbar_grab_zone_pty.rs"]
mod plan_scrollbar_grab_zone_pty;
#[path = "pty_e2e/quote_block_drag_copy_excludes_bars_pty.rs"]
mod quote_block_drag_copy_excludes_bars_pty;
#[path = "pty_e2e/quote_block_raw_mode_copy_keeps_source_pty.rs"]
mod quote_block_raw_mode_copy_keeps_source_pty;
#[path = "pty_e2e/read_tool_header_selection_copies_path_only_pty.rs"]
mod read_tool_header_selection_copies_path_only_pty;
#[path = "pty_e2e/recap_header_not_in_selection_pty.rs"]
mod recap_header_not_in_selection_pty;
#[path = "pty_e2e/resize_preserves_scroll_position.rs"]
mod resize_preserves_scroll_position;
#[path = "pty_e2e/response_top_indicator_pty.rs"]
mod response_top_indicator_pty;
#[path = "pty_e2e/rtl_bidi_drag_copy_logical_pty.rs"]
mod rtl_bidi_drag_copy_logical_pty;
#[path = "pty_e2e/scroll_debug_hud_env_toggles_overlay.rs"]
mod scroll_debug_hud_env_toggles_overlay;
#[path = "pty_e2e/scroll_does_not_crash.rs"]
mod scroll_does_not_crash;
#[path = "pty_e2e/sticky_header_drag_copy_pty.rs"]
mod sticky_header_drag_copy_pty;
#[path = "pty_e2e/stuck_drag_finishes_on_bare_motion_pty.rs"]
mod stuck_drag_finishes_on_bare_motion_pty;
#[path = "pty_e2e/stuck_drag_recovers_on_esc_pty.rs"]
mod stuck_drag_recovers_on_esc_pty;
#[path = "pty_e2e/trackpad_flood_does_not_under_travel.rs"]
mod trackpad_flood_does_not_under_travel;
#[path = "pty_e2e/verb_group_header_drag_copy_pty.rs"]
mod verb_group_header_drag_copy_pty;
#[path = "pty_e2e/wheel_burst_scrolls_viewport_without_frame_amplification.rs"]
mod wheel_burst_scrolls_viewport_without_frame_amplification;
#[path = "pty_e2e/wheel_flood_paints_no_ghost_frames.rs"]
mod wheel_flood_paints_no_ghost_frames;
#[path = "pty_e2e/wheel_overscroll_at_bottom_reengages_follow_mid_stream.rs"]
mod wheel_overscroll_at_bottom_reengages_follow_mid_stream;
#[path = "pty_e2e/wheel_scrolls_viewport_during_streaming_turn.rs"]
mod wheel_scrolls_viewport_during_streaming_turn;
#[path = "pty_e2e/word_select_tip_on_double_click_pty.rs"]
mod word_select_tip_on_double_click_pty;
