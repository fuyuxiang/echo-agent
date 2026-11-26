//! `pty-scenario` — scripted TUI regression runner for echo-agent-pager.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser as ClapParser;
use echo_agent_pager_pty_harness::{
    ScriptedRunConfig, ScriptedRunStatus, ScriptedScenario, ScriptedScenarioRunner, pager_binary,
};

#[derive(ClapParser, Debug)]
#[command(
    name = "pty-scenario",
    about = "Run declarative TUI regression scenarios against echo-agent-pager",
    long_about = None,
)]
struct Cli {
    /// Scenario file to run. Supports JSON, YAML, and YML.
    #[arg(long, value_name = "PATH")]
    scenario: PathBuf,

    /// Pager binary. Defaults to PAGER_BINARY, CARGO_BIN_EXE_echo-agent-pager,
    /// or a locally-built debug binary.
    #[arg(long, value_name = "PATH")]
    binary: Option<PathBuf>,

    /// Directory for report.json, bugs.md, text/html/svg/styled-json captures.
    #[arg(long, value_name = "DIR", default_value = "target/pty-scenarios")]
    artifacts: PathBuf,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    match run().await {
        Ok(status) => status,
        Err(error) => {
            eprintln!("pty-scenario failed: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let scenario = ScriptedScenario::from_file(&cli.scenario)
        .with_context(|| format!("load scenario {}", cli.scenario.display()))?;
    let binary = match cli.binary {
        Some(path) => path,
        None => pager_binary().context("resolve pager binary")?,
    };
    if !binary.exists() {
        bail!("pager binary does not exist: {}", binary.display());
    }

    let runner = ScriptedScenarioRunner::new(ScriptedRunConfig::new(binary, cli.artifacts));
    let report = runner.run(&scenario).await?;
    let json = serde_json::to_string_pretty(&report).context("serialize final report")?;
    println!("{json}");

    match report.status {
        ScriptedRunStatus::Passed | ScriptedRunStatus::Skipped => Ok(ExitCode::SUCCESS),
        ScriptedRunStatus::Failed | ScriptedRunStatus::Running => Ok(ExitCode::from(1)),
    }
}
