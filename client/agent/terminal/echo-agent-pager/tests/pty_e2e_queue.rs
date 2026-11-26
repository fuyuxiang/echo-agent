//! Queue, cancellation, send-now, interjection, and rewind PTY coverage.
//!
//! This is the narrow retry unit for the suite's historically timing-sensitive
//! cases. All cases are ignored for ordinary Cargo runs; Bazel opts in and caps
//! the family at four concurrent libtest workers.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;

#[path = "pty_e2e/auto_wake_cancel_preserves_queued_user_prompt.rs"]
mod auto_wake_cancel_preserves_queued_user_prompt;
#[path = "pty_e2e/auto_wake_cancel_via_esc_preserves_queued_user_prompt.rs"]
mod auto_wake_cancel_via_esc_preserves_queued_user_prompt;
#[path = "pty_e2e/auto_wake_cancel_via_stop_click_preserves_queued_user_prompt.rs"]
mod auto_wake_cancel_via_stop_click_preserves_queued_user_prompt;
#[path = "pty_e2e/bash_queued_mid_turn_drains_as_bash.rs"]
mod bash_queued_mid_turn_drains_as_bash;
#[path = "pty_e2e/cancel_discards_buffered_interjection.rs"]
mod cancel_discards_buffered_interjection;
#[path = "pty_e2e/cancel_then_resend_prompt_appears_once.rs"]
mod cancel_then_resend_prompt_appears_once;
#[path = "pty_e2e/ctrl_c_cancel_during_stream_recovers_cleanly.rs"]
mod ctrl_c_cancel_during_stream_recovers_cleanly;
#[path = "pty_e2e/ctrlc_after_activity_no_rewind_prompt_once.rs"]
mod ctrlc_after_activity_no_rewind_prompt_once;
#[path = "pty_e2e/ctrlc_with_queued_prompt_no_dup.rs"]
mod ctrlc_with_queued_prompt_no_dup;
#[path = "pty_e2e/edit_interject_lone_queued_row_keeps_tui_alive.rs"]
mod edit_interject_lone_queued_row_keeps_tui_alive;
#[path = "pty_e2e/empty_enter_force_sends_top_queued.rs"]
mod empty_enter_force_sends_top_queued;
#[path = "pty_e2e/empty_enter_sends_top_not_last_of_two.rs"]
mod empty_enter_sends_top_not_last_of_two;
#[path = "pty_e2e/esc_cancels_running_turn_from_prompt_preserves_draft.rs"]
mod esc_cancels_running_turn_from_prompt_preserves_draft;
#[path = "pty_e2e/esc_cancels_running_turn_from_scrollback.rs"]
mod esc_cancels_running_turn_from_scrollback;
#[path = "pty_e2e/esc_esc_clears_idle_prompt_into_the_stash.rs"]
mod esc_esc_clears_idle_prompt_into_the_stash;
#[path = "pty_e2e/esc_esc_opens_rewind_picker_silent_first_press.rs"]
mod esc_esc_opens_rewind_picker_silent_first_press;
#[path = "pty_e2e/esc_idle_empty_no_messages_is_swallowed_noop.rs"]
mod esc_idle_empty_no_messages_is_swallowed_noop;
#[path = "pty_e2e/interjection_reaches_model_ctrl_l_in_vscode_family.rs"]
mod interjection_reaches_model_ctrl_l_in_vscode_family;
#[path = "pty_e2e/interjection_reaches_model_in_same_turn.rs"]
mod interjection_reaches_model_in_same_turn;
#[path = "pty_e2e/mid_turn_slash_dropdown_esc_dismisses_not_cancel.rs"]
mod mid_turn_slash_dropdown_esc_dismisses_not_cancel;
#[path = "pty_e2e/minimal/minimal_ctrl_o_send_now_queued_apple_terminal.rs"]
mod minimal_ctrl_o_send_now_queued_apple_terminal;
#[path = "pty_e2e/queue_and_interjection_lifecycle.rs"]
mod queue_and_interjection_lifecycle;
#[path = "pty_e2e/queue_reorder_local_row_above_server_row.rs"]
mod queue_reorder_local_row_above_server_row;
#[path = "pty_e2e/queue_reorder_moves_row_up.rs"]
mod queue_reorder_moves_row_up;
#[path = "pty_e2e/queued_bash_promotion_renders_output_pty.rs"]
mod queued_bash_promotion_renders_output_pty;
#[path = "pty_e2e/queued_message_renders_once_not_twice.rs"]
mod queued_message_renders_once_not_twice;
#[path = "pty_e2e/removed_queued_prompt_never_sent.rs"]
mod removed_queued_prompt_never_sent;
#[path = "pty_e2e/send_now_tip_after_mid_turn_queue.rs"]
mod send_now_tip_after_mid_turn_queue;
#[path = "pty_e2e/send_then_ctrlc_rewinds_to_composer_no_history_dup.rs"]
mod send_then_ctrlc_rewinds_to_composer_no_history_dup;
#[path = "pty_e2e/shift_tab_plan_nudge_from_always_approve_enters_plan.rs"]
mod shift_tab_plan_nudge_from_always_approve_enters_plan;
#[path = "pty_e2e/up_focuses_queue_bottom_row.rs"]
mod up_focuses_queue_bottom_row;
#[path = "pty_e2e/verify_bashq_claim2_force_interject.rs"]
mod verify_bashq_claim2_force_interject;
#[path = "pty_e2e/verify_bashq_claim3_edit_keeps_bash.rs"]
mod verify_bashq_claim3_edit_keeps_bash;
