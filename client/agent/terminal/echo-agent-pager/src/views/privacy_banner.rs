//! Coding-data sharing upsell banner (Figma "Data Sharing Upsell",
//! node 8698:3690). Shared by the welcome tip slot and the agent-view
//! banner slot; visibility is gated by `AppView::privacy_banner_should_show`.

use crate::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// Shares its row with the buttons.
const PRIVACY_BANNER_TITLE: &str = "Help improve EchoAgent";

const PRIVACY_BANNER_DESC: &str = "Off by default. Opt-in to allow EchoAgent to retain coding \
     data, e.g., prompts, traces, & metrics, for training and debugging purposes. Change \
     anytime via settings.";

pub(crate) const PRIVACY_BANNER_TERMS_URL: &str =
    "https://echo-agent.invalid/legal/terms-of-service";
pub(crate) const PRIVACY_BANNER_POLICY_URL: &str =
    "https://echo-agent.invalid/legal/privacy-policy";

/// `(text, url_when_link)`.
type LegalSegment = (&'static str, Option<&'static str>);

/// Widest first; the first that fits *whole* wins. A clipped line would
/// leave hit rects over unreadable link text, and every variant keeps both
/// links so neither document becomes unreachable.
const PRIVACY_BANNER_LEGAL_VARIANTS: [&[LegalSegment]; 3] = [
    &[
        ("Read ", None),
        ("Terms", Some(PRIVACY_BANNER_TERMS_URL)),
        (" and ", None),
        ("Privacy Policy", Some(PRIVACY_BANNER_POLICY_URL)),
        (".", None),
    ],
    &[
        ("Terms", Some(PRIVACY_BANNER_TERMS_URL)),
        (" and ", None),
        ("Privacy Policy", Some(PRIVACY_BANNER_POLICY_URL)),
    ],
    &[
        ("Terms", Some(PRIVACY_BANNER_TERMS_URL)),
        (" & ", None),
        ("Privacy", Some(PRIVACY_BANNER_POLICY_URL)),
    ],
];

const OPT_OUT_LABEL: &str = "[Opt out]";
const OPT_IN_LABEL: &str = "[Opt in]";

/// Title + legal.
const CHROME_ROWS: u16 = 2;

pub(crate) const MIN_HEIGHT: u16 = CHROME_ROWS + 1;

/// Caps banner growth on narrow terminals; overflow is elided with `…` so
/// the disclosure never looks complete when it isn't.
const MAX_BODY_ROWS: usize = 4;

/// Past this, the body abandons the button column for the full slot width:
/// a shorter banner beats a tidy right edge.
const PREFERRED_BODY_ROWS: usize = 3;

pub(crate) struct PrivacyBannerRects {
    pub opt_in: Rect,
    pub opt_out: Rect,
    pub terms: Rect,
    pub policy: Rect,
}

impl PrivacyBannerRects {
    fn none() -> Self {
        Self {
            opt_in: Rect::default(),
            opt_out: Rect::default(),
            terms: Rect::default(),
            policy: Rect::default(),
        }
    }
}

fn button_block_width() -> u16 {
    (OPT_OUT_LABEL.len() + 1 + OPT_IN_LABEL.len()) as u16
}

fn legal_width(variant: &[LegalSegment]) -> u16 {
    variant.iter().map(|(text, _)| text.len() as u16).sum()
}

/// Buttons render whole or not at all, and never at the cost of the title:
/// a clipped/overflowing `[Opt in]` must not leave a click target in the
/// blank margin (a stray click there would silently opt the user in).
fn buttons_fit(area_width: u16) -> bool {
    area_width >= PRIVACY_BANNER_TITLE.len() as u16 + 1 + button_block_width()
}

fn title_width(area_width: u16) -> u16 {
    if buttons_fit(area_width) {
        area_width - button_block_width() - 1
    } else {
        area_width
    }
}

fn wrap_to(width: usize) -> Vec<std::borrow::Cow<'static, str>> {
    if width == 0 {
        return vec![];
    }
    let opts = textwrap::Options::new(width).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit);
    textwrap::wrap(PRIVACY_BANNER_DESC, opts)
}

fn body_lines(area_width: u16) -> Vec<std::borrow::Cow<'static, str>> {
    let column = wrap_to(title_width(area_width) as usize);
    let mut lines = if column.len() <= PREFERRED_BODY_ROWS {
        column
    } else {
        let full = wrap_to(area_width as usize);
        if full.len() < column.len() {
            full
        } else {
            column
        }
    };
    if lines.len() > MAX_BODY_ROWS {
        lines.truncate(MAX_BODY_ROWS);
        if let Some(last) = lines.last_mut() {
            let mut s = last.trim_end().to_string();
            while s.chars().count() + 1 > area_width as usize {
                s.pop();
            }
            s.push('\u{2026}');
            *last = std::borrow::Cow::Owned(s);
        }
    }
    lines
}

