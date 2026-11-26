//! Hook data types and rendering helpers for tool call blocks.
//!
//! Hook runs are displayed as part of tool call blocks rather than
//! as standalone scrollback entries. The tool header comes first,
//! then pre_tool_use hooks, then post_tool_use hooks.

use std::time::Duration;

use ratatui::text::{Line, Span};

use crate::scrollback::types::{BlockLine, DisplayMode};
use crate::theme::Theme;

// ── Data types ────────────────────────────────────────────────────────

/// Status of a single hook execution within a batch.
#[derive(Debug, Clone)]
pub enum HookRunStatus {
    Success {
        elapsed: Duration,
    },
    Skipped,
    /// The hook ran and blocked (a stop-gate decision, not a failure).
    Blocked {
        detail: String,
        elapsed: Duration,
    },
    Failed {
        error: String,
        elapsed: Duration,
    },
}

/// A single hook run entry for display.
#[derive(Debug, Clone)]
pub struct HookRunEntry {
    pub name: String,
    pub status: HookRunStatus,
    /// Truncated stdout/stderr from the hook command, if any.
    pub output: Option<String>,
}

/// Which phase of tool execution the hooks belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Pre,
    Post,
}

/// Hook data attached to a tool call block.
#[derive(Debug, Clone, Default)]
pub struct ToolCallHookData {
    pub pre_hooks: Vec<HookRunEntry>,
    pub post_hooks: Vec<HookRunEntry>,
    /// Lifecycle hooks (session_start, session_end, stop) — rendered with their own event name.
    pub lifecycle: Vec<(String, Vec<HookRunEntry>)>,
}

impl ToolCallHookData {
    pub fn is_empty(&self) -> bool {
        self.pre_hooks.is_empty() && self.post_hooks.is_empty() && self.lifecycle.is_empty()
    }

    pub fn has_content(&self) -> bool {
        HookRunCounts::from_data(self).total() > 0
    }
}

// ── Rendering helpers ─────────────────────────────────────────────────

const INDENT: &str = "    ";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HookRunCounts {
    pub(crate) success: usize,
    pub(crate) blocked: usize,
    pub(crate) failed: usize,
}

impl HookRunCounts {
    pub(crate) fn from_data(data: &ToolCallHookData) -> Self {
        let mut counts = Self::default();
        counts.add_data(data);
        counts
    }

    fn from_runs(runs: &[HookRunEntry]) -> Self {
        let mut counts = Self::default();
        counts.add_runs(runs);
        counts
    }

    pub(crate) fn add_data(&mut self, data: &ToolCallHookData) {
        self.add_runs(&data.pre_hooks);
        self.add_runs(&data.post_hooks);
        for (_, runs) in &data.lifecycle {
            self.add_runs(runs);
        }
    }

    pub(crate) fn has_failures(self) -> bool {
        self.failed > 0
    }

    fn compact_completed(self) -> usize {
        self.success + self.blocked
    }

    fn add_runs(&mut self, runs: &[HookRunEntry]) {
        for run in runs {
            match run.status {
                HookRunStatus::Success { .. } => self.success += 1,
                HookRunStatus::Blocked { .. } => self.blocked += 1,
                HookRunStatus::Failed { .. } => self.failed += 1,
                HookRunStatus::Skipped => {}
            }
        }
    }

    fn total(self) -> usize {
        self.success + self.blocked + self.failed
    }
}

#[derive(Debug, Clone, Copy)]
enum HookCountShape {
    /// Individual rows preserve the established completed/failed contract:
    /// blocked hooks completed normally and remain in the green numerator.
    Compact,
    /// Aggregate rows name every outcome because no member detail is visible.
    Labeled,
}

