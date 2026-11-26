//! Hero box component — side-by-side logo + menu inside a bordered box.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::theme::Theme;

use super::WelcomeLayout;

/// Minimum terminal width for the side-by-side hero box layout.
pub(super) const HERO_BOX_MIN_WIDTH: u16 = 90;

/// Vertical padding (rows) between the box border and its inner content.
const V_PAD: u16 = 1;

/// Horizontal inset (cols) between the right column's content and the box
/// border; also the collapsed left-column width when the logo is hidden.
const H_INSET: u16 = 2;

/// Horizontal gap (cols) between the logo and the right column inside the box.
const LOGO_H_PAD: u16 = 3;

/// Rows the promo upgrade CTA reserves in the info slot: a spacer row above the
/// `[label]` button row. Reserved on top of the announcement text rows so the
/// message never paints over the button.
const UPGRADE_CTA_ROWS: u16 = 2;

const HERO_SUBTITLE: &str = "Thanks for trying EchoAgent Build, give feedback with /feedback!";

use super::{PROMPT_HEIGHT, VERSION_GAP};

/// Rows the "thanks" subtitle occupies. Hidden when the in-box info slot
/// (changelog / announcement) is shown, to keep the box compact.
fn subtitle_rows(info_height: u16) -> u16 {
    if info_height > 0 { 0 } else { 1 }
}

/// Height of the hero box's right column: version + optional subtitle +
/// optional info block + the gap before the menu + the menu itself.
fn right_col_height(menu_height: u16, info_height: u16) -> u16 {
    let info_gap = if info_height > 0 { 1u16 } else { 0 };
    // version(1) + subtitle + [info_gap + info] + gap-before-menu(1) + menu
    1 + subtitle_rows(info_height) + info_gap + info_height + 1 + menu_height
}

/// Minimum content-area height the hero box needs to render without truncating:
/// the optional error row, the box, a one-row flex gap, and the fixed rows
/// below (tip + prompt + version). The box always shows the full-height logo,
/// so a terminal shorter than this falls back to the stacked layout instead of
/// overflowing.
pub(super) fn min_content_height(
    error_height: u16,
    menu_height: u16,
    tip_height: u16,
    info_height: u16,
) -> u16 {
    let inner = super::logo::full_logo_line_count().max(right_col_height(menu_height, info_height));
    let hero_box_height = 2 + V_PAD * 2 + inner;
    let gap_after_error = if error_height > 0 { 1u16 } else { 0 };
    gap_after_error + error_height + hero_box_height + 1 + WelcomeLayout::fixed_below(tip_height)
}

/// Largest in-box info-slot height ≤ `desired` for which the hero box still
/// fits in `content_height`. Lets the expanded announcement grow without ever
/// pushing the box past the fit gate; the renderer trails a `…` for whatever
/// tail still doesn't fit (graceful fallback, never an overflow).
pub(super) fn clamp_info_height(
    desired: u16,
    content_height: u16,
    error_height: u16,
    menu_height: u16,
    tip_height: u16,
) -> u16 {
    (0..=desired)
        .rev()
        .find(|&h| content_height >= min_content_height(error_height, menu_height, tip_height, h))
        .unwrap_or(0)
}

/// Width (cols) of the hero box's left (logo) column, including padding.
/// Collapses to a small inset when the logo is hidden.
fn left_col_width() -> u16 {
    let logo_width = super::logo::full_logo_visual_width();
    if logo_width == 0 {
        H_INSET
    } else {
        logo_width + LOGO_H_PAD.saturating_sub(1) + LOGO_H_PAD
    }
}

