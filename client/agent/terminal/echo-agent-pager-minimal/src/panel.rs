//! Minimal-mode below-prompt **list panels**: `/resume` (session picker) and
//! `/mcps` (MCP server status), rendered as simple lists *below the input bar*
//! instead of centered modal windows (design nit: "the mcps / resume lists
//! should not be in a modal").
//!
//! ## Why this is a render-only change
//!
//! Input routing is unchanged — the existing `handle_modal_key`
//! (`ActiveModal::SessionPicker`) and `handle_extensions_modal_key`
//! (`extensions_modal`) own navigation and close-on-Esc. Two different coupling
//! contracts are honored here:
//!
//! * **Session picker** rebuilds its entry map from data on every keypress
//!   (render-independent), so we just reuse the *same* builders
//!   ([`build_grouped_picker_entries`]) — the rendered order then matches the
//!   handler's `selected`.
//! * **Extensions modal** reads render-stored state (`entry_data_indices`,
//!   `entry_group_keys`, `entry_non_selectable*`). The MCP renderer repopulates
//!   those exactly as the full modal does (via the shared
//!   [`build_mcp_servers_picker_rows`]), so keyboard nav + section fold stay in
//!   sync without touching the input handler.
//!
//! Both reuse [`picker::render_picker_content`] for the rows, so row look +
//! selection highlight match the full TUI; only the modal-window chrome (border,
//! tabs, footer bar) is dropped.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use echo_agent_pager::app::agent_view::AgentView;
use echo_agent_pager::minimal_api;
use echo_agent_pager::theme::Theme;
use echo_agent_pager::views::extensions_modal::{ExtensionsTab, TabDataState};
use echo_agent_pager::views::modal::ActiveModal;
use echo_agent_pager::views::picker::{self, PickerEntry, PickerField, PickerHitAreas, PickerRow};

/// Rows of chrome around the scrolling list: title + subtitle/search + divider
/// + footer.
const CHROME_ROWS: u16 = 4;

/// Which below-prompt list panel is active for the focused agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListPanel {
    /// `/resume` session picker (`ActiveModal::SessionPicker`).
    Resume,
    /// `/mcps` MCP server status (extensions modal on the McpServers tab).
    Mcps,
}

/// Detect an active below-prompt list panel, or `None`.
///
/// Only the session picker and the MCP-servers tab are hosted as simple lists;
/// every other modal keeps its existing (centered) rendering. Callers must check
/// this *before* `overlay::app_modal_active`, since `SessionPicker` is also an
/// `active_modal`.
pub(super) fn active(agent: &AgentView) -> Option<ListPanel> {
    if matches!(agent.active_modal, Some(ActiveModal::SessionPicker { .. })) {
        return Some(ListPanel::Resume);
    }
    if minimal_api::extensions_modal(agent)
        .is_some_and(|s| s.active_tab == ExtensionsTab::McpServers)
    {
        return Some(ListPanel::Mcps);
    }
    None
}

/// Target viewport height for the active list panel: chrome + the exact body
/// height, clamped to `[CHROME_ROWS + 1, ceiling]`. Sizing to the exact content
/// height keeps the footer directly under the last row (no blank band); when the
/// body exceeds `ceiling` the list scrolls internally.
pub(super) fn panel_height(agent: &AgentView, kind: ListPanel, width: u16, ceiling: u16) -> u16 {
    let body = match kind {
        ListPanel::Resume => resume_body_rows(agent, width),
        ListPanel::Mcps => mcps_body_rows(agent),
    };
    CHROME_ROWS
        .saturating_add(body)
        .clamp(CHROME_ROWS + 1, ceiling.max(CHROME_ROWS + 1))
}

/// Render the active list panel into `area` (the whole live region). Returns the
/// text cursor for the panel's search bar when search is focused, else `None`.
pub(super) fn render(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut AgentView,
    kind: ListPanel,
    theme: &Theme,
) -> Option<(u16, u16)> {
    if area.height < 2 || area.width < 8 {
        return None;
    }
    match kind {
        ListPanel::Resume => render_resume(buf, area, agent, theme),
        ListPanel::Mcps => render_mcps(buf, area, agent, theme),
    }
}

// ─────────────────────────────── chrome ─────────────────────────────────────

