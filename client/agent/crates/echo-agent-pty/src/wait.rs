//! Event-driven wait/expect primitives over the terminal grid.
//!
//! Waiters never poll: the session's feeder bumps a generation watch after
//! each `term.feed()`, and conditions are re-checked only on bumps.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{Mutex, watch};

use crate::term::{CursorPosition, ScreenOpts, Terminal, TerminalModes};

/// Maximum bytes of recent raw PTY output kept for wait-timeout diagnostics.
pub(crate) const RAW_TAIL_CAP: usize = 2048;

/// A condition `wait_for` blocks on.
#[derive(Debug, Clone)]
pub enum WaitCondition {
    /// Text appears on screen (substring of the trimmed screen text).
    Text(String),
    /// Regex matches the trimmed screen text.
    Regex(String),
    /// Text is absent from the screen.
    Gone(String),
    /// No grid update for this many milliseconds.
    StableMs(u64),
}

/// Result of a `wait_for` call.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WaitOutcome {
    pub matched: bool,
    pub elapsed_ms: u64,
    /// Present only on timeout, so the agent never needs a follow-up call.
    #[serde(flatten)]
    pub diagnostics: Option<WaitDiagnostics>,
}

/// Screen snapshot attached to a timed-out wait.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WaitDiagnostics {
    /// Trimmed screen text (the exact text the condition was evaluated against).
    pub screen: String,
    pub cursor: CursorPosition,
    pub modes: TerminalModes,
    /// Last bytes of raw PTY output (lossy UTF-8, at most [`RAW_TAIL_CAP`] bytes).
    pub raw_tail: String,
    /// Grid generation at timeout.
    pub generation: u64,
    /// True when the session's output ended (child exited) before the deadline,
    /// so the condition could never have been met.
    pub ended: bool,
}

/// Handles needed to wait on screen conditions without holding any outer session lock.
///
/// The HTTP server wraps `PtySession` in a mutex; a long-poll must clone
/// this handle and drop the session guard, or it would block send/screen
/// for the whole wait.
///
/// TODO: the next verb needing lock-free session access must instead make
/// `stop()` take `&self` and switch the server state to `Arc<PtySession>`,
/// deleting this handle — do not clone another field trio.
pub struct WaitHandle {
    pub(crate) terminal: Arc<Mutex<Terminal>>,
    pub(crate) generation_rx: watch::Receiver<u64>,
    pub(crate) raw_tail: Arc<std::sync::Mutex<VecDeque<u8>>>,
}

impl WaitHandle {
    /// Wait until `condition` is met or `timeout` elapses.
    ///
    /// Event-driven: the grid is re-checked only when the feeder bumps the
    /// generation. Errors only on an invalid regex pattern.
    pub async fn wait_for(
        mut self,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<WaitOutcome> {
        let start = Instant::now();
        let deadline = tokio::time::Instant::now() + timeout;
        // Compile once so a bad pattern fails fast instead of on every check.
        let regex = match &condition {
            WaitCondition::Regex(pattern) => {
                Some(regex::Regex::new(pattern).context("invalid regex")?)
            }
            _ => None,
        };

        if let WaitCondition::StableMs(window_ms) = condition {
            let window = Duration::from_millis(window_ms);
            return Ok(self.wait_stable(window, start, deadline).await);
        }

        loop {
            // Mark the current generation seen before checking, so a feed racing the check wakes changed().
            self.generation_rx.borrow_and_update();
            {
                let term = self.terminal.lock().await;
                let text = screen_text(&term);
                let met = match &condition {
                    WaitCondition::Text(needle) => text.contains(needle),
                    WaitCondition::Gone(needle) => !text.contains(needle),
                    WaitCondition::Regex(_) => regex.as_ref().is_some_and(|re| re.is_match(&text)),
                    WaitCondition::StableMs(_) => unreachable!("handled above"),
                };
                if met {
                    return Ok(WaitOutcome {
                        matched: true,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        diagnostics: None,
                    });
                }
            }
            tokio::select! {
                changed = self.generation_rx.changed() => {
                    if changed.is_err() {
                        // Feeder gone (session over): the grid is final, so fail fast with diagnostics.
                        return Ok(self.timeout_outcome(start, true).await);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Ok(self.timeout_outcome(start, false).await);
                }
            }
        }
    }

    /// Wait until the grid has been unchanged for `window`, bounded by `deadline`.
    async fn wait_stable(
        &mut self,
        window: Duration,
        start: Instant,
        deadline: tokio::time::Instant,
    ) -> WaitOutcome {
        // A dropped sender means no further grid updates: the remaining window always completes.
        let mut sender_gone = false;
        loop {
            self.generation_rx.borrow_and_update();
            let window_end = tokio::time::Instant::now() + window;
            tokio::select! {
                changed = self.generation_rx.changed(), if !sender_gone => {
                    if changed.is_err() {
                        sender_gone = true;
                    }
                    // Activity: restart the stability window unless out of time.
                    if tokio::time::Instant::now() >= deadline {
                        return self.timeout_outcome(start, sender_gone).await;
                    }
                }
                _ = tokio::time::sleep_until(window_end.min(deadline)) => {
                    if window_end <= deadline {
                        return WaitOutcome {
                            matched: true,
                            elapsed_ms: start.elapsed().as_millis() as u64,
                            diagnostics: None,
                        };
                    }
                    return self.timeout_outcome(start, sender_gone).await;
                }
            }
        }
    }

    /// Snapshot the screen state for a timed-out wait.
    async fn timeout_outcome(&self, start: Instant, ended: bool) -> WaitOutcome {
        let (screen, cursor, modes) = {
            let term = self.terminal.lock().await;
            (
                screen_text(&term),
                term.cursor_position(),
                term.terminal_modes(),
            )
        };
        let raw_tail = {
            let tail = self.raw_tail.lock().unwrap();
            String::from_utf8_lossy(&tail.iter().copied().collect::<Vec<u8>>()).into_owned()
        };
        WaitOutcome {
            matched: false,
            elapsed_ms: start.elapsed().as_millis() as u64,
            diagnostics: Some(WaitDiagnostics {
                screen,
                cursor,
                modes,
                raw_tail,
                generation: *self.generation_rx.borrow(),
                ended,
            }),
        }
    }
}

/// Trimmed screen text — identical to what agents see via the screen API.
fn screen_text(term: &Terminal) -> String {
    term.screen_content(&ScreenOpts::default()).lines.join("\n")
}

/// Append bytes to the bounded raw-output tail, evicting the oldest bytes.
pub(crate) fn push_raw_tail(tail: &std::sync::Mutex<VecDeque<u8>>, bytes: &[u8]) {
    let mut tail = tail.lock().unwrap();
    if bytes.len() >= RAW_TAIL_CAP {
        tail.clear();
        tail.extend(&bytes[bytes.len() - RAW_TAIL_CAP..]);
        return;
    }
    while tail.len() + bytes.len() > RAW_TAIL_CAP {
        tail.pop_front();
    }
    tail.extend(bytes);
}
