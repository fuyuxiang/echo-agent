//! `grep` tool — new architecture (`Tool` trait).
//!
//! Wraps ripgrep to search file contents. Reads `Cwd` from Resources and
//! truncation settings from its own `Params<GrepParams>`.
//!
//! The ripgrep binary resolution logic (`rg_path()`) is shared with the
//! old implementation via `implementations::grep::ripgrep`.

use std::io;
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use crate::DEFAULT_TOOL_OUTPUT_BYTES;
use crate::types::output::{GrepFileMatch, GrepLineMatch, GrepSearchOutput};
#[allow(unused_imports)]
use crate::types::resources::{
    Cwd, DenyReadGlobs, DisplayCwd, Params, PathNotFoundHints, SharedResources, display_cwd_or_cwd,
    resolve_model_path,
};
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::util::truncate::truncate_line;

// ───────────────────────────────────────────────────────────────────────────
// Input
// ───────────────────────────────────────────────────────────────────────────

use serde::{Deserialize, Serialize};

pub mod ripgrep;

// Re-export the shared EchoAgentIntegerSchema from types module
pub use crate::types::EchoAgentIntegerSchema;
use ripgrep::{rg_path, unavailable_message};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Content,
    FilesWithMatches,
    Count,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GrepSearchInput {
    #[schemars(
        description = "The regular expression pattern to search for in file contents (rg --regexp)"
    )]
    pub pattern: String,

    #[schemars(
        description = "File or directory to search in (rg pattern -- PATH). Defaults to workspace path."
    )]
    pub path: Option<String>,

    #[schemars(
        description = r#"Glob pattern (rg --glob GLOB -- PATH) to filter files (e.g. "*.js", "*.{ts,tsx}")."#
    )]
    pub glob: Option<String>,

    /// Accepted on the wire when present; omitted from the JSON schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub output_mode: Option<OutputMode>,

    #[schemars(
        rename = "-B",
        with = "EchoAgentIntegerSchema",
        description = "Number of lines to show before each match (rg -B)."
    )]
    #[serde(rename = "-B")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_context: Option<usize>,

    #[schemars(
        rename = "-A",
        with = "EchoAgentIntegerSchema",
        description = "Number of lines to show after each match (rg -A)."
    )]
    #[serde(rename = "-A")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_context: Option<usize>,

    #[schemars(
        rename = "-C",
        with = "EchoAgentIntegerSchema",
        description = "Number of lines to show before and after each match (rg -C)."
    )]
    #[serde(rename = "-C")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,

    #[schemars(rename = "-i", description = "Case insensitive search (rg -i).")]
    #[serde(
        rename = "-i",
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    pub case_insensitive: bool,

    #[schemars(
        description = "File type to search (rg --type). Common types: js, py, rust, go, java, etc. More efficient than glob for standard file types."
    )]
    pub r#type: Option<String>,

    #[schemars(
        with = "EchoAgentIntegerSchema",
        description = "Limit output to first N lines/entries, equivalent to \"| head -N\". Defaults to 200 lines or 500 entries."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_limit: Option<usize>,

    #[schemars(
        description = "Enable multiline mode where . matches newlines and patterns can span lines (rg -U --multiline-dotall)."
    )]
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    pub multiline: bool,
}

// ───────────────────────────────────────────────────────────────────────────
// Params
// ───────────────────────────────────────────────────────────────────────────

/// Per-tool configuration for `grep`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrepParams {
    /// Maximum output size in bytes before truncation.
    /// Defaults to `DEFAULT_TOOL_OUTPUT_BYTES` (40 KB) when `None`.
    pub max_output_bytes: Option<usize>,
    /// Maximum characters per line before truncation.
    /// Defaults to 1000 when `None`.
    pub max_chars_per_line: Option<usize>,
}

crate::register_resource!("echo_agent_build", "Grep", GrepParams);

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

/// Hard max when the model passes an explicit `head_limit` (content lines).
const CONTENT_LINE_LIMIT: usize = 2_000;
/// Default when `head_limit` is omitted (content). Chosen near observed agent
/// usage (most explicit limits are 20–50; max useful exploration is well under
/// 2000) so large-repository walks stop earlier than the hard max.
const CONTENT_LINE_DEFAULT: usize = 200;
/// Hard max for files_with_matches / count entry lists.
const FILE_COUNT_LIMIT: usize = 10_000;
/// Default when `head_limit` is omitted (files/count modes).
const FILE_COUNT_DEFAULT: usize = 500;
pub const DEFAULT_MAX_CHARS_PER_LINE: usize = 1_000;

/// Hard cap on bytes read from ripgrep's stdout (5 MB).
const MAX_STDOUT_BYTES: usize = 5_000_000;

/// After the line/byte budget is filled, how long to wait for one more byte to
/// distinguish exact-fit (EOF) from overflow. Must stay far below the tool
/// wall-clock timeout: an unbounded probe can block until the outer timeout
/// and discard the already-buffered matches via `grep_timeout_output`.
const EXACT_FIT_PROBE_TIMEOUT: Duration = Duration::from_millis(100);

/// Default grep wall-clock timeout (seconds) on non-WSL platforms.
const GREP_TIMEOUT_DEFAULT_SECS: u64 = 20;

/// Grep wall-clock timeout (seconds) under WSL, where filesystem reads are 3-5x slower.
const GREP_TIMEOUT_WSL_SECS: u64 = 60;

/// Grep's wall-clock timeout in whole seconds: 60s on WSL (slow filesystem), 20s elsewhere.
fn grep_timeout_secs(is_wsl: bool) -> u64 {
    if is_wsl {
        GREP_TIMEOUT_WSL_SECS
    } else {
        GREP_TIMEOUT_DEFAULT_SECS
    }
}

/// Grep's wall-clock timeout for the current platform.
fn grep_timeout() -> Duration {
    Duration::from_secs(grep_timeout_secs(echo_agent_tty_utils::is_wsl()))
}

/// Resolve the effective line/entry budget for this call.
///
/// Always returns a finite limit so we can stop reading (and kill `rg`) once
/// enough output is in hand — even when the model omits `head_limit`.
fn resolve_effective_head_limit(input: &GrepSearchInput, output_mode: &OutputMode) -> usize {
    let (default, cap) = match output_mode {
        OutputMode::Content => (CONTENT_LINE_DEFAULT, CONTENT_LINE_LIMIT),
        OutputMode::FilesWithMatches | OutputMode::Count => (FILE_COUNT_DEFAULT, FILE_COUNT_LIMIT),
    };
    input.head_limit.unwrap_or(default).min(cap)
}

/// Hard `head_limit` ceiling for a mode (what an explicit limit is clamped to).
///
/// Callers that paginate over the full underlying result themselves
/// must request this instead of `head_limit: None`, which
/// now resolves to the small omitted-`head_limit` default and kills `rg` early.
pub fn max_head_limit(output_mode: &OutputMode) -> usize {
    match output_mode {
        OutputMode::Content => CONTENT_LINE_LIMIT,
        OutputMode::FilesWithMatches | OutputMode::Count => FILE_COUNT_LIMIT,
    }
}

/// grep's capabilities incl. its streaming spec (single source of truth).
/// grep streams the formatted card body (`PlainText` / `Append`), never raw
/// stdout; the `<workspace_result …>` wrapper and "Found N …" summary are a
/// terminal-only footer, so the stream is a faithful prefix of the card body.
static GREP_CAPABILITIES: LazyLock<echo_agent_tool_protocol::ToolCapabilities> =
    LazyLock::new(|| echo_agent_tool_protocol::ToolCapabilities {
        is_read_only: true,
        tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
        streaming: Some(echo_agent_tool_protocol::StreamingSpec {
            subkind: "grep_match_chunk".to_owned(),
            max_delta_bytes: None,
        }),
        ..Default::default()
    });

// ───────────────────────────────────────────────────────────────────────────
// Tool implementation
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct GrepTool;

impl crate::types::tool_metadata::ToolMetadata for GrepTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        r#"Search file contents with regular expressions (ripgrep).

- Full regex syntax, so escape literal special characters: `functionCall\(`, or `interface\{\}` to find interface{} in Go.
- Pass ${{ params.search.pattern }} as a raw regex string — no surrounding quotes.
- Respects .gitignore unless you pass a broad glob like '--glob *'.
- Only filter by '${{ params.search.type }}' or '${{ params.search.glob }}' when you are sure of the file type; import paths may not match source file types (.js vs .ts).
- Output is ripgrep-style: ':' marks match lines, '-' marks context lines, grouped by file. Large results are capped and report "at least" counts."#
    }
}