/// Compute the hero box layout: bordered box with logo left, version + menu right.
///
/// Sizes the in-box info slot here (the announcement clamped to fit, else the
/// fixed `changelog_height`) so the renderer just draws into `hero_info`.
#[allow(clippy::too_many_arguments)]
pub(super) fn compute_hero_box(
    content_area: Rect,
    error_height: u16,
    menu_height: u16,
    tip_height: u16,
    changelog_height: u16,
    announcement: Option<&echo_agent_announcements::RemoteAnnouncement>,
    expanded: bool,
    has_upgrade_cta: bool,
) -> WelcomeLayout {
    let zero = Rect::default();
    let tip_gap = if tip_height > 0 { 1u16 } else { 0 };
    let fixed_below = WelcomeLayout::fixed_below(tip_height);

    // Column widths are height-independent, so derive them once and reuse for
    // both the measurement and the rects: `hero_info.width == info_slot_width`,
    // i.e. measured == drawn.
    let box_width = content_area.width.saturating_sub(6).min(120);
    let inner_width = box_width.saturating_sub(2);
    let left_col_width = left_col_width();
    let right_width = inner_width.saturating_sub(left_col_width);
    let info_slot_width = right_width.saturating_sub(H_INSET);
    let info_height = match announcement {
        Some(ann) => clamp_info_height(
            announcement_desired_rows(ann, info_slot_width, expanded, has_upgrade_cta),
            content_area.height,
            error_height,
            menu_height,
            tip_height,
        ),
        None => changelog_height,
    };

    let logo_rows = super::logo::full_logo_line_count();
    let info_gap = if info_height > 0 { 1u16 } else { 0 };
    let inner_height = logo_rows.max(right_col_height(menu_height, info_height));
    let hero_box_height = 2 + V_PAD * 2 + inner_height;

    let gap_after_error = if error_height > 0 { 1 } else { 0 };
    let fixed_above = gap_after_error + error_height;

    // Top padding for vertical centering (use the default menu height so the
    // logo position stays constant regardless of picker/focus state).
    let default_menu_height = 4u16;
    let default_inner = logo_rows.max(right_col_height(default_menu_height, info_height));
    let default_hero = 2 + V_PAD * 2 + default_inner;
    let remaining = content_area.height.saturating_sub(fixed_above);
    let top_pad = remaining
        .saturating_sub(default_hero)
        .saturating_sub(fixed_below)
        / 3;
    // Centering derives top_pad from the default-menu box, but the fit gate
    // (min_content_height) sizes for the actual box with no pad. Clamp to the
    // real slack so a taller-than-default menu can't push the rows below the
    // box off the bottom at the tight boundary.
    let top_pad = top_pad.min(
        content_area
            .height
            .saturating_sub(fixed_above + hero_box_height + 1 + fixed_below),
    );

    let [
        _,
        _,
        error,
        hero_box_slot,
        _,
        tip,
        _,
        prompt,
        _,
        version_slot,
    ] = Layout::vertical([
        Constraint::Length(top_pad),
        Constraint::Length(gap_after_error),
        Constraint::Length(error_height),
        Constraint::Length(hero_box_height),
        Constraint::Min(1), // flex gap
        Constraint::Length(tip_height),
        Constraint::Length(tip_gap),
        Constraint::Length(PROMPT_HEIGHT),
        Constraint::Length(VERSION_GAP),
        Constraint::Length(1),
    ])
    .areas(content_area);

    // Horizontally center the hero box (`box_width` derived above).
    let [_, hero_box, _] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(box_width),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(hero_box_slot);

    // Inner area inside the border + v_pad. Widths reuse the values above; only
    // x/y come from the laid-out box.
    let inner = Rect {
        x: hero_box.x + 1,
        y: hero_box.y + 1 + V_PAD,
        width: inner_width,
        height: inner_height,
    };

    // Left column: balanced padding around the logo; collapses to a small
    // inset when the logo is hidden.
    let logo_width = super::logo::full_logo_visual_width();
    // Logo body leans right; shave a column off the left pad to optically center.
    let logo_left_pad = LOGO_H_PAD.saturating_sub(1);

    // Logo top-aligned, horizontally centered within left column.
    let hero_logo = Rect {
        x: inner.x + logo_left_pad,
        y: inner.y,
        width: logo_width.min(inner.width.saturating_sub(logo_left_pad)),
        height: logo_rows.min(inner.height),
    };

    // Right column: rest of inner width after left column.
    let right_x = inner.x + left_col_width;

    // Version line at top of right column.
    let hero_version = Rect {
        x: right_x,
        y: inner.y,
        width: right_width,
        height: 1,
    };

    // Subtitle line below version — hidden when the info slot is shown.
    let hero_subtitle = if subtitle_rows(info_height) > 0 {
        Rect {
            x: right_x,
            y: inner.y + 1,
            width: right_width,
            height: 1,
        }
    } else {
        zero
    };

    // Info block (announcement or changelog) below version + optional subtitle.
    let info_y = inner.y + 1 + subtitle_rows(info_height) + info_gap;
    let hero_info = if info_height > 0 {
        Rect {
            x: right_x,
            y: info_y,
            width: info_slot_width,
            height: info_height,
        }
    } else {
        zero
    };

    // version + subtitle + info_gap + info + gap-before-menu
    let right_header_rows = 1 + subtitle_rows(info_height) + info_gap + info_height + 1;

    // Menu below the header rows, left-aligned in right column.
    let hero_menu = Rect {
        x: right_x,
        y: inner.y + right_header_rows,
        width: info_slot_width,
        height: menu_height.min(inner.height.saturating_sub(right_header_rows)),
    };

    WelcomeLayout {
        logo: zero,
        error,
        menu: zero,
        changelog: zero,
        tip,
        prompt,
        version: version_slot,
        hero_box,
        hero_logo,
        hero_version,
        hero_subtitle,
        hero_info,
        hero_menu,
    }
}

