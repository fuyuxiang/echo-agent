use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::render::SafeBuf;
use crate::theme::Theme;
use crate::views::agent_status::format_tokens_compact;
use crate::views::goal_detail::{format_elapsed, strip_control_chars, truncate_to_width};
use crate::views::picker::{PickerRow, render_picker_row};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowAgentRowView {
    pub agent_id: String,
    pub label: String,
    pub phase: Option<String>,
    pub model: Option<String>,
    pub state: String,
    pub tokens_used: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowAgentLiveStatus {
    pub activity: Option<String>,
    pub context_tokens: Option<u64>,
    pub context_window_tokens: Option<u64>,
    pub elapsed_ms: Option<u64>,
}

pub type WorkflowAgentLiveMap = std::collections::HashMap<String, WorkflowAgentLiveStatus>;

#[derive(Debug, Clone)]
pub struct WorkflowRunSnapshot {
    pub run_id: String,
    pub name: String,
    pub objective: String,
    pub status: String,
    pub management_available: bool,
    pub builtin: bool,
    pub phases: Vec<(String, String)>,
    pub current_phase: Option<String>,
    pub agents: Vec<WorkflowAgentRowView>,
    pub agent_budget: Option<u64>,
    pub agents_used: u64,
    pub agents_reserved: u64,
    pub agents_remaining: Option<u64>,
    pub agent_usage_incomplete: bool,
    pub active_agents: u32,
    pub elapsed_ms: u64,
    pub received_at: std::time::Instant,
    pub pause_message: Option<String>,
    pub result_summary: Option<String>,
}

impl WorkflowRunSnapshot {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "interrupted" | "complete" | "failed" | "cancelled"
        )
    }

    pub fn can_pause(&self) -> bool {
        self.management_available && self.is_active()
    }

    pub fn can_resume(&self) -> bool {
        if !self.management_available {
            return false;
        }
        matches!(
            self.status.as_str(),
            "user_paused"
                | "back_off_paused"
                | "no_progress_paused"
                | "infra_paused"
                | "blocked"
                | "failed"
                | "cancelled"
        )
    }

    pub fn can_stop(&self) -> bool {
        self.management_available && !self.is_terminal()
    }

    pub fn can_save(&self) -> bool {
        self.management_available && !self.builtin
    }

    pub fn active_agent_count(&self) -> usize {
        self.agents.iter().filter(|a| a.state == "running").count()
    }

    pub fn live_elapsed_ms(&self) -> u64 {
        let base = self.elapsed_ms;
        if self.is_active() {
            base.saturating_add(self.received_at.elapsed().as_millis() as u64)
        } else {
            base
        }
    }

    pub fn agents_in_phase(&self, phase: Option<&str>) -> Vec<&WorkflowAgentRowView> {
        match phase {
            Some(title) => self
                .agents
                .iter()
                .filter(|a| a.phase.as_deref() == Some(title))
                .collect(),
            None => self.agents.iter().collect(),
        }
    }

    pub fn phase_has_running_agents(&self, phase: &str) -> bool {
        self.agents
            .iter()
            .any(|a| a.state == "running" && a.phase.as_deref() == Some(phase))
    }

    pub fn effective_active_phase(&self) -> Option<String> {
        phase_rail(self)
            .iter()
            .rev()
            .find(|(title, _)| self.phase_has_running_agents(title))
            .map(|(title, _)| title.clone())
            .or_else(|| self.current_phase.clone())
    }

    fn done_agents(&self) -> usize {
        self.agents.iter().filter(|a| a.state != "running").count()
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowsViewState {
    pub selected_run: usize,
    pub selected_run_id: Option<String>,
    pub run_viewport: usize,
    pub detail_run_id: Option<String>,
    pub selected_phase: usize,
    pub selected_phase_name: Option<String>,
    pub phase_viewport: usize,
    pub phase_pinned: bool,
    pub pin_active_phase: Option<String>,
    pub window: crate::views::modal_window::ModalWindowState,
    pub run_hits: Vec<(Rect, String)>,
    pub phase_hits: Vec<(Rect, String)>,
    pub agent_hits: Vec<(Rect, String)>,
    pub list_area: Option<Rect>,
    pub rail_area: Option<Rect>,
    pub roster_area: Option<Rect>,
    pub roster_scroll: usize,
    pub roster_top_agent_id: Option<String>,
    pub roster_anchor: Option<(String, String)>,
}

pub mod shortcut_ids {
    pub const OPEN: usize = 1;
    pub const RUNS: usize = 2;
    pub const PAUSE: usize = 3;
    pub const RESUME: usize = 4;
    pub const STOP: usize = 5;
    pub const SAVE: usize = 6;
}

pub fn footer_shortcuts(
    in_detail: bool,
    has_run_list: bool,
    run: Option<&WorkflowRunSnapshot>,
) -> Vec<crate::views::modal_window::Shortcut<'static>> {
    use crate::views::modal_window::Shortcut;
    let mut s = Vec::new();
    if in_detail {
        s.push(Shortcut {
            label: "↑↓ phase · enter agent",
            clickable: false,
            id: 0,
        });
        if has_run_list {
            s.push(Shortcut {
                label: "←/tab runs",
                clickable: true,
                id: shortcut_ids::RUNS,
            });
        }
        if run.is_some_and(WorkflowRunSnapshot::can_pause) {
            s.push(Shortcut {
                label: "p pause",
                clickable: true,
                id: shortcut_ids::PAUSE,
            });
        }
        if run.is_some_and(WorkflowRunSnapshot::can_resume) {
            s.push(Shortcut {
                label: "r resume",
                clickable: true,
                id: shortcut_ids::RESUME,
            });
        }
        if run.is_some_and(WorkflowRunSnapshot::can_stop) {
            s.push(Shortcut {
                label: "x stop",
                clickable: true,
                id: shortcut_ids::STOP,
            });
        }
        if run.is_some_and(WorkflowRunSnapshot::can_save) {
            s.push(Shortcut {
                label: "s save",
                clickable: true,
                id: shortcut_ids::SAVE,
            });
        }
    } else {
        s.push(Shortcut {
            label: "↑↓ select",
            clickable: false,
            id: 0,
        });
        s.push(Shortcut {
            label: "enter open",
            clickable: true,
            id: shortcut_ids::OPEN,
        });
        if run.is_some_and(WorkflowRunSnapshot::can_stop) {
            s.push(Shortcut {
                label: "x stop",
                clickable: true,
                id: shortcut_ids::STOP,
            });
        }
    }
    s.push(Shortcut {
        label: "esc close",
        clickable: false,
        id: 0,
    });
    s
}

