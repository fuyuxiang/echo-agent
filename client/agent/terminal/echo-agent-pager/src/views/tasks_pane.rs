//! Combined tasks pane — unified overlay panel showing both background tasks
//! and subagents in a single interleaved list.
//!
//! Replaces the separate `BgTaskPane` and `SubagentPane`. Items are sorted
//! running-first, then by start time (newest first). Each entry dispatches
//! to the correct action type (kill task vs kill agent, view output vs view
//! session) based on its variant.

use crossterm::event::{KeyEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::StatefulWidget;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Instant, SystemTime};
use unicode_width::UnicodeWidthStr;

use crate::app::agent::{BgTaskState, BgTaskStatus, ScheduledTaskInfo};
use crate::app::subagent::{SubagentInfo, format_context_badge, format_subagent_label};
use crate::appearance::LayoutConfig;
use crate::scrollback::layout::HorizontalLayout;
use crate::syntax::get_syntect;
use crate::theme::{Theme, ThemeKind};
use crate::util::format_duration;
use chrono::{DateTime, Utc};

use super::list_pane::{
    ListItem, ListPane, ListPaneConfig, ListPaneState, ListPaneStyle, WrapMode,
};
use super::overlay::OverlayState;

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

const SPINNER_DIVISOR: u64 = 4;

// ---------------------------------------------------------------------------
// Shell command syntax highlighting (used by other modules too)
// ---------------------------------------------------------------------------

/// Highlight a shell command string into styled spans.
///
/// Uses syntect with the best available grammar for the platform: tries
/// "powershell" first on Windows, falls back to "bash". Returns plain
/// `theme.command` color if no grammar matches. Results should be cached.
pub fn highlight_bash_command(command: &str) -> Vec<Span<'static>> {
    let syntect = get_syntect();
    let grammar = if cfg!(windows) { "powershell" } else { "bash" };
    let Some(mut hl) = syntect
        .highlight_lines_for_token(grammar)
        .or_else(|| syntect.highlight_lines_for_token("bash"))
    else {
        let theme = Theme::current();
        return vec![Span::styled(
            command.to_string(),
            Style::default().fg(theme.command),
        )];
    };

    let line = format!("{command}\n");
    match hl.highlight_line(&line, &syntect.syntax_set) {
        Ok(ranges) => {
            let mut spans = Vec::new();
            for (style, segment) in ranges {
                let mut text = segment.to_owned();
                while text.ends_with('\n') || text.ends_with('\r') {
                    text.pop();
                }
                if text.is_empty() {
                    continue;
                }
                // Raw syntect RGB here used to bypass quantization and leak
                // polarity-tuned tmTheme colors into minimal.
                spans.push(Span::styled(
                    text,
                    crate::syntax::syntect_to_ratatui_fg(style),
                ));
            }
            if spans.is_empty() {
                let theme = Theme::current();
                vec![Span::styled(
                    command.to_string(),
                    Style::default().fg(theme.command),
                )]
            } else {
                spans
            }
        }
        Err(_) => {
            let theme = Theme::current();
            vec![Span::styled(
                command.to_string(),
                Style::default().fg(theme.command),
            )]
        }
    }
}

