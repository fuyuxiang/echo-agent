//! Rendering for the MCP elicitation card: header, a scrollable body
//! viewport (form fields or the full URL), and action rows pinned at the
//! bottom so Accept/Decline stay reachable however long the body is.

use echo_agent_tools::mcp_elicitation::ElicitFieldKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

use super::state::{
    ElicitationActionFocus, ElicitationFocus, ElicitationStage, ElicitationViewState, FormFieldUi,
    UrlDisplay,
};

const ELICIT_HPAD: u16 = 5;
const MAX_MESSAGE_LINES: u16 = 3;
const MIN_LABEL_VALUE_GAP: usize = 2;
const MAX_VALUE_COL: usize = 36;
/// A value may render a little past the column it starts in before truncating.
const MAX_VALUE_WIDTH: usize = MAX_VALUE_COL + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitHit {
    Field(usize),
    /// One option row of an expanded multi-select field.
    Option {
        field: usize,
        option: usize,
    },
    Accept,
    Decline,
}

/// One row of the scrollable body.
enum BodyRow {
    Text(Line<'static>),
    Field(usize),
    FieldError(usize),
    Option { field: usize, option: usize },
}

fn url_rows(rows: &mut Vec<BodyRow>, display: &UrlDisplay, content_w: usize, theme: &Theme) {
    if let Some(host) = &display.host {
        rows.push(BodyRow::Text(Line::from(vec![
            Span::styled("Host: ", Style::default().fg(theme.gray)),
            Span::styled(
                host.clone(),
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ])));
        if display.punycode_host {
            rows.push(BodyRow::Text(Line::from(vec![Span::styled(
                "Punycode host: check it is the site you expect".to_string(),
                Style::default().fg(theme.accent_error),
            )])));
        }
    }
    // The full URL, wrapped without a cap: consent must show everything the
    // browser would be sent, not a trusted-looking prefix.
    let raw = Line::from(vec![Span::styled(
        display.url.clone(),
        Style::default().fg(theme.accent_user),
    )]);
    for line in crate::render::wrapping::word_wrap_line(&raw, content_w.max(1)) {
        rows.push(BodyRow::Text(owned_line(&line)));
    }
}

/// Detach a wrapped line from its source buffer so it can be stored as a
/// body row.
fn owned_line(line: &Line<'_>) -> Line<'static> {
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| Span::styled(s.content.to_string(), s.style))
        .collect();
    Line::from(spans).style(line.style)
}

/// Build the body rows plus the row index the cursor sits on (form only).
fn build_body_rows(
    state: &ElicitationViewState,
    content_w: usize,
    theme: &Theme,
) -> (Vec<BodyRow>, Option<usize>) {
    let mut rows = Vec::new();
    let mut cursor_row = None;
    match &state.stage {
        ElicitationStage::UrlConsent(consent) => {
            url_rows(&mut rows, &consent.display, content_w, theme);
        }
        ElicitationStage::UrlWaiting(waiting) => {
            url_rows(&mut rows, &waiting.display, content_w, theme);
            rows.push(BodyRow::Text(Line::from(vec![Span::styled(
                "Waiting for the server to confirm…".to_string(),
                Style::default().fg(theme.gray),
            )])));
        }
        ElicitationStage::Form(form) => {
            for (i, field) in form.fields.iter().enumerate() {
                let is_cur = i == form.field_cursor
                    && matches!(
                        state.focus,
                        ElicitationFocus::Fields | ElicitationFocus::Editing
                    );
                if is_cur {
                    cursor_row = Some(rows.len());
                }
                rows.push(BodyRow::Field(i));
                if field.error.is_some() {
                    rows.push(BodyRow::FieldError(i));
                }
                if is_cur && state.focus == ElicitationFocus::Editing && field.is_multi_select() {
                    for option in 0..field.option_count() {
                        if option == field.option_cursor() {
                            cursor_row = Some(rows.len());
                        }
                        rows.push(BodyRow::Option { field: i, option });
                    }
                }
                // Full-value review: a text draft longer than the in-row
                // value column is wrapped in full beneath the focused field,
                // so the complete submitted value is inspectable before
                // Accept.
                if is_cur && field.is_text() && field.draft().width() > MAX_VALUE_WIDTH {
                    let raw = Line::from(vec![Span::styled(
                        format!("      {}", field.draft()),
                        Style::default().fg(theme.gray),
                    )]);
                    for line in crate::render::wrapping::word_wrap_line(&raw, content_w.max(1)) {
                        rows.push(BodyRow::Text(owned_line(&line)));
                    }
                }
            }
        }
    }
    (rows, cursor_row)
}

fn actions(state: &ElicitationViewState) -> Vec<(ElicitHit, char, &'static str)> {
    match &state.stage {
        ElicitationStage::Form(_) => vec![
            (ElicitHit::Accept, 'y', "Accept"),
            (ElicitHit::Decline, 'd', "Decline"),
        ],
        ElicitationStage::UrlConsent(_) => vec![
            (ElicitHit::Accept, 'y', "Open URL"),
            (ElicitHit::Decline, 'd', "Decline"),
        ],
        // The response is already sent: the only local action left is
        // dismissing the waiting chrome ('o' reopens via the shortcut bar).
        ElicitationStage::UrlWaiting(_) => vec![(ElicitHit::Accept, 'y', "Done")],
    }
}

pub fn elicitation_view_height(
    state: &ElicitationViewState,
    screen_h: u16,
    content_w: usize,
) -> u16 {
    let w = content_w.max(1);
    let theme = Theme::default();
    let title_h = wrap_count(&state.title(), w);
    let msg_h = wrap_count(&state.message, w).min(MAX_MESSAGE_LINES);
    let banner_h = u16::from(state.banner_error().is_some());
    let body_total = build_body_rows(state, w, &theme).0.len() as u16;
    let actions_h = actions(state).len() as u16;
    let chrome = 1 + title_h + 1 + msg_h + banner_h + 1 + 1 + actions_h + 1;
    let raw = chrome + body_total;
    // Preferred cap is a third of the screen, but never so small that the
    // pinned action rows squeeze the body out entirely: a clipped body keeps
    // at least three visible rows (the viewport scrolls the rest).
    let min_viable = chrome + body_total.min(3);
    let soft_cap = (screen_h as u32 * 33 / 100).max(8) as u16;
    let hard_cap = (screen_h as u32 * 80 / 100) as u16;
    raw.max(8)
        .min(soft_cap)
        .max(min_viable)
        .min(hard_cap.max(8))
        .min(screen_h)
}

pub fn render_elicitation_view(
    buf: &mut Buffer,
    area: Rect,
    state: &mut ElicitationViewState,
    theme: &Theme,
    focused: bool,
    mut hits: Option<&mut Vec<(ElicitHit, Rect)>>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    buf.set_style(area, Style::default().bg(theme.bg_light));
    let accent_style = Style::default().fg(theme.accent_user);
    for row in area.y..area.y + area.height {
        if let Some(cell) = buf.cell_mut((area.x, row)) {
            cell.set_symbol(crate::glyphs::accent_bar());
            cell.set_style(accent_style);
        }
    }

    let content_x = area.x + 3;
    let content_width = area.width.saturating_sub(ELICIT_HPAD);
    let bottom = area.y + area.height;
    let mut y = area.y.saturating_add(1);

    y = write_wrapped(
        buf,
        content_x,
        y,
        content_width,
        bottom,
        &state.title(),
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
        u16::MAX,
    );
    y = y.saturating_add(1);
    y = write_wrapped(
        buf,
        content_x,
        y,
        content_width,
        bottom,
        &state.message,
        Style::default().fg(theme.gray),
        MAX_MESSAGE_LINES,
    );
    if let Some(err) = state.banner_error() {
        let err = err.to_string();
        y = write_wrapped(
            buf,
            content_x,
            y,
            content_width,
            bottom,
            &err,
            Style::default().fg(theme.accent_error),
            2,
        );
    }
    let above_body_y = y;
    y = y.saturating_add(1);

    // Pin the action rows at the bottom; the body scrolls in between.
    let action_rows = actions(state);
    let actions_h = action_rows.len() as u16;
    // Bottom padding row + action rows + separator row above them.
    let actions_y = bottom.saturating_sub(1).saturating_sub(actions_h).max(y);
    let body_h = actions_y.saturating_sub(1).saturating_sub(y) as usize;

    let (rows, cursor_row) = build_body_rows(state, content_width as usize, theme);
    let total = rows.len();
    let max_scroll = total.saturating_sub(body_h);
    state.scroll = state.scroll.min(max_scroll);
    if body_h > 0
        && let Some(cur) = cursor_row
    {
        // Keep the cursor row — and its error row, when it directly follows —
        // inside the viewport; the cursor itself wins if both cannot fit.
        let mut want_last = cur;
        if matches!(rows.get(cur + 1), Some(BodyRow::FieldError(_))) {
            want_last = cur + 1;
        }
        if want_last >= state.scroll + body_h {
            state.scroll = want_last + 1 - body_h;
        }
        if cur < state.scroll {
            state.scroll = cur;
        }
    }
    let scroll = state.scroll;

    // Clipped-content cues live in the separator rows the layout already has.
    if scroll > 0 {
        paint_more_marker(buf, content_x, above_body_y, content_width, "↑ more", theme);
    }
    if total > scroll + body_h {
        paint_more_marker(
            buf,
            content_x,
            actions_y.saturating_sub(1),
            content_width,
            "↓ more",
            theme,
        );
    }

    let form = state.form();
    let value_col = form
        .map(|f| form_value_column(&f.fields, content_width as usize))
        .unwrap_or(0);
    for (offset, row) in rows.iter().skip(scroll).take(body_h).enumerate() {
        let row_y = y + offset as u16;
        match row {
            BodyRow::Text(line) => {
                buf.set_line(content_x, row_y, line, content_width);
            }
            BodyRow::Field(i) => {
                let Some(field) = form.and_then(|f| f.fields.get(*i)) else {
                    continue;
                };
                let is_cur = cursor_is_on_field(state, *i);
                paint_row(
                    buf,
                    content_x,
                    row_y,
                    content_width,
                    field_row(state, *i, field, is_cur, focused, theme, value_col),
                );
                if let Some(ref mut hits) = hits {
                    hits.push((
                        ElicitHit::Field(*i),
                        Rect::new(content_x, row_y, content_width, 1),
                    ));
                }
            }
            BodyRow::FieldError(i) => {
                let Some(field) = form.and_then(|f| f.fields.get(*i)) else {
                    continue;
                };
                let Some(err) = &field.error else { continue };
                let is_cur = cursor_is_on_field(state, *i);
                paint_row(
                    buf,
                    content_x,
                    row_y,
                    content_width,
                    error_row(err, is_cur && focused, theme),
                );
            }
            BodyRow::Option { field, option } => {
                let Some(field_ui) = form.and_then(|f| f.fields.get(*field)) else {
                    continue;
                };
                paint_row(
                    buf,
                    content_x,
                    row_y,
                    content_width,
                    option_row(field_ui, *option, focused, theme),
                );
                if let Some(ref mut hits) = hits {
                    hits.push((
                        ElicitHit::Option {
                            field: *field,
                            option: *option,
                        },
                        Rect::new(content_x, row_y, content_width, 1),
                    ));
                }
            }
        }
    }

    let mut action_y = actions_y;
    for (hit, shortcut, label) in action_rows {
        if action_y >= bottom {
            break;
        }
        let is_cur = state.focus == ElicitationFocus::Actions
            && match hit {
                ElicitHit::Accept => state.action_focus == ElicitationActionFocus::Accept,
                ElicitHit::Decline => state.action_focus == ElicitationActionFocus::Decline,
                _ => false,
            };
        paint_row(
            buf,
            content_x,
            action_y,
            content_width,
            action_row(shortcut, label, is_cur, focused, theme),
        );
        if let Some(ref mut hits) = hits {
            hits.push((hit, Rect::new(content_x, action_y, content_width, 1)));
        }
        action_y = action_y.saturating_add(1);
    }

    if !focused {
        crate::render::color::blend_area(buf, area, Some((theme.bg_light, 0.66)), None);
    }
}

fn cursor_is_on_field(state: &ElicitationViewState, i: usize) -> bool {
    matches!(
        state.focus,
        ElicitationFocus::Fields | ElicitationFocus::Editing
    ) && state.field_cursor() == i
}

fn paint_more_marker(buf: &mut Buffer, x: u16, y: u16, width: u16, label: &str, theme: &Theme) {
    let line = Line::from(vec![Span::styled(
        label.to_string(),
        Style::default().fg(theme.gray_dim),
    )]);
    buf.set_line(x, y, &line, width);
}

fn wrap_count(text: &str, width: usize) -> u16 {
    if text.is_empty() {
        return 0;
    }
    let line = Line::from(vec![Span::raw(text.to_string())]);
    crate::render::wrapping::word_wrap_line(&line, width.max(1))
        .len()
        .max(1) as u16
}

#[allow(clippy::too_many_arguments)]
fn write_wrapped(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    max_y: u16,
    text: &str,
    style: Style,
    cap: u16,
) -> u16 {
    if text.is_empty() || y >= max_y {
        return y;
    }
    let raw = Line::from(vec![Span::styled(text.to_string(), style)]);
    let wrapped = crate::render::wrapping::word_wrap_line(&raw, width.max(1) as usize);
    let mut cur = y;
    for line in wrapped.into_iter().take(cap as usize) {
        if cur >= max_y {
            break;
        }
        buf.set_line(x, cur, &line, width);
        cur = cur.saturating_add(1);
    }
    cur
}

fn row_bg(is_cur: bool, focused: bool, theme: &Theme) -> ratatui::style::Color {
    if is_cur && focused {
        theme.bg_visual
    } else {
        theme.bg_light
    }
}

fn paint_row(buf: &mut Buffer, x: u16, y: u16, width: u16, line: Line<'_>) {
    let row = Rect {
        x,
        y,
        width,
        height: 1,
    };
    buf.set_style(row, line.style);
    buf.set_line(x, y, &line, width);
}

pub(super) fn form_value_column(fields: &[FormFieldUi], content_w: usize) -> usize {
    let max_left = fields
        .iter()
        .map(|f| {
            let req = if f.spec.required { " (required)" } else { "" };
            2 + f.spec.title.width() + req.width()
        })
        .max()
        .unwrap_or(0);
    // The caps win over the label-derived width: long titles fall back to the
    // per-row 1-space gap in `field_row` instead of blowing out the column.
    (max_left + MIN_LABEL_VALUE_GAP)
        .min(MAX_VALUE_COL)
        .min(content_w.saturating_sub(1))
}

fn multi_select_summary(field: &FormFieldUi) -> String {
    let ElicitFieldKind::MultiSelect { options, .. } = &field.spec.kind else {
        return String::new();
    };
    let labels: Vec<&str> = options
        .iter()
        .enumerate()
        .filter_map(|(i, o)| field.option_selected(i).then_some(o.label.as_str()))
        .collect();
    if labels.is_empty() {
        "(none selected)".into()
    } else {
        labels.join(", ")
    }
}

#[allow(clippy::too_many_arguments)]
fn field_row(
    state: &ElicitationViewState,
    idx: usize,
    field: &FormFieldUi,
    is_cur: bool,
    focused: bool,
    theme: &Theme,
    value_col: usize,
) -> Line<'static> {
    use crate::render::line_utils::truncate_str;

    let bg = row_bg(is_cur, focused, theme);
    let shortcut = crate::views::question_view::option_shortcut_label(idx).unwrap_or(' ');
    let num_style = Style::default().fg(theme.accent_user).bg(bg);
    let label_style =
        Style::default()
            .fg(theme.text_primary)
            .bg(bg)
            .add_modifier(if is_cur && focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
    let req_style = Style::default().fg(theme.gray_dim).bg(bg);
    let value_style = Style::default().fg(theme.text_primary).bg(bg);

    let value = match (&field.value, &field.spec.kind) {
        (super::state::FieldValueUi::Toggle { on }, _) => {
            if *on {
                "[x]".into()
            } else {
                "[ ]".into()
            }
        }
        (
            super::state::FieldValueUi::Choice { index },
            ElicitFieldKind::SingleSelect { options, .. },
        ) => index
            .and_then(|i| options.get(i))
            .map(|o| o.label.clone())
            .unwrap_or_else(|| "(select)".into()),
        (super::state::FieldValueUi::Multi { .. }, _) => multi_select_summary(field),
        (super::state::FieldValueUi::Text { draft }, _) => {
            let mut draft = draft.clone();
            if is_cur && state.focus == ElicitationFocus::Editing {
                draft.push('▌');
            }
            draft
        }
        (_, ElicitFieldKind::Unsupported { reason }) => format!("({reason})"),
        _ => String::new(),
    };

    let prefix = format!("{shortcut} ");
    let req = if field.spec.required {
        " (required)"
    } else {
        ""
    };
    let left_w = prefix.width() + field.spec.title.width() + req.width();
    let value_disp = truncate_str(&value, MAX_VALUE_WIDTH);
    let gap = value_col.saturating_sub(left_w).max(1);

    let mut spans = vec![
        Span::styled(prefix, num_style),
        Span::styled(field.spec.title.clone(), label_style),
    ];
    if field.spec.required {
        spans.push(Span::styled(req.to_string(), req_style));
    }
    spans.push(Span::styled(" ".repeat(gap), Style::default().bg(bg)));
    if !value_disp.is_empty() {
        spans.push(Span::styled(value_disp.to_string(), value_style));
    }
    Line::from(spans).style(Style::default().bg(bg))
}

fn option_row(field: &FormFieldUi, option: usize, focused: bool, theme: &Theme) -> Line<'static> {
    let ElicitFieldKind::MultiSelect { options, .. } = &field.spec.kind else {
        return Line::default();
    };
    let is_cur = option == field.option_cursor();
    let bg = row_bg(is_cur, focused, theme);
    let on = field.option_selected(option);
    let mark = if on { "[x]" } else { "[ ]" };
    let label = options
        .get(option)
        .map(|o| o.label.clone())
        .unwrap_or_default();
    let label_style =
        Style::default()
            .fg(theme.text_primary)
            .bg(bg)
            .add_modifier(if is_cur && focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
    Line::from(vec![
        Span::styled("    ".to_string(), Style::default().bg(bg)),
        Span::styled(format!("{mark} "), Style::default().fg(theme.gray).bg(bg)),
        Span::styled(label, label_style),
    ])
    .style(Style::default().bg(bg))
}

fn error_row(err: &str, on_cursor: bool, theme: &Theme) -> Line<'static> {
    let bg = if on_cursor {
        theme.bg_visual
    } else {
        theme.bg_light
    };
    Line::from(vec![
        Span::styled("      ", Style::default().bg(bg)),
        Span::styled(
            err.to_string(),
            Style::default().fg(theme.accent_error).bg(bg),
        ),
    ])
    .style(Style::default().bg(bg))
}

fn action_row(
    shortcut: char,
    label: &str,
    is_cur: bool,
    focused: bool,
    theme: &Theme,
) -> Line<'static> {
    let bg = row_bg(is_cur, focused, theme);
    let selected = is_cur;
    let marker = if selected {
        format!("({})", crate::glyphs::filled_dot())
    } else {
        "(\u{25cb})".to_string()
    };
    let marker_style = if selected {
        Style::default()
            .fg(theme.text_primary)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.gray).bg(bg)
    };
    let label_style =
        Style::default()
            .fg(theme.text_primary)
            .bg(bg)
            .add_modifier(if is_cur && focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
    Line::from(vec![
        Span::styled(
            format!("{shortcut} "),
            Style::default().fg(theme.accent_user).bg(bg),
        ),
        Span::styled(format!("{marker} "), marker_style),
        Span::styled(label.to_string(), label_style),
    ])
    .style(Style::default().bg(bg))
}
