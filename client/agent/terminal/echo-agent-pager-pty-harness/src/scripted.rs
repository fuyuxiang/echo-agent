//! Scripted TUI scenario runner for echo-agent-pager.
//!
//! This layer lets UI regression tests describe a scenario declaratively, run
//! the real pager binary in a PTY, interact with it through keyboard input and
//! resizes, assert observable terminal output, and persist visual artifacts for
//! bug triage.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    AgentTurnExpectation, ContentController, PtyHarness, StyledLine, pager_binary, parse_keys,
};

const SGR_LEFT_BUTTON: u16 = 0;
const SGR_MIDDLE_BUTTON: u16 = 1;
const SGR_RIGHT_BUTTON: u16 = 2;
const SGR_DRAG_BUTTON: u16 = 32;
/// SGR wheel button codes (bit 6 / +64 marks a wheel event). Public so PTY
/// tests share this single definition instead of respelling 64/65.
pub const SGR_SCROLL_UP: u16 = 64;
pub const SGR_SCROLL_DOWN: u16 = 65;

const DEFAULT_ROWS: u16 = 50;
const DEFAULT_COLS: u16 = 120;
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 15_000;
const EXPECTATION_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Declarative scenario consumed by [`ScriptedScenarioRunner`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedScenario {
    /// Stable scenario name used for artifact directories and reports.
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub environment: EnvironmentConfig,
    /// Optional ephemeral workspace materialized into a temp dir and used as the
    /// pager's cwd. Lets a scenario exercise repo-local behavior — e.g. the
    /// folder-trust prompt, which only renders when `cwd` has a repo-local
    /// config (`.mcp.json`). `None` inherits the test process cwd.
    #[serde(default)]
    pub workspace: Option<WorkspaceConfig>,
    #[serde(default)]
    pub mock: MockConfig,
    #[serde(default)]
    pub steps: Vec<ScenarioStep>,
}

impl ScriptedScenario {
    /// Load a scenario from JSON.
    pub fn from_json_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read scenario file {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parse scenario JSON {}", path.display()))
    }

    /// Load a scenario from YAML.
    pub fn from_yaml_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read scenario file {}", path.display()))?;
        serde_yaml::from_str(&text)
            .with_context(|| format!("parse scenario YAML {}", path.display()))
    }

    /// Load a scenario from JSON or YAML based on file extension.
    pub fn from_file(path: &Path) -> Result<Self> {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("yaml" | "yml") => Self::from_yaml_file(path),
            _ => Self::from_json_file(path),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("scenario name must not be empty");
        }
        if self.steps.is_empty() {
            bail!("scenario {} has no steps", self.name);
        }
        Ok(())
    }
}

/// Terminal dimensions for a scripted run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalConfig {
    #[serde(default = "default_rows")]
    pub rows: u16,
    #[serde(default = "default_cols")]
    pub cols: u16,
    /// Answer terminal device queries (cursor-position reports for `ESC[6n`,
    /// etc.) from the embedded vt100 emulator, like a real terminal would.
    /// Off by default — most scenarios don't need it. Required for `--minimal`
    /// scenarios: the inline viewport's startup cursor-position probe otherwise
    /// times out and `--minimal` silently downgrades to full-height inline
    /// (see `PtyHarness::set_respond_to_queries`).
    #[serde(default)]
    pub respond_to_queries: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            respond_to_queries: false,
        }
    }
}

/// Environment filters and extra environment variables for a scenario.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvironmentConfig {
    /// Optional OS allow-list. Values are Rust `std::env::consts::OS` strings
    /// such as `macos`, `linux`, or `windows`.
    #[serde(default)]
    pub os: Vec<String>,
    /// Optional architecture allow-list. Values are
    /// `std::env::consts::ARCH` strings such as `aarch64` or `x86_64`.
    #[serde(default)]
    pub arch: Vec<String>,
    /// Additional env vars set on the pager process.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Extra CLI args passed to the pager binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional `config.toml` written into the run's isolated `$ECHO_AGENT_HOME`
    /// before spawn (e.g. `[ui] keep_text_selection` so selection
    /// highlights survive long enough to assert on).
    #[serde(default)]
    pub config_toml: Option<String>,
}

impl EnvironmentConfig {
    fn matches_current(&self) -> bool {
        (self.os.is_empty() || self.os.iter().any(|os| os == std::env::consts::OS))
            && (self.arch.is_empty() || self.arch.iter().any(|arch| arch == std::env::consts::ARCH))
    }
}

/// One environment variable assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

/// Ephemeral workspace materialized for a scenario run: a temp dir seeded with
/// declared files and optionally `git init`-ed, then used as the pager's cwd.
/// The folder-trust prompt, for example, only renders when `cwd` contains a
/// repo-local config (`.mcp.json`), so a scenario can declare one here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Run `git init` so the temp dir resolves as a git repository root — the
    /// scope repo-local config discovery and folder-trust use to bound a repo.
    #[serde(default)]
    pub git_init: bool,
    /// Files to create in the workspace, keyed by path relative to its root.
    /// Parent directories are created as needed.
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
}

/// Mock server configuration used to drive assistant output into the pager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockConfig {
    #[serde(default = "default_mock_response")]
    pub response: String,
    /// Required per-agent-turn responses, registered as ordered foreground
    /// expectations on both supported pager inference backends. Every listed
    /// turn must be satisfied before the runner reports success. Lets a
    /// scenario give each turn a distinct sentinel, e.g. to prove a transcript
    /// tail was truncated and re-generated. Falls back to `response` when
    /// exhausted.
    #[serde(default)]
    pub turns: Vec<String>,
    #[serde(default)]
    pub fixture_images: Vec<ImageFixture>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageFixture {
    pub name: String,
    #[serde(default = "default_fixture_extension")]
    pub extension: String,
    #[serde(default)]
    pub kind: ImageFixtureKind,
}

/// Variant of synthetic image to write for an [`ImageFixture`].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageFixtureKind {
    /// 8x8 RGBA PNG — meets the minimum vision-model dimension requirement.
    #[default]
    Standard,
    /// 1x1 RGBA PNG — below the 8 px minimum; rejected client-side.
    #[serde(rename = "tiny_1x1")]
    Tiny1x1,
    /// Valid 64x64 PNG header with a clobbered IDAT CRC; full decode rejects.
    CrcCorruptPng,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            response: default_mock_response(),
            turns: Vec::new(),
            fixture_images: Vec::new(),
        }
    }
}

