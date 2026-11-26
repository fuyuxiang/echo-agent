//! Minimal-mode welcome card.
//!
//! Minimal skips the full-screen welcome view entirely, so the start of a
//! session is otherwise invisible — you land straight at the prompt. To make a
//! fresh session obvious (and on `/new` / `Ctrl+N`), this commits a compact,
//! rounded card once into native scrollback: the braille logo, the version, the
//! cwd, the model, and a one-line hint. It mirrors the full-TUI hero box's style
//! (rounded dim border + logo) without its menu/onboarding.
//!
//! It is printed via [`echo_agent_ratatui_inline::Terminal::insert_before`] — the same
//! one-shot mechanism the commit pipeline uses — gated on an `AppView` flag set
//! at session creation, so it prints exactly once per session and re-prints when
//! a new session starts.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use echo_agent_pager::app::PagerTerminal;
use echo_agent_pager::app::app_view::{ActiveView, AppView};
use echo_agent_pager::minimal_api;
use echo_agent_pager::theme::Theme;

/// Commit the welcome card when one is pending (set at session start / `/new`).
///
/// Called at the top of the minimal draw, before `commit_active`, so the card
/// lands above the first conversation block in native scrollback.
pub fn maybe_commit_welcome(app: &mut AppView, terminal: &mut PagerTerminal) {
    if !minimal_api::minimal_welcome_pending(app) {
        return;
    }
    let width = terminal.viewport_area().width;
    // Too narrow to draw a bordered card — leave the flag set and retry next
    // frame (e.g. during an initial 0-width probe).
    if width < 8 {
        return;
    }
    // NB: the pending flag is cleared only after the `insert_before` at the
    // bottom SUCCEEDS — clearing it up front meant a failed insert silently
    // dropped the card forever (bugbot). A failed frame retries next draw.

    // Reset the live viewport to the TOP of the screen and clear what's visible,
    // so the welcome card commits at row 0 and the app "owns" the window. The
    // viewport is not bottom-pinned, so subsequent commits flow downward from
    // here. Pre-existing native scrollback is untouched — scrolling up still
    // shows whatever was there before.
    let live_h = terminal.viewport_area().height;
    terminal.set_viewport_area(ratatui::layout::Rect {
        x: 0,
        y: 0,
        width,
        height: live_h,
    });
    let _ = terminal.clear();

    let theme = Theme::current();
    let version = echo_agent_version::VERSION;
    let (cwd, model) = match &app.active_view {
        ActiveView::Agent(id) => {
            let agent = app.agents.get(id);
            (
                agent
                    .map(|a| a.session.cwd.display().to_string())
                    .unwrap_or_default(),
                agent.and_then(|a| a.session.models.current_model_name()),
            )
        }
        _ => (app.cwd.display().to_string(), None),
    };

    // Info lines below the logo: title + version, cwd, optional model, hint.
    let mut info: Vec<Line<'static>> = Vec::new();
    info.push(Line::from(vec![
        Span::styled(
            "EchoAgent Build",
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  v{version}"), theme.muted()),
    ]));
    if !cwd.is_empty() {
        info.push(Line::from(Span::styled(cwd, theme.muted())));
    }
    if let Some(model) = model {
        info.push(Line::from(Span::styled(
            format!("Model · {model}"),
            theme.muted(),
        )));
    }
    info.push(Line::from(Span::styled("/help for commands", theme.dim())));

    let logo_lines = minimal_api::compact_logo_line_count();
    // logo (+ a blank separator row) when present, then the info lines, wrapped
    // in a border with one row of vertical padding top and bottom.
    let logo_block = if logo_lines > 0 { logo_lines + 1 } else { 0 };
    let height = 2 + 1 + logo_block + info.len() as u16 + 1;

    // RGB themes: blend a soft border. Terminal-native (both Reset): fall
    // through to Reset so the terminal default fg draws the chrome.
    let border_color =
        echo_agent_pager::render::color::blend_color(theme.bg_base, theme.gray_dim, 0.45)
            .unwrap_or(theme.gray_dim);

    let inserted = terminal.insert_before(height, move |buf| {
        let area = buf.area;
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .render(area, buf);

        let inner_x = area.x + 2;
        let inner_w = area.width.saturating_sub(4);
        // Top border + one row of vertical padding.
        let mut y = area.y + 2;

        if logo_lines > 0 {
            let logo_area = ratatui::layout::Rect {
                x: area.x + 1,
                y,
                width: area.width.saturating_sub(2),
                height: logo_lines,
            };
            minimal_api::render_compact_logo(logo_area, buf, &Theme::current());
            y += logo_lines + 1;
        }

        for line in &info {
            buf.set_line(inner_x, y, line, inner_w);
            y += 1;
        }
    });
    if inserted.is_err() {
        // Terminal write failed — keep the flag pending so the card retries on
        // the next frame instead of being dropped forever.
        return;
    }
    minimal_api::set_minimal_welcome_pending(app, false);
    // Trailing gap, matching every committed block, so the first conversation
    // block is separated from the card.
    super::commit::insert_gap(terminal);
}
