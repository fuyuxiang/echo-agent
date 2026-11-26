//! MemorySearchToolCallBlock — structured memory search results display.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::TOOL_HEADER_RANGE;
use crate::appearance::AppearanceConfig;
use crate::render::line_utils::truncate_str;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode, Selectable,
};
use crate::theme::Theme;

/// A single memory search result parsed from the tool output.
#[derive(Debug, Clone)]
pub struct MemoryResult {
    pub score: f64,
    pub source: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
}

/// Memory search tool call block with structured result display.
#[derive(Debug, Clone)]
pub struct MemorySearchToolCallBlock {
    pub query: String,
    pub results: Vec<MemoryResult>,
    pub error: Option<String>,
    pub started_at: Option<std::time::Instant>,
    pub elapsed_ms: Option<i64>,
}

impl MemorySearchToolCallBlock {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            results: Vec::new(),
            error: None,
            started_at: None,
            elapsed_ms: None,
        }
    }

    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        if self.elapsed_ms.is_none()
            && let Some(start) = self.started_at
        {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
        self.error = error;
    }

    pub fn finish(&mut self) {
        if self.elapsed_ms.is_some() {
            return;
        }
        if let Some(start) = self.started_at {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
    }

    pub fn elapsed_ms(&self) -> Option<i64> {
        self.elapsed_ms.or_else(|| {
            self.started_at
                .map(|start| start.elapsed().as_millis() as i64)
        })
    }

    fn header_line(&self, theme: &Theme, muted: bool, max_width: Option<usize>) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let query_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.command)
        };

        let prefix = "Memory Search ";
        let count = self.results.len();
        let suffix = if count > 0 {
            let s = if count == 1 { "" } else { "s" };
            format!(" ({count} result{s})")
        } else {
            String::new()
        };

        match max_width {
            Some(w) => {
                let suffix_fits = prefix.len() + suffix.len() < w;
                let effective_suffix = if suffix_fits { &suffix } else { "" };
                let query_budget = w
                    .saturating_sub(prefix.len())
                    .saturating_sub(effective_suffix.len());
                let display_query = truncate_str(&self.query, query_budget);

                let mut spans = vec![
                    Span::styled(prefix, bold_style),
                    Span::styled(display_query, query_style),
                ];
                if !effective_suffix.is_empty() {
                    spans.push(Span::styled(effective_suffix.to_string(), theme.dim()));
                }
                Line::from(spans)
            }
            None => Line::from(vec![
                Span::styled(prefix, bold_style),
                Span::styled(self.query.clone(), query_style),
                Span::styled(suffix, theme.dim()),
            ]),
        }
    }

    /// Header line with only the query span selectable (exclude label/suffix).
    fn header_block_line(&self, line: Line<'static>) -> BlockLine {
        let query_end = 2.min(line.spans.len()).max(1);
        BlockLine {
            selectable: Selectable::Spans(1..query_end),
            selection_range: Some(TOOL_HEADER_RANGE),
            selection_text: Some(self.query.clone()),
            content: line,
            ..Default::default()
        }
    }
}

