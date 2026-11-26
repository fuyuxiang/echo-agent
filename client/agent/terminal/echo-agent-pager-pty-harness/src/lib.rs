//! Unified PTY harness for echo-agent-pager.
//!
//! The same layered API serves three consumers:
//!
//! 1. **Regression scenarios** (e.g. `scenarios::plan_approval_resume`,
//!    exercised via `tests/` in this crate and `pty-scenario` YAML under
//!    `echo-agent-pager/tests/scenarios/`) — assert screen contents and
//!    multi-process resume behavior.
//! 2. **Benchmarks** (`benches/pty_bench.rs`) — run timing scenarios, collect
//!    per-frame timings, emit JSON / compare against baselines.
//! 3. **Ad-hoc scenario runs** — spin up the harness to reproduce issues locally.
//!
//! ## Layers
//!
//! - **`pty`** (L1)       — PTY management (spawn, inject keys, resize, drain).
//! - **`screen`** (L2a)   — Virtual terminal state via `alacritty_terminal` ("what the user sees").
//! - **`timing`** (L2b)   — Per-frame durations via `?2026 h/l` markers.
//! - **`content`** (L3)   — Mock inference server driving real content into the pager.
//! - **`scenarios`**      — Named, parameterised workloads returning `BenchResults`.
//! - **`results`**        — Aggregated statistics, baseline compare.
//! - **`scroll_matrix`**  — `ECHO_AGENT_SCROLL_LOG` JSONL ingestion for the scroll validation matrix.
//! - **`env`**            — Binary resolution and workspace path helpers.
//! - **`flows`**          — Cross-suite drive/seed helpers shared by the pager's e2e targets.

pub mod content;
pub mod env;
pub mod flows;
pub mod host_clipboard;
pub mod leader;
pub mod pty;
pub mod results;
pub mod scenarios;
pub mod screen;
pub mod scripted;
pub mod scroll_matrix;
pub mod timing;

pub use content::{
    AgentTurnExpectation, ContentController, InferenceEndpoint, InferenceExpectation,
    InferenceRequestMatcher, MockModel, ScriptedResponse, SseEvent, sse,
};
pub use env::pager_binary;
pub use flows::{
    inference_request_count, oauth_credential_ops, seed_fake_oauth,
    seed_fake_oauth_coding_data_opted_out, seed_fake_oauth_team_member, seed_fake_oauth_zdr_team,
    submit_turn, wait_for_labels_absent, wait_for_model_via_new_sessions,
};
pub use host_clipboard::HostClipboardTextGuard;
pub use leader::LeaderCluster;
use pty::PtyRead;
pub use pty::{EnvOp, PtyController, PtyExitPoll, keys};
pub use results::{BenchResults, compare_baseline};
pub use scenarios::Scenario;
pub use screen::ScreenTracker;
pub use scripted::{
    BugFinding, BugSeverity, DimensionAssertion, EnvVar, EnvironmentConfig, ImageFixture,
    ImageFixtureKind, MockConfig, MouseButton, MousePoint, SGR_SCROLL_DOWN, SGR_SCROLL_UP,
    ScenarioStep, ScriptedRunConfig, ScriptedRunReport, ScriptedRunStatus, ScriptedScenario,
    ScriptedScenarioRunner, ScrollDirection, StepOutcome, StepStatus, TerminalConfig,
    VisualArtifact,
};
pub use timing::{FrameTiming, FrameTimingParser};

// Re-export echo-agent-pty types for richer terminal emulation, vim key notation,
// and styled output support.
pub use echo_agent_pty::keys::parse_keys;
pub use echo_agent_pty::styled::{StyledLine, StyledRun};
pub use echo_agent_pty::term::{ScreenOutput, Terminal as AlacrittyTerminal};

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::PtySize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtyPump {
    Chunk,
    Timeout,
    Closed,
}

