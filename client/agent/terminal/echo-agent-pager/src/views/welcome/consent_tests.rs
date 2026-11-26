use super::*;
use crate::app::consent::ConsentSegment;

fn notice() -> ConsentNotice {
    ConsentNotice {
        id: "tos-2026".to_string(),
        version: 2,
        title: "Updated Terms".to_string(),
        segments: vec![
            ConsentSegment::Text("Review the ".to_string()),
            ConsentSegment::Link {
                index: 0,
                label: "Acceptable Use Policy".to_string(),
            },
            ConsentSegment::Text(". Now's the time.".to_string()),
        ],
        links: vec!["https://echo-agent.invalid/legal/aup".to_string()],
        accept_label: "I accept".to_string(),
    }
}

fn render(width: u16, height: u16) -> (Buffer, WelcomeRenderResult) {
    render_with(width, height, &notice(), None, None)
}

fn render_with(
    width: u16,
    height: u16,
    notice: &ConsentNotice,
    hovered_link: Option<usize>,
    pending_hint: Option<crate::views::shortcuts_bar::PendingHint>,
) -> (Buffer, WelcomeRenderResult) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let result = render_consent(
        area,
        &mut buf,
        &Theme::current(),
        notice,
        Some(0),
        hovered_link,
        pending_hint,
        2,
        false,
    );
    (buf, result)
}