impl BlockContent for MemorySearchToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let muted_collapsed =
            ctx.mute_when_collapsed(ctx.appearance.scrollback.blocks.tool.muted_collapsed);

        match ctx.mode {
            DisplayMode::Collapsed => BlockOutput {
                lines: vec![self.header_block_line(self.header_line(
                    &theme,
                    muted_collapsed,
                    Some(ctx.content_width()),
                ))],
            },
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let header = self.header_line(&theme, false, None);
                let wrapped = crate::render::wrapping::wrap_header_flush(
                    header,
                    ctx.width as usize,
                    ctx.bullet_indent(),
                );
                let mut lines: Vec<BlockLine> = wrapped
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        // First span is label (or indent on continuations); only
                        // the query span is selectable on the first visual row.
                        let selectable = if i == 0 {
                            let query_end = 2.min(line.spans.len()).max(1);
                            Selectable::Spans(1..query_end)
                        } else {
                            Selectable::Spans(1..line.spans.len())
                        };
                        BlockLine {
                            selectable,
                            selection_range: Some(TOOL_HEADER_RANGE),
                            selection_text: if i == 0 {
                                Some(self.query.clone())
                            } else {
                                None
                            },
                            joiner: if i == 0 { None } else { Some(" ".to_string()) },
                            content: line,
                            ..Default::default()
                        }
                    })
                    .collect();

                if self.results.is_empty() && self.error.is_none() {
                    lines.push(BlockLine::separator(Line::from("")));
                    lines.push(BlockLine::separator(Line::from(Span::styled(
                        "  (no results)",
                        theme.muted(),
                    ))));
                }

                for (i, r) in self.results.iter().enumerate() {
                    lines.push(Line::from("").into());

                    // "  1. path/file.md:10-25  (score: 0.72, global)"
                    let idx_span = Span::styled(format!("  {}. ", i + 1), theme.muted());
                    let path_display = shorten_path(&r.path);
                    let path_span = Span::styled(
                        format!("{path_display}:{}-{}", r.start_line, r.end_line),
                        theme.primary().add_modifier(Modifier::BOLD),
                    );
                    let meta_span = Span::styled(
                        format!("  (score: {:.2}, {})", r.score, r.source),
                        theme.dim(),
                    );
                    lines.push(BlockLine::styled(Line::from(vec![
                        idx_span, path_span, meta_span,
                    ])));

                    // Snippet preview (first 3 non-empty lines, with bg_dark)
                    let snippet_lines: Vec<&str> = r
                        .snippet
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .take(3)
                        .collect();
                    for sl in &snippet_lines {
                        let trimmed = sl.trim();
                        let display = truncate_str(trimmed, ctx.content_width().saturating_sub(4));
                        lines.push(
                            BlockLine::from(Line::from(Span::styled(
                                format!("    {display}"),
                                theme.muted(),
                            )))
                            .with_panel_background(theme.bg_dark),
                        );
                    }
                }

                if let Some(ref err) = self.error {
                    lines.push(Line::from("").into());
                    lines.push(
                        Line::from(Span::styled(
                            format!("  {err}"),
                            theme.fg(theme.accent_error),
                        ))
                        .into(),
                    );
                }

                BlockOutput { lines }
            }
        }
    }

    fn accent(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        if ctx.mode == DisplayMode::Collapsed {
            return None;
        }
        let theme = Theme::current();
        if self.error.is_some() {
            Some(AccentStyle::static_color(theme.accent_error))
        } else if ctx.is_running {
            Some(AccentStyle::animated(theme.accent_running))
        } else {
            Some(AccentStyle::static_color(theme.accent_tool))
        }
    }

    fn bullet(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        if self.error.is_some() {
            let theme = Theme::current();
            Some(AccentStyle::static_color(theme.accent_error))
        } else if ctx.mode == DisplayMode::Collapsed {
            None
        } else {
            self.accent(ctx)
        }
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false
    }

    fn background(&self, _ctx: &BlockContext) -> BlockBackground {
        BlockBackground::None
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        self.error.is_none() && !self.results.is_empty()
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    fn next_fold_mode(&self, current: DisplayMode, _is_running: bool) -> DisplayMode {
        match current {
            DisplayMode::Collapsed => DisplayMode::Expanded,
            _ => DisplayMode::Collapsed,
        }
    }

    fn collapse_mode(&self, _is_running: bool) -> DisplayMode {
        DisplayMode::Collapsed
    }
}

fn shorten_path(path: &str) -> &str {
    let memory_root = echo_agent_config::echo_agent_home().join("memory");
    let memory_prefix = memory_root.display().to_string();
    if let Some(rest) = path.strip_prefix(&memory_prefix) {
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        if let Some(after_slash) = rest.find('/') {
            return &rest[after_slash + 1..];
        }
        return rest;
    }
    // Fallback: strip to filename
    path.rsplit('/').next().unwrap_or(path)
}