/// High-level harness that composes PTY control, screen state, and frame timing.
///
/// The key method is [`update`](PtyHarness::update), which receives PTY output
/// chunks inline and feeds each to **both** the [`ScreenTracker`] and
/// [`FrameTimingParser`] as it arrives. This preserves inter-chunk timing for
/// accurate frame measurement.
pub struct PtyHarness {
    pty: PtyController,
    screen: ScreenTracker,
    timing: FrameTimingParser,
    raw_output: Vec<u8>,
    /// Spawn instant — the time origin for asciinema cast event timestamps.
    spawned_at: Instant,
    /// Per-chunk cast events as `(elapsed_secs, end_offset_into_raw_output)`.
    /// Each event's bytes are `raw_output[prev_end..end]`, so the chunks are
    /// not duplicated in memory.
    cast_events: Vec<(f64, usize)>,
    /// Terminal size at spawn as `(cols, rows)` for the cast header.
    cast_size: (u16, u16),
    /// When true, [`update`](Self::update) forwards terminal-generated replies
    /// (cursor-position reports, device attributes, …) back to the child.
    /// Off by default so tests that script their own probe replies (e.g.
    /// `pty_xtversion`) keep full control; minimal-mode tests turn it on so the
    /// inline viewport's startup cursor query completes instead of timing out.
    respond_to_queries: bool,
}

impl PtyHarness {
    /// Inherit the parent environment for terminal/shell behavior tests
    /// (XTVERSION probes and echoagent wrap). Content-backed launches must use
    /// [`Self::new_in_sandbox`].
    pub fn new_inherited_env(
        binary: &Path,
        rows: u16,
        cols: u16,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pty = PtyController::spawn_inherited_env(binary, size, args, env, cwd)
            .context("failed to spawn pager in PTY")?;
        Ok(Self::from_pty(pty, rows, cols))
    }

    /// Spawn from a canonical [`echo_agent_test_support::TestSandbox`] baseline
    /// plus Set-only convenience overrides.
    pub fn new_in_sandbox(
        binary: &Path,
        rows: u16,
        cols: u16,
        args: &[&str],
        sandbox: &echo_agent_test_support::TestSandbox,
        env: &[(&str, &str)],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        let operations: Vec<_> = env
            .iter()
            .map(|(key, value)| EnvOp::set(key, value))
            .collect();
        Self::new_in_sandbox_ops(binary, rows, cols, args, sandbox, &operations, cwd)
    }