impl echo_agent_tool_runtime::Tool for GrepTool {
    type Args = GrepSearchInput;
    type Output = GrepSearchOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "grep",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        // Clone of `GREP_CAPABILITIES`; read at registration time only.
        GREP_CAPABILITIES.clone()
    }

    /// Streaming entry point. Gate OFF (default): byte-for-byte the blocking
    /// [`GrepTool::run`] contract. Gate ON: spawn ripgrep, project each match
    /// line via [`BodyStreamer`] (same projection [`finalize_grep`] re-derives
    /// in batch) and emit `grep_match_chunk` deltas — the stream is a faithful
    /// prefix of the terminal card body. Gated by
    /// `WorkspaceViewerContext::stream_tool_progress`.
    async fn execute(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: GrepSearchInput,
    ) -> echo_agent_tool_runtime::ToolStream<GrepSearchOutput> {
        // Absent extension or spec ⇒ gate off. `Some(spec)` iff the gate is
        // on; the spec borrow is `'static` (LazyLock), so it moves straight
        // into the stream below.
        let admitted_spec = ctx
            .get::<echo_agent_tool_runtime::WorkspaceViewerContext>()
            .zip(GREP_CAPABILITIES.streaming.as_ref())
            .filter(|(vctx, _)| vctx.stream_tool_progress)
            .map(|(_, spec)| spec);

        // Fast path: gate off ⇒ run the blocking implementation and wrap its
        // single result. Identical to the pre-streaming contract.
        let Some(spec) = admitted_spec else {
            return echo_agent_tool_runtime::terminal_only(self.run(ctx, input).await);
        };

        // `tool.grep` span matching `run`'s; a guard can't be held across
        // the stream's await points, so the handle is scoped explicitly.
        let span = tracing::info_span!(
            "tool.grep",
            timed_out = tracing::field::Empty,
            wall_ms = tracing::field::Empty,
            early_kill = tracing::field::Empty,
            effective_head_limit = tracing::field::Empty,
        );

        grep_progress_stream(ctx, input, spec, span)
    }

    #[tracing::instrument(
        name = "tool.grep",
        skip_all,
        fields(
            timed_out = tracing::field::Empty,
            wall_ms = tracing::field::Empty,
            early_kill = tracing::field::Empty,
            effective_head_limit = tracing::field::Empty,
        )
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: GrepSearchInput,
    ) -> Result<GrepSearchOutput, echo_agent_tool_runtime::ToolError> {
        let started = std::time::Instant::now();
        let GrepReady {
            mut child,
            stdout_pipe,
            stderr_pipe,
            config,
        } = match prepare_grep(&ctx, &input).await? {
            GrepStep::Ready(ready) => ready,
            GrepStep::Early(out) => {
                tracing::Span::current().record("wall_ms", started.elapsed().as_millis() as u64);
                tracing::Span::current().record("early_kill", false);
                return Ok(out);
            }
        };
        tracing::Span::current().record("effective_head_limit", config.effective_head_limit as u64);

        let timeout = grep_timeout();
        let deadline_at = tokio::time::Instant::now() + timeout;
        let stderr_task =
            stderr_pipe.map(|stderr_pipe| tokio::spawn(read_rg_stderr_capped(stderr_pipe)));
        let io_result = tokio::time::timeout_at(deadline_at, async {
            // Read stdout until EOF, byte cap, or one line past the budget.
            // Reading `effective_head_limit + 1` lines lets us distinguish an
            // exact-fit result (not truncated) from an overflowing one, so we
            // never flag truncation when there are exactly `effective_head_limit`
            // lines — matching `finalize_grep`'s `> limit` check.
            let (stdout_buf, stdout_truncated) = if let Some(stdout_pipe) = stdout_pipe {
                read_rg_stdout_capped(stdout_pipe, config.effective_head_limit.saturating_add(1))
                    .await?
            } else {
                (Vec::new(), false)
            };

            // Kill `rg` **before** draining stderr when we stopped at the budget.
            // Dropping `stdout_pipe` above closes the read end, but a tree-walking
            // `rg` only observes that on its next match write; until then it holds
            // stderr open, so `read_to_end` would block until `rg` exits or the
            // outer timeout fires — the latter returns `grep_timeout_output` and
            // drops the matches we already buffered (the same failure the
            // exact-fit probe bound guards against, one step later).
            if stdout_truncated {
                let _ = child.start_kill();
            }

            // stderr is drained concurrently from process start so a noisy rg
            // can never block while stdout is being consumed.
            let stderr_buf = match stderr_task {
                Some(task) => task
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))??,
                None => Vec::new(),
            };

            Ok::<_, io::Error>((stdout_buf, stdout_truncated, stderr_buf))
        })
        .await;

        let (stdout_buf, stdout_truncated, stderr_buf) = match io_result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                tracing::Span::current().record("early_kill", true);
                tracing::Span::current().record("wall_ms", started.elapsed().as_millis() as u64);
                let _ = child.start_kill();
                crate::util::reap_killed_search_child(&mut child).await;
                return Err(echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id"),
                    format!("读取内置搜索组件输出失败：{error}"),
                )
                .with_source(error));
            }
            Err(_elapsed) => {
                tracing::Span::current().record("timed_out", true);
                tracing::Span::current().record("early_kill", true);
                tracing::Span::current().record("wall_ms", started.elapsed().as_millis() as u64);
                tracing::warn!(timeout_secs = timeout.as_secs(), "grep timed out");
                let _ = child.start_kill();
                crate::util::reap_killed_search_child(&mut child).await;
                return Ok(grep_timeout_output(timeout.as_secs()));
            }
        };

        // Truncated output means `rg` was already killed above — bounded reap,
        // and the exit code is defined as 0. A natural EOF means rg is exiting,
        // so the plain wait is prompt.
        let exit_code = if stdout_truncated {
            crate::util::reap_killed_search_child(&mut child).await;
            0
        } else {
            match tokio::time::timeout_at(deadline_at, child.wait()).await {
                Ok(Ok(status)) => status.code().unwrap_or(-1),
                Ok(Err(error)) => {
                    return Err(echo_agent_tool_runtime::ToolError::execution(
                        echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id"),
                        format!("等待内置搜索组件结束失败：{error}"),
                    )
                    .with_source(error));
                }
                Err(_) => {
                    let _ = child.start_kill();
                    crate::util::reap_killed_search_child(&mut child).await;
                    return Ok(grep_timeout_output(timeout.as_secs()));
                }
            }
        };

        tracing::Span::current().record("early_kill", stdout_truncated);
        tracing::Span::current().record("wall_ms", started.elapsed().as_millis() as u64);
        tracing::info!(
            wall_ms = started.elapsed().as_millis() as u64,
            early_kill = stdout_truncated,
            effective_head_limit = config.effective_head_limit,
            exit_code,
            "grep finished"
        );

        Ok(finalize_grep(
            stdout_buf,
            stdout_truncated,
            stderr_buf,
            exit_code,
            &config,
        ))
    }
}

/// Streaming grep pipeline: spawn ripgrep, project each match line via
/// `BodyStreamer`, and emit deltas before the terminal card.
fn grep_progress_stream(
    ctx: echo_agent_tool_runtime::ToolCallContext,
    input: GrepSearchInput,
    spec: &'static echo_agent_tool_protocol::StreamingSpec,
    span: tracing::Span,
) -> echo_agent_tool_runtime::ToolStream<GrepSearchOutput> {
    Box::pin(async_stream::stream! {
        let stream_started = std::time::Instant::now();
        let GrepReady {
            mut child,
            stdout_pipe,
            stderr_pipe,
            config,
        } = match prepare_grep(&ctx, &input).await {
            Ok(GrepStep::Ready(ready)) => ready,
            Ok(GrepStep::Early(out)) => {
                // Mirror `run`'s Early arm so path-not-found / spawn short-circuits
                // still populate the `tool.grep` span in the streaming (prod) path.
                span.record("wall_ms", stream_started.elapsed().as_millis() as u64);
                span.record("early_kill", false);
                yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(out));
                return;
            }
            Err(e) => {
                yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(e));
                return;
            }
        };

        // Raw bytes for the authoritative terminal card.
        span.record("effective_head_limit", config.effective_head_limit as u64);
        let mut stdout_buf = Vec::with_capacity(MAX_STDOUT_BYTES.min(65_536));
        let mut stdout_truncated = false;
        let mut stdout_read_error = None;
        let mut stderr_task = stderr_pipe
            .map(|stderr_pipe| tokio::spawn(read_rg_stderr_capped(stderr_pipe)));
        // Incremental card-body formatter (deltas == terminal body).
        let mut streamer = BodyStreamer::new(spec, &config);
        let mut timed_out = false;
        // Complete newlines accepted into `stdout_buf` (same budget as
        // `read_rg_stdout_capped` / `finalize_grep`).
        let mut complete_lines = 0usize;
        // One deadline shared by stdout loop + stderr drain (same total
        // budget as `run`).
        let timeout = grep_timeout();
        let deadline_at = tokio::time::Instant::now() + timeout;

        if let Some(mut stdout_pipe) = stdout_pipe {
            let mut tmp = [0u8; 8192];
            // Deadline rides the `select!` (can't wrap a yielding block).
            let deadline = tokio::time::sleep_until(deadline_at);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut deadline => {
                        timed_out = true;
                        break;
                    }
                    res = stdout_pipe.read(&mut tmp) => {
                        let n = match res {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(error) => {
                                stdout_read_error = Some(error);
                                break;
                            }
                        };
                        // Mirror `run`'s hard byte + line caps when filling
                        // `stdout_buf`, then kill so rg stops walking the tree.
                        // `+ 1`: read one line past the budget so truncation is
                        // only flagged when there are genuinely MORE than
                        // `effective_head_limit` lines (matches `run` /
                        // `finalize_grep`). The extra line is dropped by
                        // `BodyStreamer`/`finalize_grep`, never emitted.
                        let (accepted, hit_cap) = accept_rg_stdout_chunk(
                            &tmp[..n],
                            stdout_buf.len(),
                            complete_lines,
                            config.effective_head_limit.saturating_add(1),
                        );
                        if accepted > 0 {
                            complete_lines += tmp[..accepted]
                                .iter()
                                .filter(|&&b| b == b'\n')
                                .count();
                            stdout_buf.extend_from_slice(&tmp[..accepted]);
                        }

                        // Project + emit each newly completed line BEFORE the
                        // exact-fit probe below: the probe reads into `tmp`,
                        // overwriting the just-accepted bytes, so feeding after
                        // it would stream corrupted data (the terminal card is
                        // rebuilt from `stdout_buf`, but streamed deltas must
                        // stay a faithful prefix of it).
                        for p in streamer.feed(&tmp[..accepted]) {
                            yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
                        }

                        if hit_cap {
                            // Same short exact-fit probe as `read_rg_stdout_capped`.
                            // Use ONLY `EXACT_FIT_PROBE_TIMEOUT` — never the shared
                            // tool `deadline_at`. Clamping the probe to `deadline_at`
                            // and setting `timed_out` on expiry would force the
                            // timeout terminal branch (banner, exit -1) for a
                            // normal head-limit fill near the wall-clock edge.
                            if accepted < n {
                                stdout_truncated = true;
                            } else {
                                match tokio::time::timeout(
                                    EXACT_FIT_PROBE_TIMEOUT,
                                    stdout_pipe.read(&mut tmp),
                                )
                                .await
                                {
                                    Ok(Ok(0)) => stdout_truncated = false,
                                    Ok(Ok(_)) => stdout_truncated = true,
                                    Ok(Err(_)) => stdout_truncated = true,
                                    // Probe budget only: head-limit truncation path
                                    // (keep buffer, kill `rg` below). Never set
                                    // `timed_out` here.
                                    Err(_elapsed) => stdout_truncated = true,
                                }
                            }
                        }

                        // Also stop once the formatted body has hit its own
                        // head/byte budget (may trip before raw line count when
                        // max_output_bytes is small).
                        if hit_cap || streamer.done {
                            if streamer.done {
                                stdout_truncated = true;
                            }
                            break;
                        }
                    }
                }
            }
        }

        if let Some(error) = stdout_read_error {
            let _ = child.start_kill();
            crate::util::reap_killed_search_child(&mut child).await;
            if let Some(task) = stderr_task.take() {
                let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            }
            yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(
                echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id"),
                    format!("读取内置搜索组件输出失败：{error}"),
                )
                .with_source(error),
            ));
            return;
        }

        if timed_out {
            span.record("timed_out", true);
            span.record("early_kill", true);
            span.record("wall_ms", stream_started.elapsed().as_millis() as u64);
            let secs = timeout.as_secs();
            span.in_scope(|| {
                tracing::warn!(timeout_secs = secs, "grep timed out");
            });
            let _ = child.start_kill();
            crate::util::reap_killed_search_child(&mut child).await;
            if let Some(task) = stderr_task.take() {
                let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            }
            // Timeout: finalize what was read (marked truncated) plus an
            // explicit notice, so the stream isn't contradicted; with
            // nothing streamed, fall back to the timeout-only card.
            if stdout_buf.is_empty() {
                yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(grep_timeout_output(secs)));
            } else {
                if let Some(p) = streamer.finish() {
                    yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
                }
                let mut output = finalize_grep(stdout_buf, true, Vec::new(), 0, &config);
                output.stdout.extend_from_slice(
                    format!(
                        "\nRipgrep search timed out after {secs} seconds; \
                         the matches above are partial. Try searching a more specific \
                         path or pattern."
                    )
                    .as_bytes(),
                );
                output.exit_code = -1;
                yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(output));
            }
            return;
        }
        span.record("timed_out", false);

        // Flush the final non-terminated segment (see `BodyStreamer::finish`).
        if let Some(p) = streamer.finish() {
            yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
        }

        // Kill the child **before** draining stderr when we stopped early
        // (byte/line/format cap); rg may still be walking the tree and only
        // notices the closed stdout on its next write, so a stderr drain first
        // would stall until the deadline (up to the full timeout) even though we
        // already have a full budget.
        if stdout_truncated {
            let _ = child.start_kill();
        }

        // stderr is small and never streamed; still bounded by the shared
        // deadline as a backstop so a wedged child can't stall the stream.
        let stderr_buf = if let Some(task) = stderr_task.take() {
            match tokio::time::timeout(Duration::from_secs(1), task).await {
                Ok(Ok(Ok(stderr_buf))) => stderr_buf,
                Ok(Ok(Err(error))) => {
                    let _ = child.start_kill();
                    crate::util::reap_killed_search_child(&mut child).await;
                    yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(
                        echo_agent_tool_runtime::ToolError::execution(
                            echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id"),
                            format!("读取内置搜索组件错误信息失败：{error}"),
                        )
                        .with_source(error),
                    ));
                    return;
                }
                Ok(Err(error)) => {
                    let _ = child.start_kill();
                    crate::util::reap_killed_search_child(&mut child).await;
                    yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(
                        echo_agent_tool_runtime::ToolError::execution(
                            echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id"),
                            format!("收集内置搜索组件错误信息失败：{error}"),
                        ),
                    ));
                    return;
                }
                Err(_) => {
                    let _ = child.start_kill();
                    crate::util::reap_killed_search_child(&mut child).await;
                    yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(
                        grep_timeout_output(timeout.as_secs()),
                    ));
                    return;
                }
            }
        } else {
            Vec::new()
        };

        // Truncated output means `rg` was already killed above — bounded reap,
        // and the exit code is defined as 0. A natural EOF means rg is exiting,
        // so the plain wait is prompt.
        let exit_code = if stdout_truncated {
            crate::util::reap_killed_search_child(&mut child).await;
            0
        } else {
            match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
                Ok(Ok(status)) => status.code().unwrap_or(-1),
                Ok(Err(error)) => {
                    yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(
                        echo_agent_tool_runtime::ToolError::execution(
                            echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id"),
                            format!("等待内置搜索组件结束失败：{error}"),
                        )
                        .with_source(error),
                    ));
                    return;
                }
                Err(_) => {
                    let _ = child.start_kill();
                    crate::util::reap_killed_search_child(&mut child).await;
                    yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(
                        grep_timeout_output(timeout.as_secs()),
                    ));
                    return;
                }
            }
        };

        let wall_ms = stream_started.elapsed().as_millis() as u64;
        span.record("early_kill", stdout_truncated);
        span.record("wall_ms", wall_ms);
        span.in_scope(|| {
            tracing::info!(
                wall_ms,
                early_kill = stdout_truncated,
                effective_head_limit = config.effective_head_limit,
                exit_code,
                "grep finished"
            );
        });

        let output =
            finalize_grep(stdout_buf, stdout_truncated, stderr_buf, exit_code, &config);
        yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(output));
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Execution helpers (shared by `run` and `execute`)
// ───────────────────────────────────────────────────────────────────────────