/// Dim highlighted spans by blending each color toward background.
fn dim_spans(spans: &[Span<'static>], blend_factor: f32) -> Vec<Span<'static>> {
    let theme = Theme::current();
    spans
        .iter()
        .map(|span| {
            let fg = span
                .style
                .fg
                .and_then(|fg| crate::render::color::blend_color(theme.bg_base, fg, blend_factor))
                .or(span.style.fg);
            let style = Style::default().fg(fg.unwrap_or(theme.gray));
            Span::styled(span.content.clone(), style)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Line count badge formatting
// ---------------------------------------------------------------------------

/// Format an stdout line count as a compact `(N)` badge with SI scaling.
///
/// Returns an empty string for `0` so callers can treat that as "no badge".
/// Truncation (not rounding) is used throughout so the badge never
/// overstates the count — e.g. `1999` renders as `(1.9k)` (not `(2.0k)`),
/// `999_999` renders as `(999k)` (not `(1.0M)`), and `9_999_999` renders as
/// `(9.9M)` (not `(10M)`). Each branch boundary is exact: `1_000_000` is the
/// first count to render with an `M` suffix.
///
/// When `truncated` is `true`, a `+` is inserted before the closing paren
/// (`(2.0k+)`) to signal "at least this many" — the rolling buffer has
/// dropped data so the real total is larger than `count`.
///
/// - `<1000`:   `(42)`, `(999)`
/// - `<10_000`: `(1.0k)`, `(9.9k)`     — one decimal
/// - `<1M`:     `(10k)`, `(999k)`      — whole thousands
/// - `<10M`:    `(1.0M)`, `(9.9M)`     — one decimal
/// - `≥10M`:    `(10M)`, `(999M)`      — whole millions
fn format_line_count_badge(count: usize, truncated: bool) -> String {
    if count == 0 {
        return String::new();
    }
    let suffix = if truncated { "+" } else { "" };
    if count < 1_000 {
        return format!("({count}{suffix})");
    }
    if count < 10_000 {
        let tenths = count / 100;
        return format!("({}.{}k{suffix})", tenths / 10, tenths % 10);
    }
    if count < 1_000_000 {
        return format!("({}k{suffix})", count / 1_000);
    }
    if count < 10_000_000 {
        let tenths = count / 100_000;
        return format!("({}.{}M{suffix})", tenths / 10, tenths % 10);
    }
    format!("({}M{suffix})", count / 1_000_000)
}

// ---------------------------------------------------------------------------
// TaskEntryId — identifies which entry a button belongs to
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEntryId {
    BgTask(String),
    Agent(String),
    Scheduled(String),
    Workflow(String),
}

/// Logical group a [`TaskEntry`] belongs to. Drives both the sort order (so
/// each kind is contiguous) and the collapsible group headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKind {
    Workflows,
    Subagents,
    Tasks,
    /// Recurring background processes: `monitor` tasks and `/loop` scheduled
    /// tasks share one section. They stay contiguous (monitors first, then
    /// loops) via [`TaskEntry::type_order`].
    Watchers,
}

const GROUP_KIND_COUNT: usize = 4;

impl GroupKind {
    /// Display label shown in the group header.
    fn label(self) -> &'static str {
        match self {
            GroupKind::Workflows => "Workflows",
            GroupKind::Subagents => "Subagents",
            GroupKind::Tasks => "Tasks",
            GroupKind::Watchers => "Watchers",
        }
    }

    fn order(self) -> u8 {
        match self {
            GroupKind::Workflows => 0,
            GroupKind::Subagents => 1,
            GroupKind::Tasks => 2,
            GroupKind::Watchers => 3,
        }
    }
}

// ---------------------------------------------------------------------------
// TaskEntry — unified entry for the combined list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TaskEntry {
    BgTask {
        id: u64,
        task_id: String,
        label: String,
        styled: Line<'static>,
        running: bool,
        start_time: SystemTime,
        /// True for `monitor` tool tasks. Used to sort monitors into their
        /// own contiguous group (separate from one-shot bg commands).
        is_monitor: bool,
    },
    Agent {
        id: u64,
        subagent_id: String,
        child_session_id: String,
        label: String,
        styled: Line<'static>,
        running: bool,
        started_at: Instant,
        /// Capitalized agent-type / persona label (e.g. `Explore`, `Plan`,
        /// `General`). Used to order subagents by type within their group.
        type_label: String,
    },
    Scheduled {
        id: u64,
        task_id: String,
        label: String,
        styled: Line<'static>,
        started_at: Instant,
        linked_subagent: Option<String>,
    },
    Workflow {
        id: u64,
        name: String,
        label: String,
        styled: Line<'static>,
        running: bool,
        stoppable: bool,
        started_at: Instant,
    },
    /// Collapsible group header row (e.g. `▾ Subagents 2`). Not a task —
    /// selecting it and pressing Enter (or clicking it) toggles the group's
    /// collapse state.
    Header {
        group: GroupKind,
        styled: Line<'static>,
    },
}

impl TaskEntry {
    fn from_bg_task(
        task: &BgTaskState,
        highlight_cache: &mut HashMap<String, Vec<Span<'static>>>,
    ) -> Self {
        // Prefer the tool call's description over the raw command for the
        // pane label. The full command is always available via the block
        // viewer (preamble of the BgTaskBlock).
        let description = task
            .description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let running = task.status == BgTaskStatus::Running;
        let (label, styled) = if task.is_monitor {
            // Monitor: blue "Monitor" tag + neutral description, mirroring
            // scheduled `/loop` rows. Falls back to the command if the
            // description is somehow empty. The description (not the raw
            // command) is what we show, so it never gets bash-highlighted.
            let theme = Theme::current();
            let text = description
                .map(|d| d.replace('\n', " "))
                .unwrap_or_else(|| task.command.trim().replace('\n', " "));
            const TAG: &str = "Monitor";
            let desc_style = if running {
                Style::default().fg(theme.text_secondary)
            } else {
                Style::default().fg(theme.gray_bright)
            };
            let label = format!("{TAG} {text}");
            let styled = Line::from(vec![
                Span::styled(format!("{TAG} "), Style::default().fg(theme.accent_system)),
                Span::styled(text, desc_style),
            ]);
            (label, styled)
        } else if let Some(desc) = description {
            // Collapse newlines so multi-line descriptions render on one row.
            let one_line = desc.replace('\n', " ");
            let theme = Theme::current();
            // Prefix the description with a constant `Task` tag in the
            // theme's secondary text color so the entry type is identifiable
            // at a glance, the same way subagent rows lead with their
            // persona/role label. The prefix is included in `label` so it
            // is searchable (the tasks-pane filter matches against `label`).
            const PREFIX: &str = "Task ";
            let desc_style = if running {
                Style::default().fg(theme.text_primary)
            } else {
                Style::default().fg(theme.gray_bright)
            };
            let label = format!("{PREFIX}{one_line}");
            let styled = Line::from(vec![
                Span::styled(PREFIX, Style::default().fg(theme.text_secondary)),
                Span::styled(one_line, desc_style),
            ]);
            (label, styled)
        } else {
            let trimmed = task.command.trim();
            let label = if let Some(nl) = trimmed.find('\n') {
                let first_line = trimmed[..nl].trim_end();
                format!("{first_line}\u{2026}")
            } else {
                trimmed.to_string()
            };

            let base_spans = highlight_cache
                .entry(label.clone())
                .or_insert_with(|| highlight_bash_command(&label))
                .clone();

            let spans = if running {
                base_spans
            } else {
                dim_spans(&base_spans, 0.45)
            };
            (label, Line::from(spans))
        };

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        task.task_id.hash(&mut hasher);
        let id = hasher.finish();

        TaskEntry::BgTask {
            id,
            task_id: task.task_id.clone(),
            label,
            styled,
            running,
            start_time: task.start_time,
            is_monitor: task.is_monitor,
        }
    }

    fn from_subagent(info: &SubagentInfo) -> Self {
        let theme = Theme::current();

        // Single consolidated label (persona > role > subagent_type > tag >
        // "general") plus description with any `[tag]` prefix stripped.
        let (type_label, description) = format_subagent_label(info);
        let model_suffix = info
            .model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("");

        // Label color is state-driven: pending_kill / running stay vivid;
        // completed / failed keep their hue (green / red) but blend toward
        // the background so finished entries recede without losing their
        // success-vs-failure signal.
        let raw_type_color = if info.pending_kill {
            theme.accent_error
        } else if info.is_running() {
            theme.accent_running
        } else if info.status.as_deref() == Some("completed") {
            theme.accent_success
        } else {
            theme.accent_error
        };
        let type_color = if info.is_running() || info.pending_kill {
            raw_type_color
        } else {
            crate::render::color::blend_color(theme.bg_base, raw_type_color, 0.45)
                .unwrap_or(raw_type_color)
        };
        let type_style = Style::default().fg(type_color);
        let desc_style = if info.is_running() {
            Style::default().fg(theme.text_primary)
        } else {
            Style::default().fg(theme.gray_bright)
        };

        // Live activity suffix, running rows only. The description is capped
        // so the live part survives typical pane widths (the right overlay's
        // end-truncation remains the final safety net); the suffix stays out
        // of `label` so filter matches don't flicker as activity changes.
        const ACTIVITY_DESC_MAX_WIDTH: usize = 40;
        let activity = info
            .is_running()
            .then_some(info.activity_label.as_deref())
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let shown_desc = match activity {
            Some(_) => {
                crate::render::line_utils::truncate_str(&description, ACTIVITY_DESC_MAX_WIDTH)
            }
            None => description.clone(),
        };

        // Skip the trailing-space separator when the cleaned description is
        // empty (reachable when `info.description == "[tag]"`); otherwise we
        // render `"Tag "` with a stray trailing space.
        //
        // The model is NOT rendered inline here — it's drawn right-aligned in
        // the overlay (just to the left of the elapsed/duration). The label
        // string below still includes the model so it remains searchable.
        let type_sep = if description.is_empty() { "" } else { " " };
        let mut spans = vec![
            Span::styled(format!("{type_label}{type_sep}"), type_style),
            Span::styled(shown_desc, desc_style),
        ];
        if let Some(activity) = activity {
            spans.push(Span::styled(
                format!(" \u{00b7} {activity}"),
                Style::default().fg(theme.gray),
            ));
        }

        let label = match (description.is_empty(), model_suffix.is_empty()) {
            (true, true) => type_label.clone(),
            (true, false) => format!("{type_label} {model_suffix}"),
            (false, true) => format!("{type_label} {description}"),
            (false, false) => format!("{type_label} {description} {model_suffix}"),
        };
        let styled = Line::from(spans);

        // Use a different hash namespace to avoid collisions with bg tasks
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        "agent:".hash(&mut hasher);
        info.child_session_id.hash(&mut hasher);
        let id = hasher.finish();

        TaskEntry::Agent {
            id,
            subagent_id: info.subagent_id.to_string(),
            child_session_id: info.child_session_id.to_string(),
            label,
            styled,
            running: info.is_running(),
            started_at: info.started_at,
            type_label,
        }
    }

    fn from_workflow_run(run: &crate::views::workflows::WorkflowRunSnapshot) -> Self {
        let theme = Theme::current();
        let running = run.is_active();

        let raw_tag_color = if running {
            theme.accent_running
        } else if run.status == "complete" {
            theme.accent_success
        } else if run.is_terminal() {
            theme.accent_error
        } else {
            theme.warning
        };
        let tag_color = if running {
            raw_tag_color
        } else {
            crate::render::color::blend_color(theme.bg_base, raw_tag_color, 0.45)
                .unwrap_or(raw_tag_color)
        };
        let name_style = if running {
            Style::default().fg(theme.text_primary)
        } else {
            Style::default().fg(theme.gray_bright)
        };

        let suffix = if running {
            let phase = run
                .current_phase
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty());
            let agents = match run.agents.iter().filter(|a| a.state == "running").count() {
                0 => None,
                1 => Some("1 agent".to_string()),
                n => Some(format!("{n} agents")),
            };
            match (phase, agents) {
                (Some(p), Some(a)) => format!("{p} · {a}"),
                (Some(p), None) => p.to_string(),
                (None, Some(a)) => a,
                (None, None) => "running".to_string(),
            }
        } else {
            run.status.replace('_', " ")
        };

        let mut spans = vec![
            Span::styled("Workflow ".to_string(), Style::default().fg(tag_color)),
            Span::styled(run.name.clone(), name_style),
        ];
        if !suffix.is_empty() {
            spans.push(Span::styled(
                format!(" \u{00b7} {suffix}"),
                Style::default().fg(theme.gray),
            ));
        }

        let label = format!("Workflow {} {suffix}", run.name);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        "workflow:".hash(&mut hasher);
        run.run_id.hash(&mut hasher);
        let id = hasher.finish();

        TaskEntry::Workflow {
            id,
            name: run.name.clone(),
            label,
            styled: Line::from(spans),
            running,
            stoppable: run.can_stop(),
            started_at: Instant::now()
                .checked_sub(std::time::Duration::from_millis(run.live_elapsed_ms()))
                .unwrap_or_else(Instant::now),
        }
    }

    fn from_scheduled(
        info: &ScheduledTaskInfo,
        current_cron: Option<&str>,
        is_queued: bool,
        linked: Option<(String, bool)>,
    ) -> Self {
        let linked_running = linked.as_ref().is_some_and(|(_, running)| *running);
        let theme = Theme::current();
        let prompt_preview = if info.prompt.chars().count() > 60 {
            info.prompt.chars().take(57).collect::<String>() + "..."
        } else {
            info.prompt.clone()
        };
        let countdown = |schedule: &str, created: std::time::Instant| -> String {
            if let Some(secs) = crate::util::parse_schedule_interval_secs(schedule) {
                let approx = created + std::time::Duration::from_secs(secs);
                let now = std::time::Instant::now();
                if approx > now {
                    format!(" (next in {})", format_duration(approx.duration_since(now)))
                } else {
                    " (due now)".to_string()
                }
            } else {
                String::new()
            }
        };
        let is_provisional = info.task_id.starts_with("provisional-");
        let suffix = if current_cron == Some(&info.task_id) || linked_running {
            " (running)".to_string()
        } else if is_queued {
            " (queued)".to_string()
        } else if is_provisional {
            " (starting)".to_string()
        } else if let Some(n) = &info.next_fire_at {
            if let Ok(dt) = DateTime::<chrono::FixedOffset>::parse_from_rfc3339(n) {
                let dt = dt.with_timezone(&Utc);
                let now = Utc::now();
                if dt > now {
                    let dur = (dt - now).to_std().unwrap_or_default();
                    format!(" (next in {})", format_duration(dur))
                } else {
                    " (due now)".to_string()
                }
            } else {
                countdown(&info.human_schedule, info.created_at)
            }
        } else {
            countdown(&info.human_schedule, info.created_at)
        };
        // Capitalize the tag for display (`loop` → `Loop`) so it reads as a
        // proper label, matching the monitor row's `Monitor` tag.
        let tag_display = {
            let mut chars = info.tag.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        };
        let label = format!(
            "{} {} \u{b7} {}{}",
            tag_display, info.human_schedule, &prompt_preview, &suffix
        );

        // Only the tag (e.g. `Loop`) carries color — the blue system accent.
        // The schedule, prompt preview, and status suffix all render in the
        // neutral secondary text color so the row reads calmly with a single
        // point of color. No surrounding `[ ]` brackets: the color alone
        // sets the tag apart from the schedule that follows it.
        let schedule_style = format!("{} \u{b7} ", info.human_schedule);
        let neutral = Style::default().fg(theme.text_secondary);
        let styled = Line::from(vec![
            Span::styled(
                format!("{} ", tag_display),
                Style::default().fg(theme.accent_system),
            ),
            Span::styled(schedule_style, neutral),
            Span::styled(prompt_preview, neutral),
            if !suffix.is_empty() {
                Span::styled(suffix, neutral)
            } else {
                Span::raw("")
            },
        ]);

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        "sched:".hash(&mut hasher);
        info.task_id.hash(&mut hasher);
        let id = hasher.finish();

        TaskEntry::Scheduled {
            id,
            task_id: info.task_id.clone(),
            label,
            styled,
            started_at: info.created_at,
            linked_subagent: linked.map(|(sid, _)| sid),
        }
    }

    /// Build a collapsible group header row, e.g. `▾ Subagents 2` (expanded)
    /// or `▸ Subagents 2` (collapsed). The chevron + count are baked into the
    /// styled line; the label aligns with item labels (the `chevron + space`
    /// prefix is the same width as an item's 2-space indent).
    fn header(group: GroupKind, count: usize, collapsed: bool) -> Self {
        let theme = Theme::current();
        let chevron = if collapsed { "\u{25B8} " } else { "\u{25BE} " };
        let styled = Line::from(vec![
            Span::styled(chevron, Style::default().fg(theme.gray)),
            Span::styled(
                group.label(),
                Style::default()
                    .fg(theme.gray_bright)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {count}"), Style::default().fg(theme.gray)),
        ]);
        TaskEntry::Header { group, styled }
    }

    /// Which collapsible group this entry belongs to.
    fn group_kind(&self) -> GroupKind {
        match self {
            TaskEntry::Agent { .. } => GroupKind::Subagents,
            TaskEntry::BgTask {
                is_monitor: false, ..
            } => GroupKind::Tasks,
            TaskEntry::BgTask {
                is_monitor: true, ..
            } => GroupKind::Watchers,
            TaskEntry::Scheduled { .. } => GroupKind::Watchers,
            TaskEntry::Workflow { .. } => GroupKind::Workflows,
            TaskEntry::Header { group, .. } => *group,
        }
    }

    fn is_running(&self) -> bool {
        match self {
            TaskEntry::BgTask { running, .. }
            | TaskEntry::Agent { running, .. }
            | TaskEntry::Workflow { running, .. } => *running,
            TaskEntry::Scheduled { .. } => true,
            TaskEntry::Header { .. } => false,
        }
    }

    /// Fine-grained sort rank, distinct per task kind so each renders as a
    /// contiguous block: subagents (0) → one-shot bg tasks (1) → monitors
    /// (2) → scheduled/loops (3). Monitors and loops share the `Watchers`
    /// group/header but keep distinct ranks so monitors always sort before
    /// loops within that section.
    fn type_order(&self) -> u8 {
        match self {
            TaskEntry::Workflow { .. } => 0,
            TaskEntry::Agent { .. } => 1,
            TaskEntry::BgTask {
                is_monitor: false, ..
            } => 2,
            TaskEntry::BgTask {
                is_monitor: true, ..
            } => 3,
            TaskEntry::Scheduled { .. } => 4,
            // Headers never appear in the sorted `items` list; fall back to
            // the group's coarse order for completeness.
            TaskEntry::Header { group, .. } => group.order(),
        }
    }
}

impl ListItem for TaskEntry {
    fn content(&self) -> &Line<'_> {
        match self {
            TaskEntry::BgTask { styled, .. }
            | TaskEntry::Agent { styled, .. }
            | TaskEntry::Scheduled { styled, .. }
            | TaskEntry::Workflow { styled, .. }
            | TaskEntry::Header { styled, .. } => styled,
        }
    }

    fn prefix(&self) -> Option<Line<'_>> {
        match self {
            // Headers sit flush-left; their chevron occupies the same two
            // columns as an item's indent, so labels still line up.
            TaskEntry::Header { .. } => None,
            _ => Some(Line::from(Span::raw("  "))),
        }
    }

    fn stable_id(&self) -> u64 {
        match self {
            TaskEntry::BgTask { id, .. }
            | TaskEntry::Agent { id, .. }
            | TaskEntry::Scheduled { id, .. }
            | TaskEntry::Workflow { id, .. } => *id,
            TaskEntry::Header { group, .. } => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                "header:".hash(&mut hasher);
                group.order().hash(&mut hasher);
                hasher.finish()
            }
        }
    }

    fn is_selectable(&self) -> bool {
        true
    }

    fn search_text(&self) -> &str {
        match self {
            TaskEntry::BgTask { label, .. }
            | TaskEntry::Agent { label, .. }
            | TaskEntry::Scheduled { label, .. }
            | TaskEntry::Workflow { label, .. } => label,
            TaskEntry::Header { group, .. } => group.label(),
        }
    }
}

// ---------------------------------------------------------------------------
// TasksPane
// ---------------------------------------------------------------------------

/// Temporary data for the overlay pass (avoids borrowing entries during mutation).
enum OverlayEntryData {
    BgTask(String),
    Agent(String, String),
    Scheduled(String, Option<String>),
    Workflow(String),
}

const MAX_TASKS_HEIGHT: u16 = 8;
const MAX_TASKS_FRACTION: f32 = 0.15;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TaskStatusCounts {
    pub(crate) running: usize,
    pub(crate) paused_workflows: usize,
}

pub struct TasksPane {
    /// Display list: sorted `items` with group headers inserted and
    /// collapsed groups' items removed. This is what the `ListPane` renders.
    entries: Vec<TaskEntry>,
    /// Sorted task items only (no headers). `entries` is derived from this by
    /// [`Self::rebuild_entries`]; kept so collapse toggles can rebuild the
    /// display list without re-reading the live task data.
    items: Vec<TaskEntry>,
    /// Groups the user has collapsed (header shown, items hidden).
    collapsed_groups: std::collections::HashSet<GroupKind>,
    pub list_state: ListPaneState,
    list_style: ListPaneStyle,
    show_done: bool,
    pub overlay: OverlayState,
    tick: u64,
    pub kill_button_rects: Vec<(TaskEntryId, Rect)>,
    pub view_button_rects: Vec<(TaskEntryId, Rect)>,
    pub hovered_kill: Option<TaskEntryId>,
    pub hovered_view: Option<TaskEntryId>,
    prev_running_count: usize,
    opened_by_auto: bool,
    highlight_cache: HashMap<String, Vec<Span<'static>>>,
    last_theme: ThemeKind,
    workflow_runs: Vec<crate::views::workflows::WorkflowRunSnapshot>,
}

impl Default for TasksPane {
    fn default() -> Self {
        Self::new()
    }
}

/// Fill overlay cells with spaces so label text doesn't bleed through.
///
/// When the label extends into the area we're about to clear, place an
/// ellipsis `…` at the cell immediately to the left of the clear boundary so
/// the user sees that the row was truncated rather than just clipped silently.
/// The ellipsis inherits the style of the cell it lands on so it blends with
/// the surrounding label text.
fn clear_overlay_area(buf: &mut Buffer, area: Rect, y: u16, overlay_w: u16) {
    let clamped = overlay_w.min(area.width);
    if clamped == 0 {
        return;
    }
    let clear_x = area.x + area.width - clamped;

    // Detect truncation BEFORE clearing: if the cell at `clear_x` contains
    // non-blank label content, the label is wider than the row minus the
    // overlay reservation. Capture the style at `clear_x - 1` (the cell that
    // will host the ellipsis) so the inserted `…` matches the label color.
    let needs_ellipsis = clear_x > area.x
        && buf
            .cell((clear_x, y))
            .map(|c| !c.symbol().trim().is_empty())
            .unwrap_or(false);
    let ellipsis_style = if needs_ellipsis {
        buf.cell((clear_x - 1, y))
            .map(|c| c.style())
            .unwrap_or_default()
    } else {
        Style::default()
    };

    let blanks = " ".repeat(clamped as usize);
    buf.set_span(clear_x, y, &Span::raw(blanks), clamped);

    if needs_ellipsis {
        buf.set_span(clear_x - 1, y, &Span::styled("\u{2026}", ellipsis_style), 1);
    }
}

