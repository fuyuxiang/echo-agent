use super::*;
use crate::views::dashboard::DashboardRowId;
use crate::views::dashboard::state::DashboardState;

/// Spinner glyph stays stable for `SPINNER_DIVISOR`
/// successive ticks before advancing.
#[test]
fn state_icon_spinner_advances_every_n_ticks() {
    let g0 = state_icon(RowState::Working, 0);
    // Same glyph for divisor-1 more ticks.
    for t in 1..SPINNER_DIVISOR {
        assert_eq!(state_icon(RowState::Working, t), g0);
    }
    let g1 = state_icon(RowState::Working, SPINNER_DIVISOR);
    assert_ne!(g1, g0, "spinner must advance after SPINNER_DIVISOR ticks");
}

/// Every `RowState` variant resolves to a glyph.
#[test]
fn state_icon_one_per_variant() {
    assert!(!state_icon(RowState::Working, 0).is_empty());
    assert!(!state_icon(RowState::NeedsInput, 0).is_empty());
    assert!(!state_icon(RowState::Idle, 0).is_empty());
    assert!(!state_icon(RowState::Completed, 0).is_empty());
    assert!(!state_icon(RowState::Failed, 0).is_empty());
    assert!(!state_icon(RowState::Blocked, 0).is_empty());
}

/// The dispatch dropdown paints upward from the input, so a panel taller than the space above it
/// used to saturate to row 0 and run off the bottom of the buffer.
#[test]
fn slash_dropdown_never_paints_outside_a_short_dashboard() {
    for (top, height) in [(0u16, 24u16)]
        .into_iter()
        .chain((4..=24u16).map(|h| (0, h)))
        // A non-zero `area.y` is the only term in the clamp the rest of the loop never varies.
        .chain((6..=12u16).map(|h| (3, h)))
    {
        let area = Rect::new(0, top, 80, height.saturating_sub(top));
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, height));
        let mut state = DashboardState::new();
        state.dispatch.set_text("/");
        state
            .dispatch
            .refresh_slash(&crate::acp::model_state::ModelState::default());
        assert!(
            !state.dispatch.slash_snapshot().matches.is_empty(),
            "`/` must offer commands, or the geometry below is never exercised"
        );
        let dispatch_rect = Rect::new(0, area.bottom().saturating_sub(2), 80, 1);

        render_slash_dropdown(&mut buf, area, dispatch_rect, &Theme::default(), &mut state);

        // Six rows above the input is well past the 3-row minimum panel, so a `None` here
        // would mean the dropdown stopped rendering instead of clamping.
        if area.height >= 8 {
            assert!(
                state.slash_dropdown_items_area.is_some(),
                "area={area:?}: dropdown must paint when the input has room above it"
            );
        }
        if let Some(items) = state.slash_dropdown_items_area {
            assert!(
                items.height >= 1,
                "area={area:?}: a clamped panel must still hold an item row"
            );
            assert!(
                items.bottom() <= dispatch_rect.y,
                "area={area:?}: items ran into the dispatch input"
            );
            assert!(
                items.y >= area.y && items.bottom() <= area.bottom(),
                "area={area:?}: items ran off the buffer"
            );
        }
    }
}

/// Helper: read buffer row-by-row so multi-cell substring checks
/// see the visible text in left-to-right order.
fn buf_to_text(buf: &Buffer) -> String {
    let mut content = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            content.push_str(buf[(x, y)].symbol());
        }
        content.push('\n');
    }
    content
}

/// edge cases 1+25: empty state with no agents renders
/// the single hint line (never a fully blank screen).
#[test]
fn render_empty_state_paints_hint_line() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let theme = Theme::current();
    render_empty_state(&mut buf, Rect::new(0, 0, 80, 10), &theme, false);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("No agents yet, type a prompt to start one."),
        "expected empty-state hint, got: {content:?}"
    );
}

#[test]
fn render_dashboard_shows_roster_when_local_agents_empty() {
    use crate::app::roster::{RosterActivity, RosterEntry, RosterOrigin};

    let area = Rect::new(0, 0, 100, 24);
    let mut buf = Buffer::empty(area);
    let mut agents: IndexMap<AgentId, AgentView> = IndexMap::new();
    let mut state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    let roster = [RosterEntry {
        session_id: "sess-fleet-1".into(),
        title: Some("Fix fleet dashboard".into()),
        cwd: "/repo/work".into(),
        is_worktree: false,
        model_id: None,
        yolo: false,
        activity: RosterActivity::Working,
        last_turn_summary: None,
        resident: true,
        last_change_unix_ms: 1_725_000_000_000,
        origin: RosterOrigin::default(),
    }];

    let _ = render_dashboard(
        &mut buf,
        area,
        &mut state,
        &mut agents,
        &registry,
        None,
        &roster,
        false,
        None,
    );

    let content = buf_to_text(&buf);
    assert!(
        content.contains("Fix fleet dashboard"),
        "roster-only working session must paint when local agents are empty, got: {content:?}"
    );
    assert!(
        !content.contains("No agents yet"),
        "must not show empty-state while roster rows exist, got: {content:?}"
    );
}

/// Hover `[✗]` paints only on settled rows, never on a busy one.
#[test]
fn render_dashboard_hover_shows_delete_x_only_for_settled_rows() {
    use crate::app::roster::{RosterActivity, RosterEntry, RosterOrigin};

    let ballot = crate::glyphs::ballot_x_button();
    let render_with = |activity: RosterActivity| -> String {
        let area = Rect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);
        let mut agents: IndexMap<AgentId, AgentView> = IndexMap::new();
        let mut state = DashboardState::new();
        let registry = crate::actions::ActionRegistry::defaults();
        let roster = [RosterEntry {
            session_id: "sess-hover".into(),
            title: Some("Hover me".into()),
            cwd: "/repo/work".into(),
            is_worktree: false,
            model_id: None,
            yolo: false,
            activity,
            last_turn_summary: None,
            resident: true,
            last_change_unix_ms: 1_725_000_000_000,
            origin: RosterOrigin::default(),
        }];
        state.hovered_row = Some(DashboardRowId::Roster {
            session_id: "sess-hover".into(),
        });
        let _ = render_dashboard(
            &mut buf,
            area,
            &mut state,
            &mut agents,
            &registry,
            None,
            &roster,
            false,
            None,
        );
        buf_to_text(&buf)
    };

    assert!(
        render_with(RosterActivity::Completed).contains(ballot),
        "hovering a settled (completed) row must show the [✗] delete affordance",
    );
    assert!(
        !render_with(RosterActivity::Working).contains(ballot),
        "hovering a busy (working) row must NOT show the [✗] delete affordance",
    );
}

/// While the local session roster is still loading the empty body
/// shows a loading hint instead of the "no agents" copy.
#[test]
fn render_empty_state_paints_loading_hint() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let theme = Theme::current();
    render_empty_state(&mut buf, Rect::new(0, 0, 80, 10), &theme, true);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Loading sessions"),
        "expected loading hint while sessions load, got: {content:?}"
    );
    assert!(
        !content.contains("No agents yet"),
        "loading state must not show the empty-state copy, got: {content:?}"
    );
}

/// The hint still paints on a 1-row area (the `y_offset` collapses
/// to 0 instead of overflowing the rect).
#[test]
fn render_empty_state_paints_on_single_row_area() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
    let theme = Theme::current();
    render_empty_state(&mut buf, Rect::new(0, 0, 80, 1), &theme, false);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("No agents yet"),
        "expected empty-state hint on 1-row area, got: {content:?}"
    );
}

/// Local-only preview: render a representative dashboard frame
/// to stdout. Run with:
///
/// ```text
/// cargo test -p echo-agent-pager --lib \
///     views::dashboard::render::tests::dashboard_visual_preview -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn dashboard_visual_preview() {
    use std::path::PathBuf;
    use std::time::SystemTime;
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 32));
    let mut state = DashboardState::new();
    state.spinner_tick = 8;
    let theme = Theme::current();
    let now = SystemTime::now();
    let rows = vec![
        DashboardRow {
            id: DashboardRowId::TopLevel(crate::app::agent::AgentId(1)),
            label: "Add responsiveness to /context".to_string(),
            subtitle: Some("echoagent my-branch-2 worktree".to_string()),
            state: RowState::NeedsInput,
            activity: Some("Awaiting your input".to_string()),
            secondary_line: Some("Pending: plan approval plan.md".to_string()),
            cwd_display: String::new(),
            cwd: PathBuf::from("/tmp"),
            last_change_at: now - std::time::Duration::from_secs(240),
            pinned: false,
            is_active: false,
            badges: Vec::new(),
            context_pct: None,
            indent: 0,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        },
        DashboardRow {
            id: DashboardRowId::TopLevel(crate::app::agent::AgentId(2)),
            label: "Add buttons for /models".to_string(),
            subtitle: Some("echoagent my-branch-3 worktree".to_string()),
            state: RowState::Completed,
            activity: None,
            secondary_line: Some("all tests completed, should I push?".to_string()),
            cwd_display: String::new(),
            cwd: PathBuf::from("/tmp"),
            last_change_at: now - std::time::Duration::from_secs(3600),
            pinned: false,
            is_active: false,
            badges: Vec::new(),
            context_pct: None,
            indent: 0,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        },
        DashboardRow {
            id: DashboardRowId::TopLevel(crate::app::agent::AgentId(3)),
            label: "Investigate bug".to_string(),
            subtitle: Some("echoagent main".to_string()),
            state: RowState::Working,
            activity: Some("read somefile.md".to_string()),
            secondary_line: Some("read somefile.md".to_string()),
            cwd_display: String::new(),
            cwd: PathBuf::from("/tmp"),
            last_change_at: now - std::time::Duration::from_secs(5),
            pinned: false,
            is_active: false,
            badges: Vec::new(),
            context_pct: None,
            indent: 0,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        },
        DashboardRow {
            id: DashboardRowId::TopLevel(crate::app::agent::AgentId(4)),
            label: "Add responsiveness to /context".to_string(),
            subtitle: Some("echoagent my-branch-2 worktree".to_string()),
            state: RowState::Working,
            activity: Some("edit somefile.md".to_string()),
            secondary_line: Some("edit somefile.md".to_string()),
            cwd_display: String::new(),
            cwd: PathBuf::from("/tmp"),
            last_change_at: now - std::time::Duration::from_secs(240),
            pinned: false,
            is_active: false,
            badges: Vec::new(),
            context_pct: None,
            indent: 0,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        },
        DashboardRow {
            id: DashboardRowId::TopLevel(crate::app::agent::AgentId(5)),
            label: "Add buttons for /models".to_string(),
            subtitle: Some("echoagent mybranch worktree".to_string()),
            state: RowState::Working,
            activity: Some("thinking about life".to_string()),
            secondary_line: Some("thinking about life".to_string()),
            cwd_display: String::new(),
            cwd: PathBuf::from("/tmp"),
            last_change_at: now - std::time::Duration::from_secs(3600),
            pinned: false,
            is_active: false,
            badges: Vec::new(),
            context_pct: None,
            indent: 0,
            parent_label: None,
            is_more_placeholder: false,
            more_count: 0,
        },
    ];
    // Select the first row (NeedsInput) so the footer flips
    // to the "see details" mode and the dispatch placeholder
    // reads "Reply to agent".
    state.selected = Some(rows[0].id.clone());

    let area = Rect::new(0, 0, 100, 32);
    let layout = super::super::layout::compute_layout(area, false);
    // Manually paint each region so we don't need a live AgentView.
    buf.set_style(area, Style::default().bg(theme.bg_base));
    render_header(&mut buf, layout.header, &theme, &rows, &mut state, None);
    render_rows(&mut buf, layout.list, &theme, &rows, &mut state);
    let _ = render_dispatch(&mut buf, layout.dispatch, &theme, &mut state, None);
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        layout.footer,
        &theme,
        &state,
        &registry,
        Some(RowState::NeedsInput),
        false,
        None,
    );

    println!(
        "\n┌─── Dashboard preview ({}x{}) ───\n│",
        area.width, area.height
    );
    for y in 0..buf.area.height {
        print!("│");
        for x in 0..buf.area.width {
            print!("{}", buf[(x, y)].symbol());
        }
        println!();
    }
    println!("└─── end preview ───\n");
}

/// Local-only preview of the overlay chrome alone. Run with:
///
/// ```text
/// cargo test -p echo-agent-pager --lib \
///     views::dashboard::render::tests::dashboard_overlay_visual_preview \
///     -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn dashboard_overlay_visual_preview() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 12));
    let theme = Theme::current();
    let _ = render_dashboard_session_overlay(
        &mut buf,
        Rect::new(0, 0, 100, 12),
        &theme,
        "Add responsiveness to /context",
        Some((1, 3)),
        false,
        false,
        false,
    );
    println!("\n┌── overlay preview ──");
    for y in 0..buf.area.height {
        print!("│");
        for x in 0..buf.area.width {
            print!("{}", buf[(x, y)].symbol());
        }
        println!();
    }
    println!("└── end ──\n");
}

/// Session overlay paints a full bordered frame with
/// `{title}` on the left of the title row and four
/// affordances on the right: position indicator `{i}/{n}`,
/// previous-row chip `[‹]`, next-row chip `[›]`, and close
/// chip `[Dashboard]`. All four are plain bracketed text on
/// `bg_base` (no button-fill background); hover only changes
/// the foreground color.
#[test]
fn render_dashboard_session_overlay_paints_bordered_frame_chrome() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let theme = Theme::current();
    let chrome = render_dashboard_session_overlay(
        &mut buf,
        Rect::new(0, 0, 80, 10),
        &theme,
        "Add responsiveness to /context",
        Some((1, 2)),
        false,
        false,
        false,
    )
    .expect("overlay must paint on a reasonably sized area");
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Add responsiveness to /context"),
        "title must render, got: {content:?}",
    );
    for chip in ["[‹]", "[›]", "[Dashboard]"] {
        assert!(
            content.contains(chip),
            "overlay must paint `{chip}`, got: {content:?}",
        );
    }
    assert!(
        content.contains("1/2"),
        "overlay must paint the `1/2` position indicator, got: {content:?}",
    );
    // Frame chrome.
    for corner in ['\u{250c}', '\u{2510}', '\u{2514}', '\u{2518}'] {
        assert!(
            content.contains(corner),
            "overlay must paint frame corner `{corner}`, got: {content:?}",
        );
    }
    for tee in ['\u{251c}', '\u{2524}'] {
        assert!(
            content.contains(tee),
            "overlay must paint title-divider T-junction `{tee}`, got: {content:?}",
        );
    }
    assert!(chrome.prev_rect.is_some(), "prev_rect must be populated");
    assert!(chrome.next_rect.is_some(), "next_rect must be populated");
    assert!(chrome.close_rect.is_some(), "close_rect must be populated");
    // The `[‹]` / `[›]` chips are painted as plain text on
    // `bg_base` (matching `[Dashboard]` and every other close
    // affordance in the pager).
    let prev = chrome.prev_rect.unwrap();
    let prev_cell = &buf[(prev.x, prev.y)];
    assert_eq!(
        prev_cell.bg, theme.bg_base,
        "`[‹]` must paint on `bg_base` (plain text like [Dashboard], no button bg), got bg={:?}",
        prev_cell.bg,
    );
    // The `[‹]` and `[›]` chips paint flush against each
    // other (no separating space) so the pair reads as one
    // nav widget. Trailing edge of `[‹]` (prev.x + prev.width)
    // must equal the leading edge of `[›]` (next.x).
    let next = chrome.next_rect.unwrap();
    assert_eq!(
        prev.x + prev.width,
        next.x,
        "`[‹]` and `[›]` must be adjacent (no space between), got prev_end={}, next_start={}",
        prev.x + prev.width,
        next.x,
    );
    // The rendered row should literally contain the adjacent
    // pair `[‹][›]` (no internal whitespace).
    assert!(
        content.contains("[‹][›]"),
        "row must contain `[‹][›]` as a single adjacent group, got: {content:?}",
    );
    let close = chrome.close_rect.unwrap();
    let close_cell = &buf[(close.x, close.y)];
    assert_eq!(
        close_cell.bg, theme.bg_base,
        "close `[Dashboard]` must paint on `bg_base` (NOT a button), got bg={:?}",
        close_cell.bg,
    );
}

/// With `position = None` (overlay not active or single-agent
/// dashboard), the overlay still paints the close button but
/// omits the position indicator and both cycle chips.
#[test]
fn render_dashboard_session_overlay_omits_cycle_chips_when_position_is_none() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let theme = Theme::current();
    let chrome = render_dashboard_session_overlay(
        &mut buf,
        Rect::new(0, 0, 80, 10),
        &theme,
        "Investigate bug",
        None,
        false,
        false,
        false,
    )
    .expect("overlay must paint");
    let content = buf_to_text(&buf);
    assert!(content.contains("[Dashboard]"));
    assert!(!content.contains("[‹]"));
    assert!(!content.contains("[›]"));
    assert!(chrome.prev_rect.is_none());
    assert!(chrome.next_rect.is_none());
    assert!(chrome.close_rect.is_some());
}

/// `position = Some((1, 1))` (the user is the only attachable
/// row) also omits the cycle chips — there's nowhere to walk
/// to, so the chips would be dead clicks.
#[test]
fn render_dashboard_session_overlay_omits_cycle_chips_when_total_is_one() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let theme = Theme::current();
    let chrome = render_dashboard_session_overlay(
        &mut buf,
        Rect::new(0, 0, 80, 10),
        &theme,
        "Solo agent",
        Some((1, 1)),
        false,
        false,
        false,
    )
    .expect("overlay must paint");
    let content = buf_to_text(&buf);
    assert!(!content.contains("[‹]"));
    assert!(!content.contains("[›]"));
    // The position indicator is also suppressed — a `1/1`
    // chip would be visual noise.
    assert!(
        !content.contains("1/1"),
        "single-row overlays must omit the position indicator, got: {content:?}",
    );
    assert!(chrome.prev_rect.is_none());
    assert!(chrome.next_rect.is_none());
}