/// Split `area` into (title_row, second_row, divider_row, list_area, footer_row).
/// `second_row` hosts the subtitle (mcps) or the search bar (resume).
fn chrome_layout(area: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
    let row = |dy: u16| Rect {
        x: area.x,
        y: area.y + dy,
        width: area.width,
        height: 1,
    };
    let title = row(0);
    let second = row(1);
    let divider = row(2);
    let footer = Rect {
        x: area.x,
        y: area.y + area.height - 1,
        ..row(0)
    };
    let list = Rect {
        x: area.x,
        y: area.y + 3,
        width: area.width,
        height: area.height.saturating_sub(CHROME_ROWS),
    };
    (title, second, divider, list, footer)
}

fn render_title(buf: &mut Buffer, row: Rect, theme: &Theme, title: &str) {
    buf.set_style(row, Style::default().bg(Color::Reset));
    let style = Style::default()
        .fg(theme.accent_user)
        .bg(Color::Reset)
        .add_modifier(Modifier::BOLD);
    buf.set_span(row.x + 1, row.y, &Span::styled(title, style), row.width);
}

fn render_dim_line(buf: &mut Buffer, row: Rect, theme: &Theme, text: &str) {
    buf.set_style(row, Style::default().bg(Color::Reset));
    let style = theme.dim().bg(Color::Reset);
    buf.set_span(row.x + 1, row.y, &Span::styled(text, style), row.width);
}

/// `/resume` session picker: Enter picks a session.
const RESUME_FOOTER: &str = "\u{2191}/\u{2193} navigate \u{00b7} enter confirm \u{00b7} esc cancel";

/// `/mcps` list: Enter expands tools; reconnect is space (off then on); `r` re-lists status.
const MCPS_FOOTER: &str = "\u{2191}/\u{2193} navigate \u{00b7} space enable/disable \u{00b7} r refresh \u{00b7} enter expand \u{00b7} esc cancel";

fn render_footer(buf: &mut Buffer, row: Rect, theme: &Theme, text: &str) {
    render_dim_line(buf, row, theme, text);
}

fn render_divider(buf: &mut Buffer, row: Rect, theme: &Theme) {
    picker::render_divider(buf, row.x, row.y, row.width, theme, None);
}

// ─────────────────────────────── resume ─────────────────────────────────────

/// Exact body height (display rows) for the session-picker list.
fn resume_body_rows(agent: &AgentView, width: u16) -> u16 {
    let Some(ActiveModal::SessionPicker {
        entries,
        state,
        source_filter,
        ..
    }) = &agent.active_modal
    else {
        return 0;
    };
    let entries_data = entries.as_deref().unwrap_or(&[]);
    let content_width = width.saturating_sub(2);
    let filtered =
        minimal_api::filter_session_entries(entries.as_deref(), state.query(), *source_filter);
    let built =
        minimal_api::build_session_entry_data(entries_data, &filtered, state, content_width);
    let fields_vecs: Vec<Vec<PickerField>> = built
        .iter()
        .map(|b| {
            b.field_data
                .iter()
                .map(|(l, v)| PickerField { label: l, value: v })
                .collect()
        })
        .collect();
    let current_repo = minimal_api::repo_name_from_cwd(&agent.session.cwd.to_string_lossy());
    let (picker_entries, _) = minimal_api::build_grouped_picker_entries(
        entries_data,
        &filtered,
        &built,
        &fields_vecs,
        state,
        Some(current_repo.as_str()),
    );
    // Reserve a row for the pinned hidden-external hint when shown.
    let hint_row = u16::from(
        !agent.app_chat_mode
            && minimal_api::hidden_external_hint(entries.as_deref(), *source_filter).is_some(),
    );
    measure_entries(&picker_entries).saturating_add(hint_row)
}

