//! ReadToolCallBlock - reads a file with syntax highlighting.

use std::path::Path;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::{LineRange, TOOL_HEADER_RANGE};
use crate::prompt_images::ScrollbackImageRef;
use crate::render::wrapping::word_wrap_lines_with_joiners;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode, Selectable,
};
use crate::syntax::get_syntect;
use crate::theme::Theme;

const FIRST_LINES: usize = 5;
const LAST_LINES: usize = 3;

use crate::appearance::AppearanceConfig;
use echo_agent_tools::implementations::skills::types::skill_name_from_path;

/// What kind of non-text media this read produced.
#[derive(Debug, Clone)]
pub enum ReadMediaKind {
    /// Image file (PNG, JPEG, etc.)
    Image,
    /// PDF rendered as page images.
    Pdf { pages: usize },
}

/// Read file tool call.
#[derive(Debug, Clone)]
pub struct ReadToolCallBlock {
    /// Path to the file being read.
    pub path: String,
    /// Line range if specified: [start, end] (1-based, inclusive).
    pub line_range: Option<LineRange>,
    /// Error message if the tool call failed (None = success).
    pub error: Option<String>,
    /// When the tool started running (Phase 2: time tracking).
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion (Phase 2: time tracking).
    pub elapsed_ms: Option<i64>,
    /// Raw file content (unformatted). `None` for errors, images, PDFs.
    pub content: Option<String>,
    /// Total number of lines in the file (from `FileContent.total_lines`).
    pub total_lines: Option<usize>,
    /// Inline image reference (for image file reads).
    pub image_ref: Option<ScrollbackImageRef>,
    /// Non-text media kind (image, PDF).
    pub media_kind: Option<ReadMediaKind>,
}