/// Hover feedback on the plain-text affordances (`[‹]`, `[›]`)
/// only changes the foreground color (to `text_primary` on
/// hover, `gray` otherwise). Background is always `bg_base`
/// (no button fill).
#[test]
fn render_dashboard_session_overlay_highlights_hovered_affordance() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let theme = Theme::current();
    let chrome = render_dashboard_session_overlay(
        &mut buf,
        Rect::new(0, 0, 80, 10),
        &theme,
        "x",
        Some((1, 2)),
        false,
        true, // hover_next
        false,
    )
    .expect("overlay must paint");
    let next = chrome.next_rect.unwrap();
    let next_cell = &buf[(next.x, next.y)];
    assert_eq!(
        next_cell.bg, theme.bg_base,
        "hovered `[›]` must paint on `bg_base` (plain text like [Dashboard]), got bg={:?}",
        next_cell.bg,
    );
    assert_eq!(
        next_cell.fg, theme.text_primary,
        "hovered `[›]` must use text_primary fg, got: {:?}",
        next_cell.fg,
    );

    let prev = chrome.prev_rect.unwrap();
    let prev_cell = &buf[(prev.x, prev.y)];
    assert_eq!(
        prev_cell.bg, theme.bg_base,
        "non-hovered `[‹]` must paint on `bg_base`, got bg={:?}",
        prev_cell.bg,
    );
    assert_eq!(
        prev_cell.fg, theme.gray,
        "non-hovered `[‹]` must use gray fg, got: {:?}",
        prev_cell.fg,
    );
}

/// The `[+ New Agent]` header button paints green (`accent_success`)
/// when focused so the cursor is obvious, and dim gray otherwise.
#[test]
fn header_new_agent_button_focused_is_green() {
    let theme = Theme::current();
    let rows: Vec<DashboardRow> = Vec::new();
    let area = Rect::new(0, 0, 120, 1);

    // Focused (default for a fresh dashboard with no row selected).
    let mut focused = DashboardState::new();
    focused.focus_new_agent_button();
    let mut buf = Buffer::empty(area);
    render_header(&mut buf, area, &theme, &rows, &mut focused, None);
    let rect = focused
        .new_agent_button_hit
        .rect
        .expect("button must render");
    assert_eq!(
        buf[(rect.x, rect.y)].fg,
        theme.accent_success,
        "focused [+ New Agent] must paint green (accent_success), got {:?}",
        buf[(rect.x, rect.y)].fg,
    );

    // Unfocused (a row holds the cursor instead).
    let mut unfocused = DashboardState::new();
    unfocused.focus_row(super::super::state::DashboardRowId::TopLevel(
        crate::app::agent::AgentId(0),
    ));
    let mut buf2 = Buffer::empty(area);
    render_header(&mut buf2, area, &theme, &rows, &mut unfocused, None);
    let rect2 = unfocused
        .new_agent_button_hit
        .rect
        .expect("button must render");
    assert_eq!(
        buf2[(rect2.x, rect2.y)].fg,
        theme.gray,
        "unfocused [+ New Agent] must paint dim gray, got {:?}",
        buf2[(rect2.x, rect2.y)].fg,
    );
}

/// The `[+ New Agent]` header button brightens its text on hover
/// (`gray` → `text_primary`, when not focused) so the mouse user
/// gets clear feedback that it's clickable. Only the foreground
/// changes — the background stays `bg_base` (no fill). Driven by
/// the `hovered` flag the mouse-move handler flips via
/// `HitArea::update_hover`.
#[test]
fn header_new_agent_button_hover_brightens_text() {
    let theme = Theme::current();
    let rows: Vec<DashboardRow> = Vec::new();
    let area = Rect::new(0, 0, 120, 1);

    // Unfocused so the hover styling is isolated from the focus
    // (green) styling.
    let mut state = DashboardState::new();
    state.focus_row(super::super::state::DashboardRowId::TopLevel(
        crate::app::agent::AgentId(0),
    ));

    // First render populates the button's hit rect.
    let mut buf = Buffer::empty(area);
    render_header(&mut buf, area, &theme, &rows, &mut state, None);
    let rect = state.new_agent_button_hit.rect.expect("button must render");

    // Moving the mouse over the button flips hover on.
    assert!(
        state.new_agent_button_hit.update_hover(rect.x, rect.y),
        "moving the mouse over the button must flip hover on",
    );

    // Re-render with hover active → text_primary fg, background
    // unchanged (still bg_base — no fill on hover).
    let mut buf2 = Buffer::empty(area);
    render_header(&mut buf2, area, &theme, &rows, &mut state, None);
    let cell = &buf2[(rect.x, rect.y)];
    assert_eq!(
        cell.fg, theme.text_primary,
        "hovered [+ New Agent] must use text_primary fg, got {:?}",
        cell.fg,
    );
    assert_eq!(
        cell.bg, theme.bg_base,
        "hovered [+ New Agent] must keep bg_base (no hover fill), got {:?}",
        cell.bg,
    );

    // Moving the mouse off the button clears hover → back to the
    // dim resting state (gray fg, bg_base).
    assert!(
        state.new_agent_button_hit.update_hover(0, 0),
        "moving the mouse off the button must flip hover off",
    );
    let mut buf3 = Buffer::empty(area);
    render_header(&mut buf3, area, &theme, &rows, &mut state, None);
    let cell3 = &buf3[(rect.x, rect.y)];
    assert_eq!(
        cell3.bg, theme.bg_base,
        "non-hovered [+ New Agent] must paint on bg_base, got {:?}",
        cell3.bg,
    );
    assert_eq!(
        cell3.fg, theme.gray,
        "non-hovered [+ New Agent] must use gray fg, got {:?}",
        cell3.fg,
    );
}

/// Regression: the header renders from the dashboard's STAGED `cwd`
/// (synced from `app.cwd` on a `/cd`), not the live process cwd. A
/// location change updates `state.cwd` immediately while the process cwd
/// only moves later via `Effect::SetWorkingDir` (which can fail), so the
/// header must follow `state.cwd` to show where dispatches will run.
#[test]
fn header_location_renders_from_staged_cwd() {
    let theme = Theme::current();
    let rows: Vec<DashboardRow> = Vec::new();
    // Wide area so the path isn't width-truncated.
    let area = Rect::new(0, 0, 200, 1);

    let mut state = DashboardState::new();
    // A distinct absolute path outside $HOME (rendered verbatim) that
    // differs from the process cwd. No git cache entry → no branch span.
    state.cwd = std::path::PathBuf::from("/echo-agent-staged-cwd-marker");

    let mut buf = Buffer::empty(area);
    render_header(&mut buf, area, &theme, &rows, &mut state, None);

    let top_row: String = (0..area.width)
        .map(|x| buf[(x, 0)].symbol().to_string())
        .collect();
    assert!(
        top_row.contains("/echo-agent-staged-cwd-marker"),
        "header must render the staged cwd, not the process cwd; got: {top_row:?}",
    );
}

/// The header button reads `[+ New Worktree]` when worktree mode is armed
/// in a git repo, and `[+ New Agent]` otherwise (off, or armed but not a
/// git repo — worktree mode can't take effect there).
#[test]
fn header_button_label_reflects_worktree_mode() {
    let theme = Theme::current();
    let rows: Vec<DashboardRow> = Vec::new();
    let area = Rect::new(0, 0, 120, 1);

    // Off → plain new-agent button.
    let mut off = DashboardState::new();
    off.cwd_has_git_ancestor = true;
    let mut buf = Buffer::empty(area);
    render_header(&mut buf, area, &theme, &rows, &mut off, None);
    let text = buf_to_text(&buf);
    assert!(
        text.contains("[+ New Agent]") && !text.contains("Worktree"),
        "worktree mode off → [+ New Agent], got: {text:?}",
    );

    // Armed in a git repo → worktree button.
    let mut armed = DashboardState::new();
    armed.cwd_has_git_ancestor = true;
    armed.dispatch_worktree = true;
    let mut buf2 = Buffer::empty(area);
    render_header(&mut buf2, area, &theme, &rows, &mut armed, None);
    let text2 = buf_to_text(&buf2);
    assert!(
        text2.contains("[+ New Worktree]"),
        "worktree mode armed in a repo → [+ New Worktree], got: {text2:?}",
    );

    // Armed but NOT a git repo → still the plain button (mode is inert).
    let mut armed_no_git = DashboardState::new();
    armed_no_git.cwd_has_git_ancestor = false;
    armed_no_git.dispatch_worktree = true;
    let mut buf3 = Buffer::empty(area);
    render_header(&mut buf3, area, &theme, &rows, &mut armed_no_git, None);
    let text3 = buf_to_text(&buf3);
    assert!(
        text3.contains("[+ New Agent]") && !text3.contains("Worktree"),
        "armed outside a repo → [+ New Agent], got: {text3:?}",
    );
}

/// Tiny areas return `None` so the caller falls back to a
/// chromeless render.
#[test]
fn render_dashboard_session_overlay_returns_none_on_tiny_area() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 12, 3));
    let theme = Theme::current();
    let chrome = render_dashboard_session_overlay(
        &mut buf,
        Rect::new(0, 0, 12, 3),
        &theme,
        "x",
        Some((1, 2)),
        false,
        false,
        false,
    );
    assert!(chrome.is_none());
}

/// The chromeless header variant paints the title + chips on the
/// title row, applies the requested top / side padding so it
/// aligns with the body below, populates the affordance hit rects,
/// hands back a full-width `content` rect beneath the header band,
/// and crucially paints NO border frame.
#[test]
fn render_dashboard_session_header_paints_padded_top_bar_without_border() {
    const PAD_LEFT: u16 = 2;
    const PAD_RIGHT: u16 = 2;
    const PAD_TOP: u16 = 1;
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let theme = Theme::current();
    let chrome = render_dashboard_session_header(
        &mut buf,
        Rect::new(0, 0, 80, 10),
        &theme,
        "Add responsiveness to /context",
        Some((1, 2)),
        false,
        false,
        false,
        PAD_LEFT,
        PAD_RIGHT,
        PAD_TOP,
    )
    .expect("header must paint on a reasonably sized area");
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Add responsiveness to /context"),
        "title must render, got: {content:?}",
    );
    for chip in ["[‹]", "[›]", "[Dashboard]"] {
        assert!(
            content.contains(chip),
            "header must paint `{chip}`, got: {content:?}",
        );
    }
    assert!(
        content.contains("1/2"),
        "header must paint the `1/2` position indicator, got: {content:?}",
    );
    // No bordered frame: none of the box-drawing glyphs the
    // bordered overlay paints should appear.
    for glyph in [
        '\u{250c}', '\u{2510}', '\u{2514}', '\u{2518}', '\u{251c}', '\u{2524}', '\u{2502}',
    ] {
        assert!(
            !content.contains(glyph),
            "header must NOT paint frame glyph `{glyph}`, got: {content:?}",
        );
    }
    // Content is the full-width area below the header band
    // (`PAD_TOP` blank rows + 1 title row).
    assert_eq!(
        chrome.content,
        Rect::new(0, PAD_TOP + 1, 80, 10 - (PAD_TOP + 1))
    );
    assert!(chrome.prev_rect.is_some(), "prev_rect must be populated");
    assert!(chrome.next_rect.is_some(), "next_rect must be populated");
    assert!(chrome.close_rect.is_some(), "close_rect must be populated");
    // All affordances live on the title row (below the top pad).
    assert_eq!(chrome.close_rect.unwrap().y, PAD_TOP);
    assert_eq!(chrome.prev_rect.unwrap().y, PAD_TOP);
    assert_eq!(chrome.next_rect.unwrap().y, PAD_TOP);
    // Left side spacing: the title's first glyph lands exactly at
    // column `PAD_LEFT`, and the columns before it are blank.
    assert_eq!(
        buf[(PAD_LEFT, PAD_TOP)].symbol(),
        "A",
        "title must start at column PAD_LEFT",
    );
    for x in 0..PAD_LEFT {
        assert_eq!(
            buf[(x, PAD_TOP)].symbol(),
            " ",
            "columns before the title must be blank left padding",
        );
    }
    // Right side spacing: the close chip's last glyph ends exactly
    // `PAD_RIGHT` columns from the right edge.
    let close = chrome.close_rect.unwrap();
    assert_eq!(
        close.x + close.width,
        80 - PAD_RIGHT,
        "close chip must end PAD_RIGHT columns from the right edge",
    );
}

/// edge case 9: narrow-mode rendering truncates labels
/// and still registers row_rects.
#[test]
fn render_narrow_mode_registers_row_rects() {
    use crate::app::agent::AgentId;
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 5));
    let mut state = DashboardState::new();
    let row = DashboardRow {
        id: DashboardRowId::TopLevel(AgentId(1)),
        label: "abcdefghij ".repeat(10),
        subtitle: None,
        state: RowState::Working,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/tmp"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };
    let rows = vec![row];
    let theme = Theme::current();
    render_narrow_rows(&mut buf, Rect::new(0, 0, 30, 5), &theme, &rows, &mut state);
    assert!(!state.row_rects.is_empty());
}

/// Wide-mode hit rects include each item's trailing gap line and
/// tile the list contiguously, so hover/click never falls into a
/// dead zone between items.
#[test]
fn render_rows_hit_rects_leave_no_dead_zones() {
    let rows = vec![
        header_test_row(1, RowState::Working, "alpha"),
        header_test_row(2, RowState::Working, "beta"),
        header_test_row(3, RowState::Idle, "gamma"),
    ];
    let area = Rect::new(0, 0, 60, 30);
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    state.grouping = Grouping::State;
    let theme = Theme::current();
    render_rows(&mut buf, area, &theme, &rows, &mut state);

    assert_eq!(state.row_rects.len(), 3);
    for (id, rect) in &state.row_rects {
        assert_eq!(rect.height, ROW_HEIGHT, "row {id:?} must be full-height");
    }
    assert_eq!(state.section_rects.len(), 2);
    for (key, rect) in &state.section_rects {
        assert_eq!(
            rect.height, GROUP_HEADER_HEIGHT,
            "section {key:?} must be full-height",
        );
    }

    // Each hit rect starts exactly where the previous one ended.
    let mut rects: Vec<Rect> = state
        .row_rects
        .iter()
        .map(|(_, r)| *r)
        .chain(state.section_rects.iter().map(|(_, r)| *r))
        .collect();
    rects.sort_by_key(|r| r.y);
    for pair in rects.windows(2) {
        assert_eq!(
            pair[0].y + pair[0].height,
            pair[1].y,
            "hit rects must tile without gaps: {pair:?}",
        );
    }

    // Hovering a row highlights its content line fully and paints
    // half-cell halos on the spacer lines above and below, so the
    // highlight reads as centered on the text. These rows are
    // title-only, so the content line is the middle of the 3-cell
    // rect. Use an unquantized theme: `Theme::current()` in the
    // test environment collapses `bg_hover` onto `bg_base`, which
    // (correctly) suppresses the halos.
    let theme = Theme::echo_agent_night();
    assert_ne!(theme.bg_hover, theme.bg_base);
    let (id, rect) = state.row_rects[0].clone();
    state.hovered_row = Some(id);
    render_rows(&mut buf, area, &theme, &rows, &mut state);
    let title_y = rect.y + 1;
    assert_eq!(
        buf[(rect.x, title_y)].style().bg,
        Some(theme.bg_hover),
        "hovered row must highlight its content line",
    );
    let above = &buf[(rect.x, title_y - 1)];
    assert_eq!(above.symbol(), "\u{2580}", "spacer above must be a halo");
    assert_eq!(
        above.style().bg,
        Some(theme.bg_hover),
        "halo above must show the hover colour in its bottom half",
    );
    let below = &buf[(rect.x, title_y + 1)];
    assert_eq!(below.symbol(), "\u{2580}", "spacer below must be a halo");
    assert_eq!(
        below.style().fg,
        Some(theme.bg_hover),
        "halo below must show the hover colour in its top half",
    );
}

/// A row's content is vertically centered within its 3-cell rect:
/// a title-only row renders padding + title + padding, while a
/// title + secondary row stays top-aligned (2 lines cannot center
/// in 3 cells).
#[test]
fn render_row_centers_title_only_content() {
    let theme = Theme::current();
    let mut state = DashboardState::new();

    // Title-only → centered on the middle line.
    let row = header_test_row(1, RowState::Idle, "solo");
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 3));
    render_row(&mut buf, Rect::new(0, 0, 40, 3), &theme, &row, &mut state);
    assert_eq!(buf[(4, 1)].symbol(), "s", "title must sit on line 1");
    assert_eq!(buf[(4, 0)].symbol(), " ", "line 0 must be padding");
    assert_eq!(buf[(4, 2)].symbol(), " ", "line 2 must be padding");

    // Title + secondary → top-aligned.
    let mut row = header_test_row(2, RowState::Working, "pair");
    row.secondary_line = Some("Responding".to_string());
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 3));
    render_row(&mut buf, Rect::new(0, 0, 40, 3), &theme, &row, &mut state);
    assert_eq!(buf[(4, 0)].symbol(), "p", "title must sit on line 0");
    assert_eq!(buf[(4, 1)].symbol(), "R", "secondary must sit on line 1");
    assert_eq!(buf[(4, 2)].symbol(), " ", "line 2 must be padding");
}

/// Empty area is a quick exit.
#[test]
fn render_empty_state_zero_area_is_no_op() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
    let theme = Theme::current();
    render_empty_state(&mut buf, Rect::new(0, 0, 0, 0), &theme, false);
    // No-op assertion: nothing crashes.
}

/// no-match branch renders the filter feedback.
#[test]
fn render_no_match_paints_filter_hint() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 5));
    let theme = Theme::current();
    render_no_match(
        &mut buf,
        Rect::new(0, 0, 60, 5),
        &theme,
        &Filter::Agent("reviewer".into()),
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains("reviewer"),
        "no-match hint should embed the filter value, got: {content:?}"
    );
}