fn render_hook_counts_inline_suffix(
    counts: HookRunCounts,
    shape: HookCountShape,
    theme: &Theme,
) -> Option<Vec<Span<'static>>> {
    if counts.total() == 0 {
        return None;
    }
    let dimmed = ratatui::style::Modifier::DIM;
    let mut spans = vec![Span::styled("  [hooks: ", theme.muted())];
    match shape {
        HookCountShape::Compact => {
            let completed = counts.compact_completed();
            if completed > 0 {
                spans.push(Span::styled(
                    completed.to_string(),
                    theme.fg(theme.accent_success).add_modifier(dimmed),
                ));
            }
            if completed > 0 && counts.failed > 0 {
                spans.push(Span::styled("/", theme.muted()));
            }
            if counts.failed > 0 {
                spans.push(Span::styled(
                    counts.failed.to_string(),
                    theme.fg(theme.accent_error).add_modifier(dimmed),
                ));
            }
        }
        HookCountShape::Labeled if counts.blocked == 0 && counts.failed == 0 => {
            spans.push(Span::styled(
                counts.success.to_string(),
                theme.fg(theme.accent_success).add_modifier(dimmed),
            ));
        }
        HookCountShape::Labeled => {
            let mut is_first = true;
            for (count, label, color) in [
                (counts.success, "ok", theme.accent_success),
                (counts.blocked, "blocked", theme.accent_running),
                (counts.failed, "failed", theme.accent_error),
            ] {
                if count == 0 {
                    continue;
                }
                if !is_first {
                    spans.push(Span::styled(", ", theme.muted()));
                }
                is_first = false;
                spans.push(Span::styled(
                    format!("{count} {label}"),
                    theme.fg(color).add_modifier(dimmed),
                ));
            }
        }
    }
    spans.push(Span::styled("]", theme.muted()));
    Some(spans)
}

pub(crate) fn render_group_hook_counts_inline_suffix(
    counts: HookRunCounts,
    theme: &Theme,
) -> Option<Vec<Span<'static>>> {
    render_hook_counts_inline_suffix(counts, HookCountShape::Labeled, theme)
}

pub fn render_hooks_inline_suffix(data: &ToolCallHookData) -> Option<Vec<Span<'static>>> {
    render_hook_counts_inline_suffix(
        HookRunCounts::from_data(data),
        HookCountShape::Compact,
        &Theme::current(),
    )
}

/// Right-side summary for stop hooks merged onto a turn-terminal marker line:
/// `stop  [hooks: 2]` per group (bold muted event name + colored counts),
/// groups joined by two spaces. Returns `None` when nothing ran.
pub fn render_stop_hooks_summary(
    groups: &[(String, Vec<HookRunEntry>)],
) -> Option<Vec<Span<'static>>> {
    let theme = Theme::current();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (event_name, runs) in groups {
        let Some(count_spans) = render_hook_counts_inline_suffix(
            HookRunCounts::from_runs(runs),
            HookCountShape::Compact,
            &theme,
        ) else {
            continue;
        };
        if !spans.is_empty() {
            spans.push(Span::styled("  ", theme.muted()));
        }
        spans.push(Span::styled(
            event_name.clone(),
            theme.muted().add_modifier(ratatui::style::Modifier::BOLD),
        ));
        spans.extend(count_spans);
    }
    if spans.is_empty() { None } else { Some(spans) }
}

/// Render a separator line between tool output and hooks.
fn render_separator() -> BlockLine {
    let theme = Theme::current();
    Line::from(vec![Span::styled(
        format!("{}\u{2500}\u{2500}\u{2500}", INDENT),
        theme.muted(),
    )])
    .into()
}

/// Render hook details as expanded lines.
///
/// Format:
///   **pre_tool_use**
///     \u2713 hook-name (12ms)
///     \u2717 hook-name (3ms): error message
///   **post_tool_use**
///     \u2713 hook-name (5ms)
fn render_hooks_expanded(event: &str, runs: &[HookRunEntry]) -> Vec<BlockLine> {
    let theme = Theme::current();
    let mut lines = Vec::new();

    // If all hooks were skipped, render nothing.
    if runs
        .iter()
        .all(|r| matches!(r.status, HookRunStatus::Skipped))
    {
        return lines;
    }

    // Header: indented, bold, muted
    lines.push(
        Line::from(vec![Span::styled(
            format!("{}{}", INDENT, event),
            theme.muted().add_modifier(ratatui::style::Modifier::BOLD),
        )])
        .into(),
    );

    // Per-hook detail lines
    lines.extend(render_hooks_expanded_inner(runs));

    lines
}

