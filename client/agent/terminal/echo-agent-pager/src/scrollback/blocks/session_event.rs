//! SessionEventBlock — typed session-level events displayed in scrollback.
//!
//! Unlike [`super::SystemMessageBlock`] (which renders arbitrary text),
//! `SessionEventBlock` uses a [`SessionEvent`] enum so each event variant
//! carries structured data (e.g., elapsed time, error messages, token counts).
//! This enables variant-specific rendering and future styling differentiation.

use std::time::Duration;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::tool::HookRunEntry;
use crate::appearance::AppearanceConfig;
use crate::render::wrapping::word_wrap_lines;
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{
    AccentStyle, BlockContext, BlockLine, BlockOutput, DisplayMode, Selectable,
};
use crate::theme::Theme;
use crate::util::format_duration;

/// Shared text-selection range id for recap body lines (header is excluded).
const RECAP_BODY_RANGE: u16 = 0;

/// A session-level event with structured data.
///
/// Each variant carries the information needed to render a concise,
/// informational message in the scrollback. These are non-interactive:
/// unselectable, unfoldable, no accent.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Agent turn completed normally.
    TurnCompleted {
        /// Wall-clock elapsed time for the turn. `None` when unknown — a
        /// wake turn whose deltas carried no `turnStartMs` (old shells)
        /// renders without a duration rather than lying with "0.0s".
        elapsed: Option<Duration>,
    },
    /// Agent turn was cancelled by the user.
    TurnCancelled {
        /// Wall-clock elapsed time before cancellation.
        elapsed: Duration,
    },
    /// Agent turn ended because a hook denied it — today only a
    /// `UserPromptSubmit` block (a `PreToolUse` deny feeds back and the turn
    /// continues). Distinct from [`SessionEvent::TurnCancelled`] so the
    /// marker never claims the USER cancelled a policy block; the warning
    /// annotation above the marker attributes the hook and reason.
    TurnBlockedByHook {
        /// Wall-clock elapsed time before the block.
        elapsed: Duration,
    },
    /// Agent turn was halted by the system (e.g. doom loop detection).
    TurnHalted {
        /// Wall-clock elapsed time before the turn was halted.
        elapsed: Duration,
    },
    /// Agent turn failed with an error.
    TurnFailed {
        /// Error description.
        error: String,
        /// Elapsed time, if available.
        elapsed: Option<Duration>,
    },
    /// Auto-compaction started (context window threshold reached).
    CompactionStarted {
        /// Percentage of context window used (e.g., 85).
        percentage: u8,
    },
    /// Auto-compaction completed successfully.
    CompactionCompleted {
        /// Tokens used before compaction (`None` from older shells).
        tokens_before: Option<u64>,
        /// Tokens used after compaction.
        tokens_after: u64,
        /// How long compaction took (milliseconds).
        elapsed_ms: Option<i64>,
    },
    /// Auto-compaction failed.
    CompactionFailed {
        /// Error description.
        error: String,
    },
    /// Auto-compaction was cancelled (turn was cancelled mid-compact).
    CompactionCancelled,
    /// Retry failed — all retries exhausted or a non-retryable error.
    ///
    /// Covers both `RetryState::Exhausted` (tried N times, all failed) and
    /// `RetryState::Failed` (non-retryable error like auth or 413).
    RetryFailed {
        /// Human-readable error description.
        error: String,
        /// Structured error category from `RetryState::Failed::error_type`.
        /// Used to match known error patterns without fragile string matching.
        error_type: Option<String>,
    },
    /// A non-success API / HTTP response (or similar terminal request error).
    /// Rendered like [`SessionEvent::ReAuthRequired`]: warning color + accent,
    /// no JSON dump.
    RequestFailed {
        /// HTTP status when known. `None` for transport / idle-timeout / etc.
        status: Option<u16>,
        /// Short headline, e.g. `"Server error (500)"`.
        headline: String,
        /// Sanitized one-line detail (server message or fallback guidance).
        detail: String,
    },
    /// The server rejected the credentials (401 / auth error) and automatic
    /// recovery was exhausted. Rendered as a prominent call-to-action that
    /// points the user at `/login` to re-authenticate, replacing the raw
    /// "Retry failed: Unauthorized (401) …" dump.
    ReAuthRequired,
    /// Terminal context overflow — ideally unreachable, since auto-compaction should
    /// shrink the conversation first; a safeguard for when it didn't (estimate drift
    /// vs the server's max_prompt_length, or compaction suppressed/failed). One actionable
    /// prompt, replacing the CompactionFailed + RetryFailed + TurnFailed stack.
    ContextTooLarge,
    /// Session disk is full.
    DiskFull,
    /// Manual `/compact` command completed.
    CompactCompleted {
        /// Wall-clock elapsed time for the command.
        elapsed: Duration,
    },
    /// Hook annotation — displayed inline after a tool call.
    /// Message comes from agent via EchoAgentSessionUpdate::HookAnnotation.
    HookAnnotation {
        /// The hook message
        message: String,
    },
    /// The session's persisted model is no longer available after re-auth.
    /// Both IDs are empty when re-shown on blocked prompt attempts.
    ModelUnavailable {
        previous_model_id: String,
        new_model_id: String,
        reason: String,
    },
    /// Memory was saved (flush, dream, or session-end).
    MemorySaved {
        /// File path that was written.
        path: String,
        /// What triggered the save: "session-end", "flush", or "dream".
        trigger: String,
    },
    /// A `/goal` finished (status → Complete). Carries the goal's total
    /// elapsed time across all its turns, distinct from the per-turn
    /// "Worked for" marker.
    GoalCompleted {
        /// Goal end-to-end elapsed time (`GoalUpdated.elapsed_ms`).
        elapsed: Duration,
    },
    /// A session recap — a short "where was I" summary of the session so far.
    /// Surfaced on demand via `/recap` (`auto = false`) or automatically when
    /// the user returns to the terminal after being away (`auto = true`).
    Recap {
        /// The one-line recap text.
        summary: String,
        /// `true` for the automatic return-from-away recap, `false` for `/recap`.
        auto: bool,
    },
}