impl ReadToolCallBlock {
    /// Create a new read block.
    ///
    /// Pre-completed blocks have no meaningful local timing — `started_at`
    /// is `None`. Timing is only set for blocks that enter a running UI
    /// state (via `set_last_running(true)` in `ScrollbackState`).
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line_range: None,
            error: None,
            started_at: None,
            elapsed_ms: None,
            content: None,
            total_lines: None,
            image_ref: None,
            media_kind: None,
        }
    }

    /// Set line range.
    pub fn with_line_range(mut self, range: LineRange) -> Self {
        self.line_range = Some(range);
        self
    }

    /// Set file content and total line count.
    pub fn with_content(mut self, content: String, total_lines: usize) -> Self {
        self.content = Some(content);
        self.total_lines = Some(total_lines);
        self
    }

    /// Set error (marks as failed).
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Check if successful (no error).
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Whether the block has non-empty text content to display.
    pub fn has_content(&self) -> bool {
        self.content.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Skill name when this read targets a skill definition (`SKILL.md`).
    /// Single source of truth for skill-read detection.
    pub fn skill_name(&self) -> Option<&str> {
        skill_name_from_path(&self.path)
    }

    /// Whether this read targets a skill definition rather than a plain file.
    pub fn is_skill_read(&self) -> bool {
        self.skill_name().is_some()
    }

    /// Set error (mutable) — compute elapsed time if not already set (Phase 2).
    pub fn set_error(&mut self, error: Option<String>) {
        if self.elapsed_ms.is_none()
            && let Some(start) = self.started_at
        {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
        self.error = error;
    }

    /// Finalize elapsed time from `started_at`.
    ///
    /// Idempotent: no-op if `started_at` is `None` (pre-completed block)
    /// or if `elapsed_ms` is already set (already finalized).
    pub fn finish(&mut self) {
        if self.elapsed_ms.is_some() {
            return;
        }
        if let Some(start) = self.started_at {
            self.elapsed_ms = Some(start.elapsed().as_millis() as i64);
        }
    }

    /// Get elapsed time in ms (Phase 2).
    pub fn elapsed_ms(&self) -> Option<i64> {
        match self.elapsed_ms {
            Some(ms) => Some(ms),
            None => self
                .started_at
                .map(|start| start.elapsed().as_millis() as i64),
        }
    }

    /// Render header line: `Read path (start-end)`.
    fn collapsed_line(
        &self,
        theme: &Theme,
        muted: bool,
        dim_details: bool,
        surface: crate::render::tool_paths::ToolPathSurface,
        cwd: Option<&std::path::Path>,
        width: Option<usize>,
    ) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(Modifier::BOLD);
        let path_style = if muted {
            theme.muted()
        } else {
            theme.fg(theme.path)
        };
        let detail_style = if dim_details {
            theme.dim()
        } else {
            theme.muted()
        };

        // SKILL.md reads render as "Skill {skill_name}".
        if let Some(skill) = self.skill_name() {
            return Line::from(vec![
                Span::styled("Skill ", bold_style),
                Span::styled(skill.to_owned(), path_style),
            ]);
        }

        let prefix = "Read ";
        let range_suffix = self
            .line_range
            .map(|r| {
                if let Some(total) = self.total_lines
                    && total > r.end.saturating_sub(r.start) + 1
                {
                    format!(" ({} of {total})", r)
                } else {
                    format!(" ({})", r)
                }
            })
            .unwrap_or_default();
        // Extra suffix for errors or empty content
        let extra_suffix = if self.content.as_ref().is_some_and(|c| c.is_empty()) {
            " (empty)".to_string()
        } else if let Some(media) = &self.media_kind {
            match media {
                ReadMediaKind::Image => " (image)".to_string(),
                ReadMediaKind::Pdf { pages } => format!(" ({pages} pages)"),
            }
        } else {
            String::new()
        };
        let total_suffix_len = range_suffix.len() + extra_suffix.len();
        let path = crate::render::tool_paths::path_for_tool_surface(
            &self.path,
            surface,
            cwd,
            width,
            prefix.len() + total_suffix_len,
        );

        let mut spans = vec![
            Span::styled(prefix, bold_style),
            Span::styled(path, path_style),
        ];

        if !range_suffix.is_empty() {
            spans.push(Span::styled(range_suffix, detail_style));
        }

        if !extra_suffix.is_empty() {
            spans.push(Span::styled(extra_suffix, detail_style));
        }

        Line::from(spans)
    }

    /// Header line with only the path (or skill name) span selectable.
    ///
    /// Spans: `["Read ", path, optional_range_suffix, optional_extra_suffix]`
    /// or `["Skill ", skill_name]`. Prefix/suffixes excluded (no `selection_text`
    /// override). Attaches a semantic filesystem target for non-skill paths.
    fn header_block_line(&self, line: Line<'static>, cwd: Option<&std::path::Path>) -> BlockLine {
        let path_end = 2.min(line.spans.len()).max(1);
        let link_target = if self.skill_name().is_some() {
            None
        } else {
            crate::render::osc8::tool_path_file_target(&self.path, cwd)
        };
        BlockLine {
            selectable: Selectable::Spans(1..path_end),
            selection_range: Some(TOOL_HEADER_RANGE),
            content: line,
            link_target,
            ..Default::default()
        }
    }

    /// Render content lines with absolute line numbers in the gutter.
    ///
    /// Wraps all lines first, then applies truncation -- matching ExecuteToolCallBlock.
    fn render_content_lines(
        &self,
        theme: &Theme,
        width: usize,
        truncate: Option<(usize, usize)>,
    ) -> Vec<BlockLine> {
        let Some(content) = &self.content else {
            return Vec::new();
        };

        let base_line = self.line_range.map_or(1, |r| r.start);
        let raw_lines: Vec<&str> = content.lines().collect();

        let gutter_width = digit_count(base_line + raw_lines.len().saturating_sub(1));
        let content_width = width.saturating_sub(gutter_width + 2).max(20);

        // Use Theme::dim/primary so terminal-native (minimal) maps grays to
        // SGR dim / default fg instead of raw gray_dim slots.
        let gutter_style = theme.dim();
        let text_style = theme.primary();

        let syntect = get_syntect();
        let mut highlighter = syntect.highlight_lines_by_file_path(Path::new(&self.path));

        let styled_lines: Vec<Line<'static>> = raw_lines
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let gutter = format!("{:>w$}  ", base_line + i, w = gutter_width);
                let mut spans = vec![Span::styled(gutter, gutter_style)];
                spans.extend(crate::syntax::highlight_line(
                    text,
                    &mut highlighter,
                    syntect,
                    text_style,
                ));
                Line::from(spans)
            })
            .collect();

        let (wrapped, joiners) = word_wrap_lines_with_joiners(styled_lines, content_width);
        let total = wrapped.len();

        let mut lines = Vec::new();

        if let Some((first, last)) = truncate {
            let threshold = first + last;
            if total > threshold {
                for (wrapped_line, joiner) in wrapped.iter().zip(joiners.iter()).take(first) {
                    lines.push(
                        BlockLine::styled(wrapped_line.clone())
                            .with_panel_background(theme.bg_dark)
                            .with_joiner(joiner.clone()),
                    );
                }
                lines.push(
                    BlockLine::separator(Line::from(Span::styled("\u{2026}", theme.muted())))
                        .with_panel_background(theme.bg_dark),
                );
                for (wrapped_line, joiner) in wrapped.iter().zip(joiners.iter()).skip(total - last)
                {
                    lines.push(
                        BlockLine::styled(wrapped_line.clone())
                            .with_panel_background(theme.bg_dark)
                            .with_joiner(joiner.clone()),
                    );
                }
            } else {
                for (wrapped_line, joiner) in wrapped.into_iter().zip(joiners) {
                    lines.push(
                        BlockLine::styled(wrapped_line)
                            .with_panel_background(theme.bg_dark)
                            .with_joiner(joiner),
                    );
                }
            }
        } else {
            for (wrapped_line, joiner) in wrapped.into_iter().zip(joiners) {
                lines.push(
                    BlockLine::styled(wrapped_line)
                        .with_panel_background(theme.bg_dark)
                        .with_joiner(joiner),
                );
            }
        }

        lines
    }
}