/// Draw a single scroll indicator glyph centered on row `y`, blanking the rest
/// of the row so it reads as a dedicated, easy-to-see indicator row — the same
/// ▲/▼ as the corner indicators, just centered for visibility.
fn draw_centered_arrow(buf: &mut Buffer, area: Rect, y: u16, arrow: &str, color: Color) {
    if area.width == 0 {
        return;
    }
    let blanks = " ".repeat(area.width as usize);
    buf.set_span(area.x, y, &Span::raw(blanks), area.width);
    let cx = area.x + area.width / 2;
    buf.set_span(
        cx,
        y,
        &Span::styled(arrow.to_string(), Style::default().fg(color)),
        1,
    );
}
impl TasksPane {
    pub fn new() -> Self {
        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: false,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: false,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
        list_state.set_clipboard_provider(Box::new(crate::clipboard::SystemClipboard));
        // This pane draws the scroll indicators (▲/▼) centered on dedicated
        // rows, so the generic right-corner indicators are suppressed.
        let list_style = ListPaneStyle {
            show_corner_indicators: false,
            ..ListPaneStyle::default()
        };
        Self {
            entries: Vec::new(),
            items: Vec::new(),
            collapsed_groups: std::collections::HashSet::new(),
            list_state,
            list_style,
            show_done: false,
            overlay: OverlayState::hidden(),
            tick: 0,
            kill_button_rects: Vec::new(),
            view_button_rects: Vec::new(),
            hovered_kill: None,
            hovered_view: None,
            prev_running_count: 0,
            opened_by_auto: false,
            highlight_cache: HashMap::new(),
            last_theme: Theme::current_kind(),
            workflow_runs: Vec::new(),
        }
    }

    // -- Data sync -----------------------------------------------------------

    pub fn sync(
        &mut self,
        bg_tasks: &std::collections::BTreeMap<String, BgTaskState>,
        subagents: &HashMap<String, SubagentInfo>,
        scheduled: &HashMap<String, ScheduledTaskInfo>,
        current_cron_task_id: Option<&str>,
        queued_cron_ids: &std::collections::HashSet<&str>,
        workflow_runs: &[crate::views::workflows::WorkflowRunSnapshot],
    ) {
        // Detect theme switch and refresh caches.
        let current_theme = Theme::current_kind();
        if current_theme != self.last_theme {
            self.last_theme = current_theme;
            self.list_style = ListPaneStyle {
                show_corner_indicators: false,
                ..ListPaneStyle::default()
            };
            self.highlight_cache.clear();
        }

        self.items.clear();

        // Add bg task items
        for task in bg_tasks.values() {
            if self.show_done || task.status == BgTaskStatus::Running {
                self.items
                    .push(TaskEntry::from_bg_task(task, &mut self.highlight_cache));
            }
        }

        for info in subagents.values() {
            if info.workflow_run_id.is_some() {
                continue;
            }
            if self.show_done || info.is_running() {
                self.items.push(TaskEntry::from_subagent(info));
            }
        }

        // Add scheduled task items (always "running")
        for info in scheduled.values() {
            let linked = info.last_subagent_id.as_deref().and_then(|sid| {
                subagents
                    .values()
                    .find(|s| s.subagent_id.as_ref() == sid)
                    .map(|s| (sid.to_string(), s.is_running()))
            });
            self.items.push(TaskEntry::from_scheduled(
                info,
                current_cron_task_id,
                queued_cron_ids.contains(info.task_id.as_str()),
                linked,
            ));
        }

        self.workflow_runs = workflow_runs.to_vec();
        for run in workflow_runs {
            if self.show_done || !run.is_terminal() {
                self.items.push(TaskEntry::from_workflow_run(run));
            }
        }

        // Sort: group by type first (subagents → tasks → monitors →
        // scheduled) so each kind is one contiguous block, then running
        // before done within each group, then newest-first, then a stable
        // id tiebreak. Monitors and scheduled/loops render under one shared
        // "Watchers" header but keep distinct ranks (monitors first).
        self.items.sort_by(|a, b| {
            // 1. Group by type so each kind is one contiguous block:
            //    subagents → tasks → monitors → scheduled.
            a.type_order()
                .cmp(&b.type_order())
                // 2. Running before done *within* each group.
                .then_with(|| b.is_running().cmp(&a.is_running()))
                // 3. Within a (group, run-state): subagents order by agent
                //    type (alphabetical) then newest-first; tasks/monitors/
                //    loops order newest-first. Avoids mixing SystemTime and
                //    Instant across types.
                .then_with(|| match (a, b) {
                    (
                        TaskEntry::Agent {
                            type_label: ta,
                            started_at: sa,
                            ..
                        },
                        TaskEntry::Agent {
                            type_label: tb,
                            started_at: sb,
                            ..
                        },
                    ) => ta.cmp(tb).then_with(|| sb.cmp(sa)),
                    (
                        TaskEntry::Scheduled { started_at: a, .. },
                        TaskEntry::Scheduled { started_at: b, .. },
                    ) => b.cmp(a),
                    (
                        TaskEntry::BgTask { start_time: a, .. },
                        TaskEntry::BgTask { start_time: b, .. },
                    ) => b.cmp(a),
                    (
                        TaskEntry::Workflow { started_at: a, .. },
                        TaskEntry::Workflow { started_at: b, .. },
                    ) => b.cmp(a),
                    _ => std::cmp::Ordering::Equal,
                })
                // 4. Stable tiebreak so equal-timestamp rows don't reshuffle
                //    frame-to-frame.
                .then_with(|| a.stable_id().cmp(&b.stable_id()))
        });

        // Auto-expand: forget the collapse state of any group that no longer
        // has items, so when items later return (e.g. a new subagent spawns
        // after the group emptied) the group reappears expanded instead of
        // hidden under a stale collapsed header.
        if !self.collapsed_groups.is_empty() {
            let mut present = [false; GROUP_KIND_COUNT];
            for it in &self.items {
                present[it.group_kind().order() as usize] = true;
            }
            self.collapsed_groups
                .retain(|g| present[g.order() as usize]);
        }

        // Build the display list: group headers + (non-collapsed) items.
        self.rebuild_entries();

        let counts = Self::status_counts_from(bg_tasks, subagents, scheduled, workflow_runs);
        // Replay-restored bg tasks are ambient history and must not auto-open on resume.
        let running_count = counts.running.saturating_sub(
            bg_tasks
                .values()
                .filter(|task| task.status == BgTaskStatus::Running && task.restored_from_replay)
                .count(),
        );

        if running_count > 0 && self.prev_running_count == 0 {
            self.overlay.show();
            self.opened_by_auto = true;
        }

        if running_count == 0
            && counts.paused_workflows == 0
            && self.overlay.visible
            && !self.overlay.focused
            && self.opened_by_auto
            && !self.show_done
        {
            self.overlay.visible = false;
            self.opened_by_auto = false;
        }

        self.prev_running_count = running_count;
    }

    /// Rebuild the display `entries` from the sorted `items`: insert a header
    /// at each group boundary (with the group's item count), and include a
    /// group's items only when the group is not collapsed. Relies on `items`
    /// already being sorted so each group is contiguous.
    fn rebuild_entries(&mut self) {
        self.entries.clear();
        // Per-group item counts (indexed by `GroupKind::order`).
        let mut counts: [usize; GROUP_KIND_COUNT] = [0; GROUP_KIND_COUNT];
        for it in &self.items {
            counts[it.group_kind().order() as usize] += 1;
        }
        let mut last: Option<GroupKind> = None;
        for it in &self.items {
            let group = it.group_kind();
            if last != Some(group) {
                let collapsed = self.collapsed_groups.contains(&group);
                self.entries.push(TaskEntry::header(
                    group,
                    counts[group.order() as usize],
                    collapsed,
                ));
                last = Some(group);
            }
            if !self.collapsed_groups.contains(&group) {
                self.entries.push(it.clone());
            }
        }
    }

    /// Toggle a group's collapse state (used by Enter / Ctrl-F / click).
    pub fn toggle_group(&mut self, group: GroupKind) {
        let collapsed = self.collapsed_groups.contains(&group);
        self.set_group_collapsed(group, !collapsed);
    }

    /// Explicitly set a group's collapse state (used by ← / →, which collapse
    /// and expand respectively rather than toggle). When the state changes,
    /// rebuild the display list and keep the group's header selected so the
    /// cursor doesn't jump. Returns `true` if the state actually changed.
    pub fn set_group_collapsed(&mut self, group: GroupKind, collapsed: bool) -> bool {
        let changed = if collapsed {
            self.collapsed_groups.insert(group)
        } else {
            self.collapsed_groups.remove(&group)
        };
        if changed {
            self.rebuild_entries();
            if let Some(header) = self
                .entries
                .iter()
                .find(|e| matches!(e, TaskEntry::Header { group: g, .. } if *g == group))
            {
                let id = header.stable_id();
                self.list_state.select_by_id(id);
            }
        }
        changed
    }

    /// If the selected entry is a group header, return its group.
    pub fn selected_header_group(&self) -> Option<GroupKind> {
        match self.selected_entry()? {
            TaskEntry::Header { group, .. } => Some(*group),
            _ => None,
        }
    }

    pub(crate) fn status_counts(
        &self,
        bg_tasks: &std::collections::BTreeMap<String, BgTaskState>,
        subagents: &HashMap<String, SubagentInfo>,
        scheduled: &HashMap<String, ScheduledTaskInfo>,
        workflow_runs: &[crate::views::workflows::WorkflowRunSnapshot],
    ) -> TaskStatusCounts {
        Self::status_counts_from(bg_tasks, subagents, scheduled, workflow_runs)
    }

    fn status_counts_from(
        bg_tasks: &std::collections::BTreeMap<String, BgTaskState>,
        subagents: &HashMap<String, SubagentInfo>,
        scheduled: &HashMap<String, ScheduledTaskInfo>,
        workflow_runs: &[crate::views::workflows::WorkflowRunSnapshot],
    ) -> TaskStatusCounts {
        TaskStatusCounts {
            running: bg_tasks
                .values()
                .filter(|task| task.status == BgTaskStatus::Running)
                .count()
                + subagents
                    .values()
                    .filter(|subagent| subagent.is_running() && subagent.workflow_run_id.is_none())
                    .count()
                + scheduled.len()
                + workflow_runs.iter().filter(|run| run.is_active()).count(),
            paused_workflows: workflow_runs
                .iter()
                .filter(|run| !run.is_active() && !run.is_terminal())
                .count(),
        }
    }

    // -- Visibility ----------------------------------------------------------

    pub fn show_done(&self) -> bool {
        self.show_done
    }

    pub fn is_visible(&self) -> bool {
        self.overlay.visible
    }

    pub fn on_state_change(&mut self) {
        if !self.overlay.visible {
            self.list_state.close_input_bar();
        }
        self.opened_by_auto = false;
    }

    pub fn desired_height(&self, view_height: u16) -> u16 {
        if !self.overlay.visible {
            return 0;
        }
        if view_height < 12 {
            return 0;
        }
        let count = self.entries.len();
        if count == 0 {
            return 1;
        }
        let fraction_cap = (view_height as f32 * MAX_TASKS_FRACTION).floor() as u16;
        let max = MAX_TASKS_HEIGHT.min(fraction_cap).max(1);
        // Reserve one extra row for the search/filter input bar (or an
        // accepted matcher's status line) so it gets its own line instead of
        // displacing the last task/agent entry. `ListPane` carves the bar out
        // of the bottom of the area it's given, so without this the pane would
        // show one fewer entry the moment `/` (or `f`) is pressed.
        let bar = u16::from(
            self.list_state.input_mode().is_some() || self.list_state.matcher().is_some(),
        );
        (count as u16).min(max).max(1) + bar
    }

    // -- Tick ----------------------------------------------------------------