/// Formatting/projection knobs resolved once in [`prepare_grep`] and consumed by
/// both the streamed body ([`BodyStreamer`]) and the terminal card
/// ([`finalize_grep`]) so the two never drift.
struct GrepFormatConfig {
    output_mode: OutputMode,
    /// Line/entry budget: model `head_limit` clamped to the per-mode cap, or
    /// the per-mode default when omitted. Always finite so we can kill `rg`
    /// once enough output is collected.
    effective_head_limit: usize,
    /// Per-line truncation width (`trim_line`).
    max_chars_per_line: usize,
    /// Cumulative body byte cap.
    max_output_bytes: usize,
    /// Stable display path used in the `<workspace_result …>` wrapper / errors.
    cwd_display: String,
}

/// A spawned ripgrep ready to be read, plus the resolved formatting config.
struct GrepReady {
    child: Child,
    stdout_pipe: Option<ChildStdout>,
    stderr_pipe: Option<ChildStderr>,
    config: GrepFormatConfig,
}

/// Outcome of [`prepare_grep`]: either a spawned process to read, or a fully
/// formed early result (path-not-found / spawn failure) that needs no reading.
#[allow(clippy::large_enum_variant)]
enum GrepStep {
    Ready(GrepReady),
    Early(GrepSearchOutput),
}

/// Resolve resources, build the ripgrep command, and spawn it; `Early` for
/// pre-read short-circuits. Shared by `run` and `execute`.
async fn prepare_grep(
    ctx: &echo_agent_tool_runtime::ToolCallContext,
    input: &GrepSearchInput,
) -> Result<GrepStep, echo_agent_tool_runtime::ToolError> {
    use crate::types::tool_metadata::{resolve_cwd, shared_resources};
    let resources = shared_resources(ctx)?;
    let cwd = resolve_cwd(ctx, &resources).await?;
    let (display_cwd, hints_enabled, deny_read_globs) = {
        let res = resources.lock().await;
        (
            res.get::<DisplayCwd>().map(|d| d.0.clone()),
            res.get::<PathNotFoundHints>().is_some_and(|h| h.0),
            res.get::<DenyReadGlobs>()
                .map(|d| d.0.clone())
                .unwrap_or_default(),
        )
    };

    // Resolve the model-provided path for the working directory.
    let workdir = resolve_model_path(
        &cwd,
        display_cwd.as_deref(),
        input.path.as_deref().unwrap_or(""),
    );
    // Use display_cwd for output paths so model sees stable paths.
    let display_base = display_cwd_or_cwd(&cwd, display_cwd.as_deref());
    let cwd_display = display_base.display().to_string();

    // Pre-check: if the search path doesn't exist, return enriched hints
    // before rg runs. We intentionally pre-check with metadata() rather
    // than parsing rg's stderr after the fact because rg lumps all errors
    // under exit code 2 (path not found, invalid regex, bad glob, unknown
    // file type, etc.). Distinguishing path-not-found would require
    // matching on OS error strings in stderr, which is fragile. The
    // pre-check avoids that and keeps the exit-code-2 handler below
    // unchanged for all other rg error classes.
    if input.path.is_some()
        && let Err(e) = tokio::fs::metadata(&workdir).await
        && e.kind() == std::io::ErrorKind::NotFound
    {
        let display_path = if let Ok(suffix) = workdir.strip_prefix(&cwd) {
            display_base.join(suffix)
        } else {
            workdir.clone()
        };
        let msg = crate::util::format_not_found_error(
            &display_path,
            &workdir,
            &cwd,
            &display_base,
            hints_enabled,
        )
        .await;
        return Ok(GrepStep::Early(GrepSearchOutput {
            stdout: msg.into_bytes(),
            stderr: Vec::new(),
            exit_code: 2,
            match_count: 0,
            file_matches: Vec::new(),
        }));
    }

    let output_mode = input.output_mode.clone().unwrap_or(OutputMode::Content);
    let effective_head_limit = resolve_effective_head_limit(input, &output_mode);

    let rg_exec = rg_path().map_err(|error| {
        echo_agent_tool_runtime::ToolError::service_unavailable(unavailable_message(&error))
            .with_source(error)
    })?;

    let mut cmd = Command::new(rg_exec);
    cmd.arg("--heading")
        .arg("--with-filename")
        .arg("--line-number")
        .arg("--color=never")
        .arg("--max-columns")
        .arg("1000")
        .arg("--max-columns-preview");

    if input.case_insensitive {
        cmd.arg("--ignore-case");
    }

    if let Some(glob) = &input.glob
        && !glob.is_empty()
    {
        cmd.arg("--glob").arg(glob);
    }

    // Managed Read-deny globs become ripgrep excludes so a search never reads
    // a policy-forbidden path — whether reached by a recursive walk or by a
    // `glob` arg that targets a denied file. Added AFTER the caller's `--glob`
    // so the exclude wins (ripgrep applies the last matching glob). An
    // explicitly-passed denied `path` is blocked earlier by the permission
    // manager (ripgrep searches explicit paths even against excludes).
    for deny in &deny_read_globs {
        cmd.arg("--glob").arg(format!("!{deny}"));
    }

    if let Some(t) = &input.r#type
        && !t.is_empty()
    {
        cmd.arg("--type").arg(t);
    }

    if input.multiline {
        cmd.arg("-U").arg("--multiline-dotall");
    }

    if let Some(c) = input.context
        && c > 0
    {
        cmd.arg("-C").arg(c.to_string());
    }
    if let Some(b) = input.before_context
        && b > 0
    {
        cmd.arg("-B").arg(b.to_string());
    }
    if let Some(a) = input.after_context
        && a > 0
    {
        cmd.arg("-A").arg(a.to_string());
    }

    match output_mode {
        OutputMode::FilesWithMatches => {
            cmd.arg("-l");
        }
        OutputMode::Count => {
            cmd.arg("-c");
        }
        OutputMode::Content => {}
    }

    cmd.arg("-e").arg(&input.pattern);
    cmd.arg(workdir.to_string_lossy().as_ref());
    cmd.arg("--max-filesize").arg("5M");

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::util::detach_search_command(&mut cmd);

    #[allow(clippy::disallowed_methods)]
    // search helper; killed and reaped with a bound on timeout/truncation,
    // abandoned to the orphan reaper if unreapable (D-state)
    let mut child = cmd.spawn().map_err(|error| {
        echo_agent_tool_runtime::ToolError::service_unavailable(unavailable_message(&error))
            .with_source(error)
    })?;

    // Take pipes so child remains accessible for cleanup on timeout.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    // Resolve truncation settings from tool-specific Params (static config; no
    // dependency on the rg output, so it is resolved up front).
    let params = resources
        .lock()
        .await
        .get::<Params<GrepParams>>()
        .cloned()
        .unwrap_or_default();
    let max_chars_per_line = params
        .0
        .max_chars_per_line
        .unwrap_or(DEFAULT_MAX_CHARS_PER_LINE);
    let max_output_bytes = params
        .0
        .max_output_bytes
        .unwrap_or(DEFAULT_TOOL_OUTPUT_BYTES);

    Ok(GrepStep::Ready(GrepReady {
        child,
        stdout_pipe,
        stderr_pipe,
        config: GrepFormatConfig {
            output_mode,
            effective_head_limit,
            max_chars_per_line,
            max_output_bytes,
            cwd_display,
        },
    }))
}

/// Longest prefix of `bytes` that ends on a UTF-8 character boundary.
///
/// Used when a hard *byte* budget would otherwise cut mid-code-unit; line-budget
/// stops already land on `\n` (ASCII), so they are always boundaries. Counting
/// lines by `b'\n'` is UTF-8-safe (newlines are never multi-byte).
fn utf8_char_boundary_prefix_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) => e.valid_up_to(),
    }
}

