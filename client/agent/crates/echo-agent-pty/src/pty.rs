//! PTY wrapper using `portable-pty` for cross-platform pseudoterminal support.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Configuration for spawning a PTY session.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    /// Command and arguments to run.
    pub command: Vec<String>,
    /// Terminal width in columns.
    pub cols: u16,
    /// Terminal height in rows.
    pub rows: u16,
    /// Working directory.
    pub cwd: Option<PathBuf>,
    /// Additional environment variables.
    pub env: HashMap<String, String>,
}

/// Handle to a running PTY session.
pub struct PtyHandle {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

/// Resize-capable master half of a dismantled [`PtyHandle`].
///
/// portable-pty's unix master is not `Sync` (interior `RefCell`), so it sits
/// behind a mutex that is only held for the synchronous resize ioctl.
pub struct PtyMaster {
    master: std::sync::Mutex<Box<dyn MasterPty + Send>>,
}

impl PtyMaster {
    /// Resize the PTY (TIOCSWINSZ; the kernel delivers SIGWINCH to the child).
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .lock()
            .unwrap()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize PTY")
    }
}

/// Child-process half of a dismantled [`PtyHandle`].
pub struct PtyChild {
    child: Box<dyn portable_pty::Child + Send>,
}

impl PtyChild {
    /// Check if the child process is still alive.
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// Get the child process ID.
    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Wait for the child to exit and return the exit code.
    pub fn wait(&mut self) -> Result<u32> {
        let status = self.child.wait().context("failed to wait for child")?;
        Ok(status.exit_code())
    }

    /// Kill the child process.
    pub fn kill(&mut self) -> Result<()> {
        self.child.kill().context("failed to kill child process")
    }
}

impl PtyHandle {
    /// Spawn a new process in a PTY.
    pub fn spawn(config: &PtyConfig) -> Result<Self> {
        let pty_system = native_pty_system();

        let pty_size = PtySize {
            rows: config.rows,
            cols: config.cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(pty_size).context("failed to open PTY")?;

        let mut cmd = CommandBuilder::new(&config.command[0]);
        if config.command.len() > 1 {
            cmd.args(&config.command[1..]);
        }
        if let Some(ref cwd) = config.cwd {
            cmd.cwd(cwd);
        }
        for (key, value) in &config.env {
            cmd.env(key, value);
        }
        // Set TERM for proper terminal detection.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        // Not session-scoped: echo-agent-pty's child is the process it exists to run.
        #[allow(clippy::disallowed_methods)]
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn command in PTY")?;

        let reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;

        let writer = pair
            .master
            .take_writer()
            .context("failed to take PTY writer")?;

        Ok(Self {
            master: pair.master,
            child,
            reader,
            writer,
        })
    }

    /// Dismantle into the master half (kept for resize), the child half
    /// (moved into a waiter task), and the reader/writer streams.
    pub fn into_parts(
        self,
    ) -> (
        PtyMaster,
        PtyChild,
        Box<dyn Read + Send>,
        Box<dyn Write + Send>,
    ) {
        (
            PtyMaster {
                master: std::sync::Mutex::new(self.master),
            },
            PtyChild { child: self.child },
            self.reader,
            self.writer,
        )
    }
}
