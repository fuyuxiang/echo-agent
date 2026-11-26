//! OtherToolCallBlock - unknown/generic tool types.

use ratatui::text::{Line, Span};

use crate::appearance::AppearanceConfig;
use crate::render::wrapping::word_wrap_lines;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockBackground, BlockContext, BlockLine, BlockOutput, DisplayMode,
};
use crate::theme::Theme;

/// Other/unknown tool call.
#[derive(Debug, Clone)]
pub struct OtherToolCallBlock {
    /// Tool name.
    pub name: String,
    /// Summary/target.
    pub summary: String,
    /// Error message if the tool call failed (None = success).
    pub error: Option<String>,
    /// Optional output.
    pub output: Option<String>,
    /// When the tool started running (Phase 2: time tracking).
    pub started_at: Option<std::time::Instant>,
    /// Elapsed time in ms after completion (Phase 2: time tracking).
    pub elapsed_ms: Option<i64>,
    /// Image references detected in the tool output.
    image_refs: Vec<crate::prompt_images::ScrollbackImageRef>,
    /// Video references detected in the tool output.
    video_refs: Vec<crate::prompt_images::ScrollbackVideoRef>,
}

impl OtherToolCallBlock {
    /// Create a new other tool block.
    ///
    /// Pre-completed blocks have no meaningful local timing — `started_at`
    /// is `None`. Timing is only set for blocks that enter a running UI
    /// state (via `set_last_running(true)` in `ScrollbackState`).
    pub fn new(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            summary: summary.into(),
            error: None,
            output: None,
            started_at: None,
            elapsed_ms: None,
            image_refs: Vec::new(),
            video_refs: Vec::new(),
        }
    }

    /// Set error (marks as failed).
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Set output (builder).
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.set_output_text(output.into());
        self
    }

    /// Set or replace the output text.
    pub fn set_output_text(&mut self, text: String) {
        self.image_refs = crate::prompt_images::extract_image_refs(&text);
        self.video_refs = crate::prompt_images::extract_video_refs(&text);
        self.output = Some(text);
    }

    /// Set the media reference from a typed path (no prose scraping).
    /// `from_path` validates the file and normalizes `\\?\`; an unresolvable
    /// path is a no-op.
    pub fn with_media_ref(mut self, path: impl Into<std::path::PathBuf>, is_video: bool) -> Self {
        let path = path.into();
        if is_video {
            if let Some(r) = crate::prompt_images::ScrollbackVideoRef::from_path(path) {
                self.video_refs = vec![r];
            }
        } else if let Some(r) = crate::prompt_images::ScrollbackImageRef::from_path(path) {
            self.image_refs = vec![r];
        }
        self
    }

    /// Check if successful (no error).
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Path of the first media reference (image or video) for the filepath
    /// line of an inline-media block, independent of inline-graphics support.
    pub(crate) fn media_ref_path(&self) -> Option<std::path::PathBuf> {
        if let Some(img) = self.image_refs.first() {
            return Some(img.path.clone());
        }
        if let Some(vid) = self.video_refs.first() {
            return Some(vid.path.clone());
        }
        None
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

    /// Render collapsed line: **`Label`** `content` or **`Name`**.
    ///
    /// If the name contains `: `, splits into bold label + muted/primary content
    /// (e.g. "Ask: What is your favorite language?"). Otherwise renders
    /// the full name in bold.
    ///
    /// When `muted` is true (collapsed state), all text uses dim styles to
    /// match other collapsed blocks. The label ("Ask") stays bold.
    fn collapsed_line(&self, theme: &Theme, muted: bool, width: Option<usize>) -> Line<'static> {
        let text_style = if muted {
            theme.muted()
        } else {
            theme.primary()
        };
        let bold_style = text_style.add_modifier(ratatui::style::Modifier::BOLD);

        let mut spans = if let Some((label, content)) = self.name.split_once(": ") {
            vec![
                Span::styled(format!("{} ", label), bold_style),
                Span::styled(content.to_string(), text_style),
            ]
        } else {
            vec![Span::styled(self.name.clone(), bold_style)]
        };

        if !self.summary.is_empty() {
            if let Some(w) = width {
                // Only include summary if there's room.
                let used: usize = spans
                    .iter()
                    .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                let summary = format!("  {}", self.summary);
                if used + summary.len() <= w {
                    spans.push(Span::styled(summary, theme.muted()));
                }
            } else {
                spans.push(Span::styled(format!("  {}", self.summary), theme.muted()));
            }
        }

        let line = Line::from(spans);
        if let Some(w) = width {
            crate::render::line_utils::truncate_line(line, w)
        } else {
            line
        }
    }
}

