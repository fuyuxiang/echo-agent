//! Session announcement banner: one slot, critical always wins over promo.
//!
//! Critical layout (always 2 rows when shown):
//! ```text
//! ! Title                                  [hide]   (prefix+title error red, [hide] dim + clickable)
//!   Message…  hide: /announcements hide            (message default fg, CTA dim gray)
//! ```
//!
//! The message row indents past the `! ` prefix so its column matches the
//! title's; the CTA keeps its full reserved width and the message truncates.
//!
//! Promo layout (1 row, only when no critical is selected; the caption is
//! pinned-only and the hide affordances dismissible-only, so they never
//! co-occur):
//! ```text
//! [Label] {cta.caption}                                    (pinned)
//! [Label]        hide: /announcements hide  [hide]    (dismissible)
//! ```
//!
//! The `[Label]` button is the promo's CTA (semantic warning yellow,
//! clickable); it is omitted when the announcement has no usable CTA. A pinned
//! (non-dismissible) promo also paints its dim `cta.caption` helper text (e.g.
//! "or use Ctrl+O") after the button when one is configured (dropped whole when
//! it can't fit, or while a permission prompt owns the chord). The promo
//! `message` is not painted here (it renders on the roomy welcome hero
//! instead). Both hide affordances sit right-aligned (dismissible promos only).
//!
//! `dismissible: false` suppresses every hide affordance on either kind and
//! the text reclaims the reserved columns (absent/`true` = hideable).

use std::collections::BTreeSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
};

use crate::render::line_utils::truncate_str;
use crate::theme::Theme;
use echo_agent_announcements::visible_announcements;

const HIDE_CTA: &str = "hide: /announcements hide";
/// Clickable hide button, far right of the title row.
const HIDE_BUTTON: &str = "[hide]";
/// Alert prefix on the title row; the message row indents by its width.
const TITLE_PREFIX: &str = "! ";
/// Columns between title/message text and the right-hand button/CTA.
const GAP: usize = 2;

/// Columns the `[label]` button (+ optional ` {caption}`) wants — the
/// reservation every surface subtracts from its own budget so the adjacent text
/// (message/path/location) truncates first. Excludes any surface-specific lead
/// space (callers add their own).
pub(crate) fn upgrade_cta_reserve(label: &str, caption: Option<&str>) -> u16 {
    use unicode_width::UnicodeWidthStr;
    let cap_w = caption.map_or(0, |c| UnicodeWidthStr::width(c) + 1);
    (UnicodeWidthStr::width(format!("[{label}]").as_str()) + cap_w) as u16
}

/// Paint the promo upgrade `[label]` button (semantic warning yellow; hovered →
/// warning fg on `bg_hover`) at (`x`, `y`), then the dim `caption` one space
/// after it when it fits. The label truncates to `max_width` and the caption is
/// dropped whole when it no longer fits, so the button never overpaints past
/// `max_width`. Returns the clickable button rect (caption excluded), or `None`
/// when not even a clipped button fits. The ONE painter every surface (banner,
/// hero, in-session header, dashboard) shares so the button/caption style,
/// truncation, and clamping can't drift.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_cta_button(
    buf: &mut Buffer,
    theme: &Theme,
    x: u16,
    y: u16,
    max_width: u16,
    label: &str,
    caption: Option<&str>,
    hovered: bool,
) -> Option<Rect> {
    use unicode_width::UnicodeWidthStr;

    let max_w = max_width as usize;
    if max_w == 0 {
        return None;
    }
    let button = format!("[{label}]");
    let disp = truncate_str(&button, max_w);
    let disp_w = UnicodeWidthStr::width(disp.as_str()).min(max_w);
    if disp_w == 0 {
        return None;
    }
    let cta_style = if hovered {
        Style::default().fg(theme.warning).bg(theme.bg_hover)
    } else {
        Style::default().fg(theme.warning).bg(theme.bg_base)
    };
    buf.set_span(x, y, &Span::styled(disp, cta_style), disp_w as u16);
    // Reservation-first: the button is already painted; the dim caption follows
    // one space later and drops WHOLE when it won't fit (never a partial).
    if let Some(caption) = caption {
        let cap = format!(" {caption}");
        let cap_w = UnicodeWidthStr::width(cap.as_str());
        if disp_w + cap_w <= max_w {
            let cap_style = Style::default()
                .fg(theme.gray)
                .bg(theme.bg_base)
                .add_modifier(Modifier::DIM);
            buf.set_span(
                x + disp_w as u16,
                y,
                &Span::styled(cap, cap_style),
                cap_w as u16,
            );
        }
    }
    Some(Rect::new(x, y, disp_w as u16, 1))
}

fn is_critical(a: &echo_agent_announcements::RemoteAnnouncement) -> bool {
    a.severity.as_deref() == Some("critical")
}

fn is_promo(a: &echo_agent_announcements::RemoteAnnouncement) -> bool {
    a.severity.as_deref() == Some("promo")
}

/// One definition of "live critical" (visible message + critical + not expired)
/// shared by every predicate below so the meanings cannot drift.
fn is_live_critical(
    a: &echo_agent_announcements::RemoteAnnouncement,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    is_critical(a) && !echo_agent_announcements::is_expired_at(a, now)
}

/// Promo twin of [`is_live_critical`]. A CTA is NOT required: a promo without
/// one is still a valid 1-line message row (the selection's visible-message
/// guarantee already skips items with nothing to render).
fn is_live_promo(
    a: &echo_agent_announcements::RemoteAnnouncement,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    is_promo(a) && !echo_agent_announcements::is_expired_at(a, now)
}

/// The session-surfaced severities (critical or promo) — the one name the
/// hide-key set and the slash-gate predicate share so the set of severities
/// that open the in-session slot cannot drift between them.
fn is_live_session_announcement(
    a: &echo_agent_announcements::RemoteAnnouncement,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    is_live_critical(a, now) || is_live_promo(a, now)
}

/// Hideable unless the server says otherwise: absent/`true` = dismissible
/// (back-compat with every pre-flag announcement), only an explicit `false`
/// pins the banner. Shared by the selection seam, both painters, and the
/// hide dispatch so the meanings cannot drift.
pub fn is_dismissible(a: &echo_agent_announcements::RemoteAnnouncement) -> bool {
    a.dismissible != Some(false)
}

/// The hidden-ids filter the selection gates share. It applies only to
/// dismissible items: an explicit `dismissible: false` stays selectable even
/// with its hide key stored, so flipping the flag server-side resurrects a
/// previously-hidden banner (the remote config stays source of truth).
fn is_hidden(
    a: &echo_agent_announcements::RemoteAnnouncement,
    hidden_ids: &BTreeSet<String>,
) -> bool {
    is_dismissible(a) && hidden_ids.contains(&echo_agent_announcements::announcement_hide_key(a))
}

/// The promo's CTA when it is renderable: both label and url trimmed
/// non-empty (the server validates this pair; the tolerant client re-checks
/// so a partial object never paints a dead button), and the url scheme
/// allowed by the same filter the click's open path enforces. This is the
/// ONE gate — paint, hit-rect, OSC 8 emission, and dispatch all inherit it.
/// The scheme re-check fails closed here because OSC 8 activation is
/// terminal-native and would otherwise hand a raw remote URL (`file://`,
/// custom schemes) past `open_url_if_safe` entirely; a non-https CTA renders
/// as a plain message row instead of a dead or unsafe button.
fn usable_cta(a: &echo_agent_announcements::RemoteAnnouncement) -> Option<(&str, &str)> {
    let cta = a.cta.as_ref()?;
    let label = cta
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let url = cta
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|u| {
            crate::app::link_opener::is_safe_to_open(
                u,
                crate::terminal::hyperlinks::SchemeFilter::Standard,
            )
        })?;
    Some((label, url))
}