impl SessionEvent {
    /// Format the event as a human-readable string.
    pub fn message(&self) -> String {
        match self {
            // Deliberately period-less — don't re-punctuate.
            SessionEvent::TurnCompleted {
                elapsed: Some(elapsed),
            } => {
                format!("Worked for {}", format_duration(*elapsed))
            }
            SessionEvent::TurnCompleted { elapsed: None } => "Turn completed.".to_string(),
            SessionEvent::TurnCancelled { elapsed } => {
                format!("Turn cancelled by user in {}.", format_duration(*elapsed))
            }
            SessionEvent::TurnBlockedByHook { elapsed } => {
                format!("Turn blocked by a hook in {}.", format_duration(*elapsed))
            }
            SessionEvent::TurnHalted { elapsed } => {
                format!(
                    "Agent was unable to make progress. Turn ended in {}.",
                    format_duration(*elapsed)
                )
            }
            SessionEvent::TurnFailed {
                error,
                elapsed: Some(elapsed),
            } => {
                format!("Turn failed in {}: {error}", format_duration(*elapsed))
            }
            SessionEvent::TurnFailed {
                error,
                elapsed: None,
            } => {
                format!("Turn failed: {error}")
            }
            SessionEvent::CompactionStarted { percentage } => {
                format!("Context {percentage}% full. Compacting…")
            }
            SessionEvent::CompactionCompleted {
                tokens_before,
                tokens_after,
                elapsed_ms,
            } => {
                let after = format_tokens(*tokens_after);
                // Older shells don't send tokens_before — keep the legacy format.
                let body = match tokens_before {
                    Some(before) if *before > 0 => {
                        format!(
                            "Context compacted: {} → {after} tokens",
                            format_tokens(*before)
                        )
                    }
                    _ => format!("Context compacted → {after} tokens"),
                };
                if let Some(ms) = elapsed_ms {
                    let secs = *ms as f64 / 1000.0;
                    format!("{body} ({secs:.1}s)")
                } else {
                    body
                }
            }
            SessionEvent::CompactionFailed { error } => {
                if error.trim().is_empty() {
                    "Compaction failed.".to_string()
                } else {
                    format!("Compaction failed: {error}")
                }
            }
            SessionEvent::CompactionCancelled => "Compaction cancelled.".to_string(),
            SessionEvent::RetryFailed { error, error_type } => {
                use crate::app::error_display::WireErrorType;
                if WireErrorType::parse(error_type.as_deref())
                    == WireErrorType::EncryptedContentMismatch
                {
                    "This session's conversation history is incompatible with the \
                     current model. Please start a new session."
                        .to_string()
                } else {
                    format!("Retry failed: {error}")
                }
            }
            SessionEvent::RequestFailed {
                headline, detail, ..
            } => crate::app::error_display::banner_message(headline, detail),
            SessionEvent::ReAuthRequired => {
                "Authentication required: your session has expired or your \
                 credentials were rejected. Run /login to re-authenticate, then resend \
                 your message."
                    .to_string()
            }
            SessionEvent::ContextTooLarge => {
                "This conversation is too large for the model's context window. \
                 Use /new to start a new session."
                    .to_string()
            }
            SessionEvent::DiskFull => {
                echo_agent_runtime::extensions::notification::DISK_FULL_USER_MESSAGE.to_string()
            }
            SessionEvent::CompactCompleted { elapsed } => {
                format!("Compaction completed in {}.", format_duration(*elapsed))
            }
            SessionEvent::HookAnnotation { message } => message.clone(),
            SessionEvent::ModelUnavailable {
                new_model_id,
                reason,
                ..
            } => {
                if new_model_id.is_empty() {
                    reason.clone()
                } else {
                    format!("{reason} Switched to \"{new_model_id}\".")
                }
            }
            SessionEvent::MemorySaved { path, trigger } => {
                let short_path = crate::util::abbreviate_path(path);
                format!("Memory saved ({trigger}) \u{2192} {short_path}  \u{00b7}  /memory to view")
            }
            SessionEvent::GoalCompleted { elapsed } => {
                format!("Goal complete in {} end-to-end.", format_duration(*elapsed))
            }
            SessionEvent::Recap { summary, auto: _ } => {
                // Always "Recap:" (manual `/recap` and auto return-from-away).
                format!("Recap: {summary}")
            }
        }
    }

    /// The recap summary text when this is a [`SessionEvent::Recap`].
    ///
    /// Recap events render in the tool-call visual style (bullet + bold
    /// "Recap" header + muted body); every other variant stays a plain
    /// informational line. This accessor is the single branch point the
    /// `SessionEventBlock` trait methods use to opt the recap into that style.
    fn recap_summary(&self) -> Option<&str> {
        match self {
            SessionEvent::Recap { summary, .. } => Some(summary.as_str()),
            _ => None,
        }
    }

    /// Failures and actionable prompts stand out (warning color + accent bar).
    fn is_warning_banner(&self) -> bool {
        matches!(
            self,
            SessionEvent::ReAuthRequired
                | SessionEvent::ContextTooLarge
                | SessionEvent::DiskFull
                | SessionEvent::CompactionFailed { .. }
                | SessionEvent::RequestFailed { .. }
                | SessionEvent::RetryFailed { .. }
                | SessionEvent::TurnFailed { .. }
        )
    }