fn render_resume(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut AgentView,
    theme: &Theme,
) -> Option<(u16, u16)> {
    let cwd = agent.session.cwd.to_string_lossy().to_string();
    let chat_mode = agent.app_chat_mode;
    let Some(ActiveModal::SessionPicker {
        entries,
        state,
        source_filter,
        ..
    }) = &mut agent.active_modal
    else {
        return None;
    };
    let (title_row, search_row, divider_row, mut list_area, footer_row) = chrome_layout(area);

    let entries_data = entries.as_deref().unwrap_or(&[]);
    let content_width = area.width.saturating_sub(2);
    let filtered =
        minimal_api::filter_session_entries(entries.as_deref(), state.query(), *source_filter);
    let built =
        minimal_api::build_session_entry_data(entries_data, &filtered, state, content_width);
    let fields_vecs: Vec<Vec<PickerField>> = built
        .iter()
        .map(|b| {
            b.field_data
                .iter()
                .map(|(l, v)| PickerField { label: l, value: v })
                .collect()
        })
        .collect();
    let current_repo = minimal_api::repo_name_from_cwd(&cwd);
    let (picker_entries, non_sel) = minimal_api::build_grouped_picker_entries(
        entries_data,
        &filtered,
        &built,
        &fields_vecs,
        state,
        Some(current_repo.as_str()),
    );
    let hidden_hint = (!chat_mode)
        .then(|| minimal_api::hidden_external_hint(entries.as_deref(), *source_filter))
        .flatten();

    render_title(buf, title_row, theme, "Resume session");
    // Focus-aware search bar (cursor only when search is focused).
    minimal_api::render_picker_search_bar(
        buf,
        Rect::new(
            search_row.x + 1,
            search_row.y,
            search_row.width.saturating_sub(1),
            1,
        ),
        theme,
        state,
        true,
        None,
    );
    render_divider(buf, divider_row, theme);

    // Pinned above the list so it stays visible regardless of list scroll.
    if let Some(hint) = hidden_hint.as_deref() {
        render_dim_line(
            buf,
            Rect {
                height: 1,
                ..list_area
            },
            theme,
            hint,
        );
        list_area.y += 1;
        list_area.height = list_area.height.saturating_sub(1);
    }

    let nsc = vec![false; picker_entries.len()];
    let hit = picker::render_picker_content(
        buf,
        list_area,
        theme,
        state,
        &picker_entries,
        &non_sel,
        &nsc,
        None,
        false,
    );
    state.hit_areas = Some(PickerHitAreas {
        close_button: Rect::default(),
        search_bar: search_row,
        item_rects: hit.item_rects,
        entry_indices: hit.entry_indices,
        tab_rects: vec![],
        filter_rect: None,
    });

    render_footer(buf, footer_row, theme, RESUME_FOOTER);
    None
}

// ──────────────────────────────── mcps ──────────────────────────────────────

/// Exact body height (display rows) for the MCP list: one line per row.
fn mcps_body_rows(agent: &AgentView) -> u16 {
    let Some(s) = minimal_api::extensions_modal(agent) else {
        return 0;
    };
    let servers = match &s.mcps_data {
        TabDataState::Loaded(v) => v.as_slice(),
        _ => return 1, // a single "loading…" / error row
    };
    let rows = minimal_api::build_mcp_picker_rows(
        servers,
        s.picker_state.query(),
        s.mcps_filter,
        &s.mcps_collapsed_sections,
        &s.mcps_tools_expanded,
    );
    rows.0.len() as u16
}