/// Hit-test rects produced by [`render_hero_box`].
pub(super) struct HeroBoxRects {
    /// Hit-test rect per menu item row (for click/hover).
    pub(super) menu_rects: Vec<Rect>,
    /// Clickable changelog info block, if drawn.
    pub(super) changelog_cta_rect: Option<Rect>,
    /// Whether the announcement overflowed (the "expandable" signal).
    pub(super) announcement_truncated: bool,
    /// Full announcement block area (clickable anywhere to toggle), if shown.
    pub(super) announcement_rect: Option<Rect>,
    /// Promo upgrade CTA `[label]` button rect (click → open), if drawn.
    pub(super) upgrade_cta_rect: Option<Rect>,
    #[cfg(feature = "local-workspace")]
    pub(super) workspace_mode_rects: super::WorkspaceModeHitRects,
}

/// Render the bordered hero box with logo left, version + subtitle + menu right.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_hero_box(
    layout: &WelcomeLayout,
    buf: &mut Buffer,
    theme: &Theme,
    menu_items: &[(&str, &str)],
    selected: Option<usize>,
    mouse_pos: Option<(u16, u16)>,
    announcement: Option<&echo_agent_announcements::RemoteAnnouncement>,
    announcement_expanded: bool,
    changelog_bullets: &[String],
    changelog_has_full_notes: bool,
    upgrade_cta: Option<&str>,
    #[cfg(feature = "local-workspace")] workspace_mode: Option<(
        super::WelcomeWorkspaceMode,
        bool,
        bool,
    )>,
) -> HeroBoxRects {
    // Dim the box border toward the background for a softer, dimmer gray.
    let border_color = crate::render::color::blend_color(theme.bg_base, theme.gray_dim, 0.45)
        .unwrap_or(theme.gray_dim);
    let border_block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));
    border_block.render(layout.hero_box, buf);

    super::logo::render_full_logo(layout.hero_logo, buf, theme);

    super::render_version_badge(
        layout.hero_version,
        buf,
        theme,
        None,
        0,
        false,
        super::VersionBadgeMode::HeroInline,
    );

    // Subtitle line below the version.
    if layout.hero_subtitle.height > 0 {
        let subtitle_style = Style::default().fg(theme.gray);
        buf.set_span(
            layout.hero_subtitle.x,
            layout.hero_subtitle.y,
            &Span::styled(HERO_SUBTITLE, subtitle_style),
            layout.hero_subtitle.width,
        );
    }

    // In-box info slot: the announcement takes priority over the changelog,
    // and only one is ever shown — always in this same position.
    let mut changelog_cta_rect = None;
    let mut announcement_truncated = false;
    let mut announcement_rect = None;
    let mut upgrade_cta_rect = None;
    if layout.hero_info.height > 0 {
        if let Some(ann) = announcement {
            let (text_area, truncated, cta_rect) = render_announcement_with_upgrade_cta(
                buf,
                theme,
                layout.hero_info,
                ann,
                announcement_expanded,
                mouse_pos,
                upgrade_cta,
            );
            announcement_rect = Some(text_area);
            announcement_truncated = truncated;
            upgrade_cta_rect = cta_rect;
        } else if !changelog_bullets.is_empty() {
            changelog_cta_rect = render_hero_changelog(
                buf,
                theme,
                layout.hero_info,
                changelog_bullets,
                changelog_has_full_notes,
                mouse_pos,
            );
        }
    }

    #[cfg(feature = "local-workspace")]
    let (menu_area, workspace_mode_rects) =
        if let Some((mode, locked, ack_pending)) = workspace_mode {
            let picker_rect = Rect {
                height: 1.min(layout.hero_menu.height),
                ..layout.hero_menu
            };
            let rects = super::render_workspace_mode_picker(
                picker_rect,
                buf,
                theme,
                mode,
                mouse_pos,
                locked,
                ack_pending,
            );
            let menu_area = Rect {
                y: layout.hero_menu.y + super::workspace_mode::WORKSPACE_MODE_MENU_ROWS,
                height: layout
                    .hero_menu
                    .height
                    .saturating_sub(super::workspace_mode::WORKSPACE_MODE_MENU_ROWS),
                ..layout.hero_menu
            };
            (menu_area, rects)
        } else {
            (layout.hero_menu, super::WorkspaceModeHitRects::default())
        };
    #[cfg(not(feature = "local-workspace"))]
    let menu_area = layout.hero_menu;

    let menu_rects = super::menu::render_menu(
        menu_area,
        buf,
        theme,
        menu_items,
        selected,
        mouse_pos,
        menu_area.width,
    );
    HeroBoxRects {
        menu_rects,
        changelog_cta_rect,
        announcement_truncated,
        announcement_rect,
        upgrade_cta_rect,
        #[cfg(feature = "local-workspace")]
        workspace_mode_rects,
    }
}