// ── snap_offset_to_line_boundary unit tests ──────────────────────

/// An offset already on a boundary is returned unchanged.
#[test]
fn snap_offset_already_on_boundary_returns_input() {
    let heights = vec![3u16, 3, 3];
    assert_eq!(snap_offset_to_line_boundary(0, &heights), 0);
    assert_eq!(snap_offset_to_line_boundary(3, &heights), 3);
    assert_eq!(snap_offset_to_line_boundary(6, &heights), 6);
}

/// A sub-row offset (1 or 2 cells into a 3-cell row) snaps DOWN
/// to the row's starting cell — the topmost visible row always
/// paints from its first cell.
#[test]
fn snap_offset_subrow_clips_to_row_start() {
    let heights = vec![3u16, 3, 3];
    assert_eq!(snap_offset_to_line_boundary(1, &heights), 0);
    assert_eq!(snap_offset_to_line_boundary(2, &heights), 0);
    assert_eq!(snap_offset_to_line_boundary(4, &heights), 3);
    assert_eq!(snap_offset_to_line_boundary(5, &heights), 3);
}

/// Headers (2 cells) and rows (3 cells) mix; the helper snaps
/// to whichever item-boundary precedes the offset.
#[test]
fn snap_offset_mixed_heights() {
    // [header=2, row=3, row=3]
    let heights = vec![2u16, 3, 3];
    // Inside the header (0..2):
    assert_eq!(snap_offset_to_line_boundary(0, &heights), 0);
    assert_eq!(snap_offset_to_line_boundary(1, &heights), 0);
    // At the row 0 start:
    assert_eq!(snap_offset_to_line_boundary(2, &heights), 2);
    // Inside row 0 (2..5):
    assert_eq!(snap_offset_to_line_boundary(3, &heights), 2);
    assert_eq!(snap_offset_to_line_boundary(4, &heights), 2);
    // At the row 1 start:
    assert_eq!(snap_offset_to_line_boundary(5, &heights), 5);
}

/// Offsets past the last item just stay clamped at the last
/// boundary — the bounds clamp in `clamp_viewport` is the layer
/// that prevents this in practice, but `snap_offset_to_line_boundary`
/// must be safe on its own to keep the contract local.
#[test]
fn snap_offset_past_last_item_returns_last_boundary() {
    let heights = vec![3u16, 3, 3];
    // Last boundary is at cell 6 (start of row index 2).
    assert_eq!(snap_offset_to_line_boundary(7, &heights), 6);
    assert_eq!(snap_offset_to_line_boundary(99, &heights), 6);
}

/// Empty heights → snap returns 0 regardless of offset.
#[test]
fn snap_offset_empty_heights_returns_zero() {
    assert_eq!(snap_offset_to_line_boundary(0, &[]), 0);
    assert_eq!(snap_offset_to_line_boundary(5, &[]), 0);
}

/// `popup_rect` takes the FULL bottom area
/// (no horizontal inset, no bottom inset) with only a top inset
/// reserved for the dashboard banner. Replaces the previous
/// centred-inset design which left the dashboard's own dispatch
/// input + footer visible below the popup, producing two
/// stacked input bars.
#[test]
fn popup_rect_takes_full_bottom_area_with_top_banner() {
    let view = Rect::new(0, 0, 200, 80);
    let popup = popup_rect(view);
    // No horizontal inset — popup spans the full width.
    assert_eq!(
        popup.x, view.x,
        "popup must start at view.x (no left inset)"
    );
    assert_eq!(
        popup.width, view.width,
        "popup must span the full view width",
    );
    // Top inset present — popup starts BELOW the banner.
    assert!(
        popup.y > view.y,
        "popup must sit below the banner (y={} expected > view.y={})",
        popup.y,
        view.y,
    );
    // No bottom inset — popup extends to the bottom of the view.
    assert_eq!(
        popup.y + popup.height,
        view.y + view.height,
        "popup must extend to the bottom of the view",
    );
}

/// Banner height is sized as ~1/3 of the screen
/// clamped into a sensible range (6-14 rows) so the rows are
/// readable on tall terminals without crowding the popup.
#[test]
fn popup_rect_leaves_room_for_banner_on_large_terminal() {
    let view = Rect::new(0, 0, 200, 60);
    let popup = popup_rect(view);
    let banner_h = popup.y - view.y;
    assert!(
        (6..=14).contains(&banner_h),
        "banner height {banner_h} must be in [6, 14] on a 60-row terminal",
    );
    // Popup still gets the majority of the screen height.
    assert!(
        popup.height as u32 * 100 >= view.height as u32 * 70,
        "popup height {} <70% of {} — banner too tall",
        popup.height,
        view.height,
    );
}

/// Very short terminals (height < banner_min +
/// 10) collapse the banner to 0 so the popup gets every available
/// row. Mirrors the agent view's "drop bottom_vpad on short
/// terminals" behaviour.
#[test]
fn popup_rect_collapses_banner_on_tiny_terminal() {
    let view = Rect::new(0, 0, 12, 6);
    let popup = popup_rect(view);
    // Tiny terminals: zero banner, popup IS the view.
    assert_eq!(popup.width, view.width);
    assert_eq!(popup.height, view.height);
    assert_eq!(popup.y, view.y);
}

/// Replacing the home-rolled chrome
/// with `picker::render_bordered_frame` means the divider sits
/// ABOVE the returned content rect. This test paints the chrome
/// plus a "fake agent" pattern in the inner rect and verifies
/// the divider's `─` glyph survives the inner paint.
#[test]
fn render_popup_overlay_divider_survives_inner_paint() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let (_cursor, _post_flush, drawn) = render_popup_overlay(
        &mut buf,
        Rect::new(0, 0, 60, 20),
        &theme,
        "Test session",
        &mut state,
        |inner, buf| {
            // Fill the inner with a non-divider character so a
            // regression where the inner paints over the divider
            // would clobber the `─` glyphs.
            for y in inner.y..inner.y + inner.height {
                for x in inner.x..inner.x + inner.width {
                    buf.set_string(x, y, "x", Style::default());
                }
            }
            (None, None)
        },
    );
    assert!(drawn);
    let content = buf_to_text(&buf);
    // The divider glyph `─` (U+2500) must appear at least once
    // somewhere AFTER the title row. If the inner paint
    // overwrote it the count would be zero.
    let divider_count = content.matches('\u{2500}').count();
    assert!(
        divider_count > 0,
        "divider U+2500 missing after inner paint; got: {content:?}",
    );
}

/// The popup paints a `[✗]` close
/// affordance and registers its hit rect on `DashboardState` so
/// `handle_mouse` can dispatch a popup close on click.
#[test]
fn render_popup_overlay_registers_close_hit_rect() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let _ = render_popup_overlay(
        &mut buf,
        Rect::new(0, 0, 60, 10),
        &theme,
        "Sample",
        &mut state,
        |_inner, _buf| (None, None),
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains('\u{2717}'),
        "close affordance [✗] missing, got: {content:?}",
    );
    let close_rect = state
        .popup_close_rect
        .expect("popup_close_rect must be registered");
    // The close rect should be on the title row (y == area.y + 1)
    // and on the right edge of the popup.
    assert_eq!(close_rect.y, 1);
    assert!(close_rect.x > 50);
    // outer rect should be set to the full popup area.
    let outer = state
        .popup_outer_rect
        .expect("popup_outer_rect must be registered");
    assert_eq!(outer, Rect::new(0, 0, 60, 10));
}

/// When the popup area is too small for
/// the canonical bordered frame, the overlay paints a fallback
/// hint inside an outlined box (rather than leaving the user
/// staring at an empty popup).
#[test]
fn render_popup_overlay_small_area_paints_fallback_hint() {
    // 4 rows of height — below `picker::render_bordered_frame`'s
    // 5-row minimum — triggers the fallback path.
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 4));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let (cursor, post_flush, drawn) = render_popup_overlay(
        &mut buf,
        Rect::new(0, 0, 40, 4),
        &theme,
        "Tiny",
        &mut state,
        |_inner, _buf| {
            panic!("draw_agent must NOT be called on the fallback path");
        },
    );
    assert!(cursor.is_none());
    assert!(post_flush.is_none());
    assert!(!drawn);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("too small"),
        "fallback hint missing, got: {content:?}",
    );
}

/// The footer hint always includes the rename
/// shortcut. Stops a regression where the footer dropped the
/// rename shortcut behind a feature flag or omitted it during a
/// conditional rebuild.
#[test]
fn render_footer_surfaces_shortcuts_link() {
    // Trailing shortcuts chip must match the registry primary key.
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains("shortcuts"),
        "footer must mention `shortcuts` (the help chip), got: {content:?}",
    );
    let primary = registry
        .find(crate::actions::ActionId::DashboardShortcutsHelp)
        .map(|d| d.default_key.display())
        .unwrap_or_else(|| "Ctrl+.".into());
    let expected = format!("{primary}:shortcuts");
    assert!(
        content.contains(&expected),
        "footer must include `{expected}` chip (registry primary), got: {content:?}",
    );
}

/// The location picker opens input-default, but under vim `Esc`
/// drops it to NAV — so its footer must surface the `i search` hint (and
/// hide it in input mode / when vim is off).
#[test]
fn location_picker_footer_shows_i_hint_in_vim_nav() {
    use super::super::state::LocationPickerState;
    let make = || {
        LocationPickerState::new(
            vec![],
            std::path::PathBuf::from("/tmp"),
            std::collections::HashMap::new(),
        )
    };
    let area = Rect::new(0, 0, 160, 48);
    let theme = Theme::current();

    // vim on + NAV (search inactive) → hint present.
    crate::appearance::cache::set_vim_mode(true);
    let mut nav = make();
    nav.picker.search_active = false;
    let mut buf = Buffer::empty(area);
    render_location_picker(&mut buf, area, &theme, &mut nav);
    assert!(
        buf_to_text(&buf).contains("i search"),
        "location picker footer must show `i search` in vim nav mode",
    );

    // vim on + INPUT (the open default) → hint absent.
    let mut input = make();
    input.picker.search_active = true;
    let mut buf_input = Buffer::empty(area);
    render_location_picker(&mut buf_input, area, &theme, &mut input);
    assert!(
        !buf_to_text(&buf_input).contains("i search"),
        "no `i search` hint while typing (input mode)",
    );

    // vim off → hint absent regardless of mode.
    crate::appearance::cache::set_vim_mode(false);
    let mut off = make();
    off.picker.search_active = false;
    let mut buf_off = Buffer::empty(area);
    render_location_picker(&mut buf_off, area, &theme, &mut off);
    assert!(
        !buf_to_text(&buf_off).contains("i search"),
        "no `i search` hint when vim-mode is off",
    );
}

/// Pressing the help key returns the
/// `DashboardOpenShortcutsHelp` action so the dispatcher can
/// build the modal state. No `error_toast` is set (the
/// an earlier polish iteration surfaced a hint via the dispatch
/// input placeholder, which the user explicitly rejected
/// because it conflicted with their typing slot).
#[test]
fn dashboard_shortcuts_help_action_opens_modal() {
    use super::super::state::DashboardState;
    let mut state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let key = KeyEvent {
        code: KeyCode::Char('.'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    let outcome = state.handle_input(&Event::Key(key), &registry);
    assert!(
        matches!(
            outcome,
            crate::app::app_view::InputOutcome::Action(
                crate::app::actions::Action::DashboardOpenShortcutsHelp,
            )
        ),
        "shortcuts key must emit DashboardOpenShortcutsHelp, got: {outcome:?}",
    );
    assert!(
        state.error_toast.is_none(),
        "no error_toast should be set — the modal carries the help, \
         not the dispatch input placeholder. Got: {:?}",
        state.error_toast,
    );
}

/// RenameDraft sanitation keeps control characters out of both render paths.
#[test]
fn sanitized_rename_draft_is_safe_in_both_render_paths() {
    use crate::app::agent::AgentId;
    let id = DashboardRowId::TopLevel(AgentId(7));
    let row = DashboardRow {
        id: id.clone(),
        label: "row label".to_string(),
        subtitle: None,
        state: RowState::Working,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/tmp"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };
    let rows = vec![row];
    let theme = Theme::current();

    // Wide path.
    {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        let mut state = DashboardState::new();
        state.selected = Some(id.clone());
        state.rename = Some(RenameDraft::new(id.clone(), "a\x1b[31m"));
        render_rows(&mut buf, Rect::new(0, 0, 80, 3), &theme, &rows, &mut state);
        let content = buf_to_text(&buf);
        assert!(
            !content.contains('\x1b'),
            "wide rename overlay must not retain ESC: {content:?}",
        );
        assert!(
            content.contains('a'),
            "wide rename overlay must keep the visible draft char: {content:?}",
        );
    }

    // Narrow path.
    {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        let mut state = DashboardState::new();
        state.selected = Some(id.clone());
        state.rename = Some(RenameDraft::new(id.clone(), "a\x1b[31m"));
        render_narrow_rows(&mut buf, Rect::new(0, 0, 30, 3), &theme, &rows, &mut state);
        let content = buf_to_text(&buf);
        assert!(
            !content.contains('\x1b'),
            "narrow rename overlay must not retain ESC: {content:?}",
        );
        assert!(
            content.contains('a'),
            "narrow rename overlay must keep the visible draft char: {content:?}",
        );
    }
}

/// Rename rendering preserves row chrome and title alignment in both layouts.
#[test]
fn render_rename_overlay_aligns_with_title_and_keeps_icon() {
    use crate::app::agent::AgentId;
    let id = DashboardRowId::TopLevel(AgentId(7));
    let row = DashboardRow {
        id: id.clone(),
        label: "row label".to_string(),
        subtitle: None,
        state: RowState::Idle,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/tmp"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };
    let rows = vec![row];
    let theme = Theme::current();
    let row_text = |buf: &Buffer, y: u16, w: u16| -> String {
        (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect()
    };

    // Wide path: this title-only row centers its title within its
    // 3-cell rect, so the title sits 3 below the group header
    // (header + gap + row top padding).
    // `title_byte` is a byte offset (for `str::find` comparisons);
    // `title_col` is the display column (the icon glyph is
    // multi-byte UTF-8, so the two differ) for cursor math.
    let (title_byte, title_col) = {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let mut state = DashboardState::new();
        render_rows(&mut buf, Rect::new(0, 0, 80, 5), &theme, &rows, &mut state);
        let line = row_text(&buf, 3, 80);
        let byte = line.find("row label").expect("title must render");
        (byte, line[..byte].chars().count() as u16)
    };
    {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let mut state = DashboardState::new();
        state.rename = Some(RenameDraft::new(id.clone(), "new name"));
        render_rows(&mut buf, Rect::new(0, 0, 80, 5), &theme, &rows, &mut state);
        let line = row_text(&buf, 3, 80);
        assert_eq!(
            line.find("rename: new name"),
            Some(title_byte),
            "wide: `rename:` must start at the title column, got: {line:?}",
        );
        assert_eq!(
            buf[(2, 3)].symbol(),
            crate::glyphs::diamond_hollow(),
            "wide: the state icon must stay in place while renaming",
        );
        // The cursor parks one cell past the typed draft — after
        // the `rename: ` prefix, never overlapping it.
        let prefix_w = "rename: ".len() as u16;
        let draft_w = "new name".len() as u16;
        assert_eq!(
            rename_cursor_pos(&state, &rows),
            Some((title_col + prefix_w + draft_w, 3)),
            "cursor must sit one cell past the draft text",
        );
        // With an empty draft the cursor sits immediately after
        // `rename: ` (the position typing lands at).
        state.rename = Some(RenameDraft::new(id.clone(), ""));
        assert_eq!(
            rename_cursor_pos(&state, &rows),
            Some((title_col + prefix_w, 3)),
            "empty draft: cursor must sit right after `rename: `",
        );
    }

    // Narrow path: row sits 1 below the group header (no gap).
    let title_col = {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        let mut state = DashboardState::new();
        render_narrow_rows(&mut buf, Rect::new(0, 0, 30, 3), &theme, &rows, &mut state);
        row_text(&buf, 1, 30)
            .find("row label")
            .expect("narrow title must render") as u16
    };
    {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        let mut state = DashboardState::new();
        state.rename = Some(RenameDraft::new(id.clone(), "nn"));
        render_narrow_rows(&mut buf, Rect::new(0, 0, 30, 3), &theme, &rows, &mut state);
        let line = row_text(&buf, 1, 30);
        assert_eq!(
            line.find("rename: nn").map(|c| c as u16),
            Some(title_col),
            "narrow: `rename:` must start at the title column, got: {line:?}",
        );
        assert_eq!(
            buf[(2, 1)].symbol(),
            crate::glyphs::diamond_hollow(),
            "narrow: the state icon must stay in place while renaming",
        );
    }
}

#[test]
fn rename_viewport_handles_long_unicode_in_wide_and_narrow_rows() {
    use crate::app::agent::AgentId;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    let id = DashboardRowId::TopLevel(AgentId(7));
    let row = DashboardRow {
        id: id.clone(),
        label: "row label".to_string(),
        subtitle: None,
        state: RowState::Idle,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/tmp"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };
    let rows = vec![row];
    let text = format!("{}中e\u{301}👩🏽\u{200d}💻", "x".repeat(90));
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();

    for (width, narrow, row_y) in [(80, false, 3), (30, true, 1)] {
        let area = Rect::new(0, 0, width, if narrow { 3 } else { 5 });
        let mut buffer = Buffer::empty(area);
        let mut state = DashboardState::new();
        state.rename = Some(RenameDraft::new(id.clone(), text.clone()));
        if narrow {
            render_narrow_rows(&mut buffer, area, &theme, &rows, &mut state);
        } else {
            render_rows(&mut buffer, area, &theme, &rows, &mut state);
        }
        let line = (0..width)
            .map(|x| buffer[(x, row_y)].symbol().to_string())
            .collect::<String>();
        assert!(line.contains('中'), "CJK tail missing: {line:?}");
        assert!(line.contains("e\u{301}"), "combining tail split: {line:?}");
        assert!(line.contains("👩🏽\u{200d}💻"), "ZWJ tail split: {line:?}",);
        let end_cursor = rename_cursor_pos(&state, &rows).expect("end cursor");

        let _ = state.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            &registry,
        );
        for _ in 0..20 {
            let _ = state.handle_input(
                &Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
                &registry,
            );
        }
        let mut middle_buffer = Buffer::empty(area);
        if narrow {
            render_narrow_rows(&mut middle_buffer, area, &theme, &rows, &mut state);
        } else {
            render_rows(&mut middle_buffer, area, &theme, &rows, &mut state);
        }
        let middle_cursor = rename_cursor_pos(&state, &rows).expect("middle cursor");
        assert_ne!(
            state.rename.as_ref().expect("rename draft").cursor_byte(),
            text.len()
        );
        if !narrow {
            assert_ne!(middle_cursor, end_cursor);
        }
        let prefix_x = (0..width)
            .find(|x| middle_buffer[(*x, row_y)].symbol() == "r")
            .expect("rename prefix");
        let row_rect = state
            .row_rects
            .iter()
            .find(|(row_id, _)| row_id == &id)
            .map(|(_, rect)| *rect)
            .expect("rename row rect");
        let editor_x = prefix_x + RENAME_PREFIX.len() as u16;
        let editor_width = row_rect
            .x
            .saturating_add(row_rect.width)
            .saturating_sub(editor_x);
        let expected_cursor = editor_x + 20u16.min(editor_width.saturating_sub(1));
        assert_eq!(middle_cursor, (expected_cursor, row_y));
    }
}

/// On a 3-row rect the dispatch input
/// paints a rounded-box chrome so it reads as a real input
/// field. The text row contains the `❯` prefix.
#[test]
fn render_dispatch_paints_rounded_box_chrome() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 3));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let cursor = render_dispatch(&mut buf, Rect::new(0, 0, 60, 3), &theme, &mut state, None);
    assert!(cursor.is_some(), "dispatch must return a cursor position");
    let content = buf_to_text(&buf);
    for corner in ['\u{256d}', '\u{2570}', '\u{256e}', '\u{256f}'] {
        assert!(
            content.contains(corner),
            "dispatch chrome must paint corner `{corner}`, got: {content:?}",
        );
    }
    assert!(
        content.contains('\u{276F}'),
        "dispatch must paint ❯ prefix inside the box, got: {content:?}",
    );
}

#[test]
fn render_search_mode_uses_textarea_cursor_not_text_end() {
    let area = Rect::new(0, 0, 40, 3);
    let mut buffer = Buffer::empty(area);
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.search_mode = true;
    state.dispatch.set_text("abcdef");
    state.dispatch.set_cursor(2);

    let cursor = render_dispatch(&mut buffer, area, &theme, &mut state, None)
        .expect("focused search cursor");
    let prefix_x = (0..area.width)
        .find(|x| buffer[(*x, cursor.1)].symbol() == "S")
        .expect("Search prefix");
    assert_eq!(cursor.0, prefix_x + "Search: ".len() as u16 + 2);
}

#[test]
fn render_search_mode_clips_prefix_and_cursor_at_widths_one_through_nine() {
    let theme = Theme::current();
    for width in 1..=9 {
        let full = Rect::new(0, 0, 14, 1);
        let area = Rect::new(2, 0, width, 1);
        let mut buffer = Buffer::empty(full);
        buffer.set_string(0, 0, "#".repeat(full.width as usize), Style::default());
        let mut state = DashboardState::new();
        state.search_mode = true;
        state.dispatch.set_text("abcdef");
        state.dispatch.set_cursor(2);

        let cursor = render_dispatch(&mut buffer, area, &theme, &mut state, None)
            .expect("focused narrow search cursor");
        assert!(cursor.0 >= area.x && cursor.0 < area.x + area.width);
        for x in 0..full.width {
            if x < area.x || x >= area.x + area.width {
                assert_eq!(
                    buffer[(x, 0)].symbol(),
                    "#",
                    "width {width} wrote outside at column {x}",
                );
            }
        }
    }
}

#[test]
fn render_dispatch_keeps_generic_paste_preview_but_suppresses_image_preview() {
    let area = Rect::new(0, 17, 80, 3);
    let overlay = Rect::new(0, 0, 80, 17);
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.dispatch.handle_paste("alpha\nbeta\ngamma\ndelta");
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
    let _ = render_dispatch(&mut buf, area, &theme, &mut state, Some(overlay));
    let text = buf_to_text(&buf);
    assert!(text.contains("alpha"));
    assert!(text.contains("delta"));

    state.dispatch.set_text("");
    state
        .dispatch
        .insert_image(crate::prompt_images::from_clipboard_data(
            &crate::clipboard::ImageData {
                data: vec![1, 2, 3],
                mime_type: "image/png".into(),
            },
        ))
        .unwrap();
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
    let _ = render_dispatch(&mut buf, area, &theme, &mut state, Some(overlay));
    let text = buf_to_text(&buf);
    assert!(text.contains("Image #1"));
    assert!(!text.contains("Format:"));
    assert!(!text.contains("Preview pending"));
}

/// On a 1-row rect the dispatch falls back to a bare
/// `❯ {text}` line (no chrome) so the input stays usable on
/// terminals too short for the box.
#[test]
fn render_dispatch_falls_back_to_single_line_on_short_area() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 60, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let cursor = render_dispatch(&mut buf, Rect::new(0, 0, 60, 1), &theme, &mut state, None);
    assert!(
        cursor.is_some(),
        "single-line fallback must return a cursor"
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains('\u{276F}'),
        "fallback must paint ❯, got: {content:?}"
    );
    for corner in ['\u{256d}', '\u{2570}'] {
        assert!(
            !content.contains(corner),
            "single-line fallback must NOT paint `{corner}`, got: {content:?}",
        );
    }
}

/// Placeholder reads `Dispatch a new agent` — but only while the
/// input is UNFOCUSED (overview list holds focus). The dispatch
/// input always spawns a new session.
#[test]
fn render_dispatch_placeholder_paints_only_when_unfocused() {
    // Unfocused input (list focused) → placeholder shows.
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.list_focused = true;
    let _ = render_dispatch(&mut buf, Rect::new(0, 0, 80, 3), &theme, &mut state, None);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Dispatch a new agent"),
        "unfocused placeholder missing, got: {content:?}",
    );
}