/// How many leading bytes of a newly-read `rg` chunk to accept, given the
/// running byte/line budgets. Returns `(accepted_len, hit_cap)`.
///
/// Stops at the first of: remaining room under [`MAX_STDOUT_BYTES`], or the
/// newline that brings complete line count to `max_lines`. Used by both the
/// blocking and streaming read loops so early-kill behavior cannot drift.
///
/// On a pure byte-cap stop (no line budget hit), the accepted slice is snapped
/// to a UTF-8 char boundary so we never append a partial multi-byte sequence
/// into `stdout_buf` (downstream uses `String::from_utf8_lossy`, but mid-char
/// cuts also break incremental `BodyStreamer` line assembly).
fn accept_rg_stdout_chunk(
    chunk: &[u8],
    buf_len: usize,
    complete_lines: usize,
    max_lines: usize,
) -> (usize, bool) {
    if chunk.is_empty() {
        return (0, false);
    }
    if complete_lines >= max_lines || buf_len >= MAX_STDOUT_BYTES {
        return (0, true);
    }

    let byte_room = MAX_STDOUT_BYTES - buf_len;
    let limited = &chunk[..chunk.len().min(byte_room)];
    let mut lines = complete_lines;
    for (i, &b) in limited.iter().enumerate() {
        if b == b'\n' {
            lines += 1;
            if lines >= max_lines {
                // Include the newline that filled the budget, then stop.
                // `\n` is a single-byte ASCII boundary — no UTF-8 snap needed.
                return (i + 1, true);
            }
        }
    }
    let hit_byte_cap = limited.len() < chunk.len();
    if hit_byte_cap {
        // Prefer a complete UTF-8 prefix over a mid-code-unit cut. If the entire
        // limited slice is an incomplete sequence (shouldn't happen when the
        // prior buffer always ends on a boundary), accept 0 and hit the cap.
        let safe = utf8_char_boundary_prefix_len(limited);
        return (safe, true);
    }
    (limited.len(), false)
}

/// Read `rg` stdout until EOF or a hard stop (byte cap / effective head_limit
/// lines). Callers should kill the child when the returned truncated flag is
/// set so `rg` does not keep walking the tree.
/// When the line budget is filled exactly and the next read is EOF, `truncated`
/// is **false** (exact fit). If more bytes remain after the budget, true.
///
/// The post-budget "exact-fit" probe is **time-bounded** ([`EXACT_FIT_PROBE_TIMEOUT`]).
/// An unbounded `read` would hold the outer tool timeout and, on expiry, drop the
/// already-buffered matches in favor of a timeout error card.
async fn read_rg_stdout_capped(
    mut stdout_pipe: ChildStdout,
    max_lines: usize,
) -> io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::with_capacity(MAX_STDOUT_BYTES.min(65_536));
    let mut complete_lines = 0usize;
    let mut truncated = false;
    let mut tmp = [0u8; 8192];
    loop {
        match stdout_pipe.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                let (accepted, hit_cap) =
                    accept_rg_stdout_chunk(&tmp[..n], buf.len(), complete_lines, max_lines);
                if accepted > 0 {
                    complete_lines += tmp[..accepted].iter().filter(|&&b| b == b'\n').count();
                    buf.extend_from_slice(&tmp[..accepted]);
                }
                if hit_cap {
                    if accepted < n {
                        truncated = true;
                    } else {
                        // Bounded probe: never wait for the full tool timeout here.
                        match tokio::time::timeout(
                            EXACT_FIT_PROBE_TIMEOUT,
                            stdout_pipe.read(&mut tmp),
                        )
                        .await
                        {
                            Ok(Ok(0)) => truncated = false,
                            Ok(Ok(_)) => truncated = true,
                            Ok(Err(_)) => truncated = true,
                            // No more data arrived quickly — assume overflow so the
                            // caller kills `rg` and keeps the buffer (do not escalate
                            // to the outer timeout path that drops matches).
                            Err(_elapsed) => truncated = true,
                        }
                    }
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok((buf, truncated))
}

/// Drain stderr to EOF while retaining only a bounded prefix. Continuing to
/// drain after the display budget is reached is essential: stopping at the cap
/// would let a noisy child fill the OS pipe and stall its stdout/exit path.
async fn read_rg_stderr_capped(mut stderr_pipe: ChildStderr) -> io::Result<Vec<u8>> {
    const MAX_STDERR_BYTES: usize = 1_000_000;

    let mut retained = Vec::with_capacity(64 * 1024);
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = stderr_pipe.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_STDERR_BYTES.saturating_sub(retained.len());
        if remaining > 0 {
            retained.extend_from_slice(&chunk[..read.min(remaining)]);
        }
    }
    Ok(retained)
}

/// Terminal card for a grep that exceeded its wall-clock timeout. Shared by the
/// blocking and streaming paths.
fn grep_timeout_output(secs: u64) -> GrepSearchOutput {
    GrepSearchOutput {
        stdout: format!(
            "Ripgrep search timed out after {secs} seconds. \
             The search may have matched files but did not complete in time. \
             Try searching a more specific path or pattern."
        )
        .into_bytes(),
        stderr: Vec::new(),
        exit_code: -1,
        match_count: 0,
        file_matches: Vec::new(),
    }
}

/// Build the authoritative terminal card from the fully-read rg output.
/// Single source of truth; the streamed body is a faithful prefix of it.
fn finalize_grep(
    stdout_buf: Vec<u8>,
    stdout_truncated: bool,
    stderr_buf: Vec<u8>,
    exit_code: i32,
    config: &GrepFormatConfig,
) -> GrepSearchOutput {
    let stdout = String::from_utf8_lossy(&stdout_buf);
    let stderr = String::from_utf8_lossy(&stderr_buf);

    // Handle exit codes.
    if (exit_code == 1 && stdout.is_empty())
        || (exit_code == 2 && stderr.contains("No files were searched"))
    {
        let result = format!(
            "<workspace_result workspace_path=\"{}\">\nNo matches found\n</workspace_result>",
            config.cwd_display
        );
        return GrepSearchOutput {
            stdout: result.into_bytes(),
            stderr: Vec::new(),
            // "No files were searched" is a no-match outcome, not an rg
            // execution failure. Normalize it so every downstream surface
            // derives the same status.
            exit_code: 1,
            match_count: 0,
            file_matches: Vec::new(),
        };
    }

    if exit_code == 2 {
        let error_msg = format!(
            "Error calling tool: {} (exit 2, root: {})",
            stderr, config.cwd_display
        );
        return GrepSearchOutput {
            stdout: error_msg.into_bytes(),
            stderr: stderr_buf,
            exit_code,
            match_count: 0,
            file_matches: Vec::new(),
        };
    }

    if exit_code != 0 {
        let error_msg = format!(
            "Error calling tool: unknown error (exit {}, root: {})",
            exit_code, config.cwd_display
        );
        return GrepSearchOutput {
            stdout: error_msg.into_bytes(),
            stderr: stderr_buf,
            exit_code,
            match_count: 0,
            file_matches: Vec::new(),
        };
    }

    let (formatted_output, match_count, file_matches) = {
        let mut output_lines: Vec<String> = stdout.lines().map(|s| s.to_string()).collect();
        let mut is_truncated = stdout_truncated;
        if output_lines.len() > config.effective_head_limit {
            is_truncated = true;
            output_lines.truncate(config.effective_head_limit);
        }

        let file_matches = if matches!(config.output_mode, OutputMode::Content) {
            parse_file_matches(&output_lines, config.max_chars_per_line)
        } else {
            Vec::new()
        };

        let match_count_value = match config.output_mode {
            OutputMode::Content => count_matches(&output_lines),
            OutputMode::FilesWithMatches => output_lines.len(),
            OutputMode::Count => {
                let mut sum_matches = 0usize;
                for line in &output_lines {
                    if let Some(count_str) = line.split(':').next_back()
                        && let Ok(count) = count_str.parse::<usize>()
                    {
                        sum_matches += count;
                    }
                }
                sum_matches
            }
        };

        let formatted = match config.output_mode {
            OutputMode::Content => format_content_output(
                output_lines,
                is_truncated,
                config.max_chars_per_line,
                config.max_output_bytes,
            ),
            OutputMode::FilesWithMatches => format_files_with_matches_output(
                output_lines,
                is_truncated,
                config.max_chars_per_line,
                config.max_output_bytes,
            ),
            OutputMode::Count => format_count_output(
                output_lines,
                is_truncated,
                config.max_chars_per_line,
                config.max_output_bytes,
            ),
        };
        (formatted, match_count_value, file_matches)
    };

    GrepSearchOutput {
        stdout: format!(
            "<workspace_result workspace_path=\"{}\">\n{}\n</workspace_result>",
            config.cwd_display, formatted_output
        )
        .into_bytes(),
        stderr: stderr_buf,
        exit_code,
        match_count,
        file_matches,
    }
}

/// Incremental builder for grep's streamed card body: raw stdout in via
/// [`BodyStreamer::feed`], flushed at EOF via [`BodyStreamer::finish`]. Each
/// line is projected exactly as [`finalize_grep`] projects the terminal body,
/// so the concatenated deltas equal the card body (prefix mode). Line
/// splitting matches `str::lines()` exactly (incl. trailing-`\r` handling).
struct BodyStreamer<'a> {
    spec: &'a echo_agent_tool_protocol::StreamingSpec,
    config: &'a GrepFormatConfig,
    /// Accumulated card body. Equals the body `finalize_grep` produces.
    body: String,
    /// Monotonic body bytes already surfaced as deltas.
    last_total: u64,
    /// Body lines emitted so far (drives the head-limit).
    emitted_lines: usize,
    /// Cumulative trimmed-line length (drives the byte-cap).
    cum_len: usize,
    /// Set once the head-limit or byte-cap is hit (body complete).
    done: bool,
    /// Bytes after the last newline — the in-progress line, carried across feeds.
    pending: Vec<u8>,
}

impl<'a> BodyStreamer<'a> {
    fn new(
        spec: &'a echo_agent_tool_protocol::StreamingSpec,
        config: &'a GrepFormatConfig,
    ) -> Self {
        Self {
            spec,
            config,
            body: String::new(),
            last_total: 0,
            emitted_lines: 0,
            cum_len: 0,
            done: false,
            pending: Vec::new(),
        }
    }

    /// Feed raw stdout; returns a delta per newly completed line. Partial
    /// trailing line is buffered. No-op once [`Self::done`].
    fn feed(&mut self, bytes: &[u8]) -> Vec<echo_agent_tool_runtime::ToolProgress> {
        let mut deltas = Vec::new();
        if self.done {
            return deltas;
        }
        self.pending.extend_from_slice(bytes);
        // Own the buffer to project lines from borrowed slices (no per-line
        // alloc); the unconsumed tail is carried forward at the end.
        let buf = std::mem::take(&mut self.pending);
        let mut start = 0;
        while let Some(rel) = buf[start..].iter().position(|&b| b == b'\n') {
            let nl = start + rel;
            let mut end = nl; // exclusive; drops the '\n'
            if end > start && buf[end - 1] == b'\r' {
                end -= 1; // drop the '\r' of a '\r\n' (matches `str::lines()`)
            }
            if let Some(p) = self.push_line(&buf[start..end]) {
                deltas.push(p);
            }
            start = nl + 1;
            if self.done {
                break;
            }
        }
        // Carry the in-progress (post-last-newline) bytes to the next feed.
        self.pending.extend_from_slice(&buf[start..]);
        deltas
    }