pub fn modal_config(
    in_detail: bool,
    has_run_list: bool,
    run: Option<&WorkflowRunSnapshot>,
) -> (
    Vec<crate::views::modal_window::Shortcut<'static>>,
    crate::views::modal_window::ModalSizing,
) {
    (
        footer_shortcuts(in_detail, has_run_list, run),
        crate::views::modal_window::ModalSizing::large(),
    )
}

impl WorkflowsViewState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn normalize(&mut self, runs: &[&WorkflowRunSnapshot]) {
        if runs.is_empty() {
            self.selected_run = 0;
            self.selected_run_id = None;
            self.run_viewport = 0;
        } else if let Some(id) = self.selected_run_id.as_deref() {
            if let Some(idx) = runs.iter().position(|run| run.run_id == id) {
                self.selected_run = idx;
            } else {
                self.selected_run = self.selected_run.min(runs.len() - 1);
                self.selected_run_id = Some(runs[self.selected_run].run_id.clone());
            }
        } else {
            self.selected_run = self.selected_run.min(runs.len() - 1);
            self.selected_run_id = Some(runs[self.selected_run].run_id.clone());
        }

        if let Some(id) = &self.detail_run_id
            && !runs.iter().any(|run| &run.run_id == id)
        {
            self.detail_run_id = None;
        }
        if runs.len() == 1 && self.detail_run_id.is_none() {
            self.detail_run_id = Some(runs[0].run_id.clone());
        }

        if let Some(run) = self.detail_run(runs) {
            self.selected_run_id = Some(run.run_id.clone());
            if let Some(idx) = runs
                .iter()
                .position(|candidate| candidate.run_id == run.run_id)
            {
                self.selected_run = idx;
            }
            let rail = phase_rail(run);
            if self.phase_pinned && run.effective_active_phase() != self.pin_active_phase {
                self.phase_pinned = false;
                self.pin_active_phase = None;
            }
            if self.phase_pinned {
                if let Some(name) = self.selected_phase_name.as_deref()
                    && let Some(idx) = rail.iter().position(|(title, _)| title == name)
                {
                    self.selected_phase = idx;
                } else {
                    self.selected_phase = self.selected_phase.min(rail.len().saturating_sub(1));
                }
            } else {
                self.selected_phase = default_phase_index(run);
            }
            self.selected_phase_name = rail
                .get(self.selected_phase)
                .map(|(title, _)| title.clone());
        } else {
            self.selected_phase = 0;
            self.selected_phase_name = None;
            self.phase_viewport = 0;
        }

        let anchor = self
            .detail_run_id
            .clone()
            .zip(self.selected_phase_name.clone());
        if self.roster_anchor != anchor {
            self.roster_anchor = anchor;
            self.roster_scroll = 0;
            self.roster_top_agent_id = None;
        }
    }

    pub fn select_run(&mut self, idx: usize, runs: &[&WorkflowRunSnapshot]) {
        if runs.is_empty() {
            self.selected_run = 0;
            self.selected_run_id = None;
            return;
        }
        self.selected_run = idx.min(runs.len() - 1);
        self.selected_run_id = Some(runs[self.selected_run].run_id.clone());
    }

    pub fn select_phase(&mut self, idx: usize, run: &WorkflowRunSnapshot) {
        let rail = phase_rail(run);
        self.selected_phase = idx.min(rail.len().saturating_sub(1));
        self.selected_phase_name = rail
            .get(self.selected_phase)
            .map(|(title, _)| title.clone());
        self.phase_pinned = true;
        self.pin_active_phase = run.effective_active_phase();
    }

    pub fn ensure_run_visible(&mut self, visible_rows: usize, total_rows: usize) {
        self.run_viewport = ensure_selection_visible(
            self.run_viewport,
            self.selected_run,
            visible_rows,
            total_rows,
        );
    }

    pub fn ensure_phase_visible(&mut self, visible_rows: usize, total_rows: usize) {
        self.phase_viewport = ensure_selection_visible(
            self.phase_viewport,
            self.selected_phase,
            visible_rows,
            total_rows,
        );
    }

    pub fn handle_scroll(&mut self, lines: i32, col: u16, row: u16, runs: &[&WorkflowRunSnapshot]) {
        if lines == 0 {
            return;
        }
        self.normalize(runs);
        let over = |area: Option<Rect>| area.is_some_and(|area| area.contains((col, row).into()));
        match self.detail_run(runs) {
            None => {
                if over(self.list_area) {
                    let idx = if lines > 0 {
                        self.selected_run.saturating_add(1)
                    } else {
                        self.selected_run.saturating_sub(1)
                    };
                    self.select_run(idx, runs);
                }
            }
            Some(run) if over(self.rail_area) => {
                let idx = if lines > 0 {
                    self.selected_phase.saturating_add(1)
                } else if lines < 0 {
                    self.selected_phase.saturating_sub(1)
                } else {
                    self.selected_phase
                };
                self.select_phase(idx, run);
                self.normalize(runs);
            }
            Some(_) if over(self.roster_area) => {
                if lines < 0 {
                    self.roster_scroll = self.roster_scroll.saturating_add((-lines) as usize);
                    self.roster_top_agent_id = None;
                } else {
                    self.roster_scroll = self.roster_scroll.saturating_sub(lines as usize);
                    if self.roster_scroll == 0 {
                        self.roster_top_agent_id = None;
                    }
                }
            }
            Some(_) => {}
        }
    }

    pub fn detail_run<'a>(
        &self,
        runs: &[&'a WorkflowRunSnapshot],
    ) -> Option<&'a WorkflowRunSnapshot> {
        let id = self.detail_run_id.as_ref()?;
        runs.iter().copied().find(|run| &run.run_id == id)
    }
}

fn ensure_selection_visible(
    viewport: usize,
    selected: usize,
    visible_rows: usize,
    total_rows: usize,
) -> usize {
    if visible_rows == 0 || total_rows <= visible_rows {
        return 0;
    }
    let max_viewport = total_rows - visible_rows;
    let viewport = viewport.min(max_viewport);
    if selected < viewport {
        selected
    } else if selected >= viewport + visible_rows {
        selected + 1 - visible_rows
    } else {
        viewport
    }
}

pub fn phase_rail(run: &WorkflowRunSnapshot) -> Vec<(String, String)> {
    let mut phases = run.phases.clone();
    if let Some(current) = run.current_phase.as_deref()
        && !phases.iter().any(|(title, _)| title == current)
    {
        let state = if run.status == "complete" {
            "done"
        } else {
            "active"
        };
        phases.push((current.to_owned(), state.to_owned()));
    }
    if phases.is_empty() {
        phases.push(("All agents".to_owned(), "active".to_owned()));
    }
    phases
}

