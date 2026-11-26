//! BgTaskBlock — scrollback entries for background task lifecycle.
//!
//! Three kinds: Started, Completed, Failed. All render as always-collapsed,
//! groupable blocks with dimmed colored bullets (same dimming as execute blocks).
//! Enter / Ctrl-F opens block viewer with stdout from central store.

use std::time::Duration;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};

use crate::appearance::AppearanceConfig;
use crate::render::color::blend_color;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{AccentStyle, BlockContext, BlockOutput, DisplayMode};
use crate::theme::Theme;
use crate::util::format_duration;

/// What kind of bg task lifecycle event this block represents.
#[derive(Debug, Clone)]
pub enum BgTaskKind {
    /// Task was started (process is running).
    Started,
    /// Task completed successfully.
    Completed { elapsed: Duration },
    /// Task failed (non-zero exit, signal, timeout, etc.).
    Failed {
        elapsed: Duration,
        exit_code: Option<i32>,
        signal: Option<String>,
    },
}

/// Background task scrollback block.
///
/// Always collapsed, not foldable, groupable, selectable.
/// Enter / Ctrl-F opens block viewer with stdout from the central store.
#[derive(Debug, Clone)]
pub struct BgTaskBlock {
    /// The command that was run.
    pub command: String,
    /// Background task ID (for looking up stdout in central store).
    pub task_id: String,
    /// Lifecycle kind (started / completed / failed).
    pub kind: BgTaskKind,
    /// Optional description (from the tool call's description field).
    pub description: Option<String>,
}

impl BgTaskBlock {
    /// Create a "Task started" block.
    pub fn started(command: impl Into<String>, task_id: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            task_id: task_id.into(),
            kind: BgTaskKind::Started,
            description: None,
        }
    }

    /// Create a "Task completed" block.
    pub fn completed(
        command: impl Into<String>,
        task_id: impl Into<String>,
        elapsed: Duration,
    ) -> Self {
        Self {
            command: command.into(),
            task_id: task_id.into(),
            kind: BgTaskKind::Completed { elapsed },
            description: None,
        }
    }

    /// Create a "Task failed" block.
    pub fn failed(
        command: impl Into<String>,
        task_id: impl Into<String>,
        elapsed: Duration,
        exit_code: Option<i32>,
        signal: Option<String>,
    ) -> Self {
        Self {
            command: command.into(),
            task_id: task_id.into(),
            kind: BgTaskKind::Failed {
                elapsed,
                exit_code,
                signal,
            },
            description: None,
        }
    }

    /// Set the description (builder pattern).
    pub fn with_description(mut self, description: Option<String>) -> Self {
        self.description = description;
        self
    }

    /// Whether this block represents a running task (Started kind).
    pub fn is_running(&self) -> bool {
        matches!(self.kind, BgTaskKind::Started)
    }

    /// Mark a Started block as completed (called when task finishes).
    pub fn mark_completed(&mut self, elapsed: Duration) {
        self.kind = BgTaskKind::Completed { elapsed };
    }

    /// Mark a Started block as failed (called when task fails).
    pub fn mark_failed(
        &mut self,
        elapsed: Duration,
        exit_code: Option<i32>,
        signal: Option<String>,
    ) {
        self.kind = BgTaskKind::Failed {
            elapsed,
            exit_code,
            signal,
        };
    }
}

