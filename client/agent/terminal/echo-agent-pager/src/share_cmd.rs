use anyhow::Result;
use echo_agent_runtime::agent::config::Config as AgentConfig;

#[derive(Debug, clap::Args, Clone)]
pub struct ShareArgs {
    /// Session ID to share
    pub session_id: String,
}

pub async fn run(args: &ShareArgs, agent_config: &AgentConfig) -> Result<()> {
    let _ = (args, agent_config);
    anyhow::bail!("Session sharing is temporarily disabled");
}