    /// Flush the final non-`\n`-terminated segment verbatim at EOF (matches
    /// `str::lines()`, which keeps a trailing `\r`).
    fn finish(&mut self) -> Option<echo_agent_tool_runtime::ToolProgress> {
        if self.done || self.pending.is_empty() {
            return None;
        }
        let line = std::mem::take(&mut self.pending);
        self.push_line(&line)
    }

    /// Project one line into the body; returns its delta. Sets [`Self::done`]
    /// at the head-limit or byte-cap.
    fn push_line(&mut self, line: &[u8]) -> Option<echo_agent_tool_runtime::ToolProgress> {
        // Head-limit (matches `finalize_grep`).
        if self.emitted_lines >= self.config.effective_head_limit {
            self.done = true;
            return None;
        }
        let line_str = String::from_utf8_lossy(line);
        let trimmed = trim_line(&line_str, self.config.max_chars_per_line);
        // Byte-cap; shares `exceeds_cum_byte_cap` with the batch path.
        if exceeds_cum_byte_cap(self.cum_len, trimmed.len(), self.config.max_output_bytes) {
            self.done = true;
            return None;
        }
        // Separator keyed off `emitted_lines` so a leading empty line still
        // gets one.
        if self.emitted_lines > 0 {
            self.body.push('\n');
        }
        self.body.push_str(&trimmed);
        self.cum_len += trimmed.len();
        self.emitted_lines += 1;
        echo_agent_tool_runtime::stream_chunk(
            self.spec,
            self.body.as_bytes(),
            self.body.len() as u64,
            &mut self.last_total,
            // No upstream cumulative truncation; only the per-tick `gap`.
            false,
        )
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Parsing & formatting helpers (free functions)
// ───────────────────────────────────────────────────────────────────────────

fn trim_line(line: &str, max_chars_per_line: usize) -> String {
    truncate_line(line, max_chars_per_line).into_owned()
}

/// Parse a ripgrep "numbered line" prefix: `123:content` or `45-context`.
///
/// `pub` so siblings can reuse the parser instead of
/// duplicating it -- avoids drift between the two namespaces' rg-output
/// reformatters.
pub fn parse_numbered_line_prefix(line: &str) -> Option<(usize, char, &str)> {
    let bytes = line.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == 0 || idx >= bytes.len() {
        return None;
    }

    let sep = bytes[idx] as char;
    if sep != ':' && sep != '-' {
        return None;
    }

    let line_number = line[..idx].parse::<usize>().ok()?;
    Some((line_number, sep, &line[idx + 1..]))
}

/// Parse ripgrep `--heading` output into structured per-file matches.
pub fn parse_file_matches(
    output_lines: &[String],
    max_chars_per_line: usize,
) -> Vec<GrepFileMatch> {
    let mut file_matches: Vec<GrepFileMatch> = Vec::new();
    let mut current_file: Option<GrepFileMatch> = None;

    let mut flush_current = |current: &mut Option<GrepFileMatch>| {
        if let Some(file) = current.take()
            && !file.matches.is_empty()
        {
            file_matches.push(file);
        }
    };

    for line in output_lines {
        let stripped = line.trim();
        if stripped.is_empty() {
            flush_current(&mut current_file);
            continue;
        }
        if stripped == "--" {
            continue;
        }

        if current_file.is_none() {
            current_file = Some(GrepFileMatch {
                path: stripped.to_owned(),
                matches: Vec::new(),
            });
            continue;
        }

        if let Some((line_number, sep, rest)) = parse_numbered_line_prefix(line) {
            if sep == ':'
                && let Some(ref mut file) = current_file
            {
                file.matches.push(GrepLineMatch {
                    line_number,
                    content: trim_line(rest, max_chars_per_line),
                });
            }
            continue;
        }

        flush_current(&mut current_file);
        current_file = Some(GrepFileMatch {
            path: stripped.to_owned(),
            matches: Vec::new(),
        });
    }

    flush_current(&mut current_file);
    file_matches
}

pub fn count_matches(output_lines: &[String]) -> usize {
    output_lines
        .iter()
        .filter(|line| parse_numbered_line_prefix(line).is_some_and(|(_, sep, _)| sep == ':'))
        .count()
}

/// Cumulative byte-cap check shared by the batch path and [`BodyStreamer`].
fn exceeds_cum_byte_cap(cum_len: usize, line_len: usize, max_output_bytes: usize) -> bool {
    cum_len + line_len > max_output_bytes
}

fn first_idx_exceed_cum_limit(lines: &[String], limit: usize) -> usize {
    let mut cum_len = 0;
    for (i, line) in lines.iter().enumerate() {
        if exceeds_cum_byte_cap(cum_len, line.len(), limit) {
            return i;
        }
        cum_len += line.len();
    }
    lines.len()
}

pub fn format_content_output(
    output_lines: Vec<String>,
    is_truncated: bool,
    max_chars_per_line: usize,
    max_output_bytes: usize,
) -> String {
    let is_truncated_str = if is_truncated { "at least " } else { "" };
    let num_matching_lines = count_matches(&output_lines);
    let mut final_output_lines = vec![format!(
        "Found {}{} matching lines",
        is_truncated_str, num_matching_lines
    )];

    let trimmed_lines: Vec<String> = output_lines
        .iter()
        .map(|line| trim_line(line, max_chars_per_line))
        .collect();

    let cut_idx = first_idx_exceed_cum_limit(&trimmed_lines, max_output_bytes);
    final_output_lines.extend_from_slice(&trimmed_lines[..cut_idx]);

    let remaining_matches = count_matches(&trimmed_lines[cut_idx..]);
    if remaining_matches > 0 {
        final_output_lines.push(format!(
            "... [{}{} lines truncated] ...",
            is_truncated_str, remaining_matches
        ));
    }

    final_output_lines.join("\n")
}

pub fn format_files_with_matches_output(
    output_lines: Vec<String>,
    is_truncated: bool,
    max_chars_per_line: usize,
    max_output_bytes: usize,
) -> String {
    let is_truncated_str = if is_truncated { "at least " } else { "" };
    let mut final_output_lines = vec![format!(
        "Found {}{} files",
        is_truncated_str,
        output_lines.len()
    )];

    let trimmed_lines: Vec<String> = output_lines
        .iter()
        .map(|line| trim_line(line, max_chars_per_line))
        .collect();

    let cut_idx = first_idx_exceed_cum_limit(&trimmed_lines, max_output_bytes);
    final_output_lines.extend_from_slice(&trimmed_lines[..cut_idx]);

    if output_lines.len() > cut_idx {
        final_output_lines.push(format!(
            "... [{}{} lines truncated] ...",
            is_truncated_str,
            output_lines.len() - cut_idx
        ));
    }

    final_output_lines.join("\n")
}

pub fn format_count_output(
    output_lines: Vec<String>,
    is_truncated: bool,
    max_chars_per_line: usize,
    max_output_bytes: usize,
) -> String {
    let is_truncated_str = if is_truncated { "at least " } else { "" };

    let mut sum_matches = 0;
    for line in &output_lines {
        if let Some(count_str) = line.split(':').next_back()
            && let Ok(count) = count_str.parse::<usize>()
        {
            sum_matches += count;
        }
    }

    let mut final_output_lines = vec![format!(
        "Found {} across {}{} files",
        sum_matches,
        is_truncated_str,
        output_lines.len()
    )];

    let trimmed_lines: Vec<String> = output_lines
        .iter()
        .map(|line| trim_line(line, max_chars_per_line))
        .collect();

    let cut_idx = first_idx_exceed_cum_limit(&trimmed_lines, max_output_bytes);
    final_output_lines.extend_from_slice(&trimmed_lines[..cut_idx]);

    if output_lines.len() > cut_idx {
        final_output_lines.push(format!(
            "... [{}{} lines truncated] ...",
            is_truncated_str,
            output_lines.len() - cut_idx
        ));
    }

    final_output_lines.join("\n")
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    use crate::types::resources::Resources;
    use std::fs;
    use tempfile::TempDir;

    fn make_grep_input(pattern: &str) -> GrepSearchInput {
        GrepSearchInput {
            pattern: pattern.to_string(),
            path: None,
            glob: None,
            output_mode: None,
            before_context: None,
            after_context: None,
            context: None,
            case_insensitive: false,
            r#type: None,
            head_limit: None,
            multiline: false,
        }
    }

    /// Boolean flags must be non-optional in the model-facing schema so the
    /// default is unambiguous (`false`, not `null` + "Default: false" prose).
    #[test]
    fn grep_bool_flags_schema_is_plain_boolean_with_default_false() {
        let schema = serde_json::to_value(schemars::schema_for!(GrepSearchInput)).unwrap();
        let props = &schema["properties"];

        // Field is renamed to "-i" for the model-facing name.
        let case = &props["-i"];
        assert_eq!(case["type"], "boolean", "case_insensitive schema: {case}");
        assert_eq!(case["default"], false, "case_insensitive schema: {case}");
        assert!(
            case.get("anyOf").is_none(),
            "must not use nullable anyOf: {case}"
        );

        let multi = &props["multiline"];
        assert_eq!(multi["type"], "boolean", "multiline schema: {multi}");
        assert_eq!(multi["default"], false, "multiline schema: {multi}");
        assert!(
            multi.get("anyOf").is_none(),
            "must not use nullable anyOf: {multi}"
        );
    }

    #[test]
    fn grep_bool_flags_deserialize_missing_and_null_as_false() {
        let missing: GrepSearchInput = serde_json::from_str(r#"{"pattern":"foo"}"#).unwrap();
        assert!(!missing.case_insensitive);
        assert!(!missing.multiline);

        let nulls: GrepSearchInput =
            serde_json::from_str(r#"{"pattern":"foo","-i":null,"multiline":null}"#).unwrap();
        assert!(!nulls.case_insensitive);
        assert!(!nulls.multiline);

        let truths: GrepSearchInput =
            serde_json::from_str(r#"{"pattern":"foo","-i":"yes","multiline":1}"#).unwrap();
        assert!(truths.case_insensitive);
        assert!(truths.multiline);
    }

    #[test]
    fn grep_timeout_secs_platform_defaults() {
        assert_eq!(grep_timeout_secs(false), 20);
        assert_eq!(grep_timeout_secs(true), 60);
    }

    #[test]
    fn grep_timeout_output_is_error_with_guidance() {
        let out = grep_timeout_output(20);
        assert_eq!(out.exit_code, -1);
        assert_eq!(out.match_count, 0);
        assert!(out.file_matches.is_empty());
        let msg = String::from_utf8_lossy(&out.stdout);
        assert!(msg.contains("timed out after 20 seconds"), "msg: {msg}");
        assert!(msg.contains("did not complete in time"), "msg: {msg}");
        assert!(msg.contains("more specific path or pattern"), "msg: {msg}");
    }

    #[test]
    fn grep_search_input_schema_omits_output_mode() {
        let schema = crate::registry::types::generate_schema::<GrepSearchInput>();
        let props = schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("schema has properties");
        assert!(!props.contains_key("output_mode"));
    }

    #[test]
    fn grep_search_input_deserializes_output_mode_variants() {
        let content: GrepSearchInput =
            serde_json::from_value(serde_json::json!({"pattern": "x", "output_mode": "content"}))
                .unwrap();
        assert_eq!(content.output_mode, Some(OutputMode::Content));

        let files: GrepSearchInput = serde_json::from_value(serde_json::json!({
            "pattern": "x",
            "output_mode": "files_with_matches"
        }))
        .unwrap();
        assert_eq!(files.output_mode, Some(OutputMode::FilesWithMatches));

        let count: GrepSearchInput =
            serde_json::from_value(serde_json::json!({"pattern": "x", "output_mode": "count"}))
                .unwrap();
        assert_eq!(count.output_mode, Some(OutputMode::Count));

        let omitted: GrepSearchInput =
            serde_json::from_value(serde_json::json!({"pattern": "x"})).unwrap();
        assert_eq!(omitted.output_mode, None);
    }

    #[test]
    fn test_parse_numbered_line_prefix() {
        assert_eq!(
            parse_numbered_line_prefix("123:content"),
            Some((123, ':', "content"))
        );
        assert_eq!(
            parse_numbered_line_prefix("45-context line"),
            Some((45, '-', "context line"))
        );
        assert_eq!(parse_numbered_line_prefix("not a match"), None);
        assert_eq!(parse_numbered_line_prefix(""), None);
    }

    #[test]
    fn test_trim_line() {
        assert_eq!(trim_line("short", DEFAULT_MAX_CHARS_PER_LINE), "short");

        let long_line = "a".repeat(2000);
        let trimmed = trim_line(&long_line, DEFAULT_MAX_CHARS_PER_LINE);
        assert!(trimmed.len() < long_line.len());
        assert!(trimmed.contains("[... truncated"));
    }

    #[test]
    fn test_parse_file_matches() {
        let lines: Vec<String> = vec![
            "src/main.rs",
            "10:fn main() {",
            "15:    println!(\"hello\");",
            "",
            "src/lib.rs",
            "5:pub fn greet() {",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let matches = parse_file_matches(&lines, DEFAULT_MAX_CHARS_PER_LINE);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].path, "src/main.rs");
        assert_eq!(matches[0].matches.len(), 2);
        assert_eq!(matches[0].matches[0].line_number, 10);
        assert_eq!(matches[1].path, "src/lib.rs");
        assert_eq!(matches[1].matches.len(), 1);
    }

    #[test]
    fn test_count_matches() {
        let lines: Vec<String> = vec!["10:match", "11-context", "12:match", ""]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(count_matches(&lines), 2);
    }

    #[test]
    fn test_format_content_output_not_truncated() {
        let lines: Vec<String> = vec!["src/main.rs", "10:fn main() {", ""]
            .into_iter()
            .map(String::from)
            .collect();

        let result = format_content_output(
            lines,
            false,
            DEFAULT_MAX_CHARS_PER_LINE,
            DEFAULT_TOOL_OUTPUT_BYTES,
        );
        assert!(result.starts_with("Found 1 matching lines"));
    }

    #[test]
    fn test_format_content_output_truncated() {
        let lines: Vec<String> = vec!["src/main.rs", "10:fn main() {"]
            .into_iter()
            .map(String::from)
            .collect();

        let result = format_content_output(
            lines,
            true,
            DEFAULT_MAX_CHARS_PER_LINE,
            DEFAULT_TOOL_OUTPUT_BYTES,
        );
        assert!(result.starts_with("Found at least 1 matching lines"));
    }

    #[test]
    fn tool_name_and_description() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = GrepTool;
        assert_eq!(echo_agent_tool_runtime::Tool::id(&tool).as_str(), "grep");
        assert!(tool.description_template().contains("ripgrep"));
        assert!(tool.description_template().contains("regex"));
    }

    #[test]
    fn description_template_tracks_renamed_search_params() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use crate::types::tool_metadata::ToolMetadata;
        use std::collections::HashMap;

        let tools = HashMap::from([(ToolKind::Search, "grep".to_string())]);
        let params = HashMap::from([(
            ToolKind::Search,
            HashMap::from([
                ("pattern".to_string(), "query".to_string()),
                ("type".to_string(), "filetype".to_string()),
                ("glob".to_string(), "include".to_string()),
            ]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&GrepTool))
            .unwrap();
        assert!(
            rendered.contains("Pass query as a raw regex")
                && rendered.contains("'filetype'")
                && rendered.contains("'include'"),
            "renamed search params must appear:\n{rendered}"
        );
        assert!(
            !rendered.contains("Pass pattern as")
                && !rendered.contains("'type'")
                && !rendered.contains("'glob'"),
            "canonical search param names must not remain after rename:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn tool_grep_no_matches() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("hello.txt"), "hello world\n").unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let tool = GrepTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_grep_input("nonexistent_xyz_pattern"),
        )
        .await
        .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("No matches found"));
        assert_eq!(output.match_count, 0);
    }

    #[tokio::test]
    async fn tool_grep_finds_matches() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let tool = GrepTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_grep_input("main"),
        )
        .await
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert!(output.match_count > 0);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("main"));
    }

