//! Consent gate, modelled on folder trust. Which accounts see a notice is a targeting decision.
//!
//! The answer is recorded locally, so it does not survive a second machine or a wiped config.
//!
//! Every failure path fails open: a client that cannot reach settings must stay usable.
//! Validation is the trust boundary, so a notice that survives it is safe to paint.

use std::collections::BTreeMap;

use crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind};
use echo_agent_runtime::util::config::{ConsentAnswer, ConsentGate};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::key;

/// Backstop only. Bytes say nothing about rows, so [`MAX_CONSENT_BODY_ROWS`] is the real bound.
const MAX_CONSENT_BODY_BYTES: usize = 2_000;

/// No scrolling and an unreadable notice cannot be accepted, so the body must fit the smallest
/// terminal we support. `the_largest_allowed_body_paints_at_every_height_it_promises` pins it.
pub(crate) const MAX_CONSENT_BODY_ROWS: usize = 12;

/// Body width on an 80-column terminal: the screen's own wrap width at the default margin.
const REFERENCE_BODY_COLS: u16 = 76;

/// Display columns, so a wide title cannot push the action row off screen.
const MAX_CONSENT_TITLE_COLS: usize = 78;
const MAX_CONSENT_LABEL_COLS: usize = 24;

/// A fat-fingered version must not arm a gate nobody can clear.
const MAX_CONSENT_VERSION: i32 = 10_000;

/// Long enough for a real legal url, short enough that nothing absurd reaches the browser.
const MAX_CONSENT_URL_BYTES: usize = 512;

/// The id becomes a TOML table key in the user's config, so it stays boring.
const MAX_CONSENT_ID_BYTES: usize = 64;

/// `1` through `9`: [`link_index_for_key`] reads one digit, and `0` is not an index.
const MAX_KEYED_LINKS: usize = 9;

/// Named so the reason a payload did not arm the gate reaches the log instead of a bare `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentArmRefusal {
    EmptyBody,
    /// Too tall for the smallest supported terminal, so the gate could never be read or accepted.
    BodyTooTall(usize),
    /// Markup the parser cannot pair off. Painting it would show a url, dropping it would cut the
    /// notice short, and a legal notice may not be shown with a sentence missing.
    UnpairedMarkup,
    /// A url outside a link. Terminals linkify one on sight, so it would open unchecked.
    UrlInPlainText,
    /// Nothing to record an acceptance against.
    MissingVersion,
    ImplausibleVersion(i32),
    /// The id keys both the stored answer and the upstream record.
    UnusableId,
}

/// One piece of the body as painted. A link's index is how a click or number key finds its url.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentSegment {
    Text(String),
    Link { index: usize, label: String },
}

/// Only [`ConsentNotice::try_from_remote`] builds one, so every field here is already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentNotice {
    pub id: String,
    pub version: i32,
    pub title: String,
    /// Painted in order. The raw markdown never reaches the screen.
    pub segments: Vec<ConsentSegment>,
    /// Indexed by [`ConsentSegment::Link`]'s `index`.
    pub links: Vec<String>,
    pub accept_label: String,
}

/// Accept is withheld while `Illegible`, so text that never painted cannot be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsentLegibility {
    #[default]
    Illegible,
    Painted,
}

impl ConsentLegibility {
    pub fn can_accept(self) -> bool {
        matches!(self, Self::Painted)
    }
}

#[derive(Debug)]
pub enum ConsentState {
    Done,
    Pending {
        notice: ConsentNotice,
        legibility: ConsentLegibility,
        /// Keys that arrived before the body painted were aimed at whatever it replaced.
        painted_at: Option<std::time::Instant>,
    },
}

/// Newlines survive; an embedded escape would corrupt the frame, and a bidi or zero-width character
/// would let the painted order differ from the text the acceptance is recorded against.
fn sanitize_notice_text(raw: &str) -> String {
    raw.chars()
        .filter(|c| {
            *c == '\n'
                || !(crate::render::line_utils::is_unsafe_display_char(*c) || is_format_char(*c))
        })
        .collect()
}