/// A single executable scenario step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ScenarioStep {
    /// Wait until the screen contains text.
    WaitForText {
        text: String,
        #[serde(default = "default_wait_timeout_ms")]
        timeout_ms: u64,
    },
    /// Assert text is present on the current screen.
    AssertContains { text: String },
    /// Assert text is absent from the current screen.
    AssertNotContains { text: String },
    /// Type literal text into the TUI.
    TypeText { text: String },
    /// Inject keys using echo-agent-pty notation, for example `<Enter>`, `<Esc>`, `jj`.
    Keys { keys: String },
    /// Paste text using bracketed paste sequences.
    Paste { text: String },
    /// Paste fixture image path(s), using the same path parser as terminal drag/drop.
    PasteImagePaths { images: Vec<String> },
    /// Paste fixture image file:// URL(s), matching Finder-style file URL payloads.
    PasteImageFileUrls { images: Vec<String> },
    /// Paste fixture image path(s) with shell escaping for spaces/special chars.
    PasteEscapedImagePaths { images: Vec<String> },
    /// Focus the prompt input by locating the rendered prompt box.
    FocusPrompt,
    /// Paste into the prompt using semantic prompt focus instead of coordinates.
    PastePrompt { text: String },
    /// Simulate copying an image to the OS clipboard, then pressing paste.
    PasteClipboardImage { image: String },
    /// Drop text into the prompt using semantic prompt focus instead of coordinates.
    DropTextPrompt { text: String },
    /// Simulate a mouse click. Coordinates are 0-indexed.
    MouseClick {
        row: u16,
        col: u16,
        #[serde(default)]
        button: MouseButton,
    },
    /// Simulate a double-click at one coordinate.
    DoubleClick { row: u16, col: u16 },
    /// Simulate a triple-click at one coordinate.
    TripleClick { row: u16, col: u16 },
    /// Simulate mouse-wheel scrolling at a coordinate.
    Scroll {
        row: u16,
        col: u16,
        direction: ScrollDirection,
        #[serde(default = "default_scroll_count")]
        count: u16,
    },
    /// Scroll over a semantic target instead of coordinates.
    ScrollAt {
        target: TargetLocator,
        direction: ScrollDirection,
        #[serde(default = "default_scroll_count")]
        count: u16,
    },
    /// Simulate a primary-button mouse drag. Coordinates are 0-indexed.
    Drag { from: MousePoint, to: MousePoint },
    /// Alias for drag used by text-selection scenarios.
    SelectText { from: MousePoint, to: MousePoint },
    /// Click visible text by finding it on screen.
    ClickText {
        text: String,
        #[serde(default)]
        occurrence: usize,
        #[serde(default)]
        button: MouseButton,
    },
    /// Double-click visible text by finding it on screen.
    DoubleClickText {
        text: String,
        #[serde(default)]
        occurrence: usize,
    },
    /// Triple-click visible text by finding it on screen.
    TripleClickText {
        text: String,
        #[serde(default)]
        occurrence: usize,
    },
    /// Select a visible text range by locating both endpoints on screen.
    /// `to_offset_cols` shifts the drag's end column right (clamped at the
    /// screen edge), e.g. onto a table border when exercising dead-zones.
    SelectTextRange {
        from_text: String,
        to_text: String,
        #[serde(default)]
        to_offset_cols: u16,
    },
    /// Simulate dropping text at a position with click + bracketed paste.
    DropText { row: u16, col: u16, text: String },
    /// Simulate dropping text at a semantic target.
    DropTextAt { target: TargetLocator, text: String },
    /// Simulate drag/drop of image files onto the prompt as terminal paste payload.
    DropImagesPrompt { images: Vec<String> },
    /// Trigger a copy shortcut/key sequence from the TUI.
    Copy {
        #[serde(default = "default_copy_keys")]
        keys: String,
    },
    /// Assert the captured terminal output contains an OSC 52 clipboard write.
    AssertOsc52Contains { text: String },
    /// Assert NO OSC 52 clipboard payload contains `text` (negative of
    /// [`AssertOsc52Contains`]). Used to prove a label prefix was excluded
    /// from a selection/copy, e.g. that copying a Read tool header yields the
    /// path alone rather than the full `Read {path}` line.
    AssertOsc52NotContains { text: String },
    /// Assert the contiguous highlighted run at the first occurrence of
    /// `at_text` renders `equals`, untrimmed, over one uniform background.
    /// Needs a persistent `keep_text_selection` via `config_toml` (flash
    /// clears in ~150ms).
    AssertHighlightRun { at_text: String, equals: String },
    /// Assert no screen cell rendering the first occurrence of `text` has a
    /// non-default background (the text is outside any selection highlight).
    AssertTextNotHighlighted { text: String },
    /// Assert the captured **raw** PTY output contains at least `min` Kitty
    /// graphics APC sequences (`\x1b_G`). Mirrors [`AssertOsc52Contains`]: the
    /// graphics escapes are written into the synchronized-update frame buffer,
    /// outside the vt100 cell grid, so a screen-text snapshot can't see them —
    /// this scans the raw bytes instead. Proves an inline diagram was emitted.
    AssertKittyGraphics {
        #[serde(default = "default_assert_min")]
        min: usize,
    },
    /// Poll the raw PTY output until at least `min` Kitty graphics APC sequences
    /// appear (or `timeout_ms` expires). Preferred over a fixed `wait` before
    /// [`AssertKittyGraphics`]: it returns as soon as the diagram is placed and
    /// doesn't flake under load.
    WaitForKittyGraphics {
        #[serde(default = "default_assert_min")]
        min: usize,
        #[serde(default = "default_wait_timeout_ms")]
        timeout_ms: u64,
    },
    /// Assert the captured **raw** PTY output contains NO Kitty graphics APC
    /// sequences. The inverse of [`AssertKittyGraphics`]: proves a feature (e.g.
    /// a Mermaid diagram) was rendered as text, never transmitted as an inline
    /// image.
    AssertNoKittyGraphics {},
    /// Assert that a submitted request included at least this many image payloads.
    AssertRequestImageCount { min: usize },
    /// Assert every submitted image has inline bytes, the expected MIME type, and decodes.
    AssertInlineImages {
        min: usize,
        #[serde(default = "default_expected_image_mime")]
        mime_type: String,
        #[serde(default = "default_image_width")]
        width: DimensionAssertion,
        #[serde(default = "default_image_height")]
        height: DimensionAssertion,
    },
    /// Assert at least one request body contains the literal `text`.
    AssertRequestContains {
        text: String,
        /// Optional — restrict to the Nth most recent request body (0 = newest).
        #[serde(default)]
        request_index: Option<usize>,
    },
    /// Assert no `*.tmp` files remain anywhere under the mock content home directory.
    AssertNoTempArtifacts {},

    /// Resize the terminal.
    Resize { rows: u16, cols: u16 },
    /// Drain output for a fixed period.
    Wait { millis: u64 },
    /// Capture text, HTML, SVG, and JSON artifacts for the current screen.
    Screenshot {
        name: String,
        #[serde(default)]
        note: String,
    },
    /// Assert the pager process is still alive.
    AssertRunning,
    /// Assert the mock server received at least one chat completion request.
    AssertChatCompletion,
}

/// Dimension assertion for [`ScenarioStep::AssertInlineImages`]: either an
/// exact pixel count (`width: 28`) or a range (`width: { min: 28 }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DimensionAssertion {
    Exact(u32),
    Range {
        #[serde(default)]
        min: Option<u32>,
        #[serde(default)]
        max: Option<u32>,
    },
}

impl DimensionAssertion {
    fn matches(&self, actual: u32) -> bool {
        match self {
            DimensionAssertion::Exact(v) => actual == *v,
            DimensionAssertion::Range { min, max } => {
                min.is_none_or(|m| actual >= m) && max.is_none_or(|m| actual <= m)
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            DimensionAssertion::Exact(v) => v.to_string(),
            DimensionAssertion::Range { min, max } => match (min, max) {
                (Some(lo), Some(hi)) => format!("[{lo}..={hi}]"),
                (Some(lo), None) => format!("[>={lo}]"),
                (None, Some(hi)) => format!("[<={hi}]"),
                (None, None) => "[any]".to_owned(),
            },
        }
    }
}

/// A 0-indexed terminal coordinate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MousePoint {
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
}

/// Semantic locator for UI targets that should not require hard-coded coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetLocator {
    Prompt,
    Text {
        text: String,
        #[serde(default)]
        occurrence: usize,
    },
    Point {
        row: u16,
        col: u16,
    },
}

/// Runner configuration for scripted scenarios.
#[derive(Debug, Clone)]
pub struct ScriptedRunConfig {
    pub binary: PathBuf,
    pub artifact_dir: PathBuf,
}

impl ScriptedRunConfig {
    pub fn new(binary: PathBuf, artifact_dir: PathBuf) -> Self {
        Self {
            binary,
            artifact_dir,
        }
    }

    pub fn auto(artifact_dir: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self::new(pager_binary()?, artifact_dir.into()))
    }
}

/// Runs declarative scenarios and writes regression artifacts.
pub struct ScriptedScenarioRunner {
    config: ScriptedRunConfig,
}

impl ScriptedScenarioRunner {
    pub fn new(config: ScriptedRunConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self, scenario: &ScriptedScenario) -> Result<ScriptedRunReport> {
        scenario.validate()?;
        let started_at_ms = epoch_ms();
        let run_dir = self
            .config
            .artifact_dir
            .join(safe_name(&scenario.name))
            .join(started_at_ms.to_string());
        fs::create_dir_all(&run_dir)
            .with_context(|| format!("create artifact directory {}", run_dir.display()))?;

        let fixture_paths = prepare_image_fixtures(&run_dir, &scenario.mock.fixture_images)?;
        let mut report = ScriptedRunReport::new(scenario, &run_dir, started_at_ms);
        if !scenario.environment.matches_current() {
            report.status = ScriptedRunStatus::Skipped;
            report.skip_reason = Some(format!(
                "current platform {}/{} is outside scenario environment filters",
                std::env::consts::OS,
                std::env::consts::ARCH
            ));
            report.write(&run_dir)?;
            return Ok(report);
        }

        let content = ContentController::start()
            .await
            .context("start mock content")?;
        content.set_response(&scenario.mock.response);
        let turn_expectations: Vec<_> = scenario
            .mock
            .turns
            .iter()
            .enumerate()
            .map(|(index, turn)| {
                content.expect_agent_turn(format!("scenario turn {}", index + 1), turn)
            })
            .collect();

        if let Some(config_toml) = &scenario.environment.config_toml {
            let echo_agent_home = content.home().join(".echo-agent");
            fs::create_dir_all(&echo_agent_home).with_context(|| {
                format!(
                    "create scenario ECHO_AGENT_HOME {}",
                    echo_agent_home.display()
                )
            })?;
            fs::write(echo_agent_home.join("config.toml"), config_toml)
                .context("write scenario config.toml")?;
        }

        let env_refs: Vec<(&str, &str)> = scenario
            .environment
            .env
            .iter()
            .map(|v| (v.key.as_str(), v.value.as_str()))
            .collect();
        let args: Vec<&str> = scenario
            .environment
            .args
            .iter()
            .map(String::as_str)
            .collect();

        // Materialize an optional ephemeral workspace (temp dir + files + git
        // init) and run the pager there. Bound for the whole run so the dir
        // outlives the pager process; `None` inherits the test process cwd.
        let workspace_dir = match scenario.workspace.as_ref() {
            Some(ws) => Some(materialize_workspace(ws, content.sandbox())?),
            None => None,
        };
        let workspace_cwd = workspace_dir.as_ref().map(|dir| dir.path());

        let mut harness = PtyHarness::new_in_sandbox(
            &self.config.binary,
            scenario.terminal.rows,
            scenario.terminal.cols,
            &args,
            content.sandbox(),
            &env_refs,
            workspace_cwd,
        )
        .with_context(|| format!("spawn pager binary {}", self.config.binary.display()))?;
        harness.set_respond_to_queries(scenario.terminal.respond_to_queries);

        for (index, step) in scenario.steps.iter().enumerate() {
            let step_number = index + 1;
            match run_step(
                &mut harness,
                &content,
                &run_dir,
                &fixture_paths,
                step_number,
                step,
            ) {
                Ok(outcome) => {
                    report.artifacts.extend(outcome.artifacts.clone());
                    report.steps.push(outcome);
                }
                Err(error) => {
                    let bug = BugFinding {
                        step: step_number,
                        severity: BugSeverity::Bug,
                        message: error.to_string(),
                        screen_text: harness.screen_contents(),
                    };
                    report.bugs.push(bug);
                    report
                        .steps
                        .push(StepOutcome::failed(step_number, step, error.to_string()));
                    let raw_path = run_dir.join("raw_output.bin");
                    let _ = fs::write(&raw_path, harness.raw_output());
                    let _ = capture_artifacts(
                        &harness,
                        &run_dir,
                        step_number,
                        "failure",
                        "automatic capture after failed step",
                    )
                    .map(|artifacts| report.artifacts.extend(artifacts));
                    report.status = ScriptedRunStatus::Failed;
                    report.write(&run_dir)?;
                    write_bug_markdown(&run_dir, &report)?;
                    let _ = harness.quit();
                    return Ok(report);
                }
            }
        }

        if !harness.is_running()? {
            report.bugs.push(BugFinding {
                step: scenario.steps.len(),
                severity: BugSeverity::Bug,
                message: "pager exited before scenario completed".to_owned(),
                screen_text: harness.screen_contents(),
            });
            report.status = ScriptedRunStatus::Failed;
        }

        if report.status == ScriptedRunStatus::Running {
            let settle_deadline = Instant::now() + EXPECTATION_SETTLE_TIMEOUT;
            while turn_expectations
                .iter()
                .any(|expectation| !expectation.is_satisfied())
                && Instant::now() < settle_deadline
            {
                harness.update(Duration::from_millis(100));
            }
        }
        let unsatisfied_turns: Vec<_> = turn_expectations
            .iter()
            .filter(|expectation| !expectation.is_satisfied())
            .map(AgentTurnExpectation::diagnostic)
            .collect();
        if !unsatisfied_turns.is_empty() {
            report.bugs.push(BugFinding {
                step: scenario.steps.len(),
                severity: BugSeverity::Bug,
                message: format!(
                    "required mock.turns expectations were not satisfied:\n- {}",
                    unsatisfied_turns.join("\n- ")
                ),
                screen_text: harness.screen_contents(),
            });
            report.status = ScriptedRunStatus::Failed;
        }

        let _ = harness.quit();
        if report.status == ScriptedRunStatus::Running {
            report.status = ScriptedRunStatus::Passed;
        }
        // Persist raw PTY bytes for offline analysis (e.g. Kitty a=t vs a=p counts).
        let raw_path = run_dir.join("raw_output.bin");
        let _ = fs::write(&raw_path, harness.raw_output());
        report.write(&run_dir)?;
        write_bug_markdown(&run_dir, &report)?;
        Ok(report)
    }
}

