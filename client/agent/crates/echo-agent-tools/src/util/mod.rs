pub mod base64_images;
pub mod binary;
pub mod command_display;
pub mod echo_agent_home;
pub mod env;
pub mod fs;
pub mod git_detect;
pub mod hash;
pub mod image_compress;
pub mod image_validate;
pub(crate) mod knowledge_result;
pub mod mcp_truncate;
pub mod path_suggestions;
pub mod process_safety;
pub(crate) mod query_tools;
pub mod remap;
pub mod serde_base64;
pub(crate) mod shared_http;
pub mod shell_env_policy;
pub mod spawn;
pub mod truncate;
pub mod unicode_confusables;

pub use crate::implementations::echo_agent_build::grep::ripgrep::rg_path;
pub use command_display::strip_redundant_session_cd;
pub use echo_agent_home::{echo_agent_application, echo_agent_home};
pub use echo_agent_tty_utils::detach_std_command;
#[cfg(unix)]
pub use env::detach_from_tty;
pub use env::substitute_plugin_tokens;
pub use env::{
    ECHO_AGENT_AGENT_ENV, ECHO_AGENT_AGENT_ENV_VALUE, apply_echo_agent_agent_marker, pager_env,
};
pub use fs::{UnicodePathMatch, canonicalize_with_timeout, try_resolve_unicode_filename};
pub use path_suggestions::format_not_found_error;
pub use remap::{remap_json_keys, remap_schema_properties, reverse_map};
pub use shell_env_policy::{
    EnvironmentVariablePattern, ShellEnvironmentPolicy, ShellEnvironmentPolicyInherit,
    apply_shell_environment_policy,
};
pub use spawn::{
    ProcessGroup, ProcessScope, detach_command, detach_search_command, global_process_scope,
    new_process_group, reap_killed_search_child,
};
pub use truncate::{
    DEFAULT_SOFT_WRAP_WIDTH, ceil_char_boundary, estimate_tokens, floor_char_boundary,
    format_bytes, soft_wrap_line, soft_wrap_lines, truncate_line, truncate_str,
    truncate_str_with_marker,
};