/// Render per-hook detail lines without a section header.
fn render_hooks_expanded_inner(runs: &[HookRunEntry]) -> Vec<BlockLine> {
    let theme = Theme::current();
    let mut lines = Vec::new();

    for run in runs {
        match &run.status {
            HookRunStatus::Success { elapsed } => {
                lines.push(
                    Line::from(vec![
                        Span::styled(format!("{}  ", INDENT), theme.muted()),
                        Span::styled(
                            format!("{} ", crate::glyphs::check_mark()),
                            theme.fg(theme.accent_success),
                        ),
                        Span::styled(run.name.clone(), theme.muted()),
                        Span::styled(format!(" ({}ms)", elapsed.as_millis()), theme.muted()),
                    ])
                    .into(),
                );
            }
            HookRunStatus::Skipped => {
                lines.push(
                    Line::from(vec![
                        Span::styled(format!("{}  ", INDENT), theme.muted()),
                        Span::styled("- ", theme.muted()),
                        Span::styled(run.name.clone(), theme.muted()),
                        Span::styled(" skipped", theme.muted()),
                    ])
                    .into(),
                );
            }
            HookRunStatus::Blocked { detail, elapsed } => {
                lines.push(
                    Line::from(vec![
                        Span::styled(format!("{}  ", INDENT), theme.muted()),
                        Span::styled("\u{21a9} ", theme.fg(theme.accent_running)),
                        Span::styled(run.name.clone(), theme.muted()),
                        Span::styled(format!(" ({}ms)", elapsed.as_millis()), theme.muted()),
                    ])
                    .into(),
                );
                let detail_text = crate::render::line_utils::truncate_str(detail, 120);
                for detail_line in detail_text.lines().take(3) {
                    lines.push(
                        Line::from(vec![
                            Span::styled(format!("{}      ", INDENT), theme.muted()),
                            Span::styled(detail_line.to_string(), theme.fg(theme.accent_running)),
                        ])
                        .into(),
                    );
                }
            }
            HookRunStatus::Failed { error, elapsed } => {
                lines.push(
                    Line::from(vec![
                        Span::styled(format!("{}  ", INDENT), theme.muted()),
                        Span::styled(
                            format!("{} ", crate::glyphs::ballot_x()),
                            theme.fg(theme.accent_error),
                        ),
                        Span::styled(run.name.clone(), theme.muted()),
                        Span::styled(format!(" ({}ms)", elapsed.as_millis()), theme.muted()),
                    ])
                    .into(),
                );
                // Error text — strip redundant hook name prefix if present
                let cleaned = error
                    .strip_prefix(&format!("hook '{}' ", run.name))
                    .unwrap_or(error);
                let err_text = crate::render::line_utils::truncate_str(cleaned, 120);
                for err_line in err_text.lines().take(3) {
                    lines.push(
                        Line::from(vec![
                            Span::styled(format!("{}      ", INDENT), theme.muted()),
                            Span::styled(err_line.to_string(), theme.fg(theme.accent_error)),
                        ])
                        .into(),
                    );
                }
            }
        }

        // Truncated output (if present)
        if let Some(ref output) = run.output {
            let truncated = crate::render::line_utils::truncate_str(output, 120);
            for out_line in truncated.lines().take(3) {
                lines.push(
                    Line::from(vec![
                        Span::styled(format!("{}      ", INDENT), theme.muted()),
                        Span::styled(out_line.to_string(), theme.muted()),
                    ])
                    .into(),
                );
            }
        }
    }

    lines
}

/// Render hook lines for a given display mode.
pub fn render_hooks_for_mode(
    event: &str,
    runs: &[HookRunEntry],
    mode: DisplayMode,
) -> Vec<BlockLine> {
    if runs.is_empty() {
        return Vec::new();
    }
    match mode {
        DisplayMode::Collapsed => Vec::new(),
        DisplayMode::Expanded | DisplayMode::Truncated => render_hooks_expanded(event, runs),
    }
}

/// Render hook detail lines (no section header) for expanded/truncated modes.
///
/// Used by lifecycle blocks where the block header already shows the event name,
/// so repeating it as a section header would be redundant.
pub fn render_hooks_detail(runs: &[HookRunEntry], mode: DisplayMode) -> Vec<BlockLine> {
    if runs.is_empty() {
        return Vec::new();
    }
    match mode {
        DisplayMode::Collapsed => Vec::new(),
        DisplayMode::Expanded | DisplayMode::Truncated => render_hooks_expanded_inner(runs),
    }
}

/// Render a separator line (for use between tool output and hooks in expanded mode).
pub fn render_hook_separator() -> BlockLine {
    render_separator()
}

#[cfg(test)]
#[path = "hook_tests.rs"]
mod tests;