    /// Spawn from a canonical sandbox baseline plus typed Set/Remove operations.
    pub fn new_in_sandbox_ops(
        binary: &Path,
        rows: u16,
        cols: u16,
        args: &[&str],
        sandbox: &echo_agent_test_support::TestSandbox,
        operations: &[EnvOp<'_>],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pty = PtyController::spawn_in_sandbox(binary, size, args, sandbox, operations, cwd)
            .context("failed to spawn pager in PTY")?;
        Ok(Self::from_pty(pty, rows, cols))
    }

    fn from_pty(pty: PtyController, rows: u16, cols: u16) -> Self {
        Self {
            pty,
            screen: ScreenTracker::new(rows, cols),
            timing: FrameTimingParser::new(),
            raw_output: Vec::new(),
            spawned_at: Instant::now(),
            cast_events: Vec::new(),
            cast_size: (cols, rows),
            respond_to_queries: false,
        }
    }

    /// Enable (or disable) forwarding terminal-generated replies back to the
    /// child during [`update`](Self::update). Real terminals answer device
    /// queries automatically; the harness leaves this off by default so probe
    /// tests can script their own replies. Minimal-mode tests enable it so the
    /// inline viewport's startup cursor-position query (`ESC[6n`) is answered
    /// and `--minimal` is not silently downgraded to full-screen inline.
    pub fn set_respond_to_queries(&mut self, enabled: bool) {
        self.respond_to_queries = enabled;
    }

    /// Spawn the pager with env vars from a [`ContentController`] attached.
    ///
    /// This is the common pattern for both e2e tests and benchmarks:
    ///
    /// ```no_run
    /// # use std::time::Duration;
    /// # use echo_agent_pager_pty_harness::{PtyHarness, ContentController, pager_binary};
    /// # async fn example() -> anyhow::Result<()> {
    /// let content = ContentController::start().await?;
    /// content.set_response("# Hello\n\nAgent said hi.");
    ///
    /// let mut harness = PtyHarness::spawn_with_content(
    ///     &pager_binary()?, 50, 120, &content, &[],
    /// )?;
    /// harness.wait_for_text("Hello", Duration::from_secs(10))?;
    /// harness.quit()?;
    /// # Ok(()) }
    /// ```
    pub fn spawn_with_content(
        binary: &Path,
        rows: u16,
        cols: u16,
        content: &ContentController,
        extra_args: &[&str],
    ) -> Result<Self> {
        Self::spawn_with_content_env_in_dir(binary, rows, cols, content, extra_args, &[], None)
    }

    /// Like [`spawn_with_content`](Self::spawn_with_content), with an explicit working directory.
    pub fn spawn_with_content_in_dir(
        binary: &Path,
        rows: u16,
        cols: u16,
        content: &ContentController,
        extra_args: &[&str],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        Self::spawn_with_content_env_in_dir(binary, rows, cols, content, extra_args, &[], cwd)
    }

    /// Content-backed spawn with Set-only convenience overrides applied after
    /// the sandbox baseline. Duplicate keys are last-wins.
    pub fn spawn_with_content_env(
        binary: &Path,
        rows: u16,
        cols: u16,
        content: &ContentController,
        extra_args: &[&str],
        overrides: &[(&str, &str)],
    ) -> Result<Self> {
        Self::spawn_with_content_env_in_dir(
            binary, rows, cols, content, extra_args, overrides, None,
        )
    }

    pub fn spawn_with_content_env_in_dir(
        binary: &Path,
        rows: u16,
        cols: u16,
        content: &ContentController,
        extra_args: &[&str],
        overrides: &[(&str, &str)],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        let operations: Vec<_> = overrides
            .iter()
            .map(|(key, value)| EnvOp::set(key, value))
            .collect();
        Self::spawn_with_content_env_ops_in_dir(
            binary,
            rows,
            cols,
            content,
            extra_args,
            &operations,
            cwd,
        )
    }

    /// Content-backed spawn with typed Set/Remove operations.
    pub fn spawn_with_content_env_ops(
        binary: &Path,
        rows: u16,
        cols: u16,
        content: &ContentController,
        extra_args: &[&str],
        operations: &[EnvOp<'_>],
    ) -> Result<Self> {
        Self::spawn_with_content_env_ops_in_dir(
            binary, rows, cols, content, extra_args, operations, None,
        )
    }

    pub fn spawn_with_content_env_ops_in_dir(
        binary: &Path,
        rows: u16,
        cols: u16,
        content: &ContentController,
        extra_args: &[&str],
        operations: &[EnvOp<'_>],
        cwd: Option<&Path>,
    ) -> Result<Self> {
        Self::new_in_sandbox_ops(
            binary,
            rows,
            cols,
            extra_args,
            content.sandbox(),
            operations,
            cwd,
        )
    }

    // ── PTY control ──────────────────────────────────────────────────

    /// Inject raw key bytes into the PTY.
    pub fn inject_keys(&mut self, keys: &[u8]) -> Result<()> {
        self.pty.inject_keys(keys)
    }

    /// Resize the PTY and virtual screen. Arguments are `(rows, cols)`.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.pty.resize(rows, cols)?;
        self.screen.resize(rows, cols);
        Ok(())
    }

    // ── Update: receive PTY output inline → feed both parsers ────────

    /// Receive PTY output for up to `timeout`, feeding each chunk to both
    /// the screen state tracker and the frame timing parser as it arrives.
    ///
    /// Processing inline (rather than buffering all chunks first) preserves
    /// inter-chunk timing so that `FrameTimingParser` records accurate
    /// wall-clock frame durations.
    pub fn update(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !matches!(self.pump_one(remaining), PtyPump::Chunk) {
                break;
            }
        }
    }

    fn pump_one(&mut self, timeout: Duration) -> PtyPump {
        match self.pty.recv_chunk(timeout) {
            PtyRead::Chunk(chunk) => {
                self.raw_output.extend_from_slice(&chunk);
                self.cast_events.push((
                    self.spawned_at.elapsed().as_secs_f64(),
                    self.raw_output.len(),
                ));
                self.screen.feed(&chunk);
                self.timing.feed(&chunk);
                if self.respond_to_queries {
                    let responses = self.screen.drain_responses();
                    if !responses.is_empty() {
                        let _ = self.pty.inject_keys(&responses);
                    }
                }
                PtyPump::Chunk
            }
            PtyRead::Timeout => PtyPump::Timeout,
            PtyRead::Closed => PtyPump::Closed,
        }
    }

    /// Feed bytes **directly into the virtual screen only**, bypassing the
    /// child (echoagent).
    ///
    /// Simulates an out-of-band repaint/reflow by an outer layer (tmux, or an
    /// nvim/vim `:terminal`) that changes what's on screen without going
    /// through echoagent's stdout. Used to reproduce the doubled-line class of bugs
    /// where echoagent's diff renderer never re-asserts a region it didn't write
    /// itself (since the harness is a single faithful emulator and cannot nest
    /// a real tmux/nvim).
    pub fn feed_screen(&mut self, bytes: &[u8]) {
        self.screen.feed(bytes);
    }

    /// Return true only while the child is live; pending status is non-running.
    pub fn is_running(&mut self) -> Result<bool> {
        self.pty.is_running()
    }

    // ── Screen state queries ─────────────────────────────────────────

    /// Return structured plain-text screen contents.
    pub fn screen_output(&self) -> ScreenOutput {
        self.screen.output()
    }

    /// Return the full text contents of the virtual screen.
    pub fn screen_contents(&self) -> String {
        self.screen.contents()
    }

    /// Return the full screen with style information.
    pub fn screen_styled(&self) -> Vec<StyledLine> {
        self.screen.styled()
    }

    /// Render the current screen as HTML.
    pub fn screen_html(&self) -> String {
        self.screen.html()
    }

    /// Check whether the screen contains the given text.
    pub fn contains_text(&self, text: &str) -> bool {
        self.screen.contains(text)
    }

    /// Active terminal modes the child has enabled (bracketed paste, mouse
    /// reporting, alt screen, …), as tracked by the virtual terminal.
    pub fn terminal_modes(&self) -> echo_agent_pty::term::TerminalModes {
        self.screen.terminal().terminal_modes()
    }

    /// Pump PTY output until `condition` becomes true or `timeout` expires.
    ///
    /// The condition is checked before the first pump and after each output
    /// slice. `description` names the semantic state in timeout diagnostics.
    pub fn wait_until(
        &mut self,
        description: &str,
        timeout: Duration,
        mut condition: impl FnMut(&Self) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if condition(self) {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!(
                    "timed out after {timeout:?} waiting for {description}\n\
                     process running: {}\nprocess tree: {}\nscreen contents:\n{}",
                    self.pty.is_running()?,
                    self.pty.process_tree_diagnostics(),
                    self.screen.contents()
                );
            }
            match self.pump_one(Duration::from_millis(50).min(remaining)) {
                PtyPump::Chunk | PtyPump::Timeout => {}
                PtyPump::Closed => {
                    anyhow::bail!(
                        "PTY closed while waiting for {description}\n\
                         process running: false\nscreen contents:\n{}\nraw output:\n{}",
                        self.screen.contents(),
                        String::from_utf8_lossy(&self.raw_output)
                    );
                }
            }
        }
    }