impl BlockContent for OtherToolCallBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        let theme = Theme::current();
        let width = ctx.width as usize;
        let muted_collapsed =
            ctx.mute_when_collapsed(ctx.appearance.scrollback.blocks.tool.muted_collapsed);

        // Inline media blocks (image_gen / video_gen): render the header and a
        // filepath line on every terminal.
        if let Some(media_path) = self.media_ref_path() {
            let header = self.collapsed_line(&theme, muted_collapsed, Some(ctx.content_width()));
            let max_w = ctx.content_width();
            // Percent-decode for display only (e.g. `%2F` → `/`); the stored
            // path is unchanged so Open / copy-path still target the file.
            let raw_path = media_path.display().to_string();
            let path_str = urlencoding::decode(&raw_path)
                .map(|s| s.into_owned())
                .unwrap_or(raw_path);
            // Char-boundary middle-ellipsis (decoded paths may be multibyte).
            let path_display = if path_str.chars().count() > max_w {
                let keep = max_w.saturating_sub(3) / 2;
                let end_keep = max_w.saturating_sub(3) - keep;
                let chars: Vec<char> = path_str.chars().collect();
                let head: String = chars[..keep].iter().collect();
                let tail: String = chars[chars.len() - end_keep..].iter().collect();
                format!("{head}...{tail}")
            } else {
                path_str
            };
            let path_line = Line::from(Span::styled(
                path_display,
                ratatui::style::Style::default().fg(theme.gray_dim),
            ));
            let mut lines: Vec<BlockLine> = vec![header.into(), path_line.into()];

            // No inline graphics: centered "[Open]" button between blank
            // spacers (its click target is registered in render.rs).
            if let Some((_, is_video)) = self.inline_open_button() {
                let label = crate::scrollback::render::media_open_button_label(is_video);
                let col = crate::scrollback::render::media_open_button_col(
                    ctx.content_width() as u16,
                    is_video,
                );
                let open_line = Line::from(vec![
                    Span::raw(" ".repeat(col as usize)),
                    Span::styled(
                        label.to_string(),
                        ratatui::style::Style::default()
                            .fg(theme.md_code)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);
                lines.push(Line::from("").into());
                lines.push(open_line.into());
                lines.push(Line::from("").into());
            }

            return BlockOutput { lines };
        }

        match ctx.mode {
            DisplayMode::Collapsed => BlockOutput {
                lines: vec![
                    self.collapsed_line(&theme, muted_collapsed, Some(ctx.content_width()))
                        .into(),
                ],
            },
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let mut lines: Vec<BlockLine> =
                    vec![self.collapsed_line(&theme, false, None).into()];

                if let Some(output) = &self.output {
                    // Try to render as structured Q&A (AskUserQuestion output).
                    let qa_lines = parse_ask_user_qa_pairs(output);
                    if !qa_lines.is_empty() {
                        for (i, (question, answer)) in qa_lines.iter().enumerate() {
                            // "  1. question text"
                            let q_line = Line::from(vec![
                                Span::styled(format!("  {}. ", i + 1), theme.muted()),
                                Span::styled(question.clone(), theme.primary()),
                            ]);
                            lines.push(BlockLine::styled(q_line));

                            // "     → answer" or "     (no answer)"
                            let a_line = if answer.is_empty() {
                                Line::from(Span::styled(
                                    "     (no answer)".to_string(),
                                    theme.dim(),
                                ))
                            } else {
                                Line::from(vec![
                                    Span::styled(
                                        "     \u{2192} ".to_string(),
                                        theme.fg(theme.accent_user),
                                    ),
                                    Span::styled(answer.clone(), theme.fg(theme.accent_user)),
                                ])
                            };
                            lines.push(BlockLine::styled(a_line));
                        }
                    } else {
                        // Generic output rendering (non-Q&A tools).
                        lines.push(Line::from("").into());

                        let styled_lines: Vec<Line<'static>> = output
                            .lines()
                            .map(|line| Line::from(Span::styled(line.to_string(), theme.muted())))
                            .collect();

                        let wrapped =
                            word_wrap_lines(styled_lines, width.saturating_sub(2).max(20));

                        for wrapped_line in wrapped {
                            lines.push(BlockLine::styled(wrapped_line));
                        }
                    }
                }

                BlockOutput { lines }
            }
        }
    }

    fn accent(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        // No accent when collapsed — keeps accents reserved for Execute blocks in dense groups
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
        // Failed: red bullet. Running/expanded: accent color. Collapsed: default.
        if self.error.is_some() {
            let theme = Theme::current();
            Some(AccentStyle::static_color(theme.accent_error))
        } else if ctx.mode == DisplayMode::Collapsed {
            None // default gray
        } else {
            self.accent(ctx) // inherit from accent when expanded/running
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
        // Not foldable if failed
        if self.error.is_some() {
            return false;
        }
        self.output.is_some()
    }

    fn default_display_mode(&self) -> DisplayMode {
        DisplayMode::Collapsed
    }

    fn next_fold_mode(&self, current: DisplayMode, is_running: bool) -> DisplayMode {
        if is_running {
            match current {
                DisplayMode::Truncated => DisplayMode::Expanded,
                _ => DisplayMode::Truncated,
            }
        } else {
            match current {
                DisplayMode::Collapsed => DisplayMode::Expanded,
                _ => DisplayMode::Collapsed,
            }
        }
    }

    fn collapse_mode(&self, is_running: bool) -> DisplayMode {
        if is_running {
            DisplayMode::Truncated
        } else {
            DisplayMode::Collapsed
        }
    }

    fn image_references(&self) -> &[crate::prompt_images::ScrollbackImageRef] {
        &self.image_refs
    }

    fn video_references(&self) -> &[crate::prompt_images::ScrollbackVideoRef] {
        &self.video_refs
    }

    fn inline_media(&self) -> Option<crate::prompt_images::InlineMediaInfo> {
        if let Some(img) = self.image_refs.first() {
            let (w, h) = img.dimensions?;
            return Some(crate::prompt_images::InlineMediaInfo {
                path: img.path.clone(),
                width: w,
                height: h,
                is_video: false,
                alt_text: img.alt_text.clone(),
            });
        }
        if let Some(vid) = self.video_refs.first() {
            return Some(crate::prompt_images::InlineMediaInfo {
                path: vid.path.clone(),
                width: 1280,
                height: 720,
                is_video: true,
                alt_text: vid.alt_text.clone(),
            });
        }
        None
    }

    fn inline_open_button(&self) -> Option<(std::path::PathBuf, bool)> {
        // Only used when there is no inline-graphics overlay to host the button
        // row. When the overlay is active it draws its own button row instead.
        if crate::terminal::image::scrollback_inline_overlay_active() {
            return None;
        }
        if let Some(img) = self.image_refs.first() {
            return Some((img.path.clone(), false));
        }
        if let Some(vid) = self.video_refs.first() {
            return Some((vid.path.clone(), true));
        }
        None
    }
}