impl BlockContent for BgTaskBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        // When selected, lift only the bold "Task" label to `text_primary`
        // so it reads as undimmed (mirrors `read.rs` / `search.rs`, which
        // bump only the label and leave the rest at `muted`). The detail
        // text (verb + description) stays muted in every state.
        let bold = if ctx.is_selected {
            theme.primary().add_modifier(Modifier::BOLD)
        } else {
            theme.muted().add_modifier(Modifier::BOLD)
        };
        let muted = theme.muted();

        // Collapse newlines for single-line display (ratatui drops '\n' as zero-width,
        // merging adjacent lines without spacing).
        let command = self.command.replace('\n', " ");

        // Prefer description over raw command for the collapsed one-line display.
        // The full command is always available in the block viewer (preamble).
        let display = match &self.description {
            Some(d) if !d.trim().is_empty() => d.replace('\n', " "),
            _ => command,
        };
        let line = match &self.kind {
            BgTaskKind::Started => Line::from(vec![
                Span::styled("Task ", bold),
                Span::styled("started: ", muted),
                Span::styled(display, muted),
            ]),
            BgTaskKind::Completed { elapsed } => Line::from(vec![
                Span::styled("Task ", bold),
                Span::styled(
                    format!("completed in {}: ", format_duration(*elapsed)),
                    muted,
                ),
                Span::styled(display, muted),
            ]),
            BgTaskKind::Failed {
                elapsed,
                exit_code,
                signal,
            } => {
                // Detect kill signals to show "killed" instead of "failed"
                let is_killed = signal
                    .as_deref()
                    .is_some_and(|s| matches!(s, "killed" | "SIGTERM" | "SIGKILL" | "oom"));
                let verb = if is_killed { "killed" } else { "failed" };
                let detail = if is_killed {
                    String::new()
                } else {
                    match (exit_code, signal) {
                        (_, Some(sig)) => format!(" ({})", sig),
                        (Some(code), None) => format!(" (exit {})", code),
                        (None, None) => String::new(),
                    }
                };
                Line::from(vec![
                    Span::styled("Task ", bold),
                    Span::styled(format!("{verb} in {}: ", format_duration(*elapsed)), muted),
                    Span::styled(format!("{}{}", display, detail), muted),
                ])
            }
        };

        BlockOutput {
            lines: vec![line.into()],
        }
    }

    fn accent(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        let theme = Theme::current();
        match &self.kind {
            BgTaskKind::Started if ctx.is_running => {
                Some(AccentStyle::static_color(theme.accent_running))
            }
            _ => None,
        }
    }

    fn bullet(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        let theme = Theme::current();
        match &self.kind {
            BgTaskKind::Started => {
                if ctx.is_running {
                    // Animated pulse between bg and dimmed magenta.
                    // Pre-dim using the same ratio as collapsed execute bullets
                    // so the peak brightness matches other collapsed blocks.
                    let dim = ctx.appearance.scrollback.display.dim_accent;
                    let dimmed = blend_color(theme.bg_base, theme.accent_running, dim)
                        .unwrap_or(theme.accent_running);
                    Some(AccentStyle::animated(dimmed))
                } else {
                    // Normal gray after finish_running() is called
                    None
                }
            }
            BgTaskKind::Completed { .. } => Some(AccentStyle::static_color(theme.accent_success)),
            BgTaskKind::Failed { .. } => Some(AccentStyle::static_color(theme.accent_error)),
        }
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        false
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    fn is_selectable(&self) -> bool {
        true
    }

    fn has_bullet(&self, _ctx: &BlockContext) -> bool {
        true
    }

    fn is_groupable(&self) -> bool {
        true
    }

    fn preamble(&self, ctx: &BlockContext) -> Option<Text<'static>> {
        let theme = Theme::current();
        let mut lines = Vec::new();

        // Description first (primary text), then a blank separator, then
        // the `$ command` with bash syntax highlighting. When there is no
        // description, the command stands alone with no leading blank row.
        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if let Some(desc) = description {
            // Trim trailing whitespace per line and collapse runs of blank
            // lines to a single blank — multi-line descriptions can carry
            // noisy internal blank rows that would otherwise stretch the
            // preamble.
            let mut prev_blank = false;
            for line in desc.lines() {
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    if prev_blank {
                        continue;
                    }
                    prev_blank = true;
                } else {
                    prev_blank = false;
                }
                lines.push(Line::from(Span::styled(
                    trimmed.to_string(),
                    theme.primary(),
                )));
            }
            lines.push(Line::from(""));
        }

        // Multi-line `$ command` must be separate ratatui Lines. A single Line
        // drops '\n' as zero-width, which smashes `cmd1\ncmd2` into `cmd1cmd2`
        // (visible when expanding a started bg task in the block viewer).
        // Match execute / permission-panel soft-wrap so physical newlines and
        // long lines render the same way as foreground shell tool calls.
        push_shell_command_preamble_lines(&mut lines, &self.command, ctx.width as usize, &theme);

        Some(Text::from(lines))
    }
}