/// Focused input (the default) suppresses the placeholder — the
/// visible caret is the affordance; the text area stays clear.
#[test]
fn render_dispatch_placeholder_hidden_when_focused() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    assert!(!state.list_focused, "fresh state must focus the input");
    let cursor = render_dispatch(&mut buf, Rect::new(0, 0, 80, 3), &theme, &mut state, None);
    let content = buf_to_text(&buf);
    assert!(
        !content.contains("Dispatch a new agent"),
        "focused input must not paint the placeholder, got: {content:?}",
    );
    // The `❯` prefix and the caret position survive.
    assert!(
        content.contains('\u{276F}'),
        "prefix must still paint, got: {content:?}",
    );
    assert!(cursor.is_some(), "focused input must report a caret");
}

/// The placeholder stays `Dispatch a new agent` even when a row is
/// selected — the input never becomes a reply target (selection is
/// purely the overview navigation cursor; Enter on it OPENS the
/// agent). This is the regression guard for the "stuck replying to
/// the same agent" trap.
#[test]
fn render_dispatch_placeholder_stays_new_session_when_row_selected() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.focus_row(super::super::state::DashboardRowId::TopLevel(
        crate::app::agent::AgentId(0),
    ));
    // Unfocus the input (placeholder only paints while unfocused).
    state.list_focused = true;
    let _ = render_dispatch(&mut buf, Rect::new(0, 0, 80, 3), &theme, &mut state, None);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Dispatch a new agent"),
        "placeholder must stay new-session even with a row selected, got: {content:?}",
    );
    assert!(
        !content.contains("Reply to"),
        "the dispatch input must never show a reply placeholder, got: {content:?}",
    );
}

/// When a dispatch-validation toast is pending (e.g. the user
/// pressed Enter on a too-short prompt), the feedback is painted as
/// a badge on the box's TOP BORDER row — visible even while the
/// rejected text is still in the input — rather than only as an
/// empty-input placeholder. The placeholder itself stays the plain
/// new-session text.
#[test]
fn render_dispatch_paints_feedback_badge_on_top_border() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    // Unfocus the input so the placeholder assertion below stays
    // meaningful (placeholder only paints while unfocused).
    state.list_focused = true;
    state.error_toast = Some("Too short — describe the task (4+ chars)".to_string());
    let _ = render_dispatch(&mut buf, Rect::new(0, 0, 80, 3), &theme, &mut state, None);

    // Toast text lands on the TOP border row (y == 0).
    let top_row: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    assert!(
        top_row.contains("Too short"),
        "feedback badge must paint on the top border, got: {top_row:?}",
    );
    // The badge ends before the right `╮` corner (corner preserved).
    assert_eq!(
        buf[(79, 0)].symbol(),
        "\u{256e}",
        "right rounded corner must survive the badge",
    );
    // Placeholder remains the plain new-session text (error is NOT
    // shown inline anymore).
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Dispatch a new agent"),
        "placeholder must remain the new-session text, got: {content:?}",
    );
}

/// The badge paints the toast VERBATIM in the neutral accent colour:
/// a message that already carries its own glyph (as the `show_toast`
/// builders produce, e.g. `✓ Theme: …`) keeps that single glyph — no
/// `✗` is prepended (regression guard for the `✗ ✓ …` doubling) — and
/// it is NOT painted in the error red.
#[test]
fn feedback_badge_renders_verbatim_in_neutral_color() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let check = crate::glyphs::check_mark();
    state.error_toast = Some(format!("{check} Theme: EchoAgent Day"));
    let _ = render_dispatch(&mut buf, Rect::new(0, 0, 80, 3), &theme, &mut state, None);

    let top_row: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    assert!(
        top_row.contains(&format!("{check} Theme: EchoAgent Day")),
        "badge must paint the message verbatim, got: {top_row:?}",
    );
    assert!(
        !top_row.contains(crate::glyphs::ballot_x()),
        "badge must not prepend a ✗ to a message that already has a glyph, got: {top_row:?}",
    );
    // Neutral colour — accent_user, never the error red.
    let cx = (0..80)
        .find(|&x| buf[(x, 0)].symbol() == check)
        .expect("the ✓ glyph must be painted");
    assert_eq!(
        buf[(cx, 0)].fg,
        theme.accent_user,
        "badge must paint in the neutral accent_user colour (not the error red)",
    );
}

/// Helper for the group-header tests: build a top-level row with
/// the given id + state, all other fields filled with sensible
/// defaults. Keeps the per-test setup compact.
fn header_test_row(id: u32, state: RowState, label: &str) -> DashboardRow {
    use crate::app::agent::AgentId;
    DashboardRow {
        id: DashboardRowId::TopLevel(AgentId(id as usize)),
        label: label.to_string(),
        subtitle: None,
        state,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: std::path::PathBuf::from("/tmp"),
        last_change_at: std::time::SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    }
}

/// A collapsed state section keeps its header (with the true count)
/// but hides its rows; other sections are unaffected.
#[test]
fn build_dashboard_lines_hides_collapsed_state_section() {
    use std::collections::HashSet;
    let rows = vec![
        header_test_row(1, RowState::Working, "a"),
        header_test_row(2, RowState::Working, "b"),
        header_test_row(3, RowState::Idle, "c"),
    ];

    // Nothing collapsed → both Working rows present.
    let none: HashSet<SectionKey> = HashSet::new();
    let lines = build_dashboard_lines(&rows, Grouping::State, &Filter::None, &none, false, false);
    let working_rows = lines
        .iter()
        .filter(|l| matches!(l, DashboardLine::Row(r) if r.state == RowState::Working))
        .count();
    assert_eq!(working_rows, 2, "expanded Working section shows both rows");

    // Collapse Working → header stays (count 2), rows hidden.
    let mut collapsed = HashSet::new();
    collapsed.insert(SectionKey::State(RowState::Working));
    let lines = build_dashboard_lines(
        &rows,
        Grouping::State,
        &Filter::None,
        &collapsed,
        false,
        false,
    );
    assert!(
        lines.iter().any(|l| matches!(
            l,
            DashboardLine::Header { state, count } if *state == RowState::Working && *count == 2
        )),
        "collapsed Working header must still render with its true count",
    );
    let working_rows = lines
        .iter()
        .filter(|l| matches!(l, DashboardLine::Row(r) if r.state == RowState::Working))
        .count();
    assert_eq!(working_rows, 0, "collapsed Working section hides its rows");
    // The Idle section is unaffected.
    let idle_rows = lines
        .iter()
        .filter(|l| matches!(l, DashboardLine::Row(r) if r.state == RowState::Idle))
        .count();
    assert_eq!(idle_rows, 1, "other sections stay expanded");
}

/// A collapsed "Pinned" section keeps its header but hides the pinned
/// rows (grouping ON).
#[test]
fn build_dashboard_lines_hides_collapsed_pinned_section() {
    use std::collections::HashSet;
    let mut pinned_row = header_test_row(1, RowState::Idle, "pinned");
    pinned_row.pinned = true;
    let rows = vec![pinned_row, header_test_row(2, RowState::Working, "other")];

    let mut collapsed = HashSet::new();
    collapsed.insert(SectionKey::Pinned);
    let lines = build_dashboard_lines(
        &rows,
        Grouping::State,
        &Filter::None,
        &collapsed,
        false,
        false,
    );
    assert!(
        lines
            .iter()
            .any(|l| matches!(l, DashboardLine::PinnedHeader { count } if *count == 1)),
        "collapsed Pinned header must still render",
    );
    // The pinned row is hidden; the (non-pinned) Working row remains.
    let visible_rows = lines
        .iter()
        .filter(|l| matches!(l, DashboardLine::Row(_)))
        .count();
    assert_eq!(visible_rows, 1, "only the non-pinned row stays visible");
}

/// An idle row last active `secs_ago` seconds in the past.
fn aged_idle_row(id: u32, secs_ago: u64) -> DashboardRow {
    let mut r = header_test_row(id, RowState::Idle, "idle");
    r.last_change_at = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(secs_ago))
        .expect("test clock underflow");
    r
}

/// Seconds clearly outside the 1h Idle freshness window.
const OLD_SECS: u64 = 2 * 60 * 60;

fn idle_row_count(lines: &[DashboardLine]) -> usize {
    lines
        .iter()
        .filter(|l| matches!(l, DashboardLine::Row(r) if r.state == RowState::Idle))
        .count()
}

fn overflow_of(lines: &[DashboardLine]) -> Option<(usize, bool)> {
    lines.iter().find_map(|l| match l {
        DashboardLine::IdleOverflow { hidden, expanded } => Some((*hidden, *expanded)),
        _ => None,
    })
}

/// Old idle agents beyond `MAX_VISIBLE_IDLE` fold into the overflow
/// row; the header still reports the true total.
#[test]
fn idle_cap_folds_old_agents() {
    use std::collections::HashSet;
    // (MAX_VISIBLE_IDLE + 3) OLD idle agents → cap shown, 3 folded.
    let total = MAX_VISIBLE_IDLE as u32 + 3;
    let rows: Vec<DashboardRow> = (0..total).map(|i| aged_idle_row(i, OLD_SECS)).collect();
    let none: HashSet<SectionKey> = HashSet::new();
    let lines = build_dashboard_lines(&rows, Grouping::State, &Filter::None, &none, false, false);

    assert_eq!(
        idle_row_count(&lines),
        MAX_VISIBLE_IDLE,
        "caps to MAX_VISIBLE_IDLE"
    );
    assert_eq!(
        overflow_of(&lines),
        Some((total as usize - MAX_VISIBLE_IDLE, false)),
        "overflow row reports the folded count, not expanded",
    );
    // Header still shows the TRUE total, not the visible count.
    assert!(
        lines.iter().any(|l| matches!(
            l,
            DashboardLine::Header { state, count } if *state == RowState::Idle && *count == total as usize
        )),
        "Idle header keeps the true total count",
    );
}

/// Recent idle agents are never folded, even beyond the count cap —
/// the freshness window keeps a burst of new sessions visible.
#[test]
fn idle_cap_keeps_recent_beyond_count() {
    use std::collections::HashSet;
    // 9 RECENT idle agents (just now) → all shown, no overflow.
    let rows: Vec<DashboardRow> = (0..9).map(|i| aged_idle_row(i, 0)).collect();
    let none: HashSet<SectionKey> = HashSet::new();
    let lines = build_dashboard_lines(&rows, Grouping::State, &Filter::None, &none, false, false);
    assert_eq!(
        idle_row_count(&lines),
        9,
        "all recent idle agents stay visible"
    );
    assert_eq!(overflow_of(&lines), None, "no overflow when nothing is old");
}

/// Mixed freshness: recent agents always show; the oldest beyond the
/// cap fold. Rows arrive recent-first (matching the real sort).
#[test]
fn idle_cap_mixes_recent_and_old() {
    use std::collections::HashSet;
    // 4 recent + (cap - 1) old = cap + 3 total. base_limit = max(cap, 4) = cap → 3 folded.
    let total = MAX_VISIBLE_IDLE as u32 + 3;
    let mut rows: Vec<DashboardRow> = (0..4).map(|i| aged_idle_row(i, 0)).collect();
    rows.extend((4..total).map(|i| aged_idle_row(i, OLD_SECS)));
    let none: HashSet<SectionKey> = HashSet::new();
    let lines = build_dashboard_lines(&rows, Grouping::State, &Filter::None, &none, false, false);
    assert_eq!(
        idle_row_count(&lines),
        MAX_VISIBLE_IDLE,
        "shows cap (incl. all 4 recent)"
    );
    assert_eq!(
        overflow_of(&lines),
        Some((total as usize - MAX_VISIBLE_IDLE, false))
    );
}