    pub fn tick(&mut self) -> bool {
        self.tick += 1;
        self.entries.iter().any(|e| e.is_running())
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    pub fn needs_tick(&self) -> bool {
        self.entries.iter().any(|e| e.is_running())
    }

    // -- Input handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        if crate::key!('h').matches(key) && self.list_state.input_mode().is_none() {
            self.show_done = !self.show_done;
            return true;
        }
        if self.entries.is_empty() {
            return false;
        }
        self.list_state.handle_key_event(key, &self.entries)
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        self.list_state.handle_paste(text, &self.entries)
    }

    pub fn handle_scroll(&mut self, lines: i32, col: u16, row: u16) {
        let max = match self.list_state.viewport_height() {
            0..=5 => 1,
            6..=10 => 2,
            _ => lines.unsigned_abs() as i32,
        };
        let capped = lines.signum() * lines.abs().min(max);
        self.list_state
            .handle_scroll_event(capped, col, row, &self.entries);
    }

    pub fn handle_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16, area: Rect) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.list_state
            .handle_mouse_event(kind, col, row, area, &self.entries)
    }

    /// Get the selected entry (if any).
    pub fn selected_entry(&self) -> Option<&TaskEntry> {
        let sel = self.list_state.selected_index()?;
        self.entries.get(sel)
    }

    /// Get the task_id if the selected entry is a BgTask.
    pub fn selected_task_id(&self) -> Option<&str> {
        match self.selected_entry()? {
            TaskEntry::BgTask { task_id, .. } => Some(task_id),
            _ => None,
        }
    }

    /// Get the subagent_id if the selected entry is an Agent.
    pub fn selected_subagent_id(&self) -> Option<&str> {
        match self.selected_entry()? {
            TaskEntry::Agent { subagent_id, .. } => Some(subagent_id),
            _ => None,
        }
    }

    /// Get the child_session_id if the selected entry is an Agent.
    pub fn selected_child_session_id(&self) -> Option<&str> {
        match self.selected_entry()? {
            TaskEntry::Agent {
                child_session_id, ..
            } => Some(child_session_id),
            _ => None,
        }
    }

    // -- Rendering -----------------------------------------------------------

    fn content_area(area: Rect, layout_cfg: &LayoutConfig) -> Rect {
        let pad_left = HorizontalLayout::ACCENT + layout_cfg.block_pad_left;
        let pad_right = layout_cfg.block_pad_right;
        Rect {
            x: area.x + pad_left,
            y: area.y,
            width: area.width.saturating_sub(pad_left + pad_right),
            height: area.height,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        focused: bool,
        layout_cfg: &LayoutConfig,
        bg_tasks: &std::collections::BTreeMap<String, BgTaskState>,
        subagents: &HashMap<String, SubagentInfo>,
        scheduled: &HashMap<String, ScheduledTaskInfo>,
    ) {
        let inner = Self::content_area(area, layout_cfg);
        if self.entries.is_empty() {
            if inner.height > 0 && inner.width > 0 {
                let theme = Theme::current();
                if self.show_done {
                    let span = Span::styled(
                        "No tasks or agents.",
                        Style::default().fg(theme.gray_bright),
                    );
                    buf.set_span(inner.x, inner.y, &span, inner.width);
                } else {
                    let muted = Style::default().fg(theme.gray_bright);
                    let key_style = Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(Modifier::BOLD);
                    let line = Line::from(vec![
                        Span::styled("No running tasks. Press ", muted),
                        Span::styled("h", key_style),
                        Span::styled(" to show all.", muted),
                    ]);
                    buf.set_line(inner.x, inner.y, &line, inner.width);
                }
            }
            return;
        }

        // Decide the reserved indicator rows from the CURRENT scroll offset
        // (NoWrap ⇒ one row per entry, so the total is the entry count), then
        // prepare the layout exactly once with the final viewport. Preparing
        // first with the full `inner` height would clamp the offset to that
        // larger viewport's max, leaving the offset short of the smaller
        // viewport's bottom — so ▼ could never turn off at the end.
        let total = self.entries.len();
        let scrollable = total > inner.height as usize && inner.height >= 3;
        let scroll = self.list_state.scroll_offset();

        // Reserve a row for a centered ▲ / ▼ indicator ONLY when that indicator
        // is actually shown — no blank reserved rows. The top row appears when
        // scrolled down; the bottom row appears when, after the top
        // reservation, content still extends past the viewport.
        let reserve_top = scrollable && scroll > 0;
        let top = u16::from(reserve_top);
        let rows_without_bottom = inner.height.saturating_sub(top) as usize;
        let reserve_bottom = scrollable && scroll + rows_without_bottom < total;
        let bottom = u16::from(reserve_bottom);

        let list_area = Rect {
            x: inner.x,
            y: inner.y + top,
            width: inner.width,
            height: inner.height - top - bottom,
        };
        self.list_state
            .prepare_layout(&self.entries, list_area.width, list_area.height);

        // ListPane draws its scrollbar in the last column of the area it's
        // given. The overlay (right-aligned kill/view buttons) paints over
        // that same column, covering the scrollbar. Hand the list an area
        // extended one column into the right padding so the scrollbar lands
        // just past the overlay's right edge.
        //
        // Only widen when the list will *actually* draw a scrollbar there
        // (content overflows the viewport). When it won't, ListPane gives the
        // full area to content — so the extra column would be filled with
        // label text that bleeds one cell past the overlay's `[✗]` button
        // (the overlay only clears within `list_area`). Keeping `lp_area ==
        // list_area` in that case lets the overlay truncate the label cleanly
        // before the button, with nothing rendered to its right.
        let needs_scrollbar = total > list_area.height as usize;
        let lp_area = if needs_scrollbar && list_area.right() < area.right() {
            Rect {
                width: list_area.width + 1,
                ..list_area
            }
        } else {
            list_area
        };
        ListPane::new(&self.entries)
            .focused(focused)
            .style(self.list_style)
            .render(lp_area, buf, &mut self.list_state);

        // The right-corner indicators (▲/▼) are suppressed for this pane;
        // instead we draw the same glyphs, in the same color, centered on the
        // reserved row(s) so they're easier to see.
        let arrow_color = self.list_style.indicator_fg;
        if reserve_top {
            draw_centered_arrow(buf, inner, inner.y, "\u{25B2}", arrow_color);
        }
        if reserve_bottom {
            draw_centered_arrow(
                buf,
                inner,
                inner.y + inner.height - 1,
                "\u{25BC}",
                arrow_color,
            );
        }

        // Overlay pass — positioned over the list area so the kill/view button
        // rows line up with the rows the list actually rendered. When the
        // search/filter input bar is open (or an accepted matcher's status is
        // shown), `ListPane` reserves the bottom row(s) of `list_area` for it,
        // so shrink the overlay area to match — otherwise the spinner icons
        // and kill/view buttons paint over the input bar (e.g. the `⸬` spinner
        // corrupting `search:` into `⸬earch:`).
        let bar_height = self.list_state.bottom_bar_height(list_area.height);
        let overlay_area = Rect {
            height: list_area.height.saturating_sub(bar_height),
            ..list_area
        };
        self.render_overlay(overlay_area, buf, bg_tasks, subagents, scheduled);
    }

    fn render_overlay(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        bg_tasks: &std::collections::BTreeMap<String, BgTaskState>,
        subagents: &HashMap<String, SubagentInfo>,
        _scheduled: &HashMap<String, ScheduledTaskInfo>,
    ) {
        let theme = Theme::current();
        let scroll_offset = self.list_state.scroll_offset();
        self.kill_button_rects.clear();
        self.view_button_rects.clear();

        // Collect entry data first to avoid borrowing self.entries during mutation.
        let visible: Vec<(u16, OverlayEntryData)> = self
            .entries
            .iter()
            .skip(scroll_offset)
            .enumerate()
            .take_while(|(vis_row, _)| area.y + (*vis_row as u16) < area.y + area.height)
            .filter_map(|(vis_row, entry)| {
                let y = area.y + vis_row as u16;
                let data = match entry {
                    TaskEntry::BgTask { task_id, .. } => OverlayEntryData::BgTask(task_id.clone()),
                    TaskEntry::Agent {
                        subagent_id,
                        child_session_id,
                        ..
                    } => OverlayEntryData::Agent(subagent_id.clone(), child_session_id.clone()),
                    TaskEntry::Scheduled {
                        task_id,
                        linked_subagent,
                        ..
                    } => OverlayEntryData::Scheduled(task_id.clone(), linked_subagent.clone()),
                    TaskEntry::Workflow { name, .. } => OverlayEntryData::Workflow(name.clone()),
                    // Group headers have no kill/view buttons; they still
                    // occupy a row (vis_row is enumerated before this filter),
                    // so the y offsets for following items stay correct.
                    TaskEntry::Header { .. } => return None,
                };
                Some((y, data))
            })
            .collect();

        for (y, data) in visible {
            match data {
                OverlayEntryData::BgTask(ref task_id) => {
                    let Some(task) = bg_tasks.get(task_id) else {
                        continue;
                    };
                    self.render_bg_task_overlay(area, buf, y, task_id, task, &theme);
                }
                OverlayEntryData::Agent(ref subagent_id, ref child_session_id) => {
                    let Some(info) = subagents.get(child_session_id) else {
                        continue;
                    };
                    self.render_agent_overlay(area, buf, y, subagent_id, info, &theme);
                }
                OverlayEntryData::Scheduled(ref task_id, ref linked_subagent) => {
                    self.render_scheduled_overlay(
                        area,
                        buf,
                        y,
                        task_id,
                        linked_subagent.as_deref(),
                        &theme,
                    );
                }
                OverlayEntryData::Workflow(ref name) => {
                    let Some(run) = self.workflow_runs.iter().find(|r| r.name == *name).cloned()
                    else {
                        continue;
                    };
                    self.render_workflow_overlay(area, buf, y, &run, &theme);
                }
            }
        }
    }

    fn render_workflow_overlay(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        y: u16,
        run: &crate::views::workflows::WorkflowRunSnapshot,
        theme: &Theme,
    ) {
        let running = run.is_active();
        let elapsed = format_duration(std::time::Duration::from_millis(run.live_elapsed_ms()));
        let (icon, icon_style) = if running {
            let frames = crate::glyphs::dot_spinner_frames();
            let frame_idx = (self.tick / SPINNER_DIVISOR) as usize % frames.len();
            (frames[frame_idx], Style::default().fg(theme.accent_running))
        } else if run.status == "complete" {
            (
                crate::glyphs::check_mark(),
                Style::default().fg(theme.accent_success),
            )
        } else if run.is_terminal() {
            (
                crate::glyphs::ballot_x(),
                Style::default().fg(theme.accent_error),
            )
        } else {
            ("⏸", Style::default().fg(theme.warning))
        };
        let right_text = format!("{elapsed} ");

        buf.set_span(area.x, y, &Span::styled(icon, icon_style), 2);

        let right_text_w = right_text.width() as u16;
        let kill_w: u16 = if running { 3 } else { 0 };
        let overlay_w = kill_w + right_text_w + 1;
        clear_overlay_area(buf, area, y, overlay_w);

        let mut rx = area.x + area.width;
        if running {
            rx = rx.saturating_sub(3);
            let is_hovered = matches!(
                &self.hovered_kill,
                Some(TaskEntryId::Workflow(n)) if n == &run.name
            );
            let kill_style = if is_hovered {
                Style::default().fg(theme.accent_error)
            } else {
                Style::default().fg(theme.gray)
            };
            buf.set_span(
                rx,
                y,
                &Span::styled(crate::glyphs::ballot_x_button(), kill_style),
                3,
            );
            self.kill_button_rects.push((
                TaskEntryId::Workflow(run.name.clone()),
                Rect::new(rx, y, 3, 1),
            ));
        }

        rx = rx.saturating_sub(right_text_w);
        buf.set_span(
            rx,
            y,
            &Span::styled(right_text, Style::default().fg(theme.gray)),
            right_text_w,
        );
    }

    fn render_bg_task_overlay(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        y: u16,
        task_id: &str,
        task: &BgTaskState,
        theme: &Theme,
    ) {
        let (icon, icon_style, right_text, right_style) = if task.pending_kill {
            let frames = crate::glyphs::dot_spinner_frames();
            let frame_idx = (self.tick / SPINNER_DIVISOR) as usize % frames.len();
            (
                frames[frame_idx],
                Style::default().fg(theme.accent_error),
                "killing\u{2026} ".to_string(),
                Style::default().fg(theme.accent_error),
            )
        } else {
            match task.status {
                BgTaskStatus::Running => {
                    let frames = crate::glyphs::dot_spinner_frames();
                    let frame_idx = (self.tick / SPINNER_DIVISOR) as usize % frames.len();
                    let elapsed = format_duration(task.elapsed());
                    (
                        frames[frame_idx],
                        Style::default().fg(theme.accent_running),
                        format!("{elapsed} "),
                        Style::default().fg(theme.gray),
                    )
                }
                BgTaskStatus::Done => {
                    let elapsed = format_duration(task.elapsed());
                    (
                        crate::glyphs::check_mark(),
                        Style::default().fg(theme.accent_success),
                        format!("{elapsed} "),
                        Style::default().fg(theme.gray),
                    )
                }
                BgTaskStatus::Failed => {
                    let elapsed = format_duration(task.elapsed());
                    (
                        crate::glyphs::ballot_x(),
                        Style::default().fg(theme.accent_error),
                        format!("{elapsed} "),
                        Style::default().fg(theme.gray),
                    )
                }
            }
        };

        buf.set_span(area.x, y, &Span::styled(icon, icon_style), 2);

        // Stdout line count, shown just to the left of the duration as
        // a compact `(N)` badge with SI scaling (`(1.2k)`, `(5.4M)`) so
        // huge outputs don't push the label off-screen. Hidden when
        // there's no captured output. Styled dim like the time text.
        // Reads the cached `stdout_line_count` (maintained by
        // `BgTaskState::set_stdout` / `append_stdout`) so the overlay
        // doesn't memchr-scan the full buffer per render frame.
        let badge = format_line_count_badge(task.stdout_line_count, task.truncated);
        let lines_text = if badge.is_empty() {
            String::new()
        } else {
            format!("{badge} ")
        };
        let lines_w = lines_text.width() as u16;

        // Clear overlay area to prevent label text bleeding through.
        let right_text_w = right_text.width() as u16;
        let bg_kill_w: u16 = if task.status == BgTaskStatus::Running {
            3
        } else {
            0
        };
        let bg_overlay_w = bg_kill_w + 3 + right_text_w + lines_w + 1;
        clear_overlay_area(buf, area, y, bg_overlay_w);

        let mut rx = area.x + area.width;

        // Kill button (visible even during pending_kill so the user can retry)
        if task.status == BgTaskStatus::Running {
            rx = rx.saturating_sub(3);
            let is_hovered = matches!(
                &self.hovered_kill,
                Some(TaskEntryId::BgTask(tid)) if tid == task_id
            );
            let kill_style = if is_hovered {
                Style::default().fg(theme.accent_error)
            } else {
                Style::default().fg(theme.gray)
            };
            buf.set_span(
                rx,
                y,
                &Span::styled(crate::glyphs::ballot_x_button(), kill_style),
                3,
            );
            self.kill_button_rects.push((
                TaskEntryId::BgTask(task_id.to_string()),
                Rect::new(rx, y, 3, 1),
            ));
        }

        // View button
        rx = rx.saturating_sub(3);
        let is_view_hovered = matches!(
            &self.hovered_view,
            Some(TaskEntryId::BgTask(tid)) if tid == task_id
        );
        let view_style = if is_view_hovered {
            Style::default().fg(theme.text_primary)
        } else {
            Style::default().fg(theme.gray)
        };
        buf.set_span(
            rx,
            y,
            &Span::styled(crate::glyphs::enlarge_button(), view_style),
            3,
        );
        self.view_button_rects.push((
            TaskEntryId::BgTask(task_id.to_string()),
            Rect::new(rx, y, 3, 1),
        ));

        // Time/status text
        let right_width = right_text.width() as u16;
        rx = rx.saturating_sub(right_width);
        buf.set_span(rx, y, &Span::styled(right_text, right_style), right_width);

        // Line count (just to the left of the duration).
        if lines_w > 0 {
            rx = rx.saturating_sub(lines_w);
            buf.set_span(
                rx,
                y,
                &Span::styled(lines_text, Style::default().fg(theme.gray)),
                lines_w,
            );
        }

        if rx > area.x {
            buf.set_span(rx - 1, y, &Span::raw(" "), 1);
        }
    }

    fn render_agent_overlay(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        y: u16,
        subagent_id: &str,
        info: &SubagentInfo,
        theme: &Theme,
    ) {
        let (icon, icon_style, right_text, right_style) = if info.pending_kill {
            let frames = crate::glyphs::dot_spinner_frames();
            let frame_idx = (self.tick / SPINNER_DIVISOR) as usize % frames.len();
            (
                frames[frame_idx],
                Style::default().fg(theme.accent_error),
                "killing\u{2026} ".to_string(),
                Style::default().fg(theme.accent_error),
            )
        } else if info.is_running() {
            let frames = crate::glyphs::dot_spinner_frames();
            let frame_idx = (self.tick / SPINNER_DIVISOR) as usize % frames.len();
            let elapsed = format_duration(info.display_elapsed());
            (
                frames[frame_idx],
                Style::default().fg(theme.accent_running),
                format!("{elapsed} "),
                Style::default().fg(theme.gray),
            )
        } else if info.status.as_deref() == Some("completed") {
            let elapsed = format_duration(info.display_elapsed());
            (
                crate::glyphs::check_mark(),
                Style::default().fg(theme.accent_success),
                format!("{elapsed} "),
                Style::default().fg(theme.gray),
            )
        } else {
            let elapsed = format_duration(info.display_elapsed());
            (
                crate::glyphs::ballot_x(),
                Style::default().fg(theme.accent_error),
                format!("{elapsed} "),
                Style::default().fg(theme.gray),
            )
        };

        buf.set_span(area.x, y, &Span::styled(icon, icon_style), 2);

        // Clear overlay area to prevent label text bleeding through.
        let badge = format_context_badge(info);
        let model_text = info
            .model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("");
        let right_text_w = right_text.width() as u16;
        let kill_w: u16 = if info.is_running() { 3 } else { 0 };
        let badge_w: u16 = if badge.is_empty() {
            0
        } else {
            badge.width() as u16 + 1
        };
        let model_w: u16 = if model_text.is_empty() {
            0
        } else {
            model_text.width() as u16 + 1
        };
        let overlay_w = kill_w + 3 + right_text_w + model_w + badge_w + 1;
        clear_overlay_area(buf, area, y, overlay_w);

        let mut rx = area.x + area.width;

        // Kill button (visible even during pending_kill so the user can retry)
        if info.is_running() {
            rx = rx.saturating_sub(3);
            let is_hovered = matches!(
                &self.hovered_kill,
                Some(TaskEntryId::Agent(sid)) if sid == subagent_id
            );
            let kill_style = if is_hovered {
                Style::default().fg(theme.accent_error)
            } else {
                Style::default().fg(theme.gray)
            };
            buf.set_span(
                rx,
                y,
                &Span::styled(crate::glyphs::ballot_x_button(), kill_style),
                3,
            );
            self.kill_button_rects.push((
                TaskEntryId::Agent(subagent_id.to_string()),
                Rect::new(rx, y, 3, 1),
            ));
        }

        // View button
        rx = rx.saturating_sub(3);
        let is_view_hovered = matches!(
            &self.hovered_view,
            Some(TaskEntryId::Agent(sid)) if sid == subagent_id
        );
        let view_style = if is_view_hovered {
            Style::default().fg(theme.text_primary)
        } else {
            Style::default().fg(theme.gray)
        };
        buf.set_span(
            rx,
            y,
            &Span::styled(crate::glyphs::enlarge_button(), view_style),
            3,
        );
        self.view_button_rects.push((
            TaskEntryId::Agent(subagent_id.to_string()),
            Rect::new(rx, y, 3, 1),
        ));

        // Time/status text
        let right_width = right_text.width() as u16;
        rx = rx.saturating_sub(right_width);
        buf.set_span(rx, y, &Span::styled(right_text, right_style), right_width);

        // Model (right-aligned, just to the left of elapsed). Pre-computed
        // above for overlay clearing.
        if !model_text.is_empty() {
            rx = rx.saturating_sub(model_w);
            let mstyle = Style::default().fg(theme.gray);
            buf.set_span(
                rx,
                y,
                &Span::styled(format!("{model_text} "), mstyle),
                model_w,
            );
        }

        // Context badge (pre-computed above for overlay clearing)
        if !badge.is_empty() {
            rx = rx.saturating_sub(badge_w);
            let bstyle = Style::default().fg(theme.gray).add_modifier(Modifier::DIM);
            buf.set_span(rx, y, &Span::styled(format!("{badge} "), bstyle), badge_w);
        }

        if rx > area.x {
            buf.set_span(rx - 1, y, &Span::raw(" "), 1);
        }
    }

    fn render_scheduled_overlay(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        y: u16,
        task_id: &str,
        linked_subagent: Option<&str>,
        theme: &Theme,
    ) {
        let frames = crate::glyphs::dot_spinner_frames();
        let frame_idx = (self.tick / SPINNER_DIVISOR) as usize % frames.len();
        buf.set_span(
            area.x,
            y,
            &Span::styled(frames[frame_idx], Style::default().fg(theme.accent_running)),
            2,
        );

        let overlay_cols = if linked_subagent.is_some() { 7 } else { 4 };
        clear_overlay_area(buf, area, y, overlay_cols);

        let mut rx = area.x + area.width;

        // Kill button [✗]
        rx = rx.saturating_sub(3);
        let is_hovered = matches!(
            &self.hovered_kill,
            Some(TaskEntryId::Scheduled(tid)) if tid == task_id
        );
        let kill_style = if is_hovered {
            Style::default().fg(theme.accent_error)
        } else {
            Style::default().fg(theme.gray)
        };
        buf.set_span(
            rx,
            y,
            &Span::styled(crate::glyphs::ballot_x_button(), kill_style),
            3,
        );
        self.kill_button_rects.push((
            TaskEntryId::Scheduled(task_id.to_string()),
            Rect::new(rx, y, 3, 1),
        ));

        if linked_subagent.is_some() {
            rx = rx.saturating_sub(3);
            let is_view_hovered = matches!(
                &self.hovered_view,
                Some(TaskEntryId::Scheduled(tid)) if tid == task_id
            );
            let view_style = if is_view_hovered {
                Style::default().fg(theme.text_primary)
            } else {
                Style::default().fg(theme.gray)
            };
            buf.set_span(
                rx,
                y,
                &Span::styled(crate::glyphs::enlarge_button(), view_style),
                3,
            );
            self.view_button_rects.push((
                TaskEntryId::Scheduled(task_id.to_string()),
                Rect::new(rx, y, 3, 1),
            ));
        }

        if rx > area.x {
            buf.set_span(rx - 1, y, &Span::raw(" "), 1);
        }
    }
}

