//! Shell mode, edit/tool rendering, folder trust, skills, and MCP PTY coverage.
//!
//! All cases are ignored for ordinary Cargo runs; Bazel opts in and caps this
//! process-heavy family at four concurrent libtest workers.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/scroll.rs"]
mod scroll;

#[path = "pty_e2e/bash_mode_file_completion_shell_like.rs"]
mod bash_mode_file_completion_shell_like;
#[path = "pty_e2e/bash_mode_strips_redundant_session_cd_from_chrome.rs"]
mod bash_mode_strips_redundant_session_cd_from_chrome;
#[path = "pty_e2e/bash_mode_tab_completion_dropdown.rs"]
mod bash_mode_tab_completion_dropdown;
#[path = "pty_e2e/consent_gate_blocks_session_pty.rs"]
mod consent_gate_blocks_session_pty;
#[path = "pty_e2e/edit_collapsed_oneliner_pty.rs"]
mod edit_collapsed_oneliner_pty;
#[path = "pty_e2e/edit_hl_inplace_refresh_pty.rs"]
mod edit_hl_inplace_refresh_pty;
#[path = "pty_e2e/edit_merge_parallel_pty.rs"]
mod edit_merge_parallel_pty;
#[path = "pty_e2e/edit_merge_sequential_pty.rs"]
mod edit_merge_sequential_pty;
#[path = "pty_e2e/file_path_with_space_emits_full_osc8_hyperlink.rs"]
mod file_path_with_space_emits_full_osc8_hyperlink;
#[path = "pty_e2e/folder_trust_cwd_is_home_git_repo_no_prompt.rs"]
mod folder_trust_cwd_is_home_git_repo_no_prompt;
#[path = "pty_e2e/folder_trust_decline_quits_without_grant.rs"]
mod folder_trust_decline_quits_without_grant;
#[path = "pty_e2e/folder_trust_feature_off_shows_no_question.rs"]
mod folder_trust_feature_off_shows_no_question;
#[path = "pty_e2e/folder_trust_home_git_repo_subdir_keys_on_subdir.rs"]
mod folder_trust_home_git_repo_subdir_keys_on_subdir;
#[path = "pty_e2e/folder_trust_question_renders_and_accept_persists_grant.rs"]
mod folder_trust_question_renders_and_accept_persists_grant;
#[path = "pty_e2e/managed_policy_gate_refusal_reaches_real_terminal.rs"]
mod managed_policy_gate_refusal_reaches_real_terminal;
#[path = "pty_e2e/mcp_menu_loads_servers_in_non_project_dir.rs"]
mod mcp_menu_loads_servers_in_non_project_dir;
#[path = "pty_e2e/mcp_menu_loads_servers_in_project_dir.rs"]
mod mcp_menu_loads_servers_in_project_dir;
#[path = "pty_e2e/mid_text_skill_token_echo_styled_pty.rs"]
mod mid_text_skill_token_echo_styled_pty;
#[path = "pty_e2e/permission_prompt_hook_chimes_only_on_real_wait.rs"]
mod permission_prompt_hook_chimes_only_on_real_wait;