/// `idle_show_all` reveals every agent and flips the overflow row to
/// the "show fewer" (expanded) state.
#[test]
fn idle_cap_show_all_reveals_all() {
    use std::collections::HashSet;
    let total = MAX_VISIBLE_IDLE as u32 + 3;
    let rows: Vec<DashboardRow> = (0..total).map(|i| aged_idle_row(i, OLD_SECS)).collect();
    let none: HashSet<SectionKey> = HashSet::new();
    let lines = build_dashboard_lines(&rows, Grouping::State, &Filter::None, &none, true, false);
    assert_eq!(
        idle_row_count(&lines),
        total as usize,
        "show-all reveals every idle agent"
    );
    assert_eq!(
        overflow_of(&lines),
        Some((total as usize - MAX_VISIBLE_IDLE, true)),
        "overflow stays as a 'show fewer' affordance when expanded",
    );
}

/// Folding only kicks in at MIN_IDLE_FOLD (2): a single over-cap row
/// is shown rather than hidden behind a same-height overflow row.
#[test]
fn idle_cap_does_not_fold_a_single_row() {
    use std::collections::HashSet;
    let none: HashSet<SectionKey> = HashSet::new();
    // MAX_VISIBLE_IDLE + 1 old → would hide 1 → no fold.
    let rows: Vec<DashboardRow> = (0..MAX_VISIBLE_IDLE as u32 + 1)
        .map(|i| aged_idle_row(i, OLD_SECS))
        .collect();
    let lines = build_dashboard_lines(&rows, Grouping::State, &Filter::None, &none, false, false);
    assert_eq!(
        idle_row_count(&lines),
        MAX_VISIBLE_IDLE + 1,
        "1 over cap is not folded"
    );
    assert_eq!(overflow_of(&lines), None);
    // MAX_VISIBLE_IDLE + 2 old → hides 2 → folds.
    let rows: Vec<DashboardRow> = (0..MAX_VISIBLE_IDLE as u32 + 2)
        .map(|i| aged_idle_row(i, OLD_SECS))
        .collect();
    let lines = build_dashboard_lines(&rows, Grouping::State, &Filter::None, &none, false, false);
    assert_eq!(overflow_of(&lines), Some((2, false)), "2 over cap fold");
}

/// The cap is suppressed under an active filter — when you search,
/// every match shows (no folding).
#[test]
fn idle_cap_disabled_under_filter() {
    use std::collections::HashSet;
    let total = MAX_VISIBLE_IDLE as u32 + 3;
    let rows: Vec<DashboardRow> = (0..total).map(|i| aged_idle_row(i, OLD_SECS)).collect();
    let none: HashSet<SectionKey> = HashSet::new();
    let lines = build_dashboard_lines(
        &rows,
        Grouping::State,
        &Filter::Substring("idle".into()),
        &none,
        false,
        false,
    );
    assert_eq!(
        overflow_of(&lines),
        None,
        "no fold under a substring filter"
    );

    // Search mode with an EMPTY query keeps `Filter::None`, but folding
    // must still be suspended (search is active) — the doc'd rule.
    let lines = build_dashboard_lines(
        &rows,
        Grouping::State,
        &Filter::None,
        &none,
        false,
        /* search_active */ true,
    );
    assert_eq!(
        overflow_of(&lines),
        None,
        "no fold while search mode is active even with an empty query",
    );
    assert_eq!(
        idle_row_count(&lines),
        total as usize,
        "every idle agent shows while searching",
    );
}

/// A collapsed Idle section hides its rows (and the overflow) via the
/// section-collapse path — the cap and collapse don't double up.
#[test]
fn idle_cap_yields_to_section_collapse() {
    use std::collections::HashSet;
    let rows: Vec<DashboardRow> = (0..9).map(|i| aged_idle_row(i, OLD_SECS)).collect();
    let mut collapsed = HashSet::new();
    collapsed.insert(SectionKey::State(RowState::Idle));
    let lines = build_dashboard_lines(
        &rows,
        Grouping::State,
        &Filter::None,
        &collapsed,
        false,
        false,
    );
    assert_eq!(idle_row_count(&lines), 0, "collapsed Idle hides all rows");
    assert_eq!(overflow_of(&lines), None, "no overflow row under collapse");
}

/// The overflow row is a keyboard cursor target.
#[test]
fn idle_overflow_is_focusable_when_capped() {
    let rows: Vec<DashboardRow> = (0..MAX_VISIBLE_IDLE as u32 + 3)
        .map(|i| aged_idle_row(i, OLD_SECS))
        .collect();
    let none = std::collections::HashSet::new();
    let f = focusables(&rows, Grouping::State, &Filter::None, &none, false, false);
    assert!(
        f.iter().any(|x| matches!(x, Focusable::IdleOverflow)),
        "focusables must include the overflow toggle when capped",
    );
    // And the hidden idle rows are NOT focusable (capped away).
    let row_targets = f.iter().filter(|x| matches!(x, Focusable::Row(_))).count();
    assert_eq!(
        row_targets, MAX_VISIBLE_IDLE,
        "only the visible idle rows are focusable"
    );
}

/// Pinned top-level agents are lifted into a dedicated "Pinned" section
/// at the very top — above the state groups — so a pinned (e.g. idle)
/// agent reads as pinned rather than landing under its state header. The
/// pinned rows are NOT counted in the state-group headers.
#[test]
fn render_rows_emits_pinned_section_at_top() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 30));
    let mut state = DashboardState::new();
    assert_eq!(state.grouping, Grouping::State);
    // Input is pre-sorted (pinned first), as `sort_rows` guarantees.
    let mut pinned = header_test_row(1, RowState::Idle, "pinned idle row");
    pinned.pinned = true;
    let rows = vec![
        pinned,
        header_test_row(2, RowState::Working, "working row"),
        header_test_row(3, RowState::Idle, "idle row"),
    ];
    let theme = Theme::current();
    render_rows(&mut buf, Rect::new(0, 0, 80, 30), &theme, &rows, &mut state);
    let content = buf_to_text(&buf);

    // A dedicated "Pinned" section header with a count of 1.
    assert!(
        content.contains("Pinned 1"),
        "missing `Pinned` section header, got: {content:?}",
    );
    // It sits ABOVE the state groups.
    let idx_pinned = content.find("Pinned").expect("Pinned header present");
    let idx_working = content.find("Working").expect("Working header present");
    assert!(
        idx_pinned < idx_working,
        "the Pinned section must be at the top, got: {content:?}",
    );
    // The remaining (unpinned) idle row still gets its own `Idle 1` header
    // — the pinned idle row is NOT folded into it.
    assert!(
        content.contains("Idle 1"),
        "unpinned idle row must keep its own `Idle 1` header, got: {content:?}",
    );
    assert!(
        content.contains("pinned idle row"),
        "pinned row label renders"
    );
}

/// With grouping OFF (Directory) the "Pinned" text header is suppressed;
/// instead a textless divider (a horizontal rule, no label) separates the
/// pinned block from the rest. No state headers are emitted either.
#[test]
fn render_rows_groups_off_uses_divider_not_pinned_header() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 30));
    let mut state = DashboardState::new();
    state.grouping = Grouping::Directory; // groups off (Ctrl+G)
    let mut pinned = header_test_row(1, RowState::Idle, "pinned row");
    pinned.pinned = true;
    let rows = vec![pinned, header_test_row(2, RowState::Working, "working row")];
    let theme = Theme::current();
    render_rows(&mut buf, Rect::new(0, 0, 80, 30), &theme, &rows, &mut state);
    let content = buf_to_text(&buf);

    // No labelled "Pinned"/state headers in groups-off mode.
    assert!(
        !content.contains("Pinned"),
        "the `Pinned` text header must be hidden when grouping is off, got: {content:?}",
    );
    assert!(
        !content.contains("Working "),
        "no state headers when grouping is off, got: {content:?}",
    );
    // A horizontal-rule divider separates the pinned block from the rest.
    assert!(
        content.contains('\u{2500}'),
        "a divider rule must separate pinned from non-pinned, got: {content:?}",
    );
    // The pinned row still renders above the divider, which is above the rest.
    let idx_pinned = content.find("pinned row").expect("pinned row present");
    let idx_rule = content.find('\u{2500}').expect("divider present");
    let idx_working = content.find("working row").expect("working row present");
    assert!(
        idx_pinned < idx_rule && idx_rule < idx_working,
        "order must be pinned → divider → rest, got: {content:?}",
    );
}

/// State group headers are emitted at every
/// top-level state transition when grouping is `State`. The
/// renderer must paint the headers in NeedsInput → Working →
/// Idle → Completed → Failed order (matching
/// `RowState::group_priority`).
///
/// Header chrome now uses Option A
/// (`  ● Label (N)`): a 2-col indent, a state-coloured dot, then
/// the label + count in `gray_dim`. The previous full-row
/// `── Label (N) ────────────────` chrome was dropped (the
/// trailing dashes felt visually obnoxious — user complaint).
#[test]
fn render_rows_emits_group_headers_in_state_order() {
    // Rows are 3 cells tall, headers 2 cells; 5 of each
    // needs 25 cells of vertical room.
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 30));
    let mut state = DashboardState::new();
    assert_eq!(state.grouping, Grouping::State);
    let rows = vec![
        header_test_row(1, RowState::NeedsInput, "needs-input row"),
        header_test_row(2, RowState::Working, "working row"),
        header_test_row(3, RowState::Idle, "idle row"),
        header_test_row(4, RowState::Completed, "completed row"),
        header_test_row(5, RowState::Failed, "failed row"),
    ];
    let theme = Theme::current();
    render_rows(&mut buf, Rect::new(0, 0, 80, 30), &theme, &rows, &mut state);
    let content = buf_to_text(&buf);
    // Group labels: Awaiting / Working / Idle / Done / Failed.
    for label in ["Awaiting", "Working", "Idle", "Done", "Failed"] {
        assert!(
            content.contains(label),
            "missing group header `{label}`, got: {content:?}",
        );
    }
    // Headers appear in the canonical priority order.
    let idx_aw = content.find("Awaiting").expect("Awaiting present");
    let idx_wk = content.find("Working").expect("Working present");
    let idx_id = content.find("Idle").expect("Idle present");
    let idx_dn = content.find("Done").expect("Done present");
    let idx_fl = content.find("Failed").expect("Failed present");
    assert!(idx_aw < idx_wk, "Awaiting must precede Working");
    assert!(idx_wk < idx_id, "Working must precede Idle");
    assert!(idx_id < idx_dn, "Idle must precede Done");
    assert!(idx_dn < idx_fl, "Done must precede Failed");
    // Group headers read `Label N ──────…`.
    assert!(
        content.contains("Awaiting 1"),
        "header label + count missing or malformed, got: {content:?}",
    );
    assert!(
        content.contains('\u{2500}'),
        "header must paint the trailing horizontal rule, got: {content:?}",
    );
    assert!(
        !content.contains("Awaiting ("),
        "old parenthesised `Awaiting (1)` form must be gone, got: {content:?}",
    );
    // Each row's label still renders.
    for label in [
        "needs-input row",
        "working row",
        "idle row",
        "completed row",
        "failed row",
    ] {
        assert!(
            content.contains(label),
            "missing row label `{label}`, got: {content:?}",
        );
    }
}

/// The list scrollbar is a thick `█` thumb overlaid on the right edge,
/// and it does NOT reserve a column — the row content is byte-for-byte
/// identical whether or not the scrollbar shows (no layout shift).
#[test]
fn render_rows_scrollbar_is_thick_overlay_without_layout_shift() {
    let theme = Theme::current();
    // 6 working rows → 1 header (2 cells) + 6 rows (3 cells) = 20 cells.
    let rows: Vec<_> = (0..6)
        .map(|i| header_test_row(i, RowState::Working, "working task"))
        .collect();
    let w = 60u16;

    // Tall viewport → everything fits, no scrollbar.
    let mut buf_fit = Buffer::empty(Rect::new(0, 0, w, 24));
    let mut state_fit = DashboardState::new();
    render_rows(
        &mut buf_fit,
        Rect::new(0, 0, w, 24),
        &theme,
        &rows,
        &mut state_fit,
    );

    // Short viewport → overflow, scrollbar overlays.
    let h = 8u16;
    let mut buf_scroll = Buffer::empty(Rect::new(0, 0, w, h));
    let mut state_scroll = DashboardState::new();
    render_rows(
        &mut buf_scroll,
        Rect::new(0, 0, w, h),
        &theme,
        &rows,
        &mut state_scroll,
    );

    // The thick `█` thumb is painted on the rightmost column.
    let last_x = w - 1;
    let has_thumb = (0..h).any(|y| buf_scroll[(last_x, y)].symbol() == "\u{2588}");
    assert!(has_thumb, "scrollbar thumb (█) must overlay the right edge");
    // Old thin `│`-only thumb is gone.
    let thin_only = (0..h).all(|y| buf_scroll[(last_x, y)].symbol() != "\u{2588}");
    assert!(!thin_only, "thumb must be the thick block glyph");

    // No layout shift: every content column (all but the overlaid right
    // edge) matches the no-scrollbar render across the visible top.
    for y in 0..h {
        for x in 0..(w - 1) {
            assert_eq!(
                buf_scroll[(x, y)].symbol(),
                buf_fit[(x, y)].symbol(),
                "content shifted at ({x},{y}) when the scrollbar appeared",
            );
        }
    }
}

/// Row layout is two visual lines:
///
/// ```text
///   ◆ who are you?                                                     4s    <- title row
///     Responding                                                             <- secondary row
/// ```
///
/// - Col 0: selection marker (thin bar `▏` when selected, space
///   otherwise). The bar spans the full content height of a
///   selected row (title + secondary lines). No hover glyph.
/// - Col 1: 1-col gap.
/// - Col 2: state icon.
/// - Col 3: 1-col gap.
/// - Col 4: label starts on row 0, and the secondary text starts
///   at the same column on row 1.
/// - Right edge: age column (`{n}s/m/h`).
#[test]
fn render_row_two_line_layout_paints_title_and_secondary() {
    use std::path::PathBuf;
    use std::time::SystemTime;
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 2));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.spinner_tick = 8; // → dot_spinner_frames()[2] = `⸬`.
    let row = DashboardRow {
        id: DashboardRowId::TopLevel(crate::app::agent::AgentId(1)),
        label: "who are you?".to_string(),
        subtitle: None,
        state: RowState::Working,
        activity: Some("Responding".to_string()),
        secondary_line: Some("Responding".to_string()),
        cwd_display: String::new(),
        cwd: PathBuf::from("/tmp"),
        last_change_at: SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };
    render_row(&mut buf, Rect::new(0, 0, 100, 2), &theme, &row, &mut state);

    // Title row.
    assert_eq!(
        buf[(0, 0)].symbol(),
        " ",
        "row 0 col 0 must be marker space"
    );
    assert_eq!(
        buf[(1, 0)].symbol(),
        " ",
        "row 0 col 1 must be the post-marker gap"
    );
    assert_eq!(
        buf[(2, 0)].symbol(),
        "\u{2e2c}",
        "row 0 col 2 must be the spinner glyph `⸬` at tick=8",
    );
    assert_eq!(
        buf[(3, 0)].symbol(),
        " ",
        "row 0 col 3 must be the post-icon gap"
    );
    assert_eq!(
        buf[(4, 0)].symbol(),
        "w",
        "row 0 col 4 must start the label"
    );

    // Secondary row — `Responding` starts at the same column as
    // the title's label start (col 4).
    assert_eq!(
        buf[(4, 1)].symbol(),
        "R",
        "row 1 col 4 must start the secondary text",
    );

    // Age column right-aligns in the last few cells of row 0.
    let mut saw_s_in_age_zone = false;
    for x in (100 - 8)..100 {
        if buf[(x, 0)].symbol() == "s" {
            saw_s_in_age_zone = true;
            break;
        }
    }
    let content = buf_to_text(&buf);
    assert!(
        saw_s_in_age_zone,
        "age column must paint a duration ending in `s`, got: {content:?}",
    );
}

/// The SELECTED row brightens its secondary text from
/// `gray_dim` to `text_secondary` so the user can read what
/// the agent is doing without leaving the dashboard. The
/// unselected baseline stays dim — the row's metadata tail
/// shouldn't compete with the title for attention. Pins
/// both states in one test so a regression that flipped
/// either direction would fail.
#[test]
fn render_row_selected_brightens_secondary_text() {
    use std::path::PathBuf;
    use std::time::SystemTime;
    let theme = Theme::current();
    let id = DashboardRowId::TopLevel(crate::app::agent::AgentId(7));
    let row = DashboardRow {
        id: id.clone(),
        label: "investigate caching".to_string(),
        subtitle: None,
        state: RowState::Working,
        activity: Some("Responding".to_string()),
        // The 'R' in "Responding" lives at column 4 (matches
        // `render_row_two_line_layout_paints_title_and_secondary`),
        // so we sample fg at (4, 1).
        secondary_line: Some("Responding".to_string()),
        cwd_display: String::new(),
        cwd: PathBuf::from("/tmp"),
        last_change_at: SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };

    // Unselected → dim secondary.
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 2));
    let mut state_unselected = DashboardState::new();
    render_row(
        &mut buf,
        Rect::new(0, 0, 100, 2),
        &theme,
        &row,
        &mut state_unselected,
    );
    assert_eq!(
        buf[(4, 1)].fg,
        theme.gray_dim,
        "unselected row's secondary must paint in `gray_dim`",
    );

    // Selected → brighter secondary.
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 2));
    let mut state_selected = DashboardState::new();
    state_selected.focus_row(id);
    render_row(
        &mut buf,
        Rect::new(0, 0, 100, 2),
        &theme,
        &row,
        &mut state_selected,
    );
    assert_eq!(
        buf[(4, 1)].fg,
        theme.text_secondary,
        "selected row's secondary must brighten to `text_secondary` \
         so the response line is readable",
    );
}

