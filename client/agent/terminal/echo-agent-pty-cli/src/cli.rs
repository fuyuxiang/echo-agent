//! CLI argument definitions using clap derive.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "echo-agent-pty")]
#[command(about = "Run commands in PTY and control them via HTTP")]
#[command(version)]
#[command(arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Spawn a command in a PTY and start an HTTP control server
    #[command(arg_required_else_help = true)]
    Run {
        /// Command to run (use -- before command)
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,

        /// Terminal width in columns
        #[arg(short = 'W', long, default_value = "80")]
        width: u16,

        /// Terminal height in rows
        #[arg(short = 'H', long, default_value = "24")]
        height: u16,

        /// Working directory
        #[arg(short = 'c', long)]
        cwd: Option<PathBuf>,

        /// Environment variable (KEY=VAL, repeatable)
        #[arg(short = 'e', long = "env", value_name = "KEY=VAL")]
        env: Vec<String>,

        /// TCP port to listen on (0 = auto-assign)
        #[arg(short, long, default_value = "0")]
        port: u16,

        /// Session name (registers in ~/.local/state/echo-agent-pty/sessions/)
        #[arg(short, long)]
        name: Option<String>,

        /// Take over an existing session name (replaces the registration; does not stop the old server)
        #[arg(long)]
        force: bool,

        /// Shutdown after N seconds
        #[arg(short, long, value_name = "SECS")]
        timeout: Option<u64>,

        /// Keep server running after process exits
        #[arg(short, long)]
        linger: bool,

        /// Suppress output (just print port number)
        #[arg(short, long)]
        quiet: bool,
    },

    /// Send keystrokes to a running session
    #[command(arg_required_else_help = true)]
    Send {
        #[command(flatten)]
        target: Target,

        /// Keys to send (vim notation)
        keys: String,

        /// Append Enter (<CR>) after keys
        #[arg(short = 'e', long)]
        enter: bool,
    },

    /// Query terminal screen content
    #[command(arg_required_else_help = true)]
    Screen {
        #[command(flatten)]
        target: Target,

        /// Row range, 1-indexed (e.g. "1:5")
        #[arg(short, long)]
        rows: Option<String>,

        /// Column range, 1-indexed
        #[arg(short = 'C', long)]
        cols: Option<String>,

        /// Output as JSON
        #[arg(short, long, conflicts_with_all = ["ansi", "styled", "html"])]
        json: bool,

        /// Show cursor position with this character
        #[arg(short = 'c', long)]
        cursor: Option<char>,

        /// Include ANSI escape codes
        #[arg(short, long, conflicts_with_all = ["json", "styled", "html"])]
        ansi: bool,

        /// Output as styled JSON (LLM-friendly)
        #[arg(short = 's', long, conflicts_with_all = ["json", "ansi", "html"])]
        styled: bool,

        /// Output as HTML
        #[arg(long, conflicts_with_all = ["json", "ansi", "styled"])]
        html: bool,

        /// Include trailing empty lines
        #[arg(long)]
        full: bool,

        /// Show line numbers
        #[arg(short = 'l', long)]
        line_numbers: bool,
    },

    /// Query process status
    #[command(arg_required_else_help = true)]
    Status {
        #[command(flatten)]
        target: Target,
    },

    /// Stop a running session
    #[command(arg_required_else_help = true)]
    Stop {
        #[command(flatten)]
        target: Target,
    },

    /// Resize terminal dimensions
    #[command(arg_required_else_help = true)]
    Resize {
        #[command(flatten)]
        target: Target,

        /// New size as COLSxROWS (e.g. "120x40")
        size: String,
    },

    /// Query cursor position
    #[command(arg_required_else_help = true)]
    Cursor {
        #[command(flatten)]
        target: Target,
    },

    /// Wait for a screen condition (event-driven, no polling).
    /// Exit codes: 0 matched, 1 timeout (failure JSON on stdout), 2 usage/connection error
    #[command(arg_required_else_help = true)]
    #[command(group(clap::ArgGroup::new("condition").required(true)))]
    Wait {
        #[command(flatten)]
        target: Target,

        /// Wait until this text appears on screen
        #[arg(long, group = "condition")]
        text: Option<String>,

        /// Wait until this regex matches the screen text
        #[arg(long, group = "condition")]
        regex: Option<String>,

        /// Wait until this text is absent from the screen
        #[arg(long, group = "condition")]
        gone: Option<String>,

        /// Wait until the screen has been unchanged for this many milliseconds
        #[arg(long, value_name = "MS", group = "condition")]
        stable_ms: Option<u64>,

        /// Timeout in seconds (server caps at 120)
        #[arg(short, long, default_value = "10")]
        timeout: u64,
    },

    /// List registered sessions
    List {
        /// Output as JSON
        #[arg(short, long)]
        json: bool,
    },
}

/// Target session — exactly one of host, port, or name must be provided.
#[derive(clap::Args)]
#[group(required = true, multiple = false)]
pub struct Target {
    /// Remote host address (e.g. 127.0.0.1:8080)
    #[arg(short = 'H', long)]
    pub host: Option<String>,

    /// Local server port
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Session name (from registry)
    #[arg(short, long)]
    pub name: Option<String>,
}

impl Target {
    /// Resolve target to a base URL.
    pub fn to_url(&self) -> anyhow::Result<String> {
        if let Some(ref h) = self.host {
            if h.starts_with("http") {
                return Ok(h.clone());
            }
            return Ok(format!("http://{h}"));
        }
        if let Some(p) = self.port {
            return Ok(format!("http://127.0.0.1:{p}"));
        }
        if let Some(ref n) = self.name {
            let info = crate::registry::lookup_session(n)?;
            return Ok(format!("http://127.0.0.1:{}", info.port));
        }
        anyhow::bail!("no target specified")
    }
}