/// Draw the announcement text + (optional) upgrade CTA into `area`, reserving
/// the CTA rows at the bottom so a long/expanded message never overpaints the
/// button; the button is placed right after the drawn text + a spacer row.
/// Shared by the hero box and the stacked layout. Returns `(text_area,
/// truncated, upgrade_cta_rect)`.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_announcement_with_upgrade_cta(
    buf: &mut Buffer,
    theme: &Theme,
    area: Rect,
    ann: &echo_agent_announcements::RemoteAnnouncement,
    expanded: bool,
    mouse_pos: Option<(u16, u16)>,
    upgrade_cta: Option<&str>,
) -> (Rect, bool, Option<Rect>) {
    let cta_rows = if upgrade_cta.is_some() {
        UPGRADE_CTA_ROWS
    } else {
        0
    };
    let text_area = Rect {
        height: area.height.saturating_sub(cta_rows),
        ..area
    };
    let truncated = render_announcement_block(buf, theme, text_area, ann, expanded, mouse_pos);
    let mut cta_rect = None;
    if let Some(label) = upgrade_cta {
        use unicode_width::UnicodeWidthStr;
        let text_rows =
            announcement_text_rows(ann, text_area.width, expanded).min(text_area.height);
        let cta_y = area.y + text_rows + 1;
        if cta_y < area.y + area.height {
            // Hover follows the button cells (mouse-pos driven, like the sibling
            // info blocks); the shared painter owns the styling + truncation.
            let btn_w =
                UnicodeWidthStr::width(format!("[{label}]").as_str()).min(area.width as usize);
            let hovered = mouse_pos.is_some_and(|(mx, my)| {
                my == cta_y && mx >= area.x && (mx as usize) < area.x as usize + btn_w
            });
            // Pinned (non-dismissible) promo shows its dim `cta.caption`; a
            // dismissible one stays bare. No permission prompt on the welcome
            // screen, so no gating; the painter drops it whole if too narrow.
            let caption = (!crate::views::announcements::is_dismissible(ann))
                .then(|| crate::views::announcements::usable_cta_caption(ann))
                .flatten();
            cta_rect = crate::views::announcements::render_cta_button(
                buf, theme, area.x, cta_y, area.width, label, caption, hovered,
            );
        }
    }
    (text_area, truncated, cta_rect)
}