/// The invisible characters the shared set misses: soft hyphen, line and
/// paragraph separators, annotation marks.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}' | '\u{2028}' | '\u{2029}' | '\u{fff9}'..='\u{fffb}'
    )
}

/// Every string the server supplies goes through here, so none can skip the cap or the sanitize.
fn text_or(raw: Option<&str>, fallback: &str, max_cols: usize) -> String {
    let cleaned = raw
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(fallback);
    crate::render::line_utils::truncate_str(&sanitize_notice_text(cleaned), max_cols)
}

/// Narrower than a url's own set: a machine with no browser offers the URL for
/// pasting into a shell, so keep it free of quoting and expansion hazards.
fn is_safe_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.' | '_' | '~' | ':' | '/' | '?' | '#' | '@' | '+' | '='
        )
}

fn is_paintable_url(url: &str) -> bool {
    url.starts_with("https://")
        && url.len() <= MAX_CONSENT_URL_BYTES
        && url.chars().all(is_safe_url_char)
        && crate::app::link_opener::is_safe_to_open(
            url,
            crate::terminal::hyperlinks::SchemeFilter::Standard,
        )
}

/// Spaces and combining marks occupy no columns of their own, so text made of them reads as blank.
fn paints_something(text: &str) -> bool {
    UnicodeWidthStr::width(text.trim()) > 0
}

/// The id must be safe as a TOML key in the user's config and legible in the upstream record.
fn is_usable_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CONSENT_ID_BYTES
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Split `[label](url)` out of the body. A url that fails the checks above degrades to its label
/// as plain text, so one bad url costs a hyperlink rather than the notice. Markup that cannot be
/// paired off is refused: the alternatives are painting a url or dropping part of the notice.
fn split_markdown_links(
    body: &str,
) -> Result<(Vec<ConsentSegment>, Vec<String>), ConsentArmRefusal> {
    let mut segments = Vec::new();
    let mut links = Vec::new();
    let mut rest = body;

    while let Some(open) = rest.find('[') {
        // The label ends at the first `]`, so a stray bracket cannot swallow the sentence that
        // follows it into a hyperlink. A `[` with no `]` at all is prose, and carries no url.
        let Some(close) = rest[open..].find(']').map(|i| open + i) else {
            break;
        };
        let Some(after_label) = rest[close..].strip_prefix("](") else {
            // A bracketed phrase, not a link.
            segments.push(ConsentSegment::Text(rest[..=close].to_string()));
            rest = &rest[close + 1..];
            continue;
        };
        // A closer with whitespace or a bracket before it belongs to something further along, and
        // taking it would swallow the text in between. Unsafe characters are a different matter:
        // they make a well-formed link we will not open, which costs the hyperlink, not the copy.
        let Some(url_len) = after_label.find(')').filter(|end| {
            !after_label[..*end].contains(|c: char| c.is_whitespace() || "[](".contains(c))
        }) else {
            return Err(ConsentArmRefusal::UnpairedMarkup);
        };

        let label = &rest[open + 1..close];
        let url = &after_label[..url_len];
        if !rest[..open].is_empty() {
            segments.push(ConsentSegment::Text(rest[..open].to_string()));
        }

        // A label with nothing to paint takes its url with it, rather than leave a link the reader
        // cannot see and a number key that opens it anyway.
        if paints_something(label) {
            if is_paintable_url(url) {
                segments.push(ConsentSegment::Link {
                    index: links.len(),
                    label: label.to_string(),
                });
                links.push(url.to_string());
            } else {
                segments.push(ConsentSegment::Text(label.to_string()));
            }
        }

        rest = &after_label[url_len + 1..];
    }

    // A `](` the loop never reached is a `]` too many earlier in the body.
    if rest.contains("](") {
        return Err(ConsentArmRefusal::UnpairedMarkup);
    }
    if !rest.is_empty() {
        segments.push(ConsentSegment::Text(rest.to_string()));
    }
    Ok((segments, links))
}