/// Create a temp dir for a scenario [`WorkspaceConfig`]: write its files
/// (creating parent dirs) and optionally `git init` it. The returned `TempDir`
/// must be held for the whole run so the directory outlives the pager process.
fn materialize_workspace(
    workspace: &WorkspaceConfig,
    sandbox: &echo_agent_test_support::TestSandbox,
) -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir().context("create scenario workspace temp dir")?;
    for (rel_path, contents) in &workspace.files {
        // Fail closed: a `files` key must be a relative path that stays inside
        // the workspace. Reject absolute paths and any root/prefix/`..`
        // component so a scenario can never write outside the tempdir. This is
        // author-controlled test YAML (not a security boundary), but it's the
        // shared materialization path, so guard it.
        let rel = Path::new(rel_path);
        if rel.is_absolute()
            || rel.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            bail!("workspace file path must be relative and within the workspace: {rel_path}");
        }
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create workspace dir {}", parent.display()))?;
        }
        fs::write(&path, contents)
            .with_context(|| format!("write workspace file {}", path.display()))?;
    }
    if workspace.git_init {
        // A real repo root keeps repo-local discovery independent of the
        // system temp path.
        let mut cmd = sandbox.git_command();
        let output = cmd
            .args(["init", "-q"])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .context("run `git init` for scenario workspace")?;
        if !output.status.success() {
            bail!(
                "`git init` for scenario workspace failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    Ok(dir)
}

fn run_step(
    harness: &mut PtyHarness,
    content: &ContentController,
    run_dir: &Path,
    fixture_paths: &std::collections::HashMap<String, PathBuf>,
    step_number: usize,
    step: &ScenarioStep,
) -> Result<StepOutcome> {
    match step {
        ScenarioStep::WaitForText { text, timeout_ms } => harness
            .wait_for_text(text, Duration::from_millis(*timeout_ms))
            .with_context(|| format!("wait for text {text:?}"))?,
        ScenarioStep::AssertContains { text } => {
            if !harness.contains_text(text) {
                bail!("expected screen to contain {text:?}");
            }
        }
        ScenarioStep::AssertNotContains { text } => {
            if harness.contains_text(text) {
                bail!("expected screen not to contain {text:?}");
            }
        }
        ScenarioStep::TypeText { text } => {
            harness
                .inject_keys(text.as_bytes())
                .with_context(|| format!("type text {text:?}"))?;
            harness.update(Duration::from_millis(100));
        }
        ScenarioStep::Keys { keys } => {
            let bytes = parse_keys(keys).map_err(|e| anyhow!("parse keys {keys:?}: {e}"))?;
            harness
                .inject_keys(&bytes)
                .with_context(|| format!("inject keys {keys:?}"))?;
            harness.update(Duration::from_millis(100));
        }
        ScenarioStep::Paste { text } => {
            harness
                .inject_keys(bracketed_paste(text).as_bytes())
                .with_context(|| format!("paste {} bytes", text.len()))?;
            harness.update(Duration::from_millis(250));
        }
        ScenarioStep::PasteImagePaths { images } => {
            let text = image_path_payload(fixture_paths, images, PathPayloadStyle::Plain)?;
            paste_prompt_text(harness, &text).context("paste image path payload")?;
        }
        ScenarioStep::PasteImageFileUrls { images } => {
            let text = image_path_payload(fixture_paths, images, PathPayloadStyle::FileUrl)?;
            paste_prompt_text(harness, &text).context("paste image file URL payload")?;
        }
        ScenarioStep::PasteEscapedImagePaths { images } => {
            let text = image_path_payload(fixture_paths, images, PathPayloadStyle::Escaped)?;
            paste_prompt_text(harness, &text).context("paste escaped image path payload")?;
        }
        ScenarioStep::FocusPrompt => {
            click_point(harness, locate_prompt(harness)?, MouseButton::Left)
                .context("focus prompt")?;
        }
        ScenarioStep::PastePrompt { text } => {
            paste_prompt_text(harness, text).context("paste prompt text")?;
        }
        ScenarioStep::PasteClipboardImage { image } => {
            let text = image_path_payload(
                fixture_paths,
                std::slice::from_ref(image),
                PathPayloadStyle::Plain,
            )?;
            paste_prompt_text(harness, &text).context("paste clipboard image fixture")?;
        }
        ScenarioStep::DropTextPrompt { text } => {
            click_point(
                harness,
                locate_prompt_drop_point(harness)?,
                MouseButton::Left,
            )
            .context("focus prompt before drop text")?;
            harness
                .inject_keys(bracketed_paste(text).as_bytes())
                .with_context(|| format!("drop text {} bytes into prompt", text.len()))?;
            harness.update(Duration::from_millis(250));
        }
        ScenarioStep::MouseClick { row, col, button } => {
            harness
                .inject_keys(mouse_click_bytes(*row, *col, *button).as_bytes())
                .with_context(|| format!("mouse click at row={row} col={col}"))?;
            harness.update(Duration::from_millis(100));
        }
        ScenarioStep::DoubleClick { row, col } => {
            harness
                .inject_keys(repeated_click_bytes(*row, *col, 2).as_bytes())
                .with_context(|| format!("double click at row={row} col={col}"))?;
            harness.update(Duration::from_millis(150));
        }
        ScenarioStep::TripleClick { row, col } => {
            harness
                .inject_keys(repeated_click_bytes(*row, *col, 3).as_bytes())
                .with_context(|| format!("triple click at row={row} col={col}"))?;
            harness.update(Duration::from_millis(150));
        }
        ScenarioStep::Scroll {
            row,
            col,
            direction,
            count,
        } => {
            harness
                .inject_keys(mouse_scroll_bytes(*row, *col, *direction, *count).as_bytes())
                .with_context(|| format!("mouse scroll {direction:?} at row={row} col={col}"))?;
            harness.update(Duration::from_millis(150));
        }
        ScenarioStep::ScrollAt {
            target,
            direction,
            count,
        } => {
            let point = locate_target(harness, target)?;
            harness
                .inject_keys(
                    mouse_scroll_bytes(point.row, point.col, *direction, *count).as_bytes(),
                )
                .with_context(|| format!("mouse scroll {direction:?} at {point:?}"))?;
            harness.update(Duration::from_millis(150));
        }
        ScenarioStep::Drag { from, to } | ScenarioStep::SelectText { from, to } => {
            harness
                .inject_keys(mouse_drag_bytes(*from, *to).as_bytes())
                .with_context(|| format!("drag from {from:?} to {to:?}"))?;
            harness.update(Duration::from_millis(100));
        }
        ScenarioStep::ClickText {
            text,
            occurrence,
            button,
        } => {
            click_point(harness, locate_text(harness, text, *occurrence)?, *button)
                .with_context(|| format!("click text {text:?}"))?;
        }
        ScenarioStep::DoubleClickText { text, occurrence } => {
            let point = locate_text(harness, text, *occurrence)?;
            harness
                .inject_keys(repeated_click_bytes(point.row, point.col, 2).as_bytes())
                .with_context(|| format!("double-click text {text:?}"))?;
            harness.update(Duration::from_millis(150));
        }
        ScenarioStep::TripleClickText { text, occurrence } => {
            let point = locate_text(harness, text, *occurrence)?;
            harness
                .inject_keys(repeated_click_bytes(point.row, point.col, 3).as_bytes())
                .with_context(|| format!("triple-click text {text:?}"))?;
            harness.update(Duration::from_millis(150));
        }
        ScenarioStep::SelectTextRange {
            from_text,
            to_text,
            to_offset_cols,
        } => {
            let from = locate_text(harness, from_text, 0)?;
            let mut to = locate_text_end(harness, to_text, 0)?;
            if to.col == 0 {
                to.col = 1;
            }
            to.col = to
                .col
                .saturating_add(*to_offset_cols)
                .min(harness.screen_output().size.cols.saturating_sub(1) as u16);
            harness
                .inject_keys(mouse_drag_bytes(from, to).as_bytes())
                .with_context(|| format!("select text range {from_text:?}..{to_text:?}"))?;
            harness.update(Duration::from_millis(150));
        }
        ScenarioStep::DropText { row, col, text } => {
            harness
                .inject_keys(mouse_click_bytes(*row, *col, MouseButton::Left).as_bytes())
                .with_context(|| format!("drop click at row={row} col={col}"))?;
            harness.update(Duration::from_millis(100));
            harness
                .inject_keys(bracketed_paste(text).as_bytes())
                .with_context(|| format!("drop text {} bytes", text.len()))?;
            harness.update(Duration::from_millis(250));
        }
        ScenarioStep::DropTextAt { target, text } => {
            click_point(harness, locate_target(harness, target)?, MouseButton::Left)
                .context("focus drop target")?;
            harness
                .inject_keys(bracketed_paste(text).as_bytes())
                .with_context(|| format!("drop text {} bytes", text.len()))?;
            harness.update(Duration::from_millis(250));
        }
        ScenarioStep::DropImagesPrompt { images } => {
            let text = image_path_payload(fixture_paths, images, PathPayloadStyle::Plain)?;
            click_point(
                harness,
                locate_prompt_drop_point(harness)?,
                MouseButton::Left,
            )
            .context("focus prompt before image drop")?;
            harness
                .inject_keys(bracketed_paste(&text).as_bytes())
                .with_context(|| format!("drop image path payload {} bytes", text.len()))?;
            harness.update(Duration::from_millis(500));
        }
        ScenarioStep::Copy { keys } => {
            let bytes = parse_keys(keys).map_err(|e| anyhow!("parse copy keys {keys:?}: {e}"))?;
            harness
                .inject_keys(&bytes)
                .with_context(|| format!("inject copy keys {keys:?}"))?;
            harness.update(Duration::from_millis(250));
        }
        ScenarioStep::AssertOsc52Contains { text } => {
            let clipboards = decode_osc52_payloads(harness.raw_output())?;
            if !clipboards.iter().any(|payload| payload.contains(text)) {
                bail!(
                    "expected OSC 52 clipboard payload to contain {text:?}; decoded payloads: {clipboards:?}"
                );
            }
        }
        ScenarioStep::AssertOsc52NotContains { text } => {
            let clipboards = decode_osc52_payloads(harness.raw_output())?;
            if clipboards.iter().any(|payload| payload.contains(text)) {
                bail!(
                    "expected no OSC 52 clipboard payload to contain {text:?}; decoded payloads: {clipboards:?}"
                );
            }
        }
        ScenarioStep::AssertHighlightRun { at_text, equals } => {
            let point = locate_text(harness, at_text, 0)?;
            let cells = row_char_bgs(harness, point.row)
                .with_context(|| format!("styled row {} for {at_text:?}", point.row))?;
            // Highlighted = differs from the row's dominant background; the
            // hovered-entry wash tints whole rows, so bg-set alone is wrong.
            let dominant = dominant_bg(&cells);
            let col = point.col as usize;
            let highlighted = |idx: usize| cells.get(idx).is_some_and(|(_, bg)| *bg != dominant);
            if !highlighted(col) {
                bail!(
                    "expected {at_text:?} (row {}, col {}) to be inside a selection highlight",
                    point.row,
                    point.col
                );
            }
            let mut start = col;
            while start > 0 && highlighted(start - 1) {
                start -= 1;
            }
            let mut end = col + 1;
            while highlighted(end) {
                end += 1;
            }
            // Untrimmed: a highlight claiming padding or borders must fail.
            let run: String = cells[start..end].iter().map(|(ch, _)| *ch).collect();
            if run != *equals {
                bail!("highlight run at {at_text:?} renders {run:?}, expected {equals:?}");
            }
            // The run must be one uniform band, styled spans included.
            let bgs: std::collections::BTreeSet<_> =
                cells[start..end].iter().map(|(_, bg)| bg.clone()).collect();
            if bgs.len() > 1 {
                bail!("highlight run at {at_text:?} is not uniform: backgrounds {bgs:?}");
            }
        }
        ScenarioStep::AssertTextNotHighlighted { text } => {
            let point = locate_text(harness, text, 0)?;
            let cells = row_char_bgs(harness, point.row)
                .with_context(|| format!("styled row {} for {text:?}", point.row))?;
            let dominant = dominant_bg(&cells);
            let start = point.col as usize;
            let end = (start + text.chars().count()).min(cells.len());
            for (idx, (_, bg)) in cells.iter().enumerate().take(end).skip(start) {
                if *bg != dominant {
                    bail!(
                        "expected {text:?} to be outside any selection highlight, but cell {idx} on row {} has background {bg:?} (row dominant: {dominant:?})",
                        point.row
                    );
                }
            }
        }
        ScenarioStep::AssertKittyGraphics { min } => {
            let count = count_kitty_graphics(harness.raw_output());
            if count < *min {
                bail!("expected at least {min} Kitty graphics escape(s), found {count}");
            }
        }
        ScenarioStep::WaitForKittyGraphics { min, timeout_ms } => harness
            .wait_for_kitty_graphics(*min, Duration::from_millis(*timeout_ms))
            .with_context(|| format!("wait for {min} Kitty graphics escape(s)"))?,
        ScenarioStep::AssertNoKittyGraphics {} => {
            let count = count_kitty_graphics(harness.raw_output());
            if count > 0 {
                bail!("expected no Kitty graphics escapes, found {count}");
            }
        }
        ScenarioStep::AssertRequestImageCount { min } => {
            let count = count_request_images(&content.request_bodies());
            if count < *min {
                bail!("expected at least {min} request image(s), got {count}");
            }
        }
        ScenarioStep::AssertInlineImages {
            min,
            mime_type,
            width,
            height,
        } => assert_inline_images(&content.request_bodies(), *min, mime_type, width, height)?,
        ScenarioStep::AssertRequestContains {
            text,
            request_index,
        } => assert_request_contains(&content.request_bodies(), text, *request_index)?,
        ScenarioStep::AssertNoTempArtifacts {} => assert_no_temp_artifacts(content.home())?,
        ScenarioStep::Resize { rows, cols } => harness
            .resize(*rows, *cols)
            .with_context(|| format!("resize to {rows}x{cols}"))?,
        ScenarioStep::Wait { millis } => harness.update(Duration::from_millis(*millis)),
        ScenarioStep::Screenshot { name, note } => {
            let artifacts = capture_artifacts(harness, run_dir, step_number, name, note)?;
            return Ok(StepOutcome::passed_with_artifacts(
                step_number,
                step,
                artifacts,
            ));
        }
        ScenarioStep::AssertRunning => {
            if !harness.is_running()? {
                bail!("pager process is not running");
            }
        }
        ScenarioStep::AssertChatCompletion => {
            if !content.has_chat_completion() {
                bail!("mock server did not receive a chat completion request");
            }
        }
    }
    Ok(StepOutcome::passed(step_number, step))
}

fn capture_artifacts(
    harness: &PtyHarness,
    run_dir: &Path,
    step_number: usize,
    name: &str,
    note: &str,
) -> Result<Vec<VisualArtifact>> {
    let basename = format!("{:02}-{}", step_number, safe_name(name));
    let text_path = run_dir.join(format!("{basename}.txt"));
    let html_path = run_dir.join(format!("{basename}.html"));
    let svg_path = run_dir.join(format!("{basename}.svg"));
    let json_path = run_dir.join(format!("{basename}.json"));

    let text = harness.screen_contents();
    fs::write(&text_path, &text).with_context(|| format!("write {}", text_path.display()))?;

    let html = harness.screen_html();
    fs::write(&html_path, html).with_context(|| format!("write {}", html_path.display()))?;

    let styled = harness.screen_styled();
    let svg = render_svg(&styled, harness.cursor_position(), note);
    fs::write(&svg_path, svg).with_context(|| format!("write {}", svg_path.display()))?;

    let styled_json = serde_json::to_string_pretty(&styled).context("serialize styled screen")?;
    fs::write(&json_path, styled_json).with_context(|| format!("write {}", json_path.display()))?;

    Ok(vec![
        VisualArtifact::new("text", text_path),
        VisualArtifact::new("html", html_path),
        VisualArtifact::new("svg", svg_path),
        VisualArtifact::new("styled_json", json_path),
    ])
}

fn render_svg(lines: &[StyledLine], cursor: (u16, u16), note: &str) -> String {
    let cols = lines
        .iter()
        .flat_map(|line| line.runs.iter())
        .map(|run| run.text.chars().count())
        .max()
        .unwrap_or(1)
        .max(80);
    let rows = lines.len().max(1);
    let char_width = 8usize;
    let line_height = 18usize;
    let margin = 16usize;
    let note_height = if note.is_empty() { 0 } else { line_height + 8 };
    let width = cols * char_width + margin * 2;
    let height = rows * line_height + margin * 2 + note_height;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n<rect width=\"100%\" height=\"100%\" fill=\"#1e1e1e\"/>\n<style>text {{ font-family: Menlo, Monaco, Consolas, monospace; font-size: 14px; white-space: pre; }}</style>\n"
    );
    if !note.is_empty() {
        svg.push_str(&format!(
            "<text x=\"{margin}\" y=\"{}\" fill=\"#9cdcfe\">{}</text>\n",
            margin + 12,
            escape_xml(note)
        ));
    }
    let y_offset = margin + note_height;
    for (row, line) in lines.iter().enumerate() {
        let mut x = margin;
        let y = y_offset + row * line_height + 14;
        for run in &line.runs {
            let fill = run.fg.as_deref().unwrap_or("#d4d4d4");
            let mut attrs = format!("fill=\"{}\"", escape_attr(fill));
            if run.bold {
                attrs.push_str(" font-weight=\"700\"");
            }
            if run.italic {
                attrs.push_str(" font-style=\"italic\"");
            }
            if run.underline {
                attrs.push_str(" text-decoration=\"underline\"");
            }
            if let Some(bg) = &run.bg {
                let w = run.text.chars().count() * char_width;
                svg.push_str(&format!(
                    "<rect x=\"{x}\" y=\"{}\" width=\"{w}\" height=\"{line_height}\" fill=\"{}\"/>\n",
                    y - 14,
                    escape_attr(bg)
                ));
            }
            svg.push_str(&format!(
                "<text x=\"{x}\" y=\"{y}\" {attrs}>{}</text>\n",
                escape_xml(&run.text)
            ));
            x += run.text.chars().count() * char_width;
        }
    }
    let cursor_row = cursor.0 as usize;
    let cursor_col = cursor.1 as usize;
    svg.push_str(&format!(
        "<rect x=\"{}\" y=\"{}\" width=\"{char_width}\" height=\"{line_height}\" fill=\"none\" stroke=\"#ffffff\" stroke-width=\"1\"/>\n",
        margin + cursor_col * char_width,
        y_offset + cursor_row * line_height,
    ));
    svg.push_str("</svg>\n");
    svg
}

/// Machine-readable run report written to `report.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedRunReport {
    pub scenario: String,
    pub description: String,
    pub status: ScriptedRunStatus,
    pub skip_reason: Option<String>,
    pub os: String,
    pub arch: String,
    pub started_at_ms: u128,
    pub artifact_dir: PathBuf,
    pub steps: Vec<StepOutcome>,
    pub artifacts: Vec<VisualArtifact>,
    pub bugs: Vec<BugFinding>,
}