/// Render the announcement (title + message) into `area`, used by both welcome
/// layouts. Collapsed wraps to 2 lines + a `…`; expanded shows what fits; the
/// block brightens while hovered, but only when it's interactive (overflowing
/// or already expanded). Returns whether the message was truncated (the
/// "expandable" signal).
pub(super) fn render_announcement_block(
    buf: &mut Buffer,
    theme: &Theme,
    area: Rect,
    ann: &echo_agent_announcements::RemoteAnnouncement,
    expanded: bool,
    mouse_pos: Option<(u16, u16)>,
) -> bool {
    let over = mouse_pos.is_some_and(|(mx, my)| area.contains(Position::new(mx, my)));
    let mut row = area.y;
    let max_w = area.width as usize;
    if let Some(title) = ann.title.as_deref() {
        let title_color = match ann.severity.as_deref() {
            Some("critical") => theme.accent_error,
            _ => theme.warning,
        };
        let title_style = Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD);
        let display = crate::render::line_utils::truncate_str(title, max_w);
        buf.set_span(area.x, row, &Span::styled(display, title_style), area.width);
        row += 1;
    }
    if let Some(msg) = ann.message.as_deref() {
        let remaining_rows = (area.y + area.height).saturating_sub(row) as usize;
        let max_lines = if expanded {
            remaining_rows
        } else {
            remaining_rows.min(2)
        };
        // Only brighten when there's something to toggle (an overflowing message
        // or the already-expanded state); a short message that fits isn't
        // clickable, so it must not look interactive.
        let interactive = expanded || wrapped_line_count(msg, area.width) as usize > max_lines;
        let hovered = over && interactive;
        let msg_style = super::hover_style(theme, hovered, Style::default().fg(theme.gray));
        // Dim `…` affordance unless hovered.
        let ell_style = super::hover_style(
            theme,
            hovered,
            Style::default()
                .fg(theme.gray_bright)
                .add_modifier(Modifier::DIM),
        );
        return render_wrapped_text(
            buf, area.x, row, area.width, msg, msg_style, ell_style, max_lines,
        );
    }
    false
}

/// Render the changelog block (header + bullets) in the info slot. When
/// `clickable` (full notes exist), the whole block opens the notes on click and
/// brightens while hovered; returns that clickable rect.
fn render_hero_changelog(
    buf: &mut Buffer,
    theme: &Theme,
    area: Rect,
    bullets: &[String],
    clickable: bool,
    mouse_pos: Option<(u16, u16)>,
) -> Option<Rect> {
    if area.width == 0 || area.height == 0 {
        return None;
    }

    let hovered =
        clickable && mouse_pos.is_some_and(|(mx, my)| area.contains(Position::new(mx, my)));

    let header_style = super::hover_style(
        theme,
        hovered,
        Style::default()
            .fg(theme.gray_bright)
            .add_modifier(Modifier::DIM),
    );
    let title = "Changelog";
    buf.set_span(
        area.x,
        area.y,
        &Span::styled(title, header_style),
        area.width,
    );

    // Bullets start 2 rows down (header + blank), matching the height budget.
    let bullet_style = super::hover_style(theme, hovered, Style::default().fg(theme.gray_bright));
    let max_text_width = area.width.saturating_sub(4) as usize; // " • " prefix + pad
    for (i, bullet) in bullets.iter().enumerate() {
        let row = area.y + 2 + i as u16;
        if row >= area.y + area.height {
            break;
        }
        let truncated = crate::render::line_utils::truncate_str(bullet, max_text_width);
        let text = format!(" \u{2022} {truncated}");
        buf.set_span(area.x, row, &Span::styled(text, bullet_style), area.width);
    }

    clickable.then_some(area)
}