/// A `NeedsInput` row: the bullet is yellow (and blinks to a dimmer
/// yellow), the `[needs input]` badge is suppressed, and the `Pending:`
/// subtitle prefix is painted yellow.
#[test]
fn render_row_needs_input_yellow_blink_no_badge_pending_prefix() {
    use std::path::PathBuf;
    use std::time::SystemTime;
    let theme = Theme::current();
    let make_row = || DashboardRow {
        id: DashboardRowId::TopLevel(crate::app::agent::AgentId(1)),
        label: "ask me".to_string(),
        subtitle: None,
        state: RowState::NeedsInput,
        activity: None,
        secondary_line: Some("Pending: plan approval".to_string()),
        cwd_display: String::new(),
        cwd: PathBuf::from("/tmp"),
        last_change_at: SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: vec![RowBadge::NeedsInput],
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };
    let render = |tick: u64| {
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 2));
        let mut state = DashboardState::new();
        state.spinner_tick = tick;
        render_row(
            &mut buf,
            Rect::new(0, 0, 100, 2),
            &theme,
            &make_row(),
            &mut state,
        );
        buf
    };

    // Bright phase (tick 0): the bullet is full yellow.
    let bright = render(0);
    assert_eq!(
        bright[(2, 0)].symbol(),
        crate::glyphs::diamond_filled(),
        "bullet glyph"
    );
    assert_eq!(
        bright[(2, 0)].fg,
        theme.warning,
        "bright needs-input bullet must be yellow (warning)",
    );

    // No `[needs input]` badge on the title row.
    let mut title = String::new();
    for x in 0..bright.area.width {
        title.push_str(bright[(x, 0)].symbol());
    }
    assert!(
        !title.contains("needs input"),
        "needs-input badge must be hidden, got: {title:?}",
    );

    // `Pending:` subtitle prefix is painted yellow (the rest of the
    // subtitle is painted separately in the dim secondary colour).
    assert_eq!(
        bright[(4, 1)].symbol(),
        "P",
        "secondary starts with `Pending:`"
    );
    assert_eq!(
        bright[(4, 1)].fg,
        theme.warning,
        "`Pending:` prefix must be yellow",
    );

    // The dim blink phase fades the bullet (only assertable when the
    // theme supports blending; non-truecolor falls back to full yellow).
    if crate::render::color::blend_color(theme.bg_base, theme.warning, 0.5).is_some() {
        let dim = render(NEEDS_INPUT_BLINK_DIVISOR);
        assert_ne!(
            dim[(2, 0)].fg,
            theme.warning,
            "dim blink phase must fade the bullet away from full yellow",
        );
    }
}

/// The `New session #<id>` fallback title is painted two-tone: the
/// `New session` head in the primary colour and the ` #id` suffix dim.
#[test]
fn render_row_new_session_fallback_label_is_two_tone() {
    use std::path::PathBuf;
    use std::time::SystemTime;
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, 2));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let row = DashboardRow {
        id: DashboardRowId::TopLevel(crate::app::agent::AgentId(1)),
        label: "New session #abc12345".to_string(),
        subtitle: None,
        state: RowState::Idle,
        activity: None,
        secondary_line: None,
        cwd_display: String::new(),
        cwd: PathBuf::from("/tmp"),
        last_change_at: SystemTime::now(),
        pinned: false,
        is_active: false,
        badges: Vec::new(),
        context_pct: None,
        indent: 0,
        parent_label: None,
        is_more_placeholder: false,
        more_count: 0,
    };
    render_row(&mut buf, Rect::new(0, 0, 100, 2), &theme, &row, &mut state);

    // Title starts at col 4: "New session" (11 chars, cols 4..15) then
    // " #abc12345" (suffix from col 15).
    assert_eq!(
        buf[(4, 0)].symbol(),
        "N",
        "title head starts with `New session`"
    );
    assert_eq!(
        buf[(4, 0)].fg,
        theme.text_primary,
        "`New session` head must use the primary colour",
    );
    // The `#` of the suffix sits at col 16 and must be dim.
    assert_eq!(buf[(16, 0)].symbol(), "#", "suffix must start with `#`");
    assert_eq!(
        buf[(16, 0)].fg,
        theme.gray_dim,
        "the `#id` suffix must be dim gray",
    );
}

/// Group header (section title) leads with a disclosure glyph at
/// col 0, then the label at col 2, within the list area. Row content
/// below is indented (marker col 0, gap col 1, icon col 2). The
/// header is 2 visual cells tall (label + gap) and the title-only
/// row centers its title, so the title sits 3 rows below the
/// header in this fixture.
#[test]
fn render_group_header_leads_with_disclosure_glyph() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 8));
    let mut state = DashboardState::new();
    let rows = vec![header_test_row(1, RowState::Idle, "session 019e5d9f")];
    let theme = Theme::current();
    render_rows(&mut buf, Rect::new(0, 0, 80, 8), &theme, &rows, &mut state);

    // Col 0 is the (expanded) disclosure glyph; the label starts at
    // col 2 (glyph + a space). Rows below have their marker/icon in
    // the left columns and text indented.
    assert_eq!(
        buf[(0, 0)].symbol(),
        crate::glyphs::disclosure_open(),
        "section header must lead with the expanded disclosure glyph",
    );
    let header_label_x = buf[(2, 0)].symbol().to_string();
    assert_eq!(
        header_label_x, "I",
        "section title `Idle …` must start after the disclosure glyph, got: {header_label_x:?}",
    );

    // Header gap → row 1 is blank. The title-only row centers its
    // title within its 3-cell rect (y=2..5), so the title sits at
    // y=3. Rows still render their marker/icon in the left chrome
    // columns.
    let row_col0 = buf[(0, 3)].symbol().to_string();
    let row_col1 = buf[(1, 3)].symbol().to_string();
    let row_col2 = buf[(2, 3)].symbol().to_string();
    assert_eq!(
        row_col0, " ",
        "row's col 0 must be the marker space when nothing selected, got: {row_col0:?}",
    );
    assert_eq!(
        row_col1, " ",
        "row's col 1 must be the 1-col gap after the marker, got: {row_col1:?}",
    );
    assert_eq!(
        row_col2,
        crate::glyphs::diamond_hollow(),
        "row's col 2 must be the hollow diamond for Idle, got: {row_col2:?}",
    );
}

/// `Grouping::Directory` keeps cwd as the
/// grouping primitive, so state headers are suppressed.
///
/// Header chrome marker updated to match Option
/// A. The `(count)` parenthesis pattern is the new specific
/// fingerprint for a state header.
#[test]
fn render_rows_skips_headers_when_grouping_is_directory() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let mut state = DashboardState::new();
    state.grouping = Grouping::Directory;
    let rows = vec![
        header_test_row(1, RowState::Working, "a"),
        header_test_row(2, RowState::Idle, "b"),
    ];
    let theme = Theme::current();
    render_rows(&mut buf, Rect::new(0, 0, 80, 10), &theme, &rows, &mut state);
    let content = buf_to_text(&buf);
    assert!(
        !content.contains("Working ("),
        "Directory grouping must suppress Working header, got: {content:?}",
    );
    assert!(
        !content.contains("Idle ("),
        "Directory grouping must suppress Idle header, got: {content:?}",
    );
    // Rows themselves still render.
    assert!(
        content.contains('a') && content.contains('b'),
        "rows must still render under Directory grouping, got: {content:?}",
    );
}

/// `Filter::State(_)` collapses the view to a
/// single state, so the header would be redundant chrome.
///
/// Header chrome marker updated to match Option
/// A (look for `Working (` instead of `── Working`).
#[test]
fn render_rows_skips_headers_when_filter_is_state() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let mut state = DashboardState::new();
    state.filter = Filter::State(RowState::Working);
    let rows = vec![
        header_test_row(1, RowState::Working, "first working"),
        header_test_row(2, RowState::Working, "second working"),
    ];
    let theme = Theme::current();
    render_rows(&mut buf, Rect::new(0, 0, 80, 10), &theme, &rows, &mut state);
    let content = buf_to_text(&buf);
    assert!(
        !content.contains("Working ("),
        "state-filtered view must suppress Working header, got: {content:?}",
    );
    // Rows themselves still render.
    assert!(
        content.contains("first working"),
        "first row must render, got: {content:?}",
    );
    assert!(
        content.contains("second working"),
        "second row must render, got: {content:?}",
    );
}

/// Subagent rows (indent > 0) must NOT trigger
/// their own state header; they inherit their parent's group.
/// Test: a parent in `Working` followed by a finished
/// (`Completed`) subagent + a finished (`Failed`) subagent must
/// emit only the parent's `Working` header, not extra
/// `Completed` / `Failed` ones tied to the subagents.
#[test]
fn render_rows_subagents_do_not_trigger_their_own_headers() {
    use crate::app::agent::AgentId;
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
    let mut state = DashboardState::new();
    let parent = DashboardRow {
        id: DashboardRowId::TopLevel(AgentId(1)),
        indent: 0,
        ..header_test_row(1, RowState::Working, "parent")
    };
    let sub_completed = DashboardRow {
        id: DashboardRowId::Subagent {
            parent: AgentId(1),
            child_session_id: "c1".to_string(),
        },
        label: "sub-completed".to_string(),
        indent: 1,
        ..header_test_row(11, RowState::Completed, "sub-completed")
    };
    let sub_failed = DashboardRow {
        id: DashboardRowId::Subagent {
            parent: AgentId(1),
            child_session_id: "c2".to_string(),
        },
        label: "sub-failed".to_string(),
        indent: 1,
        ..header_test_row(12, RowState::Failed, "sub-failed")
    };
    let rows = vec![parent, sub_completed, sub_failed];
    let theme = Theme::current();
    render_rows(&mut buf, Rect::new(0, 0, 80, 20), &theme, &rows, &mut state);
    let content = buf_to_text(&buf);
    // Group header reads `Working 1 ─────` (no
    // parens; trailing rule fills the rest of the row).
    assert!(
        content.contains("Working 1"),
        "parent's Working header must render, got: {content:?}",
    );
    // Subagents inherit their parent's group and must NOT emit
    // their own headers. The trailing `\u{2500}` rule is the
    // marker that distinguishes a header from a row.
    assert!(
        !content.contains("Completed 1"),
        "subagent must NOT trigger a Completed header, got: {content:?}",
    );
    assert!(
        !content.contains("Failed 1"),
        "subagent must NOT trigger a Failed header, got: {content:?}",
    );
}

/// Narrow mode emits a compact `Done 12` header
/// (no bullet, no parens, no trailing rule — narrow terminals
/// don't have the width budget).
#[test]
fn render_narrow_rows_emits_compact_group_headers() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    let mut state = DashboardState::new();
    let rows = vec![
        header_test_row(1, RowState::Working, "wrk"),
        header_test_row(2, RowState::Idle, "idl"),
    ];
    let theme = Theme::current();
    render_narrow_rows(&mut buf, Rect::new(0, 0, 30, 10), &theme, &rows, &mut state);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Working 1"),
        "narrow header `Working 1` missing, got: {content:?}",
    );
    assert!(
        content.contains("Idle 1"),
        "narrow header `Idle 1` missing, got: {content:?}",
    );
    // Narrow mode must NOT paint the wide trailing rule.
    assert!(
        !content.contains("\u{2500}\u{2500}\u{2500}"),
        "narrow header must not paint a trailing rule, got: {content:?}",
    );
}

/// Narrow-layout regression — the viewport clamp must follow a
/// selected *section header*, not just a selected row. With the new
/// section cursor a header can become the keyboard target; if the
/// clamp only tracks `state.selected` (a row) that header stays
/// off-screen even though the wide layout scrolls it in. Here the
/// second group's "Idle" header lands below a 5-line viewport, so it
/// is off-screen at offset 0 and must be scrolled in once selected.
#[test]
fn render_narrow_viewport_follows_selected_section_header() {
    let mut rows = Vec::new();
    for i in 0..6 {
        rows.push(header_test_row(i + 1, RowState::Working, "wrk"));
    }
    rows.push(header_test_row(99, RowState::Idle, "idl"));
    let theme = Theme::current();
    let area = Rect::new(0, 0, 30, 5);

    // Control — nothing selected: the Idle header starts off-screen.
    {
        let mut buf = Buffer::empty(area);
        let mut state = DashboardState::new();
        render_narrow_rows(&mut buf, area, &theme, &rows, &mut state);
        let content = buf_to_text(&buf);
        assert!(
            !content.contains("Idle"),
            "fixture invalid — Idle header must start off-screen, got: {content:?}",
        );
    }

    // Selecting the Idle section must scroll its header into view.
    {
        let mut buf = Buffer::empty(area);
        let mut state = DashboardState::new();
        state.selected_section = Some(SectionKey::State(RowState::Idle));
        render_narrow_rows(&mut buf, area, &theme, &rows, &mut state);
        let content = buf_to_text(&buf);
        assert!(
            content.contains("Idle 1"),
            "selected Idle section header must be clamped into view, got: {content:?}",
        );
        assert!(
            state.viewport_offset > 0,
            "viewport must scroll to reveal the selected section header, got offset {}",
            state.viewport_offset,
        );
    }
}

/// Alt-screen polish — `render_dashboard` must paint the theme's
/// base background across the entire `area` before any
/// sub-renderer runs. Without this, cells untouched by the
/// header/list/dispatch/footer renderers keep stale paint from
/// the previous frame and the dashboard looks like it doesn't
/// cover the full panel.
///
/// The check pins a cell that no sub-renderer touches — the
/// trailing whitespace one column past the right edge of the
/// list — and asserts that its background matches
/// `theme.bg_base`. Pre-seed the buffer with a contrasting bg so
/// a regression that drops the fill is visible (otherwise the
/// default-empty buffer would already show the right colour).
#[test]
fn render_dashboard_paints_full_area_background() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 80, 20);
    let mut buf = Buffer::empty(area);
    // Seed every cell with a contrasting bg so a missing fill is
    // detectable: any cell still carrying this seed colour after
    // `render_dashboard` runs means the fill didn't reach it.
    let seed = ratatui::style::Color::Rgb(0xFF, 0x00, 0xFF);
    buf.set_style(area, Style::default().bg(seed));

    let mut agents: IndexMap<AgentId, AgentView> = IndexMap::new();
    let mut state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    let _ = render_dashboard(
        &mut buf,
        area,
        &mut state,
        &mut agents,
        &registry,
        None,
        &[],
        false,
        None,
    );

    // Sample cells across the area; none should retain the seed
    // bg colour. The dashboard fills with `theme.bg_base`; the
    // exact colour need not match a constant — we only assert
    // the seed is gone.
    for y in 0..area.height {
        for x in 0..area.width {
            let cell_bg = buf[(x, y)].bg;
            assert_ne!(
                cell_bg, seed,
                "cell at ({x}, {y}) still carries the seed bg — render_dashboard must fill the entire area",
            );
        }
    }
    // And spot-check that at least one cell matches the theme bg,
    // i.e., the fill actually used `theme.bg_base` (not just any
    // non-seed colour).
    let mut saw_bg_base = false;
    for y in 0..area.height {
        for x in 0..area.width {
            if buf[(x, y)].bg == theme.bg_base {
                saw_bg_base = true;
                break;
            }
        }
        if saw_bg_base {
            break;
        }
    }
    assert!(
        saw_bg_base,
        "render_dashboard must paint at least one cell with theme.bg_base",
    );
}

// ─────────────────────────────────────────────────────────────────
// Header redesign tests
// ─────────────────────────────────────────────────────────────────

/// Basename of the test process's cwd — the one deterministic
/// fragment of the header's location label. The full label depends
/// on global git caches (`git_info::*`) that parallel tests may
/// touch, but every fallback path renders a cwd display ending in
/// the current directory's basename.
fn cwd_basename() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .expect("test process must have a cwd with a basename")
}

/// The header pairs the location label (cwd + git info) on the
/// left with right-aligned state-count chips.
#[test]
fn render_header_paints_label_and_state_chips() {
    let theme = Theme::current();
    // Wide rect so the location label never truncates regardless
    // of how deep the test machine's checkout path is.
    let area = Rect::new(0, 0, 400, 1);
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    let rows = vec![
        header_test_row(1, RowState::NeedsInput, "a"),
        header_test_row(2, RowState::NeedsInput, "b"),
        header_test_row(3, RowState::Working, "c"),
        header_test_row(4, RowState::Idle, "d"),
    ];
    render_header(&mut buf, area, &theme, &rows, &mut state, None);
    let content = buf_to_text(&buf);
    let basename = cwd_basename();
    assert!(
        content.contains(&basename),
        "header must show the current location (`{basename}`), got: {content:?}",
    );
    for chip in ["2 awaiting", "1 working", "1 idle"] {
        assert!(
            content.contains(chip),
            "header missing chip `{chip}`, got: {content:?}",
        );
    }
    // The old static label and total count are gone.
    assert!(
        !content.contains("Agents"),
        "header must not paint the old `Agents` label, got: {content:?}",
    );
    assert!(
        !content.contains("4 agents"),
        "total must not appear as right-side chip, got: {content:?}",
    );
    // The button form is gone.
    assert!(
        !content.contains("[New agent +]"),
        "v2-round-2 header must not paint the `[New agent +]` button anymore, got: {content:?}",
    );
}

/// The header records a click target for the location label so the
/// mouse handler can open the location picker.
#[test]
fn render_header_sets_location_click_target() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 120, 1);
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    render_header(&mut buf, area, &theme, &[], &mut state, None);
    assert!(
        state.location_hit.rect.is_some(),
        "render_header must record a click target for the location label",
    );
}