    /// Like [`Self::wait_until`], but the condition must remain true for `hold`.
    ///
    /// The single `timeout` covers both reaching the condition and holding it;
    /// PTY output continues to be pumped throughout the stability window.
    pub fn wait_until_stable(
        &mut self,
        description: &str,
        timeout: Duration,
        hold: Duration,
        mut condition: impl FnMut(&Self) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut true_since = None;
        loop {
            if condition(self) {
                let since = true_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= hold {
                    return Ok(());
                }
            } else {
                true_since = None;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!(
                    "timed out after {timeout:?} waiting for {description} to remain true for \
                     {hold:?}\nprocess running: {}\nprocess tree: {}\nscreen contents:\n{}",
                    self.pty.is_running()?,
                    self.pty.process_tree_diagnostics(),
                    self.screen.contents()
                );
            }
            match self.pump_one(Duration::from_millis(50).min(remaining)) {
                PtyPump::Chunk | PtyPump::Timeout => {}
                PtyPump::Closed => {
                    anyhow::bail!(
                        "PTY closed while waiting for {description} to remain true for {hold:?}\n\
                         process running: false\nscreen contents:\n{}\nraw output:\n{}",
                        self.screen.contents(),
                        String::from_utf8_lossy(&self.raw_output)
                    );
                }
            }
        }
    }

    /// Block until the screen contains `text` or `timeout` expires.
    pub fn wait_for_text(&mut self, text: &str, timeout: Duration) -> Result<()> {
        self.wait_until(&format!("screen text {text:?}"), timeout, |h| {
            h.contains_text(text)
        })
    }

    /// Block until the visible screen no longer contains `text`.
    pub fn wait_for_text_absent(&mut self, text: &str, timeout: Duration) -> Result<()> {
        self.wait_until(
            &format!("screen text {text:?} to disappear"),
            timeout,
            |h| !h.contains_text(text),
        )
    }