/// The CTA's optional dim helper caption (`cta.caption`), trimmed-non-empty.
/// Decorative only: deliberately independent of [`usable_cta`] so a caption can
/// never gate, resurrect, or invalidate the button — surfaces AND this with
/// their own pinned/chord gates, and no button painted means no caption shown.
pub(crate) fn usable_cta_caption(a: &echo_agent_announcements::RemoteAnnouncement) -> Option<&str> {
    let caption = a.cta.as_ref()?.caption.as_deref()?.trim();
    (!caption.is_empty()).then_some(caption)
}

/// Wall-clock [`first_critical_session_announcement_at`] — test convenience.
#[cfg(test)]
fn first_critical_session_announcement<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
) -> Option<&'a echo_agent_announcements::RemoteAnnouncement> {
    first_critical_session_announcement_at(announcements, hidden_ids, chrono::Utc::now())
}

/// The critical the session banner shows: first live critical whose hide key
/// is NOT in `hidden_ids` — hiding the first critical reveals the next
/// unhidden one. Info/warning stay welcome-only and do not open the
/// in-session slot. Private: prod consumers go through
/// [`first_session_announcement`]'s `.or_else` leg so slot precedence is
/// structurally enforced. Skips expired items at selection (draw) time so an
/// `expires_at` crossed mid-session stops rendering before the next server
/// push; the per-call timestamp parse and hide-key build are
/// allocation-light and the gate runs at most a few times per frame over a
/// tiny list, so no caching is needed.
fn first_critical_session_announcement_at<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<&'a echo_agent_announcements::RemoteAnnouncement> {
    visible_announcements(announcements)
        .into_iter()
        .find(|a| is_live_critical(a, now) && !is_hidden(a, hidden_ids))
}

/// Wall-clock [`first_promo_session_announcement_at`] — test convenience.
#[cfg(test)]
fn first_promo_session_announcement<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
) -> Option<&'a echo_agent_announcements::RemoteAnnouncement> {
    first_promo_session_announcement_at(announcements, hidden_ids, chrono::Utc::now())
}

/// Promo sibling of [`first_critical_session_announcement_at`] (same expiry
/// seam and hidden-ids filtering). Private for the same reason: only the
/// slot gate's `.or_else` leg consumes it, so nothing can bypass "critical
/// wins" again.
fn first_promo_session_announcement_at<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<&'a echo_agent_announcements::RemoteAnnouncement> {
    visible_announcements(announcements)
        .into_iter()
        .find(|a| is_live_promo(a, now) && !is_hidden(a, hidden_ids))
}

/// The single banner-slot item: the critical selection when one exists
/// (critical always wins the slot), else the promo selection. Per-frame
/// derivation makes the swap automatic when a critical arrives mid-promo.
pub fn first_session_announcement<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
) -> Option<&'a echo_agent_announcements::RemoteAnnouncement> {
    first_session_announcement_at(announcements, hidden_ids, chrono::Utc::now())
}

/// Whether a live critical session announcement exists. Used by the banner
/// slot ranking: critical outranks the privacy upsell banner (an outage
/// notice must not be hidden by a persistent nag), promo does not.
pub fn has_critical_session_announcement(
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
) -> bool {
    first_critical_session_announcement_at(announcements, hidden_ids, chrono::Utc::now()).is_some()
}

/// [`first_session_announcement`] with an injectable clock.
pub fn first_session_announcement_at<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<&'a echo_agent_announcements::RemoteAnnouncement> {
    first_critical_session_announcement_at(announcements, hidden_ids, now)
        .or_else(|| first_promo_session_announcement_at(announcements, hidden_ids, now))
}

/// The upgrade CTA to surface: `(owner, label, url)` resolved through the
/// banner-slot gate. The single resolution shared by every surface that paints
/// the `[label]` button (welcome hero, in-session header, dashboard, banner)
/// and by the click/keyboard/OSC 8 open paths — so show-logic, https-safety,
/// critical-preemption, and expiry are inherited once. `is_dismissible(owner)`
/// tells a surface whether the `Ctrl+O` override applies (pinned promos only).
/// Resolving through [`first_session_announcement`] keeps dispatch
/// slot-consistent: a critical owning the slot yields no target, so a click
/// through a stale prior-frame rect (critical preempted the promo between
/// draws) no-ops.
pub(crate) fn promo_cta<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
) -> Option<(
    &'a echo_agent_announcements::RemoteAnnouncement,
    &'a str,
    &'a str,
)> {
    let owner = first_session_announcement(announcements, hidden_ids).filter(|a| is_promo(a))?;
    let (label, url) = usable_cta(owner)?;
    Some((owner, label, url))
}

/// The `[label]` button's target: the promo owner + its validated url. The
/// url-only projection of [`promo_cta`] the click dispatch (url + announcement
/// id for telemetry) and the OSC 8 emission share.
pub fn promo_cta_target<'a>(
    announcements: &'a [echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
) -> Option<(&'a echo_agent_announcements::RemoteAnnouncement, &'a str)> {
    promo_cta(announcements, hidden_ids).map(|(owner, _label, url)| (owner, url))
}

/// Hide keys of every live (non-expired) session-surfaced announcement
/// (critical or promo) — the set `/announcements show` clears, matching the
/// selection's meaning of visible; prune owns cleanup of keys for
/// expired-but-still-listed items.
pub fn session_announcement_hide_keys(
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
) -> Vec<String> {
    session_announcement_hide_keys_at(announcements, chrono::Utc::now())
}

/// [`session_announcement_hide_keys`] with an injectable clock.
pub fn session_announcement_hide_keys_at(
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    visible_announcements(announcements)
        .into_iter()
        .filter(|a| is_live_session_announcement(a, now))
        .map(echo_agent_announcements::announcement_hide_key)
        .collect()
}

/// Slash-gate predicate: any live session-surfaced announcement (critical or
/// promo) exists, deliberately IGNORING the hidden set (unlike the banner
/// selection above) so `/announcements show` stays reachable while
/// everything is hidden.
pub fn has_session_announcements(
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
) -> bool {
    let now = chrono::Utc::now();
    visible_announcements(announcements)
        .into_iter()
        .any(|a| is_live_session_announcement(a, now))
}

/// Height for the session banner (0 when the selection is empty): 2 when a
/// critical is shown (title row + message row), 1 for the single promo row.
/// Derived from [`first_session_announcement`] so slot precedence lives in
/// exactly one function.
pub fn session_banner_height(
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
) -> u16 {
    match first_session_announcement(announcements, hidden_ids) {
        Some(a) if is_critical(a) => 2,
        Some(_) => 1,
        None => 0,
    }
}

/// Clickable rects painted by [`render_banner`] (`None` = not painted).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BannerHits {
    /// The `[hide]` button.
    pub hide: Option<Rect>,
    /// The promo `[label]` CTA button (critical rows never paint one).
    pub cta: Option<Rect>,
}

/// Shared dim style for the hide affordances (CTA text and resting button).
fn dim_hide_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.gray)
        .bg(theme.bg_base)
        .add_modifier(Modifier::DIM)
}

