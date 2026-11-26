//! The `builtin` row: one segment per [`StatusLineItem`] the session asked for,
//! each already cut to the columns it may use.

use std::time::Duration;

use echo_agent_status_line::{StatusLineContext, StatusLineItem};

use super::fit_columns;

pub const SEGMENT_SEPARATOR: &str = " │ ";

const CONTEXT_WARN_PCT: u8 = 80;

// Columns, not bytes: a byte budget halves a CJK or emoji name.
const CWD_COLS: usize = 40;
const MODEL_COLS: usize = 30;
const SESSION_NAME_COLS: usize = 40;

const MIN_DISPLAYED_COST_USD: f64 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentTone {
    Dim,
    Warn,
}

/// A `builtin` segment, already cut to the columns it may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSegment {
    // Not `pub`: a struct literal elsewhere would skip the control-character
    // filter in [`Self::new`]. Read through [`Self::text`].
    pub(super) text: String,
    pub(super) tone: SegmentTone,
}

impl StatusSegment {
    fn toned(text: String, tone: SegmentTone) -> Self {
        Self::new(text, tone)
    }

    /// Read access for tests in other modules; the fields stay closed so a
    /// literal cannot skip [`Self::new`].
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    fn dim(text: impl Into<String>) -> Self {
        Self::new(text, SegmentTone::Dim)
    }

    pub(crate) fn warn(text: impl Into<String>) -> Self {
        Self::new(text, SegmentTone::Warn)
    }

    /// Control characters are dropped here rather than at the painter: a
    /// segment carries the user's own text, a cwd, a model name, a config value
    /// they typed, and only [`SanitizedText`](super::SanitizedText) filters the
    /// path a script's output takes.
    fn new(text: impl Into<String>, tone: SegmentTone) -> Self {
        let text: String = text.into();
        Self {
            text: text.chars().filter(|c| !c.is_control()).collect(),
            tone,
        }
    }
}

#[must_use]
pub fn compose_builtin(
    ctx: &StatusLineContext,
    turn_elapsed: Option<Duration>,
    items: &[StatusLineItem],
) -> Vec<StatusSegment> {
    items
        .iter()
        .filter_map(|item| match item {
            StatusLineItem::Cwd => {
                let short = ctx.cwd.rsplit(['/', '\\']).find(|s| !s.is_empty())?;
                Some(StatusSegment::dim(fit_columns(short, CWD_COLS)))
            }
            StatusLineItem::Model => {
                let model = ctx
                    .model
                    .display_name
                    .as_deref()
                    .filter(|s| !s.is_empty())?;
                Some(StatusSegment::dim(fit_columns(model, MODEL_COLS)))
            }
            StatusLineItem::Context => {
                let window = &ctx.context_window;
                let pct = window.used_percentage?;
                let warn_at = window
                    .auto_compact_threshold_percent
                    .unwrap_or(CONTEXT_WARN_PCT);
                let tone = if pct >= warn_at {
                    SegmentTone::Warn
                } else {
                    SegmentTone::Dim
                };
                Some(StatusSegment::toned(format!("{pct}% ctx"), tone))
            }
            StatusLineItem::Cost => ctx
                .cost
                .total_cost_usd
                .filter(|usd| *usd >= MIN_DISPLAYED_COST_USD)
                .map(|usd| StatusSegment::dim(format!("${usd:.2}"))),
            StatusLineItem::TurnTimer => {
                let secs = turn_elapsed?.as_secs();
                let text = match secs {
                    0 => return None,
                    s if s < 60 => format!("{s}s"),
                    s => format!("{}m{:02}s", s / 60, s % 60),
                };
                Some(StatusSegment::dim(text))
            }
            StatusLineItem::SessionName => {
                let name = ctx.session_name.as_deref().filter(|s| !s.is_empty())?;
                Some(StatusSegment::dim(fit_columns(name, SESSION_NAME_COLS)))
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "segments_tests.rs"]
mod tests;