fn render_mcps(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut AgentView,
    theme: &Theme,
) -> Option<(u16, u16)> {
    let (title_row, subtitle_row, divider_row, list_area, footer_row) = chrome_layout(area);
    render_title(buf, title_row, theme, "Manage MCP servers");

    // Phase 1 (immutable): build the row mapping + owned per-row render data.
    let labels: Vec<String>;
    let group_keys: Vec<Option<String>>;
    let data_indices: Vec<Option<usize>>;
    let badges: Vec<String>;
    let badge_colors: Vec<Option<Color>>;
    let right_labels: Vec<String>;
    let indents: Vec<u8>;
    let collapsibles: Vec<bool>;
    let expandeds: Vec<bool>;
    let subtitle: String;
    let loading;
    {
        let s = minimal_api::extensions_modal(agent)?;
        let searching = !s.picker_state.query().is_empty();
        loading = matches!(s.mcps_data, TabDataState::Loading);
        match &s.mcps_data {
            TabDataState::Loaded(servers) => {
                let (row_labels, row_group_keys, row_data_indices) =
                    minimal_api::build_mcp_picker_rows(
                        servers,
                        s.picker_state.query(),
                        s.mcps_filter,
                        &s.mcps_collapsed_sections,
                        &s.mcps_tools_expanded,
                    );
                let n = row_labels.len();
                let mut b = vec![String::new(); n];
                let mut bc: Vec<Option<Color>> = vec![None; n];
                let mut rl = vec![String::new(); n];
                let mut ind = vec![0u8; n];
                let mut col = vec![false; n];
                let mut exp = vec![false; n];
                for i in 0..n {
                    let gk = row_group_keys[i].as_deref();
                    if gk.is_some_and(|k| k.starts_with("mcp-section:")) {
                        col[i] = true;
                        exp[i] = !minimal_api::mcp_section_children_hidden(
                            &s.mcps_collapsed_sections,
                            gk.unwrap(),
                            searching,
                        );
                    } else if gk.is_some_and(|k| k.starts_with("mcp-tools:")) {
                        ind[i] = 1;
                        col[i] = true;
                        if let Some(si) = row_data_indices[i] {
                            exp[i] = s.mcps_tools_expanded.contains(&si);
                            if let Some(srv) = servers.get(si) {
                                if !srv.enabled {
                                    b[i] = "disabled".to_string();
                                    bc[i] = Some(theme.accent_error);
                                } else {
                                    b[i] = minimal_api::mcp_status_label(&srv.status).to_string();
                                    bc[i] = Some(minimal_api::mcp_status_theme_color(
                                        &srv.status,
                                        theme,
                                    ));
                                }
                                rl[i] = if srv.tool_count == 1 {
                                    "1 tool".to_string()
                                } else {
                                    format!("{} tools", srv.tool_count)
                                };
                            }
                        }
                    } else {
                        ind[i] = 2; // tool child
                    }
                }
                subtitle = format!(
                    "{} server{}",
                    servers.len(),
                    if servers.len() == 1 { "" } else { "s" }
                );
                labels = row_labels;
                group_keys = row_group_keys;
                data_indices = row_data_indices;
                badges = b;
                badge_colors = bc;
                right_labels = rl;
                indents = ind;
                collapsibles = col;
                expandeds = exp;
            }
            TabDataState::Loading => {
                subtitle = "loading\u{2026}".to_string();
                labels = vec![];
                group_keys = vec![];
                data_indices = vec![];
                badges = vec![];
                badge_colors = vec![];
                right_labels = vec![];
                indents = vec![];
                collapsibles = vec![];
                expandeds = vec![];
            }
            TabDataState::Error(msg) => {
                subtitle = format!("error: {msg}");
                labels = vec![];
                group_keys = vec![];
                data_indices = vec![];
                badges = vec![];
                badge_colors = vec![];
                right_labels = vec![];
                indents = vec![];
                collapsibles = vec![];
                expandeds = vec![];
            }
        }
    }
    let n = labels.len();

    render_dim_line(buf, subtitle_row, theme, &subtitle);
    render_divider(buf, divider_row, theme);

    // Phase 2 (mutable): mirror the row mapping onto state for the input handler.
    {
        let s = minimal_api::extensions_modal_mut(agent)?;
        s.entry_data_indices = data_indices;
        s.entry_group_keys = group_keys;
        s.entry_labels_cache = labels.clone();
        s.entry_non_selectable = vec![false; n];
        s.entry_non_selectable_clickable = vec![false; n];
        if n == 0 {
            s.picker_state.selected = 0;
        } else if s.picker_state.selected >= n {
            s.picker_state.selected = n - 1;
        }
    }

    // Phase 3 (mutable picker_state): build PickerEntry from owned data + render.
    let s = minimal_api::extensions_modal_mut(agent)?;
    let selected = s.picker_state.selected;
    let search_active = s.picker_state.search_active;
    let empty_fields: [PickerField; 0] = [];
    let no_lines: [&str; 0] = [];
    let entries: Vec<PickerEntry> = (0..n)
        .map(|i| {
            PickerEntry::Row(PickerRow {
                label: labels[i].as_str(),
                right_label: right_labels[i].as_str(),
                selected: !search_active && i == selected,
                expanded: expandeds[i],
                fields: &empty_fields,
                description_lines: &no_lines,
                summary_lines: &no_lines,
                dimmed: false,
                indent: indents[i],
                badge: badges[i].as_str(),
                badge_color: badge_colors[i],
                collapsible: collapsibles[i],
                underline_last_desc: false,
            })
        })
        .collect();
    let non_sel = vec![false; n];
    let hit = picker::render_picker_content(
        buf,
        list_area,
        theme,
        &mut s.picker_state,
        &entries,
        &non_sel,
        &non_sel,
        None,
        loading,
    );
    s.picker_state.hit_areas = Some(PickerHitAreas {
        close_button: Rect::default(),
        search_bar: Rect::default(),
        item_rects: hit.item_rects,
        entry_indices: hit.entry_indices,
        tab_rects: vec![],
        filter_rect: None,
    });

    render_footer(buf, footer_row, theme, MCPS_FOOTER);
    None
}

// ─────────────────────────────── helpers ────────────────────────────────────