/// Right-aligned `[hide]` button on `row`, painted FIRST so its width is
/// reserved before any text budget is computed (text truncates, never the
/// button). Hover mirrors the turn-status [stop] affordance: error red on
/// hover, dim at rest. Returns the clickable rect (`None` when it cannot
/// fit) — the one paint/hover/reserve rule both banner painters share.
fn paint_hide_button(
    buf: &mut Buffer,
    area: Rect,
    row: u16,
    hovered: bool,
    theme: &Theme,
) -> Option<Rect> {
    use unicode_width::UnicodeWidthStr;

    let max_w = area.width as usize;
    let button_w = UnicodeWidthStr::width(HIDE_BUTTON);
    if max_w < button_w {
        return None;
    }
    let hide_x = area.x + (max_w - button_w) as u16;
    let button_style = if hovered {
        Style::default().fg(theme.accent_error).bg(theme.bg_base)
    } else {
        dim_hide_style(theme)
    };
    buf.set_span(
        hide_x,
        row,
        &Span::styled(HIDE_BUTTON, button_style),
        button_w as u16,
    );
    Some(Rect::new(hide_x, row, button_w as u16, 1))
}

/// Session top banner: paints the [`first_session_announcement`] selection
/// (slot precedence lives there alone) with the severity-matched painter.
///
/// `caption_allowed` gates the promo row's dim `cta.caption`: the caller
/// passes `false` while a permission prompt owns `Ctrl+O` (it toggles YOLO
/// there, so advertising the CTA open would be a mislabeled control). The
/// `[label]` button + its mouse/OSC 8 open are unaffected.
///
/// Returns the painted clickable rects so the caller can hit-test mouse
/// clicks against them.
pub fn render_banner(
    area: Rect,
    buf: &mut Buffer,
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
    hidden_ids: &BTreeSet<String>,
    hide_hovered: bool,
    cta_hovered: bool,
    caption_allowed: bool,
) -> BannerHits {
    if area.height == 0 || area.width == 0 {
        return BannerHits::default();
    }
    match first_session_announcement(announcements, hidden_ids) {
        Some(a) if is_critical(a) => render_critical_rows(area, buf, a, hide_hovered),
        Some(a) => render_promo_row(area, buf, a, hide_hovered, cta_hovered, caption_allowed),
        None => BannerHits::default(),
    }
}

/// The selected critical announcement, two lines.
///
/// Row 0: `! Title` (error red, title bold) with a right-aligned dim
/// `[hide]` button; row 1: the message (default fg) indented to the title
/// column, then the dim `hide: /announcements hide` CTA. The CTA width is
/// reserved up front so a long message truncates with `…` instead of pushing
/// the CTA off-screen. A non-dismissible announcement paints neither hide
/// affordance and the title/message reclaim the reserved widths.
fn render_critical_rows(
    area: Rect,
    buf: &mut Buffer,
    ann: &echo_agent_announcements::RemoteAnnouncement,
    hide_hovered: bool,
) -> BannerHits {
    use unicode_width::UnicodeWidthStr;

    let theme = Theme::current();
    buf.set_style(area, Style::default().bg(theme.bg_base));

    let title = ann
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let message = ann
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    if title.is_none() && message.is_none() {
        return BannerHits::default();
    }

    // Semantic error red (theme-aware) rather than a raw palette color.
    let alert_fg = theme.accent_error;
    // One visual unit: the `! ` prefix and the title share this style.
    let alert_style = Style::default()
        .fg(alert_fg)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);
    let dim_style = dim_hide_style(&theme);
    let max_w = area.width as usize;
    let prefix_w = UnicodeWidthStr::width(TITLE_PREFIX);
    let button_w = UnicodeWidthStr::width(HIDE_BUTTON);
    let row0 = area.y;
    let row1 = area.y.saturating_add(1);
    let max_y = area.y.saturating_add(area.height);
    let mut hide_rect = None;

    let dismissible = is_dismissible(ann);

    if row0 < max_y {
        // `! ` prefix anchors the alert even when the title is missing.
        let prefix_disp = truncate_str(TITLE_PREFIX, max_w);
        buf.set_span(
            area.x,
            row0,
            &Span::styled(prefix_disp, alert_style),
            area.width,
        );

        // Non-dismissible: no [hide] button — the no-button budget branch
        // below hands its columns back to the title.
        if dismissible {
            hide_rect = paint_hide_button(buf, area, row0, hide_hovered, &theme);
        }

        let title_budget = if hide_rect.is_some() {
            max_w.saturating_sub(prefix_w + button_w + GAP)
        } else {
            max_w.saturating_sub(prefix_w)
        };
        if let Some(t) = title
            && title_budget > 0
        {
            let t_disp = truncate_str(t, title_budget);
            buf.set_span(
                area.x + prefix_w as u16,
                row0,
                &Span::styled(t_disp, alert_style),
                title_budget as u16,
            );
        }
    }

    if row1 < max_y {
        // Message column == title column: indent past the `! ` prefix.
        let mut x = area.x.saturating_add(prefix_w as u16);
        let mut remaining = max_w.saturating_sub(prefix_w);
        let cta_w = UnicodeWidthStr::width(HIDE_CTA);

        // Reserve the CTA (plus gap) up front: the message truncates, never
        // the CTA. Non-dismissible reserves nothing — the message reclaims
        // the full row past the prefix (`W−2`).
        let msg_budget = if dismissible {
            remaining.saturating_sub(cta_w + GAP)
        } else {
            remaining
        };
        if let Some(m) = message
            && msg_budget > 0
        {
            let msg_style = Style::default().fg(theme.text_primary).bg(theme.bg_base);
            let m_disp = truncate_str(m, msg_budget);
            let m_w = UnicodeWidthStr::width(m_disp.as_str()).min(msg_budget);
            if m_w > 0 {
                buf.set_span(x, row1, &Span::styled(m_disp, msg_style), m_w as u16);
                // dismissible: m_w <= msg_budget keeps the gap + full CTA
                // fitting after it; non-dismissible paints no CTA below.
                x = x.saturating_add((m_w + GAP) as u16);
                remaining = remaining.saturating_sub(m_w + GAP);
            }
        }
        if dismissible && remaining > 0 {
            // Degenerate widths still truncate the CTA itself rather than panic.
            let cta_disp = truncate_str(HIDE_CTA, remaining);
            buf.set_span(
                x,
                row1,
                &Span::styled(cta_disp, dim_style),
                remaining as u16,
            );
        }
    }

    BannerHits {
        hide: hide_rect,
        cta: None,
    }
}