pub fn parse_memory_results(output: &str) -> Vec<MemoryResult> {
    let mut results = Vec::new();

    // Split on "### Result " markers
    for section in output.split("### Result ") {
        // Skip the preamble ("Found N memory result(s):\n")
        if !section.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }

        let mut score = 0.0;
        let mut source = String::new();
        let mut path = String::new();
        let mut start_line = 0;
        let mut end_line = 0;
        let mut snippet = String::new();

        let lines: Vec<&str> = section.lines().collect();

        // Line 0: "1 (score: 0.72, source: global)"
        if let Some(first) = lines.first() {
            if let Some(score_start) = first.find("score: ") {
                let after = &first[score_start + 7..];
                if let Some(end) = after.find(',') {
                    score = after[..end].parse().unwrap_or(0.0);
                }
            }
            if let Some(src_start) = first.find("source: ") {
                let after = &first[src_start + 8..];
                let end = after.find(')').unwrap_or(after.len());
                source = after[..end].to_string();
            }
        }

        // Line 1: "**File:** /path (lines 10-25)"
        for line in &lines[1..] {
            if let Some(rest) = line.strip_prefix("**File:** ") {
                if let Some(paren) = rest.find(" (lines ") {
                    path = rest[..paren].to_string();
                    let range_str = &rest[paren + 8..];
                    let range_str = range_str.trim_end_matches(')');
                    if let Some((s, e)) = range_str.split_once('-') {
                        start_line = s.parse().unwrap_or(0);
                        end_line = e.parse().unwrap_or(0);
                    }
                } else {
                    path = rest.to_string();
                }
            }
        }

        // Extract snippet between ``` markers
        let full = section;
        if let Some(code_start) = full.find("```\n") {
            let after_start = &full[code_start + 4..];
            if let Some(code_end) = after_start.find("\n```") {
                snippet = after_start[..code_end].to_string();
            }
        }

        if !path.is_empty() || !snippet.is_empty() {
            results.push(MemoryResult {
                score,
                source,
                path,
                start_line,
                end_line,
                snippet,
            });
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_result() {
        let output = r#"Found 1 memory result(s):

### Result 1 (score: 0.72, source: global)
**File:** /root/.echo-agent/memory/MEMORY.md (lines 0-10)
```
## Project Conventions
* Always use graphite for PRs
```
"#;
        let results = parse_memory_results(output);
        assert_eq!(results.len(), 1);
        assert!((results[0].score - 0.72).abs() < 0.01);
        assert_eq!(results[0].source, "global");
        assert_eq!(results[0].path, "/root/.echo-agent/memory/MEMORY.md");
        assert_eq!(results[0].start_line, 0);
        assert_eq!(results[0].end_line, 10);
        assert!(results[0].snippet.contains("graphite"));
    }

    #[test]
    fn parse_multiple_results() {
        let output = r#"Found 2 memory result(s):

### Result 1 (score: 0.85, source: workspace)
**File:** /root/.echo-agent/memory/ws/MEMORY.md (lines 1-5)
```
workspace content
```

### Result 2 (score: 0.42, source: session)
**File:** /root/.echo-agent/memory/ws/sessions/2026-05-01.md (lines 10-20)
```
session content
```
"#;
        let results = parse_memory_results(output);
        assert_eq!(results.len(), 2);
        assert!((results[0].score - 0.85).abs() < 0.01);
        assert_eq!(results[0].source, "workspace");
        assert!((results[1].score - 0.42).abs() < 0.01);
        assert_eq!(results[1].source, "session");
    }

    #[test]
    fn parse_no_results() {
        let output = "No memory results found for query.";
        let results = parse_memory_results(output);
        assert!(results.is_empty());
    }

    #[test]
    fn shorten_memory_path() {
        // Paths under the configured echoagent memory root keep one trailing segment group.
        let memory_root = echo_agent_config::echo_agent_home().join("memory");
        let session = memory_root.join("echo-agent-50aa78f0/sessions/2026-05-01.md");
        let top = memory_root.join("MEMORY.md");
        assert_eq!(
            shorten_path(session.to_str().expect("utf8 path")),
            "sessions/2026-05-01.md"
        );
        assert_eq!(shorten_path(top.to_str().expect("utf8 path")), "MEMORY.md");
        // Outside the memory root falls back to the filename.
        assert_eq!(shorten_path("/some/other/path.md"), "path.md");
    }
}