    /// Whether this event marks the end of an agent turn (the "Turn
    /// completed/cancelled/failed" markers). These are the only events that
    /// can carry the turn's stop-family hook runs inline.
    ///
    /// [`SessionEvent::RequestFailed`] is intentionally excluded — same as
    /// [`SessionEvent::ReAuthRequired`]. RetryState may push it before
    /// PromptResponse; treating it as terminal would change stop-hook
    /// attribution. Dedicated banners skip the TurnFailed marker and flush
    /// hooks standalone.
    pub fn is_turn_terminal(&self) -> bool {
        matches!(
            self,
            SessionEvent::TurnCompleted { .. }
                | SessionEvent::TurnCancelled { .. }
                | SessionEvent::TurnBlockedByHook { .. }
                | SessionEvent::TurnHalted { .. }
                | SessionEvent::TurnFailed { .. }
        )
    }
}

/// Format a token count with "k" suffix for thousands.
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

/// Block that renders a [`SessionEvent`] in scrollback.
///
/// Visually identical to [`super::SystemMessageBlock`] (muted text, compact,
/// unselectable). The structured `event` field is available for future
/// styling differentiation (e.g., red text for failures).
#[derive(Debug, Clone)]
pub struct SessionEventBlock {
    /// The typed event data.
    pub event: SessionEvent,
    /// Stop-family hook runs folded into a turn-terminal marker
    /// (`(event_name, runs)` per hook batch). Rendered as a right-justified
    /// `stop  [hooks: N]` summary on the marker line, with per-hook detail
    /// on expand. Always empty for non-terminal events.
    pub stop_hooks: Vec<(String, Vec<HookRunEntry>)>,
    /// The prompt turn a terminal marker belongs to, when known. Gates
    /// which stop-hook batches may merge into it.
    pub prompt_id: Option<String>,
}

impl SessionEventBlock {
    /// Create a new session event block.
    pub fn new(event: SessionEvent) -> Self {
        Self {
            event,
            stop_hooks: Vec::new(),
            prompt_id: None,
        }
    }

    /// A turn-terminal marker carrying the turn's stop-hook runs and prompt id.
    pub fn with_stop_hooks(
        event: SessionEvent,
        stop_hooks: Vec<(String, Vec<HookRunEntry>)>,
        prompt_id: Option<String>,
    ) -> Self {
        debug_assert!(stop_hooks.is_empty() || event.is_turn_terminal());
        Self {
            event,
            stop_hooks,
            prompt_id,
        }
    }

    /// Whether any attached stop hook actually ran (non-skipped). Gates the
    /// fold/selection affordances and the inline summary, mirroring
    /// [`ToolCallHookData::has_content`](super::tool::ToolCallHookData::has_content).
    pub fn has_stop_hook_content(&self) -> bool {
        self.stop_hooks.iter().any(|(_, runs)| {
            runs.iter()
                .any(|r| !matches!(r.status, super::tool::HookRunStatus::Skipped))
        })
    }

    /// A recap with real body content — i.e. not the empty loading spinner or a
    /// stray empty recap. Gates the interactive affordances (folding + j/k
    /// selection) so navigation never lands on a recap that can't fold.
    fn recap_has_body(&self) -> bool {
        self.event
            .recap_summary()
            .is_some_and(|s| !s.trim().is_empty())
    }

    /// Merge the stop-hook runs into the marker's output: a right-justified
    /// `stop  [hooks: N]` summary on the marker line (its own right-justified
    /// line when the marker text leaves no room or wraps), plus per-hook
    /// detail lines when expanded.
    ///
    /// The summary spans are decoration — [`Selectable::Spans`] keeps
    /// drag-copy on the marker text only, so a copied "Worked for
    /// 4.4s" never drags the padding and hook counts along.
    fn append_stop_hooks(&self, lines: &mut Vec<BlockLine>, ctx: &BlockContext) {
        use super::tool::hook::{render_hooks_for_mode, render_stop_hooks_summary};

        if !self.has_stop_hook_content() {
            return;
        }
        let Some(summary) = render_stop_hooks_summary(&self.stop_hooks) else {
            return;
        };

        let avail = ctx.width as usize;
        let summary_width: usize = summary
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        // Inline attach is for single-line markers only: on a wrapped marker
        // (a long TurnFailed error) the summary would land mid-paragraph.
        let single_line = lines.len() == 1;
        if let Some(first) = lines.first_mut() {
            let text = crate::scrollback::types::line_plain_text(&first.content);
            let text_width = unicode_width::UnicodeWidthStr::width(text.as_str());
            // Restrict drag-copy to the marker text span(s) before padding.
            let text_spans = first.content.spans.len();
            if single_line && text_width + 2 + summary_width <= avail {
                let pad = avail - text_width - summary_width;
                first.content.spans.push(Span::raw(" ".repeat(pad)));
                first.content.spans.extend(summary);
                first.selectable = Selectable::Spans(0..text_spans);
                first.selection_text = Some(text);
            } else {
                // No room on the marker line (or a wrapped marker) —
                // right-justify on its own line below the text.
                let pad = avail.saturating_sub(summary_width);
                let mut spans = vec![Span::raw(" ".repeat(pad))];
                spans.extend(summary);
                lines.push(BlockLine::separator(Line::from(spans)));
            }
        }

        // Expanded: per-hook detail below the marker line. The section header
        // ("stop") is redundant with the inline summary for a single batch;
        // keep it when both stop_failure and stop ran so the groups read apart.
        if !matches!(ctx.mode, DisplayMode::Collapsed) {
            let multiple = self.stop_hooks.len() > 1;
            for (event_name, runs) in &self.stop_hooks {
                let detail = if multiple {
                    render_hooks_for_mode(event_name, runs, ctx.mode)
                } else {
                    super::tool::hook::render_hooks_detail(runs, ctx.mode)
                };
                lines.extend(detail);
            }
        }
    }