#[cfg(test)]
#[path = "tasks_pane_status_tests.rs"]
mod status_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::Arc;
    use std::time::Instant;

    fn make_info() -> SubagentInfo {
        SubagentInfo {
            subagent_id: Arc::from("sa-1"),
            child_session_id: Arc::from("cs-1"),
            description: Arc::from("Find API endpoints"),
            subagent_type: Arc::from("explore"),
            persona: None,
            role: None,
            model: None,
            context_source: None,
            resumed_from: None,
            capability_mode: None,
            workflow_run_id: None,
            context_normalized: false,
            parent_prompt_id: None,
            started_at: Instant::now(),
            last_progress_at: Instant::now(),
            finished: false,
            status: None,
            error: None,
            duration_ms: None,
            tool_calls: None,
            turns: None,
            turn_count: None,
            tool_call_count: None,
            tokens_used: None,
            context_window_tokens: None,
            context_usage_pct: None,
            tools_used: Vec::new(),
            error_count: None,
            activity_label: None,
            is_background: false,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            prompt: None,
            child_cwd: None,
            worktree_path: None,
            transcript: Default::default(),
        }
    }

    fn make_bg_task(task_id: &str, command: &str, status: BgTaskStatus) -> BgTaskState {
        BgTaskState {
            task_id: task_id.into(),
            tool_call_id: String::new(),
            command: command.into(),
            description: None,
            cwd: String::new(),
            output_file: String::new(),
            status,
            start_time: SystemTime::now(),
            end_time: None,
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stdout_line_count: 0,
            truncated: false,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            is_monitor: false,
            restored_from_replay: false,
        }
    }

    #[test]
    fn line_badge_empty_for_zero() {
        assert_eq!(format_line_count_badge(0, false), "");
        // `truncated` doesn't conjure a badge out of an empty buffer.
        assert_eq!(format_line_count_badge(0, true), "");
    }

    #[test]
    fn line_badge_raw_under_thousand() {
        assert_eq!(format_line_count_badge(1, false), "(1)");
        assert_eq!(format_line_count_badge(42, false), "(42)");
        assert_eq!(format_line_count_badge(999, false), "(999)");
    }

    #[test]
    fn line_badge_decimal_thousands() {
        assert_eq!(format_line_count_badge(1_000, false), "(1.0k)");
        assert_eq!(format_line_count_badge(1_234, false), "(1.2k)");
        // Truncation, not rounding: 1999 stays at "1.9k".
        assert_eq!(format_line_count_badge(1_999, false), "(1.9k)");
        assert_eq!(format_line_count_badge(9_999, false), "(9.9k)");
    }

    #[test]
    fn line_badge_whole_thousands() {
        assert_eq!(format_line_count_badge(10_000, false), "(10k)");
        assert_eq!(format_line_count_badge(99_999, false), "(99k)");
        assert_eq!(format_line_count_badge(123_456, false), "(123k)");
        assert_eq!(format_line_count_badge(999_999, false), "(999k)");
    }

    #[test]
    fn line_badge_decimal_millions() {
        assert_eq!(format_line_count_badge(1_000_000, false), "(1.0M)");
        assert_eq!(format_line_count_badge(1_234_567, false), "(1.2M)");
        assert_eq!(format_line_count_badge(9_999_999, false), "(9.9M)");
    }

    #[test]
    fn line_badge_whole_millions() {
        assert_eq!(format_line_count_badge(10_000_000, false), "(10M)");
        assert_eq!(format_line_count_badge(123_456_789, false), "(123M)");
        assert_eq!(format_line_count_badge(999_999_999, false), "(999M)");
    }

    #[test]
    fn line_badge_truncated_appends_plus_suffix() {
        assert_eq!(format_line_count_badge(42, true), "(42+)");
        assert_eq!(format_line_count_badge(1_234, true), "(1.2k+)");
        assert_eq!(format_line_count_badge(123_456, true), "(123k+)");
        assert_eq!(format_line_count_badge(1_234_567, true), "(1.2M+)");
        assert_eq!(format_line_count_badge(123_456_789, true), "(123M+)");
    }

    #[test]
    fn bg_task_label_single_line() {
        let task = make_bg_task("t1", "cargo test --release", BgTaskStatus::Running);
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let label = match &entry {
            TaskEntry::BgTask { label, .. } => label.as_str(),
            _ => panic!("expected BgTask variant"),
        };
        assert_eq!(label, "cargo test --release");
    }

    #[test]
    fn bg_task_label_multiline_truncated() {
        let task = make_bg_task("t2", "echo hello\necho world", BgTaskStatus::Done);
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let label = match &entry {
            TaskEntry::BgTask { label, .. } => label.as_str(),
            _ => panic!("expected BgTask variant"),
        };
        assert!(
            label.ends_with('\u{2026}'),
            "multiline command should be truncated with ellipsis: {label}",
        );
        assert!(
            !label.contains('\n'),
            "label should be single line: {label}",
        );
    }

    #[test]
    fn bg_task_label_prefers_description_over_command() {
        let mut task = make_bg_task("t3", "cargo test --release", BgTaskStatus::Running);
        task.description = Some("Run release tests".into());
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let label = match &entry {
            TaskEntry::BgTask { label, .. } => label.as_str(),
            _ => panic!("expected BgTask variant"),
        };
        // `Task ` prefix is included in the searchable label.
        assert_eq!(label, "Task Run release tests");
    }

    #[test]
    fn bg_task_styled_prefix_uses_secondary_color() {
        let mut task = make_bg_task("t3a", "cargo test --release", BgTaskStatus::Running);
        task.description = Some("Run release tests".into());
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let styled = match &entry {
            TaskEntry::BgTask { styled, .. } => styled,
            _ => panic!("expected BgTask variant"),
        };
        let theme = Theme::current();
        assert_eq!(styled.spans.len(), 2);
        assert_eq!(styled.spans[0].content.as_ref(), "Task ");
        assert_eq!(styled.spans[0].style.fg, Some(theme.text_secondary));
        assert_eq!(styled.spans[1].content.as_ref(), "Run release tests");
        assert_eq!(styled.spans[1].style.fg, Some(theme.text_primary));
    }

    #[test]
    fn monitor_task_styled_with_monitor_tag() {
        // Monitors render a blue "Monitor" tag + neutral description,
        // mirroring scheduled /loop rows — NOT the bash-highlighted command.
        let mut task = make_bg_task("mon1", "python -u counter.py", BgTaskStatus::Running);
        task.is_monitor = true;
        task.description = Some("incrementing event counter every 3s".into());
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let (label, styled) = match &entry {
            TaskEntry::BgTask { label, styled, .. } => (label.as_str(), styled),
            _ => panic!("expected BgTask variant"),
        };
        let theme = Theme::current();
        assert_eq!(label, "Monitor incrementing event counter every 3s");
        assert_eq!(styled.spans.len(), 2);
        assert_eq!(styled.spans[0].content.as_ref(), "Monitor ");
        assert_eq!(styled.spans[0].style.fg, Some(theme.accent_system));
        assert_eq!(
            styled.spans[1].content.as_ref(),
            "incrementing event counter every 3s"
        );
        assert_eq!(styled.spans[1].style.fg, Some(theme.text_secondary));
    }

    #[test]
    fn bg_task_no_prefix_when_no_description() {
        // Bare-command branch (no description) stays prefix-free — the
        // bash-highlighted command stands alone.
        let task = make_bg_task("t3b", "ls -la", BgTaskStatus::Running);
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let (label, styled) = match &entry {
            TaskEntry::BgTask { label, styled, .. } => (label.as_str(), styled),
            _ => panic!("expected BgTask variant"),
        };
        assert_eq!(label, "ls -la");
        let joined: String = styled.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !joined.starts_with("Task "),
            "no description ⇒ no prefix, got: {joined:?}"
        );
    }

    #[test]
    fn bg_task_label_falls_back_to_command_for_blank_description() {
        let mut task = make_bg_task("t4", "ls -la", BgTaskStatus::Running);
        task.description = Some("   ".into());
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let label = match &entry {
            TaskEntry::BgTask { label, .. } => label.as_str(),
            _ => panic!("expected BgTask variant"),
        };
        assert_eq!(label, "ls -la");
    }

    #[test]
    fn bg_task_label_collapses_description_newlines() {
        let mut task = make_bg_task("t5", "ls", BgTaskStatus::Running);
        task.description = Some("First line\nSecond line".into());
        let mut cache = HashMap::new();
        let entry = TaskEntry::from_bg_task(&task, &mut cache);
        let label = match &entry {
            TaskEntry::BgTask { label, .. } => label.as_str(),
            _ => panic!("expected BgTask variant"),
        };
        assert_eq!(label, "Task First line Second line");
        assert!(!label.contains('\n'));
    }

    #[test]
    fn bg_task_stable_id_deterministic() {
        let task = make_bg_task("t1", "ls", BgTaskStatus::Running);
        let mut cache = HashMap::new();
        let e1 = TaskEntry::from_bg_task(&task, &mut cache);
        let e2 = TaskEntry::from_bg_task(&task, &mut cache);
        assert_eq!(e1.stable_id(), e2.stable_id());
    }

    #[test]
    fn bg_task_and_agent_ids_differ() {
        let task = make_bg_task("shared-id", "ls", BgTaskStatus::Running);
        let mut cache = HashMap::new();
        let bg = TaskEntry::from_bg_task(&task, &mut cache);

        let mut info = make_info();
        info.child_session_id = "shared-id".into();
        let agent = TaskEntry::from_subagent(&info);

        assert_ne!(
            bg.stable_id(),
            agent.stable_id(),
            "bg task and agent with same string id should have different stable_ids",
        );
    }

    /// Render `pane` to a fresh buffer of the given size and return the
    /// concatenated character content for every row. Lets tests do a
    /// `joined.contains("(N)")`-style assertion without depending on cell
    /// styling details.
    fn render_pane_to_strings(
        pane: &mut TasksPane,
        bg_tasks: &std::collections::BTreeMap<String, BgTaskState>,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let layout = crate::appearance::LayoutConfig::default();
        pane.render(
            area,
            &mut buf,
            false,
            &layout,
            bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
        );
        (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn render_shows_line_count_badge_for_bg_task_with_stdout() {
        let mut pane = TasksPane::new();
        pane.overlay.show();

        let mut task = make_bg_task("t1", "ls", BgTaskStatus::Running);
        // 42 newline-terminated rows ⇒ `(42)`. Use `set_stdout` so the
        // cached `stdout_line_count` is populated.
        task.set_stdout((0..42).map(|i| format!("line {i}\n")).collect::<String>());
        assert_eq!(task.stdout.lines().count(), 42);
        assert_eq!(task.stdout_line_count, 42);

        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert("t1".into(), task);

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // 12+ rows so `desired_height` is non-zero; wide enough that the
        // overlay isn't clipped.
        let lines = render_pane_to_strings(&mut pane, &bg_tasks, 80, 16);
        let joined = lines.join("\n");
        assert!(
            joined.contains("(42)"),
            "expected `(42)` badge in rendered buffer, got:\n{joined}",
        );
    }

    /// Resume regression: bg tasks restored from a `session/load`
    /// replay are historical context — they must not auto-open the overlay
    /// (on cold resumes they die again within the same load; the open/close
    /// flash looked like tasks "loading then failing"). A genuinely new live
    /// task must still trigger the auto-show edge.
    #[test]
    fn restored_running_tasks_do_not_auto_show() {
        let mut pane = TasksPane::new();

        let mut restored = make_bg_task("t-restored", "tail -f deploy.log", BgTaskStatus::Running);
        restored.restored_from_replay = true;
        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert("t-restored".to_string(), restored);

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );
        assert!(
            !pane.is_visible(),
            "replay-restored running tasks must not auto-open the tasks pane"
        );

        // A live (non-restored) task still triggers the 0→N auto-show edge.
        bg_tasks.insert(
            "t-live".to_string(),
            make_bg_task("t-live", "cargo build", BgTaskStatus::Running),
        );
        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );
        assert!(
            pane.is_visible(),
            "a new live running task must still auto-open the tasks pane"
        );
    }

    #[test]
    fn render_shows_plus_suffix_when_truncated() {
        let mut pane = TasksPane::new();
        pane.overlay.show();

        let mut task = make_bg_task("t1", "ls", BgTaskStatus::Running);
        task.set_stdout((0..42).map(|i| format!("line {i}\n")).collect::<String>());
        task.truncated = true;

        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert("t1".into(), task);

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        let lines = render_pane_to_strings(&mut pane, &bg_tasks, 80, 16);
        let joined = lines.join("\n");
        assert!(
            joined.contains("(42+)"),
            "expected `(42+)` badge in rendered buffer, got:\n{joined}",
        );
    }

    #[test]
    fn render_hides_badge_when_stdout_empty() {
        let mut pane = TasksPane::new();
        pane.overlay.show();

        let task = make_bg_task("t1", "ls", BgTaskStatus::Running);
        assert!(task.stdout.is_empty());

        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert("t1".into(), task);

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        let lines = render_pane_to_strings(&mut pane, &bg_tasks, 80, 16);
        let joined = lines.join("\n");
        assert!(
            !joined.contains("()"),
            "should not render empty `()` badge, got:\n{joined}",
        );
    }

    #[test]
    fn search_bar_not_overwritten_by_task_overlay() {
        // Regression: while subagents/tasks are running, opening the search
        // bar (`/`) used to render broken UI — the overlay pass (spinner +
        // kill/view buttons) painted over the bottom input-bar row that
        // `ListPane` reserves, corrupting `search:` into `⸬earch:` (the `⸬`
        // spinner glyph clobbering the leading `s`). The overlay must stop one
        // row short of the input bar.
        let mut pane = TasksPane::new();
        pane.overlay.show();

        // Three running tasks ⇒ entries = [Tasks header, t0, t1, t2].
        let mut bg_tasks = std::collections::BTreeMap::new();
        for i in 0..3 {
            bg_tasks.insert(
                format!("t{i}"),
                make_bg_task(
                    &format!("t{i}"),
                    &format!("sleep {i}"),
                    BgTaskStatus::Running,
                ),
            );
        }
        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // Press `/` to open the search bar.
        assert!(pane.handle_key(&crate::key!('/').to_key_event()));
        assert!(
            pane.list_state.input_mode().is_some(),
            "`/` should open the search input bar",
        );

        // Height 4: the header + 3 task rows exactly fill the area, so the
        // search bar steals the bottom row. Without the fix, the third task's
        // overlay lands on that same row and clobbers the `search:` prompt.
        let lines = render_pane_to_strings(&mut pane, &bg_tasks, 60, 4);
        let joined = lines.join("\n");

        assert!(
            joined.contains("search:"),
            "search bar prompt must render intact (not clobbered by the task \
             overlay spinner), got:\n{joined}",
        );

        // The row carrying the prompt must start with `search:` (after the
        // pane's left padding) — not an overlay spinner/kill glyph.
        let bar_row = lines
            .iter()
            .find(|l| l.contains("search:"))
            .expect("a rendered row containing the search bar");
        assert!(
            bar_row.trim_start().starts_with("search:"),
            "search bar row should begin with the prompt, not an overlay \
             glyph: {bar_row:?}",
        );
    }

    #[test]
    fn search_bar_adds_a_line_keeping_last_entry_visible() {
        // Opening the search bar should grow the pane by exactly one row so the
        // bar gets its own line — the last task/agent must stay visible rather
        // than being displaced by the bar.
        let mut pane = TasksPane::new();
        pane.overlay.show();

        let mut bg_tasks = std::collections::BTreeMap::new();
        for i in 0..3 {
            bg_tasks.insert(
                format!("t{i}"),
                make_bg_task(
                    &format!("t{i}"),
                    &format!("sleep {i}"),
                    BgTaskStatus::Running,
                ),
            );
        }
        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // Tall enough that all entries fit without scrolling.
        let view_height = 40u16;
        let h_before = pane.desired_height(view_height);

        // Press `/` to open the search bar.
        assert!(pane.handle_key(&crate::key!('/').to_key_event()));
        let h_after = pane.desired_height(view_height);
        assert_eq!(
            h_after,
            h_before + 1,
            "opening search should add exactly one row for the bar",
        );

        // Render at the grown height: all three tasks AND the search bar must
        // be visible together.
        let lines = render_pane_to_strings(&mut pane, &bg_tasks, 60, h_after);
        let joined = lines.join("\n");
        assert!(joined.contains("sleep 0"), "first task visible:\n{joined}");
        assert!(joined.contains("sleep 1"), "second task visible:\n{joined}");
        assert!(
            joined.contains("sleep 2"),
            "last task must remain visible after opening search:\n{joined}",
        );
        assert!(joined.contains("search:"), "search bar visible:\n{joined}");
    }

    #[test]
    fn render_loop_row_truncates_before_kill_button() {
        // A non-scrollable loop row with a long prompt must truncate before
        // the `[✗]` kill button — nothing may render to its right. Regression:
        // the scrollbar-padding column used to be filled with label text when
        // the list wasn't scrollable, bleeding one cell past `[✗]`.
        let mut pane = TasksPane::new();
        pane.overlay.show();

        let mut scheduled = HashMap::new();
        scheduled.insert(
            "l1".into(),
            make_scheduled_info(
                "l1",
                "every 1m",
                "do a very long thing that definitely overflows the row width several times",
                None,
            ),
        );

        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );

        // One header + one loop row in a tall pane ⇒ not scrollable.
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let layout = crate::appearance::LayoutConfig::default();
        pane.render(
            area,
            &mut buf,
            false,
            &layout,
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
        );

        // Locate the `✗` kill glyph; every cell past the closing `]` must be
        // blank (no scrollbar when not scrollable, no leaked label text).
        let mut found = false;
        for y in 0..area.height {
            let row: Vec<String> = (0..area.width)
                .map(|x| {
                    buf.cell((x, y))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
                .collect();
            if let Some(x) = row.iter().position(|s| s == "\u{2717}") {
                found = true;
                for cell in &row[x + 2..] {
                    assert!(
                        cell.trim().is_empty(),
                        "non-blank cell after `[✗]` on loop row: {row:?}",
                    );
                }
            }
        }
        assert!(found, "expected a `✗` kill button on the loop row");
    }

    #[test]
    fn render_shows_centered_down_arrow_when_overflowing() {
        let mut pane = TasksPane::new();
        pane.overlay.show();

        let mut bg_tasks = std::collections::BTreeMap::new();
        for i in 0..20 {
            bg_tasks.insert(
                format!("t{i}"),
                make_bg_task(&format!("t{i}"), &format!("cmd {i}"), BgTaskStatus::Running),
            );
        }

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // A short panel forces the list to overflow; at the top of the list a
        // centered ▼ appears on the reserved bottom row.
        let lines = render_pane_to_strings(&mut pane, &bg_tasks, 40, 6);
        let joined = lines.join("\n");
        assert!(
            joined.contains('\u{25BC}'),
            "expected centered ▼ when the list overflows, got:\n{joined}"
        );
    }

    #[test]
    fn render_hides_down_arrow_at_bottom() {
        let mut pane = TasksPane::new();
        pane.overlay.show();

        let mut bg_tasks = std::collections::BTreeMap::new();
        for i in 0..20 {
            bg_tasks.insert(
                format!("t{i}"),
                make_bg_task(&format!("t{i}"), &format!("cmd {i}"), BgTaskStatus::Running),
            );
        }

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // Establish the viewport, then scroll to the very bottom.
        let _ = render_pane_to_strings(&mut pane, &bg_tasks, 40, 6);
        pane.list_state.set_scroll_offset(1000);
        let lines = render_pane_to_strings(&mut pane, &bg_tasks, 40, 6);
        let joined = lines.join("\n");

        // At the bottom: ▲ shows (content above), ▼ must NOT (nothing below).
        assert!(
            joined.contains('\u{25B2}'),
            "expected ▲ when scrolled down, got:\n{joined}"
        );
        assert!(
            !joined.contains('\u{25BC}'),
            "▼ must hide at the bottom of the list, got:\n{joined}"
        );
    }

    #[test]
    fn sync_sorts_running_before_done() {
        let mut pane = TasksPane::new();
        pane.show_done = true;

        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert(
            "done".into(),
            make_bg_task("done", "echo done", BgTaskStatus::Done),
        );
        bg_tasks.insert(
            "running".into(),
            make_bg_task("running", "sleep 99", BgTaskStatus::Running),
        );

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        assert!(pane.items.len() >= 2);
        assert!(pane.items[0].is_running(), "first entry should be running",);
        assert!(!pane.items[1].is_running(), "second entry should be done",);
    }

    #[test]
    fn sync_groups_agents_before_bg_tasks() {
        let mut pane = TasksPane::new();
        pane.show_done = true;

        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert("t1".into(), make_bg_task("t1", "ls", BgTaskStatus::Done));

        let mut subagents = HashMap::new();
        let mut info = make_info();
        info.finished = true;
        info.status = Some("completed".into());
        subagents.insert("cs-1".into(), info);

        pane.sync(
            &bg_tasks,
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        assert_eq!(pane.items.len(), 2);
        assert!(
            matches!(&pane.items[0], TaskEntry::Agent { .. }),
            "agents should sort before bg tasks",
        );
        assert!(
            matches!(&pane.items[1], TaskEntry::BgTask { .. }),
            "bg tasks should sort after agents",
        );
    }

    #[test]
    fn sync_groups_monitors_as_their_own_block() {
        // Monitors are BgTask entries but get their own contiguous group:
        // subagents → one-shot tasks → monitors → scheduled.
        let mut pane = TasksPane::new();
        pane.show_done = true;

        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert("t1".into(), make_bg_task("t1", "ls", BgTaskStatus::Running));
        let mut mon = make_bg_task("m1", "tail -f log", BgTaskStatus::Running);
        mon.is_monitor = true;
        bg_tasks.insert("m1".into(), mon);

        let mut subagents = HashMap::new();
        subagents.insert("cs-1".into(), make_info()); // running subagent

        pane.sync(
            &bg_tasks,
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        assert_eq!(pane.items.len(), 3);
        assert!(
            matches!(&pane.items[0], TaskEntry::Agent { .. }),
            "subagent first",
        );
        assert!(
            matches!(
                &pane.items[1],
                TaskEntry::BgTask {
                    is_monitor: false,
                    ..
                }
            ),
            "one-shot task second",
        );
        assert!(
            matches!(
                &pane.items[2],
                TaskEntry::BgTask {
                    is_monitor: true,
                    ..
                }
            ),
            "monitor in its own group, after one-shot tasks",
        );
    }

    #[test]
    fn monitors_and_loops_share_one_watchers_section() {
        // Monitor (BgTask) and loop (Scheduled) tasks render under a single
        // "Watchers" header, monitors sorted before loops.
        let mut pane = TasksPane::new();
        pane.show_done = true;

        let mut bg_tasks = std::collections::BTreeMap::new();
        let mut mon = make_bg_task("m1", "tail -f log", BgTaskStatus::Running);
        mon.is_monitor = true;
        bg_tasks.insert("m1".into(), mon);

        let mut scheduled = HashMap::new();
        scheduled.insert(
            "l1".into(),
            make_scheduled_info("l1", "every 1m", "do x", None),
        );

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );

        // items: monitor first, then loop.
        assert_eq!(pane.items.len(), 2);
        assert!(matches!(
            &pane.items[0],
            TaskEntry::BgTask {
                is_monitor: true,
                ..
            }
        ));
        assert!(matches!(&pane.items[1], TaskEntry::Scheduled { .. }));

        // entries: ONE Watchers header (count 2), then monitor, then loop.
        assert_eq!(pane.entries.len(), 3);
        let header_text: String = match &pane.entries[0] {
            TaskEntry::Header {
                group: GroupKind::Watchers,
                styled,
            } => styled.spans.iter().map(|s| s.content.as_ref()).collect(),
            _ => panic!("expected Watchers header first"),
        };
        assert!(header_text.contains("Watchers"), "got: {header_text}");
        assert!(header_text.contains('2'), "combined count: {header_text}");
        assert!(matches!(
            &pane.entries[1],
            TaskEntry::BgTask {
                is_monitor: true,
                ..
            }
        ));
        assert!(matches!(&pane.entries[2], TaskEntry::Scheduled { .. }));
    }

    #[test]
    fn sync_hides_done_by_default() {
        let mut pane = TasksPane::new();

        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert(
            "done".into(),
            make_bg_task("done", "echo done", BgTaskStatus::Done),
        );
        bg_tasks.insert(
            "running".into(),
            make_bg_task("running", "sleep 99", BgTaskStatus::Running),
        );

        pane.sync(
            &bg_tasks,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        assert_eq!(pane.items.len(), 1, "only running tasks shown by default");
        assert!(pane.items[0].is_running());
    }

    #[test]
    fn sync_inserts_group_headers_with_counts() {
        let mut pane = TasksPane::new();
        let mut bg_tasks = std::collections::BTreeMap::new();
        bg_tasks.insert("t1".into(), make_bg_task("t1", "ls", BgTaskStatus::Running));
        let mut subagents = HashMap::new();
        subagents.insert("cs-1".into(), make_info()); // running subagent

        pane.sync(
            &bg_tasks,
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // Display list interleaves a header before each group's items:
        // [Header(Subagents), Agent, Header(Tasks), BgTask].
        assert_eq!(pane.entries.len(), 4);
        assert!(matches!(
            &pane.entries[0],
            TaskEntry::Header {
                group: GroupKind::Subagents,
                ..
            }
        ));
        assert!(matches!(&pane.entries[1], TaskEntry::Agent { .. }));
        assert!(matches!(
            &pane.entries[2],
            TaskEntry::Header {
                group: GroupKind::Tasks,
                ..
            }
        ));
        assert!(matches!(&pane.entries[3], TaskEntry::BgTask { .. }));

        // The header text carries the label and the count.
        let header_text: String = match &pane.entries[0] {
            TaskEntry::Header { styled, .. } => {
                styled.spans.iter().map(|s| s.content.as_ref()).collect()
            }
            _ => unreachable!(),
        };
        assert!(header_text.contains("Subagents"), "got: {header_text}");
        assert!(header_text.contains('1'), "count in header: {header_text}");
    }

    #[test]
    fn toggle_group_hides_and_shows_items() {
        let mut pane = TasksPane::new();
        let mut subagents = HashMap::new();
        subagents.insert("cs-1".into(), make_info());
        pane.sync(
            &std::collections::BTreeMap::new(),
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // Expanded: header + item.
        assert_eq!(pane.entries.len(), 2);
        assert!(matches!(&pane.entries[1], TaskEntry::Agent { .. }));

        // Collapse → only the header remains.
        pane.toggle_group(GroupKind::Subagents);
        assert_eq!(pane.entries.len(), 1);
        assert!(matches!(
            &pane.entries[0],
            TaskEntry::Header {
                group: GroupKind::Subagents,
                ..
            }
        ));

        // Expand → item returns.
        pane.toggle_group(GroupKind::Subagents);
        assert_eq!(pane.entries.len(), 2);
        assert!(matches!(&pane.entries[1], TaskEntry::Agent { .. }));
    }

    #[test]
    fn arrow_keys_expand_and_collapse_group() {
        // ← collapses, → expands (vs Enter / click which toggle). The arrow
        // handler calls `set_group_collapsed`; exercise it directly.
        let mut pane = TasksPane::new();
        let mut subagents = HashMap::new();
        subagents.insert("cs-1".into(), make_info());
        pane.sync(
            &std::collections::BTreeMap::new(),
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );
        assert_eq!(pane.entries.len(), 2);

        // ← collapses the expanded group.
        assert!(pane.set_group_collapsed(GroupKind::Subagents, true));
        assert_eq!(pane.entries.len(), 1);
        // ← again: already collapsed, no change.
        assert!(!pane.set_group_collapsed(GroupKind::Subagents, true));
        assert_eq!(pane.entries.len(), 1);

        // → expands it again.
        assert!(pane.set_group_collapsed(GroupKind::Subagents, false));
        assert_eq!(pane.entries.len(), 2);
        assert!(matches!(&pane.entries[1], TaskEntry::Agent { .. }));
        // → again: already expanded, no change.
        assert!(!pane.set_group_collapsed(GroupKind::Subagents, false));
        assert_eq!(pane.entries.len(), 2);
    }

    #[test]
    fn emptied_group_forgets_collapse_state() {
        // Collapse a group, let it empty out, then repopulate it: the new
        // items must be visible (group auto-expands) rather than hidden under
        // a stale collapsed header.
        let mut pane = TasksPane::new();
        let mut subagents = HashMap::new();
        subagents.insert("cs-1".into(), make_info());
        pane.sync(
            &std::collections::BTreeMap::new(),
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        pane.toggle_group(GroupKind::Subagents);
        assert!(pane.collapsed_groups.contains(&GroupKind::Subagents));

        // Group empties (no subagents) → collapse state is forgotten.
        pane.sync(
            &std::collections::BTreeMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );
        assert!(!pane.collapsed_groups.contains(&GroupKind::Subagents));

        // A new subagent arrives → shown expanded (header + item), not hidden.
        let mut next = make_info();
        next.child_session_id = "cs-2".into();
        next.subagent_id = "sa-2".into();
        let mut subagents2 = HashMap::new();
        subagents2.insert("cs-2".into(), next);
        pane.sync(
            &std::collections::BTreeMap::new(),
            &subagents2,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );
        assert_eq!(pane.entries.len(), 2);
        assert!(matches!(&pane.entries[1], TaskEntry::Agent { .. }));
    }

    #[test]
    fn subagents_ordered_by_agent_type() {
        let mut pane = TasksPane::new();
        let mut subagents = HashMap::new();
        // Two running subagents of different types; both running so the
        // running-first key ties and the type order decides.
        let mut plan = make_info();
        plan.child_session_id = "cs-plan".into();
        plan.subagent_type = "plan".into();
        let mut explore = make_info();
        explore.child_session_id = "cs-explore".into();
        explore.subagent_type = "explore".into();
        subagents.insert("cs-plan".into(), plan);
        subagents.insert("cs-explore".into(), explore);

        pane.sync(
            &std::collections::BTreeMap::new(),
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &[],
        );

        // Ordered by agent type alphabetically: Explore before Plan.
        let types: Vec<&str> = pane
            .items
            .iter()
            .map(|e| match e {
                TaskEntry::Agent { type_label, .. } => type_label.as_str(),
                _ => panic!("expected Agent"),
            })
            .collect();
        assert_eq!(types, vec!["Explore", "Plan"]);
    }

    #[test]
    fn entry_label_includes_type_badge() {
        let info = make_info();
        let entry = TaskEntry::from_subagent(&info);
        let label = match &entry {
            TaskEntry::Agent { label, .. } => label.as_str(),
            _ => panic!("expected Agent variant"),
        };
        assert!(
            label.starts_with("Explore "),
            "label should start with capitalized type badge: {label}",
        );
    }

    #[test]
    fn entry_label_includes_meta() {
        let mut info = make_info();
        info.persona = Some("researcher".into());
        info.model = Some("echo-agent-3".into());
        let entry = TaskEntry::from_subagent(&info);
        let label = match &entry {
            TaskEntry::Agent { label, .. } => label.as_str(),
            _ => panic!("expected Agent variant"),
        };
        assert!(
            label.contains("Researcher"),
            "label should contain capitalized persona: {label}",
        );
        assert!(
            label.contains("echo-agent-3"),
            "label should contain model: {label}",
        );
    }

    #[test]
    fn entry_label_no_meta_when_empty() {
        let info = make_info();
        let entry = TaskEntry::from_subagent(&info);
        let label = match &entry {
            TaskEntry::Agent { label, .. } => label.as_str(),
            _ => panic!("expected Agent variant"),
        };
        assert_eq!(label, "Explore Find API endpoints");
    }

    #[test]
    fn subagent_activity_suffix_renders_while_running_only() {
        let mut info = make_info();
        info.activity_label = Some("Running: cargo build".into());
        let entry = TaskEntry::from_subagent(&info);
        let (label, styled) = match &entry {
            TaskEntry::Agent { label, styled, .. } => (label, styled),
            _ => panic!("expected Agent variant"),
        };
        let suffix = styled.spans.last().unwrap();
        assert_eq!(suffix.content.as_ref(), " \u{00b7} Running: cargo build");
        assert_eq!(suffix.style.fg, Some(Theme::current().gray));
        assert!(
            !label.contains("cargo build"),
            "activity must stay out of the searchable label: {label}"
        );

        // Finished rows drop the suffix even if a stale label lingers.
        info.finished = true;
        let entry = TaskEntry::from_subagent(&info);
        let styled = match &entry {
            TaskEntry::Agent { styled, .. } => styled,
            _ => panic!("expected Agent variant"),
        };
        assert!(
            styled
                .spans
                .iter()
                .all(|s| !s.content.contains("cargo build")),
            "no activity suffix on finished rows: {styled:?}"
        );
    }

    #[test]
    fn subagent_activity_suffix_caps_description() {
        let long_desc = "d".repeat(60);
        let mut info = make_info();
        info.description = Arc::from(long_desc.as_str());
        info.activity_label = Some("Thinking".into());
        let entry = TaskEntry::from_subagent(&info);
        let (label, styled) = match &entry {
            TaskEntry::Agent { label, styled, .. } => (label, styled),
            _ => panic!("expected Agent variant"),
        };
        let desc = styled.spans[1].content.as_ref();
        assert!(
            desc.ends_with('\u{2026}') && desc.chars().count() <= 40,
            "description must be capped when an activity suffix renders: {desc}"
        );
        assert!(
            label.contains(&long_desc),
            "the searchable label keeps the full description: {label}"
        );

        // Without an activity suffix the description renders uncapped.
        info.activity_label = None;
        let entry = TaskEntry::from_subagent(&info);
        let styled = match &entry {
            TaskEntry::Agent { styled, .. } => styled,
            _ => panic!("expected Agent variant"),
        };
        assert_eq!(styled.spans[1].content.as_ref(), long_desc);
    }

    fn make_scheduled_info(
        id: &str,
        schedule: &str,
        prompt: &str,
        next: Option<&str>,
    ) -> ScheduledTaskInfo {
        ScheduledTaskInfo {
            task_id: id.to_string(),
            prompt: prompt.to_string(),
            human_schedule: schedule.to_string(),
            created_at: std::time::Instant::now(),
            next_fire_at: next.map(|s| s.to_string()),
            tag: "loop".into(),
            last_subagent_id: None,
        }
    }

    #[test]
    fn scheduled_label_shows_next_in_countdown() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        // future next
        let next = (chrono::Utc::now() + chrono::Duration::seconds(125)).to_rfc3339();
        scheduled.insert(
            "t1".into(),
            make_scheduled_info("t1", "every 1m", "do x", Some(&next)),
        );
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );
        let label = match &pane.items[0] {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(
            label.contains("next in 2m"),
            "expected countdown in label: {label}"
        );
    }

    #[test]
    fn scheduled_label_shows_running_now_on_cron_match() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        scheduled.insert(
            "cron1".into(),
            make_scheduled_info("cron1", "every 5m", "loop", None),
        );
        pane.sync(
            &std::collections::BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            Some("cron1"),
            &HashSet::new(),
            &[],
        );
        let label = match &pane.items[0] {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(
            label.contains("(running)"),
            "expected (running) when cron matches: {label}"
        );
    }

    #[test]
    fn scheduled_provisional_shows_starting() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        scheduled.insert(
            "provisional-abc".into(),
            make_scheduled_info("provisional-abc", "every 10s", "soon", None),
        );
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );
        let label = match &pane.items[0] {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(
            label.contains("(starting)"),
            "expected (starting) for provisional: {label}"
        );
    }

    #[test]
    fn scheduled_queued_shows_queued() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        scheduled.insert(
            "q1".into(),
            make_scheduled_info("q1", "every 5m", "check", None),
        );
        let mut queued = HashSet::new();
        queued.insert("q1");
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &queued,
            &[],
        );
        let label = match &pane.items[0] {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(
            label.contains("(queued)"),
            "expected (queued) when task is in pending_prompts: {label}"
        );
    }

    #[test]
    fn scheduled_past_shows_due_now() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        let past = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
        scheduled.insert(
            "due".into(),
            make_scheduled_info("due", "every 1h", "past", Some(&past)),
        );
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );
        let label = match &pane.items[0] {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(
            label.contains("due now"),
            "expected due now for past: {label}"
        );
    }

    #[test]
    fn scheduled_unicode_prompt_safe_no_panic() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        let unicode_prompt = "测试emoji🚀".repeat(20); // multi-byte >60 bytes
        scheduled.insert(
            "uni".into(),
            make_scheduled_info("uni", "every 1s", &unicode_prompt, None),
        );
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );
        let entry = &pane.items[0];
        let label = match entry {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(label.contains("Loop"), "tag rendered in label");
        assert!(
            label.contains("...") || label.len() > 10,
            "preview truncated or full"
        );
    }

    #[test]
    fn scheduled_bad_next_fire_at_falls_back_to_approx() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        scheduled.insert(
            "bad".into(),
            make_scheduled_info("bad", "every 30s", "fallback", Some("not-a-date")),
        );
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );
        let label = match &pane.items[0] {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(
            label.contains("next in") || label.contains("due now"),
            "bad rfc should fallback to approx countdown: {label}"
        );
        assert!(
            !label.contains("~soon"),
            "should not ~soon on rfc parse fail fallback: {label}"
        );
    }

    #[test]
    fn scheduled_unknown_schedule_no_suffix() {
        let mut pane = TasksPane::new();
        let mut scheduled = HashMap::new();
        scheduled.insert(
            "unk".into(),
            make_scheduled_info("unk", "unknown schedule", "x", None),
        );
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &scheduled,
            None,
            &HashSet::new(),
            &[],
        );
        let label = match &pane.items[0] {
            TaskEntry::Scheduled { label, .. } => label,
            _ => panic!("expected Scheduled"),
        };
        assert!(
            !label.contains("(next") && !label.contains("(running") && !label.contains("(queued"),
            "unknown schedule should have no status suffix: {label}"
        );
    }

    fn make_workflow_run(name: &str, status: &str) -> crate::views::workflows::WorkflowRunSnapshot {
        crate::views::workflows::WorkflowRunSnapshot {
            run_id: format!("wf_{name}"),
            name: name.to_string(),
            objective: "obj".to_string(),
            status: status.to_string(),
            management_available: true,
            builtin: false,
            phases: Vec::new(),
            current_phase: Some("Scan".to_string()),
            agents: Vec::new(),
            agent_budget: None,
            agents_used: 0,
            agents_reserved: 0,
            agents_remaining: None,
            agent_usage_incomplete: false,
            active_agents: 0,
            elapsed_ms: 5_000,
            received_at: Instant::now(),
            pause_message: None,
            result_summary: None,
        }
    }

    #[test]
    fn workflows_section_lists_runs() {
        let mut pane = TasksPane::new();
        let runs = vec![
            make_workflow_run("pii-purge", "active"),
            make_workflow_run("old-scan", "complete"),
        ];
        pane.sync(
            &BTreeMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HashSet::new(),
            &runs,
        );

        let labels: Vec<&str> = pane.entries.iter().map(|e| e.search_text()).collect();
        assert!(
            labels.contains(&"Workflows"),
            "missing Workflows header: {labels:?}"
        );
        assert!(labels.iter().any(|l| l.contains("pii-purge")), "{labels:?}");
        assert!(
            !labels.iter().any(|l| l.contains("old-scan")),
            "settled run hidden while show_done is off: {labels:?}"
        );

        let row = labels
            .iter()
            .find(|l| l.contains("pii-purge"))
            .unwrap()
            .to_string();
        assert!(row.starts_with("Workflow "), "{row}");
        assert!(row.contains("Scan"), "live phase suffix missing: {row}");
    }

    #[test]
    fn workflow_children_are_excluded_and_run_counts_once() {
        let mut pane = TasksPane::new();
        let mut child = make_info();
        child.workflow_run_id = Some(Arc::from("wf_deep-research"));
        let mut subagents = HashMap::new();
        subagents.insert("cs-1".to_string(), child);
        let runs = vec![make_workflow_run("deep-research", "active")];
        pane.sync(
            &BTreeMap::new(),
            &subagents,
            &HashMap::new(),
            None,
            &HashSet::new(),
            &runs,
        );
        assert!(
            pane.items
                .iter()
                .all(|e| !matches!(e, TaskEntry::Agent { .. }))
        );
        assert_eq!(
            pane.status_counts(&BTreeMap::new(), &subagents, &HashMap::new(), &runs),
            TaskStatusCounts {
                running: 1,
                paused_workflows: 0,
            }
        );
    }

    #[test]
    fn workflow_suffix_counts_only_running_roster_rows() {
        let mut run = make_workflow_run("deep-research", "active");
        run.agents = vec![
            crate::views::workflows::WorkflowAgentRowView {
                agent_id: "a1".into(),
                label: "one".into(),
                phase: None,
                model: None,
                state: "running".into(),
                tokens_used: 0,
                duration_ms: 0,
            },
            crate::views::workflows::WorkflowAgentRowView {
                agent_id: "a2".into(),
                label: "two".into(),
                phase: None,
                model: None,
                state: "done".into(),
                tokens_used: 0,
                duration_ms: 0,
            },
        ];
        let entry = TaskEntry::from_workflow_run(&run);
        assert!(entry.search_text().contains("1 agent"));
        assert!(!entry.search_text().contains("2 agents"));
    }

    #[test]
    fn workflow_row_stoppable_tracks_can_stop_not_is_active() {
        fn stoppable_of(run: &crate::views::workflows::WorkflowRunSnapshot) -> bool {
            match TaskEntry::from_workflow_run(run) {
                TaskEntry::Workflow { stoppable, .. } => stoppable,
                _ => panic!("expected a workflow entry"),
            }
        }

        for status in ["active", "paused", "budget_limited"] {
            let run = make_workflow_run("wf", status);
            assert!(run.can_stop(), "{status} run should be stoppable");
            assert!(stoppable_of(&run), "{status} row must be marked stoppable");
        }

        for status in ["complete", "failed", "cancelled", "interrupted"] {
            let run = make_workflow_run("wf", status);
            assert!(!run.can_stop(), "{status} run should not be stoppable");
            assert!(!stoppable_of(&run), "{status} row must not be stoppable");
        }

        match TaskEntry::from_workflow_run(&make_workflow_run("wf", "paused")) {
            TaskEntry::Workflow {
                running, stoppable, ..
            } => {
                assert!(!running, "paused is not is_active()");
                assert!(stoppable, "but paused IS can_stop()");
            }
            _ => panic!("expected a workflow entry"),
        }
    }
}