fn default_phase_index(run: &WorkflowRunSnapshot) -> usize {
    let rail = phase_rail(run);
    run.effective_active_phase()
        .and_then(|current| rail.iter().position(|(title, _)| title == &current))
        .or_else(|| rail.iter().position(|(_, state)| state == "active"))
        .unwrap_or_else(|| {
            if rail.iter().all(|(_, state)| state == "done") {
                rail.len().saturating_sub(1)
            } else {
                0
            }
        })
}

fn span_at(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, max_x: u16) {
    if max_x <= x {
        return;
    }
    buf.set_span_safe(
        x,
        y,
        &ratatui::text::Span::styled(text.to_string(), style),
        max_x - x,
    );
}

fn status_glyph_and_style(status: &str, theme: &Theme) -> (&'static str, Style) {
    match status {
        "active" => ("●", Style::default().fg(theme.accent_plan)),
        "complete" => ("✓", Style::default().fg(theme.accent_success)),
        "failed" | "interrupted" => ("✗", Style::default().fg(theme.accent_error)),
        "cancelled" => ("◌", Style::default().fg(theme.gray_dim)),
        _ => ("⏸", Style::default().fg(theme.warning)),
    }
}

fn agent_glyph_and_style(state: &str, theme: &Theme) -> (&'static str, Style) {
    match state {
        "running" => ("●", Style::default().fg(theme.accent_plan)),
        "done" => ("✓", Style::default().fg(theme.accent_success)),
        "failed" => ("✗", Style::default().fg(theme.accent_error)),
        _ => ("◌", Style::default().fg(theme.gray_dim)),
    }
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn strip_control(text: &str) -> String {
    strip_control_chars(text, false)
}

pub fn render_workflows(
    buf: &mut Buffer,
    area: Rect,
    runs: &[&WorkflowRunSnapshot],
    state: &mut WorkflowsViewState,
    tick: usize,
    live: &WorkflowAgentLiveMap,
) -> Option<Rect> {
    use crate::views::modal_window::{ModalWindowConfig, render_modal_window};

    let theme = Theme::current();
    state.run_hits.clear();
    state.phase_hits.clear();
    state.agent_hits.clear();
    state.list_area = None;
    state.rail_area = None;
    state.roster_area = None;
    let detail_run = state.detail_run(runs);
    let in_detail = detail_run.is_some();
    let has_run_list = runs.len() > 1;
    let selected_run = detail_run.or_else(|| runs.get(state.selected_run).copied());
    let (shortcuts, sizing) = modal_config(in_detail, has_run_list, selected_run);
    let config = ModalWindowConfig {
        // "Workflow Runs", not "Workflows": that name belongs to the
        // extensions-modal catalog tab.
        title: "Workflow Runs",
        tabs: None,
        shortcuts: &shortcuts,
        sizing,
        fold_info: None,
    };
    let content = render_modal_window(buf, area, &mut state.window, &config, &theme)?;
    let inner = content.content;

    match state.detail_run(runs) {
        Some(run) => render_detail(buf, inner, run, state, tick, &theme, live),
        None => render_list(buf, inner, runs, state, &theme),
    }
    state.window.popup_area
}

fn render_list(
    buf: &mut Buffer,
    inner: Rect,
    runs: &[&WorkflowRunSnapshot],
    state: &mut WorkflowsViewState,
    theme: &Theme,
) {
    let mut y = inner.y;
    if runs.is_empty() {
        span_at(
            buf,
            inner.x + 1,
            y + 1,
            "No workflow runs in this session yet.",
            Style::default().fg(theme.gray_bright),
            inner.right(),
        );
        span_at(
            buf,
            inner.x + 1,
            y + 3,
            "Start one with /deep-research <query> or ask for a workflow.",
            Style::default().fg(theme.gray),
            inner.right(),
        );
        return;
    }

    state.list_area = Some(Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    ));
    let bottom = inner.bottom();
    let visible_rows = usize::from(inner.height);
    state.ensure_run_visible(visible_rows, runs.len());
    for (idx, run) in runs.iter().enumerate().skip(state.run_viewport) {
        if y >= bottom {
            break;
        }
        let (glyph, glyph_style) = status_glyph_and_style(&run.status, theme);
        let done_phases = run.phases.iter().filter(|(_, s)| s == "done").count();
        let phase_part = if run.phases.is_empty() {
            run.status.clone()
        } else {
            format!(
                "{}/{} phase{}",
                done_phases,
                run.phases.len(),
                if run.phases.len() == 1 { "" } else { "s" }
            )
        };
        let meta = format!(
            "{phase_part} · {}/{} agent{} · {}",
            run.done_agents(),
            run.agents.len(),
            if run.agents.len() == 1 { "" } else { "s" },
            format_elapsed(run.live_elapsed_ms()),
        );
        let label = format!(
            "{} · {}",
            strip_control(&run.name),
            strip_control(&run.objective)
        );
        let badge = format!("{glyph} {}", run.status.replace('_', " "));
        let row = PickerRow {
            label: &label,
            right_label: &meta,
            selected: idx == state.selected_run,
            expanded: false,
            fields: &[],
            description_lines: &[],
            summary_lines: &[],
            dimmed: matches!(run.status.as_str(), "cancelled" | "cleared"),
            indent: 0,
            badge: &badge,
            badge_color: glyph_style.fg,
            collapsible: false,
            underline_last_desc: false,
        };
        let rendered = render_picker_row(
            buf,
            inner.x + 1,
            y,
            inner.width.saturating_sub(2),
            theme,
            &row,
            false,
            Some(theme.bg_base),
            bottom.saturating_sub(y),
        );
        let row_h = rendered.rows.max(1);
        state.run_hits.push((
            Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), row_h),
            run.run_id.clone(),
        ));
        y += row_h;
    }
}