impl ScriptedRunReport {
    fn new(scenario: &ScriptedScenario, artifact_dir: &Path, started_at_ms: u128) -> Self {
        Self {
            scenario: scenario.name.clone(),
            description: scenario.description.clone(),
            status: ScriptedRunStatus::Running,
            skip_reason: None,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            started_at_ms,
            artifact_dir: artifact_dir.to_path_buf(),
            steps: Vec::new(),
            artifacts: Vec::new(),
            bugs: Vec::new(),
        }
    }

    fn write(&self, run_dir: &Path) -> Result<()> {
        let report_path = run_dir.join("report.json");
        let json = serde_json::to_string_pretty(self).context("serialize report")?;
        fs::write(&report_path, json).with_context(|| format!("write {}", report_path.display()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptedRunStatus {
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub step: usize,
    pub action: String,
    pub status: StepStatus,
    pub message: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<VisualArtifact>,
}

impl StepOutcome {
    fn passed(step: usize, action: &ScenarioStep) -> Self {
        Self {
            step,
            action: action_name(action).to_owned(),
            status: StepStatus::Passed,
            message: None,
            artifacts: Vec::new(),
        }
    }

    fn passed_with_artifacts(
        step: usize,
        action: &ScenarioStep,
        artifacts: Vec<VisualArtifact>,
    ) -> Self {
        Self {
            artifacts,
            ..Self::passed(step, action)
        }
    }

    fn failed(step: usize, action: &ScenarioStep, message: String) -> Self {
        Self {
            step,
            action: action_name(action).to_owned(),
            status: StepStatus::Failed,
            message: Some(message),
            artifacts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualArtifact {
    pub kind: String,
    pub path: PathBuf,
}

impl VisualArtifact {
    fn new(kind: impl Into<String>, path: PathBuf) -> Self {
        Self {
            kind: kind.into(),
            path,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugFinding {
    pub step: usize,
    pub severity: BugSeverity,
    pub message: String,
    pub screen_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugSeverity {
    Bug,
}

fn write_bug_markdown(run_dir: &Path, report: &ScriptedRunReport) -> Result<()> {
    let path = run_dir.join("bugs.md");
    let mut out = String::new();
    out.push_str(&format!("# TUI Scenario Report: {}\n\n", report.scenario));
    out.push_str(&format!("Status: {:?}\n\n", report.status));
    out.push_str(&format!("Platform: {}/{}\n\n", report.os, report.arch));
    if report.bugs.is_empty() {
        out.push_str("No bugs identified.\n");
    } else {
        out.push_str("## Bugs\n\n");
        for bug in &report.bugs {
            out.push_str(&format!(
                "- Step {}: {}\n\n```text\n{}\n```\n\n",
                bug.step, bug.message, bug.screen_text
            ));
        }
    }
    if !report.artifacts.is_empty() {
        out.push_str("## Visual artifacts\n\n");
        for artifact in &report.artifacts {
            out.push_str(&format!(
                "- {}: {}\n",
                artifact.kind,
                artifact.path.display()
            ));
        }
    }
    fs::write(&path, out).with_context(|| format!("write {}", path.display()))
}

fn prepare_image_fixtures(
    run_dir: &Path,
    fixtures: &[ImageFixture],
) -> Result<std::collections::HashMap<String, PathBuf>> {
    let dir = run_dir.join("fixtures");
    fs::create_dir_all(&dir).with_context(|| format!("create fixture dir {}", dir.display()))?;
    let mut out = std::collections::HashMap::new();
    for fixture in fixtures {
        let ext = fixture.extension.trim_start_matches('.');
        let path = dir.join(format!("{}.{}", fixture.name, ext));
        write_fixture_image(&path, fixture.kind)
            .with_context(|| format!("write image fixture {}", path.display()))?;
        let path = dunce::canonicalize(&path)
            .with_context(|| format!("canonicalize image fixture {}", path.display()))?;
        out.insert(fixture.name.clone(), path);
    }
    Ok(out)
}

fn write_fixture_image(path: &Path, kind: ImageFixtureKind) -> Result<()> {
    let bytes = match kind {
        ImageFixtureKind::Standard => standard_png_bytes()?,
        ImageFixtureKind::Tiny1x1 => tiny_1x1_png_bytes()?,
        ImageFixtureKind::CrcCorruptPng => crc_corrupt_png_bytes()?,
    };
    fs::write(path, bytes)?;
    Ok(())
}

fn standard_png_bytes() -> Result<Vec<u8>> {
    use image::{ImageBuffer, ImageFormat, Rgba};

    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(8, 8, Rgba([128, 64, 32, 255]));
    let mut png = Vec::new();
    buffer.write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}

fn tiny_1x1_png_bytes() -> Result<Vec<u8>> {
    use image::{ImageBuffer, ImageFormat, Rgba};

    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(1, 1, Rgba([200, 100, 50, 255]));
    let mut png = Vec::new();
    buffer.write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}

fn crc_corrupt_png_bytes() -> Result<Vec<u8>> {
    use image::{ImageBuffer, ImageFormat, Rgba};

    let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(64, 64, Rgba([10, 20, 30, 255]));
    let mut png = Vec::new();
    buffer.write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)?;

    // Locate the IDAT chunk: [4-byte length][IDAT][...data...][4-byte CRC].
    let idat = png
        .windows(4)
        .position(|w| w == b"IDAT")
        .ok_or_else(|| anyhow!("synthetic PNG missing IDAT chunk"))?;
    if idat < 4 {
        bail!("IDAT marker at offset {idat} has no length prefix");
    }
    let length_bytes = &png[idat - 4..idat];
    let data_len = u32::from_be_bytes([
        length_bytes[0],
        length_bytes[1],
        length_bytes[2],
        length_bytes[3],
    ]) as usize;
    let crc_start = idat + 4 + data_len;
    let crc_end = crc_start + 4;
    if crc_end > png.len() {
        bail!(
            "IDAT CRC at {crc_start}..{crc_end} exceeds PNG length {}",
            png.len()
        );
    }
    // Flip a single byte of the IDAT CRC. `image::load_from_memory` rejects the result.
    png[crc_start] ^= 0xFF;
    Ok(png)
}

fn fixture_path(
    fixture_paths: &std::collections::HashMap<String, PathBuf>,
    name: &str,
) -> Result<PathBuf> {
    fixture_paths
        .get(name)
        .cloned()
        .with_context(|| format!("unknown image fixture {name:?}"))
}

#[derive(Debug, Clone, Copy)]
enum PathPayloadStyle {
    Plain,
    FileUrl,
    Escaped,
}

fn image_path_payload(
    fixture_paths: &std::collections::HashMap<String, PathBuf>,
    images: &[String],
    style: PathPayloadStyle,
) -> Result<String> {
    images
        .iter()
        .map(|name| {
            let path = fixture_path(fixture_paths, name)?;
            let s = match style {
                PathPayloadStyle::Plain => path.display().to_string(),
                PathPayloadStyle::FileUrl => format!("file://{}", path.display()),
                PathPayloadStyle::Escaped => shell_escape_path(&path),
            };
            Ok(s)
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("\n"))
}

fn shell_escape_path(path: &Path) -> String {
    path.display()
        .to_string()
        .chars()
        .flat_map(|ch| match ch {
            ' ' | '(' | ')' | '[' | ']' | '\\' => vec!['\\', ch],
            _ => vec![ch],
        })
        .collect()
}

fn paste_prompt_text(harness: &mut PtyHarness, text: &str) -> Result<()> {
    click_point(harness, locate_prompt(harness)?, MouseButton::Left)
        .context("focus prompt before paste")?;
    harness
        .inject_keys(bracketed_paste(text).as_bytes())
        .with_context(|| format!("paste {} bytes into prompt", text.len()))?;
    harness.update(Duration::from_millis(500));
    Ok(())
}

fn count_request_images(bodies: &[serde_json::Value]) -> usize {
    request_images(bodies).len()
}

fn assert_inline_images(
    bodies: &[serde_json::Value],
    min: usize,
    expected_mime: &str,
    expected_width: &DimensionAssertion,
    expected_height: &DimensionAssertion,
) -> Result<()> {
    use base64::Engine as _;

    let images = request_images(bodies);
    if images.len() < min {
        bail!(
            "expected at least {min} inline image(s), got {}",
            images.len()
        );
    }
    for (idx, image) in images.iter().enumerate() {
        let (mime, data) = inline_image_data(image)
            .with_context(|| format!("image {idx} missing inline image data: {image}"))?;
        if mime != expected_mime {
            bail!("image {idx} mime mismatch: expected {expected_mime}, got {mime}");
        }
        if data.is_empty() {
            bail!("image {idx} has empty inline data");
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .with_context(|| format!("image {idx} data is not valid base64"))?;
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .with_context(|| format!("image {idx} format sniff failed"))?
            .into_dimensions()
            .with_context(|| format!("image {idx} did not decode"))?;
        if !expected_width.matches(w) {
            bail!(
                "image {idx} width {w} did not satisfy {}",
                expected_width.describe()
            );
        }
        if !expected_height.matches(h) {
            bail!(
                "image {idx} height {h} did not satisfy {}",
                expected_height.describe()
            );
        }
    }
    Ok(())
}

fn assert_request_contains(
    bodies: &[serde_json::Value],
    needle: &str,
    request_index: Option<usize>,
) -> Result<()> {
    if bodies.is_empty() {
        bail!("expected request body containing {needle:?}, but no requests were captured");
    }
    let haystacks: Vec<String> = match request_index {
        Some(idx) => {
            let total = bodies.len();
            if idx >= total {
                bail!(
                    "request_index {idx} out of range (only {total} request body/bodies captured)"
                );
            }
            let body = &bodies[total - 1 - idx];
            vec![serde_json::to_string(body).unwrap_or_default()]
        }
        None => bodies
            .iter()
            .map(|b| serde_json::to_string(b).unwrap_or_default())
            .collect(),
    };
    if haystacks.iter().any(|s| s.contains(needle)) {
        return Ok(());
    }
    bail!(
        "no request body contained {needle:?}; searched {} request(s)",
        haystacks.len()
    );
}

fn assert_no_temp_artifacts(home: &Path) -> Result<()> {
    let mut offenders = Vec::new();
    collect_tmp_files(home, &mut offenders)?;
    if !offenders.is_empty() {
        bail!(
            "found {} leftover *.tmp file(s) under {}: {:?}",
            offenders.len(),
            home.display(),
            offenders
        );
    }
    Ok(())
}

fn collect_tmp_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read_dir {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read_dir entry under {}", dir.display()))?;
        let ft = entry
            .file_type()
            .with_context(|| format!("file_type for {}", entry.path().display()))?;
        let path = entry.path();
        if ft.is_dir() {
            collect_tmp_files(&path, out)?;
        } else if ft.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn inline_image_data(image: &serde_json::Value) -> Option<(&str, &str)> {
    let mime = image
        .get("mime_type")
        .and_then(serde_json::Value::as_str)
        .or_else(|| image.get("mimeType").and_then(serde_json::Value::as_str));
    let data = image.get("data").and_then(serde_json::Value::as_str);
    if let (Some(mime), Some(data)) = (mime, data) {
        return Some((mime, data));
    }

    // The Responses API encodes the image as `image_url: "data:..."`
    // (string), while the legacy chat-completions shape uses
    // `image_url: { url: "data:..." }` (object). Accept both.
    let url = image
        .get("image_url")
        .and_then(|v| v.as_str().or_else(|| v.get("url").and_then(|u| u.as_str())))
        .or_else(|| image.get("imageUrl").and_then(serde_json::Value::as_str))?;
    let rest = url.strip_prefix("data:")?;
    let (mime, encoded) = rest.split_once(";base64,")?;
    Some((mime, encoded))
}

fn request_images(bodies: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    let mut images = Vec::new();
    for body in bodies {
        collect_images(body, &mut images);
    }
    images
}

fn collect_images<'a>(value: &'a serde_json::Value, images: &mut Vec<&'a serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            let is_image = map
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|ty| ty.eq_ignore_ascii_case("image") || ty.contains("image"))
                || map.contains_key("mime_type") && map.contains_key("data");
            if is_image {
                images.push(value);
            }
            for child in map.values() {
                collect_images(child, images);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_images(child, images);
            }
        }
        _ => {}
    }
}

fn locate_target(harness: &PtyHarness, target: &TargetLocator) -> Result<MousePoint> {
    match target {
        TargetLocator::Prompt => locate_prompt(harness),
        TargetLocator::Text { text, occurrence } => locate_text(harness, text, *occurrence),
        TargetLocator::Point { row, col } => Ok(MousePoint {
            row: *row,
            col: *col,
        }),
    }
}

fn locate_prompt(harness: &PtyHarness) -> Result<MousePoint> {
    let output = harness.screen_output();
    for (row, line) in output.lines.iter().enumerate().rev() {
        let Some(marker_byte) = line.find('❯') else {
            continue;
        };
        let col = line[..marker_byte].chars().count() + 2;
        return Ok(MousePoint {
            row: row as u16,
            col: col as u16,
        });
    }
    bail!(
        "could not locate prompt marker `❯` on screen\n{}",
        harness.screen_contents()
    )
}

fn locate_prompt_drop_point(harness: &PtyHarness) -> Result<MousePoint> {
    let output = harness.screen_output();
    for (row, line) in output.lines.iter().enumerate().rev() {
        let Some(marker_byte) = line.find('❯') else {
            continue;
        };
        let after_marker = &line[marker_byte + '❯'.len_utf8()..];
        let content_cols = after_marker.trim_end().chars().count();
        let marker_col = line[..marker_byte].chars().count();
        return Ok(MousePoint {
            row: row as u16,
            col: (marker_col + 2 + content_cols).min(line.chars().count().saturating_sub(1)) as u16,
        });
    }
    bail!(
        "could not locate prompt marker `❯` on screen\n{}",
        harness.screen_contents()
    )
}

/// Per-character backgrounds for a 0-indexed screen row (`None` = default),
/// char-indexed to match [`locate_text`]; wide-char spacers are skipped by
/// the styled extraction.
fn row_char_bgs(harness: &PtyHarness, row: u16) -> Option<Vec<(char, Option<String>)>> {
    let styled = harness.screen_styled();
    let line = styled.iter().find(|l| l.line == row as usize + 1)?;
    let mut cells = Vec::new();
    for run in &line.runs {
        for ch in run.text.chars() {
            cells.push((ch, run.bg.clone()));
        }
    }
    Some(cells)
}

/// The most common background on a row — its "unhighlighted" baseline
/// (hovered/selected rows carry a uniform wash, not `None`). BTreeMap keys
/// make count ties deterministic.
fn dominant_bg(cells: &[(char, Option<String>)]) -> Option<String> {
    let mut counts: std::collections::BTreeMap<&Option<String>, usize> =
        std::collections::BTreeMap::new();
    for (_, bg) in cells {
        *counts.entry(bg).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(bg, count)| (count, bg.clone()))
        .map(|(bg, _)| bg.clone())
        .unwrap_or(None)
}

fn locate_text(harness: &PtyHarness, text: &str, occurrence: usize) -> Result<MousePoint> {
    locate_text_impl(harness, text, occurrence, false)
}

fn locate_text_end(harness: &PtyHarness, text: &str, occurrence: usize) -> Result<MousePoint> {
    let mut point = locate_text_impl(harness, text, occurrence, true)?;
    point.col = point.col.saturating_add(1);
    Ok(point)
}

fn locate_text_impl(
    harness: &PtyHarness,
    text: &str,
    occurrence: usize,
    end: bool,
) -> Result<MousePoint> {
    if text.is_empty() {
        bail!("cannot locate empty text");
    }
    let output = harness.screen_output();
    let mut seen = 0usize;
    for (row, line) in output.lines.iter().enumerate() {
        let mut start_byte = 0usize;
        while let Some(rel_byte) = line[start_byte..].find(text) {
            let byte = start_byte + rel_byte;
            if seen == occurrence {
                let mut col = line[..byte].chars().count();
                if end {
                    col += text.chars().count().saturating_sub(1);
                }
                return Ok(MousePoint {
                    row: row as u16,
                    col: col as u16,
                });
            }
            seen += 1;
            start_byte = byte + text.len();
        }
    }
    bail!(
        "could not locate occurrence {occurrence} of text {text:?} on screen\n{}",
        harness.screen_contents()
    )
}

fn click_point(harness: &mut PtyHarness, point: MousePoint, button: MouseButton) -> Result<()> {
    harness
        .inject_keys(mouse_click_bytes(point.row, point.col, button).as_bytes())
        .with_context(|| format!("click at {point:?}"))?;
    harness.update(Duration::from_millis(100));
    Ok(())
}

fn decode_osc52_payloads(bytes: &[u8]) -> Result<Vec<String>> {
    use base64::Engine as _;

    let output = String::from_utf8_lossy(bytes);
    let mut payloads = Vec::new();
    for segment in output.split("\x1b]52;").skip(1) {
        let Some((_, rest)) = segment.split_once(';') else {
            continue;
        };
        let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
        let encoded = &rest[..end];
        if encoded.is_empty() {
            continue;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .with_context(|| "decode OSC 52 base64 payload")?;
        let text = String::from_utf8(decoded).context("OSC 52 payload is not UTF-8 text")?;
        payloads.push(text);
    }
    Ok(payloads)
}

/// Count Kitty graphics protocol APC sequences (`ESC _ G`) that carry image
/// data or placement in raw PTY output — i.e. every graphics APC *except* the
/// pure control escapes delete (`a=d`) and capability query (`a=q`), which
/// display nothing. (Image transmits chunk into several APCs, so this counts
/// each chunk; callers use it as a presence test, not an exact image count.)
///
/// The pager writes these into the synchronized-update frame buffer, outside the
/// vt100 cell grid, so the screen-text snapshot can't observe them. Excluding
/// delete/query makes `assert_no_kitty_graphics` mean "no inline image was
/// shown", not "the pager never probed for graphics support".
pub(crate) fn count_kitty_graphics(bytes: &[u8]) -> usize {
    const INTRO: &[u8] = b"\x1b_G";
    let mut count = 0;
    let mut i = 0;
    while i + INTRO.len() <= bytes.len() {
        if &bytes[i..i + INTRO.len()] != INTRO {
            i += 1;
            continue;
        }
        // The APC's parameter section runs up to the first `;` (data follows) or
        // the terminating ESC.
        let params_start = i + INTRO.len();
        let params_end = bytes[params_start..]
            .iter()
            .position(|&b| b == b';' || b == 0x1b)
            .map_or(bytes.len(), |p| params_start + p);
        let params = &bytes[params_start..params_end];
        let has = |needle: &[u8]| params.windows(needle.len()).any(|w| w == needle);
        if !has(b"a=d") && !has(b"a=q") {
            count += 1;
        }
        i = params_start;
    }
    count
}

fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

fn mouse_click_bytes(row: u16, col: u16, button: MouseButton) -> String {
    let button_code = match button {
        MouseButton::Left => SGR_LEFT_BUTTON,
        MouseButton::Middle => SGR_MIDDLE_BUTTON,
        MouseButton::Right => SGR_RIGHT_BUTTON,
    };
    format!(
        "{}{}",
        sgr_mouse(button_code, row, col, 'M'),
        sgr_mouse(button_code, row, col, 'm')
    )
}

fn repeated_click_bytes(row: u16, col: u16, count: u8) -> String {
    let mut out = String::new();
    for _ in 0..count {
        out.push_str(&mouse_click_bytes(row, col, MouseButton::Left));
    }
    out
}

fn mouse_scroll_bytes(row: u16, col: u16, direction: ScrollDirection, count: u16) -> String {
    let code = match direction {
        ScrollDirection::Up => SGR_SCROLL_UP,
        ScrollDirection::Down => SGR_SCROLL_DOWN,
    };
    let mut out = String::new();
    for _ in 0..count {
        out.push_str(&sgr_mouse(code, row, col, 'M'));
    }
    out
}

fn mouse_drag_bytes(from: MousePoint, to: MousePoint) -> String {
    let mut out = String::new();
    out.push_str(&sgr_mouse(SGR_LEFT_BUTTON, from.row, from.col, 'M'));
    let mid_row = (from.row + to.row) / 2;
    let mid_col = (from.col + to.col) / 2;
    out.push_str(&sgr_mouse(SGR_DRAG_BUTTON, mid_row, mid_col, 'M'));
    out.push_str(&sgr_mouse(SGR_DRAG_BUTTON, to.row, to.col, 'M'));
    out.push_str(&sgr_mouse(SGR_LEFT_BUTTON, to.row, to.col, 'm'));
    out
}

fn sgr_mouse(button: u16, row: u16, col: u16, suffix: char) -> String {
    format!("\x1b[<{button};{};{}{suffix}", col + 1, row + 1)
}

fn action_name(step: &ScenarioStep) -> &'static str {
    match step {
        ScenarioStep::WaitForText { .. } => "wait_for_text",
        ScenarioStep::AssertContains { .. } => "assert_contains",
        ScenarioStep::AssertNotContains { .. } => "assert_not_contains",
        ScenarioStep::TypeText { .. } => "type_text",
        ScenarioStep::Keys { .. } => "keys",
        ScenarioStep::Paste { .. } => "paste",
        ScenarioStep::PasteImagePaths { .. } => "paste_image_paths",
        ScenarioStep::PasteImageFileUrls { .. } => "paste_image_file_urls",
        ScenarioStep::PasteEscapedImagePaths { .. } => "paste_escaped_image_paths",
        ScenarioStep::FocusPrompt => "focus_prompt",
        ScenarioStep::PastePrompt { .. } => "paste_prompt",
        ScenarioStep::PasteClipboardImage { .. } => "paste_clipboard_image",
        ScenarioStep::DropTextPrompt { .. } => "drop_text_prompt",
        ScenarioStep::MouseClick { .. } => "mouse_click",
        ScenarioStep::DoubleClick { .. } => "double_click",
        ScenarioStep::TripleClick { .. } => "triple_click",
        ScenarioStep::Scroll { .. } => "scroll",
        ScenarioStep::ScrollAt { .. } => "scroll_at",
        ScenarioStep::Drag { .. } => "drag",
        ScenarioStep::SelectText { .. } => "select_text",
        ScenarioStep::ClickText { .. } => "click_text",
        ScenarioStep::DoubleClickText { .. } => "double_click_text",
        ScenarioStep::TripleClickText { .. } => "triple_click_text",
        ScenarioStep::SelectTextRange { .. } => "select_text_range",
        ScenarioStep::DropText { .. } => "drop_text",
        ScenarioStep::DropTextAt { .. } => "drop_text_at",
        ScenarioStep::DropImagesPrompt { .. } => "drop_images_prompt",
        ScenarioStep::Copy { .. } => "copy",
        ScenarioStep::AssertOsc52Contains { .. } => "assert_osc52_contains",
        ScenarioStep::AssertOsc52NotContains { .. } => "assert_osc52_not_contains",
        ScenarioStep::AssertHighlightRun { .. } => "assert_highlight_run",
        ScenarioStep::AssertTextNotHighlighted { .. } => "assert_text_not_highlighted",
        ScenarioStep::AssertKittyGraphics { .. } => "assert_kitty_graphics",
        ScenarioStep::WaitForKittyGraphics { .. } => "wait_for_kitty_graphics",
        ScenarioStep::AssertNoKittyGraphics {} => "assert_no_kitty_graphics",
        ScenarioStep::AssertRequestImageCount { .. } => "assert_request_image_count",
        ScenarioStep::AssertInlineImages { .. } => "assert_inline_images",
        ScenarioStep::AssertRequestContains { .. } => "assert_request_contains",
        ScenarioStep::AssertNoTempArtifacts {} => "assert_no_temp_artifacts",
        ScenarioStep::Resize { .. } => "resize",
        ScenarioStep::Wait { .. } => "wait",
        ScenarioStep::Screenshot { .. } => "screenshot",
        ScenarioStep::AssertRunning => "assert_running",
        ScenarioStep::AssertChatCompletion => "assert_chat_completion",
    }
}

fn safe_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

fn escape_xml(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&apos;".to_owned(),
            _ => ch.to_string(),
        })
        .collect()
}

fn escape_attr(text: &str) -> String {
    escape_xml(text)
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn default_rows() -> u16 {
    DEFAULT_ROWS
}

fn default_cols() -> u16 {
    DEFAULT_COLS
}

fn default_wait_timeout_ms() -> u64 {
    DEFAULT_WAIT_TIMEOUT_MS
}

fn default_scroll_count() -> u16 {
    1
}

fn default_assert_min() -> usize {
    1
}

fn default_copy_keys() -> String {
    "y".to_owned()
}

fn default_mock_response() -> String {
    "Hello from scripted TUI scenario mock.".to_owned()
}

fn default_fixture_extension() -> String {
    "png".to_owned()
}

fn default_expected_image_mime() -> String {
    "image/png".to_owned()
}

fn default_image_width() -> DimensionAssertion {
    DimensionAssertion::Exact(2)
}

fn default_image_height() -> DimensionAssertion {
    DimensionAssertion::Exact(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_scenario() {
        let scenario: ScriptedScenario = serde_json::from_str(
            r#"{
              "name": "welcome",
              "steps": [
                {"action": "wait_for_text", "text": "Quit"},
                {"action": "screenshot", "name": "welcome"}
              ]
            }"#,
        )
        .expect("parse scenario");

        assert_eq!(scenario.name, "welcome");
        assert_eq!(scenario.terminal.rows, DEFAULT_ROWS);
        assert_eq!(scenario.steps.len(), 2);
    }

    #[test]
    fn safe_names_are_filesystem_friendly() {
        assert_eq!(safe_name("Welcome screen / macOS"), "Welcome-screen-macOS");
    }

    #[test]
    fn workspace_rejects_paths_outside_the_tempdir() {
        use std::collections::BTreeMap;

        // A normal relative key (the folder-trust scenario's own `.mcp.json`)
        // is accepted.
        let ok = WorkspaceConfig {
            git_init: false,
            files: BTreeMap::from([(".mcp.json".to_string(), "{}".to_string())]),
        };
        let sandbox = echo_agent_test_support::TestSandbox::new();
        assert!(materialize_workspace(&ok, &sandbox).is_ok());

        // Absolute and `..`-traversing keys are rejected before any write.
        for bad in ["/etc/evil", "../escape", "sub/../../escape"] {
            let ws = WorkspaceConfig {
                git_init: false,
                files: BTreeMap::from([(bad.to_string(), "x".to_string())]),
            };
            let err = materialize_workspace(&ws, &sandbox)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("must be relative and within the workspace"),
                "path {bad:?} must be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn workspace_git_init_materializes_a_real_repository() {
        let workspace = WorkspaceConfig {
            git_init: true,
            files: std::collections::BTreeMap::from([(
                "nested/fixture.txt".to_string(),
                "fixture\n".to_string(),
            )]),
        };

        let sandbox = echo_agent_test_support::TestSandbox::new();
        let dir = materialize_workspace(&workspace, &sandbox).expect("materialize git workspace");
        assert!(dir.path().join(".git").is_dir());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/fixture.txt")).unwrap(),
            "fixture\n"
        );
        let mut cmd = sandbox.git_command();
        let output = cmd
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(dir.path())
            .output()
            .expect("query materialized repository");
        assert!(
            output.status.success(),
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn image_fixture_defaults_to_standard_kind() {
        let f: ImageFixture =
            serde_yaml::from_str("name: legacy\nextension: png\n").expect("parse fixture");
        assert_eq!(f.kind, ImageFixtureKind::Standard);
    }

    #[test]
    fn image_fixture_parses_explicit_kind() {
        let f: ImageFixture = serde_yaml::from_str("name: tiny\nkind: tiny_1x1\n").expect("parse");
        assert_eq!(f.kind, ImageFixtureKind::Tiny1x1);
        let f: ImageFixture =
            serde_yaml::from_str("name: bad\nkind: crc_corrupt_png\n").expect("parse");
        assert_eq!(f.kind, ImageFixtureKind::CrcCorruptPng);
    }

    fn decoded_dimensions(bytes: &[u8]) -> (u32, u32) {
        image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .expect("sniff format")
            .into_dimensions()
            .expect("read dimensions")
    }

    #[test]
    fn standard_fixture_decodes_as_8x8() {
        let bytes = standard_png_bytes().expect("encode standard");
        assert_eq!(decoded_dimensions(&bytes), (8, 8));
    }

    #[test]
    fn tiny_fixture_decodes_as_1x1() {
        let bytes = tiny_1x1_png_bytes().expect("encode tiny");
        assert_eq!(decoded_dimensions(&bytes), (1, 1));
    }

    #[test]
    fn crc_corrupt_png_fails_to_decode() {
        let bytes = crc_corrupt_png_bytes().expect("encode corrupt");
        assert!(
            image::load_from_memory(&bytes).is_err(),
            "expected CRC-corrupt PNG to be rejected by image::load_from_memory"
        );
    }

    #[test]
    fn dimension_assertion_parses_exact_and_range() {
        let exact: DimensionAssertion = serde_yaml::from_str("28").expect("parse exact");
        assert!(matches!(exact, DimensionAssertion::Exact(28)));
        assert!(exact.matches(28));
        assert!(!exact.matches(29));

        let lo: DimensionAssertion = serde_yaml::from_str("min: 28").expect("parse min");
        assert!(lo.matches(28));
        assert!(lo.matches(1024));
        assert!(!lo.matches(27));

        let hi: DimensionAssertion = serde_yaml::from_str("max: 100").expect("parse max");
        assert!(hi.matches(0));
        assert!(hi.matches(100));
        assert!(!hi.matches(101));

        let both: DimensionAssertion =
            serde_yaml::from_str("{min: 10, max: 20}").expect("parse range");
        assert!(both.matches(15));
        assert!(!both.matches(9));
        assert!(!both.matches(21));
    }

    #[test]
    fn assert_request_contains_finds_substring_in_any_body() {
        let bodies = vec![
            serde_json::json!({"messages": [{"content": "hello"}]}),
            serde_json::json!({"messages": [{"content": "<image_dropped_notice>boom"}]}),
        ];
        assert_request_contains(&bodies, "<image_dropped_notice>", None).expect("found");
        assert!(assert_request_contains(&bodies, "no-such", None).is_err());
    }

    #[test]
    fn assert_request_contains_honors_request_index() {
        let bodies = vec![
            serde_json::json!({"k": "first"}),
            serde_json::json!({"k": "second"}),
            serde_json::json!({"k": "third"}),
        ];
        // index 0 = newest
        assert_request_contains(&bodies, "third", Some(0)).expect("newest");
        assert_request_contains(&bodies, "first", Some(2)).expect("oldest");
        assert!(assert_request_contains(&bodies, "first", Some(0)).is_err());
        assert!(assert_request_contains(&bodies, "anything", Some(99)).is_err());
    }

    #[test]
    fn assert_no_temp_artifacts_scans_recursively() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_no_temp_artifacts(dir.path()).expect("empty dir is clean");

        let nested = dir.path().join("sub/sub2");
        fs::create_dir_all(&nested).expect("mkdirs");
        fs::write(nested.join("ok.png"), b"x").expect("write ok");
        assert_no_temp_artifacts(dir.path()).expect("non-tmp file is clean");

        fs::write(nested.join("leftover.tmp"), b"x").expect("write tmp");
        assert!(assert_no_temp_artifacts(dir.path()).is_err());
    }

    #[test]
    fn parses_assert_no_temp_artifacts_action() {
        let step: ScenarioStep =
            serde_yaml::from_str("action: assert_no_temp_artifacts").expect("parse");
        assert!(matches!(step, ScenarioStep::AssertNoTempArtifacts {}));
    }

    #[test]
    fn parses_assert_request_contains_action() {
        let step: ScenarioStep =
            serde_yaml::from_str("action: assert_request_contains\ntext: hello").expect("parse");
        match step {
            ScenarioStep::AssertRequestContains {
                text,
                request_index,
            } => {
                assert_eq!(text, "hello");
                assert_eq!(request_index, None);
            }
            other => panic!("unexpected step: {other:?}"),
        }
    }

    #[test]
    fn count_kitty_graphics_counts_apc_introducers() {
        // Two image escapes (transmit `_Gf=100...` + place `_Ga=p...`),
        // ignoring OSC and other escapes around them.
        let raw = b"text\x1b_Gf=100,i=2;AAAA\x1b\\\x1b[0m\x1b_Ga=p,i=2\x1b\\more";
        assert_eq!(count_kitty_graphics(raw), 2);
        assert_eq!(count_kitty_graphics(b"no graphics here"), 0);
        // An OSC 52 clipboard write must not be miscounted as a graphics APC.
        assert_eq!(count_kitty_graphics(b"\x1b]52;c;AAAA\x07"), 0);
        // Delete (`a=d`) and capability query (`a=q`) display nothing, so they
        // are not counted — a diagram that renders as text emits only these.
        assert_eq!(count_kitty_graphics(b"\x1b_Ga=d,d=i,i=1,q=2\x1b\\"), 0);
        assert_eq!(
            count_kitty_graphics(b"\x1b_Gi=31,a=q,s=1,v=1;AAAA\x1b\\"),
            0
        );
        // A real place alongside a delete counts only the place.
        assert_eq!(
            count_kitty_graphics(b"\x1b_Ga=d,i=1\x1b\\\x1b_Ga=p,i=2\x1b\\"),
            1
        );
    }

    #[test]
    fn parses_assert_kitty_graphics_with_default_min() {
        let step: ScenarioStep =
            serde_yaml::from_str("action: assert_kitty_graphics\n").expect("parse");
        match step {
            ScenarioStep::AssertKittyGraphics { min } => assert_eq!(min, 1),
            other => panic!("unexpected step: {other:?}"),
        }
        let step: ScenarioStep =
            serde_yaml::from_str("action: assert_kitty_graphics\nmin: 3\n").expect("parse");
        match step {
            ScenarioStep::AssertKittyGraphics { min } => assert_eq!(min, 3),
            other => panic!("unexpected step: {other:?}"),
        }
    }

    #[test]
    fn parses_assert_no_kitty_graphics() {
        let step: ScenarioStep =
            serde_yaml::from_str("action: assert_no_kitty_graphics\n").expect("parse");
        assert!(matches!(step, ScenarioStep::AssertNoKittyGraphics {}));
        assert_eq!(action_name(&step), "assert_no_kitty_graphics");
    }

    #[test]
    fn parses_assert_inline_images_with_min_form() {
        let step: ScenarioStep = serde_yaml::from_str(
            "action: assert_inline_images\nmin: 1\nwidth: { min: 28 }\nheight: { min: 28 }\n",
        )
        .expect("parse");
        match step {
            ScenarioStep::AssertInlineImages { width, height, .. } => {
                assert!(width.matches(28));
                assert!(height.matches(64));
                assert!(!width.matches(27));
            }
            other => panic!("unexpected step: {other:?}"),
        }
    }

    #[test]
    fn assert_inline_images_accepts_min_form() {
        use base64::Engine as _;
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(standard_png_bytes().unwrap());
        let bodies = vec![serde_json::json!({
            "messages": [{
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": format!("data:image/png;base64,{encoded}")}
                }]
            }]
        })];
        // exact form still works (default behaviour)
        assert_inline_images(
            &bodies,
            1,
            "image/png",
            &DimensionAssertion::Exact(8),
            &DimensionAssertion::Exact(8),
        )
        .expect("exact match");
        // range form succeeds when width >= 1
        assert_inline_images(
            &bodies,
            1,
            "image/png",
            &DimensionAssertion::Range {
                min: Some(1),
                max: None,
            },
            &DimensionAssertion::Range {
                min: Some(1),
                max: None,
            },
        )
        .expect("range match");
        // range form fails when width must be >= 28 but actual is 8
        assert!(
            assert_inline_images(
                &bodies,
                1,
                "image/png",
                &DimensionAssertion::Range {
                    min: Some(28),
                    max: None
                },
                &DimensionAssertion::Range {
                    min: Some(28),
                    max: None
                },
            )
            .is_err()
        );
    }
}