impl ConsentNotice {
    /// Validate a served payload.
    ///
    /// # Errors
    ///
    /// [`ConsentArmRefusal`] when the payload cannot safely arm the gate; every variant fails open.
    pub fn try_from_remote(gate: &ConsentGate) -> Result<Self, ConsentArmRefusal> {
        let body_source = gate.body.as_deref().unwrap_or_default();
        // Sanitized before the emptiness check: a body of nothing but control characters would
        // otherwise arm a gate with no text to read.
        let body = sanitize_notice_text(echo_agent_tools::util::truncate_str(
            body_source,
            MAX_CONSENT_BODY_BYTES,
        ));
        let body = body.trim();
        if body.is_empty() {
            return Err(ConsentArmRefusal::EmptyBody);
        }

        let version = gate.version.ok_or(ConsentArmRefusal::MissingVersion)?;
        if version <= 0 || version > MAX_CONSENT_VERSION {
            return Err(ConsentArmRefusal::ImplausibleVersion(version));
        }

        if !is_usable_id(&gate.id) {
            return Err(ConsentArmRefusal::UnusableId);
        }

        let (segments, links) = split_markdown_links(body)?;
        // Terminals linkify a bare url, so one left in the prose would open unchecked.
        if segments
            .iter()
            .any(|s| matches!(s, ConsentSegment::Text(text) if text.contains("://")))
        {
            return Err(ConsentArmRefusal::UrlInPlainText);
        }

        let rows = wrap(&segments, REFERENCE_BODY_COLS);
        // The check above was on the raw body: dropping a link, or a body of nothing but
        // zero-width characters, leaves text the screen will not paint and so cannot accept.
        if !rows
            .iter()
            .flatten()
            .any(|cell| paints_something(&cell.text))
        {
            return Err(ConsentArmRefusal::EmptyBody);
        }
        if rows.len() > MAX_CONSENT_BODY_ROWS {
            return Err(ConsentArmRefusal::BodyTooTall(rows.len()));
        }

        Ok(Self {
            id: gate.id.clone(),
            version,
            title: text_or(
                gate.title.as_deref(),
                "Updates to our terms",
                MAX_CONSENT_TITLE_COLS,
            ),
            segments,
            links,
            accept_label: text_or(
                gate.accept_label.as_deref(),
                "Got it",
                MAX_CONSENT_LABEL_COLS,
            ),
        })
    }
}

/// One grapheme with the columns it occupies and the link it belongs to. Counting characters
/// rather than columns would clip a wide-character notice in half while reporting it painted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyCell {
    pub text: String,
    pub cols: u16,
    pub link: Option<usize>,
}

pub type BodyRow = Vec<BodyCell>;

pub fn row_cols(row: &[BodyCell]) -> u16 {
    row.iter().map(|cell| cell.cols).sum()
}

/// The key rides in the label, not a hint row, so it wraps and underlines with the link and counts
/// against the row cap the validator measures.
fn numbered_label(index: usize, label: &str) -> std::borrow::Cow<'_, str> {
    // Only nine links have a key, so a tenth gets no number it could not act on.
    if index < MAX_KEYED_LINKS {
        std::borrow::Cow::Owned(format!("{label}[{}]", index + 1))
    } else {
        std::borrow::Cow::Borrowed(label)
    }
}

fn flatten(segments: &[ConsentSegment]) -> Vec<BodyCell> {
    let mut cells = Vec::new();
    for segment in segments {
        let (text, link) = match segment {
            ConsentSegment::Text(text) => (std::borrow::Cow::Borrowed(text.as_str()), None),
            ConsentSegment::Link { index, label } => (numbered_label(*index, label), Some(*index)),
        };
        cells.extend(text.graphemes(true).map(|g| BodyCell {
            text: g.to_string(),
            cols: UnicodeWidthStr::width(g) as u16,
            link,
        }));
    }
    cells
}