/// On hover the location label underlines only its text — the leading
/// inset space (and other whitespace padding) stays un-underlined.
#[test]
fn render_header_hover_underlines_only_text() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 400, 1);
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    state.location_hit.hovered = true;
    render_header(&mut buf, area, &theme, &[], &mut state, None);

    // Leading inset (x=0) is a space → must NOT be underlined.
    let inset = buf.cell((0, 0)).expect("inset cell");
    assert_eq!(inset.symbol(), " ", "x=0 is the leading inset space");
    assert!(
        !inset.style().add_modifier.contains(Modifier::UNDERLINED),
        "the leading inset space must not be underlined",
    );

    // Within the location label (bounded by its recorded hit rect, so
    // the right-side chips are excluded), the last visible glyph is cwd
    // path text — never the branch icon — so it must be underlined.
    // Bounding to the label keeps this robust whether or not a branch is
    // present for the test's cwd.
    let label_end = state
        .location_hit
        .rect
        .expect("header records the location hit rect")
        .width;
    let last_text_x = (1..label_end)
        .rev()
        .find(|&x| {
            buf.cell((x, 0))
                .is_some_and(|c| !c.symbol().trim().is_empty())
        })
        .expect("header must paint visible location text");
    assert!(
        buf.cell((last_text_x, 0))
            .unwrap()
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED),
        "the location path text must be underlined on hover",
    );
}

/// The git span (`{icon} {branch}`) underlines only the branch *name*:
/// the branch icon and the space after it stay bare. Whitespace-only
/// spans (inset, separator) stay bare; the path is underlined.
#[test]
fn underline_location_on_hover_excludes_branch_icon() {
    let icon = "\u{e0a0}";
    let plain = Style::default();
    let spans = vec![
        Span::styled(" ".to_string(), plain),        // leading inset
        Span::styled(format!("{icon} main"), plain), // git: icon + branch
        Span::styled(" ".to_string(), plain),        // git↔path separator
        Span::styled("/home/me/repo".to_string(), plain), // path
    ];
    let out = underline_location_on_hover(spans, icon);

    let underlined = |s: &Span<'static>| s.style.add_modifier.contains(Modifier::UNDERLINED);

    // The git span split into `{icon} ` (bare) + `main` (underlined).
    let icon_part = out
        .iter()
        .find(|s| s.content.starts_with(icon))
        .expect("icon span present");
    assert_eq!(icon_part.content.as_ref(), format!("{icon} "));
    assert!(
        !underlined(icon_part),
        "the branch icon and the space after it must not be underlined",
    );
    let branch = out
        .iter()
        .find(|s| &*s.content == "main")
        .expect("branch span present");
    assert!(underlined(branch), "the branch name must be underlined");

    // Whitespace-only spans stay bare.
    for s in &out {
        if s.content.chars().all(char::is_whitespace) {
            assert!(
                !underlined(s),
                "whitespace span must not be underlined: {:?}",
                s.content,
            );
        }
    }
    // The path is underlined.
    let path = out
        .iter()
        .find(|s| &*s.content == "/home/me/repo")
        .expect("path span present");
    assert!(underlined(path), "the path must be underlined");
}

/// The location picker modal paints its title + candidate rows and
/// records the content hit areas for mouse handling.
#[test]
fn render_location_picker_shows_candidates() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut modal = LocationPickerState::new(
        vec![crate::views::dashboard::LocationCandidate {
            path: std::path::PathBuf::from("/home/me/frontend"),
            label: "frontend".to_string(),
            detail: "~/me/frontend".to_string(),
            worktree: None,
        }],
        std::path::PathBuf::from("/base"),
        std::collections::HashMap::new(),
    );
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Change directory"),
        "modal title missing, got: {content:?}",
    );
    assert!(
        content.contains("path:"),
        "path input field missing, got: {content:?}",
    );
    assert!(
        content.contains("frontend"),
        "candidate label missing, got: {content:?}",
    );
    assert!(
        modal.content_hits.is_some(),
        "content hit areas must be recorded for the mouse handler",
    );
}

/// In a git repo the path row paints a worktree toggle reflecting the
/// modal's `worktree_mode`, and records its hit rect for click handling.
#[test]
fn render_location_picker_shows_worktree_toggle_in_repo() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 80, 24);
    // A temp dir with a `.git` child so the toggle is eligible (hermetic,
    // unlike depending on the test's real cwd being a repo).
    let repo = std::env::temp_dir().join("echo-agent-loc-wt-toggle-repo-test");
    std::fs::create_dir_all(repo.join(".git")).expect("mk .git");
    let mut modal =
        LocationPickerState::new(vec![], repo.clone(), std::collections::HashMap::new());

    // Off by default.
    let mut buf = Buffer::empty(area);
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("worktree:off"),
        "off-state button missing, got: {content:?}",
    );
    assert!(
        modal.worktree_hit.rect.is_some(),
        "the worktree button hit rect must be recorded",
    );

    // On after toggling.
    modal.worktree_mode = true;
    let mut buf = Buffer::empty(area);
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("worktree:on"),
        "on-state button missing, got: {content:?}",
    );
    // The "on" word is recolored green (accent_success).
    let hit = modal.worktree_hit.rect.expect("hit rect recorded");
    let on_x = hit.x + "[worktree:".len() as u16;
    let cell = buf.cell((on_x, hit.y)).expect("on cell");
    assert_eq!(cell.symbol(), "o", "expected the 'o' of \"on\"");
    assert_eq!(
        cell.style().fg,
        Some(theme.accent_success),
        "the \"on\" word must be green",
    );

    // Hovering brightens the label text (higher-contrast fg), like other
    // clickable buttons.
    modal.worktree_hit.hovered = true;
    let mut buf = Buffer::empty(area);
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let cell = buf.cell((hit.x, hit.y)).expect("button cell");
    assert_eq!(
        cell.style().fg,
        Some(theme.text_primary),
        "hovered button must brighten the label text",
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// Outside a git repo the worktree toggle is hidden (and no hit rect is
/// recorded) so dispatch proceeds as a normal session.
#[test]
fn render_location_picker_hides_worktree_toggle_outside_repo() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 80, 24);
    let mut modal = LocationPickerState::new(
        vec![],
        std::path::PathBuf::from("/echo-agent-not-a-repo-xyz-12345"),
        std::collections::HashMap::new(),
    );
    let mut buf = Buffer::empty(area);
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let content = buf_to_text(&buf);
    assert!(
        !content.contains("worktree"),
        "the worktree toggle must be hidden outside a repo, got: {content:?}",
    );
    assert!(
        modal.worktree_hit.rect.is_none(),
        "no hit rect when the toggle is hidden",
    );
}

/// Worktree directories render a styled `worktree: <name>` badge.
#[test]
fn render_location_picker_tags_worktree() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut modal = LocationPickerState::new(
        vec![crate::views::dashboard::LocationCandidate {
            path: std::path::PathBuf::from("/home/me/wt"),
            label: "wt".to_string(),
            detail: "~/me/wt".to_string(),
            worktree: Some("my-feature".to_string()),
        }],
        std::path::PathBuf::from("/base"),
        std::collections::HashMap::new(),
    );
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("worktree: my-feature"),
        "worktree badge missing, got: {content:?}",
    );
}

/// Truncation priority: the directory name (label) is shown in full
/// and the path (right label) is truncated first.
#[test]
fn render_location_picker_truncates_path_not_label() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 54, 20);
    let mut buf = Buffer::empty(area);
    let mut modal = LocationPickerState::new(
        vec![crate::views::dashboard::LocationCandidate {
            path: std::path::PathBuf::from("/home/me/myproj"),
            label: "myproj".to_string(),
            detail: "~/very/long/path/that/keeps/going/UNIQUETAIL".to_string(),
            worktree: None,
        }],
        std::path::PathBuf::from("/base"),
        std::collections::HashMap::new(),
    );
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("myproj"),
        "full label must be shown, got: {content:?}",
    );
    assert!(
        !content.contains("UNIQUETAIL"),
        "long path must be truncated, got: {content:?}",
    );
}

/// The path input echoes what the user types, even when it matches
/// no candidate (the list then shows "No matches").
#[test]
fn render_location_picker_echoes_typed_path() {
    let theme = Theme::current();
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let mut modal = LocationPickerState::new(
        vec![crate::views::dashboard::LocationCandidate {
            path: std::path::PathBuf::from("/home/me/frontend"),
            label: "frontend".to_string(),
            detail: "~/me/frontend".to_string(),
            worktree: None,
        }],
        std::path::PathBuf::from("/base"),
        std::collections::HashMap::new(),
    );
    modal.picker.set_query("/tmp/zzz");
    render_location_picker(&mut buf, area, &theme, &mut modal);
    let content = buf_to_text(&buf);
    assert!(
        content.contains("/tmp/zzz"),
        "typed path must echo in the input field, got: {content:?}",
    );
}

/// Zero-count states are suppressed.
#[test]
fn render_header_suppresses_zero_count_chips() {
    let theme = Theme::current();
    let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
    let mut state = DashboardState::new();
    // Only one Idle row — no awaiting/working/done/failed chips.
    let rows = vec![header_test_row(1, RowState::Idle, "x")];
    render_header(
        &mut buf,
        Rect::new(0, 0, 120, 1),
        &theme,
        &rows,
        &mut state,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains("1 idle"),
        "expected `1 idle`, got: {content:?}"
    );
    for absent in ["0 awaiting", "0 working", "0 done", "0 failed", "0 blocked"] {
        assert!(
            !content.contains(absent),
            "zero-count chip `{absent}` must be suppressed, got: {content:?}",
        );
    }
}

/// Inactive (roster-only) rows get no header chip — only the
/// section header carries their count.
#[test]
fn render_header_has_no_inactive_chip() {
    let theme = Theme::current();
    let mut buf = Buffer::empty(Rect::new(0, 0, 120, 1));
    let mut state = DashboardState::new();
    let rows = vec![
        header_test_row(1, RowState::Inactive, "a"),
        header_test_row(2, RowState::Idle, "b"),
    ];
    render_header(
        &mut buf,
        Rect::new(0, 0, 120, 1),
        &theme,
        &rows,
        &mut state,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains("1 idle"),
        "idle chip must still render, got: {content:?}"
    );
    assert!(
        !content.contains("inactive"),
        "no chip for Inactive rows, got: {content:?}"
    );
}

/// The left title is the current location (cwd display) — shown
/// with and without agent rows, mirroring the session views'
/// top-bar location line.
#[test]
fn render_header_shows_location_label() {
    let theme = Theme::current();
    // Wide rect so the location label never truncates regardless
    // of how deep the test machine's checkout path is.
    let area = Rect::new(0, 0, 400, 1);
    let mut state = DashboardState::new();
    let basename = cwd_basename();

    // 0 agents — the location still shows.
    let mut buf = Buffer::empty(area);
    render_header(&mut buf, area, &theme, &[], &mut state, None);
    let c = buf_to_text(&buf);
    assert!(
        c.contains(&basename),
        "0-agent header must show the location (`{basename}`), got: {c:?}"
    );

    // 1 agent.
    let mut buf = Buffer::empty(area);
    let rows = vec![header_test_row(1, RowState::Idle, "x")];
    render_header(&mut buf, area, &theme, &rows, &mut state, None);
    let c = buf_to_text(&buf);
    assert!(
        c.contains(&basename),
        "header must show the location (`{basename}`), got: {c:?}"
    );
}

/// On a narrow header the location label truncates against the
/// leftmost chip's separator instead of painting over the chips
/// or the `[+ New Agent]` button.
#[test]
fn render_header_location_label_never_overlaps_chips() {
    let theme = Theme::current();
    // Narrow enough that any realistic checkout path overflows the
    // label budget once three chips + the button are reserved.
    let area = Rect::new(0, 0, 70, 1);
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    let rows = vec![
        header_test_row(1, RowState::NeedsInput, "a"),
        header_test_row(2, RowState::Working, "b"),
        header_test_row(3, RowState::Idle, "c"),
    ];
    render_header(&mut buf, area, &theme, &rows, &mut state, None);
    let content = buf_to_text(&buf);
    // Chips and button must survive the (long) location label.
    for chunk in ["1 awaiting", "1 working", "1 idle", "[+ New Agent]"] {
        assert!(
            content.contains(chunk),
            "`{chunk}` must not be overpainted by the location label, got: {content:?}",
        );
    }
}

/// Footer chips use the shared `ShortcutsBar` styling
/// (`Key:label` separated by ` │ `).
#[test]
fn render_footer_uses_shared_shortcuts_bar_styling() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":create") || content.contains(":attach"),
        "footer must use `Key:label` chip format, got: {content:?}",
    );
    assert!(
        content.contains(" \u{2502} "),
        "footer must use ` │ ` separator, got: {content:?}",
    );
}

/// Fresh `DashboardState` defaults to button-focused
/// with an empty prompt. The footer surfaces `Enter:create`
/// (single primary action) and the trailing shortcuts chip. The
/// ↑/↓ nav chip is no longer shown (dropped to save space), and no
/// send / send+open chip is shown because there's nothing to send.
#[test]
fn render_footer_default_compact_hints() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    for chip in [":create", ":shortcuts"] {
        assert!(
            content.contains(chip),
            "footer must contain `{chip}` chip, got: {content:?}",
        );
    }
    // The nav chip is dropped from the bottom bar to save space.
    assert!(
        !content.contains(":nav"),
        "footer must NOT include the ↑/↓ nav chip, got: {content:?}",
    );
    // Empty prompt: no send / send+open chip (`:send` is a prefix
    // of `:send+open`, so one check covers both).
    assert!(
        !content.contains(":send"),
        "empty-prompt footer must NOT include send / send+open chips, \
         got: {content:?}",
    );
}

/// The dispatch input grows for multi-line drafts: a single-line
/// prompt wants 1 text row, a 3-line prompt (Shift/Alt+Enter
/// newlines) wants 3, and growth saturates at the cap so the box
/// never starves the row list.
#[test]
fn dispatch_text_rows_grows_with_newlines() {
    let mut state = DashboardState::new();
    let width = 80;
    let height = 30;
    state.dispatch.set_text("just one line");
    assert_eq!(
        dispatch_text_rows(&state, width, height),
        1,
        "single-line prompt wants 1 text row",
    );
    state.dispatch.set_text("line one\nline two\nline three");
    assert_eq!(
        dispatch_text_rows(&state, width, height),
        3,
        "3-line prompt wants 3 text rows",
    );
    // Past the cap ((height/3).clamp(1,8) = 8 here) growth saturates.
    state.dispatch.set_text(&"x\n".repeat(40));
    assert_eq!(
        dispatch_text_rows(&state, width, height),
        8,
        "dispatch text rows saturate at the cap",
    );
}

/// Overview list focused (via Tab) with vim on → the nav chip is
/// dropped from the bottom bar (to save space); neither the vim
/// `j/k` nor the arrow nav is advertised. The action chips (open)
/// remain.
#[test]
fn render_footer_list_focused_vim_on_omits_nav() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));
    state.list_focused = true;
    let registry = crate::actions::ActionRegistry::defaults();
    crate::appearance::cache::set_vim_mode(true);
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        false,
        None,
    );
    let content = buf_to_text(&buf);
    // Reset before asserting so a failure doesn't leak vim state
    // into the next test sharing this thread's cache.
    crate::appearance::cache::set_vim_mode(false);
    assert!(
        !content.contains(":nav") && !content.contains("j/k"),
        "list-focused footer must omit the nav chip, got: {content:?}",
    );
    assert!(
        content.contains(":open"),
        "list-focused footer keeps the open chip, got: {content:?}",
    );
}

/// Overview list focused with vim off → the nav chip is likewise
/// dropped (no arrow nav advertised), saving bottom-bar space for
/// the action chips.
#[test]
fn render_footer_list_focused_vim_off_omits_nav() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));
    state.list_focused = true;
    let registry = crate::actions::ActionRegistry::defaults();
    crate::appearance::cache::set_vim_mode(false);
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        !content.contains(":nav") && !content.contains('\u{2191}'),
        "list-focused footer must omit the nav chip, got: {content:?}",
    );
}

#[test]
fn render_footer_peek_mode_shows_peek_hints() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let state = DashboardState::new();
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        true,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":open") && content.contains(":New Agent"),
        "peek-mode footer must include open + New Agent (unselect) hints, got: {content:?}",
    );
    assert!(
        content.contains(":delete"),
        "peek-mode footer must keep the Ctrl+x delete chip, got: {content:?}",
    );
    // The nav chip is dropped to save bottom-bar space.
    assert!(
        !content.contains(":nav"),
        "peek-mode footer must NOT show the nav chip, got: {content:?}",
    );
    assert!(
        !content.contains(":switch"),
        "peek-mode footer must NOT use the old `switch` label, got: {content:?}",
    );
}

/// Peek footer flips to send affordances once the reply has text
/// and is focused: `enter:send · ctrl+s:send+open · esc:back`.
#[test]
fn render_footer_peek_with_reply_text_shows_send() {
    crate::appearance::cache::set_vim_mode(false);
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(crate::app::agent::AgentId(0)),
        crate::views::dashboard::peek::PeekFields {
            label: "label".into(),
            time_ago: String::new(),
            response_type: "Idle".into(),
            last_user_message: None,
            question: None,
            options: Vec::new(),
            request_id: None,
            reject_option: None,
        },
    ));
    state.peek.as_mut().unwrap().focused = true;
    state.peek_reply.set_text("hi there");
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        true, // peek_active
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":send"),
        "peek footer with a typed reply must show `send`, got: {content:?}",
    );
}

/// Vim + unfocused peek: Enter focuses the reply (`input`), not open/send.
#[test]
fn render_footer_vim_unfocused_peek_enter_shows_input() {
    crate::appearance::cache::set_vim_mode(true);
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.list_focused = true; // used to steal the footer before the peek fix
    state.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(crate::app::agent::AgentId(0)),
        crate::views::dashboard::peek::PeekFields {
            label: "label".into(),
            time_ago: String::new(),
            response_type: "Idle".into(),
            last_user_message: None,
            question: None,
            options: Vec::new(),
            request_id: None,
            reject_option: None,
        },
    ));
    assert!(!state.peek.as_ref().unwrap().focused);
    state.peek_reply.set_text("draft");
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        true,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":input"),
        "vim unfocused peek must label Enter as input, got: {content:?}",
    );
    assert!(
        content.contains(":open"),
        "vim unfocused peek must surface Right:open for attach, got: {content:?}",
    );
    assert!(
        content.contains(":back"),
        "non-empty draft must label Esc as back, got: {content:?}",
    );
    crate::appearance::cache::set_vim_mode(false);
}