/// Append a soft-wrapped `$ command` block to `lines` (first row prefixed with
/// `$ `, continuations hang-indented under the command body).
fn push_shell_command_preamble_lines(
    lines: &mut Vec<Line<'static>>,
    command: &str,
    width: usize,
    theme: &Theme,
) {
    use unicode_width::UnicodeWidthStr;

    let prefix = "$ ";
    let hang = UnicodeWidthStr::width(prefix);
    let cmd_width = width.saturating_sub(hang).max(1);

    let command = if command.trim().is_empty() {
        "\u{2026}"
    } else {
        command
    };

    let cmd_rows =
        crate::views::permission_view::render_bash_command_display_lines(command, cmd_width);

    let hang_indent: String = " ".repeat(hang);
    if cmd_rows.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            prefix.to_string(),
            theme.dim(),
        )]));
        return;
    }
    for (i, row) in cmd_rows.into_iter().enumerate() {
        let mut spans = if i == 0 {
            vec![Span::styled(prefix.to_string(), theme.dim())]
        } else {
            vec![Span::raw(hang_indent.clone())]
        };
        spans.extend(row.spans);
        lines.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::AppearanceConfig;

    fn test_ctx() -> BlockContext {
        BlockContext {
            mode: DisplayMode::Collapsed,
            is_running: false,
            width: 120,
            raw: false,
            max_lines: None,
            appearance: AppearanceConfig::default(),
            is_selected: false,
            cwd: None,
        }
    }

    fn line_text(block: &BgTaskBlock) -> String {
        block.output(&test_ctx()).lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn multiline_command_collapses_newlines() {
        let block = BgTaskBlock::started("echo foo\necho bar", "t1");
        let text = line_text(&block);
        assert!(
            text.contains("foo echo bar"),
            "newlines should become spaces, got: {text:?}"
        );
        assert!(
            !text.contains('\n'),
            "output line must not contain literal newlines"
        );
    }

    #[test]
    fn started_and_completed_prefer_description_over_command() {
        let started = BgTaskBlock::started("sleep 20", "t1")
            .with_description(Some("Wait twenty seconds".into()));
        let started_text = line_text(&started);
        assert!(
            started_text.contains("Wait twenty seconds"),
            "started={started_text:?}"
        );
        assert!(
            !started_text.contains("sleep 20"),
            "started should not show raw command when description present: {started_text:?}"
        );

        let completed = BgTaskBlock::completed("sleep 20", "t1", Duration::from_secs(20))
            .with_description(Some("Wait twenty seconds".into()));
        let completed_text = line_text(&completed);
        assert!(
            completed_text.contains("completed"),
            "completed={completed_text:?}"
        );
        assert!(
            completed_text.contains("Wait twenty seconds"),
            "completed={completed_text:?}"
        );
        assert!(
            !completed_text.contains("sleep 20"),
            "completed should not show raw command when description present: {completed_text:?}"
        );

        let failed = BgTaskBlock::failed("sleep 20", "t1", Duration::from_secs(1), Some(1), None)
            .with_description(Some("Wait twenty seconds".into()));
        let failed_text = line_text(&failed);
        assert!(
            failed_text.contains("Wait twenty seconds"),
            "failed={failed_text:?}"
        );
        assert!(!failed_text.contains("sleep 20"), "failed={failed_text:?}");
    }

    #[test]
    fn completed_multiline_command_collapses_newlines() {
        let block = BgTaskBlock::completed("cmd1\ncmd2", "t1", std::time::Duration::from_secs(1));
        let output = block.output(&test_ctx());
        let text: String = output.lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.contains("cmd1 cmd2"),
            "newlines should become spaces in completed variant, got: {text:?}"
        );
    }

    fn preamble_plain(block: &BgTaskBlock) -> Vec<String> {
        let text = block.preamble(&test_ctx()).expect("preamble");
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn preamble_description_first_then_blank_then_command() {
        let block = BgTaskBlock::started("cargo test --release", "t1")
            .with_description(Some("Run release tests".into()));
        let plain = preamble_plain(&block);
        assert_eq!(plain.len(), 3, "expected description + blank + command");
        assert_eq!(plain[0], "Run release tests");
        assert_eq!(plain[1], "");
        assert_eq!(plain[2], "$ cargo test --release");
    }

    #[test]
    fn preamble_uses_primary_text_color_for_description() {
        // Pin theme to avoid races with parallel tests that call `cache::set`.
        crate::theme::cache::set(crate::theme::ThemeKind::EchoAgentNight);
        let block =
            BgTaskBlock::started("ls", "t1").with_description(Some("List the files".into()));
        let text = block.preamble(&test_ctx()).expect("preamble");
        let theme = Theme::current();
        let span = &text.lines[0].spans[0];
        assert_eq!(span.content.as_ref(), "List the files");
        assert_eq!(span.style.fg, Some(theme.text_primary));
    }

    #[test]
    fn preamble_without_description_renders_only_command() {
        let block = BgTaskBlock::started("ls -la", "t1");
        let plain = preamble_plain(&block);
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0], "$ ls -la");
    }

    #[test]
    fn preamble_blank_description_falls_back_to_command_only() {
        let block = BgTaskBlock::started("ls", "t1").with_description(Some("   \n  ".into()));
        let plain = preamble_plain(&block);
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0], "$ ls");
    }

    #[test]
    fn preamble_multiline_description_keeps_separate_lines() {
        let block = BgTaskBlock::started("ls", "t1")
            .with_description(Some("First line\nSecond line".into()));
        let plain = preamble_plain(&block);
        assert_eq!(plain.len(), 4);
        assert_eq!(plain[0], "First line");
        assert_eq!(plain[1], "Second line");
        assert_eq!(plain[2], "");
        assert_eq!(plain[3], "$ ls");
    }

    #[test]
    fn preamble_collapses_blank_run_and_trims_trailing_whitespace() {
        // Runs of blank lines collapse to one; trailing whitespace on a
        // line is stripped so the rendered row doesn't carry stray spaces.
        let block = BgTaskBlock::started("ls", "t1")
            .with_description(Some("First   \n\n\n\nSecond  ".into()));
        let plain = preamble_plain(&block);
        // First, single blank (collapsed from 3 internal blanks), Second,
        // separator blank, command.
        assert_eq!(plain, vec!["First", "", "Second", "", "$ ls"]);
    }

    #[test]
    fn preamble_multiline_command_keeps_separate_lines() {
        // Regression: a single ratatui Line drops '\n' as zero-width, which
        // smashed multi-line bg-task commands into one unreadable blob when
        // expanded in the block viewer.
        let block = BgTaskBlock::started(
            "export ECHO_AGENT_ROOT=/tmp\ncd /tmp\necho start\nprod-run start backend",
            "t1",
        )
        .with_description(Some("Start backend".into()));
        let plain = preamble_plain(&block);
        assert_eq!(
            plain,
            vec![
                "Start backend",
                "",
                "$ export ECHO_AGENT_ROOT=/tmp",
                "  cd /tmp",
                "  echo start",
                "  prod-run start backend",
            ]
        );
        // Sanity: must not be the smashed single-line form.
        let joined = plain.join("");
        assert!(
            !joined.contains("/tmpcd") && !joined.contains("/tmpecho"),
            "newlines must not be dropped: {plain:?}"
        );
    }

    #[test]
    fn preamble_multiline_command_without_description() {
        let block = BgTaskBlock::started("echo a\necho b", "t1");
        let plain = preamble_plain(&block);
        assert_eq!(plain, vec!["$ echo a", "  echo b"]);
    }

    #[test]
    fn failed_multiline_command_collapses_newlines() {
        let block = BgTaskBlock::failed(
            "a\nb",
            "t1",
            std::time::Duration::from_secs(2),
            Some(1),
            None,
        );
        let output = block.output(&test_ctx());
        let text: String = output.lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.contains("a b"),
            "newlines should become spaces in failed variant, got: {text:?}"
        );
    }
}