/// Sum the display height of grouped picker entries: a header is one row (plus
/// the blank spacer `render_picker_content` draws before non-first headers); a
/// row is its label line plus its collapsed summary lines (what the picker
/// draws when the row is not expanded).
fn measure_entries(entries: &[PickerEntry<'_>]) -> u16 {
    entries
        .iter()
        .enumerate()
        .map(|(idx, e)| match e {
            PickerEntry::Header { .. } => {
                if idx == 0 {
                    1u16
                } else {
                    2u16
                }
            }
            PickerEntry::Row(r) => {
                if r.expanded {
                    1u16.saturating_add(r.description_lines.len() as u16)
                        .saturating_add(r.fields.len() as u16)
                } else {
                    1u16.saturating_add(r.summary_lines.len() as u16)
                }
            }
        })
        .fold(0u16, |acc, h| acc.saturating_add(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_pager::views::extensions_modal::ExtensionsModalState;
    use echo_agent_pager::views::mcps_modal::{
        McpServerDisplayStatus, McpServerInfo, McpWireSource,
    };
    use ratatui::layout::Rect;

    fn agent() -> AgentView {
        minimal_api::test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp/repo"))
    }

    fn mcp_server(name: &str, status: McpServerDisplayStatus, tools: usize) -> McpServerInfo {
        McpServerInfo {
            name: name.to_string(),
            display_name: None,
            status,
            tool_count: tools,
            auth_required: false,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: Vec::new(),
            enabled: true,
            source: "local".to_string(),
            wire_source: McpWireSource::Local,
            plugin_name: None,
            is_managed_gateway: false,
        }
    }

    fn with_mcps(servers: Vec<McpServerInfo>) -> AgentView {
        let mut a = agent();
        minimal_api::set_extensions_modal(
            &mut a,
            Some(ExtensionsModalState {
                active_tab: ExtensionsTab::McpServers,
                mcps_data: TabDataState::Loaded(servers),
                ..Default::default()
            }),
        );
        a
    }

    fn session_entry(id: &str) -> echo_agent_pager::app::app_view::SessionPickerEntry {
        echo_agent_pager::app::app_view::SessionPickerEntry {
            id: id.into(),
            summary: id.into(),
            updated_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            cwd: "/tmp/repo".into(),
            hostname: None,
            source: String::new(),
            model_id: None,
            num_messages: 0,
            last_active_at: None,
            branch: None,
            repo_name: "repo".into(),
            worktree_label: None,
            last_turn_summary: None,
            last_recap: None,
            card_detail: None,
        }
    }

    fn with_resume(entries: Vec<echo_agent_pager::app::app_view::SessionPickerEntry>) -> AgentView {
        let mut a = agent();
        a.active_modal = Some(ActiveModal::SessionPicker {
            state: picker::PickerState::default(),
            entries: Some(entries),
            loading: false,
            lanes: Default::default(),
            previous_palette: None,
            window: echo_agent_pager::views::modal_window::ModalWindowState::new(),
            content_results: None,
            content_loading: false,
            deep_search_seq: 0,
            source_filter: echo_agent_pager::views::session_picker::SourceFilter::default(),
            pending_delete: None,
            entries_query: None,
        });
        a
    }

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                out.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn active_detects_resume_mcps_and_none() {
        assert_eq!(active(&agent()), None);
        assert_eq!(
            active(&with_mcps(vec![mcp_server(
                "alpha",
                McpServerDisplayStatus::Ready,
                3
            )])),
            Some(ListPanel::Mcps)
        );
        assert_eq!(
            active(&with_resume(vec![session_entry("hello")])),
            Some(ListPanel::Resume)
        );
    }

    #[test]
    fn mcps_panel_renders_list_and_mirrors_handler_state() {
        let mut a = with_mcps(vec![
            mcp_server("alpha", McpServerDisplayStatus::Ready, 3),
            mcp_server("bravo", McpServerDisplayStatus::Unavailable, 0),
        ]);
        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut a, ListPanel::Mcps, &theme);

        let text = buffer_text(&buf);
        assert!(text.contains("Manage MCP servers"), "title:\n{text}");
        assert!(text.contains("2 servers"), "subtitle:\n{text}");
        assert!(text.contains("alpha"), "server row:\n{text}");
        assert!(text.contains("bravo"), "server row:\n{text}");
        assert!(text.contains("space enable/disable"), "footer:\n{text}");
        assert!(text.contains("r refresh"), "footer:\n{text}");
        assert!(text.contains("enter expand"), "footer:\n{text}");
        assert!(
            !text.contains("enter confirm"),
            "MCP footer must not reuse resume confirm copy:\n{text}"
        );

        // The input handler reads these render-stored fields; the panel must
        // mirror them (section header + 2 servers = 3 rows) so keyboard nav and
        // fold stay correct without touching the handler.
        let s = minimal_api::extensions_modal(&a).unwrap();
        assert_eq!(s.entry_data_indices.len(), 3, "section + 2 servers");
        assert_eq!(
            s.entry_data_indices.iter().filter(|d| d.is_some()).count(),
            2,
            "two selectable server rows map to catalog indices"
        );
        assert_eq!(s.entry_non_selectable.len(), 3);
    }

    #[test]
    fn resume_panel_renders_title_rows_and_footer() {
        let mut a = with_resume(vec![session_entry("first task"), session_entry("second")]);
        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut a, ListPanel::Resume, &theme);

        let text = buffer_text(&buf);
        assert!(text.contains("Resume session"), "title:\n{text}");
        assert!(text.contains("first task"), "session row:\n{text}");
        assert!(text.contains("enter confirm"), "resume footer:\n{text}");
        assert!(
            !text.contains("r refresh"),
            "resume footer must stay session-picker copy:\n{text}"
        );
    }

    #[test]
    fn resume_panel_pins_hidden_external_hint_above_scrolling_list() {
        // More native rows than the panel fits: the hint must stay pinned
        // above the list instead of scrolling away with it.
        let mut entries: Vec<_> = (0..20)
            .map(|i| session_entry(&format!("native-{i}")))
            .collect();
        let mut foreign = session_entry("claude-session");
        foreign.source = "claude".into();
        entries.push(foreign);
        let mut a = with_resume(entries);
        let theme = Theme::current();
        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &mut a, ListPanel::Resume, &theme);

        let text = buffer_text(&buf);
        assert!(
            text.contains("1 external session hidden \u{b7} f to show"),
            "hidden foreign rows must stay explained while the list scrolls:\n{text}"
        );
        assert!(
            text.find("external session hidden") < text.find("native-"),
            "the hint must be pinned above the first list row:\n{text}"
        );
        assert!(
            !text.contains("claude-session"),
            "foreign row stays hidden under the default filter:\n{text}"
        );
    }

    #[test]
    fn resume_search_uses_picker_grapheme_viewport_at_narrow_width() {
        let grapheme = "👩🏽\u{200d}💻";
        let combining = "e\u{301}";
        let mut agent = with_resume(vec![session_entry("match")]);
        let Some(ActiveModal::SessionPicker { state, .. }) = &mut agent.active_modal else {
            panic!("expected session picker");
        };
        state.set_query(format!("a{grapheme}{combining}"));
        state.search_active = true;

        let theme = Theme::current();
        let area = Rect::new(0, 0, 14, 5);
        let mut actual = Buffer::empty(area);
        render(&mut actual, area, &mut agent, ListPanel::Resume, &theme);

        let Some(ActiveModal::SessionPicker { state, .. }) = &agent.active_modal else {
            panic!("expected session picker");
        };
        let mut expected = Buffer::empty(area);
        minimal_api::render_picker_search_bar(
            &mut expected,
            Rect::new(1, 1, 13, 1),
            &theme,
            state,
            true,
            None,
        );
        for x in 1..14 {
            let actual_cell = actual.cell((x, 1)).expect("actual search cell");
            let expected_cell = expected.cell((x, 1)).expect("expected search cell");
            assert_eq!(actual_cell.symbol(), expected_cell.symbol(), "column {x}");
            assert_eq!(actual_cell.style(), expected_cell.style(), "column {x}");
        }
        let text = buffer_text(&actual);
        assert!(text.contains(grapheme), "ZWJ grapheme was split: {text:?}");
        assert!(
            text.contains(combining),
            "combining grapheme was split: {text:?}"
        );
        assert_eq!(
            actual.cell((13, 1)).expect("cursor cell").bg,
            theme.text_primary
        );
    }

    #[test]
    fn mcps_panel_height_is_chrome_plus_rows() {
        // One section header + 2 server rows = 3 body rows; + 4 chrome = 7.
        let a = with_mcps(vec![
            mcp_server("alpha", McpServerDisplayStatus::Ready, 1),
            mcp_server("bravo", McpServerDisplayStatus::Ready, 2),
        ]);
        assert_eq!(panel_height(&a, ListPanel::Mcps, 80, 40), 7);
        // Clamps to the screen ceiling when content is taller.
        assert_eq!(panel_height(&a, ListPanel::Mcps, 80, 5), 5);
    }
}