/// The selected promo announcement, one line (see the module doc for the
/// pinned/dismissible row sketches).
///
/// The `[Label]` CTA button (semantic warning yellow) leads the row and is
/// omitted when the promo has no usable CTA. The promo `message` is NOT painted
/// here (it renders on the roomy welcome hero instead); a pinned
/// (non-dismissible) promo shows its dim `cta.caption` after the button when
/// one is configured and `caption_allowed` (a dismissible twin keeps `Ctrl+O`
/// on YOLO, so no caption; no caption configured = bare button).
/// The right-hand hide affordances are reserved first (dismissible promos only)
/// so the button + caption never overpaint them.
fn render_promo_row(
    area: Rect,
    buf: &mut Buffer,
    ann: &echo_agent_announcements::RemoteAnnouncement,
    hide_hovered: bool,
    cta_hovered: bool,
    caption_allowed: bool,
) -> BannerHits {
    use unicode_width::UnicodeWidthStr;

    let theme = Theme::current();
    buf.set_style(area, Style::default().bg(theme.bg_base));

    // Selection guarantees a visible message; re-check defensively so a
    // message-less promo paints nothing (the message itself is not drawn).
    let has_message = ann
        .message
        .as_deref()
        .map(str::trim)
        .is_some_and(|m| !m.is_empty());
    if !has_message {
        return BannerHits::default();
    }

    let dim_style = dim_hide_style(&theme);
    let max_w = area.width as usize;
    let button_w = UnicodeWidthStr::width(HIDE_BUTTON);
    let hide_cta_w = UnicodeWidthStr::width(HIDE_CTA);
    let row = area.y;
    let mut hits = BannerHits::default();

    // Non-dismissible: neither hide affordance paints and `right_reserved`
    // stays 0, so the button reclaims the right-hand columns.
    let mut right_reserved = 0usize;
    if is_dismissible(ann) {
        hits.hide = paint_hide_button(buf, area, row, hide_hovered, &theme);
    }
    if hits.hide.is_some() {
        right_reserved = button_w;

        // Dim hide CTA directly left of the button; skipped whole when it
        // cannot fit (it is redundant with [hide], so no partial paint).
        if max_w >= button_w + GAP + hide_cta_w {
            let hide_cta_x = area.x + (max_w - button_w - GAP - hide_cta_w) as u16;
            buf.set_span(
                hide_cta_x,
                row,
                &Span::styled(HIDE_CTA, dim_style),
                hide_cta_w as u16,
            );
            right_reserved = button_w + GAP + hide_cta_w;
        }
    }

    // Left side: the [Label] CTA button (+ pinned-only `cta.caption`), clear of
    // the reserved right-hand hide block. The shared painter owns the style,
    // truncation, and drop-whole caption.
    let remaining = if right_reserved > 0 {
        max_w.saturating_sub(right_reserved + GAP)
    } else {
        max_w
    };
    if remaining > 0
        && let Some((label, _url)) = usable_cta(ann)
    {
        // Caption only for a pinned promo whose `Ctrl+O` actually opens the CTA
        // (suppressed while a permission prompt owns the chord).
        let caption = (caption_allowed && !is_dismissible(ann))
            .then(|| usable_cta_caption(ann))
            .flatten();
        hits.cta = render_cta_button(
            buf,
            &theme,
            area.x,
            row,
            remaining as u16,
            label,
            caption,
            cta_hovered,
        );
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_announcements::RemoteAnnouncement;

    fn ann(severity: Option<&str>, message: Option<&str>) -> RemoteAnnouncement {
        RemoteAnnouncement {
            severity: severity.map(str::to_string),
            message: message.map(str::to_string),
            ..Default::default()
        }
    }

    fn no_hidden() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn promo(id: &str, message: &str, cta: Option<(&str, &str)>) -> RemoteAnnouncement {
        RemoteAnnouncement {
            id: Some(id.into()),
            severity: Some("promo".into()),
            message: Some(message.into()),
            cta: cta.map(|(label, url)| echo_agent_announcements::AnnouncementCta {
                label: Some(label.into()),
                url: Some(url.into()),
                caption: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn first_critical_session_announcement_skips_non_critical_and_empty() {
        let input = vec![
            ann(Some("info"), Some("info only")),
            ann(Some("warning"), Some("warn only")),
            ann(Some("critical"), None),
            ann(Some("critical"), Some("   ")),
            ann(Some("critical"), Some("crit one")),
            ann(Some("critical"), Some("crit two")),
            ann(None, Some("no severity")),
        ];
        let got =
            first_critical_session_announcement(&input, &no_hidden()).expect("critical present");
        assert_eq!(got.message.as_deref(), Some("crit one"));

        assert!(first_critical_session_announcement(&[], &no_hidden()).is_none());
        let no_critical = vec![
            ann(Some("info"), Some("hello")),
            ann(Some("critical"), Some("   ")),
        ];
        assert!(first_critical_session_announcement(&no_critical, &no_hidden()).is_none());
    }

    /// Hidden is a selection-level filter: hiding the first critical reveals
    /// the next unhidden one instead of closing the whole slot, while the
    /// slash-gate predicate keeps ignoring hidden so `show` stays reachable.
    #[test]
    fn first_critical_selection_skips_hidden_and_reveals_next() {
        let list = vec![
            RemoteAnnouncement {
                id: Some("a".into()),
                severity: Some("critical".into()),
                message: Some("A msg".into()),
                ..Default::default()
            },
            RemoteAnnouncement {
                id: Some("b".into()),
                severity: Some("critical".into()),
                message: Some("B msg".into()),
                ..Default::default()
            },
        ];

        let hide_a: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        assert_eq!(
            first_critical_session_announcement(&list, &hide_a).and_then(|a| a.id.as_deref()),
            Some("b"),
            "hiding the first critical must reveal the next one"
        );
        assert_eq!(session_banner_height(&list, &hide_a), 2);

        let hide_both: BTreeSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        assert!(first_critical_session_announcement(&list, &hide_both).is_none());
        assert_eq!(session_banner_height(&list, &hide_both), 0);
        assert!(
            has_session_announcements(&list),
            "slash gate ignores hidden so /announcements show stays reachable"
        );
    }

    /// Draw-time expiry: the selection gate must skip a critical whose
    /// `expires_at` has passed even though it is still in the ingested list.
    #[test]
    fn first_critical_session_announcement_at_skips_expired() {
        let expiring = RemoteAnnouncement {
            severity: Some("critical".into()),
            message: Some("expiring".into()),
            expires_at: Some("2030-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        let evergreen = RemoteAnnouncement {
            severity: Some("critical".into()),
            message: Some("evergreen".into()),
            ..Default::default()
        };
        let list = vec![expiring, evergreen];
        let expiry = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let before = expiry - chrono::Duration::seconds(1);
        assert_eq!(
            first_critical_session_announcement_at(&list, &no_hidden(), before)
                .and_then(|a| a.message.as_deref()),
            Some("expiring")
        );
        assert_eq!(
            first_critical_session_announcement_at(&list, &no_hidden(), expiry)
                .and_then(|a| a.message.as_deref()),
            Some("evergreen"),
            "expired first critical must yield to the next live one"
        );

        let only_expired = vec![list[0].clone()];
        assert!(
            first_critical_session_announcement_at(&only_expired, &no_hidden(), expiry).is_none(),
            "all-expired list must close the banner slot"
        );
    }

    /// Show's clear set matches the selection's meaning of visible: live
    /// (non-expired) criticals and promos only — expired keys are prune's job.
    #[test]
    fn session_hide_keys_cover_live_criticals_and_promos_only() {
        let mut expired_promo = promo("promo-expired", "gone promo", None);
        expired_promo.expires_at = Some("2000-01-01T00:00:00Z".into());
        let list = vec![
            ann(Some("info"), Some("skip me")),
            RemoteAnnouncement {
                id: Some("crit-1".into()),
                severity: Some("critical".into()),
                message: Some("one".into()),
                ..Default::default()
            },
            RemoteAnnouncement {
                id: None,
                title: Some("T".into()),
                severity: Some("critical".into()),
                message: Some("two".into()),
                ..Default::default()
            },
            RemoteAnnouncement {
                id: Some("crit-expired".into()),
                severity: Some("critical".into()),
                message: Some("gone".into()),
                expires_at: Some("2000-01-01T00:00:00Z".into()),
                ..Default::default()
            },
            ann(Some("critical"), None), // no message → not visible
            promo(
                "promo-1",
                "upsell",
                Some(("Go", "https://echo-agent.invalid")),
            ),
            expired_promo,
        ];
        let now = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let keys = session_announcement_hide_keys_at(&list, now);
        assert_eq!(
            keys,
            vec![
                "crit-1".to_string(),
                "content:T\u{1f}two".to_string(),
                "promo-1".to_string(),
            ],
            "expired keys must not be cleared by show; live promo keys must be"
        );
    }

    #[test]
    fn session_banner_height_zero_for_unfiltered_info_only() {
        let info_only = vec![
            ann(Some("info"), Some("hello")),
            ann(Some("warning"), Some("careful")),
        ];
        assert_eq!(session_banner_height(&info_only, &no_hidden()), 0);
    }

    #[test]
    fn session_banner_height_is_two_for_critical() {
        let msg_only = vec![ann(Some("critical"), Some("outage"))];
        assert_eq!(session_banner_height(&msg_only, &no_hidden()), 2);
        let hide_it: BTreeSet<String> = msg_only
            .iter()
            .map(echo_agent_announcements::announcement_hide_key)
            .collect();
        assert_eq!(session_banner_height(&msg_only, &hide_it), 0);

        let with_title = vec![RemoteAnnouncement {
            severity: Some("critical".into()),
            title: Some("Outage".into()),
            message: Some("Do not deploy".into()),
            ..Default::default()
        }];
        assert_eq!(session_banner_height(&with_title, &no_hidden()), 2);
    }

    fn buf_row(buf: &Buffer, area: Rect, y: u16) -> String {
        (0..area.width)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn render_banner_title_row_with_hide_button_message_row_with_cta() {
        let anns = [RemoteAnnouncement {
            severity: Some("critical".into()),
            title: Some("Outage".into()),
            message: Some("Do not deploy".into()),
            ..Default::default()
        }];
        let area = Rect::new(0, 0, 60, 2);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);

        // Row 0: `! Title` left, `[hide]` right-aligned; row 1: message
        // indented to the title column, then the dim CTA after a gap.
        let row0 = buf_row(&buf, area, 0);
        assert!(row0.starts_with("! Outage"), "row0={row0:?}");
        assert!(row0.ends_with(HIDE_BUTTON), "row0={row0:?}");
        assert_eq!(
            buf_row(&buf, area, 1),
            "  Do not deploy  hide: /announcements hide"
        );
        assert_eq!(hits.hide, Some(Rect::new(54, 0, 6, 1)), "[hide] hit rect");
        assert_eq!(hits.cta, None, "critical rows paint no CTA button");
        for y in 0..2 {
            let r = buf_row(&buf, area, y);
            assert!(!r.contains('‼') && !r.contains('⚠') && !r.contains('ℹ'));
        }

        let theme = Theme::current();
        let prefix = buf.cell((0, 0)).unwrap();
        assert_eq!(prefix.fg, theme.accent_error, "! prefix uses error red");
        let title = buf.cell((2, 0)).unwrap();
        assert_eq!(title.fg, theme.accent_error, "title uses error red");
        assert!(title.modifier.contains(Modifier::BOLD));
        let button = buf.cell((54, 0)).unwrap();
        assert_eq!(button.fg, theme.gray);
        assert!(
            button.modifier.contains(Modifier::DIM),
            "[hide] dim at rest"
        );
        let msg = buf.cell((2, 1)).unwrap();
        assert_eq!(msg.fg, theme.text_primary, "message uses default fg");
        assert!(!msg.modifier.contains(Modifier::BOLD));
        let cta = buf.cell((17, 1)).unwrap();
        assert_eq!(cta.fg, theme.gray);
        assert!(cta.modifier.contains(Modifier::DIM), "CTA dim");
    }

    #[test]
    fn render_banner_hide_button_highlights_on_hover() {
        let anns = [ann(Some("critical"), Some("outage"))];
        let area = Rect::new(0, 0, 60, 2);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), true, false, true);
        let rect = hits.hide.expect("hide button painted");
        let button = buf.cell((rect.x, rect.y)).unwrap();
        assert_eq!(button.fg, Theme::current().accent_error);
        assert!(!button.modifier.contains(Modifier::DIM));
    }

    /// Row-1 reservation: the CTA keeps its full width and the message is the
    /// part that truncates (with an ellipsis), never the other way around.
    #[test]
    fn render_banner_truncates_message_never_cta() {
        let anns = [RemoteAnnouncement {
            severity: Some("critical".into()),
            title: Some("Outage".into()),
            message: Some("0123456789ABCDEFGHIJ".into()),
            ..Default::default()
        }];
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);
        // width 40 − 2 indent − (25 CTA + 2 gap) = 11 message columns.
        assert_eq!(
            buf_row(&buf, area, 1),
            "  0123456789…  hide: /announcements hide"
        );
    }

    #[test]
    fn render_banner_shows_first_critical_only() {
        let anns = [
            RemoteAnnouncement {
                severity: Some("info".into()),
                title: Some("Info".into()),
                message: Some("ignored".into()),
                ..Default::default()
            },
            RemoteAnnouncement {
                id: Some("first".into()),
                severity: Some("critical".into()),
                title: Some("First".into()),
                message: Some("one".into()),
                ..Default::default()
            },
            RemoteAnnouncement {
                severity: Some("critical".into()),
                title: Some("Second".into()),
                message: Some("two".into()),
                ..Default::default()
            },
        ];
        let area = Rect::new(0, 0, 40, 2);

        let mut buf = Buffer::empty(area);
        render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);
        let painted = buf_row(&buf, area, 0);
        assert!(painted.starts_with("! First"), "row0={painted:?}");
        assert!(!painted.contains("Second"));
        assert!(!painted.contains("Info"));

        // Hiding the painted critical must paint the next unhidden one.
        let hide_first: BTreeSet<String> = ["first".to_string()].into_iter().collect();
        let mut buf = Buffer::empty(area);
        render_banner(area, &mut buf, &anns, &hide_first, false, false, true);
        let painted = buf_row(&buf, area, 0);
        assert!(painted.starts_with("! Second"), "row0={painted:?}");
        assert!(!painted.contains("First"));
    }

    #[test]
    fn render_banner_ignores_info_only() {
        let anns = [ann(Some("info"), Some("hello"))];
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);
        assert_eq!(hits, BannerHits::default(), "no banner, no hit rects");
        let any: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter_map(|(x, y)| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(!any.contains("hello"));
        assert!(!any.contains(HIDE_BUTTON));
    }

    /// The [hide] button width is reserved before the title budget, so a long
    /// title truncates with an ellipsis instead of overpainting the button.
    #[test]
    fn render_banner_long_title_truncates_before_hide_button() {
        let anns = [RemoteAnnouncement {
            severity: Some("critical".into()),
            title: Some("A".repeat(80)),
            message: Some("MSGBODY".into()),
            ..Default::default()
        }];
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);
        let row0 = buf_row(&buf, area, 0);
        assert!(row0.starts_with("! AAA"), "row0={row0:?}");
        assert!(
            row0.contains('…'),
            "long title must show ellipsis; row0={row0:?}"
        );
        assert!(row0.ends_with(HIDE_BUTTON), "row0={row0:?}");
        assert_eq!(hits.hide, Some(Rect::new(34, 0, 6, 1)));
        assert!(
            buf_row(&buf, area, 1).contains("MSGBODY"),
            "long title must not drop the message row"
        );
    }

    // ── Promo ───────────────────────────────────────────────────────────

    /// Promo selection mirrors the critical gate: severity filter, hidden
    /// skip-reveals-next, and the slash gate stays hidden-agnostic.
    #[test]
    fn first_promo_selection_filters_severity_and_hidden() {
        let list = vec![
            ann(Some("info"), Some("info only")),
            ann(Some("promo"), None), // no message → not visible
            promo("p-a", "A promo", Some(("Go", "https://echo-agent.invalid"))),
            promo("p-b", "B promo", None),
        ];
        assert_eq!(
            first_promo_session_announcement(&list, &no_hidden()).and_then(|a| a.id.as_deref()),
            Some("p-a")
        );
        assert_eq!(session_banner_height(&list, &no_hidden()), 1);

        let hide_a: BTreeSet<String> = ["p-a".to_string()].into_iter().collect();
        assert_eq!(
            first_promo_session_announcement(&list, &hide_a).and_then(|a| a.id.as_deref()),
            Some("p-b"),
            "hiding the first promo must reveal the next one"
        );

        let hide_both: BTreeSet<String> =
            ["p-a".to_string(), "p-b".to_string()].into_iter().collect();
        assert!(first_promo_session_announcement(&list, &hide_both).is_none());
        assert_eq!(session_banner_height(&list, &hide_both), 0);
        assert!(
            has_session_announcements(&list),
            "slash gate ignores hidden so /announcements show stays reachable"
        );

        let info_only = vec![ann(Some("info"), Some("hello"))];
        assert!(!has_session_announcements(&info_only));
    }

    /// Draw-time expiry for promo, via the same injectable clock seam.
    #[test]
    fn first_promo_session_announcement_at_skips_expired() {
        let mut expiring = promo("p-exp", "expiring", None);
        expiring.expires_at = Some("2030-01-01T00:00:00Z".into());
        let list = vec![expiring, promo("p-live", "evergreen", None)];
        let expiry = chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let before = expiry - chrono::Duration::seconds(1);
        assert_eq!(
            first_promo_session_announcement_at(&list, &no_hidden(), before)
                .and_then(|a| a.id.as_deref()),
            Some("p-exp")
        );
        assert_eq!(
            first_promo_session_announcement_at(&list, &no_hidden(), expiry)
                .and_then(|a| a.id.as_deref()),
            Some("p-live"),
            "expired first promo must yield to the next live one"
        );
    }

    /// Critical always wins the single banner slot, regardless of list order;
    /// hiding the critical hands the slot to the promo.
    #[test]
    fn first_session_announcement_prefers_critical_over_promo() {
        let list = vec![
            promo("p", "upsell", Some(("Go", "https://echo-agent.invalid"))),
            RemoteAnnouncement {
                id: Some("c".into()),
                severity: Some("critical".into()),
                message: Some("outage".into()),
                ..Default::default()
            },
        ];
        assert_eq!(
            first_session_announcement(&list, &no_hidden()).and_then(|a| a.id.as_deref()),
            Some("c")
        );
        assert_eq!(session_banner_height(&list, &no_hidden()), 2);

        let hide_crit: BTreeSet<String> = ["c".to_string()].into_iter().collect();
        assert_eq!(
            first_session_announcement(&list, &hide_crit).and_then(|a| a.id.as_deref()),
            Some("p"),
            "hidden critical hands the slot to the promo"
        );
        assert_eq!(session_banner_height(&list, &hide_crit), 1);
    }

    /// The hidden-ids filter applies only to dismissible items: an explicit
    /// `dismissible: false` stays selectable with its hide key stored (a
    /// server-side flag flip resurrects a previously-hidden banner), while
    /// absent/`true` keep today's hidden behavior.
    #[test]
    fn non_dismissible_selected_despite_stored_hide_key() {
        let mut crit = RemoteAnnouncement {
            id: Some("c".into()),
            severity: Some("critical".into()),
            message: Some("pinned outage".into()),
            dismissible: Some(false),
            ..Default::default()
        };
        let mut pinned_promo = promo("p", "pinned promo", None);
        pinned_promo.dismissible = Some(false);
        let hidden: BTreeSet<String> = ["c".to_string(), "p".to_string()].into_iter().collect();

        let list = vec![crit.clone(), pinned_promo];
        assert_eq!(
            first_session_announcement(&list, &hidden).and_then(|a| a.id.as_deref()),
            Some("c"),
            "stored hide key must not filter a non-dismissible critical"
        );
        assert_eq!(session_banner_height(&list, &hidden), 2);
        assert_eq!(
            first_promo_session_announcement(&list, &hidden).and_then(|a| a.id.as_deref()),
            Some("p"),
            "stored hide key must not filter a non-dismissible promo"
        );

        // Back-compat: absent and explicit `true` still honor the hidden set.
        for dismissible in [None, Some(true)] {
            crit.dismissible = dismissible;
            assert!(
                first_session_announcement(&[crit.clone()], &hidden).is_none(),
                "dismissible={dismissible:?} must stay hidden"
            );
        }
    }

    /// Non-dismissible critical: neither hide affordance paints and the
    /// title/message reclaim the reserved widths (title `W−2`, message `W−2`
    /// vs the dismissible `W−2−6−2` / `W−2−27`).
    #[test]
    fn render_critical_rows_non_dismissible_reclaims_hide_columns() {
        let anns = [RemoteAnnouncement {
            severity: Some("critical".into()),
            title: Some("T".repeat(50)),
            message: Some("0123456789ABCDEFGHIJ".into()),
            dismissible: Some(false),
            ..Default::default()
        }];
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);

        assert_eq!(hits.hide, None, "no [hide] target on a pinned banner");
        // Title budget 40−2=38: 37 chars + ellipsis fill to the right edge.
        let row0 = buf_row(&buf, area, 0);
        assert_eq!(row0, format!("! {}…", "T".repeat(37)));
        assert!(!row0.contains(HIDE_BUTTON), "row0={row0:?}");
        // Message budget 40−2=38: the 20-char message fits whole, no hide CTA
        // (the dismissible twin truncates it to 11 columns at this width).
        assert_eq!(buf_row(&buf, area, 1), "  0123456789ABCDEFGHIJ");
    }

    /// Non-dismissible promo: no right-hand hide block, no message — the
    /// clickable `[Go]` button, plus its dim `cta.caption` when one is
    /// configured and `caption_allowed` (suppressed while a permission prompt
    /// owns the chord); no configured caption = bare button. The hit-rect
    /// stays the button only (the caption is not clickable).
    #[test]
    fn render_promo_row_non_dismissible_shows_configured_caption() {
        let mut ann = promo(
            "p",
            &"M".repeat(60),
            Some(("Go", "https://echo-agent.invalid")),
        );
        ann.dismissible = Some(false);
        ann.cta.as_mut().unwrap().caption = Some("or use Ctrl+O".into());
        let anns = [ann];
        let area = Rect::new(0, 0, 50, 1);

        // caption_allowed: the dim caption follows the button; rect is button-only.
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);
        assert_eq!(hits.hide, None, "no [hide] target on a pinned promo");
        assert_eq!(
            hits.cta,
            Some(Rect::new(0, 0, 4, 1)),
            "rect is the button only"
        );
        assert_eq!(
            buf_row(&buf, area, 0),
            "[Go] or use Ctrl+O",
            "button + configured caption; no message painted"
        );

        // Not allowed (a permission prompt owns Ctrl+O): button only, no caption.
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, false);
        assert_eq!(
            hits.cta,
            Some(Rect::new(0, 0, 4, 1)),
            "button still clickable"
        );
        assert_eq!(
            buf_row(&buf, area, 0),
            "[Go]",
            "caption suppressed when not allowed"
        );

        // No caption configured: the pinned row stays a bare button even with
        // `caption_allowed` (nothing hardcoded fills in).
        let mut bare = promo(
            "p",
            &"M".repeat(60),
            Some(("Go", "https://echo-agent.invalid")),
        );
        bare.dismissible = Some(false);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &[bare], &no_hidden(), false, false, true);
        assert_eq!(hits.cta, Some(Rect::new(0, 0, 4, 1)));
        assert_eq!(
            buf_row(&buf, area, 0),
            "[Go]",
            "absent caption renders nothing after the button"
        );
    }

    /// `promo_cta_target` requires BOTH trimmed-non-empty label and url — a
    /// partial CTA never produces an openable target (or a painted button).
    #[test]
    fn promo_cta_target_requires_usable_pair() {
        let full = vec![promo(
            "p",
            "msg",
            Some(("Go", " https://echo-agent.invalid/promo ")),
        )];
        let (a, url) = promo_cta_target(&full, &no_hidden()).expect("usable target");
        assert_eq!(a.id.as_deref(), Some("p"));
        assert_eq!(url, "https://echo-agent.invalid/promo");

        let mut label_only = promo("p", "msg", None);
        label_only.cta = Some(echo_agent_announcements::AnnouncementCta {
            label: Some("Go".into()),
            url: None,
            caption: None,
        });
        assert!(usable_cta(&label_only).is_none());
        assert!(promo_cta_target(&[label_only], &no_hidden()).is_none());

        let mut blank_url = promo("p", "msg", Some(("Go", "   ")));
        assert!(usable_cta(&blank_url).is_none());
        blank_url.cta = None;
        assert!(promo_cta_target(&[blank_url], &no_hidden()).is_none());

        // Hidden promo: no click target exists, so nothing to open.
        let hidden: BTreeSet<String> = ["p".to_string()].into_iter().collect();
        assert!(promo_cta_target(&full, &hidden).is_none());
    }

    /// `usable_cta_caption` is trim-nonempty of `cta.caption` and deliberately
    /// independent of CTA validity — but an unusable CTA paints no button, so
    /// its caption can never surface on the promo row either.
    #[test]
    fn usable_cta_caption_trims_and_never_resurrects_unusable_cta() {
        let mut p = promo("p", "msg", Some(("Go", "https://echo-agent.invalid")));
        assert_eq!(usable_cta_caption(&p), None, "absent caption");
        p.cta.as_mut().unwrap().caption = Some("  or use Ctrl+O  ".into());
        assert_eq!(usable_cta_caption(&p), Some("or use Ctrl+O"), "trimmed");
        p.cta.as_mut().unwrap().caption = Some("   ".into());
        assert_eq!(usable_cta_caption(&p), None, "whitespace-only = none");

        // Caption on a url-less CTA: the accessor still reads it (validity is
        // usable_cta's job alone)…
        let mut label_only = promo("q", &"M".repeat(60), None);
        label_only.dismissible = Some(false);
        label_only.cta = Some(echo_agent_announcements::AnnouncementCta {
            label: Some("Go".into()),
            url: None,
            caption: Some("or use Ctrl+O".into()),
        });
        assert!(usable_cta(&label_only).is_none());
        assert_eq!(usable_cta_caption(&label_only), Some("or use Ctrl+O"));

        // …but the promo row paints no button, hence no caption anywhere.
        let area = Rect::new(0, 0, 50, 1);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(
            area,
            &mut buf,
            &[label_only],
            &no_hidden(),
            false,
            false,
            true,
        );
        assert_eq!(hits.cta, None, "unusable CTA never arms a button rect");
        assert_eq!(
            buf_row(&buf, area, 0),
            "",
            "no button means the caption cannot surface"
        );
    }

    /// `promo_cta` projects the owner + validated `(label, url)` all surfaces
    /// paint from; `is_dismissible(owner)` distinguishes the pinned (Ctrl+O)
    /// promo from a dismissible one.
    #[test]
    fn promo_cta_returns_label_and_pinned_flag() {
        let mut pinned = promo(
            "p",
            "msg",
            Some(("Upgrade Account", "https://echo-agent.invalid/promo")),
        );
        pinned.dismissible = Some(false);
        let pinned = [pinned];
        let (owner, label, url) = promo_cta(&pinned, &no_hidden()).expect("usable cta");
        assert_eq!(label, "Upgrade Account");
        assert_eq!(url, "https://echo-agent.invalid/promo");
        assert!(
            !is_dismissible(owner),
            "pinned promo drives the Ctrl+O override"
        );

        let dismissible = [promo(
            "d",
            "msg",
            Some(("Go", "https://echo-agent.invalid")),
        )];
        let (owner, label, _) = promo_cta(&dismissible, &no_hidden()).expect("usable cta");
        assert_eq!(label, "Go");
        assert!(
            is_dismissible(owner),
            "absent flag = dismissible = no override"
        );

        assert!(promo_cta(&[promo("n", "msg", None)], &no_hidden()).is_none());
    }

    /// The shared CTA-button painter (used by all four surfaces) clamps the
    /// button to `max_width` — never overpainting past it — and returns the
    /// clickable button rect. This is what keeps the in-session header /
    /// dashboard CTA from writing over the right-aligned status group / chips.
    #[test]
    fn render_cta_button_clamps_to_max_width() {
        let theme = Theme::current();

        // Fits: the full button; the rect covers it (no caption requested).
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        let rect = render_cta_button(&mut buf, &theme, 0, 0, 30, "Upgrade", None, false)
            .expect("button fits");
        assert_eq!(rect, Rect::new(0, 0, 9, 1), "rect covers `[Upgrade]` only");
        assert_eq!(buf_row(&buf, area, 0), "[Upgrade]");

        // Caption fits: painted after the button; the rect stays button-only.
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let rect = render_cta_button(&mut buf, &theme, 0, 0, 40, "Go", Some("hi there"), false)
            .expect("button fits");
        assert_eq!(rect, Rect::new(0, 0, 4, 1), "rect excludes the caption");
        assert_eq!(buf_row(&buf, area, 0), "[Go] hi there");

        // Caption drops WHOLE when only the button fits (never a partial caption).
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        let rect = render_cta_button(&mut buf, &theme, 0, 0, 6, "Go", Some("hi there"), false)
            .expect("button still paints");
        assert_eq!(rect, Rect::new(0, 0, 4, 1));
        assert_eq!(buf_row(&buf, area, 0), "[Go]", "caption dropped whole");

        // Tight: the button truncates — nothing exceeds `max_width`, so the
        // header/dashboard CTA can't overpaint the status group.
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        let rect = render_cta_button(&mut buf, &theme, 0, 0, 6, "Upgrade Account", None, false)
            .expect("clipped button still paints");
        assert!(
            rect.width <= 6,
            "button clamped to max_width; rect={rect:?}"
        );
        let row = buf_row(&buf, area, 0);
        assert!(
            row.chars().count() <= 6,
            "no overpaint past max_width; row={row:?}"
        );

        // Zero budget paints nothing and arms no rect.
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        assert!(render_cta_button(&mut buf, &theme, 0, 0, 0, "X", None, false).is_none());
    }

    /// The reserve helper counts the `[label]` button plus the optional caption.
    #[test]
    fn upgrade_cta_reserve_counts_button_and_caption() {
        assert_eq!(upgrade_cta_reserve("Go", None), 4); // `[Go]`
        // `[Go]` (4) + leading space (1) + caption width (5).
        assert_eq!(upgrade_cta_reserve("Go", Some("hello")), 4 + 1 + 5);
    }

    /// Slot consistency: `promo_cta_target` resolves through the banner-slot
    /// gate, so a live critical owning the slot yields no target (a click
    /// through a stale prior-frame rect must not open the promo URL) — and
    /// the promo resolves again once the critical is hidden or expired.
    #[test]
    fn promo_cta_target_yields_to_critical_slot_owner() {
        let promo_ann = promo(
            "p",
            "upsell",
            Some(("Go", "https://echo-agent.invalid/promo")),
        );
        let crit = RemoteAnnouncement {
            id: Some("c".into()),
            severity: Some("critical".into()),
            message: Some("outage".into()),
            ..Default::default()
        };

        let both = vec![promo_ann.clone(), crit.clone()];
        assert!(
            promo_cta_target(&both, &no_hidden()).is_none(),
            "a critical slot owner must yield no CTA target"
        );

        // Hiding the (dismissible) critical hands the slot back to the promo.
        let hide_crit: BTreeSet<String> = ["c".to_string()].into_iter().collect();
        assert_eq!(
            promo_cta_target(&both, &hide_crit).map(|(a, url)| (a.id.as_deref(), url)),
            Some((Some("p"), "https://echo-agent.invalid/promo"))
        );

        // So does the critical expiring (same draw/dispatch-time expiry gate).
        let mut expired_crit = crit;
        expired_crit.expires_at = Some("2000-01-01T00:00:00Z".into());
        let with_expired = vec![promo_ann, expired_crit];
        assert_eq!(
            promo_cta_target(&with_expired, &no_hidden()).and_then(|(a, _)| a.id.as_deref()),
            Some("p")
        );
    }

    /// The one CTA gate fails closed on schemes outside the Standard open
    /// allowlist: no painted button, no OSC 8 target, no dispatch url — the
    /// promo renders no button (its message is never painted on the banner).
    #[test]
    fn usable_cta_rejects_unsafe_schemes() {
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "vscode://open",
            "not a url",
        ] {
            let a = promo("p", "msg", Some(("Go", bad)));
            assert!(usable_cta(&a).is_none(), "scheme must be rejected: {bad}");
            assert!(promo_cta_target(&[a], &no_hidden()).is_none());
        }
        for good in [
            "https://echo-agent.invalid/promo",
            "http://echo-agent.invalid/promo",
        ] {
            let a = promo("p", "msg", Some(("Go", good)));
            assert!(usable_cta(&a).is_some(), "scheme must be allowed: {good}");
        }

        // Unsafe url arms no button; the message is never painted here, so a
        // dismissible promo shows only its hide affordances.
        let anns = [promo("p", "Plain message", Some(("Go", "file:///x")))];
        let area = Rect::new(0, 0, 60, 1);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);
        assert_eq!(hits.cta, None, "unsafe CTA must not arm a click target");
        let row0 = buf_row(&buf, area, 0);
        assert!(!row0.contains("[Go]"), "row0={row0:?}");
        assert!(!row0.contains("Plain message"), "row0={row0:?}");
        assert!(row0.ends_with(HIDE_BUTTON), "row0={row0:?}");
    }

    /// Dismissible promo row: `[Label]` leads (warning yellow), NO message and
    /// NO caption even when one is configured (dismissible keeps `Ctrl+O` on
    /// YOLO, so the caption is pinned-only regardless of `caption_allowed`),
    /// hide affordances right-aligned; rects for both buttons.
    #[test]
    fn render_promo_row_button_and_hide_affordances() {
        let mut ann = promo(
            "p",
            "New promo",
            Some(("Get EchoAgent", "https://echo-agent.invalid/echoagent")),
        );
        ann.cta.as_mut().unwrap().caption = Some("or use Ctrl+O".into());
        let anns = [ann];
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);

        let row0 = buf_row(&buf, area, 0);
        assert!(row0.starts_with("[Get EchoAgent]"), "row0={row0:?}");
        assert!(
            !row0.contains("New promo"),
            "message must not paint on the banner; row0={row0:?}"
        );
        assert!(
            !row0.contains("Ctrl+O"),
            "a dismissible promo suppresses its configured caption; row0={row0:?}"
        );
        assert!(row0.ends_with(HIDE_BUTTON), "row0={row0:?}");
        assert!(row0.contains(HIDE_CTA), "row0={row0:?}");

        // [Label] = 15 cols at x 0; [hide] right-aligned at 80−6=74; the hide
        // CTA ends gap-adjacent to it (74−2−25=47).
        assert_eq!(hits.cta, Some(Rect::new(0, 0, 15, 1)), "[Label] hit rect");
        assert_eq!(hits.hide, Some(Rect::new(74, 0, 6, 1)), "[hide] hit rect");

        let theme = Theme::current();
        let button = buf.cell((0, 0)).unwrap();
        assert_eq!(button.fg, theme.warning, "[Label] uses semantic warning");
        let hide = buf.cell((74, 0)).unwrap();
        assert_eq!(hide.fg, theme.gray);
        assert!(hide.modifier.contains(Modifier::DIM), "[hide] dim at rest");
        let hide_cta = buf.cell((47, 0)).unwrap();
        assert_eq!(hide_cta.fg, theme.gray);
        assert!(hide_cta.modifier.contains(Modifier::DIM), "hide CTA dim");
    }

    #[test]
    fn render_promo_row_hover_styles() {
        let anns = [promo(
            "p",
            "msg",
            Some(("Go", "https://echo-agent.invalid")),
        )];
        let area = Rect::new(0, 0, 80, 1);
        let theme = Theme::current();

        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), true, false, true);
        let hide = hits.hide.expect("hide painted");
        let cell = buf.cell((hide.x, hide.y)).unwrap();
        assert_eq!(cell.fg, theme.accent_error, "[hide] hover uses error red");
        assert!(!cell.modifier.contains(Modifier::DIM));

        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, true, true);
        let cta = hits.cta.expect("cta painted");
        let cell = buf.cell((cta.x, cta.y)).unwrap();
        assert_eq!(cell.fg, theme.warning, "[Label] keeps warning fg on hover");
        assert_eq!(cell.bg, theme.bg_hover, "[Label] hover highlights bg");
    }

    /// Reservation-first budget: the hide affordances keep their full width and
    /// only the `[Label]` button truncates when the row is tight (dismissible
    /// promo, width 50: hide block 25+2+6 reserved → ~15 cols for the button).
    #[test]
    fn render_promo_row_truncates_button_never_affordances() {
        let anns = [promo(
            "p",
            "msg",
            Some((
                "Upgrade to EchoAgent Heavy for the exclusive preview",
                "https://echo-agent.invalid",
            )),
        )];
        let area = Rect::new(0, 0, 50, 1);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);
        let row0 = buf_row(&buf, area, 0);
        assert!(row0.starts_with("[Upgrade"), "row0={row0:?}");
        assert!(
            row0.contains('…'),
            "button label must truncate; row0={row0:?}"
        );
        assert!(row0.contains(HIDE_CTA), "row0={row0:?}");
        assert!(row0.ends_with(HIDE_BUTTON), "row0={row0:?}");
        assert!(hits.cta.is_some());
        assert_eq!(hits.hide, Some(Rect::new(44, 0, 6, 1)));
    }

    /// No usable CTA → no button and no cta rect; the message is not painted on
    /// the banner, so a dismissible promo shows only its hide affordances.
    #[test]
    fn render_promo_row_without_cta_paints_no_button() {
        let anns = [promo("p", "Plain promo message", None)];
        let area = Rect::new(0, 0, 60, 1);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);

        let row0 = buf_row(&buf, area, 0);
        assert!(!row0.contains("Plain promo message"), "row0={row0:?}");
        assert!(row0.ends_with(HIDE_BUTTON), "row0={row0:?}");
        assert_eq!(hits.cta, None, "no usable CTA, no click target");
        assert!(hits.hide.is_some());
    }

    /// Degenerate width: the hide CTA text is skipped whole (redundant with
    /// [hide]) instead of painting a clipped fragment; nothing panics.
    #[test]
    fn render_promo_row_narrow_width_drops_hide_cta_text() {
        let anns = [promo(
            "p",
            "msg body",
            Some(("Go", "https://echo-agent.invalid")),
        )];
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        let hits = render_banner(area, &mut buf, &anns, &no_hidden(), false, false, true);

        let row0 = buf_row(&buf, area, 0);
        assert!(!row0.contains("hide:"), "row0={row0:?}");
        assert!(row0.ends_with(HIDE_BUTTON), "row0={row0:?}");
        assert!(row0.starts_with("[Go]"), "row0={row0:?}");
        assert_eq!(hits.hide, Some(Rect::new(14, 0, 6, 1)));
    }
}