/// Count decimal digits in a number (for gutter width).
fn digit_count(n: usize) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

impl BlockContent for ReadToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let tool_cfg = &ctx.appearance.scrollback.blocks.tool;
        let muted_collapsed = ctx.mute_when_collapsed(tool_cfg.muted_collapsed);

        let cwd = ctx.cwd.as_deref();
        match ctx.mode {
            DisplayMode::Collapsed => BlockOutput {
                lines: vec![self.header_block_line(
                    self.collapsed_line(
                        &theme,
                        muted_collapsed,
                        tool_cfg.dim_details,
                        crate::render::tool_paths::ToolPathSurface::Collapsed,
                        cwd,
                        Some(ctx.content_width()),
                    ),
                    cwd,
                )],
            },
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let truncate = if ctx.mode == DisplayMode::Truncated {
                    Some((FIRST_LINES, LAST_LINES))
                } else {
                    None
                };
                let header = self.collapsed_line(
                    &theme,
                    false,
                    tool_cfg.dim_details,
                    crate::render::tool_paths::ToolPathSurface::Expanded,
                    cwd,
                    None,
                );
                let mut lines: Vec<BlockLine> = vec![self.header_block_line(header, cwd)];
                if self.has_content() {
                    lines.push(BlockLine::separator(Line::from("")));
                    lines.extend(self.render_content_lines(&theme, ctx.width as usize, truncate));
                } else if let Some(err) = &self.error {
                    lines.push(BlockLine::separator(Line::from("")));
                    let error_style = Style::default().fg(theme.accent_error);
                    for line in err.lines() {
                        lines.push(BlockLine::separator(Line::from(Span::styled(
                            line.to_string(),
                            error_style,
                        ))));
                    }
                }
                BlockOutput { lines }
            }
        }
    }

    fn accent(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        None
    }

    fn bullet(&self, _ctx: &BlockContext) -> Option<AccentStyle> {
        if self.error.is_some() {
            let theme = Theme::current();
            Some(AccentStyle::static_color(theme.accent_error))
        } else {
            None
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
        self.has_content()
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    fn finished_display_mode(&self) -> Option<DisplayMode> {
        Some(DisplayMode::Collapsed)
    }

    fn next_fold_mode(&self, current: DisplayMode, _is_running: bool) -> DisplayMode {
        match current {
            DisplayMode::Collapsed => DisplayMode::Truncated,
            DisplayMode::Truncated | DisplayMode::Expanded => DisplayMode::Collapsed,
        }
    }

    fn image_references(&self) -> &[ScrollbackImageRef] {
        match &self.image_ref {
            Some(r) => std::slice::from_ref(r),
            None => &[],
        }
    }

    fn preamble(&self, ctx: &BlockContext) -> Option<Text<'static>> {
        let theme = Theme::current();
        let dim_details = ctx.appearance.scrollback.blocks.tool.dim_details;
        Some(Text::from(self.collapsed_line(
            &theme,
            false,
            dim_details,
            crate::render::tool_paths::ToolPathSurface::Fullscreen,
            ctx.cwd.as_deref(),
            None,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::types::{BlockContext, DisplayMode};

    fn make_ctx() -> BlockContext {
        BlockContext {
            width: 80,
            mode: DisplayMode::Collapsed,
            is_running: false,
            raw: false,
            max_lines: None,
            appearance: Default::default(),
            is_selected: false,
            cwd: None,
        }
    }

    #[test]
    fn skill_md_renders_as_skill_label() {
        let block = ReadToolCallBlock::new("/home/user/.echo-agent/skills/deploy/SKILL.md");
        let output = block.output(&make_ctx());
        let text: String = output.lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, "Skill deploy");
    }

    #[test]
    fn regular_file_renders_as_read() {
        let block = ReadToolCallBlock::new("src/main.rs");
        let output = block.output(&make_ctx());
        let text: String = output.lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.starts_with("Read "),
            "expected 'Read ...' got '{text}'"
        );
        assert!(text.contains("main.rs"));
    }

    #[test]
    fn collapsed_header_shows_basename_only() {
        let block = ReadToolCallBlock::new("/Users/me/project/src/main.rs")
            .with_line_range(LineRange::new(1, 10));
        let output = block.output(&make_ctx());
        let text: String = output.lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, "Read main.rs (1-10)");
    }

    #[test]
    fn expanded_shows_relative_when_under_cwd_preamble_absolute() {
        let abs = "/Users/me/project/src/main.rs";
        let cwd = std::path::PathBuf::from("/Users/me/project");
        let block = ReadToolCallBlock::new(abs).with_content("hello".into(), 1);
        let mut ctx = make_ctx();
        ctx.mode = DisplayMode::Expanded;
        ctx.cwd = Some(cwd.clone());
        let output = block.output(&ctx);
        let header: String = output.lines[0]
            .content
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(header, "Read src/main.rs");

        let preamble = block.preamble(&ctx).unwrap();
        let preamble_text: String = preamble
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(preamble_text, "Read /Users/me/project/src/main.rs");
    }

    #[test]
    fn content_preview_shading_is_marked_panel() {
        // The preview's bg_dark band is decorative chrome, not semantic
        // shading — it must be flagged `background_is_panel` so minimal
        // mode's flat rendering can drop it (EntryRenderer::flat_background).
        let block = ReadToolCallBlock::new("notes.txt").with_content("alpha\nbravo".to_string(), 2);
        let mut ctx = make_ctx();
        ctx.mode = DisplayMode::Expanded;
        let output = block.output(&ctx);
        let shaded: Vec<_> = output
            .lines
            .iter()
            .filter(|l| l.background.is_some())
            .collect();
        assert!(
            !shaded.is_empty(),
            "expanded read preview must shade its content lines"
        );
        assert!(
            shaded.iter().all(|l| l.background_is_panel),
            "read preview shading must be marked panel"
        );
    }

    #[test]
    fn header_only_path_is_selectable() {
        use crate::scrollback::types::{Selectable, derive_selection_text};

        let block = ReadToolCallBlock::new("/Users/me/project/src/main.rs")
            .with_line_range(LineRange::new(1, 10));
        let output = block.output(&make_ctx());
        let header = &output.lines[0];

        assert!(
            matches!(&header.selectable, Selectable::Spans(r) if *r == (1..2)),
            "only path span should be selectable, got {:?}",
            header.selectable
        );
        // Collapsed: copy the painted basename, not a full-path override.
        assert_eq!(
            derive_selection_text(header),
            "main.rs",
            "copy/highlight should match the painted path span, not 'Read …'"
        );
        assert_eq!(header.content.spans[0].content.as_ref(), "Read ");
        assert!(header.selection_text.is_none());
    }

    #[test]
    fn expanded_header_selection_matches_relative_path() {
        use crate::scrollback::types::derive_selection_text;

        let block = ReadToolCallBlock::new("/Users/me/project/src/main.rs");
        let mut ctx = make_ctx();
        ctx.mode = DisplayMode::Expanded;
        ctx.cwd = Some(std::path::PathBuf::from("/Users/me/project"));
        let header = &block.output(&ctx).lines[0];
        assert_eq!(derive_selection_text(header), "src/main.rs");
    }

    #[test]
    fn header_link_target_is_absolute_file_for_collapsed_and_expanded() {
        let abs = "/Users/me/project/src/main.rs";
        let block = ReadToolCallBlock::new(abs);
        let mut ctx = make_ctx();
        ctx.cwd = Some(std::path::PathBuf::from("/Users/me/project"));

        let collapsed = block.output(&ctx);
        let target = collapsed.lines[0]
            .link_target
            .as_ref()
            .expect("link target");
        assert_eq!(
            target,
            &crate::render::osc8::LinkTarget::File(
                std::sync::Arc::from(std::path::Path::new(abs),)
            )
        );
        assert_eq!(
            crate::render::osc8::resolve_link_target(target)
                .unwrap()
                .osc8_url
                .unwrap()
                .as_ref(),
            "file:///Users/me/project/src/main.rs"
        );
        assert_eq!(
            collapsed.lines[0].content.spans[1].content.as_ref(),
            "main.rs"
        );

        ctx.mode = DisplayMode::Expanded;
        let expanded = block.output(&ctx);
        assert_eq!(
            expanded.lines[0].content.spans[1].content.as_ref(),
            "src/main.rs"
        );
        assert_eq!(expanded.lines[0].link_target.as_ref(), Some(target));
    }

    #[test]
    fn skill_header_selects_skill_name_only() {
        use crate::scrollback::types::{Selectable, derive_selection_text};

        let block = ReadToolCallBlock::new("/home/user/.echo-agent/skills/deploy/SKILL.md");
        let output = block.output(&make_ctx());
        let header = &output.lines[0];

        assert!(matches!(&header.selectable, Selectable::Spans(r) if *r == (1..2)));
        assert_eq!(derive_selection_text(header), "deploy");
        assert_eq!(header.content.spans[0].content.as_ref(), "Skill ");
    }

    #[test]
    fn is_skill_read_only_for_skill_paths() {
        assert!(ReadToolCallBlock::new("/x/skills/deploy/SKILL.md").is_skill_read());
        assert!(!ReadToolCallBlock::new("src/main.rs").is_skill_read());
        assert!(!ReadToolCallBlock::new("/x/skills/deploy/README.md").is_skill_read());
    }

    #[test]
    fn foldable_when_content_present() {
        let block = ReadToolCallBlock::new("f.rs").with_content("hello\nworld".into(), 2);
        assert!(block.is_foldable());
    }

    #[test]
    fn not_foldable_when_no_content() {
        let block = ReadToolCallBlock::new("f.rs");
        assert!(!block.is_foldable());
    }

    #[test]
    fn fold_cycles_collapsed_truncated() {
        let block = ReadToolCallBlock::new("f.rs").with_content("a\nb\nc".into(), 3);
        assert_eq!(
            block.next_fold_mode(DisplayMode::Collapsed, false),
            DisplayMode::Truncated
        );
        assert_eq!(
            block.next_fold_mode(DisplayMode::Truncated, false),
            DisplayMode::Collapsed
        );
    }

    #[test]
    fn truncated_output_includes_content_lines() {
        let content = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let block = ReadToolCallBlock::new("f.rs")
            .with_line_range(LineRange::new(1, 20))
            .with_content(content, 20);
        let ctx = BlockContext {
            mode: DisplayMode::Truncated,
            ..make_ctx()
        };
        let output = block.output(&ctx);
        // Header + blank separator + FIRST_LINES + ellipsis + LAST_LINES
        assert!(
            output.lines.len() > 1,
            "truncated should have content lines"
        );
        let all_text: String = output
            .lines
            .iter()
            .map(|l| {
                l.content
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("line 1"), "should contain first line");
        assert!(all_text.contains("line 20"), "should contain last line");
        assert!(all_text.contains("\u{2026}"), "should contain ellipsis");
    }

    #[test]
    fn absolute_line_numbers_with_offset() {
        let block = ReadToolCallBlock::new("f.rs")
            .with_line_range(LineRange::new(50, 52))
            .with_content("fn foo() {}\nfn bar() {}\nfn baz() {}".into(), 100);
        let ctx = BlockContext {
            mode: DisplayMode::Truncated,
            ..make_ctx()
        };
        let output = block.output(&ctx);
        let all_text: String = output
            .lines
            .iter()
            .map(|l| {
                l.content
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("50"), "should show line 50");
        assert!(all_text.contains("52"), "should show line 52");
    }
}