    /// `DenyReadGlobs` become ripgrep excludes so a search can't read a
    /// read-denied path — neither via a recursive walk (non-dotfile secrets like
    /// `key.pem`) nor via a `glob` arg that targets a denied file.
    #[tokio::test]
    async fn deny_read_globs_exclude_denied_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".env"), "FAKE_SECRET=zzz\n").unwrap();
        fs::write(tmp.path().join("key.pem"), "FAKE in pem\n").unwrap();
        fs::write(tmp.path().join("README.md"), "FAKE in readme\n").unwrap();

        let build = |glob: Option<&str>, deny: &[&str]| {
            let mut resources = Resources::new();
            resources.insert(Cwd(tmp.path().to_path_buf()));
            resources.insert(DenyReadGlobs(deny.iter().map(|s| s.to_string()).collect()));
            let mut input = make_grep_input("FAKE");
            input.glob = glob.map(str::to_string);
            (resources, input)
        };
        let run = |resources: Resources, input| async {
            let out = echo_agent_tool_runtime::Tool::run(
                &GrepTool,
                test_ctx(resources.into_shared()),
                input,
            )
            .await
            .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        // Recursive walk: the read-denied key.pem (a non-dotfile a plain grep
        // would read) is excluded, while a non-denied file still matches.
        let (r, i) = build(None, &["**/*.pem", "**/.env"]);
        let out = run(r, i).await;
        assert!(
            out.contains("README.md"),
            "non-denied file must match: {out}"
        );
        assert!(
            !out.contains("key.pem"),
            "deny glob must exclude key.pem: {out}"
        );

        // A `glob` arg can't target a denied file: the deny exclude overrides it.
        let (r, i) = build(Some(".env"), &["**/.env"]);
        let out = run(r, i).await;
        assert!(
            !out.contains("FAKE_SECRET"),
            "deny glob must beat glob-arg .env: {out}"
        );

        // Control: the same glob arg DOES read .env when no deny globs apply.
        let (r, i) = build(Some(".env"), &[]);
        let out = run(r, i).await;
        assert!(
            out.contains("FAKE_SECRET"),
            "control: glob arg reads .env w/o deny: {out}"
        );
    }

    /// Over-block: deny globs must spare look-alikes whose names resemble a
    /// denied pattern but don't match it (`server.pem.bak`, `notes.env.md`, …).
    #[tokio::test]
    async fn deny_read_globs_spare_glob_boundary_lookalikes() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        // Real denied secrets (non-dotfiles, so a plain grep would read them).
        fs::write(tmp.path().join("src/server.pem"), "FAKE\n").unwrap();
        fs::write(tmp.path().join("terraform.tfstate"), "FAKE\n").unwrap();
        // Look-alikes that must NOT match any deny glob.
        fs::write(tmp.path().join("server.pem.bak"), "FAKE\n").unwrap();
        fs::write(tmp.path().join("notes.env.md"), "FAKE\n").unwrap();
        fs::write(tmp.path().join("env.sample"), "FAKE\n").unwrap();
        fs::write(tmp.path().join("keystore.txt"), "FAKE\n").unwrap();

        let deny = [
            "**/.env",
            "**/.env.*",
            "**/*.pem",
            "**/*.key",
            "**/*.keystore",
            "**/terraform.tfstate",
        ];
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(DenyReadGlobs(deny.iter().map(|s| s.to_string()).collect()));
        let out = String::from_utf8_lossy(
            &echo_agent_tool_runtime::Tool::run(
                &GrepTool,
                test_ctx(resources.into_shared()),
                make_grep_input("FAKE"),
            )
            .await
            .unwrap()
            .stdout,
        )
        .into_owned();

        // Denied secrets stay excluded.
        assert!(
            !out.contains("src/server.pem"),
            "deny must exclude src/server.pem: {out}"
        );
        assert!(
            !out.contains("terraform.tfstate"),
            "deny must exclude terraform.tfstate: {out}"
        );
        // Look-alikes must not be over-blocked.
        for legit in [
            "server.pem.bak",
            "notes.env.md",
            "env.sample",
            "keystore.txt",
        ] {
            assert!(
                out.contains(legit),
                "look-alike `{legit}` wrongly over-blocked: {out}"
            );
        }
    }

    /// Subdir leak: a `**/` deny glob must still exclude a denied file when the
    /// search root is a subdirectory, not just the cwd.
    #[tokio::test]
    async fn deny_read_globs_exclude_in_subdir_search_root() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/server.pem"), "FAKE\n").unwrap();
        fs::write(tmp.path().join("src/main.rs"), "FAKE\n").unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(DenyReadGlobs(vec!["**/*.pem".to_string()]));
        let mut input = make_grep_input("FAKE");
        input.path = Some("src".to_string());
        let out = String::from_utf8_lossy(
            &echo_agent_tool_runtime::Tool::run(
                &GrepTool,
                test_ctx(resources.into_shared()),
                input,
            )
            .await
            .unwrap()
            .stdout,
        )
        .into_owned();

        assert!(
            out.contains("main.rs"),
            "legit subdir file must match: {out}"
        );
        assert!(
            !out.contains("server.pem"),
            "deny glob must exclude in subdir search root: {out}"
        );
    }

    /// No-deny regression: a user with no Read-deny rules injects no `DenyReadGlobs`,
    /// so the grep tool excludes nothing — even secret-looking files (`*.pem`) stay
    /// searchable. Guards against over-blocking unrestricted users.
    #[tokio::test]
    async fn no_deny_globs_does_not_block_denied_looking_files() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/server.pem"), "FAKE\n").unwrap();
        fs::write(tmp.path().join("README.md"), "FAKE\n").unwrap();

        // No DenyReadGlobs resource inserted — the no-deny-list user.
        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let out = String::from_utf8_lossy(
            &echo_agent_tool_runtime::Tool::run(
                &GrepTool,
                test_ctx(resources.into_shared()),
                make_grep_input("FAKE"),
            )
            .await
            .unwrap()
            .stdout,
        )
        .into_owned();

        assert!(
            out.contains("server.pem"),
            "no deny list must not exclude server.pem: {out}"
        );
        assert!(
            out.contains("README.md"),
            "non-denied file must match: {out}"
        );
    }

    #[tokio::test]
    async fn tool_works_through_runtime_trait() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("file.txt"), "findme here\n").unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let tool = GrepTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_grep_input("findme"),
        )
        .await
        .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.match_count > 0);
    }

    #[tokio::test]
    async fn tool_uses_params_for_truncation() {
        let tmp = TempDir::new().unwrap();
        // Create a file with many matching lines
        let content: String = (0..100).map(|i| format!("match_line_{}\n", i)).collect();
        fs::write(tmp.path().join("big.txt"), &content).unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        // Set a very small output limit
        resources.insert(Params(GrepParams {
            max_output_bytes: Some(200),
            max_chars_per_line: None,
        }));

        let tool = GrepTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_grep_input("match_line"),
        )
        .await
        .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("truncated"),
            "output should be truncated with small max_output_bytes, got: {}",
            stdout
        );
    }

    /// Explicit `head_limit` truncates the card and stops the search early.
    #[tokio::test]
    async fn tool_grep_head_limit_truncates() {
        let tmp = TempDir::new().unwrap();
        // Many match lines so an unbounded read would exceed a tiny head_limit.
        let content: String = (0..200).map(|i| format!("findme_{i}\n")).collect();
        fs::write(tmp.path().join("many.txt"), &content).unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let mut input = make_grep_input("findme_");
        input.head_limit = Some(5);

        let output =
            echo_agent_tool_runtime::Tool::run(&GrepTool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.exit_code, 0);
        assert!(
            stdout.contains("truncated") || output.match_count <= 5,
            "expected head_limit truncation, got: {stdout}"
        );
        // Heading line + a few match lines — well under the full 200 hits.
        let body_lines = stdout.lines().count();
        assert!(
            body_lines < 50,
            "head_limit should keep the card small, got {body_lines} lines: {stdout}"
        );
    }

    /// A result whose rg output-line count exactly equals `head_limit` is
    /// complete, not truncated: early-stop reads one line past the budget, so an
    /// exact-fit search reaches EOF without tripping the cap. Regression against
    /// the early-stop path over-reporting "at least N" on an exact fit.
    /// (Grouped rg output for one file = 1 heading line + K match lines, so
    /// `head_limit = K + 1` is the exact fit.)
    #[tokio::test]
    async fn tool_grep_head_limit_exact_fit_not_truncated() {
        let tmp = TempDir::new().unwrap();
        // One file, exactly 5 matching lines → 6 rg output lines (heading + 5).
        let content: String = (0..5).map(|i| format!("findme_{i}\n")).collect();
        fs::write(tmp.path().join("exact.txt"), &content).unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let mut input = make_grep_input("findme_");
        input.head_limit = Some(6);

        let output =
            echo_agent_tool_runtime::Tool::run(&GrepTool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.match_count, 5, "all 5 matches counted: {stdout}");
        assert!(
            !stdout.contains("truncated") && !stdout.contains("at least"),
            "exact-fit result must not be marked truncated: {stdout}"
        );
    }

    #[tokio::test]
    async fn tool_grep_files_with_matches_mode() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "findme\n").unwrap();
        fs::write(tmp.path().join("b.txt"), "findme\n").unwrap();
        fs::write(tmp.path().join("c.txt"), "nothing\n").unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let tool = GrepTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepSearchInput {
                pattern: "findme".to_string(),
                output_mode: Some(OutputMode::FilesWithMatches),
                ..GrepSearchInput {
                    pattern: String::new(),
                    path: None,
                    glob: None,
                    output_mode: None,
                    before_context: None,
                    after_context: None,
                    context: None,
                    case_insensitive: false,
                    r#type: None,
                    head_limit: None,
                    multiline: false,
                }
            },
        )
        .await
        .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Found 2 files"), "got: {}", stdout);
    }

    #[tokio::test]
    async fn tool_grep_count_mode() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "findme\nfindme\n").unwrap();
        fs::write(tmp.path().join("b.txt"), "findme\n").unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let tool = GrepTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepSearchInput {
                pattern: "findme".to_string(),
                output_mode: Some(OutputMode::Count),
                ..GrepSearchInput {
                    pattern: String::new(),
                    path: None,
                    glob: None,
                    output_mode: None,
                    before_context: None,
                    after_context: None,
                    context: None,
                    case_insensitive: false,
                    r#type: None,
                    head_limit: None,
                    multiline: false,
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(output.match_count, 3);
    }

    #[tokio::test]
    async fn tool_grep_with_path_subdir() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.txt"), "secret_value\n").unwrap();
        fs::write(tmp.path().join("root.txt"), "other_value\n").unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let tool = GrepTool;
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepSearchInput {
                pattern: "secret_value".to_string(),
                path: Some("subdir".to_string()),
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                context: None,
                case_insensitive: false,
                r#type: None,
                head_limit: None,
                multiline: false,
            },
        )
        .await
        .unwrap();

        assert!(output.match_count > 0);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("secret_value"));
    }

    // ─── Streaming (GrepTool::execute) tests ───
    //
    // `test_ctx` stamps `WorkspaceViewerContext { stream_tool_progress: true }`,
    // so these exercise the streaming path.

    /// Destructure a `grep_match_chunk` payload, asserting the canonical
    /// `plain_text` / `append` envelope. Returns the `delta`.
    fn read_grep_delta(p: &echo_agent_tool_runtime::ToolProgress) -> String {
        match p {
            echo_agent_tool_runtime::ToolProgress::Custom { subkind, payload } => {
                assert_eq!(subkind, "grep_match_chunk", "unexpected subkind");
                payload["delta"].as_str().unwrap().to_owned()
            }
            other => panic!("expected Custom progress, got {other:?}"),
        }
    }

    /// Drive a `BodyStreamer` over `raw` (one synthetic read + flush) and return
    /// the concatenation of every emitted delta — i.e. the streamed card body.
    /// Feeding the whole buffer at once is equivalent to chunked feeds (the
    /// pending buffer stitches partial lines), so this exercises the same
    /// projection `execute` runs incrementally.
    fn stream_body(raw: &[u8], config: &GrepFormatConfig) -> String {
        let spec = GREP_CAPABILITIES.streaming.as_ref().unwrap();
        let mut streamer = BodyStreamer::new(spec, config);
        let mut body = String::new();
        for p in streamer.feed(raw) {
            body.push_str(&read_grep_delta(&p));
        }
        if let Some(p) = streamer.finish() {
            body.push_str(&read_grep_delta(&p));
        }
        body
    }

    /// Extract the card *body* from a `finalize_grep` card by string slicing
    /// (NOT `str::lines()`, which would strip a trailing `\r` off a body line and
    /// thereby hide the very divergence these tests guard). Drops the
    /// `<workspace_result>` wrapper, the "Found …" summary (first line), and an
    /// optional `... [N lines truncated] ...` footer (last line).
    fn card_body(card: &str) -> String {
        let after_open = &card[card.find('\n').expect("wrapper newline") + 1..];
        let formatted = after_open
            .strip_suffix("\n</workspace_result>")
            .expect("wrapper close");
        // Drop the summary (first line).
        let body_and_footer = formatted.split_once('\n').map_or("", |(_, rest)| rest);
        // Drop an optional trailing footer line.
        if let Some((body, last)) = body_and_footer.rsplit_once('\n')
            && last.starts_with("... [")
            && last.ends_with("] ...")
        {
            return body.to_string();
        }
        body_and_footer.to_string()
    }

    fn grep_config(
        output_mode: OutputMode,
        max_output_bytes: usize,
        effective_head_limit: Option<usize>,
    ) -> GrepFormatConfig {
        let default_budget = match output_mode {
            OutputMode::Content => CONTENT_LINE_DEFAULT,
            OutputMode::FilesWithMatches | OutputMode::Count => FILE_COUNT_DEFAULT,
        };
        GrepFormatConfig {
            output_mode,
            // Tests pass `None` for "use the production default budget".
            effective_head_limit: effective_head_limit.unwrap_or(default_budget),
            max_chars_per_line: DEFAULT_MAX_CHARS_PER_LINE,
            max_output_bytes,
            cwd_display: "/ws".to_string(),
        }
    }

    #[test]
    fn resolve_effective_head_limit_defaults_when_omitted() {
        let mut input = make_grep_input("x");
        input.head_limit = None;
        assert_eq!(
            resolve_effective_head_limit(&input, &OutputMode::Content),
            CONTENT_LINE_DEFAULT
        );
        assert_eq!(
            resolve_effective_head_limit(&input, &OutputMode::FilesWithMatches),
            FILE_COUNT_DEFAULT
        );
        input.head_limit = Some(50);
        assert_eq!(
            resolve_effective_head_limit(&input, &OutputMode::Content),
            50
        );
        input.head_limit = Some(CONTENT_LINE_LIMIT + 999);
        assert_eq!(
            resolve_effective_head_limit(&input, &OutputMode::Content),
            CONTENT_LINE_LIMIT
        );
        // Explicit value between default and hard max is honored.
        input.head_limit = Some(800);
        assert_eq!(
            resolve_effective_head_limit(&input, &OutputMode::Content),
            800
        );
    }

    #[test]
    fn accept_rg_stdout_chunk_stops_at_line_budget() {
        let chunk = b"a\nb\nc\nd\n";
        let (n, hit) = accept_rg_stdout_chunk(chunk, 0, 0, 2);
        assert!(hit);
        assert_eq!(&chunk[..n], b"a\nb\n");
    }

    #[test]
    fn accept_rg_stdout_chunk_stops_at_byte_budget() {
        // MAX_STDOUT_BYTES is huge; simulate an already-full buffer.
        let chunk = b"more\n";
        let (n, hit) = accept_rg_stdout_chunk(chunk, MAX_STDOUT_BYTES, 0, 100);
        assert_eq!(n, 0);
        assert!(hit);
    }

    /// Byte-cap must not cut mid multi-byte UTF-8 sequence (e.g. "é" = C3 A9).
    #[test]
    fn accept_rg_stdout_chunk_byte_cap_snaps_to_utf8_boundary() {
        // One byte of room left, but next char is 2-byte UTF-8.
        let chunk = "é\n".as_bytes(); // [0xC3, 0xA9, 0x0A]
        assert_eq!(chunk.len(), 3);
        let (n, hit) = accept_rg_stdout_chunk(chunk, MAX_STDOUT_BYTES - 1, 0, 100);
        assert!(hit, "must hit byte cap");
        assert_eq!(
            n, 0,
            "must not accept a leading incomplete UTF-8 byte; got {n}"
        );

        // Two bytes of room: full "é", drop the trailing newline for this test's
        // room — actually 2 bytes fits "é" exactly.
        let (n2, hit2) = accept_rg_stdout_chunk(chunk, MAX_STDOUT_BYTES - 2, 0, 100);
        assert!(hit2);
        assert_eq!(&chunk[..n2], "é".as_bytes());
    }

    #[test]
    fn accept_rg_stdout_chunk_line_budget_includes_ascii_newline_boundary() {
        // Multi-byte content + newline: line stop lands on `\n` (safe boundary).
        let chunk = "café\nmore\n".as_bytes();
        let (n, hit) = accept_rg_stdout_chunk(chunk, 0, 0, 1);
        assert!(hit);
        assert_eq!(&chunk[..n], "café\n".as_bytes());
        assert!(std::str::from_utf8(&chunk[..n]).is_ok());
    }

    /// Regression: a stdout truncation landing mid-CRLF leaves a final
    /// segment with no trailing `\n` that ends in `\r`. `str::lines()` (used by
    /// `finalize_grep`) keeps that `\r`, so the streamed body must too — the final
    /// flush must NOT strip it. (Without the fix the streamed body would drop the
    /// `\r` and diverge from the terminal card body by one byte.)
    #[test]
    fn body_streamer_keeps_final_crlf_segment_like_str_lines() {
        // Last segment "3:gamma\r" has no trailing '\n' (truncated mid-CRLF).
        let raw = b"src/a.rs\n1:alpha\r\n2:beta\r\n3:gamma\r";
        let config = grep_config(OutputMode::Content, DEFAULT_TOOL_OUTPUT_BYTES, None);

        let streamed = stream_body(raw, &config);
        let card = String::from_utf8_lossy(
            &finalize_grep(raw.to_vec(), false, Vec::new(), 0, &config).stdout,
        )
        .into_owned();

        assert_eq!(
            streamed,
            card_body(&card),
            "streamed body must byte-match the terminal card body on a CRLF final segment"
        );
        assert!(
            streamed.ends_with("3:gamma\r"),
            "the final `\\r` must be preserved (matches str::lines()): {streamed:?}"
        );
    }

    /// Byte-cap truncation appends a `... [N lines truncated] ...`
    /// footer. The streamed body equals `trimmed_lines[..cut_idx]` — excluding
    /// BOTH the summary and the footer (both terminal-only).
    #[test]
    fn body_streamer_matches_card_body_with_bytecap_footer() {
        let raw = b"f.txt\n1:aaaaaaaaaa\n2:bbbbbbbbbb\n3:cccccccccc\n4:dddddddddd\n";
        // Small cap so the cut lands mid-output and a footer is appended.
        let config = grep_config(OutputMode::Content, 30, None);

        let streamed = stream_body(raw, &config);
        let card = String::from_utf8_lossy(
            &finalize_grep(raw.to_vec(), false, Vec::new(), 0, &config).stdout,
        )
        .into_owned();

        assert!(
            card.contains("lines truncated]"),
            "expected a truncation footer in the card: {card}"
        );
        assert_eq!(
            streamed,
            card_body(&card),
            "streamed body excludes the terminal-only footer"
        );
        assert!(
            !streamed.contains("truncated"),
            "footer must be terminal-only: {streamed:?}"
        );
    }

    /// The body projection is mode-independent — `files_with_matches`
    /// and `count` bodies (which carry a different summary) also byte-match.
    #[test]
    fn body_streamer_matches_card_body_count_and_files_modes() {
        let files_raw = b"src/a.rs\nsrc/b.rs\nsrc/c.rs\n";
        let files_cfg = grep_config(
            OutputMode::FilesWithMatches,
            DEFAULT_TOOL_OUTPUT_BYTES,
            None,
        );
        let files_card = String::from_utf8_lossy(
            &finalize_grep(files_raw.to_vec(), false, Vec::new(), 0, &files_cfg).stdout,
        )
        .into_owned();
        assert!(files_card.contains("Found 3 files"), "card: {files_card}");
        assert_eq!(stream_body(files_raw, &files_cfg), card_body(&files_card));

        let count_raw = b"src/a.rs:3\nsrc/b.rs:2\n";
        let count_cfg = grep_config(OutputMode::Count, DEFAULT_TOOL_OUTPUT_BYTES, None);
        let count_card = String::from_utf8_lossy(
            &finalize_grep(count_raw.to_vec(), false, Vec::new(), 0, &count_cfg).stdout,
        )
        .into_owned();
        assert!(
            count_card.contains("Found 5 across 2 files"),
            "card: {count_card}"
        );
        assert_eq!(stream_body(count_raw, &count_cfg), card_body(&count_card));
    }

    /// Streamed-vs-terminal contract: the concatenation of the
    /// per-match-line deltas equals the terminal card *body* (prefix mode), while
    /// the terminal result additionally carries the `<workspace_result …>`
    /// wrapper and the "Found N …" summary (terminal-only footer).
    #[tokio::test]
    async fn grep_streaming_body_matches_card_body() {
        use futures::StreamExt;

        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("matches.txt"),
            "alpha match one\nbeta match two\ngamma match three\n",
        )
        .unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let tool = GrepTool;
        let mut stream = echo_agent_tool_runtime::Tool::execute(
            &tool,
            test_ctx(resources.into_shared()),
            make_grep_input("match"),
        )
        .await;

        let mut deltas = String::new();
        let mut progress = 0usize;
        let mut terminal: Option<Result<GrepSearchOutput, echo_agent_tool_runtime::ToolError>> =
            None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(p) => {
                    assert!(terminal.is_none(), "Progress arrived after Terminal");
                    deltas.push_str(&read_grep_delta(&p));
                    progress += 1;
                }
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    assert!(terminal.is_none(), "more than one Terminal yielded");
                    terminal = Some(r);
                }
            }
        }

        assert!(
            progress >= 1,
            "expected at least one grep_match_chunk delta, got {progress}"
        );
        let output = terminal
            .expect("stream ended without a Terminal")
            .expect("grep terminal ok");
        let card = String::from_utf8_lossy(&output.stdout);

        // Terminal card = wrapper + summary + body (the full formatted output).
        assert!(card.starts_with("<workspace_result "), "card: {card}");
        assert!(card.contains("Found 3 matching lines"), "card: {card}");
        assert!(
            card.trim_end().ends_with("</workspace_result>"),
            "card: {card}"
        );

        // The streamed deltas are exactly the card body — the match lines between
        // the summary (line 1) and the closing wrapper (last line) — with neither
        // the wrapper nor the "Found N …" summary.
        let card_lines: Vec<&str> = card.lines().collect();
        let body_from_card = card_lines[2..card_lines.len() - 1].join("\n");
        assert_eq!(
            deltas, body_from_card,
            "accumulated deltas must equal the terminal card body"
        );

        // Sanity: the body carries the matches but not the terminal-only footer.
        assert!(deltas.contains("alpha match one"), "deltas: {deltas}");
        assert!(deltas.contains("gamma match three"), "deltas: {deltas}");
        assert!(!deltas.contains("<workspace_result"), "deltas: {deltas}");
        assert!(
            !deltas.contains("Found 3 matching lines"),
            "deltas: {deltas}"
        );
    }

    /// Gate-off invariant: absent `WorkspaceViewerContext`, no Progress is
    /// emitted (byte-for-byte pre-streaming) while the terminal still surfaces.
    #[tokio::test]
    async fn grep_streaming_suppressed_when_gate_off() {
        use futures::StreamExt;

        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("file.txt"), "findme here\n").unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        // No streaming gate stamped — exercises the default (gate-off) path.
        let mut ctx = echo_agent_tool_runtime::ToolCallContext::default();
        ctx.extensions.insert(resources.into_shared());

        let tool = GrepTool;
        let mut stream =
            echo_agent_tool_runtime::Tool::execute(&tool, ctx, make_grep_input("findme")).await;

        let mut progress = 0usize;
        let mut terminal: Option<Result<GrepSearchOutput, echo_agent_tool_runtime::ToolError>> =
            None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(_) => progress += 1,
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    assert!(terminal.is_none(), "more than one Terminal yielded");
                    terminal = Some(r);
                }
            }
        }

        assert_eq!(
            progress, 0,
            "absent gate must suppress all Progress, got {progress}"
        );
        let output = terminal
            .expect("stream ended without a Terminal")
            .expect("grep terminal ok");
        assert!(
            output.match_count > 0,
            "terminal preserved regardless of gate"
        );
        let card = String::from_utf8_lossy(&output.stdout);
        assert!(card.contains("findme"), "card: {card}");
    }

    /// Streaming invariant under a hit `head_limit`: even when the budget trips
    /// mid-stream (so the early-stop / exact-fit probe path runs), the
    /// accumulated deltas must still equal the terminal card body. Regression
    /// against feeding the streamer bytes clobbered by the probe's read.
    #[tokio::test]
    async fn grep_streaming_body_matches_card_body_when_truncated() {
        use futures::StreamExt;

        let tmp = TempDir::new().unwrap();
        // Many matches in one file so the small head_limit is exceeded and the
        // hit_cap / early-stop path is exercised.
        let content: String = (0..200).map(|i| format!("findme_{i}\n")).collect();
        fs::write(tmp.path().join("many.txt"), &content).unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));

        let mut input = make_grep_input("findme_");
        input.head_limit = Some(5);

        let tool = GrepTool;
        let mut stream =
            echo_agent_tool_runtime::Tool::execute(&tool, test_ctx(resources.into_shared()), input)
                .await;

        let mut deltas = String::new();
        let mut terminal: Option<Result<GrepSearchOutput, echo_agent_tool_runtime::ToolError>> =
            None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(p) => {
                    assert!(terminal.is_none(), "Progress arrived after Terminal");
                    deltas.push_str(&read_grep_delta(&p));
                }
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    assert!(terminal.is_none(), "more than one Terminal yielded");
                    terminal = Some(r);
                }
            }
        }

        let output = terminal
            .expect("stream ended without a Terminal")
            .expect("grep terminal ok");
        let card = String::from_utf8_lossy(&output.stdout);
        let card_lines: Vec<&str> = card.lines().collect();
        let body_from_card = card_lines[2..card_lines.len() - 1].join("\n");
        assert_eq!(
            deltas, body_from_card,
            "accumulated deltas must equal the terminal card body even when truncated"
        );
    }

    /// A cancelled tool future drops the `Child` before any wait/kill path
    /// runs; the spawn config must kill rg on drop.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_spawned_grep_child_kills_rg() {
        let tmp = TempDir::new().unwrap();
        // Overflow the stdout pipe so rg blocks on write and stays alive until killed.
        let line = format!("needle {}\n", "x".repeat(120));
        fs::write(tmp.path().join("big.txt"), line.repeat(20_000)).unwrap();

        let mut resources = Resources::new();
        resources.insert(Cwd(tmp.path().to_path_buf()));
        let ctx = test_ctx(resources.into_shared());

        let step = prepare_grep(&ctx, &make_grep_input("needle"))
            .await
            .expect("prepare_grep");
        let ready = match step {
            GrepStep::Ready(r) => r,
            GrepStep::Early(out) => panic!("expected spawned rg, got early output: {out:?}"),
        };
        let pid = ready.child.id().expect("child pid");

        // Hold the read end open (no EPIPE death) and drop the child mid-run.
        let GrepReady {
            child, stdout_pipe, ..
        } = ready;
        drop(child);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !echo_agent_tty_utils::process_not_running(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "rg (pid {pid}) still running 5s after its Child was dropped — leaked"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(stdout_pipe);
    }
}