    /// Render a recap event in the tool-call visual style.
    ///
    /// Mirrors [`OtherToolCallBlock`](super::OtherToolCallBlock): a bold
    /// "Recap" header (the dot bullet is prepended later by
    /// `RenderBlock::output` via [`has_bullet`](BlockContent::has_bullet))
    /// with the summary shown as muted body text below when expanded. When
    /// collapsed, the summary's first line trails the header as a preview.
    ///
    /// While the recap is still being generated the entry is `is_running`, so
    /// only the header is shown and the animated accent sidebar (see
    /// [`accent`](BlockContent::accent)) signals progress.
    ///
    /// Text selection mirrors [`ThinkingBlock`](super::ThinkingBlock): the
    /// "Recap" label (and the blank separator under it) are decoration
    /// ([`BlockLine::separator`] / [`Selectable::None`]) so drag-highlight and
    /// copy only include the summary body, never the chrome label.
    fn recap_output(&self, ctx: &BlockContext, summary: &str) -> BlockOutput {
        let theme = Theme::current();
        let muted_collapsed =
            ctx.mute_when_collapsed(ctx.appearance.scrollback.blocks.tool.muted_collapsed);

        // "Recap" header — bold, neutral primary text (like a tool-call header).
        // Dimmed to muted gray while collapsed-and-unselected.
        let header_text_style = if muted_collapsed {
            theme.muted()
        } else {
            theme.primary()
        };
        let header_style = header_text_style.add_modifier(Modifier::BOLD);
        // Non-selectable chrome (same as Thinking / tool label prefixes).
        let header_line =
            || BlockLine::separator(Line::from(Span::styled("Recap".to_string(), header_style)));

        // Loading: header only; the animated gray sidebar is the feedback.
        if ctx.is_running {
            return BlockOutput {
                lines: vec![header_line()],
            };
        }

        match ctx.mode {
            DisplayMode::Collapsed => {
                let mut spans = vec![Span::styled("Recap".to_string(), header_style)];
                let preview = summary.lines().next().unwrap_or(summary).trim();
                if !preview.is_empty() {
                    spans.push(Span::styled(format!("  {preview}"), theme.muted()));
                }
                let line = crate::render::line_utils::truncate_line(
                    Line::from(spans),
                    ctx.content_width(),
                );
                // Only the preview span is copyable — never the "Recap" label.
                // No preview (empty after trim) → fully non-selectable.
                let selectable = if preview.is_empty() {
                    Selectable::None
                } else {
                    Selectable::Spans(1..2)
                };
                BlockOutput {
                    lines: vec![BlockLine {
                        content: line,
                        selectable,
                        selection_range: (!preview.is_empty()).then_some(RECAP_BODY_RANGE),
                        selection_text: (!preview.is_empty()).then(|| preview.to_string()),
                        ..Default::default()
                    }],
                }
            }
            DisplayMode::Truncated | DisplayMode::Expanded => {
                let mut lines: Vec<BlockLine> = vec![header_line()];
                // Blank gap under the header is decoration, not copyable text.
                lines.push(BlockLine::separator(Line::from("")));

                let styled_lines = summary
                    .split('\n')
                    .map(|line| Line::from(Span::styled(line.to_string(), theme.muted())));
                let wrapped =
                    word_wrap_lines(styled_lines, (ctx.width as usize).saturating_sub(2).max(20));
                for wrapped_line in wrapped {
                    lines.push(
                        BlockLine::styled(wrapped_line)
                            .with_selection_range(Some(RECAP_BODY_RANGE)),
                    );
                }

                BlockOutput { lines }
            }
        }
    }
}

impl BlockContent for SessionEventBlock {
    fn output(&self, ctx: &BlockContext) -> BlockOutput {
        // Recap renders in the tool-call style (bullet + bold header + body).
        if let Some(summary) = self.event.recap_summary() {
            return self.recap_output(ctx, summary);
        }

        let theme = Theme::current();
        // Failures and re-auth / context-overflow prompts are actionable, not
        // informational — render them in the warning color, not muted noise.
        let style = if self.event.is_warning_banner() {
            ratatui::style::Style::default().fg(theme.warning)
        } else {
            theme.muted()
        };

        let text = self.event.message();
        let wrapped = if text.contains('\n') {
            let input_lines = text
                .split('\n')
                .map(|s| Line::from(Span::styled(s.to_owned(), style)));
            word_wrap_lines(input_lines, ctx.width as usize)
        } else {
            word_wrap_lines(
                std::iter::once(Line::from(Span::styled(text, style))),
                ctx.width as usize,
            )
        };
        let mut lines: Vec<BlockLine> = wrapped
            .into_iter()
            .map(|line| BlockLine::styled(line).with_selection_range(Some(0)))
            .collect();

        if lines.is_empty() {
            lines.push(BlockLine::styled(Line::from("")).with_selection_range(Some(0)));
        }
        self.append_stop_hooks(&mut lines, ctx);
        BlockOutput { lines }
    }

    fn accent(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        let theme = Theme::current();
        if self.event.recap_summary().is_some() {
            // Loading: animated sidebar so there's feedback that the recap is
            // being generated. Gray rather than the magenta `accent_running` —
            // the recap is a passive marker, not an active tool turn.
            if ctx.is_running {
                return Some(AccentStyle::animated(theme.gray));
            }
            // Finished: neutral tool accent bar when expanded (no special color).
            return (ctx.mode != DisplayMode::Collapsed)
                .then(|| AccentStyle::static_color(theme.accent_tool));
        }
        if self.event.is_warning_banner() {
            Some(AccentStyle::static_color(theme.warning))
        } else {
            None
        }
    }

    fn bullet(&self, ctx: &BlockContext) -> Option<AccentStyle> {
        // Recap: animated dot while loading; default gray dot when collapsed-idle;
        // accent color when expanded. Other events never show a bullet.
        if self.event.recap_summary().is_some()
            && !ctx.is_running
            && ctx.mode == DisplayMode::Collapsed
        {
            return None;
        }
        self.accent(ctx)
    }