/// Preserves the source's exact spacing, so no whitespace is invented where two segments meet.
pub fn wrap(segments: &[ConsentSegment], width: u16) -> Vec<BodyRow> {
    if width == 0 {
        return Vec::new();
    }

    let mut rows: Vec<BodyRow> = Vec::new();
    let mut row: BodyRow = Vec::new();
    let mut word: BodyRow = Vec::new();

    let flush_word = |row: &mut BodyRow, rows: &mut Vec<BodyRow>, word: &mut BodyRow| {
        if word.is_empty() {
            return;
        }
        if !row.is_empty() && row_cols(row) + row_cols(word) > width {
            rows.push(std::mem::take(row));
        }
        cut_overlong_word(word, rows, width);
        row.append(word);
    };

    for cell in flatten(segments) {
        match cell.text.as_str() {
            "\n" => {
                flush_word(&mut row, &mut rows, &mut word);
                rows.push(std::mem::take(&mut row));
            }
            " " => {
                flush_word(&mut row, &mut rows, &mut word);
                if !row.is_empty() && row_cols(&row) < width {
                    row.push(cell);
                }
            }
            _ => word.push(cell),
        }
    }

    flush_word(&mut row, &mut rows, &mut word);
    if !row.is_empty() {
        rows.push(row);
    }

    // Trailing blank rows are padding, not a paragraph break the copy asked for.
    while rows.last().is_some_and(Vec::is_empty) {
        rows.pop();
    }
    rows
}

/// A word wider than the body (a bare url) is cut rather than overflow the block.
fn cut_overlong_word(word: &mut BodyRow, rows: &mut Vec<BodyRow>, width: u16) {
    while row_cols(word) > width {
        let mut taken = 0;
        let cut = word
            .iter()
            .take_while(|cell| {
                taken += cell.cols;
                taken <= width
            })
            .count()
            .max(1);
        rows.push(word.drain(..cut).collect());
    }
}

/// Links are addressable by number so they stay reachable with mouse reporting off, which is the
/// norm over ssh and in CI consoles.
pub fn link_index_for_key(key: &crossterm::event::KeyEvent, link_count: usize) -> Option<usize> {
    let crossterm::event::KeyCode::Char(c) = key.code else {
        return None;
    };
    // A bare digit. Alt+1 and Ctrl+1 belong to whatever binds them, not to a link.
    if !key.modifiers.is_empty() {
        return None;
    }
    let index = c.to_digit(10)?.checked_sub(1)? as usize;
    (index < link_count).then_some(index)
}

/// Everything the verdict needs, so the decision stays pure and the caller owns the IO.
pub struct ConsentInputs<'a> {
    pub gate: Option<&'a ConsentGate>,
    /// Answered during this run; the map is only as fresh as the last completed write.
    pub answered_this_run: Option<(&'a str, i32)>,
    /// Every answer this machine holds, keyed by notice id.
    pub answers: &'a BTreeMap<String, ConsentAnswer>,
    /// Whose answers count, so a second account on this machine is asked again.
    pub account: Option<&'a str>,
    /// Minimal mode has no consent renderer, so it stays ungated.
    pub minimal: bool,
}

/// Deliberately does not require the server ack: the local answer stops this machine re-asking.
fn already_answered(inputs: &ConsentInputs<'_>, notice: &ConsentNotice) -> bool {
    let this_run = inputs
        .answered_this_run
        .is_some_and(|(id, version)| id == notice.id && version >= notice.version);

    // An api key carries no email, so a stored answer with no account could be anyone's.
    let Some(account) = inputs.account else {
        return this_run;
    };

    let on_disk = inputs.answers.get(&notice.id).is_some_and(|answer| {
        answer.account.as_deref() == Some(account) && answer.version >= notice.version
    });
    this_run || on_disk
}

pub fn consent_verdict(inputs: &ConsentInputs<'_>) -> ConsentState {
    if inputs.minimal {
        return ConsentState::Done;
    }

    let Some(gate) = inputs.gate else {
        return ConsentState::Done;
    };

    let notice = match ConsentNotice::try_from_remote(gate) {
        Ok(notice) => notice,
        Err(refusal) => {
            tracing::warn!(?refusal, "consent gate not armed; failing open");
            return ConsentState::Done;
        }
    };

    if already_answered(inputs, &notice) {
        return ConsentState::Done;
    }

    ConsentState::Pending {
        notice,
        legibility: ConsentLegibility::Illegible,
        painted_at: None,
    }
}

