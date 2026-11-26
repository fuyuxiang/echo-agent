use std::path::PathBuf;

pub mod info;

pub use info::Info;

// Re-export shared feedback wire types used by downstream crates
// (e.g. echo-agent-pager-render).
pub use echo_agent_chat_proxy_types::feedback_types::FeedbackTerminalInfo;

pub fn session_dir(info: &Info) -> PathBuf {
    echo_agent_tools::util::echo_agent_home::sessions_cwd_dir(&info.cwd).join(info.id.to_string())
}