    fn has_vpad_for(&self, _appearance: &AppearanceConfig) -> bool {
        false // Compact like SystemMessageBlock
    }

    fn has_raw_mode(&self) -> bool {
        false
    }

    fn is_foldable(&self) -> bool {
        // A recap with body content folds, as does a turn marker carrying
        // stop-hook runs (fold = per-hook detail). Other events are single
        // informational lines with nothing to collapse.
        self.recap_has_body() || self.has_stop_hook_content()
    }

    fn is_selectable(&self) -> bool {
        // Recap is tool-like: navigable so it can be folded — but only once it
        // has body content (mirrors `is_foldable`), so j/k never lands on the
        // loading spinner or an empty recap. A turn marker with stop hooks is
        // navigable for the same reason. Other events stay non-interactive.
        self.recap_has_body() || self.has_stop_hook_content()
    }

    fn default_display_mode(&self) -> DisplayMode {
        // A marker with stop hooks starts collapsed: the right-justified
        // summary is the resting state; detail is opt-in via fold.
        if self.has_stop_hook_content() {
            DisplayMode::Collapsed
        } else {
            DisplayMode::Expanded
        }
    }

    fn has_bullet(&self, ctx: &BlockContext) -> bool {
        // Recap only, and only when the shared tool bullet is configured — so it
        // tracks the same appearance setting as real tool calls.
        self.event.recap_summary().is_some()
            && ctx
                .appearance
                .scrollback
                .blocks
                .tool
                .bullet
                .char()
                .is_some()
    }

    fn is_groupable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_completed_message() {
        let event = SessionEvent::TurnCompleted {
            elapsed: Some(Duration::from_secs(125)),
        };
        assert_eq!(event.message(), "Worked for 2m5s");
    }

    #[test]
    fn turn_cancelled_message() {
        let event = SessionEvent::TurnCancelled {
            elapsed: Duration::from_secs(10),
        };
        assert_eq!(event.message(), "Turn cancelled by user in 10s.");
    }

    #[test]
    fn goal_completed_message_shows_end_to_end_time() {
        let event = SessionEvent::GoalCompleted {
            elapsed: Duration::from_secs(619),
        };
        assert_eq!(event.message(), "Goal complete in 10m19s end-to-end.");
    }

    #[test]
    fn turn_halted_message() {
        let event = SessionEvent::TurnHalted {
            elapsed: Duration::from_secs(45),
        };
        assert_eq!(
            event.message(),
            "Agent was unable to make progress. Turn ended in 45s."
        );
    }

    #[test]
    fn turn_failed_with_elapsed_message() {
        let event = SessionEvent::TurnFailed {
            error: "connection reset".into(),
            elapsed: Some(Duration::from_secs(3)),
        };
        assert_eq!(event.message(), "Turn failed in 3.0s: connection reset");
    }

    #[test]
    fn turn_failed_without_elapsed_message() {
        let event = SessionEvent::TurnFailed {
            error: "auth error".into(),
            elapsed: None,
        };
        assert_eq!(event.message(), "Turn failed: auth error");
    }

    #[test]
    fn model_unavailable_with_switch() {
        let event = SessionEvent::ModelUnavailable {
            previous_model_id: "echo-agent-4.5".into(),
            new_model_id: "echo-agent-build".into(),
            reason: "Model \"echo-agent-4.5\" is no longer available.".into(),
        };
        assert_eq!(
            event.message(),
            "Model \"echo-agent-4.5\" is no longer available. Switched to \"echo-agent-build\"."
        );
    }

    #[test]
    fn model_unavailable_blocked_reprompt() {
        let event = SessionEvent::ModelUnavailable {
            previous_model_id: String::new(),
            new_model_id: String::new(),
            reason: "Your previous model is no longer available. Please start a new session."
                .into(),
        };
        assert_eq!(
            event.message(),
            "Your previous model is no longer available. Please start a new session."
        );
    }

    #[test]
    fn retry_failed_generic() {
        let event = SessionEvent::RetryFailed {
            error: "connection timeout".into(),
            error_type: None,
        };
        assert_eq!(event.message(), "Retry failed: connection timeout");
    }

    #[test]
    fn retry_failed_encrypted_content_mismatch() {
        let event = SessionEvent::RetryFailed {
            error: "raw API error message".into(),
            error_type: Some("encrypted_content_mismatch".into()),
        };
        assert_eq!(
            event.message(),
            "This session's conversation history is incompatible with the \
             current model. Please start a new session."
        );
    }

    #[test]
    fn retry_failed_other_error_type_shows_raw() {
        let event = SessionEvent::RetryFailed {
            error: "bad request".into(),
            error_type: Some("api_400".into()),
        };
        assert_eq!(event.message(), "Retry failed: bad request");
    }

    #[test]
    fn reauth_required_message_points_at_login() {
        let msg = SessionEvent::ReAuthRequired.message();
        assert!(msg.contains("/login"), "must tell the user to run /login");
        assert!(
            msg.to_lowercase().contains("authentication")
                || msg.to_lowercase().contains("credentials"),
            "must explain it is an auth problem: {msg}"
        );
    }

    #[test]
    fn reauth_required_has_warning_accent() {
        let block = SessionEventBlock::new(SessionEvent::ReAuthRequired);
        let theme = Theme::current();
        let accent = block.accent(&ctx());
        assert_eq!(
            accent.map(|a| a.color),
            Some(theme.warning),
            "re-auth prompt must stand out with a warning accent"
        );
    }

    #[test]
    fn request_failed_message_and_warning_accent() {
        let event = SessionEvent::RequestFailed {
            status: Some(500),
            headline: "Server error (500)".into(),
            detail: "upstream exploded".into(),
        };
        assert_eq!(event.message(), "Server error (500): upstream exploded");
        let block = SessionEventBlock::new(event);
        let theme = Theme::current();
        assert_eq!(
            block.accent(&ctx()).map(|a| a.color),
            Some(theme.warning),
            "request-failed banner must stand out like re-auth"
        );
    }