// ── AskUserQuestion output parser ────────────────────────────────────

/// Parse Q&A pairs from an AskUserQuestion tool result string.
///
/// Recognizes all three accepted output formats:
///
/// **Path A (accepted):** `User has answered your questions: "Q1"="A1", "Q2"="A2". You can now...`
/// **Path D (cancelled):** `User declined to answer...`
/// **Paths B/C (plan mode):** `- "Q1"\n  Answer: A1\n- "Q2"\n  (No answer provided)`
///
/// Returns `Vec<(question, answer)>`. Empty vec means the output is not a
/// recognized Q&A format and should be rendered generically.
fn parse_ask_user_qa_pairs(output: &str) -> Vec<(String, String)> {
    // Path A: "User has answered your questions: "Q"="A", "Q"="A". You can now..."
    if let Some(rest) = output.strip_prefix("User has answered your questions: ") {
        // Strip the trailing ". You can now continue with the user's answers in mind."
        let body = rest
            .strip_suffix(". You can now continue with the user's answers in mind.")
            .unwrap_or(rest);

        if body.is_empty() {
            return vec![];
        }

        // Parse "Q1"="A1", "Q2"="A2" pairs.
        // Split on `", "` that appears between pairs (after `"="value"`).
        let mut pairs = Vec::new();
        let mut remaining = body;

        while !remaining.is_empty() {
            // Expect: "question"="answer" [optional annotations...]
            if !remaining.starts_with('"') {
                break;
            }
            remaining = &remaining[1..]; // skip opening "

            // Find the closing " before =
            let Some(q_end) = remaining.find("\"=\"") else {
                break;
            };
            let question = remaining[..q_end].to_string();
            remaining = &remaining[q_end + 3..]; // skip "="

            // Find the end of the answer: next `", "` pair start or end of string.
            // The answer value continues until we hit `, "` (next pair) or end.
            let answer_end = remaining.find(", \"").unwrap_or(remaining.len());

            let mut answer_text = remaining[..answer_end].to_string();
            // Strip trailing quote if present (answer is quoted)
            if answer_text.ends_with('"') {
                answer_text.pop();
            }

            // Remove annotation suffixes (selected preview:..., user notes:...)
            // for display — keep just the label.
            if let Some(ann_start) = answer_text.find(" selected preview:") {
                answer_text.truncate(ann_start);
            }
            if let Some(ann_start) = answer_text.find(" user notes:") {
                answer_text.truncate(ann_start);
            }

            pairs.push((question, answer_text));

            // Advance past the separator
            remaining = &remaining[answer_end..];
            if remaining.starts_with(", ") {
                remaining = &remaining[2..];
            }
        }

        return pairs;
    }

    // Path D: cancelled
    if output.starts_with("User declined to answer") {
        return vec![]; // No Q&A to show
    }

    // Paths B/C: plan mode — bullet format
    // - "Q1"\n  Answer: A1\n- "Q2"\n  (No answer provided)
    if output.contains("Questions asked") && output.contains("- \"") {
        let mut pairs = Vec::new();
        let lines: Vec<&str> = output.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim_start_matches([' ', '-']).trim();
            // Check for "question text"
            if line.starts_with('"') && line.ends_with('"') {
                let question = line[1..line.len() - 1].to_string();
                let answer = if i + 1 < lines.len() {
                    let next = lines[i + 1].trim();
                    if let Some(a) = next.strip_prefix("Answer: ") {
                        i += 1;
                        a.to_string()
                    } else if next == "(No answer provided)" {
                        i += 1;
                        String::new()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                pairs.push((question, answer));
            }
            i += 1;
        }
        if !pairs.is_empty() {
            return pairs;
        }
    }

    vec![]
}