    /// Wait for a rendered response to reach the idle prompt state.
    ///
    /// Call this after observing turn output: the running status and cancel
    /// keybar disappear only after the pager finalizes the turn.
    pub fn wait_for_turn_idle(&mut self, timeout: Duration) -> Result<()> {
        self.wait_until_stable(
            "turn to become idle",
            timeout,
            Duration::from_millis(250),
            |h| {
                !h.contains_text("Ctrl+c:cancel")
                    && !h.contains_text("Waiting for response")
                    && !h.contains_text("Responding")
            },
        )
    }

    /// Return all raw bytes emitted by the child PTY so far.
    pub fn raw_output(&self) -> &[u8] {
        &self.raw_output
    }

    /// Write everything the child PTY emitted so far as an asciinema v2 cast
    /// (`.cast`), one output event per received chunk with its original
    /// arrival timestamp. Replayable locally with `asciinema play`. Bytes are
    /// decoded lossily so binary escapes cannot poison the JSON encoding.
    ///
    /// Limitation: the header is pinned to the spawn-time size and no `"r"`
    /// resize events are emitted, so a cast from a test that calls
    /// [`resize`](Self::resize) plays back at the original geometry.
    pub fn write_cast(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create cast dir {}", parent.display()))?;
        }
        let (cols, rows) = self.cast_size;
        let mut out = String::new();
        out.push_str(&serde_json::json!({"version": 2, "width": cols, "height": rows}).to_string());
        out.push('\n');
        let mut start = 0usize;
        for (elapsed, end) in &self.cast_events {
            let mut end = *end;
            // A multi-byte codepoint split across two PTY reads must not be
            // lossy-decoded in halves: back off to the char boundary and let
            // the partial bytes ride in the next event (a dangling tail at
            // end-of-capture still decodes lossily — nothing to carry into).
            while end > start
                && end < self.raw_output.len()
                && (self.raw_output[end] & 0xC0) == 0x80
            {
                end -= 1;
            }
            if end == start {
                continue;
            }
            let data = String::from_utf8_lossy(&self.raw_output[start..end]);
            out.push_str(&serde_json::json!([elapsed, "o", data]).to_string());
            out.push('\n');
            start = end;
        }
        std::fs::write(path, out).with_context(|| format!("write cast {}", path.display()))
    }

    // ── Scrollback queries (minimal mode commits blocks into native history) ──

    /// The terminal's scrollback history as text (oldest line first).
    pub fn scrollback_text(&self) -> String {
        self.screen.scrollback_text()
    }

    /// Scrollback history + the visible screen, joined oldest→newest. Use for
    /// minimal-mode assertions: a committed block may be on-screen or scrolled
    /// above the pinned viewport depending on how much has accumulated.
    pub fn full_text(&self) -> String {
        self.screen.full_text()
    }

    /// Whether scrollback + visible screen contains `text`.
    pub fn contains_full_text(&self, text: &str) -> bool {
        self.screen.full_contains(text)
    }

    /// Block until scrollback + visible screen contains `text`, or `timeout`
    /// expires. The scrollback-aware companion to [`Self::wait_for_text`] for
    /// content that may have scrolled above the viewport (minimal mode).
    pub fn wait_for_full_text(&mut self, text: &str, timeout: Duration) -> Result<()> {
        let result = self.wait_until(&format!("full text {text:?}"), timeout, |h| {
            h.contains_full_text(text)
        });
        result.map_err(|error| {
            anyhow::anyhow!("{error}\nfull contents:\n{}", self.screen.full_text())
        })
    }

    /// Block until scrollback + visible screen no longer contains `text`.
    pub fn wait_for_full_text_absent(&mut self, text: &str, timeout: Duration) -> Result<()> {
        let result = self.wait_until(&format!("full text {text:?} to disappear"), timeout, |h| {
            !h.contains_full_text(text)
        });
        result.map_err(|error| {
            anyhow::anyhow!("{error}\nfull contents:\n{}", self.screen.full_text())
        })
    }

    /// Count Kitty graphics APC sequences that carry image data or placement in
    /// the raw PTY output so far (delete / capability-query escapes excluded).
    ///
    /// These escapes are written into the synchronized-update frame buffer
    /// (outside the vt100 cell grid), so they aren't visible to `wait_for_text`;
    /// scanning the raw bytes is the only way to observe them.
    pub fn count_kitty_graphics(&self) -> usize {
        scripted::count_kitty_graphics(&self.raw_output)
    }

    /// Block until at least `min` Kitty graphics APC sequences (`ESC _ G`) have
    /// appeared in the raw PTY output, or `timeout` expires.
    ///
    /// Polling the raw bytes avoids a fixed sleep (flake under load) and returns
    /// as soon as the image is transmitted/placed.
    pub fn wait_for_kitty_graphics(&mut self, min: usize, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.count_kitty_graphics() >= min {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!(
                    "timed out after {timeout:?} waiting for {min} Kitty graphics escape(s); \
                     found {}",
                    self.count_kitty_graphics()
                );
            }
            self.update(Duration::from_millis(50).min(remaining));
        }
    }

    /// Return the current cursor position as `(row, col)`.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.screen.cursor_position()
    }

    // ── Frame timing queries ─────────────────────────────────────────

    /// Return all recorded frame timings.
    pub fn frame_timings(&self) -> &[FrameTiming] {
        self.timing.timings()
    }

    /// Compute aggregated benchmark results from collected frame timings.
    pub fn bench_results(&self, scenario: &str, wall_time: Duration) -> BenchResults {
        BenchResults::from_timings(scenario, self.timing.timings(), wall_time)
    }

    /// Return the total number of completed frames.
    pub fn frame_count(&self) -> u64 {
        self.timing.frame_count()
    }

    /// Reset all frame timing data.
    pub fn reset_timing(&mut self) {
        self.timing.reset();
    }

    // ── Lifecycle ────────────────────────────────────────────────────

    /// Send 'q' and wait for the child process to exit (5s timeout, then kill).
    pub fn quit(&mut self) -> Result<()> {
        self.pty.quit()
    }

    /// Wait without collapsing exit, pending-status, liveness, or poll errors.
    /// Returns [`PtyExitPoll::PendingStatus`] immediately for an already-exited
    /// child and [`PtyExitPoll::Running`] only when the live-child deadline expires.
    pub fn wait_exit_code(&mut self, timeout: Duration) -> Result<PtyExitPoll<u32>> {
        self.pty.wait_exit_code(timeout)
    }

    /// Wait for child exit, then drain final PTY output through EOF or quiet.
    ///
    /// `exit_timeout` applies only until exit. Once exit is observed, the known
    /// status is preserved while a separate bounded drain phase runs.
    pub fn wait_for_exit_and_drain(
        &mut self,
        exit_timeout: Duration,
        drain_timeout: Duration,
    ) -> Result<u32> {
        let exit_deadline = Instant::now() + exit_timeout;
        let exit_code = loop {
            let exit = self.pty.poll_exit_code()?;
            if let PtyExitPoll::Exited(code) = exit {
                break code;
            }
            let remaining = exit_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if exit == PtyExitPoll::PendingStatus {
                    anyhow::bail!(
                        "exit observed but status unavailable after {exit_timeout:?}\n\
                         process tree: {}\nscreen contents:\n{}\nraw output:\n{}",
                        self.pty.process_tree_diagnostics(),
                        self.screen.contents(),
                        String::from_utf8_lossy(&self.raw_output)
                    );
                }
                anyhow::bail!(
                    "timed out after {exit_timeout:?} waiting for child exit\n\
                     process running: true\nprocess tree: {}\nscreen contents:\n{}\nraw output:\n{}",
                    self.pty.process_tree_diagnostics(),
                    self.screen.contents(),
                    String::from_utf8_lossy(&self.raw_output)
                );
            }
            self.update(Duration::from_millis(50).min(remaining));
        };

        let drain_deadline = Instant::now() + drain_timeout;
        let mut last_output_at = Instant::now();
        loop {
            let remaining = drain_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(exit_code);
            }
            match self.pump_one(Duration::from_millis(50).min(remaining)) {
                PtyPump::Chunk => last_output_at = Instant::now(),
                PtyPump::Closed => return Ok(exit_code),
                PtyPump::Timeout if last_output_at.elapsed() >= Duration::from_millis(200) => {
                    return Ok(exit_code);
                }
                PtyPump::Timeout => {}
            }
        }
    }

    /// Child PID (see [`PtyController::child_pid`]).
    pub fn child_pid(&self) -> Option<u32> {
        self.pty.child_pid()
    }

    /// Process-group/job enrollment state for failure diagnostics.
    pub fn process_tree_diagnostics(&self) -> String {
        self.pty.process_tree_diagnostics()
    }

    /// Deliver a signal to the child (unix). See [`PtyController::send_signal`].
    #[cfg(unix)]
    pub fn send_signal(&self, signal: i32) -> Result<()> {
        self.pty.send_signal(signal)
    }
}