    #[test]
    fn context_too_large_message_is_actionable() {
        let msg = SessionEvent::ContextTooLarge.message();
        assert!(
            msg.to_lowercase().contains("too large"),
            "must explain the conversation is too large: {msg}"
        );
        assert!(
            msg.contains("/new"),
            "must offer /new as the recovery action: {msg}"
        );
    }

    #[test]
    fn context_too_large_has_warning_accent() {
        let block = SessionEventBlock::new(SessionEvent::ContextTooLarge);
        let theme = Theme::current();
        let accent = block.accent(&ctx());
        assert_eq!(
            accent.map(|a| a.color),
            Some(theme.warning),
            "context-too-large prompt must stand out with a warning accent"
        );
    }

    #[test]
    fn compaction_completed_renders_before_after_delta() {
        let event = SessionEvent::CompactionCompleted {
            tokens_before: Some(48_800),
            tokens_after: 27_100,
            elapsed_ms: Some(21_000),
        };
        assert_eq!(
            event.message(),
            "Context compacted: 48.8k → 27.1k tokens (21.0s)"
        );
    }

    #[test]
    fn compaction_completed_without_before_keeps_legacy_format() {
        let event = SessionEvent::CompactionCompleted {
            tokens_before: None,
            tokens_after: 27_100,
            elapsed_ms: None,
        };
        assert_eq!(event.message(), "Context compacted → 27.1k tokens");
    }

    #[test]
    fn compaction_failed_empty_error_is_terse() {
        let event = SessionEvent::CompactionFailed {
            error: String::new(),
        };
        assert_eq!(event.message(), "Compaction failed.");
    }

    #[test]
    fn compaction_failed_curated_error_is_appended() {
        let event = SessionEvent::CompactionFailed {
            error: "out of credits or over your spending limit. Add credits and retry.".into(),
        };
        assert_eq!(
            event.message(),
            "Compaction failed: out of credits or over your spending limit. Add credits and retry."
        );
    }

    #[test]
    fn compaction_failed_has_warning_accent() {
        let block = SessionEventBlock::new(SessionEvent::CompactionFailed {
            error: "out of credits or over your spending limit. Add credits and retry.".into(),
        });
        let theme = Theme::current();
        assert_eq!(
            block.accent(&ctx()).map(|a| a.color),
            Some(theme.warning),
            "an actionable compaction failure must use a warning accent, not muted"
        );
    }

    fn ctx() -> BlockContext {
        BlockContext {
            mode: crate::scrollback::types::DisplayMode::Expanded,
            is_running: false,
            width: 80,
            raw: false,
            max_lines: None,
            appearance: crate::appearance::AppearanceConfig::default(),
            is_selected: false,
            cwd: None,
        }
    }

    #[test]
    fn memory_saved_message_formats_correctly() {
        let event = SessionEvent::MemorySaved {
            path: "/some/absolute/path/MEMORY.md".into(),
            trigger: "flush".into(),
        };
        let msg = event.message();
        assert!(msg.starts_with("Memory saved (flush)"));
        assert!(msg.contains("/memory to view"));
    }

    #[test]
    fn recap_manual_and_auto_use_same_label() {
        let manual = SessionEvent::Recap {
            summary: "refactored the parser".into(),
            auto: false,
        };
        assert_eq!(manual.message(), "Recap: refactored the parser");

        let auto = SessionEvent::Recap {
            summary: "refactored the parser".into(),
            auto: true,
        };
        assert_eq!(auto.message(), "Recap: refactored the parser");
    }

    /// `ctx()` with an overridden display mode / selection state.
    fn recap_ctx(mode: DisplayMode, is_selected: bool) -> BlockContext {
        BlockContext {
            mode,
            is_selected,
            ..ctx()
        }
    }

    /// `ctx()` in the running/loading state (mode is `Expanded` from `ctx()`).
    fn recap_running_ctx() -> BlockContext {
        BlockContext {
            is_running: true,
            ..ctx()
        }
    }

    fn plain(line: &BlockLine) -> String {
        crate::scrollback::types::line_plain_text(&line.content)
    }