/// Wide graphemes occupy two cells and ratatui blanks the second, so the buffer reads back padded.
fn unpadded(row: &str) -> String {
    row.chars().filter(|c| *c != ' ').collect()
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn screen(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn row_containing(buf: &Buffer, needle: &str) -> Option<String> {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .find(|row| row.contains(needle))
}

/// The label underlines as one span, spaces and the key it answers to included, and no whitespace
/// appears between segments.
#[test]
fn a_link_label_paints_exactly_as_authored() {
    let (buf, result) = render(100, 40);
    let (_, rect) = result.consent_link_rects.first().expect("the link painted");

    for x in rect.x..rect.x + rect.width {
        let cell = &buf[(x, rect.y)];
        assert!(
            cell.modifier.contains(Modifier::UNDERLINED),
            "column {x} ({:?}) inside the label is not underlined",
            cell.symbol()
        );
    }

    assert_eq!(rect.width, "Acceptable Use Policy[1]".len() as u16);
    assert!(screen(&buf).contains("Acceptable Use Policy[1]. Now's the time."));
}

/// A trailing space inside a rect would underline past the label and shift the centring.
#[test]
fn a_wrapped_link_gets_one_tight_rect_per_row() {
    let (buf, result) = render(30, 40);

    assert!(result.consent_link_rects.len() > 1, "the link must wrap");

    let rows: std::collections::BTreeSet<u16> = result
        .consent_link_rects
        .iter()
        .map(|(_, rect)| rect.y)
        .collect();
    assert_eq!(rows.len(), result.consent_link_rects.len());

    for (_, rect) in &result.consent_link_rects {
        let last = &buf[(rect.x + rect.width - 1, rect.y)];
        assert_ne!(
            last.symbol(),
            " ",
            "a trailing space must not be underlined"
        );
    }
}

/// Measured in characters, the notice clips to half its columns and still reports itself painted.
#[test]
fn a_wide_character_notice_is_measured_in_columns() {
    let label = "利用規約";
    let tail = "を確認してください。";
    let wide = ConsentNotice {
        segments: vec![
            ConsentSegment::Link {
                index: 0,
                label: label.to_string(),
            },
            ConsentSegment::Text(tail.to_string()),
        ],
        ..notice()
    };

    let (buf, result) = render_with(100, 40, &wide, None, None);
    let (_, rect) = result.consent_link_rects.first().expect("the link painted");

    assert_eq!(result.consent_legibility, Some(ConsentLegibility::Painted));

    let painted = (0..buf.area.height)
        .map(|y| unpadded(&row_text(&buf, y)))
        .find(|row| row.starts_with('利'))
        .expect("the body paints");
    assert_eq!(
        painted,
        format!("{label}[1]{tail}"),
        "the tail must not be clipped",
    );

    assert_eq!(
        rect.width, 11,
        "four wide characters occupy eight columns, plus the three of the key",
    );
}

#[test]
fn a_pending_double_press_replaces_the_version_badge() {
    use crossterm::event::{KeyCode, KeyModifiers};

    let hint = crate::views::shortcuts_bar::PendingHint {
        shortcut: crate::input::key::KeyShortcut::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        label: "quit",
    };

    let (buf, _) = render_with(100, 40, &notice(), None, Some(hint));

    let screen = screen(&buf);
    assert!(screen.contains("press again to quit"), "{screen}");
    assert!(!screen.contains("EchoAgent Build"), "{screen}");
}

/// `Buffer::set_line` ignores a line's alignment, so the centring is done by hand.
#[test]
fn the_title_is_centred_and_stays_inside_the_margin() {
    let title = "Updated Terms";

    let (buf, _) = render(100, 40);

    let row = row_containing(&buf, title).expect("the title paints");
    assert_eq!(
        row.find(title).expect("title column") as u16,
        (buf.area.width - title.len() as u16) / 2,
    );

    let width = 60u16;
    let margin = 2u16;
    let long = ConsentNotice {
        title: "T".repeat(200),
        ..notice()
    };

    let (buf, _) = render_with(width, 40, &long, None, None);

    let painted = row_containing(&buf, "TT").expect("the title paints");
    assert!(painted.ends_with('…'), "{painted:?}");
    assert!(
        painted.trim_start().chars().count() <= (width - margin * 2) as usize,
        "the title must stay inside the margin: {painted:?}",
    );
}

#[test]
fn hovering_a_link_brightens_every_row_it_wraps_onto() {
    let theme = Theme::current();
    let (plain, result) = render(30, 40);
    let (hovered, _) = render_with(30, 40, &notice(), Some(0), None);

    assert!(result.consent_link_rects.len() > 1, "the link must wrap");

    for (_, rect) in &result.consent_link_rects {
        for x in rect.x..rect.x + rect.width {
            assert_eq!(
                plain[(x, rect.y)].fg,
                theme.link_fg,
                "a link looks like one"
            );
            assert!(!plain[(x, rect.y)].modifier.contains(Modifier::BOLD));
            assert!(
                hovered[(x, rect.y)].modifier.contains(Modifier::BOLD),
                "a 16-colour palette gives both link colours the same value, so hover cannot be \
                 colour alone",
            );
        }
    }
}

/// Quit must stay even when the body is illegible: it is the only way out.
#[test]
fn an_unreadable_body_withholds_accept_but_still_offers_quit() {
    let (small, small_result) = render(40, 10);
    let (large, large_result) = render(100, 40);

    assert_eq!(
        small_result.consent_legibility,
        Some(ConsentLegibility::Illegible)
    );
    assert!(
        screen(&small).contains("Window too small"),
        "{}",
        screen(&small)
    );

    assert!(!screen(&small).contains("I accept"), "{}", screen(&small));
    assert_eq!(small_result.menu_rects.len(), 1);
    assert!(
        small_result.consent_link_rects.is_empty(),
        "a body that did not paint offers nothing to click",
    );

    assert!(screen(&large).contains("I accept"));
    assert_eq!(large_result.menu_rects.len(), 2);

    for painted in [screen(&small), screen(&large)] {
        assert!(painted.contains("Quit"), "{painted}");
    }
}

/// The label is remote input, and the key hint paints at the right edge over whatever reaches it.
#[test]
fn a_long_accept_label_cannot_overwrite_its_key_hint() {
    let long = ConsentNotice {
        accept_label: "I have read and accept the updated enterprise terms of service".to_string(),
        ..notice()
    };

    let (buf, result) = render_with(46, 40, &long, None, None);

    let row = result.menu_rects[0];
    let painted = row_text(&buf, row.y);

    assert!(
        painted.ends_with('a'),
        "the key hint must survive: {painted:?}"
    );
    assert!(painted.contains('…'), "the label must be cut: {painted:?}");
}

/// The logo grows with the window, so one height proves nothing about the others.
#[test]
fn the_largest_allowed_body_paints_at_every_height_it_promises() {
    let rows = crate::app::consent::MAX_CONSENT_BODY_ROWS;
    let body = (0..rows)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tallest = ConsentNotice {
        segments: vec![ConsentSegment::Text(body)],
        links: Vec::new(),
        ..notice()
    };

    // 21 content rows is an 80x24 terminal, the smallest we support.
    for content_height in 21..=34 {
        let (buf, result) = render_with(80, content_height, &tallest, None, None);
        let screen = screen(&buf);

        assert_eq!(
            result.consent_legibility,
            Some(ConsentLegibility::Painted),
            "unreadable at {content_height} content rows",
        );
        assert!(
            screen.contains(&format!("line {}", rows - 1)),
            "the last body line is off screen at {content_height} rows:\n{screen}",
        );
        assert!(
            screen.contains("I accept"),
            "the accept row is off screen at {content_height} rows:\n{screen}",
        );

        assert_eq!(
            result.menu_rects.len(),
            2,
            "the menu is clipped at {content_height} rows",
        );
    }
}