#[cfg(test)]
#[path = "consent_tests.rs"]
mod tests;

/// What the welcome input arm needs, so the handler does not reach into the whole welcome context.
pub struct ConsentInputCtx<'a> {
    pub state: &'a ConsentState,
    pub arrived_at: std::time::Instant,
    pub menu_rects: &'a [ratatui::layout::Rect],
    pub link_rects: &'a [(usize, ratatui::layout::Rect)],
    pub menu_index: &'a mut Option<usize>,
    pub hover_link: &'a mut Option<usize>,
}

/// Accept is `a`: `y` accepts on the trust screen one step later, and Enter may be buffered before
/// the notice paints. No decline, so `q` quits and the rest is swallowed.
pub fn handle_answer(ev: &Event, ctx: &mut ConsentInputCtx<'_>) -> InputOutcome {
    let (link_count, painted_at, can_accept) = match ctx.state {
        ConsentState::Pending {
            notice,
            painted_at,
            legibility,
        } => (notice.links.len(), *painted_at, legibility.can_accept()),
        ConsentState::Done => (0, None, false),
    };

    // Ctrl+C and Ctrl+D answer nothing and this screen is their only handler, so they land even
    // before the paint. Nothing else does: an earlier event would quit with the composer's text.
    if let Event::Key(key) = ev
        && key.kind != KeyEventKind::Release
        && (key!('c', CONTROL).matches(key) || key!('d', CONTROL).matches(key))
    {
        return InputOutcome::Action(Action::Quit);
    }
    if painted_at.is_none_or(|painted| ctx.arrived_at < painted) {
        return InputOutcome::Unchanged;
    }

    if let Event::Key(key) = ev {
        if key.kind == KeyEventKind::Release {
            return InputOutcome::Unchanged;
        }
        if key!('q').matches(key) || key!('Q').matches(key) {
            return InputOutcome::Action(Action::QuitConfirmed);
        }
        if can_accept && (key!('a').matches(key) || key!('A').matches(key)) {
            return InputOutcome::Action(Action::AcceptConsent);
        }
        // Only when the body is on screen: the links belong to text the reader can see.
        if let Some(index) = link_index_for_key(key, link_count).filter(|_| can_accept) {
            return InputOutcome::Action(Action::OpenConsentLink(index));
        }
        return InputOutcome::Unchanged;
    }

    if let Event::Mouse(mouse) = ev {
        let at = ratatui::layout::Position::new(mouse.column, mouse.row);
        let over_menu_row = ctx.menu_rects.iter().position(|r| r.contains(at));
        let over_link = ctx
            .link_rects
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|(index, _)| *index);

        if matches!(mouse.kind, MouseEventKind::Moved) {
            let mut changed = false;
            if *ctx.menu_index != over_menu_row {
                *ctx.menu_index = over_menu_row;
                changed = true;
            }
            if *ctx.hover_link != over_link {
                *ctx.hover_link = over_link;
                changed = true;
            }

            return if changed {
                InputOutcome::Changed
            } else {
                InputOutcome::Unchanged
            };
        }

        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return InputOutcome::Unchanged;
        }

        // The accept row is painted first and only when the body is readable, so row 0 is accept.
        if let Some(row) = over_menu_row {
            return InputOutcome::Action(if can_accept && row == 0 {
                Action::AcceptConsent
            } else {
                Action::QuitConfirmed
            });
        }
        // Same rule as the number keys: the links belong to text the reader can see.
        if let Some(index) = over_link.filter(|_| can_accept) {
            return InputOutcome::Action(Action::OpenConsentLink(index));
        }
        return InputOutcome::Unchanged;
    }

    if matches!(ev, Event::Resize(_, _)) {
        return InputOutcome::Changed;
    }
    InputOutcome::Unchanged
}