#[allow(clippy::too_many_arguments)]
fn render_detail(
    buf: &mut Buffer,
    inner: Rect,
    run: &WorkflowRunSnapshot,
    state: &mut WorkflowsViewState,
    tick: usize,
    theme: &Theme,
    live: &WorkflowAgentLiveMap,
) {
    let name = strip_control(&run.name);
    let (glyph, glyph_style) = status_glyph_and_style(&run.status, theme);
    let spinner = if run.is_active() {
        let frames = crate::glyphs::dot_spinner_frames();
        format!("{} ", frames[(tick / 4) % frames.len()])
    } else {
        format!("{glyph} ")
    };
    let meta = format!(
        "{}/{} agent{} · {}",
        run.done_agents(),
        run.agents.len(),
        if run.agents.len() == 1 { "" } else { "s" },
        format_elapsed(run.live_elapsed_ms()),
    );
    let meta_w = unicode_width::UnicodeWidthStr::width(meta.as_str()) as u16;
    let meta_x = inner.right().saturating_sub(meta_w + 1);

    span_at(
        buf,
        inner.x + 1,
        inner.y,
        &spinner,
        glyph_style,
        inner.right(),
    );
    span_at(
        buf,
        inner.x + 3,
        inner.y,
        &truncate_to_width(&name, meta_x.saturating_sub(inner.x + 4) as usize),
        Style::default()
            .fg(theme.accent_plan)
            .add_modifier(Modifier::BOLD),
        meta_x,
    );
    span_at(
        buf,
        meta_x,
        inner.y,
        &meta,
        Style::default().fg(theme.gray_dim),
        inner.right(),
    );
    let objective = truncate_to_width(
        &strip_control(&run.objective),
        inner.width.saturating_sub(2) as usize,
    );
    span_at(
        buf,
        inner.x + 1,
        inner.y + 1,
        &objective,
        Style::default().fg(theme.gray),
        inner.right(),
    );

    let mut body_y = inner.y + 2;
    let status_line = if run.status == "budget_limited" {
        let body = if run.agents_used >= 1_024 {
            "budget limited: maximum agent budget reached; start a new run".to_string()
        } else if let Some(pause) = run.pause_message.as_deref().filter(|s| !s.is_empty()) {
            format!(
                "budget limited: bare resume disabled; raise agent budget via agent/tool. {}",
                strip_control(pause)
            )
        } else {
            format!(
                "budget limited: bare resume disabled; raise agent budget above {} via agent/tool",
                run.agents_used
            )
        };
        Some((body, Style::default().fg(theme.warning)))
    } else if let Some(pause) = run.pause_message.as_deref() {
        Some((
            format!("{}: {}", run.status.replace('_', " "), strip_control(pause)),
            Style::default().fg(theme.warning),
        ))
    } else if run.status == "failed" {
        Some((
            "failed: see scrollback for details; r resumes from the journal".to_string(),
            Style::default().fg(theme.accent_error),
        ))
    } else {
        None
    };
    if let Some((text, style)) = status_line {
        span_at(
            buf,
            inner.x + 1,
            body_y,
            &truncate_to_width(&text, inner.width.saturating_sub(2) as usize),
            style,
            inner.right(),
        );
        body_y += 1;
    }

    let body_h = inner.bottom().saturating_sub(body_y);
    if body_h < 3 {
        return;
    }
    let rail = phase_rail(run);
    let rail_cap = inner.width.saturating_sub(5) / 2;
    if rail_cap == 0 {
        return;
    }
    let wanted_rail_w = rail
        .iter()
        .map(|(t, _)| unicode_width::UnicodeWidthStr::width(t.as_str()) as u16)
        .max()
        .unwrap_or(8)
        .saturating_add(12);
    let rail_w = if rail_cap >= 18 {
        wanted_rail_w.clamp(18, rail_cap)
    } else {
        wanted_rail_w.min(rail_cap).max(1)
    };
    let rail_area = Rect::new(inner.x.saturating_add(1), body_y, rail_w, body_h);
    let divider_x = rail_area.right().saturating_add(1);
    let roster_x = divider_x.saturating_add(2);
    let roster_width = inner.right().saturating_sub(roster_x.saturating_add(1));
    if roster_width == 0 {
        return;
    }
    let roster_area = Rect::new(roster_x, body_y, roster_width, body_h);
    state.rail_area = Some(rail_area);
    state.roster_area = Some(roster_area);
    for y in body_y..body_y + body_h {
        span_at(
            buf,
            divider_x,
            y,
            "│",
            Style::default().fg(theme.gray_dim),
            divider_x + 1,
        );
    }

    span_at(
        buf,
        rail_area.x,
        body_y,
        "Phases",
        Style::default().fg(theme.text_secondary),
        rail_area.right(),
    );
    let rail_inner = Rect::new(
        rail_area.x,
        body_y + 1,
        rail_area.width,
        body_h.saturating_sub(1),
    );

    let selected_phase_title = rail
        .get(state.selected_phase)
        .map(|(title, _)| title.clone())
        .unwrap_or_default();
    let all_agents_phase = run.phases.is_empty() && run.current_phase.is_none();
    state.ensure_phase_visible(usize::from(rail_inner.height), rail.len());
    for (idx, (title, phase_state)) in rail.iter().enumerate().skip(state.phase_viewport) {
        let y = rail_inner.y + (idx - state.phase_viewport) as u16;
        if y >= rail_inner.bottom() {
            break;
        }
        let selected = idx == state.selected_phase;
        let agents_in = if all_agents_phase {
            run.agents.len()
        } else {
            run.agents_in_phase(Some(title)).len()
        };
        let done_in = if all_agents_phase {
            run.done_agents()
        } else {
            run.agents_in_phase(Some(title))
                .iter()
                .filter(|agent| agent.state != "running")
                .count()
        };
        let running_in = if all_agents_phase {
            run.active_agent_count() > 0
        } else {
            run.phase_has_running_agents(title)
        };
        let effective_state = if running_in {
            "active"
        } else {
            phase_state.as_str()
        };
        let marker = if selected { "❯" } else { " " };
        let num_style = match effective_state {
            "done" => Style::default().fg(theme.accent_success),
            "active" => Style::default().fg(theme.accent_plan),
            _ => Style::default().fg(theme.gray_dim),
        };
        let title_style = if selected {
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD)
        } else if effective_state == "pending" {
            Style::default().fg(theme.gray_dim)
        } else {
            Style::default().fg(theme.gray_bright)
        };
        let count = if running_in {
            format!("● {done_in}/{agents_in}")
        } else if agents_in > 0 {
            format!("{done_in}/{agents_in}")
        } else {
            String::new()
        };
        let count_style = if running_in {
            Style::default().fg(theme.accent_plan)
        } else {
            Style::default().fg(theme.gray_dim)
        };
        let count_w = unicode_width::UnicodeWidthStr::width(count.as_str()) as u16;
        let count_x = rail_inner.right().saturating_sub(count_w);

        span_at(buf, rail_inner.x, y, marker, num_style, rail_inner.right());
        span_at(
            buf,
            rail_inner.x + 2,
            y,
            &format!("{} ", idx + 1),
            num_style,
            rail_inner.right(),
        );
        span_at(
            buf,
            rail_inner.x + 4,
            y,
            &truncate_to_width(title, count_x.saturating_sub(rail_inner.x + 5) as usize),
            title_style,
            count_x,
        );
        span_at(buf, count_x, y, &count, count_style, rail_inner.right());
        state.phase_hits.push((
            Rect::new(rail_inner.x, y, rail_inner.width, 1),
            title.clone(),
        ));
    }

    let roster_agents = if all_agents_phase {
        run.agents_in_phase(None)
    } else {
        run.agents_in_phase(Some(selected_phase_title.as_str()))
    };
    let roster_inner = Rect::new(
        roster_area.x,
        body_y + 1,
        roster_area.width,
        body_h.saturating_sub(1),
    );
    let visible_rows = usize::from(roster_inner.height);
    let overflow = roster_agents.len().saturating_sub(visible_rows);
    if state.roster_scroll > 0
        && let Some(anchor_id) = state.roster_top_agent_id.as_deref()
        && let Some(anchor_idx) = roster_agents
            .iter()
            .position(|agent| agent.agent_id == anchor_id)
    {
        state.roster_scroll = overflow.saturating_sub(anchor_idx.min(overflow));
    }
    state.roster_scroll = state.roster_scroll.min(overflow);
    if state.roster_scroll == 0 {
        state.roster_top_agent_id = None;
    }
    let roster_title = if state.roster_scroll > 0 {
        format!(
            "{} · {} · ↑{}",
            selected_phase_title,
            plural(roster_agents.len(), "agent"),
            state.roster_scroll,
        )
    } else {
        format!(
            "{} · {}",
            selected_phase_title,
            plural(roster_agents.len(), "agent")
        )
    };
    span_at(
        buf,
        roster_area.x,
        body_y,
        &truncate_to_width(&roster_title, roster_area.width as usize),
        Style::default().fg(theme.text_secondary),
        roster_area.right(),
    );

    if roster_agents.is_empty() {
        span_at(
            buf,
            roster_inner.x,
            roster_inner.y,
            "No agents in this phase yet.",
            Style::default().fg(theme.gray_dim),
            roster_inner.right(),
        );
    }
    let skip = overflow - state.roster_scroll;
    state.roster_top_agent_id = (state.roster_scroll > 0)
        .then(|| roster_agents.get(skip).map(|agent| agent.agent_id.clone()))
        .flatten();
    for (row, agent) in roster_agents.iter().skip(skip).enumerate() {
        let y = roster_inner.y + row as u16;
        if y >= roster_inner.bottom() {
            break;
        }
        let running = agent.state == "running";
        let (glyph, glyph_style) = if running {
            let frames = crate::glyphs::dot_spinner_frames();
            (
                frames[(tick / 4) % frames.len()],
                Style::default().fg(theme.accent_plan),
            )
        } else {
            agent_glyph_and_style(&agent.state, theme)
        };
        let live_status = live.get(&agent.agent_id);
        let elapsed_ms = if running {
            live_status.and_then(|l| l.elapsed_ms).unwrap_or(0)
        } else {
            agent.duration_ms
        };
        let mut meta_parts: Vec<String> = Vec::new();
        if let Some((context_tokens, context_window_tokens)) = live_status.and_then(|status| {
            status
                .context_tokens
                .zip(status.context_window_tokens.filter(|&window| window > 0))
        }) {
            meta_parts.push(format!(
                "{} / {} context",
                format_tokens_compact(i64::try_from(context_tokens).unwrap_or(i64::MAX)),
                format_tokens_compact(i64::try_from(context_window_tokens).unwrap_or(i64::MAX)),
            ));
        }
        if elapsed_ms > 0 {
            meta_parts.push(format_elapsed(elapsed_ms));
        }
        let meta = truncate_to_width(
            &meta_parts.join(" · "),
            roster_inner.width.saturating_sub(6) as usize,
        );
        let meta_w = unicode_width::UnicodeWidthStr::width(meta.as_str()) as u16;
        let meta_x = roster_inner
            .right()
            .saturating_sub(meta_w + 1)
            .max(roster_inner.x + 4);

        span_at(
            buf,
            roster_inner.x,
            y,
            glyph,
            glyph_style,
            roster_inner.right(),
        );
        let label = truncate_to_width(
            &strip_control(&agent.label),
            (roster_inner.width as usize).saturating_sub(4).min(28),
        );
        span_at(
            buf,
            roster_inner.x + 2,
            y,
            &label,
            Style::default().fg(theme.text_primary),
            meta_x,
        );
        let label_w = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
        let mut trail_x = roster_inner.x + 2 + label_w + 2;
        if let Some(model) = agent.model.as_deref() {
            let model_txt = truncate_to_width(model, meta_x.saturating_sub(trail_x + 1) as usize);
            span_at(
                buf,
                trail_x,
                y,
                &model_txt,
                Style::default().fg(theme.gray),
                meta_x,
            );
            trail_x += unicode_width::UnicodeWidthStr::width(model_txt.as_str()) as u16 + 2;
        }
        if let Some(activity) = live_status.and_then(|l| l.activity.as_deref()) {
            let activity_txt = truncate_to_width(
                &format!("· {}", strip_control(activity)),
                meta_x.saturating_sub(trail_x + 1) as usize,
            );
            span_at(
                buf,
                trail_x,
                y,
                &activity_txt,
                Style::default().fg(theme.gray_dim),
                meta_x,
            );
        }
        span_at(
            buf,
            meta_x,
            y,
            &meta,
            Style::default().fg(theme.gray_dim),
            roster_inner.right(),
        );
        state.agent_hits.push((
            Rect::new(roster_inner.x, y, roster_inner.width, 1),
            agent.agent_id.clone(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn make_run(run_id: &str, name: &str, status: &str) -> WorkflowRunSnapshot {
        WorkflowRunSnapshot {
            run_id: run_id.to_string(),
            name: name.to_string(),
            objective: "Research the thing thoroughly".to_string(),
            status: status.to_string(),
            management_available: true,
            builtin: false,
            phases: vec![
                ("Plan".to_string(), "done".to_string()),
                ("Research".to_string(), "active".to_string()),
                ("Synthesize".to_string(), "pending".to_string()),
            ],
            current_phase: Some("Research".to_string()),
            agents: vec![
                WorkflowAgentRowView {
                    agent_id: "a1".into(),
                    label: "planner".into(),
                    phase: Some("Plan".into()),
                    model: None,
                    state: "done".into(),
                    tokens_used: 12_300,
                    duration_ms: 0,
                },
                WorkflowAgentRowView {
                    agent_id: "a2".into(),
                    label: "researcher-1".into(),
                    phase: Some("Research".into()),
                    model: Some("echo-agent-4.5".into()),
                    state: "running".into(),
                    tokens_used: 0,
                    duration_ms: 0,
                },
            ],
            agent_budget: Some(128),
            agents_used: 2,
            agents_reserved: 0,
            agents_remaining: Some(126),
            agent_usage_incomplete: false,
            active_agents: 1,
            elapsed_ms: 95_000,
            received_at: std::time::Instant::now(),
            pause_message: None,
            result_summary: None,
        }
    }

    fn buf_text(buf: &Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn render_to_text(runs: &[&WorkflowRunSnapshot], state: &WorkflowsViewState) -> String {
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let mut state = state.clone();
        render_workflows(
            &mut buf,
            area,
            runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        buf_text(&buf, area)
    }

    #[test]
    fn single_run_auto_enters_detail_with_phase_rail_and_roster() {
        let run = make_run("wf_1", "deep-research", "active");
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        assert_eq!(state.detail_run_id.as_deref(), Some("wf_1"));
        assert_eq!(state.selected_phase, 1);

        let text = render_to_text(&runs, &state);
        assert!(text.contains("deep-research"), "{text}");
        assert!(text.contains("Phases"), "{text}");
        assert!(text.contains("Research"), "{text}");
        assert!(text.contains("researcher-1"), "{text}");
        assert!(text.contains("echo-agent-4.5"), "{text}");
        assert!(text.contains("1/2 agents"), "{text}");
        assert!(text.contains("s save"), "{text}");
    }

    #[test]
    fn footer_hides_save_for_builtin_runs() {
        let mut run = make_run("wf_1", "deep-research", "active");
        let labels = |run: &WorkflowRunSnapshot| {
            footer_shortcuts(true, false, Some(run))
                .into_iter()
                .map(|shortcut| shortcut.label)
                .collect::<Vec<_>>()
        };
        assert!(labels(&run).contains(&"s save"));

        run.builtin = true;
        assert!(!labels(&run).contains(&"s save"));
    }

    #[test]
    fn budget_limited_run_disables_bare_resume_and_explains_raised_cap_path() {
        let mut run = make_run("wf_1", "deep-research", "budget_limited");
        assert!(!run.can_resume());
        assert!(run.can_stop());

        let labels = footer_shortcuts(true, false, Some(&run))
            .into_iter()
            .map(|shortcut| shortcut.label)
            .collect::<Vec<_>>();
        assert!(!labels.contains(&"r resume"));

        assert!(
            labels.contains(&"x stop"),
            "budget_limited is non-terminal and must keep stop"
        );

        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        let area = Rect::new(0, 0, 140, 30);
        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        let text = buf_text(&buf, area);
        assert!(text.contains("raise agent budget above 2"), "{text}");
        assert!(text.contains("bare resume disabled"), "{text}");

        run.pause_message = Some(
            "the workflow token budget was exhausted; some model requests returned unknown \
             token usage (infrastructure errors) and were counted as zero, so real usage is \
             at least the recorded total — finished work is kept; resume the run with a \
             raised token budget to continue"
                .to_string(),
        );
        let runs = vec![&run];
        state.normalize(&runs);
        let narrow = Rect::new(0, 0, 84, 30);
        let mut buf = Buffer::empty(narrow);
        render_workflows(
            &mut buf,
            narrow,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        let text = buf_text(&buf, narrow);
        assert!(text.contains("bare resume disabled"), "{text}");
        assert!(text.contains("raise agent budget"), "{text}");
    }

    #[test]
    fn failed_run_offers_resume_but_not_stop() {
        let run = make_run("wf_1", "deep-research", "failed");
        assert!(
            run.can_resume(),
            "failed runs resume via journal replay of completed agents"
        );
        assert!(!run.can_stop(), "failed is terminal");

        let labels = footer_shortcuts(true, false, Some(&run))
            .into_iter()
            .map(|shortcut| shortcut.label)
            .collect::<Vec<_>>();
        assert!(labels.contains(&"r resume"));
        assert!(!labels.contains(&"x stop"));
    }

    #[test]
    fn narrow_detail_layout_is_panic_free() {
        let run = make_run("wf_1", "deep-research", "active");
        let runs = vec![&run];
        let area = Rect::new(0, 0, 32, 12);
        let mut buf = Buffer::empty(area);
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
    }

    #[test]
    fn multiple_runs_render_as_list_until_entered() {
        let a = make_run("wf_a", "deep-research", "active");
        let b = make_run("wf_b", "count-v2", "complete");
        let runs = vec![&a, &b];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        assert!(state.detail_run_id.is_none());

        let area = Rect::new(0, 0, 180, 30);
        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        let text = buf_text(&buf, area);
        assert!(text.contains("deep-research"), "{text}");
        assert!(text.contains("count-v2"), "{text}");
        assert!(text.contains("1/2 agents"), "{text}");
        assert!(!text.contains("128"), "budget cap is not shown: {text}");
        assert!(!text.contains(" · out "), "{text}");
        assert!(text.contains("enter open"), "{text}");
    }

    #[test]
    fn run_and_phase_viewports_keep_stable_selection_visible() {
        let runs_owned: Vec<_> = (0..20)
            .map(|idx| make_run(&format!("wf_{idx:02}"), &format!("run-{idx:02}"), "active"))
            .collect();
        let mut runs: Vec<_> = runs_owned.iter().collect();
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        state.select_run(12, &runs);
        state.ensure_run_visible(5, runs.len());
        assert_eq!(state.run_viewport, 8);
        assert_eq!(state.selected_run_id.as_deref(), Some("wf_12"));

        let newest = make_run("wf_new", "newest", "active");
        runs.insert(0, &newest);
        state.normalize(&runs);
        state.ensure_run_visible(5, runs.len());
        assert_eq!(state.selected_run, 13);
        assert_eq!(state.selected_run_id.as_deref(), Some("wf_12"));
        assert!((state.run_viewport..state.run_viewport + 5).contains(&state.selected_run));

        let mut run = make_run("wf_phases", "phases", "active");
        run.phases = (0..20)
            .map(|idx| (format!("Phase {idx:02}"), "pending".to_owned()))
            .collect();
        run.current_phase = Some("Phase 12".to_owned());
        let phase_runs = vec![&run];
        let mut phase_state = WorkflowsViewState::default();
        phase_state.normalize(&phase_runs);
        phase_state.select_phase(12, &run);
        phase_state.ensure_phase_visible(4, 20);
        assert_eq!(phase_state.phase_viewport, 9);

        run.phases
            .insert(0, ("Prelude".to_owned(), "done".to_owned()));
        let phase_runs = vec![&run];
        phase_state.normalize(&phase_runs);
        phase_state.ensure_phase_visible(4, 21);
        assert_eq!(phase_state.selected_phase_name.as_deref(), Some("Phase 12"));
        assert_eq!(phase_state.selected_phase, 13);
        assert!(
            (phase_state.phase_viewport..phase_state.phase_viewport + 4)
                .contains(&phase_state.selected_phase)
        );
    }

    #[test]
    fn undeclared_current_phase_is_selectable_and_filters_its_agents() {
        let mut run = make_run("wf_1", "dynamic", "active");
        run.phases = vec![("Declared".to_owned(), "pending".to_owned())];
        run.current_phase = Some("Discovered".to_owned());
        run.agents = vec![WorkflowAgentRowView {
            agent_id: "dynamic-agent".to_owned(),
            label: "dynamic-agent".to_owned(),
            phase: Some("Discovered".to_owned()),
            model: None,
            state: "running".to_owned(),
            tokens_used: 0,
            duration_ms: 0,
        }];
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        assert_eq!(
            phase_rail(&run),
            vec![
                ("Declared".to_owned(), "pending".to_owned()),
                ("Discovered".to_owned(), "active".to_owned()),
            ]
        );
        assert_eq!(state.selected_phase, 1);
        assert_eq!(state.selected_phase_name.as_deref(), Some("Discovered"));

        let text = render_to_text(&runs, &state);
        assert!(text.contains("Discovered"), "{text}");
        assert!(text.contains("dynamic-agent"), "{text}");
    }

    #[test]
    fn phase_selection_filters_roster() {
        let run = make_run("wf_1", "deep-research", "active");
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        state.select_phase(0, &run);
        state.normalize(&runs);

        let text = render_to_text(&runs, &state);
        assert!(text.contains("planner"), "{text}");
        assert!(!text.contains("researcher-1"), "{text}");
        assert!(text.contains("Plan · 1 agent "), "{text}");
    }

    #[test]
    fn render_records_row_hit_rects_per_mode() {
        let a = make_run("wf_a", "deep-research", "active");
        let b = make_run("wf_b", "count-v2", "complete");
        let runs = vec![&a, &b];
        let area = Rect::new(0, 0, 100, 30);

        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        assert_eq!(
            state
                .run_hits
                .iter()
                .map(|(_, id)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["wf_a", "wf_b"]
        );
        assert!(state.phase_hits.is_empty());
        assert!(state.agent_hits.is_empty());
        assert!(state.list_area.is_some());

        state.detail_run_id = Some("wf_a".to_string());
        state.normalize(&runs);
        assert_eq!(state.selected_phase, 1);
        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        assert!(state.run_hits.is_empty());
        assert!(state.list_area.is_none());
        assert_eq!(
            state
                .phase_hits
                .iter()
                .map(|(_, title)| title.as_str())
                .collect::<Vec<_>>(),
            vec!["Plan", "Research", "Synthesize"]
        );
        assert_eq!(
            state
                .agent_hits
                .iter()
                .map(|(_, id)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["a2"],
        );
        let (rect, _) = &state.agent_hits[0];
        assert!(rect.width > 0 && rect.height == 1);
        let tiny = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(tiny);
        render_workflows(
            &mut buf,
            tiny,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        assert!(state.agent_hits.is_empty());
        assert!(state.phase_hits.is_empty());
        assert!(state.rail_area.is_none() && state.roster_area.is_none());
    }

    #[test]
    fn wheel_routes_by_panel() {
        let a = make_run("wf_a", "deep-research", "active");
        let b = make_run("wf_b", "count-v2", "complete");
        let runs = vec![&a, &b];

        let mut state = WorkflowsViewState {
            list_area: Some(Rect::new(5, 5, 60, 10)),
            ..Default::default()
        };
        state.handle_scroll(3, 10, 10, &runs);
        assert_eq!(state.selected_run, 1);
        state.handle_scroll(3, 10, 10, &runs);
        assert_eq!(state.selected_run, 1, "clamps at the last run");
        state.handle_scroll(-3, 10, 10, &runs);
        assert_eq!(state.selected_run, 0);
        state.handle_scroll(3, 90, 2, &runs);
        assert_eq!(
            state.selected_run, 0,
            "wheel outside the run list is ignored"
        );

        state.detail_run_id = Some("wf_a".to_string());
        state.normalize(&runs);
        state.rail_area = Some(Rect::new(0, 5, 20, 10));
        state.roster_area = Some(Rect::new(25, 5, 40, 10));
        assert_eq!(state.selected_phase, 1);
        state.handle_scroll(-3, 5, 8, &runs);
        assert_eq!(state.selected_phase, 0);
        assert!(state.phase_pinned, "wheel-selecting a phase pins it");

        state.handle_scroll(-3, 30, 8, &runs);
        assert_eq!(state.roster_scroll, 3);
        state.handle_scroll(3, 30, 8, &runs);
        assert_eq!(state.roster_scroll, 0);

        let before = state.clone();
        state.handle_scroll(-3, 90, 2, &runs);
        assert_eq!(state.selected_phase, before.selected_phase);
        assert_eq!(state.roster_scroll, before.roster_scroll);
        assert_eq!(state.selected_run, before.selected_run);
    }

    #[test]
    fn zero_delta_wheel_does_not_pin_phase() {
        let run = make_run("wf_1", "deep-research", "active");
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        state.rail_area = Some(Rect::new(0, 5, 20, 10));
        assert_eq!(state.selected_phase, 1);
        state.handle_scroll(0, 5, 8, &runs);
        assert!(
            !state.phase_pinned,
            "zero-delta wheel must not pin the phase"
        );
        assert_eq!(state.selected_phase, 1);
    }

    #[test]
    fn roster_wheel_pans_toward_older_rows_and_clamps() {
        let mut run = make_run("wf_big", "fanout", "active");
        run.phases = Vec::new();
        run.current_phase = None;
        run.agents = (0..30)
            .map(|i| WorkflowAgentRowView {
                agent_id: format!("a{i:02}"),
                label: format!("agent-{i:02}"),
                phase: None,
                model: None,
                state: "done".into(),
                tokens_used: 0,
                duration_ms: 0,
            })
            .collect();
        let runs = vec![&run];
        let area = Rect::new(0, 0, 100, 30);
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);

        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        let visible = state.agent_hits.len();
        assert!(visible > 0 && visible < 30, "fixture must overflow");
        let newest_first_visible = format!("a{:02}", 30 - visible);
        assert_eq!(state.agent_hits[0].1, newest_first_visible);

        state.roster_scroll = 5;
        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        assert_eq!(state.agent_hits[0].1, format!("a{:02}", 30 - visible - 5));
        let text = buf_text(&buf, area);
        assert!(text.contains("↑5"), "{text}");
        let anchored_top = state.agent_hits[0].1.clone();

        run.agents.push(WorkflowAgentRowView {
            agent_id: "a30".to_owned(),
            label: "agent-30".to_owned(),
            phase: None,
            model: None,
            state: "running".to_owned(),
            tokens_used: 0,
            duration_ms: 0,
        });
        let runs = vec![&run];
        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        assert_eq!(state.agent_hits[0].1, anchored_top);
        assert_eq!(state.roster_scroll, 6);

        state.roster_scroll = 10_000;
        state.roster_top_agent_id = None;
        let mut buf = Buffer::empty(area);
        render_workflows(
            &mut buf,
            area,
            &runs,
            &mut state,
            0,
            &WorkflowAgentLiveMap::default(),
        );
        assert_eq!(state.roster_scroll, 31 - visible);
        assert_eq!(state.agent_hits[0].1, "a00");

        state.normalize(&runs);
        assert_eq!(
            state.roster_scroll,
            31 - visible,
            "same target keeps the pan"
        );
        state.roster_anchor = Some(("other".into(), "Other phase".into()));
        state.normalize(&runs);
        assert_eq!(state.roster_scroll, 0);
    }

    #[test]
    fn detail_header_omits_agent_budget() {
        let run = make_run("wf_1", "deep-research", "active");
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        let text = render_to_text(&runs, &state);
        assert!(text.contains("1/2 agents"), "{text}");
        assert!(!text.contains("128"), "budget cap is not shown: {text}");
        assert!(!text.contains("left"), "{text}");
    }

    fn run_with_lagging_current_phase() -> WorkflowRunSnapshot {
        let mut run = make_run("wf_lag", "morefixes-quality-audit", "active");
        run.phases = vec![
            ("Export".to_owned(), "done".to_owned()),
            ("Audit".to_owned(), "active".to_owned()),
            ("Synthesize".to_owned(), "pending".to_owned()),
        ];
        run.current_phase = Some("Audit".to_owned());
        run.agents = vec![
            WorkflowAgentRowView {
                agent_id: "a1".into(),
                label: "audit-batch-0".into(),
                phase: Some("Audit".into()),
                model: None,
                state: "done".into(),
                tokens_used: 1_000,
                duration_ms: 0,
            },
            WorkflowAgentRowView {
                agent_id: "a2".into(),
                label: "synthesizer".into(),
                phase: Some("Synthesize".into()),
                model: None,
                state: "running".into(),
                tokens_used: 0,
                duration_ms: 0,
            },
        ];
        run
    }

    #[test]
    fn default_selection_follows_phase_with_running_agents() {
        let run = run_with_lagging_current_phase();
        assert_eq!(run.effective_active_phase().as_deref(), Some("Synthesize"));
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        assert_eq!(state.selected_phase_name.as_deref(), Some("Synthesize"));
    }

    #[test]
    fn pinned_phase_unpins_when_run_progresses() {
        let mut run = make_run("wf_1", "deep-research", "active");
        run.agents[1].state = "running".to_owned();
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        state.select_phase(0, &run);
        state.normalize(&runs);
        assert!(state.phase_pinned);
        assert_eq!(state.selected_phase_name.as_deref(), Some("Plan"));

        run.agents[1].state = "done".to_owned();
        run.agents.push(WorkflowAgentRowView {
            agent_id: "a3".into(),
            label: "synthesizer".into(),
            phase: Some("Synthesize".into()),
            model: None,
            state: "running".into(),
            tokens_used: 0,
            duration_ms: 0,
        });
        let runs = vec![&run];
        state.normalize(&runs);
        assert!(!state.phase_pinned);
        assert_eq!(state.selected_phase_name.as_deref(), Some("Synthesize"));
    }

    #[test]
    fn rail_marks_running_phase_and_roster_streams_live_status() {
        let run = run_with_lagging_current_phase();
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);

        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let mut live = WorkflowAgentLiveMap::default();
        live.insert(
            "a2".to_owned(),
            WorkflowAgentLiveStatus {
                activity: Some("Running: rg -n needle /data".to_owned()),
                context_tokens: Some(129_000),
                context_window_tokens: Some(500_000),
                elapsed_ms: Some(75_000),
            },
        );
        render_workflows(&mut buf, area, &runs, &mut state, 0, &live);
        let text = buf_text(&buf, area);
        assert!(
            text.contains("● 0/1"),
            "running phase gets a ● marker: {text}"
        );
        assert!(text.contains("· Running: r"), "{text}");
        assert!(
            text.contains("129k / 500k context · 1m15s"),
            "workflow rows show current context instead of cumulative usage: {text}"
        );
    }

    #[test]
    fn completed_agent_keeps_final_context_and_omits_cumulative_tokens() {
        let mut run = make_run("wf_1", "deep-research", "complete");
        run.current_phase = Some("Plan".to_owned());
        run.agents.truncate(1);
        run.agents[0].tokens_used = 375_136;
        run.agents[0].duration_ms = 75_000;
        let runs = vec![&run];
        let mut state = WorkflowsViewState::default();
        state.normalize(&runs);
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let mut live = WorkflowAgentLiveMap::default();
        live.insert(
            "a1".to_owned(),
            WorkflowAgentLiveStatus {
                context_tokens: Some(72_281),
                context_window_tokens: Some(500_000),
                elapsed_ms: Some(75_000),
                ..Default::default()
            },
        );
        render_workflows(&mut buf, area, &runs, &mut state, 0, &live);
        let text = buf_text(&buf, area);
        assert!(text.contains("72.3k / 500k context · 1m15s"), "{text}");
        assert!(!text.contains("375k"), "must omit cumulative usage: {text}");
    }

    #[test]
    fn admitted_agent_count_never_decreases() {
        let mut run = make_run("wf_1", "deep-research", "active");
        let first = run.agents_used;
        run.agents_used = first + 1;
        run.agents_remaining = Some(125);
        assert!(run.agents_used >= first);
        assert_eq!(
            run.agents_used + run.agents_reserved + run.agents_remaining.unwrap(),
            run.agent_budget.unwrap()
        );
    }

    #[test]
    fn empty_state_offers_starters() {
        let runs: Vec<&WorkflowRunSnapshot> = Vec::new();
        let state = WorkflowsViewState::default();
        let text = render_to_text(&runs, &state);
        assert!(text.contains("No workflow runs"), "{text}");
        assert!(text.contains("/deep-research"), "{text}");
    }

    #[test]
    fn detail_run_dropped_when_missing() {
        let a = make_run("wf_a", "deep-research", "active");
        let runs = vec![&a];
        let mut state = WorkflowsViewState {
            detail_run_id: Some("wf_gone".into()),
            ..Default::default()
        };
        state.normalize(&runs);
        assert_eq!(state.detail_run_id.as_deref(), Some("wf_a"));
    }

    #[test]
    fn stale_selection_clamps() {
        let a = make_run("wf_a", "deep-research", "active");
        let runs = vec![&a];
        let mut state = WorkflowsViewState {
            selected_run: 9,
            selected_phase: 9,
            selected_phase_name: Some("missing".to_owned()),
            phase_pinned: true,
            pin_active_phase: Some("Research".to_owned()),
            ..Default::default()
        };
        state.normalize(&runs);
        assert_eq!(state.selected_run, 0);
        assert_eq!(state.selected_phase, 2);
    }
}