/// Word-wrap `text` into lines no wider than `width` columns. A single word
/// longer than `width` becomes its own (over-wide) line; the renderer clips it.
fn wrap_lines(text: &str, width: u16) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;

    let w = width as usize;
    let mut lines: Vec<String> = Vec::new();
    if w == 0 {
        return lines;
    }
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.width() + 1 + word.width() <= w {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Number of rows `text` occupies when word-wrapped to `width` columns. Shared
/// by the layout height pre-pass and the renderer so they can't drift.
pub(super) fn wrapped_line_count(text: &str, width: u16) -> u16 {
    wrap_lines(text, width).len() as u16
}

/// Rows the announcement TEXT wants at `width`: title + message, the message
/// capped at 2 wrapped lines unless `expanded`. Shared with the renderer so the
/// upgrade CTA is placed right after the drawn text (reserved == drawn).
pub(super) fn announcement_text_rows(
    ann: &echo_agent_announcements::RemoteAnnouncement,
    width: u16,
    expanded: bool,
) -> u16 {
    let title_rows = if ann.title.is_some() { 1u16 } else { 0 };
    let msg_rows = ann.message.as_deref().map_or(0, |msg| {
        let wrapped = wrapped_line_count(msg, width);
        if expanded { wrapped } else { wrapped.min(2) }
    });
    title_rows + msg_rows
}

/// Rows the announcement info slot wants at `width`: the text rows plus, when a
/// promo upgrade CTA is shown, a spacer row + the `[label]` button row
/// (`UPGRADE_CTA_ROWS`). Shared with the renderer (reserved == drawn).
pub(super) fn announcement_desired_rows(
    ann: &echo_agent_announcements::RemoteAnnouncement,
    width: u16,
    expanded: bool,
    has_upgrade_cta: bool,
) -> u16 {
    announcement_text_rows(ann, width, expanded)
        + if has_upgrade_cta { UPGRADE_CTA_ROWS } else { 0 }
}

/// Word-wrap `text` into at most `max_lines` rows at (`x`, `y`). Overflow ends
/// the last row with a `…` painted in `ell_style`. Returns whether the text was
/// truncated.
#[allow(clippy::too_many_arguments)]
fn render_wrapped_text(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    style: Style,
    ell_style: Style,
    max_lines: usize,
) -> bool {
    use unicode_width::UnicodeWidthStr;

    if max_lines == 0 || width == 0 {
        return false;
    }
    let w = width as usize;
    let lines = wrap_lines(text, width);
    let truncated = lines.len() > max_lines;
    let visible = max_lines.min(lines.len());

    for (i, line) in lines.iter().take(visible).enumerate() {
        let row = y + i as u16;
        if i + 1 == visible && truncated {
            // Hard-cut the text and append our own styled `…` (no built-in one).
            let (head, ell_x) = if line.width() < w {
                (line.as_str(), x + line.width() as u16)
            } else {
                let cut =
                    crate::render::line_utils::byte_offset_at_width(line, w.saturating_sub(1));
                (&line[..cut], x + line[..cut].width() as u16)
            };
            buf.set_span(x, row, &Span::styled(head, style), width);
            buf.set_span(ell_x, row, &Span::styled("…", ell_style), 1);
        } else {
            buf.set_span(x, row, &Span::styled(line.as_str(), style), width);
        }
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    fn extract_text(buf: &Buffer, x: u16, y: u16, width: u16) -> String {
        (x..x + width)
            .map(|col| {
                buf.cell((col, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn theme() -> crate::theme::Theme {
        crate::theme::Theme::current()
    }

    /// Distinctive long managed-config message whose tail ("incidents") only
    /// shows when expanded — mirrors the enterprise-policy case from the bug.
    const LONG_MSG: &str = "Enterprise security policy is now in effect for all \
managed devices and accounts. Report security incidents";

    fn ann(
        title: Option<&str>,
        message: Option<&str>,
    ) -> echo_agent_announcements::RemoteAnnouncement {
        echo_agent_announcements::RemoteAnnouncement {
            title: title.map(str::to_string),
            message: message.map(str::to_string),
            ..Default::default()
        }
    }

    fn all_text(buf: &Buffer, area: Rect) -> String {
        (area.y..area.y + area.height)
            .map(|r| extract_text(buf, area.x, r, area.width))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn wrap_short_text_single_line() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 3));
        render_wrapped_text(
            &mut buf,
            0,
            0,
            40,
            "hello world",
            Style::default(),
            Style::default(),
            2,
        );
        assert_eq!(extract_text(&buf, 0, 0, 40), "hello world");
        assert_eq!(extract_text(&buf, 0, 1, 40), "");
    }

    #[test]
    fn wrap_long_text_two_lines() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
        render_wrapped_text(
            &mut buf,
            0,
            0,
            20,
            "one two three four five six",
            Style::default(),
            Style::default(),
            2,
        );
        let line0 = extract_text(&buf, 0, 0, 20);
        let line1 = extract_text(&buf, 0, 1, 20);
        assert!(!line0.is_empty());
        assert!(!line1.is_empty());
        assert_eq!(extract_text(&buf, 0, 2, 20), "");
    }

    #[test]
    fn wrap_empty_text() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        render_wrapped_text(
            &mut buf,
            0,
            0,
            20,
            "",
            Style::default(),
            Style::default(),
            2,
        );
        assert_eq!(extract_text(&buf, 0, 0, 20), "");
    }

    #[test]
    fn wrap_zero_max_lines() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        render_wrapped_text(
            &mut buf,
            0,
            0,
            20,
            "hello",
            Style::default(),
            Style::default(),
            0,
        );
        assert_eq!(extract_text(&buf, 0, 0, 20), "");
    }

    #[test]
    fn wrap_respects_max_lines() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        render_wrapped_text(
            &mut buf,
            0,
            0,
            10,
            "a b c d e f g h i j k l",
            Style::default(),
            Style::default(),
            1,
        );
        assert!(!extract_text(&buf, 0, 0, 10).is_empty());
        assert_eq!(extract_text(&buf, 0, 1, 10), "");
    }

    #[test]
    fn announcement_collapsed_long_shows_two_lines_and_ellipsis() {
        let area = Rect::new(0, 0, 28, 10);
        let mut buf = Buffer::empty(area);
        let a = ann(Some("Heads up"), Some(LONG_MSG));
        let truncated = render_announcement_block(&mut buf, &theme(), area, &a, false, None);
        // Title on row 0, exactly 2 wrapped message rows, then blank.
        assert_eq!(extract_text(&buf, 0, 0, area.width), "Heads up");
        assert!(!extract_text(&buf, 0, 1, area.width).is_empty());
        assert!(!extract_text(&buf, 0, 2, area.width).is_empty());
        assert_eq!(extract_text(&buf, 0, 3, area.width), "");
        // The 2nd message line ends with the `…` affordance, and a hit-rect.
        assert!(extract_text(&buf, 0, 2, area.width).contains('…'));
        assert!(truncated);
        // The tail of the message is hidden while collapsed.
        assert!(!all_text(&buf, area).contains("incidents"));
    }

    #[test]
    fn announcement_short_no_ellipsis_no_rect() {
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        let a = ann(Some("FYI"), Some("All systems normal."));
        let truncated = render_announcement_block(&mut buf, &theme(), area, &a, false, None);
        assert!(!truncated);
        assert!(!all_text(&buf, area).contains('…'));
    }

    #[test]
    fn short_announcement_does_not_brighten_on_hover() {
        // A short message that fits isn't clickable, so hovering must not
        // brighten it (otherwise it looks interactive when it isn't).
        let area = Rect::new(0, 0, 40, 6);
        let theme = theme();
        let a = ann(Some("FYI"), Some("All systems normal."));
        let mut buf = Buffer::empty(area);
        // Mouse over the message row (row 1; the title is row 0).
        let truncated = render_announcement_block(&mut buf, &theme, area, &a, false, Some((1, 1)));
        assert!(!truncated);
        assert_eq!(
            buf.cell((0, 1)).unwrap().fg,
            theme.gray,
            "short announcement must stay dim on hover"
        );
    }

    #[test]
    fn overflowing_announcement_brightens_on_hover() {
        // A collapsible (overflowing) message is interactive, so hovering it
        // brightens the message to the primary color.
        let area = Rect::new(0, 0, 28, 10);
        let theme = theme();
        let a = ann(Some("Heads up"), Some(LONG_MSG));
        let mut buf = Buffer::empty(area);
        let truncated = render_announcement_block(&mut buf, &theme, area, &a, false, Some((1, 1)));
        assert!(truncated);
        assert_eq!(
            buf.cell((0, 1)).unwrap().fg,
            theme.text_primary,
            "overflowing announcement should brighten on hover"
        );
    }

    #[test]
    fn announcement_expanded_shows_full_message() {
        let area = Rect::new(0, 0, 28, 12);
        // Collapsed hides the tail; expanded reveals it.
        let mut collapsed = Buffer::empty(area);
        let a = ann(Some("Heads up"), Some(LONG_MSG));
        render_announcement_block(&mut collapsed, &theme(), area, &a, false, None);
        assert!(!all_text(&collapsed, area).contains("incidents"));

        let mut expanded = Buffer::empty(area);
        let truncated = render_announcement_block(&mut expanded, &theme(), area, &a, true, None);
        assert!(all_text(&expanded, area).contains("incidents"));
        // Fully shown → nothing truncated, so no `…` and no hit-rect.
        assert!(!all_text(&expanded, area).contains('…'));
        assert!(!truncated);
    }

    #[test]
    fn announcement_expanded_clamped_keeps_ellipsis() {
        // Too few rows for the full message even when expanded: still graceful
        // (renders what fits + keeps the `…`), never overflows the area.
        let area = Rect::new(0, 0, 28, 4);
        let mut buf = Buffer::empty(area);
        let a = ann(Some("Heads up"), Some(LONG_MSG));
        let truncated = render_announcement_block(&mut buf, &theme(), area, &a, true, None);
        assert!(truncated);
        assert!(all_text(&buf, area).contains('…'));
        // Nothing drawn past the area's last row.
        assert_eq!(extract_text(&buf, 0, area.height, area.width), "");
    }

    /// The upgrade CTA reserves `UPGRADE_CTA_ROWS` on top of the text rows;
    /// `render_announcement_with_upgrade_cta` paints `[label]` below the message
    /// — plus the dim `cta.caption` for a pinned promo that configures one; bare
    /// for a caption-less pinned promo or a dismissible one — and returns the
    /// button rect (button only, caption excluded).
    #[test]
    fn upgrade_cta_reserves_rows_and_returns_button_rect() {
        let area = Rect::new(0, 0, 40, 8);
        let a = ann(None, Some("EchoAgent 4.5 is here. Upgrade now."));
        let text_rows = announcement_text_rows(&a, area.width, false);
        assert_eq!(
            announcement_desired_rows(&a, area.width, false, true),
            text_rows + UPGRADE_CTA_ROWS,
            "a CTA reserves the spacer + button rows"
        );
        assert_eq!(
            announcement_desired_rows(&a, area.width, false, false),
            text_rows
        );

        // Pinned promo with a configured caption: button + dim caption below.
        let mut pinned = ann(None, Some("EchoAgent 4.5 is here. Upgrade now."));
        pinned.dismissible = Some(false);
        pinned.cta = Some(echo_agent_announcements::AnnouncementCta {
            label: Some("Upgrade Account".into()),
            url: Some("https://echo-agent.invalid/echoagent".into()),
            caption: Some("or use Ctrl+O".into()),
        });
        let mut buf = Buffer::empty(area);
        let (text_area, _truncated, cta_rect) = render_announcement_with_upgrade_cta(
            &mut buf,
            &theme(),
            area,
            &pinned,
            false,
            None,
            Some("Upgrade Account"),
        );
        let rect = cta_rect.expect("CTA returns a button rect");
        assert_eq!(
            text_area.height,
            area.height - UPGRADE_CTA_ROWS,
            "text area shrinks by the reserved CTA rows"
        );
        assert!(
            rect.y >= text_area.y + text_rows,
            "button sits below the text"
        );
        assert_eq!(rect.width, 17, "rect is the [Upgrade Account] button only");
        let row = extract_text(&buf, area.x, rect.y, area.width);
        assert_eq!(
            row, "[Upgrade Account] or use Ctrl+O",
            "pinned promo hero shows the configured caption; row={row:?}"
        );

        // Caption-less pinned promo: bare button (nothing hardcoded fills in).
        pinned.cta.as_mut().unwrap().caption = None;
        let mut buf = Buffer::empty(area);
        let (_ta, _t, cta_rect) = render_announcement_with_upgrade_cta(
            &mut buf,
            &theme(),
            area,
            &pinned,
            false,
            None,
            Some("Upgrade Account"),
        );
        let rect = cta_rect.expect("caption-less pinned promo still shows the button");
        let row = extract_text(&buf, area.x, rect.y, area.width);
        assert_eq!(row, "[Upgrade Account]", "absent caption stays bare");

        // Dismissible promo: bare button even with a configured caption.
        let mut dismissible = ann(None, Some("EchoAgent 4.5 is here. Upgrade now."));
        dismissible.cta = Some(echo_agent_announcements::AnnouncementCta {
            label: Some("Upgrade Account".into()),
            url: Some("https://echo-agent.invalid/echoagent".into()),
            caption: Some("or use Ctrl+O".into()),
        });
        let mut buf = Buffer::empty(area);
        let (_ta, _t, cta_rect) = render_announcement_with_upgrade_cta(
            &mut buf,
            &theme(),
            area,
            &dismissible,
            false,
            None,
            Some("Upgrade Account"),
        );
        let rect = cta_rect.expect("dismissible promo still shows the button");
        let row = extract_text(&buf, area.x, rect.y, area.width);
        assert!(row.contains("[Upgrade Account]"), "row={row:?}");
        assert!(
            !row.contains("Ctrl+O"),
            "dismissible hero ignores the configured caption; row={row:?}"
        );

        // No CTA: no rect, full-height text area.
        let mut buf = Buffer::empty(area);
        let (text_area, _t, cta_rect) =
            render_announcement_with_upgrade_cta(&mut buf, &theme(), area, &a, false, None, None);
        assert!(cta_rect.is_none());
        assert_eq!(text_area.height, area.height);
    }
}