    #[test]
    fn recap_renders_tool_style_header_and_body() {
        let block = SessionEventBlock::new(SessionEvent::Recap {
            summary: "Refactored the parser and added tests.".into(),
            auto: false,
        });
        let out = block.output(&recap_ctx(DisplayMode::Expanded, false));
        assert_eq!(
            plain(&out.lines[0]),
            "Recap",
            "header line is the 'Recap' label"
        );
        let body = out.lines.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("Refactored the parser and added tests."),
            "summary is shown as body text: {body}"
        );
    }

    #[test]
    fn recap_is_foldable_selectable_and_bulleted_open_by_default() {
        let block = SessionEventBlock::new(SessionEvent::Recap {
            summary: "did stuff".into(),
            auto: false,
        });
        assert!(
            block.is_foldable(),
            "recap collapses/expands like a tool call"
        );
        assert!(
            block.is_selectable(),
            "recap is navigable so it can be folded"
        );
        assert!(
            block.has_bullet(&ctx()),
            "recap shows the shared tool bullet under default appearance"
        );
        assert_eq!(
            block.default_display_mode(),
            DisplayMode::Expanded,
            "recap is open by default"
        );
    }

    #[test]
    fn recap_accent_and_bullet_use_neutral_tool_color_when_idle() {
        let block = SessionEventBlock::new(SessionEvent::Recap {
            summary: "did stuff".into(),
            auto: false,
        });
        let theme = Theme::current();
        // Expanded: static neutral tool accent bar (no special color).
        let expanded = block.accent(&recap_ctx(DisplayMode::Expanded, false));
        assert_eq!(expanded.map(|a| a.color), Some(theme.accent_tool));
        assert_eq!(expanded.map(|a| a.animated), Some(false));
        // Collapsed: no accent bar; bullet falls back to the default gray dot.
        assert_eq!(
            block.accent(&recap_ctx(DisplayMode::Collapsed, false)),
            None
        );
        assert_eq!(
            block.bullet(&recap_ctx(DisplayMode::Collapsed, false)),
            None
        );
    }

    #[test]
    fn recap_loading_shows_header_only_with_animated_sidebar() {
        // Empty summary + running entry = the in-flight loading state.
        let block = SessionEventBlock::new(SessionEvent::Recap {
            summary: String::new(),
            auto: false,
        });
        let theme = Theme::current();
        let rc = recap_running_ctx();

        // Header only — no blank line or body while still generating.
        let out = block.output(&rc);
        assert_eq!(out.lines.len(), 1, "loading recap is just the header");
        assert_eq!(plain(&out.lines[0]), "Recap");

        // The sidebar + bullet animate in gray (the feedback) — not the magenta
        // running color used for active tool turns.
        let accent = block.accent(&rc).expect("loading recap has an accent bar");
        assert_eq!(accent.color, theme.gray);
        assert!(accent.animated, "loading sidebar animates");
        assert_eq!(
            block.bullet(&rc),
            Some(accent),
            "bullet animates while loading"
        );
    }

    #[test]
    fn recap_collapsed_shows_header_with_first_line_preview() {
        let block = SessionEventBlock::new(SessionEvent::Recap {
            summary: "First line of recap.\nSecond line.".into(),
            auto: false,
        });
        let out = block.output(&recap_ctx(DisplayMode::Collapsed, false));
        assert_eq!(out.lines.len(), 1, "collapsed recap is a single line");
        let text = plain(&out.lines[0]);
        assert!(text.starts_with("Recap"), "starts with the header: {text}");
        assert!(
            text.contains("First line of recap."),
            "shows a preview: {text}"
        );
        assert!(
            !text.contains("Second line"),
            "preview is first line only: {text}"
        );
    }

    #[test]
    fn recap_loading_or_empty_is_not_selectable_or_foldable() {
        // The in-flight spinner (empty summary) — and any stray empty recap —
        // must not be selectable or foldable, so j/k never stops on a block
        // that can't fold and offers no interaction (mirrors `is_foldable`).
        for summary in ["", "   \n  "] {
            let block = SessionEventBlock::new(SessionEvent::Recap {
                summary: summary.into(),
                auto: false,
            });
            assert!(
                !block.is_selectable(),
                "empty/loading recap is not selectable: {summary:?}"
            );
            assert!(
                !block.is_foldable(),
                "empty/loading recap is not foldable: {summary:?}"
            );
        }
    }

    #[test]
    fn recap_expanded_header_is_not_text_selectable() {
        let block = SessionEventBlock::new(SessionEvent::Recap {
            summary: "We fixed the recap body selection.\nSecond line.".into(),
            auto: false,
        });
        let out = block.output(&recap_ctx(DisplayMode::Expanded, false));
        assert!(
            matches!(out.lines[0].selectable, Selectable::None),
            "header must be decoration, not copyable"
        );
        assert_eq!(out.lines[0].selection_range, None);
        assert!(
            matches!(out.lines[1].selectable, Selectable::None),
            "blank gap under header must not be selectable"
        );
        let body: Vec<_> = out.lines.iter().skip(2).collect();
        assert!(!body.is_empty(), "expected body lines");
        for line in body {
            assert!(
                matches!(line.selectable, Selectable::All),
                "body lines are fully selectable"
            );
            assert_eq!(
                line.selection_range,
                Some(0),
                "body shares one selection range so multi-line drag works"
            );
        }
    }

    #[test]
    fn recap_collapsed_only_preview_is_text_selectable() {
        let block = SessionEventBlock::new(SessionEvent::Recap {
            summary: "First line of recap.\nSecond line.".into(),
            auto: false,
        });
        let out = block.output(&recap_ctx(DisplayMode::Collapsed, false));
        assert_eq!(out.lines.len(), 1);
        let line = &out.lines[0];
        assert!(
            matches!(&line.selectable, Selectable::Spans(r) if *r == (1..2)),
            "only the preview span is selectable, not the Recap label: {:?}",
            line.selectable
        );
        assert_eq!(line.selection_range, Some(0));
        assert_eq!(
            line.selection_text.as_deref(),
            Some("First line of recap."),
            "copy payload is the preview body only"
        );
    }

    #[test]
    fn non_recap_events_stay_non_interactive() {
        let block = SessionEventBlock::new(SessionEvent::TurnCompleted {
            elapsed: Some(Duration::from_secs(5)),
        });
        assert!(!block.is_foldable());
        assert!(!block.is_selectable());
        assert!(!block.has_bullet(&ctx()));
        assert_eq!(block.accent(&ctx()), None);
    }

    fn stop_group(name: &str) -> (String, Vec<HookRunEntry>) {
        use super::super::tool::HookRunStatus;
        (
            name.to_string(),
            vec![HookRunEntry {
                name: "global/notify".into(),
                status: HookRunStatus::Success {
                    elapsed: Duration::from_millis(12),
                },
                output: None,
            }],
        )
    }

    fn completed_with_stop_hooks() -> SessionEventBlock {
        SessionEventBlock::with_stop_hooks(
            SessionEvent::TurnCompleted {
                elapsed: Some(Duration::from_secs(5)),
            },
            vec![stop_group("stop")],
            None,
        )
    }

    #[test]
    fn stop_hooks_summary_is_right_justified_on_marker_line() {
        let block = completed_with_stop_hooks();
        let out = block.output(&BlockContext {
            mode: DisplayMode::Collapsed,
            ..ctx()
        });
        assert_eq!(out.lines.len(), 1, "collapsed marker stays a single line");
        let text = plain(&out.lines[0]);
        assert!(
            text.starts_with("Worked for 5.0s"),
            "marker text keeps the left edge: {text}"
        );
        assert!(
            text.ends_with("stop  [hooks: 1]"),
            "summary sits at the right edge: {text}"
        );
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(text.as_str()),
            80,
            "padding right-justifies the summary to the content width"
        );
        // Drag-copy stays on the marker text, never the padding or counts.
        assert!(
            matches!(&out.lines[0].selectable, Selectable::Spans(r) if *r == (0..1)),
            "only the marker text span is selectable: {:?}",
            out.lines[0].selectable
        );
        assert_eq!(
            out.lines[0].selection_text.as_deref(),
            Some("Worked for 5.0s")
        );
    }

    #[test]
    fn stop_hooks_summary_wraps_to_own_line_when_narrow() {
        let block = completed_with_stop_hooks();
        // "Worked for 5.0s" is 15 cols; the summary is 16 — no room
        // at width 30, so the summary right-justifies on its own line.
        let out = block.output(&BlockContext {
            mode: DisplayMode::Collapsed,
            width: 30,
            ..ctx()
        });
        assert_eq!(out.lines.len(), 2);
        let summary_line = plain(&out.lines[1]);
        assert!(summary_line.ends_with("stop  [hooks: 1]"));
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(summary_line.as_str()),
            30
        );
        assert!(
            matches!(out.lines[1].selectable, Selectable::None),
            "the overflow summary line is decoration"
        );
    }

    #[test]
    fn stop_hooks_summary_goes_below_wrapped_multi_line_marker() {
        // A wrapped TurnFailed marker whose first line has room for the
        // summary: attaching there would read mid-paragraph, so the summary
        // right-justifies on its own line below the text instead.
        let block = SessionEventBlock::with_stop_hooks(
            SessionEvent::TurnFailed {
                error: format!("boom {}", "x".repeat(70)),
                elapsed: Some(Duration::from_secs(3)),
            },
            vec![stop_group("stop")],
            None,
        );
        let out = block.output(&BlockContext {
            mode: DisplayMode::Collapsed,
            ..ctx()
        });
        assert_eq!(out.lines.len(), 3, "two wrapped text lines + summary line");
        assert!(
            !plain(&out.lines[0]).contains("[hooks:"),
            "no summary interleaved with the wrapped text: {}",
            plain(&out.lines[0])
        );
        let summary_line = plain(&out.lines[2]);
        assert!(summary_line.ends_with("stop  [hooks: 1]"));
        assert!(
            matches!(out.lines[2].selectable, Selectable::None),
            "the summary line is decoration"
        );
    }

    #[test]
    fn stop_hooks_detail_only_when_expanded() {
        let block = completed_with_stop_hooks();
        let collapsed = block.output(&BlockContext {
            mode: DisplayMode::Collapsed,
            ..ctx()
        });
        let collapsed_text = collapsed
            .lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !collapsed_text.contains("global/notify"),
            "collapsed marker hides per-hook detail: {collapsed_text}"
        );

        let expanded = block.output(&ctx());
        let expanded_text = expanded
            .lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            expanded_text.contains("global/notify (12ms)"),
            "expanded marker shows per-hook detail: {expanded_text}"
        );
    }

    #[test]
    fn marker_with_stop_hooks_is_interactive_and_starts_collapsed() {
        let block = completed_with_stop_hooks();
        assert!(block.is_foldable(), "fold reveals per-hook detail");
        assert!(block.is_selectable(), "navigable so it can be folded");
        assert_eq!(block.default_display_mode(), DisplayMode::Collapsed);

        // All-skipped batches change nothing (mirrors has_content()).
        use super::super::tool::HookRunStatus;
        let skipped = SessionEventBlock::with_stop_hooks(
            SessionEvent::TurnCompleted {
                elapsed: Some(Duration::from_secs(5)),
            },
            vec![(
                "stop".into(),
                vec![HookRunEntry {
                    name: "h".into(),
                    status: HookRunStatus::Skipped,
                    output: None,
                }],
            )],
            None,
        );
        assert!(!skipped.is_foldable());
        assert!(!skipped.is_selectable());
        let out = skipped.output(&BlockContext {
            mode: DisplayMode::Collapsed,
            ..ctx()
        });
        assert_eq!(plain(&out.lines[0]), "Worked for 5.0s");
    }

    #[test]
    fn stop_and_stop_failure_groups_render_labeled_sections() {
        let block = SessionEventBlock::with_stop_hooks(
            SessionEvent::TurnFailed {
                error: "boom".into(),
                elapsed: Some(Duration::from_secs(3)),
            },
            vec![stop_group("stop_failure"), stop_group("stop")],
            None,
        );
        let out = block.output(&BlockContext {
            mode: DisplayMode::Collapsed,
            ..ctx()
        });
        let text = plain(&out.lines[0]);
        assert!(
            text.ends_with("stop_failure  [hooks: 1]  stop  [hooks: 1]"),
            "both groups summarized: {text}"
        );

        let expanded = block.output(&ctx());
        let expanded_text = expanded
            .lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            expanded_text.contains("stop_failure") && expanded_text.contains("global/notify"),
            "multi-group detail keeps section headers: {expanded_text}"
        );
    }

    #[test]
    fn only_turn_terminal_events_accept_stop_hooks() {
        let settled = SessionEventBlock::new(SessionEvent::TurnCompleted {
            elapsed: Some(Duration::from_secs(24)),
        });
        assert!(settled.event.is_turn_terminal());
        let recap = SessionEventBlock::new(SessionEvent::Recap {
            summary: "did stuff".into(),
            auto: false,
        });
        assert!(!recap.event.is_turn_terminal());
    }
}