/// Non-vim unfocused peek with a typed draft: Esc clears the draft first
/// (`back`), not New Agent.
#[test]
fn render_footer_peek_unfocused_with_draft_esc_is_back() {
    crate::appearance::cache::set_vim_mode(false);
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    let mut peek = crate::views::dashboard::peek::PeekPanelState::new(
        DashboardRowId::TopLevel(crate::app::agent::AgentId(0)),
        crate::views::dashboard::peek::PeekFields {
            label: "label".into(),
            time_ago: String::new(),
            response_type: "Idle".into(),
            last_user_message: None,
            question: None,
            options: Vec::new(),
            request_id: None,
            reject_option: None,
        },
    );
    peek.focused = false;
    state.peek = Some(peek);
    state.peek_reply.set_text("draft");
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        true,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":back"),
        "unfocused peek with draft must show Esc:back, got: {content:?}",
    );
    assert!(
        !content.contains("New Agent"),
        "must not label Esc as New Agent while a draft remains, got: {content:?}",
    );
}

/// A pending question is an ANSWER surface only when focused AND an
/// option is selected. Focused + selected → `enter:answer` (+ Tab `list`);
/// focused + no selection → `enter:open` + `1-9:select`; unfocused still
/// keeps `1-9:select` (digits work). None of the non-answer states show `answer`.
#[test]
fn render_footer_peek_question_focus_flips_answer_vs_open() {
    crate::appearance::cache::set_vim_mode(false);
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();
    let make_state = |focused: bool, selected: Option<usize>| {
        let mut state = DashboardState::new();
        let mut peek = crate::views::dashboard::peek::PeekPanelState::new(
            DashboardRowId::TopLevel(crate::app::agent::AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "NeedsInput".into(),
                last_user_message: None,
                question: Some("Allow?".into()),
                options: vec![
                    ("allow".into(), "Allow".into()),
                    ("deny".into(), "Deny".into()),
                ],
                request_id: None,
                reject_option: None,
            },
        );
        peek.focused = focused;
        peek.selected_option = selected;
        state.peek = Some(peek);
        state
    };
    let render = |state: &DashboardState| {
        let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
        render_footer(
            &mut buf,
            Rect::new(0, 0, 200, 1),
            &theme,
            state,
            &registry,
            None,
            true, // peek_active
            None,
        );
        buf_to_text(&buf)
    };

    // Focused + an option selected → answer surface (+ Tab `list`).
    let answering = render(&make_state(true, Some(0)));
    assert!(
        answering.contains(":answer"),
        "focused + selected footer must show `answer`, got: {answering:?}",
    );
    assert!(
        answering.contains(":list"),
        "answer footer must surface the Tab `list` hint, got: {answering:?}",
    );

    // Focused + nothing selected → navigation surface: open + `1-9 select`.
    let picking = render(&make_state(true, None));
    assert!(
        picking.contains(":open"),
        "focused + no selection footer must show `open`, got: {picking:?}",
    );
    assert!(
        picking.contains(":select"),
        "focused + no selection footer must surface the `1-9 select` hint, got: {picking:?}",
    );
    assert!(
        !picking.contains(":answer"),
        "no-selection footer must NOT show `answer`, got: {picking:?}",
    );

    // Unfocused → Enter opens; 1-9 select still shown (digits work).
    let unfocused = render(&make_state(false, None));
    assert!(
        unfocused.contains(":open"),
        "unfocused question footer must show `open`, got: {unfocused:?}",
    );
    assert!(
        unfocused.contains(":select"),
        "unfocused question footer must keep 1-9 select, got: {unfocused:?}",
    );
    assert!(
        !unfocused.contains(":answer"),
        "unfocused question footer must NOT show `answer`, got: {unfocused:?}",
    );

    // Vim unfocused + question: Enter:input, Right:open, still 1-9 select.
    crate::appearance::cache::set_vim_mode(true);
    let mut vim_q = make_state(false, None);
    // Rebuild under vim so focused defaults false.
    vim_q.peek = Some({
        let mut peek = crate::views::dashboard::peek::PeekPanelState::new(
            DashboardRowId::TopLevel(crate::app::agent::AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "NeedsInput".into(),
                last_user_message: None,
                question: Some("Allow?".into()),
                options: vec![
                    ("allow".into(), "Allow".into()),
                    ("deny".into(), "Deny".into()),
                ],
                request_id: None,
                reject_option: None,
            },
        );
        peek.focused = false;
        peek
    });
    let vim_unfocused = render(&vim_q);
    assert!(
        vim_unfocused.contains(":input"),
        "vim unfocused question must label Enter as input, got: {vim_unfocused:?}",
    );
    assert!(
        vim_unfocused.contains(":select"),
        "vim unfocused question must keep 1-9 select, got: {vim_unfocused:?}",
    );
    crate::appearance::cache::set_vim_mode(false);
}

/// When a row (NeedsInput or otherwise) is selected
/// with an empty prompt, the footer shows `Enter:open`.
/// The previous `see details` label is folded into the
/// unified "row selected → open" semantics — every row's
/// detail view is the answer surface for any user-input
/// state, including `NeedsInput`.
#[test]
fn render_footer_row_selected_empty_prompt_shows_enter_open() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::NeedsInput),
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":open"),
        "row-selected + empty prompt footer must hint `Enter:open`, got: {content:?}",
    );
    assert!(
        !content.contains(":send"),
        "empty-prompt footer must NOT include `:send` chip, got: {content:?}",
    );
    // Two-focus discoverability — mirrors the `[+ New Agent]`
    // button's empty-prompt chip: Tab hands focus to the list.
    assert!(
        content.contains(":list"),
        "row-selected + empty prompt footer must hint `Tab:list`, got: {content:?}",
    );
}

#[test]
fn render_footer_inactive_row_shows_delete() {
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();

    // Input focused, row selected.
    let mut state = DashboardState::new();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Inactive),
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":delete"),
        "list-focused idle-row footer must show the delete chip, got: {content:?}",
    );
    assert!(
        content.contains(":open"),
        "list-focused idle-row footer must show the open chip, got: {content:?}",
    );

    state.list_focused = true;
    let mut buf2 = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf2,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Inactive),
        false,
        None,
    );
    let content2 = buf_to_text(&buf2);
    assert!(content2.contains(":delete"), "{content2:?}");

    state.list_focused = false;
    let mut buf3 = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf3,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        false,
        None,
    );
    let content3 = buf_to_text(&buf3);
    assert!(content3.contains(":delete"), "{content3:?}");
}

#[test]
fn render_footer_stop_label_follows_state() {
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));

    // Working → `stop`.
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Working),
        false,
        None,
    );
    let working = buf_to_text(&buf);
    assert!(
        working.contains(":stop") && !working.contains(":close"),
        "Working agent footer must label Ctrl+x as `stop`, got: {working:?}",
    );

    // NeedsInput → `stop` (a paused-but-running turn — first Ctrl+x
    // cancels, mirroring `dispatch_dashboard_stop`).
    let mut buf_ni = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf_ni,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::NeedsInput),
        false,
        None,
    );
    let needs_input = buf_to_text(&buf_ni);
    assert!(
        needs_input.contains(":stop") && !needs_input.contains(":close"),
        "NeedsInput agent footer must label Ctrl+x as `stop`, got: {needs_input:?}",
    );

    // Idle → `delete`.
    let mut buf2 = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf2,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        false,
        None,
    );
    let idle = buf_to_text(&buf2);
    assert!(
        idle.contains(":delete") && !idle.contains(":stop"),
        "Idle agent footer must label Ctrl+x as `delete`, got: {idle:?}",
    );
}

/// When a section header is selected the footer shows the toggle
/// (Enter:collapse / :expand) and `Esc:New Agent`, and omits the
/// stop chip (no session under a header).
#[test]
fn render_footer_section_selected_shows_toggle_no_stop() {
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();

    // Expanded section → Enter collapses.
    let mut state = DashboardState::new();
    state.focus_section(SectionKey::State(RowState::Working));
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":collapse"),
        "expanded section footer must hint Enter:collapse, got: {content:?}",
    );
    assert!(
        content.contains("New Agent"),
        "section footer must hint Esc:New Agent, got: {content:?}",
    );
    assert!(
        !content.contains(":stop") && !content.contains(":close"),
        "section footer must NOT show the stop chip, got: {content:?}",
    );

    // Collapsed section → the toggle label flips to expand.
    state.set_section_collapsed(SectionKey::State(RowState::Working), true);
    let mut buf2 = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf2,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content2 = buf_to_text(&buf2);
    assert!(
        content2.contains(":expand"),
        "collapsed section footer must hint Enter:expand, got: {content2:?}",
    );
}

/// Section header selected while the LIST is focused (Tab) — the
/// footer shows the section's own hints (collapse / Tab:input)
/// instead of the generic row chips (`open` / `stop` would lie:
/// Enter toggles the section, and there's no session to stop).
#[test]
fn render_footer_list_focused_section_shows_toggle() {
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    state.focus_section(SectionKey::State(RowState::Working));
    state.list_focused = true;
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":collapse"),
        "list-focused section footer must hint Enter:collapse, got: {content:?}",
    );
    assert!(
        content.contains(":input"),
        "list-focused section footer must hint Tab:input, got: {content:?}",
    );
    assert!(
        !content.contains(":open") && !content.contains(":stop") && !content.contains(":close"),
        "list-focused section footer must NOT show open/stop chips, got: {content:?}",
    );
    assert!(
        content.contains(":shortcuts"),
        "list-focused section footer keeps the shortcuts chip, got: {content:?}",
    );
}

/// Section header selected + typed text in the (focused) dispatch
/// input — the draft dispatches a NEW agent (a section header is
/// never a reply target), so the footer flips to the dispatch
/// chips: send / send+open / mode. The collapse toggle is gone
/// (it only fires on an empty prompt).
#[test]
fn render_footer_section_selected_with_prompt_shows_dispatch_chips() {
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    state.focus_section(SectionKey::State(RowState::Working));
    state.dispatch.set_text("kick off a fresh session");
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":send") && content.contains(":send+open"),
        "section + typed prompt footer must hint send / send+open, got: {content:?}",
    );
    assert!(
        content.contains(":mode"),
        "section + typed prompt footer must hint Shift+Tab:mode, got: {content:?}",
    );
    assert!(
        !content.contains(":collapse") && !content.contains(":expand"),
        "section + typed prompt footer must NOT show the toggle chip, got: {content:?}",
    );
    assert!(
        !content.contains(":stop") && !content.contains(":close"),
        "section footer never shows the stop chip, got: {content:?}",
    );
}

/// Rename mode shows only save and cancel actions.
#[test]
fn render_footer_rename_shows_save_and_cancel() {
    use crate::app::agent::AgentId;
    let theme = Theme::current();
    let registry = crate::actions::ActionRegistry::defaults();
    let mut state = DashboardState::new();
    let id = DashboardRowId::TopLevel(AgentId(0));
    state.focus_row(id.clone());
    state.rename = Some(RenameDraft::new(id, ""));
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains(":save"),
        "rename footer must hint Enter:save, got: {content:?}",
    );
    assert!(
        content.contains(":cancel"),
        "rename footer must hint Esc:cancel, got: {content:?}",
    );
    assert!(
        !content.contains(":stop") && !content.contains(":close") && !content.contains(":open"),
        "rename footer must hide the normal nav/stop chips, got: {content:?}",
    );
}

/// When a row is selected AND the user has typed,
/// Enter sends (reply, stays on dashboard) and Ctrl+S
/// sends + opens detail. The footer surfaces both chips so
/// the chord is discoverable.
#[test]
fn render_footer_row_selected_with_prompt_shows_send_and_send_open() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(0)));
    state.dispatch.set_text("reply text");
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        Some(RowState::Idle),
        false,
        None,
    );
    let content = buf_to_text(&buf);
    for chip in [":send", ":send+open"] {
        assert!(
            content.contains(chip),
            "row-selected + non-empty footer must contain `{chip}`, got: {content:?}",
        );
    }
    assert!(
        !content.contains(":open  "),
        "send/send+open footer must NOT include the empty-prompt `:open` chip, \
         got: {content:?}",
    );
}

/// When the `[+ New Agent]` button is focused AND the
/// user has typed, Enter sends (stays on dashboard) and
/// Ctrl+S sends + opens detail. Stop chip is suppressed
/// because the button has no underlying session to close.
#[test]
fn render_footer_button_focused_with_prompt_shows_send_and_send_open_no_stop() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    // Default state: button focused. Plant typed text.
    assert!(state.new_agent_button_focused);
    state.dispatch.set_text("kick off a fresh session");
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    for chip in [":send", ":send+open"] {
        assert!(
            content.contains(chip),
            "button-focused + non-empty footer must contain `{chip}`, \
             got: {content:?}",
        );
    }
    assert!(
        content.contains("Enter:send"),
        "default compose: bare Enter is send, got: {content:?}",
    );
    assert!(
        !content.contains(":stop") && !content.contains(":close"),
        "button-focused footer must NOT show the stop chip (no row to close), \
         got: {content:?}",
    );
}

/// Multiline compose swaps the submit chord in the footer so it matches
/// the Enter ↔ Shift/Alt+Enter behavior (agent keybar parity).
#[test]
fn render_footer_multiline_mode_send_uses_shift_or_alt_enter() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.multiline_mode = true;
    state.dispatch.set_text("multi\nline draft");
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Shift+Enter:send") || content.contains("Alt+Enter:send"),
        "multiline footer must advertise Shift/Alt+Enter as send, got: {content:?}",
    );
    // Bare Enter:send would appear as "  Enter:send" (footer pad); the
    // modified chords contain the substring "Enter:send" so avoid that.
    assert!(
        !content.contains("  Enter:send"),
        "multiline footer must not claim bare Enter:send, got: {content:?}",
    );
    assert!(
        content.contains(":send+open"),
        "Ctrl+S send+open must remain, got: {content:?}",
    );
}

/// Empty draft under multiline: create is on the submit chord, not bare Enter.
#[test]
fn render_footer_multiline_empty_create_uses_shift_or_alt_enter() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.multiline_mode = true;
    assert!(state.new_agent_button_focused);
    assert!(state.dispatch.text().trim().is_empty());
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.contains("Shift+Enter:create") || content.contains("Alt+Enter:create"),
        "multiline empty footer must advertise Shift/Alt+Enter as create, got: {content:?}",
    );
    assert!(
        !content.contains("  Enter:create"),
        "multiline empty footer must not claim bare Enter:create, got: {content:?}",
    );
}

/// Delete-confirm armed while the input is focused routes through
/// `ShortcutsBar::with_pending` ("press Ctrl+x again to delete").
#[test]
fn render_footer_delete_confirm_uses_pending_hint() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.arm_delete(DashboardRowId::TopLevel(crate::app::agent::AgentId(1)));
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        content.to_lowercase().contains("press again"),
        "stop-confirm footer must say `press again`, got: {content:?}",
    );
    assert!(
        content.to_lowercase().contains("delete this session"),
        "delete-confirm footer must name the action, got: {content:?}",
    );
}

/// An EXPIRED delete-confirm (older than `CONFIRM_WINDOW`) must
/// not claim the footer — the dispatcher would re-arm rather than
/// delete on the next press, so "press again" would lie. Regular
/// hints render instead (e.g. after a mouse click moved the
/// selection without a keypress to disarm the confirm).
#[test]
fn render_footer_expired_delete_confirm_shows_regular_hints() {
    use std::time::{Duration, Instant};
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 1));
    let theme = Theme::current();
    let mut state = DashboardState::new();
    state.focus_row(DashboardRowId::TopLevel(crate::app::agent::AgentId(1)));
    state.delete_confirm = Some((
        DashboardRowId::TopLevel(crate::app::agent::AgentId(1)),
        Instant::now() - (super::super::state::CONFIRM_WINDOW + Duration::from_secs(1)),
    ));
    let registry = crate::actions::ActionRegistry::defaults();
    render_footer(
        &mut buf,
        Rect::new(0, 0, 200, 1),
        &theme,
        &state,
        &registry,
        None,
        false,
        None,
    );
    let content = buf_to_text(&buf);
    assert!(
        !content.to_lowercase().contains("press again"),
        "expired stop-confirm must not paint the pending hint, got: {content:?}",
    );
    assert!(
        content.contains(":open"),
        "expired stop-confirm must fall back to the regular hints, got: {content:?}",
    );
}

/// Subagents inherit their parent's
/// state and must NOT inflate the header chip tallies. The
/// header counts top-level rows only.
#[test]
fn render_header_counts_top_level_rows_only() {
    let theme = Theme::current();
    let mut buf = Buffer::empty(Rect::new(0, 0, 160, 1));
    let mut state = DashboardState::new();
    let parent = DashboardRow {
        indent: 0,
        ..header_test_row(1, RowState::Working, "parent")
    };
    let sub_completed = DashboardRow {
        id: DashboardRowId::Subagent {
            parent: crate::app::agent::AgentId(1),
            child_session_id: "c1".to_string(),
        },
        indent: 1,
        ..header_test_row(11, RowState::Completed, "child")
    };
    let rows = vec![parent, sub_completed];
    render_header(
        &mut buf,
        Rect::new(0, 0, 160, 1),
        &theme,
        &rows,
        &mut state,
        None,
    );
    let content = buf_to_text(&buf);
    // Only the top-level parent counts: its Working chip shows.
    assert!(
        content.contains("1 working"),
        "expected `1 working` chip for the top-level parent, got: {content:?}"
    );
    // Subagent's Completed must NOT show up as `1 done`.
    assert!(
        !content.contains("1 done"),
        "header must not count subagent state, got: {content:?}",
    );
}