/// Rows needed at `width` — the body wraps, so both slot owners must size
/// from this rather than a constant.
pub(crate) fn height(width: u16) -> u16 {
    CHROME_ROWS + (body_lines(width).len() as u16).max(1)
}

/// Needs `area.height >= MIN_HEIGHT`; give it [`height`] rows for the full
/// body.
pub(crate) fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    mouse_pos: Option<(u16, u16)>,
) -> PrivacyBannerRects {
    if area.height < MIN_HEIGHT || area.width == 0 {
        return PrivacyBannerRects::none();
    }

    let hovered = |r: Rect| {
        mouse_pos.is_some_and(|(mx, my)| r.contains(ratatui::layout::Position::new(mx, my)))
    };

    // Figma node 8698:3806.
    buf.set_stringn(
        area.x,
        area.y,
        PRIVACY_BANNER_TITLE,
        title_width(area.width) as usize,
        Style::default().fg(theme.text_primary),
    );

    let body_style = Style::default().fg(theme.gray_bright);
    let body_rows = area.height - CHROME_ROWS;
    let body: Vec<Line> = body_lines(area.width)
        .into_iter()
        .take(body_rows as usize)
        .map(|l| Line::styled(l.into_owned(), body_style))
        .collect();
    Paragraph::new(body).render(
        Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: body_rows,
        },
        buf,
    );

    // Last row, so it gets the full width — no buttons to dodge.
    let gray = Style::default().fg(theme.gray);
    let legal_y = area.y + area.height - 1;
    let mut terms_rect = Rect::default();
    let mut policy_rect = Rect::default();
    if let Some(variant) = PRIVACY_BANNER_LEGAL_VARIANTS
        .into_iter()
        .find(|v| legal_width(v) <= area.width)
    {
        let mut x = area.x;
        let mut spans = Vec::with_capacity(variant.len());
        for (text, url) in variant {
            let w = text.len() as u16;
            let style = match url {
                None => gray,
                Some(url) => {
                    let rect = Rect {
                        x,
                        y: legal_y,
                        width: w,
                        height: 1,
                    };
                    if *url == PRIVACY_BANNER_TERMS_URL {
                        terms_rect = rect;
                    } else {
                        policy_rect = rect;
                    }
                    let fg = if hovered(rect) {
                        theme.gray_bright
                    } else {
                        theme.gray
                    };
                    Style::default().fg(fg).add_modifier(Modifier::UNDERLINED)
                }
            };
            spans.push(Span::styled(*text, style));
            x += w;
        }
        Paragraph::new(Line::from(spans)).render(
            Rect {
                x: area.x,
                y: legal_y,
                width: x - area.x,
                height: 1,
            },
            buf,
        );
    }

    if !buttons_fit(area.width) {
        return PrivacyBannerRects {
            opt_in: Rect::default(),
            opt_out: Rect::default(),
            terms: terms_rect,
            policy: policy_rect,
        };
    }
    let opt_out_rect = Rect {
        x: area.x + area.width - button_block_width(),
        y: area.y,
        width: OPT_OUT_LABEL.len() as u16,
        height: 1,
    };
    let opt_in_rect = Rect {
        x: opt_out_rect.x + opt_out_rect.width + 1,
        y: area.y,
        width: OPT_IN_LABEL.len() as u16,
        height: 1,
    };
    let opt_out_style = if hovered(opt_out_rect) {
        Style::default().fg(theme.text_primary).bg(theme.bg_hover)
    } else {
        Style::default().fg(theme.gray_bright)
    };
    let opt_in_style = if hovered(opt_in_rect) {
        Style::default().fg(theme.link_fg).bg(theme.bg_hover)
    } else {
        Style::default().fg(theme.text_primary)
    };
    buf.set_stringn(
        opt_out_rect.x,
        opt_out_rect.y,
        OPT_OUT_LABEL,
        opt_out_rect.width as usize,
        opt_out_style,
    );
    buf.set_stringn(
        opt_in_rect.x,
        opt_in_rect.y,
        OPT_IN_LABEL,
        opt_in_rect.width as usize,
        opt_in_style,
    );
    PrivacyBannerRects {
        opt_in: opt_in_rect,
        opt_out: opt_out_rect,
        terms: terms_rect,
        policy: policy_rect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render at `width` into a buffer sized by [`height`], returning the
    /// rows (trailing blanks trimmed) and the hit rects.
    fn draw(width: u16) -> (Vec<String>, PrivacyBannerRects) {
        let h = height(width);
        let area = Rect::new(0, 0, width, h);
        let mut buf = Buffer::empty(area);
        let rects = render(area, &mut buf, &Theme::current(), None);
        let rows = (0..h)
            .map(|y| {
                (0..width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        (rows, rects)
    }

    fn rows(width: u16) -> Vec<String> {
        draw(width).0
    }

    /// The text a legal variant reassembles to.
    fn legal_text(variant: &[LegalSegment]) -> String {
        variant.iter().map(|(text, _)| *text).collect()
    }

    /// The buffer text under `rect` on its row.
    fn text_at(rows: &[String], rect: Rect) -> String {
        let row = &rows[rect.y as usize];
        row.chars()
            .skip(rect.x as usize)
            .take(rect.width as usize)
            .collect()
    }

    /// Slot owners reserve [`height`] rows, so the last one it promises must
    /// be the legal line — not a body row pushed off the end.
    #[test]
    fn height_reserves_every_row_the_banner_paints() {
        for width in [200, 117, 110, 100, 80, 72, 60, 45, 40, 36, 30, 24, 18] {
            let rows = rows(width);
            assert_eq!(rows.len(), height(width) as usize);
            assert!(
                rows[0].starts_with(PRIVACY_BANNER_TITLE),
                "width {width}: title must never be clipped, got {:?}",
                rows[0]
            );
            let legal = rows.last().expect("legal row");
            assert!(
                PRIVACY_BANNER_LEGAL_VARIANTS
                    .iter()
                    .any(|v| legal_text(v) == *legal),
                "width {width}: legal line must survive whole, got {legal:?}"
            );
            assert!(
                rows[1..rows.len() - 1].iter().all(|r| !r.is_empty()),
                "width {width}: body rows must not be blank: {rows:?}"
            );
        }
    }

    /// The row cap's elision is a narrow-terminal fallback, not the norm.
    #[test]
    fn body_copy_is_complete_at_common_widths() {
        for width in [200, 117, 100, 80, 60] {
            let body = rows(width)[1..].join(" ");
            let flattened: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                flattened.contains(PRIVACY_BANNER_DESC),
                "width {width}: body copy was truncated: {flattened:?}"
            );
        }
    }

    #[test]
    fn buttons_drop_whole_when_the_row_is_too_narrow() {
        let width = PRIVACY_BANNER_TITLE.len() as u16 + button_block_width(); // one short
        let h = height(width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, h));
        let rects = render(Rect::new(0, 0, width, h), &mut buf, &Theme::current(), None);
        assert_eq!(rects.opt_in, Rect::default());
        assert_eq!(rects.opt_out, Rect::default());
        assert_ne!(rects.terms, Rect::default(), "terms link still clickable");
        assert_ne!(rects.policy, Rect::default(), "policy link still clickable");

        let rects = {
            let width = width + 1;
            let h = height(width);
            let mut buf = Buffer::empty(Rect::new(0, 0, width, h));
            render(Rect::new(0, 0, width, h), &mut buf, &Theme::current(), None)
        };
        assert_eq!(rects.opt_out.width, OPT_OUT_LABEL.len() as u16);
        assert_eq!(rects.opt_in.width, OPT_IN_LABEL.len() as u16);
    }

    #[test]
    fn slot_below_min_height_arms_no_hit_rects() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, MIN_HEIGHT));
        let rects = render(
            Rect::new(0, 0, 100, MIN_HEIGHT - 1),
            &mut buf,
            &Theme::current(),
            None,
        );
        assert_eq!(rects.opt_in, Rect::default());
        assert_eq!(rects.opt_out, Rect::default());
        assert_eq!(rects.terms, Rect::default());
        assert_eq!(rects.policy, Rect::default());
    }

    /// The two links open different documents, so an off-by-one rect sends
    /// the user to the wrong page.
    #[test]
    fn each_legal_link_hits_its_own_words() {
        for width in [200, 117, 80, 60, 40, 30, 24, 18] {
            let (rows, rects) = draw(width);
            assert_eq!(
                text_at(&rows, rects.terms),
                "Terms",
                "width {width}: terms rect is off its word: {rows:?}"
            );
            let policy = text_at(&rows, rects.policy);
            assert!(
                policy == "Privacy Policy" || policy == "Privacy",
                "width {width}: policy rect is off its word, got {policy:?}"
            );
            assert!(
                rects.terms.right() <= rects.policy.x,
                "width {width}: link rects must not overlap"
            );
        }
    }
}
