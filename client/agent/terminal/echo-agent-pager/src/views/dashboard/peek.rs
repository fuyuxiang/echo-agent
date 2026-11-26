//! Peek-panel state + helpers.
//!
//! Phase 1 stub for the panel, with enough infrastructure that Phase 2
//! can lift the question text + permission options directly off the
//! parent agent.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::state::DashboardRowId;
use crate::app::actions::Action;
use crate::app::agent_view::AgentView;
use crate::render::line_utils::truncate_str;
use crate::theme::Theme;

/// Args for painting a dense bottom-pinned live tail in the peek middle.
pub struct PeekLiveTailArgs<'a> {
    pub scrollback: &'a crate::scrollback::state::ScrollbackState,
}

/// Exclusive bottom y of the live-tail middle band (above the reply).
///
/// Reserves a 1-row breathing blank above the reply only when middle still
/// has ≥2 rows after that blank so pin + body can share. When only one row
/// would remain (`middle_h_with_blank == 1`), expand into the blank so the
/// current-turn body is not starved — matches measure when
/// `blank_row=false` (e.g. max_content = fixed+1 with pin).
fn live_tail_middle_bottom(middle_top: u16, reply_top_y: u16) -> u16 {
    let with_blank = reply_top_y.saturating_sub(1);
    let h_with_blank = with_blank.saturating_sub(middle_top);
    if h_with_blank > 1 {
        with_blank
    } else if reply_top_y > middle_top {
        reply_top_y
    } else {
        with_blank
    }
}

/// Maximum number of rows the `❯ reply` input grows to as the user
/// inserts newlines (Shift+Enter / Alt+Enter). Past this the reply
/// scrolls internally (the widget keeps the caret visible). Mirrors the
/// dispatch box's multiline cap so neither input can crowd out the row
/// list; the layout additionally clamps the whole box to keep ≥1 list
/// row.
pub const MAX_REPLY_ROWS: u16 = 6;

// The peek panel's `❯ reply` editor is a full `PromptWidget` (the same
// component backing the dashboard dispatch box and the agent prompt),
// so paste chips (`[Pasted: N lines]` + preview/expand), word navigation,
// undo, and text selection all behave like the other inputs.
//
// The widget lives on `DashboardState` (`peek_reply`), NOT on
// `PeekPanelState`: a `PromptWidget` owns a fuzzy-file-matcher daemon
// thread, and the panel struct is rebuilt whenever the selection
// cursor lands on a row — per-row construction would spawn a thread
// per cursor move. Keeping the widget on the dashboard also preserves
// `PeekPanelState`'s `Clone`/`Debug` derives.

/// Display content for the peek panel, recomputed live from the
/// currently-selected agent (see [`compute_peek_fields`]). Split out
/// from [`PeekPanelState`] so the panel can be refreshed every frame
/// — following the selection cursor and surfacing live status — while
/// preserving the user's in-progress reply draft (the dashboard's
/// `peek_reply` widget).
#[derive(Debug, Clone)]
pub struct PeekFields {
    pub label: String,
    pub time_ago: String,
    /// Type of the most recent agent response (e.g. `"Thinking"`,
    /// `"Response"`, `"Edit"`), shown as the header's left label.
    pub response_type: String,
    pub last_user_message: Option<String>,
    pub question: Option<String>,
    pub options: Vec<(String, String)>,
    pub request_id: Option<usize>,
    /// Index into `options` of the `RejectOnce` ("No") option, when the
    /// request has one. Selecting it lets the user type a free-text
    /// feedback message (mirrors the chat permission panel).
    pub reject_option: Option<usize>,
}

/// Per-row peek panel state.
///
/// Peek panel display state for a selected dashboard row.
///
/// - Status (`response_type` + `time_ago`) on the header row.
/// - Middle body: live-tail scrollback (see `PeekLiveTailArgs`) or a
///   pending permission / ask-question UI.
/// - `❯ reply` input backed by the dashboard-owned `peek_reply`
///   [`PromptWidget`](crate::views::prompt_widget::PromptWidget).
///
/// Display fields refresh every frame from the selected agent; the reply
/// draft is preserved across refreshes and only cleared when the peeked
/// row changes or the panel closes (`DashboardState::set_peek`).
#[derive(Debug, Clone)]
pub struct PeekPanelState {
    /// Which row is being peeked. Tracks the selection cursor; the
    /// row may disappear between frames (edge case 3).
    pub row: DashboardRowId,
    /// Human-readable label of the row for the panel title.
    pub label: String,
    /// Time-ago suffix for the header line (e.g. `"2m"`, `"1d"`,
    /// `"just now"`). Painted right-aligned in dim grey.
    pub time_ago: String,
    /// Type of the most recent agent response, shown as the header's
    /// left label: `"Thinking"` / `"Thought"`, `"Response"`, `"Edit"`,
    /// `"Read"`, `"Bash"`, … (or `"Working"` / `"Idle"` when the agent
    /// hasn't produced a response yet). See [`extract_last_response_type`].
    pub response_type: String,
    /// First line of the most recent user prompt, or `None`.
    pub last_user_message: Option<String>,
    /// Question text from a pending `PermissionView`, when applicable.
    pub question: Option<String>,
    /// Multiple-choice options for a pending permission request. Each
    /// option's `id` corresponds to a `acp::PermissionOptionId`.
    pub options: Vec<(String, String)>,
    /// `PermissionViewState::id` captured at refresh time. Used to
    /// detect a stale answer when the front of the
    /// permission queue rotates between snapshot and key-press.
    pub request_id: Option<usize>,
    /// Index into `options` of the `RejectOnce` ("No") option, when the
    /// pending request has one. When that option is highlighted the
    /// user can type a free-text feedback message (reusing the
    /// dashboard's `peek_reply` widget as the buffer), mirroring the
    /// chat permission panel.
    pub reject_option: Option<usize>,
    /// Whether the reply input is focused (bright border + caret vs row-nav keys).
    /// Non-vim defaults focused; vim defaults unfocused so `j`/`k` keep selecting.
    /// Same-row live updates keep focus; a row change in vim clears it.
    pub focused: bool,
    /// Selected option index when a permission `question` is pending, or
    /// `None` when no option is selected (the default). With `None` the panel
    /// is a navigation surface — `↑`/`↓` switch agents and `Enter` opens the
    /// row in detail. A number key `1`–`9` selects (toggles) the matching
    /// option, turning the panel into an answer surface where `↑`/`↓` move
    /// within the options and `Enter` answers. Reset to `None` when the
    /// pending request changes; dropped if it falls out of range.
    pub selected_option: Option<usize>,
    /// Peeked agent's current model display name, painted on the box's
    /// bottom border (mirrors the dispatch box's config badge). `None`
    /// when unknown. Set by the render-time refresh from the live agent,
    /// not carried in [`PeekFields`].
    pub model_name: Option<String>,
    /// Whether the peeked agent runs in always-approve (yolo) mode. Shown
    /// as an `always-approve` flag next to the model on the bottom border —
    /// the same signal the dashboard row badge carried.
    pub auto_approve: bool,
    /// Whether the peeked agent is in Auto (LLM classifier) mode. Shown as an
    /// `auto` flag (mutually exclusive with `always-approve` — yolo wins).
    pub auto: bool,
    /// Whether the peeked agent is in plan mode. Shown as a `plan` flag on
    /// the bottom border (so the Shift+Tab mode cycle's three states are
    /// all visible). Set live by the render-time refresh.
    pub plan_mode: bool,
}

impl PeekPanelState {
    /// Build a fresh peek panel for the given row from computed
    /// fields. (The reply draft lives on the dashboard's `peek_reply`
    /// widget and is cleared by `DashboardState::set_peek` on open.)
    pub fn new(row: DashboardRowId, fields: PeekFields) -> Self {
        Self {
            row,
            label: fields.label,
            time_ago: fields.time_ago,
            response_type: fields.response_type,
            last_user_message: fields.last_user_message,
            question: fields.question,
            options: fields.options,
            request_id: fields.request_id,
            reject_option: fields.reject_option,
            // Vim: unfocused so row nav isn't stolen by the reply; non-vim: focused to type.
            focused: !crate::appearance::cache::load_vim_mode(),
            selected_option: None,
            // Populated by the render-time refresh from the live agent
            // (see `peek_model_and_mode`); defaults are harmless until then.
            model_name: None,
            auto_approve: false,
            auto: false,
            plan_mode: false,
        }
    }

    /// Refresh the display fields from a fresh [`compute_peek_fields`]
    /// snapshot. Returns `true` when the peeked `row` CHANGED (the user
    /// moved the selection cursor while the panel was open) so the
    /// caller can clear the dashboard's `peek_reply` draft — a
    /// half-typed reply must not be sent to the wrong agent.
    pub fn apply_fields(&mut self, row: DashboardRowId, fields: PeekFields) -> bool {
        let row_changed = row != self.row;
        if row_changed {
            self.row = row;
            // Vim: drop reply focus on row change so continued j/k isn't typed into the reply.
            if crate::appearance::cache::load_vim_mode() {
                self.focused = false;
            }
        }
        self.label = fields.label;
        self.time_ago = fields.time_ago;
        self.response_type = fields.response_type;
        self.last_user_message = fields.last_user_message;
        self.question = fields.question;
        // Reset the selected option when the pending request changes
        // (a new/rotated permission); otherwise keep the user's selection.
        if self.request_id != fields.request_id {
            self.selected_option = None;
        }
        self.options = fields.options;
        self.request_id = fields.request_id;
        self.reject_option = fields.reject_option;
        // Drop a stale selection that's now out of range.
        if let Some(i) = self.selected_option
            && i >= self.options.len()
        {
            self.selected_option = None;
        }
        // Focus/draft preserved on same-row live refreshes (row changes handled above).
        row_changed
    }

    /// Whether the pending question is an agent `AskUserQuestion` (the Ask
    /// tool) rather than a permission request. Permissions carry a
    /// `request_id` (their stale-guard id); ask questions don't. Drives
    /// the freeform placeholder and which answer action is emitted.
    pub fn is_ask_question(&self) -> bool {
        self.question.is_some() && self.request_id.is_none()
    }
}

/// Compute the live display fields for a dashboard row.
///
/// Returns `None` when the row's owning agent (or subagent) no longer
/// exists, signalling the caller to close the peek. Extracted from the
/// dashboard dispatcher so both the initial open and the per-frame
/// refresh share one source of truth.
pub fn compute_peek_fields(
    row: &DashboardRowId,
    agents: &indexmap::IndexMap<crate::app::agent::AgentId, AgentView>,
) -> Option<PeekFields> {
    use crate::views::session_title::{entry_title, sanitize_display_text};
    match row {
        DashboardRowId::TopLevel(id) => {
            let agent = agents.get(id)?;
            let label = sanitize_display_text(&entry_title(agent)).into_owned();
            let response_type = extract_last_response_type(agent);
            let last_user_message = extract_last_user_message(agent);
            let time_ago = agent
                .last_active_at
                .map(|t| crate::util::format_time_ago(t.elapsed()))
                .unwrap_or_default();
            // A pending permission takes the question slot; otherwise a
            // single-question, single-select agent `AskUserQuestion`
            // (ext, not a local pager dialog) is surfaced the same way —
            // options + an "Other" free-text row. `request_id == Some`
            // distinguishes a permission (with its stale-guard id) from
            // an ask question (`None`).
            let (question, options, request_id, reject_option) =
                if let Some(p) = agent.permission_queue.front() {
                    let q = sanitize_display_text(&p.title).into_owned();
                    // Live scope-aware labels: the peek's answer path attaches
                    // the same selection meta, so each row's label must match
                    // its own count (allow and deny scopes are independent).
                    let selected_words = p.bash_highlights.as_ref().map(|h| {
                        crate::views::permission_view::allow_scope_label(
                            h,
                            p.bash_command_raw.as_deref(),
                            p.bash_selection_count,
                        )
                    });
                    let deny_selected_words = p
                        .bash_highlights
                        .as_ref()
                        .filter(|_| p.bash_deny_selection_count > 0)
                        .map(|h| h.highlighted_words[..p.bash_deny_selection_count].join(" "));
                    let opts = p
                        .options
                        .iter()
                        .map(|opt| {
                            let row_words = if opt.option_id.0.as_ref()
                                == crate::views::permission_view::REJECT_ALWAYS_COMMAND_OPTION_ID
                            {
                                deny_selected_words.as_deref()
                            } else {
                                selected_words.as_deref()
                            };
                            let name = crate::views::permission_view::option_label_for_selection(
                                opt,
                                row_words,
                                p.mcp_scope.as_ref(),
                            );
                            (
                                opt.option_id.0.to_string(),
                                sanitize_display_text(&name).into_owned(),
                            )
                        })
                        .collect::<Vec<_>>();
                    // The `RejectOnce` option accepts free-text feedback.
                    let reject = p.options.iter().position(|o| {
                        o.kind == agent_client_protocol::PermissionOptionKind::RejectOnce
                    });
                    (Some(q), opts, Some(p.id), reject)
                } else if let Some((qv, q)) = agent.question_view.as_ref().and_then(|qv| {
                    // Ext (agent) ask whose questions are all
                    // single-select. The peek walks through them one at
                    // a time, surfacing the active question.
                    let ok = qv.local_kind.is_none()
                        && !qv.questions.is_empty()
                        && qv
                            .questions
                            .iter()
                            .all(|q| !q.multi_select.unwrap_or(false));
                    ok.then(|| qv.questions.get(qv.active_tab).map(|q| (qv, q)))
                        .flatten()
                }) {
                    let mut opts: Vec<(String, String)> = q
                        .options
                        .iter()
                        .map(|o| {
                            let label = sanitize_display_text(&o.label).into_owned();
                            (label.clone(), label)
                        })
                        .collect();
                    // The freeform "Other" row (unless suppressed) is the
                    // free-text option, rendered like the permission reject row.
                    let reject = if qv.no_freeform {
                        None
                    } else {
                        opts.push(("__other__".to_string(), "Other".to_string()));
                        Some(opts.len() - 1)
                    };
                    // Prefix a `(i/N)` position marker for multi-question
                    // forms so the user knows how many remain.
                    let n = qv.questions.len();
                    let question = if n > 1 {
                        format!("({}/{}) {}", qv.active_tab + 1, n, q.question)
                    } else {
                        q.question.clone()
                    };
                    let question = sanitize_display_text(&question).into_owned();
                    (Some(question), opts, None, reject)
                } else {
                    (None, Vec::new(), None, None)
                };
            Some(PeekFields {
                label,
                time_ago,
                response_type,
                last_user_message,
                question,
                options,
                request_id,
                reject_option,
            })
        }
        DashboardRowId::Subagent {
            parent,
            child_session_id,
        } => {
            let parent_agent = agents.get(parent)?;
            let info = parent_agent.subagent_sessions.get(child_session_id)?;
            let label = {
                let (l, _) = crate::app::subagent::format_subagent_label(info);
                sanitize_display_text(&l).into_owned()
            };
            // `subagent_views` holds `Box<AgentView>`; the closures let
            // deref coercion turn `&Box<AgentView>` into `&AgentView`.
            let child = parent_agent.subagent_views.get(child_session_id);
            let response_type = child
                .map(|c| extract_last_response_type(c))
                .unwrap_or_else(|| "Subagent".to_string());
            let last_user_message = child.and_then(|c| extract_last_user_message(c));
            let time_ago = crate::util::format_time_ago(info.last_progress_at.elapsed());
            Some(PeekFields {
                label,
                time_ago,
                response_type,
                last_user_message,
                // Subagents are driven by their parent — no direct
                // permission prompts surface here.
                question: None,
                options: Vec::new(),
                request_id: None,
                reject_option: None,
            })
        }
        // Roster-only rows are not locally hosted — there is no local
        // `AgentView` to peek into.
        DashboardRowId::Roster { .. } => None,
    }
}

/// The peeked row's live config-badge state for the peek box's bottom border:
/// model display name, always-approve (yolo), auto (classifier) mode, and plan
/// mode. Named fields so the three adjacent bools can't be transposed at a
/// call/return site.
pub struct PeekModeBadge {
    pub model: Option<String>,
    pub yolo: bool,
    pub auto: bool,
    pub plan: bool,
}

/// The peeked row's current config-badge state. Sourced live (not via
/// [`PeekFields`]) so it always reflects a `/model` switch or a Shift+Tab mode
/// change. A subagent shows its own model when its view is loaded, else the
/// parent's; always-approve and auto follow the parent (subagents run under the
/// parent's permission mode) and subagents have no plan mode of their own.
/// All-default for a vanished agent or a roster-only row.
pub fn peek_model_and_mode(
    row: &DashboardRowId,
    agents: &indexmap::IndexMap<crate::app::agent::AgentId, AgentView>,
) -> PeekModeBadge {
    let default = || PeekModeBadge {
        model: None,
        yolo: false,
        auto: false,
        plan: false,
    };
    match row {
        DashboardRowId::TopLevel(id) => match agents.get(id) {
            Some(agent) => {
                // Prefer the optimistic pending plan state over the
                // confirmed one (matches `dispatch_cycle_mode`).
                let plan = agent.plan_mode_pending.unwrap_or(agent.plan_mode_active);
                PeekModeBadge {
                    model: agent.session.models.current_model_name(),
                    yolo: agent.session.yolo_mode,
                    auto: agent.session.is_auto(),
                    plan,
                }
            }
            None => default(),
        },
        DashboardRowId::Subagent {
            parent,
            child_session_id,
        } => match agents.get(parent) {
            Some(parent_agent) => {
                let model = parent_agent
                    .subagent_views
                    .get(child_session_id)
                    .and_then(|c| c.session.models.current_model_name())
                    .or_else(|| parent_agent.session.models.current_model_name());
                // Auto (like always-approve) follows the parent: subagents run
                // under the parent's permission mode. Plan stays false (subagents
                // have no plan mode of their own).
                PeekModeBadge {
                    model,
                    yolo: parent_agent.session.yolo_mode,
                    auto: parent_agent.session.is_auto(),
                    plan: false,
                }
            }
            None => default(),
        },
        DashboardRowId::Roster { .. } => default(),
    }
}

/// Result of [`render_peek_panel`].
#[derive(Debug, Default)]
pub struct PeekRenderResult {
    /// Screen position of the reply caret, when the reply / feedback
    /// input is focused. The caller parks the terminal cursor here.
    pub caret: Option<(u16, u16)>,
    /// Screen rect of the reply input row (the `❯ reply` line, or the
    /// reject-feedback slot in question mode). Recorded by the caller
    /// for mouse routing (click-to-focus, drag text selection).
    pub reply_rect: Option<Rect>,
}

/// Paint the peeked agent's model + always-approve flag onto the peek
/// box's **bottom border**, reusing the shared prompt info-line renderer
/// so its style and position match the dispatch box's config badge
/// exactly (`╰──model · always-approve──╯`). No-op when the box is too
/// small or there's nothing to show. The badge follows the reply input's
/// focus dimming (`panel.focused`).
fn paint_peek_config_badge(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    panel: &PeekPanelState,
    reply: &crate::views::prompt_widget::PromptWidget,
    multiline: bool,
) {
    use crate::views::prompt_widget::{PromptFlag, PromptInfo};

    if area.height < 3 || area.width < 6 {
        return;
    }
    let model_label = panel.model_name.clone().unwrap_or_default();
    let mut flags: Vec<PromptFlag> = Vec::new();
    // Mirror the chat prompt's flag precedence: plan wins over always-approve
    // wins over auto. Plan mode blocks edits regardless of the underlying
    // permission mode (the gate in echo-agent-runtime), so `plan` alone is the
    // honest badge even when yolo stays armed underneath.
    if panel.plan_mode {
        flags.push(PromptFlag {
            text: "plan",
            color: Some(theme.accent_plan),
            bold: false,
        });
    } else if panel.auto_approve {
        flags.push(PromptFlag {
            text: "always-approve",
            color: None,
            bold: false,
        });
    } else if panel.auto {
        // Auto (LLM classifier) mode. Blue `accent_system`.
        flags.push(PromptFlag {
            text: "auto",
            color: Some(theme.accent_system),
            bold: false,
        });
    }
    if model_label.is_empty() && flags.is_empty() && !multiline {
        return;
    }
    let info = PromptInfo {
        model_name: &model_label,
        flags: &flags,
        multiline,
        usage_warning: None,
        usage_warning_critical: false,
    };
    // Bottom border row, inside the corners — the same content rect the
    // chat prompt and dispatch box use for their info line.
    let info_rect = Rect {
        x: area.x + 1,
        y: area.y + area.height - 1,
        width: area.width.saturating_sub(2),
        height: 1,
    };
    reply.render_info_line(buf, info_rect, &info, theme.bg_base, theme, panel.focused);
}

/// Render the peek panel inline in place of the dispatch input.
///
/// The panel is a single rounded box that REPLACES the dispatch input
/// (same screen position): one box containing the recent activity
/// summary above and a `❯ reply` input at the bottom. Bottom footer
/// hints flip accordingly (handled in `render_footer`).
///
/// Layout (rounded box, 5 rows when full-height):
///
/// ```text
/// ╭───────────────────────────────────────────────────────────╮
/// │ 2m Running: cargo test · ❯ hello? · working on the fix     │
/// │                                                            │
/// │ ❯ reply                                                    │
/// ╰────────────────────────────────────────────────────────────╯
/// ```
///
/// When a permission is pending, the top line switches to the
/// question text + numbered options.
///
/// On a too-narrow / too-short area, paints nothing and returns an
/// empty result — the caller can fall back to the regular dispatch
/// rendering.
///
/// `reply` is the dashboard-owned `peek_reply` [`PromptWidget`] backing
/// the `❯ reply` line (and the reject-feedback slot in question mode);
/// rendering through the shared widget is what gives the reply paste
/// chips, selection highlighting, and caret-following scroll for free.
///
/// `overlay_area` is the rect above the box for paste-chip text
/// previews (`None` suppresses them).
///
/// Returns the reply caret position (so the caller can park the
/// terminal cursor) plus the reply input's screen rect (recorded for
/// mouse routing — click-to-focus and drag selection).
#[allow(clippy::too_many_arguments)]
pub fn render_peek_panel(
    buf: &mut Buffer,
    area: Rect,
    panel: &PeekPanelState,
    reply: &mut crate::views::prompt_widget::PromptWidget,
    theme: &Theme,
    voice_listening: bool,
    voice_interim: Option<&str>,
    multiline: bool,
    overlay_area: Option<Rect>,
    live_tail: Option<PeekLiveTailArgs<'_>>,
    empty_hint: Option<&str>,
) -> PeekRenderResult {
    use crate::views::prompt_widget::{PromptBg, PromptStyle};
    use ratatui::widgets::{Block, BorderType, Borders, Widget};
    use unicode_width::UnicodeWidthStr;
    if area.area() == 0 || area.height < 3 || area.width < 20 {
        return PeekRenderResult::default();
    }

    // Focus-aware chrome, mirroring the dispatch box's two-focus model:
    // a focused reply input gets the bright selection border (and a
    // caret below); an unfocused one (Tab) dims to the prompt border and
    // hides the caret.
    let border_fg = if panel.focused {
        theme.selection_border
    } else {
        theme.prompt_border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_fg).bg(theme.bg_base));
    let frame_inner = block.inner(area);
    block.render(area, buf);
    // Bottom-right model + always-approve indicator on the box's bottom
    // border, painted through the same shared prompt info-line renderer
    // as the dispatch box's config badge so style and position match.
    // Covers every peek mode (summary, QA, approval) since it sits on the
    // border, outside the content rows. Painted after the block so it
    // overwrites the plain `╰──╯` fill.
    paint_peek_config_badge(buf, area, theme, panel, reply, multiline);

    // Record badge on the top border while the mic is hot — the peek panel
    // replaces the dispatch box, so without this a capture started with a row
    // selected would show no indicator.
    super::render::paint_record_badge(buf, area, theme, voice_listening);

    // Add a 1-cell left + right inset inside the rounded chrome so
    // content doesn't hug the border.
    let inner = Rect {
        x: frame_inner.x.saturating_add(1),
        y: frame_inner.y,
        width: frame_inner.width.saturating_sub(2),
        height: frame_inner.height,
    };
    if inner.height == 0 || inner.width == 0 {
        return PeekRenderResult::default();
    }

    // Layout INSIDE the padded inner box:
    //   row 0: time-ago + status line
    //   middle rows: up to 3 lines of the last agent response
    //               (or the permission question + options)
    //   last rows: `❯ reply` live input (grows for multi-line drafts)
    //
    // The reply input grows upward from the box bottom as the user
    // inserts newlines (Shift+Enter); `reply_top_y` is the first reply
    // row. Clamped to leave the status row, and degenerate to the
    // bottom row when the box is single-line. The `❯ ` prefix width is
    // needed to size the reply text column for the height computation.
    let prefix = "\u{276F} ";
    let prefix_w = UnicodeWidthStr::width(prefix) as u16;
    let reply_text_w = inner.width.saturating_sub(prefix_w);
    let reply_rows = reply_row_count(reply, reply_text_w, MAX_REPLY_ROWS)
        .min(inner.height.saturating_sub(1).max(1));
    let reply_top_y = inner.y + inner.height.saturating_sub(reply_rows);

    if let Some(ref q) = panel.question {
        // Permission / ask-tool pending — render the question + options
        // across the WHOLE inner box (the `❯ reply` row is hidden here).
        // The highlighted option carries a ▸ marker; the `RejectOnce`
        // ("No") option accepts inline free-text feedback that the user
        // types after selecting it (mirrors the chat permission panel).
        let mut y = inner.y;
        let q_line = format!("\u{25B8} {q}");
        let trunc = truncate_str(&q_line, inner.width as usize);
        buf.set_string(
            inner.x,
            y,
            trunc,
            Style::default()
                .fg(theme.warning)
                .bg(theme.bg_base)
                .add_modifier(Modifier::BOLD),
        );
        y += 1;
        let max_opt_y = inner.y + inner.height.saturating_sub(1);
        let mut caret: Option<(u16, u16)> = None;
        let mut reply_rect: Option<Rect> = None;
        for (i, (_, label)) in panel.options.iter().enumerate().take(9) {
            if y > max_opt_y {
                break;
            }
            // Mark an option selected only while the panel is focused AND the
            // user has actually selected it (default is `None` — no selection,
            // so the panel reads as a navigation surface, not an answer one).
            let selected = panel.focused && panel.selected_option == Some(i);
            let is_reject = panel.reject_option == Some(i);
            let marker = if selected { "\u{25B8} " } else { "  " };
            let style = if selected {
                Style::default()
                    .fg(theme.accent_user)
                    .bg(theme.bg_base)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_primary).bg(theme.bg_base)
            };
            if is_reject {
                // Freeform feedback row (mirrors the chat permission
                // panel's `build_reject_once_line`): the `▸ N. ` prefix in
                // the option style, then the dim placeholder
                // `No, reject (type to add feedback)` which the typed
                // feedback replaces (rendered through the shared
                // `PromptWidget` so chips / selection / caret scroll all
                // work) as the user types.
                let prefix = format!("{marker}{}. ", i + 1);
                let prefix_trunc = truncate_str(&prefix, inner.width as usize);
                let prefix_w = UnicodeWidthStr::width(prefix_trunc.as_str()) as u16;
                buf.set_string(inner.x, y, &prefix_trunc, style);
                let text_x = inner.x + prefix_w;
                if text_x < inner.x + inner.width {
                    let avail = inner.x + inner.width - text_x;
                    let slot = Rect {
                        x: text_x,
                        y,
                        width: avail,
                        height: 1,
                    };
                    reply_rect = Some(slot);
                    if reply.text().is_empty() {
                        // Permission reject vs. ask-tool "Other" free-text.
                        // Painted manually (not via the widget's
                        // unfocused-only placeholder) so the hint stays
                        // visible while the caret sits on the row.
                        let placeholder_text = if panel.is_ask_question() {
                            "Other (type your own answer)"
                        } else {
                            "No, reject (type to add feedback)"
                        };
                        let placeholder = truncate_str(placeholder_text, avail as usize);
                        buf.set_string(
                            text_x,
                            y,
                            placeholder,
                            Style::default().fg(theme.gray_dim).bg(theme.bg_base),
                        );
                        if selected && panel.focused {
                            caret = Some((text_x, y));
                        }
                    } else {
                        let widget_style = PromptStyle {
                            focused: selected && panel.focused,
                            show_prefix: false,
                            vpad_top: 0,
                            chrome: false,
                            bg: PromptBg::Canvas(theme.bg_base),
                            image_preview: false,
                            ..PromptStyle::default()
                        };
                        let res = reply.draw(buf, slot, overlay_area, &widget_style, None, None);
                        if selected && panel.focused {
                            caret = res.cursor_pos;
                        }
                    }
                }
            } else {
                let opt_line = format!("{marker}{}. {}", i + 1, label);
                let trunc = truncate_str(&opt_line, inner.width as usize);
                buf.set_string(inner.x, y, trunc, style);
            }
            y += 1;
        }
        // Two-focus cue: slightly dim the whole question panel when it's not
        // focused (Tab → row nav) so it's clear the options aren't a live
        // answer surface. The border already dims; this fades the content.
        if !panel.focused {
            crate::render::color::blend_area(buf, frame_inner, Some((theme.bg_base, 0.45)), None);
        }
        // No reply row in question mode — return the feedback caret (if
        // the user is typing into the reject option).
        return PeekRenderResult { caret, reply_rect };
    }

    {
        // Last-response TYPE on the LEFT, time-ago on the FAR RIGHT —
        // like the row list's primary/secondary columns. The type label
        // and time are both rendered DIM (they're chrome); the response
        // body below gets the bright text colour so it's the easiest
        // thing to read. The label is truncated so it never collides
        // with the time.
        let time = panel.time_ago.as_str();
        let time_w = UnicodeWidthStr::width(time) as u16;
        // Reserve the time column (+1 gap) on the right; the label gets
        // the rest. When the box is too narrow for both, the label wins
        // and the time is dropped.
        let label_avail = if time_w > 0 && time_w + 1 < inner.width {
            inner.width.saturating_sub(time_w + 1) as usize
        } else {
            inner.width as usize
        };
        // While Working, the status label is secondary (a touch brighter than
        // dim chrome). Live-tail keeps painting the middle regardless.
        let working = panel.response_type == "Working";
        let label_fg = if working {
            theme.text_secondary
        } else {
            theme.gray_dim
        };
        let label_trunc = truncate_str(&panel.response_type, label_avail);
        buf.set_string(
            inner.x,
            inner.y,
            label_trunc,
            Style::default().fg(label_fg).bg(theme.bg_base),
        );
        if time_w > 0 && time_w + 1 < inner.width {
            let time_x = inner.x + inner.width - time_w;
            buf.set_string(
                time_x,
                inner.y,
                time,
                Style::default().fg(theme.gray_dim).bg(theme.bg_base),
            );
        }

        let middle_top = inner.y + 1;
        // Match `peek_live_tail_desired_content`: blank only when middle still
        // has ≥2 rows after it (pin + body). When only 1 row remains, keep it.
        let middle_bottom = live_tail_middle_bottom(middle_top, reply_top_y);
        let middle_h = middle_bottom.saturating_sub(middle_top);
        let middle_area = Rect {
            x: inner.x,
            y: middle_top,
            width: inner.width,
            height: middle_h,
        };
        if let Some(PeekLiveTailArgs { scrollback }) = live_tail {
            if middle_h > 0 {
                if scrollback.is_empty() {
                    if let Some(hint) = empty_hint.or(Some("No activity yet")) {
                        let trunc = truncate_str(hint, inner.width as usize);
                        buf.set_string(
                            inner.x,
                            middle_top,
                            trunc,
                            Style::default().fg(theme.gray_dim).bg(theme.bg_base),
                        );
                    }
                } else {
                    super::peek_tail::paint_peek_live_tail(scrollback, middle_area, buf);
                }
            }
        } else if let Some(hint) = empty_hint
            && middle_h > 0
        {
            let trunc = truncate_str(hint, inner.width as usize);
            buf.set_string(
                inner.x,
                middle_top,
                trunc,
                Style::default().fg(theme.gray_dim).bg(theme.bg_base),
            );
        }
    }

    // `❯ reply` live input occupying the bottom `reply_rows` rows,
    // rendered through the shared `PromptWidget` so folded paste chips
    // (`[Pasted: N lines]`), selection highlighting, multi-line drafts,
    // and caret-following scroll behave exactly like the dispatch box /
    // agent prompt. The `❯` prefix is painted manually on the FIRST
    // reply row only (always `accent_user`; the unfocused blend below
    // dims it); the widget draws the (possibly multi-line) text area to
    // its right, continuation lines aligning under the first. The widget
    // paints the dim `reply…` placeholder when empty AND unfocused
    // (mirrors the dispatch box — a focused input keeps its text area
    // clear, the caret is the affordance) and returns a caret only when
    // focused.
    buf.set_string(
        inner.x,
        reply_top_y,
        prefix,
        Style::default().fg(theme.accent_user).bg(theme.bg_base),
    );
    let text_area = Rect {
        x: inner.x + prefix_w,
        y: reply_top_y,
        width: reply_text_w,
        height: reply_rows,
    };
    let widget_style = PromptStyle {
        focused: panel.focused,
        show_prefix: false,
        vpad_top: 0,
        chrome: false,
        bg: PromptBg::Canvas(theme.bg_base),
        placeholder_override: Some("reply\u{2026}"),
        image_preview: false,
        ..PromptStyle::default()
    };
    // Interim STT into the reply box so voice stays visible with a peek open.
    let voice_overlay = (voice_listening || voice_interim.is_some()).then_some(
        crate::views::prompt_widget::VoicePromptOverlay {
            interim: voice_interim,
            color: theme.accent_running,
        },
    );
    let caret = reply
        .draw(
            buf,
            text_area,
            overlay_area,
            &widget_style,
            None,
            voice_overlay,
        )
        .cursor_pos;
    // The clickable reply rect spans all reply rows and includes the
    // `❯ ` prefix column for a fatter mouse target; the widget maps
    // clicks left of its text area to position 0.
    let reply_rect = Some(Rect {
        x: inner.x,
        y: reply_top_y,
        width: inner.width,
        height: reply_rows,
    });

    // Two-focus cue: slightly dim the whole panel content when it's not
    // focused (Tab → row nav), matching the question panel. The border
    // already dims; this fades the response + reply so it's clear the
    // input isn't active. Caret is `None` when unfocused, so dimming the
    // painted cells doesn't affect cursor placement.
    if !panel.focused {
        crate::render::color::blend_area(buf, frame_inner, Some((theme.bg_base, 0.45)), None);
    }
    PeekRenderResult { caret, reply_rect }
}

/// Number of rows the `❯ reply` input wants at the given reply TEXT
/// width (the inner box width minus the `❯ ` prefix), capped at `cap`.
/// Used both by the dashboard layout to size the peek box and by
/// [`render_peek_panel`] to place the reply, so a multi-line draft
/// (Shift+Enter) is fully visible. Routes through
/// [`PromptWidget::desired_height`](crate::views::prompt_widget::PromptWidget::desired_height)
/// (chromeless, no prefix — the prefix is painted separately) so paste
/// chips and wrapped lines are counted exactly as they render. Returns
/// ≥1.
pub fn reply_row_count(
    reply: &crate::views::prompt_widget::PromptWidget,
    reply_text_width: u16,
    cap: u16,
) -> u16 {
    use crate::views::prompt_widget::PromptStyle;
    let style = PromptStyle {
        focused: true,
        show_prefix: false,
        vpad_top: 0,
        chrome: false,
        ..PromptStyle::default()
    };
    reply
        .desired_height(reply_text_width, &style, false, cap.max(1))
        .max(1)
}

/// The header label for the peek panel, e.g. `"Thinking"` / `"Thought"`,
/// `"Response"`, `"Edit"`, `"Read"`, `"Bash"`, `"Preparing"`, `"Working"`, …
///
/// While the turn is RUNNING the label follows the live turn activity
/// (`Thinking` / `Responding` / a running tool / `Working` when waiting),
/// mirroring the agent view's turn-status line so the peek never dwells on a
/// stale completed response while the agent has actually moved on.
///
/// When IDLE it scans the scrollback newest-first and returns a label for the
/// first agent-produced block (a `Thinking` block reads `"Thought"` once
/// done). Scanning stops at the user's latest prompt / interjection (anything
/// before it belongs to a previous turn), falling back to `"Idle"`.
pub fn extract_last_response_type(agent: &AgentView) -> String {
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::blocks::ToolCallBlock;

    use crate::acp::tracker::TurnActivity;

    let running = !agent.session.state.is_idle();
    // While the turn is running, the live activity is the ground truth for
    // what the agent is doing RIGHT NOW — mirrors the agent view's turn-status
    // line. Driving the status from this (instead of only the scrollback scan)
    // is what keeps the peek from dwelling on the previous, now-stale
    // "Response" once the agent has moved past its last message into tool
    // execution or waiting for results.
    if running {
        match agent.session.turn_activity() {
            Some(TurnActivity::Thinking) => return "Thinking".to_string(),
            Some(TurnActivity::Responding) => return "Response".to_string(),
            Some(TurnActivity::AutoCompacting) => return "Compacting".to_string(),
            Some(TurnActivity::Retrying { .. }) => return "Retrying".to_string(),
            // A tool is executing: fall through to the scan to recover its
            // specific label (Bash/Read/…); a missing/stale block yields the
            // generic "Working" fallback below.
            Some(TurnActivity::ToolRunning { .. }) => {}
            // Blocked on a suppressed tool (task output / wait / sleep) → keep
            // the compact "Working" the peek showed before this was surfaced.
            Some(TurnActivity::Waiting(_)) => return "Working".to_string(),
            Some(TurnActivity::WritingToolCall(_)) => return "Preparing".to_string(),
            // Turn running but no live activity (e.g. just granted a
            // permission and waiting for tool results / the next inference) →
            // "Working", never a stale response.
            None => return "Working".to_string(),
        }
    }
    let len = agent.scrollback.len();
    for idx in (0..len).rev() {
        let Some(entry) = agent.scrollback.entry(idx) else {
            break;
        };
        match &entry.block {
            RenderBlock::AgentMessage(_) => {
                // Idle → the last message is the result ("Response"). While
                // running we only reach here in the ToolRunning case (other
                // activities returned above): a message is then stale, so skip
                // it and fall through to "Working" / a newer tool label.
                if running {
                    break;
                }
                return "Response".to_string();
            }
            RenderBlock::Thinking(_) => {
                return if running { "Thinking" } else { "Thought" }.to_string();
            }
            RenderBlock::ToolCall(tc) => {
                let label = match tc {
                    ToolCallBlock::Execute(_) => Some("Bash"),
                    ToolCallBlock::Read(_) => Some("Read"),
                    ToolCallBlock::Edit(_) => Some("Edit"),
                    ToolCallBlock::ListDir(_) => Some("List"),
                    ToolCallBlock::Search(_) => Some("Search"),
                    ToolCallBlock::WebFetch(_) => Some("Fetch"),
                    ToolCallBlock::WebSearch(_) => Some("Web search"),
                    ToolCallBlock::IntegrationSearch(_) => Some("Tool search"),
                    ToolCallBlock::UseTool(_) => Some("Tool"),
                    ToolCallBlock::MemorySearch(_) => Some("Memory"),
                    ToolCallBlock::Skill(_) => Some("Skill"),
                    ToolCallBlock::Other(_) => Some("Tool"),
                    // Lifecycle events aren't real tool calls — keep scanning.
                    ToolCallBlock::Lifecycle(_) => None,
                };
                if let Some(label) = label {
                    return label.to_string();
                }
            }
            RenderBlock::Subagent(_) => return "Subagent".to_string(),
            RenderBlock::Workflow(_) => return "Workflow".to_string(),
            RenderBlock::BgTask(_) => return "Task".to_string(),
            RenderBlock::Btw(_) => return "Btw".to_string(),
            RenderBlock::ContextInfo(_) => return "Context".to_string(),
            RenderBlock::CreditLimit(_) => return "Credit limit".to_string(),
            // The user's latest input marks the turn boundary — there's
            // no agent response after it yet.
            RenderBlock::UserPrompt(_) => break,
            // Structural blocks carry no response type — keep scanning.
            RenderBlock::System(_) | RenderBlock::SessionEvent(_) | RenderBlock::Stub(_) => {}
        }
    }
    if running {
        "Working".to_string()
    } else {
        "Idle".to_string()
    }
}

/// Pull the first line of the most recent user prompt
/// (`RenderBlock::UserPrompt`) from the
/// agent's scrollback. Sanitised + ANSI-stripped. Returns
/// `None` when the user hasn't sent any prompts yet.
pub fn extract_last_user_message(agent: &AgentView) -> Option<String> {
    crate::views::session_title::last_user_prompt_line(agent)
}

/// Pull the first line of the FIRST user prompt (`RenderBlock::UserPrompt`)
/// from the agent's scrollback, oldest-first. Sanitised + ANSI-stripped.
///
/// Used as a dashboard row title fallback: once a dashboard-dispatched
/// prompt drains out of `pending_prompts` it lives in the scrollback, so
/// this keeps showing the task as the title instead of flashing the
/// session-id fallback while the generated title is still being produced.
/// Returns `None` when the user hasn't sent any prompt yet.
pub fn extract_first_user_message(agent: &AgentView) -> Option<String> {
    use crate::scrollback::block::RenderBlock;
    use crate::views::session_title::sanitize_display_text;
    let len = agent.scrollback.len();
    for idx in 0..len {
        let entry = agent.scrollback.entry(idx)?;
        if let RenderBlock::UserPrompt(b) = &entry.block {
            let first = b.text.lines().next().unwrap_or("").trim();
            if first.is_empty() {
                continue;
            }
            let stripped = strip_ansi_escapes::strip_str(first);
            let safe = sanitize_display_text(&stripped).into_owned();
            return Some(safe.trim().to_string());
        }
    }
    None
}

/// Extract the last `count` short text descriptions from the given
/// agent view's scrollback.
///
/// No more `format!("{:?}", entry.block)`, which leaked
/// Rust Debug output (variant tags, struct field names, escaped
/// strings — and worst of all the head of bash commands containing
/// credentials).
///
/// Every projected string is run through
/// `strip_ansi_escapes::strip_str` so embedded `\x1b[...]` sequences
/// from agent output cannot reach the buffer.
///
/// Every projected string is also run
/// through `sanitize_display_text` so a maliciously crafted block
/// can't smuggle terminal escapes via this path.
///
/// The pipeline is project → first line → ANSI-strip
/// → sanitise. Splitting BEFORE sanitisation matters because
/// `sanitize_display_text` rewrites `\n` to U+FFFD (it's a control
/// character), so a `.lines().next()` AFTER sanitisation returns the
/// whole concatenated body. Doing the split first also avoids
/// allocating the entire body when only the first line is needed.
///
/// Returns newest-last (top to bottom = chronological).
pub fn extract_recent_lines(agent: &AgentView, count: usize) -> Vec<String> {
    let mut out = Vec::new();
    if count == 0 {
        return out;
    }
    let total = agent.scrollback.len();
    let mut i = total;
    while i > 0 && out.len() < count {
        i -= 1;
        if let Some(entry) = agent.scrollback.entry(i)
            && let Some(text) = block_short_text(&entry.block)
        {
            // Project → first line → ANSI-strip → sanitize → trim.
            let head = text.lines().next().unwrap_or("");
            let stripped = strip_ansi_escapes::strip_str(head);
            let safe = crate::views::session_title::sanitize_display_text(&stripped).into_owned();
            let first = safe.trim();
            if !first.is_empty() {
                out.push(first.to_string());
            }
        }
    }
    out.reverse();
    out
}

/// Project a `RenderBlock` to a short, user-friendly description for
/// the peek panel. Returns `None` for blocks that have no obvious
/// short text projection (we'd rather omit a row than render Rust
/// metadata).
/// For variants that own large bodies (`AgentMessage`,
/// `Thinking`), project to the first line *here* so we never allocate
/// the full body for the peek panel. UserPrompt is already first-line
/// in `b.text` because the on-disk schema collapses multi-line prompts
/// to single-line. The other variants return short fixed labels.
fn block_short_text(block: &crate::scrollback::block::RenderBlock) -> Option<String> {
    use crate::scrollback::block::RenderBlock;
    /// Read just the first non-empty line of a body owned elsewhere.
    /// `lines()` iterates without allocating; the `.to_string()` at the
    /// end is the only allocation, sized to the first line only.
    fn first_line_of(body: &str) -> String {
        body.lines().next().unwrap_or("").to_string()
    }
    match block {
        RenderBlock::UserPrompt(b) => Some(format!("\u{2771} {}", first_line_of(&b.text))),
        RenderBlock::AgentMessage(b) => Some(first_line_of(&b.text())),
        RenderBlock::Thinking(b) => Some(format!("(thinking) {}", first_line_of(&b.text()))),
        RenderBlock::System(_) => Some("(system event)".to_string()),
        RenderBlock::SessionEvent(_) => Some("(session event)".to_string()),
        RenderBlock::ToolCall(_) => Some("(tool call)".to_string()),
        RenderBlock::BgTask(_) => Some("(background task)".to_string()),
        RenderBlock::Subagent(_) => Some("(subagent)".to_string()),
        RenderBlock::Workflow(_) => Some("(workflow)".to_string()),
        RenderBlock::Btw(_) => Some("(btw)".to_string()),
        RenderBlock::ContextInfo(_) => Some("(context info)".to_string()),
        RenderBlock::CreditLimit(_) => Some("(credit limit)".to_string()),
        RenderBlock::Stub(_) => None,
    }
}

/// Map a 1-based number key on the peek panel to a
/// `DashboardPermissionSelect` action when the panel is showing a
/// permission question.
///
/// Emit the `DashboardPermissionSelect` variant so
/// the dispatcher can route the answer to the row's owning agent
/// (not the active-view agent — which is the dashboard itself).
///
/// Couple the `request_id` captured at snapshot time
/// so the dispatcher can drop a stale answer if the front of the
/// permission queue has rotated.
///
/// `n = 0` returns `None` (would otherwise
/// `saturating_sub(1)` to index 0 and erroneously select option 1).
/// `n > options.len()` returns `None`. No peek / no question returns
/// `None`.
pub fn peek_number_key(state: &super::state::DashboardState, n: usize) -> Option<Action> {
    let panel = state.peek.as_ref()?;
    // No active question, or 0/out-of-range indices, → no-op.
    panel.question.as_ref()?;
    if n == 0 {
        return None;
    }
    let idx = n - 1;
    panel.options.get(idx)?;
    if panel.is_ask_question() {
        // Ask tool: answer by option index, or the "Other" free-text row
        // (the reject_option index) with the current draft (held by the
        // dashboard's `peek_reply` widget).
        if panel.reject_option == Some(idx) {
            return Some(Action::DashboardQuestionAnswer {
                row: panel.row.clone(),
                option_idx: None,
                freeform: state.peek_reply.text_without_image_chips(),
            });
        }
        return Some(Action::DashboardQuestionAnswer {
            row: panel.row.clone(),
            option_idx: Some(idx),
            freeform: String::new(),
        });
    }
    let request_id = panel.request_id?;
    let (option_id, _) = panel.options.get(idx)?;
    let id =
        agent_client_protocol::PermissionOptionId::new(std::sync::Arc::from(option_id.as_str()));
    Some(Action::DashboardPermissionSelect {
        row: panel.row.clone(),
        request_id,
        option_id: id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::agent::AgentId;
    use crate::views::dashboard::state::DashboardState;

    /// Build a `PeekFields` for tests with sensible defaults.
    fn fields(response_type: &str) -> PeekFields {
        PeekFields {
            label: "label".to_string(),
            time_ago: "2m".to_string(),
            response_type: response_type.to_string(),
            last_user_message: None,
            question: None,
            options: Vec::new(),
            request_id: None,
            reject_option: None,
        }
    }

    /// Fresh reply widget for render tests (the dashboard-owned
    /// `peek_reply` stand-in).
    fn test_reply() -> crate::views::prompt_widget::PromptWidget {
        crate::views::prompt_widget::PromptWidget::new()
    }

    #[test]
    fn live_tail_middle_bottom_skips_blank_when_only_one_content_row() {
        // status@0, middle from 1, 3-line reply starts at 3 → span 2.
        // blank would leave middle_h=1 (pin-only); expand so pin+body fit.
        assert_eq!(live_tail_middle_bottom(1, 3), 3);
        // Generous middle (span 4) keeps blank above reply.
        assert_eq!(live_tail_middle_bottom(1, 5), 4);
        // Zero middle span stays empty.
        assert_eq!(live_tail_middle_bottom(3, 3), 2);
    }

    /// Tight box: status + pin + body + 3-line reply, no blank budget.
    /// Paint must not steal the body row for a breathing blank.
    #[test]
    fn render_peek_tight_pin_shows_current_turn_body() {
        use crate::scrollback::block::RenderBlock;
        use crate::scrollback::entry::ScrollbackEntry;
        use crate::scrollback::state::ScrollbackState;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        // borders(2) + inner content_rows(6) = 8.
        let area = Rect::new(0, 0, 80, 8);
        let mut buf = Buffer::empty(area);
        let theme = Theme::current();
        let panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Response"));
        let mut reply = test_reply();
        reply.set_text("r1\nr2\nr3");
        let mut sb = ScrollbackState::new();
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt("user pin")));
        sb.push(ScrollbackEntry::new(RenderBlock::agent_message(
            "current turn line",
        )));
        let _ = render_peek_panel(
            &mut buf,
            area,
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            Some(PeekLiveTailArgs { scrollback: &sb }),
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        assert!(content.contains("user pin"), "pin must paint: {content:?}");
        assert!(
            content.contains("current turn line"),
            "body must not be eaten by blank: {content:?}"
        );
    }

    /// While the agent is working the "Working" status label renders in the
    /// secondary colour; other status labels stay dim chrome.
    #[test]
    fn render_peek_working_status_uses_secondary_colour() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = Theme::current();
        let render = |response_type: &str| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 80, 6));
            let panel =
                PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields(response_type));
            let mut reply = test_reply();
            let _ = render_peek_panel(
                &mut buf,
                Rect::new(0, 0, 80, 6),
                &panel,
                &mut reply,
                &theme,
                false,
                None,
                false,
                None,
                None,
                None,
            );
            buf
        };
        // Inner content sits two cells in (1 border + 1 pad inset): status at (2,1).
        let working = render("Working");
        assert_eq!(working[(2, 1)].symbol(), "W", "status label is `Working`");
        assert_eq!(
            working[(2, 1)].fg,
            theme.text_secondary,
            "the `Working` status must render in the secondary colour",
        );

        let idle = render("Response");
        assert_eq!(idle[(2, 1)].symbol(), "R", "status label is `Response`");
        assert_eq!(
            idle[(2, 1)].fg,
            theme.gray_dim,
            "a non-working status stays dim chrome",
        );
    }

    /// The peeked agent's model + always-approve flag render on the box's
    /// bottom border (the same config-badge slot the dispatch box uses) —
    /// in both summary mode and pending-question (approval) mode.
    #[test]
    fn render_peek_shows_model_and_auto_approve_on_bottom_border() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = Theme::current();

        let badge_row = |panel: &PeekPanelState, h: u16| -> String {
            let mut buf = Buffer::empty(Rect::new(0, 0, 80, h));
            let mut reply = test_reply();
            let _ = render_peek_panel(
                &mut buf,
                Rect::new(0, 0, 80, h),
                panel,
                &mut reply,
                &theme,
                false,
                None,
                false,
                None,
                None,
                None,
            );
            (0..80)
                .map(|x| buf[(x, h - 1)].symbol().to_string())
                .collect()
        };

        // Summary mode → model + always-approve on the bottom border.
        let mut panel =
            PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Response"));
        panel.model_name = Some("EchoAgent 4 Fast".to_string());
        panel.auto_approve = true;
        let bottom = badge_row(&panel, 6);
        assert!(
            bottom.contains("EchoAgent 4 Fast"),
            "model on bottom border: {bottom:?}"
        );
        assert!(
            bottom.contains("always-approve"),
            "always-approve flag: {bottom:?}"
        );

        // Pending-question (approval) mode → badge still painted.
        let mut q = fields("Response");
        q.question = Some("Allow write?".to_string());
        q.options = vec![
            ("allow".into(), "Allow".into()),
            ("deny".into(), "Deny".into()),
        ];
        q.request_id = Some(1);
        let mut qpanel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), q);
        qpanel.model_name = Some("EchoAgent 4 Fast".to_string());
        let qbottom = badge_row(&qpanel, 8);
        assert!(
            qbottom.contains("EchoAgent 4 Fast"),
            "model shows in question/approval mode: {qbottom:?}",
        );

        // No always-approve flag when the agent isn't in yolo mode.
        let mut plain =
            PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Response"));
        plain.model_name = Some("EchoAgent 4 Fast".to_string());
        plain.auto_approve = false;
        let plain_bottom = badge_row(&plain, 6);
        assert!(
            !plain_bottom.contains("always-approve"),
            "no flag without yolo: {plain_bottom:?}",
        );

        // Plan mode → a `plan` flag (so all three Shift+Tab cycle states
        // are visible on the badge).
        let mut planp =
            PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Response"));
        planp.model_name = Some("EchoAgent 4 Fast".to_string());
        planp.plan_mode = true;
        let plan_bottom = badge_row(&planp, 6);
        assert!(
            plan_bottom.contains("plan"),
            "plan flag must show in plan mode: {plan_bottom:?}",
        );

        // Plan + always-approve → `plan` only. Plan mode blocks edits in every
        // permission mode (shell-side gate), so the plan badge is the honest
        // one; yolo stays armed underneath and reappears once plan exits.
        planp.auto_approve = true;
        planp.auto = true;
        let plan_yolo_bottom = badge_row(&planp, 6);
        assert!(
            plan_yolo_bottom.contains("plan"),
            "plan flag must show in plan+yolo: {plan_yolo_bottom:?}",
        );
        assert!(
            !plan_yolo_bottom.contains("always-approve") && !plan_yolo_bottom.contains("auto"),
            "plan suppresses always-approve and auto: {plan_yolo_bottom:?}",
        );

        // Yolo without plan → `always-approve` (and it wins over auto).
        planp.plan_mode = false;
        let yolo_bottom = badge_row(&planp, 6);
        assert!(
            yolo_bottom.contains("always-approve") && !yolo_bottom.contains("auto"),
            "always-approve shows once plan is off and wins over auto: {yolo_bottom:?}",
        );
    }

    /// The peek panel paints the `● rec` badge on its top border while voice
    /// capture is active, and streams the interim transcript into the reply box
    /// — without this, voice started with a row selected (peek replaces the
    /// dispatch box) would show no indicator at all.
    #[test]
    fn render_peek_paints_record_badge_and_interim_when_listening() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = Theme::current();
        let panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Response"));
        let mut reply = test_reply();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 6));
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 6),
            &panel,
            &mut reply,
            &theme,
            true,
            Some("hello there"),
            false,
            None,
            None,
            None,
        );
        // Badge `" ● rec "` starts at x = area.x + 2, so the dot is at x = 3.
        assert_eq!(
            buf[(3, 0)].symbol(),
            "\u{25CF}",
            "record dot must paint on the peek top border while listening"
        );
        // The interim transcript renders somewhere in the box body.
        let body: String = (0..6)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(
            body.contains("hello there"),
            "interim transcript must stream into the peek reply, got: {body:?}"
        );
    }

    #[test]
    fn peek_handles_missing_question() {
        let mut f = fields("Idle");
        f.last_user_message = Some("hello?".to_string());
        let state = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), f);
        assert!(state.question.is_none());
        assert_eq!(state.last_user_message.as_deref(), Some("hello?"));
    }

    /// `apply_fields` reports whether the peeked row CHANGED so the
    /// caller (the render-time refresh) can clear the dashboard's
    /// `peek_reply` draft — a half-typed reply can never be sent to the
    /// wrong agent after the selection cursor moves.
    #[test]
    fn apply_fields_reports_row_change() {
        let mut state = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
        // Same row → no change reported (caller preserves the draft).
        let changed = state.apply_fields(
            DashboardRowId::TopLevel(AgentId(0)),
            fields("Running\u{2026}"),
        );
        assert!(!changed, "same row must not report a change");
        assert_eq!(state.response_type, "Running\u{2026}");
        // Different row → change reported (caller clears the draft).
        let changed = state.apply_fields(DashboardRowId::TopLevel(AgentId(1)), fields("Idle"));
        assert!(changed, "row change must be reported");
        assert_eq!(state.row, DashboardRowId::TopLevel(AgentId(1)));
    }

    /// `render_peek_panel` paints a single rounded box
    /// (no title bar, no inline hint strip) with the status +
    /// recent messages condensed onto the top row and a `❯ reply`
    /// input on the bottom row (focused by default, so no dim
    /// placeholder — the caret is the affordance). The bottom footer
    /// hints (rendered by `render_footer` outside the box) carry the
    /// `space:close` / `enter:open` affordances.
    #[test]
    fn render_peek_paints_rounded_box_with_summary_and_reply_input() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let theme = Theme::current();
        let mut f = fields("Edit");
        f.label = "Add responsiveness to /context".to_string();
        f.last_user_message = Some("hello, can you help?".to_string());
        let mut panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), f);
        panel.focused = true;
        let mut reply = test_reply();
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 5),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        // Rounded-box corners.
        for corner in ['\u{256d}', '\u{256e}', '\u{2570}', '\u{256f}'] {
            assert!(
                content.contains(corner),
                "peek must paint rounded corner `{corner}`, got: {content:?}",
            );
        }
        // Header row: last-response TYPE on the left, time-ago right.
        assert!(content.contains("2m"));
        assert!(content.contains("Edit"));
        // The most recent user message is NOT jammed into the header.
        assert!(!content.contains("hello, can you help?"));
        // Reply input on the bottom row. Focused by default → the dim
        // `reply…` placeholder is suppressed (the caret is the
        // affordance, mirroring the dispatch box).
        assert!(
            content.contains('\u{276F}'),
            "peek must paint ❯ on the reply row"
        );
        assert!(
            !content.contains("reply\u{2026}"),
            "focused reply input must not paint the placeholder, got: {content:?}"
        );
        // No in-box title bar / [×] close.
        assert!(
            !content.contains("Peek \u{2014}"),
            "round-7 peek must NOT paint `Peek —` title, got: {content:?}",
        );
        assert!(
            !content.contains("[\u{00d7}]"),
            "round-7 peek must NOT paint `[×]` close, got: {content:?}",
        );
    }

    /// The reply input renders the typed draft (not the dim
    /// placeholder) and reports a caret position.
    #[test]
    fn render_peek_shows_typed_reply_and_caret() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
        panel.focused = true;
        let mut reply = test_reply();
        reply.set_text("ship it");
        let res = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 5),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        assert!(content.contains("ship it"), "got: {content:?}");
        // No placeholder once the user has typed.
        assert!(!content.contains("reply\u{2026}"), "got: {content:?}");
        assert!(res.caret.is_some(), "reply input must report a caret");
        assert!(res.reply_rect.is_some(), "reply rect must be reported");
    }

    /// An unfocused panel (Tab) hides the caret — `render_peek_panel`
    /// returns `None` — while still painting the draft text.
    #[test]
    fn render_peek_unfocused_hides_caret() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
        let mut reply = test_reply();
        reply.set_text("draft");
        panel.focused = false;
        let res = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 5),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        assert!(
            res.caret.is_none(),
            "unfocused panel must not report a caret"
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        // Draft is still visible even when unfocused.
        assert!(content.contains("draft"), "got: {content:?}");
    }

    /// An unfocused panel with an EMPTY reply paints the dim `reply…`
    /// placeholder (a focused one keeps the row clear — the caret is
    /// the affordance, mirroring the dispatch box).
    #[test]
    fn render_peek_unfocused_empty_reply_paints_placeholder() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
        panel.focused = false;
        let mut reply = test_reply();
        let res = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 5),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        assert!(
            res.caret.is_none(),
            "unfocused panel must not report a caret"
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        assert!(
            content.contains("reply\u{2026}"),
            "unfocused empty reply must paint the placeholder, got: {content:?}"
        );
    }

    /// An unfocused simple peek (response + reply) dims its CONTENT as a
    /// two-focus cue — not just the border. At least one interior cell's
    /// fg is faded toward bg relative to the focused render.
    #[test]
    fn render_peek_unfocused_simple_panel_dims_content() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let theme = Theme::current();
        let render = |focused: bool| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 80, 6));
            let mut panel =
                PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
            let mut reply = test_reply();
            reply.set_text("draft");
            panel.focused = focused;
            let _ = render_peek_panel(
                &mut buf,
                Rect::new(0, 0, 80, 6),
                &panel,
                &mut reply,
                &theme,
                false,
                None,
                false,
                None,
                None,
                None,
            );
            buf
        };
        let focused = render(true);
        let unfocused = render(false);
        // Scan INTERIOR cells only (exclude the border, which changes color
        // independently of the content dim).
        let mut content_differs = false;
        for y in 1..focused.area.height - 1 {
            for x in 1..focused.area.width - 1 {
                if focused[(x, y)].fg != unfocused[(x, y)].fg {
                    content_differs = true;
                }
            }
        }
        // On terminals without truecolor the theme quantizes to named /
        // indexed colors that `blend_color` leaves unchanged, so the dim is
        // a no-op there (the border still dims). Only assert the content
        // fade when the active theme actually supports blending.
        let blend_supported =
            crate::render::color::blend_color(theme.bg_base, theme.text_primary, 0.45).is_some();
        if blend_supported {
            assert!(
                content_differs,
                "unfocused simple peek must fade its content, not only the border",
            );
        }
    }

    /// When a permission is pending, the peek paints
    /// the question + numbered options at the top and still
    /// shows the reply slot on the last inner row. The 1-9
    /// keys still answer the permission via `peek_number_key`.
    #[test]
    fn render_peek_paints_permission_question_with_options() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 8));
        let theme = Theme::current();
        let panel = PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            PeekFields {
                label: "agent".to_string(),
                time_ago: String::new(),
                response_type: "Awaiting your input".to_string(),
                last_user_message: None,
                question: Some("Allow Edit?".to_string()),
                options: vec![
                    ("allow_once".into(), "Allow once".into()),
                    ("deny".into(), "Deny".into()),
                ],
                request_id: Some(42),
                // No RejectOnce option here — both render as plain rows.
                reject_option: None,
            },
        );
        let mut reply = test_reply();
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 8),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        assert!(content.contains("Allow Edit?"));
        assert!(content.contains("1. Allow once"));
        assert!(content.contains("2. Deny"));
        // The `❯ reply` row is hidden while a question is pending.
        assert!(!content.contains("reply"), "got: {content:?}");
    }

    /// The highlighted option (`selected_option`) is marked with `▸`
    /// and the others with a plain indent, so `↑`/`↓` navigation is
    /// visible.
    #[test]
    fn render_peek_highlights_selected_option() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 8));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            PeekFields {
                label: "agent".to_string(),
                time_ago: String::new(),
                response_type: "Awaiting your input".to_string(),
                last_user_message: None,
                question: Some("Allow Edit?".to_string()),
                options: vec![
                    ("allow".into(), "Allow".into()),
                    ("deny".into(), "Deny".into()),
                ],
                request_id: Some(1),
                reject_option: None,
            },
        );
        panel.selected_option = Some(1); // highlight the 2nd option
        panel.focused = true;
        let mut reply = test_reply();
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 8),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        // Selected option carries the ▸ marker; the other is plain.
        assert!(content.contains("\u{25b8} 2. Deny"), "got: {content:?}");
        assert!(content.contains("  1. Allow"), "got: {content:?}");
    }

    /// An UNFOCUSED question panel (Tab → row nav) marks no option as
    /// selected — every option renders with the plain indent, never the
    /// `▸` marker — so it doesn't falsely imply Enter answers it.
    #[test]
    fn render_peek_unfocused_question_has_no_selected_option() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 8));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            PeekFields {
                label: "agent".to_string(),
                time_ago: String::new(),
                response_type: "Awaiting your input".to_string(),
                last_user_message: None,
                question: Some("Allow Edit?".to_string()),
                options: vec![
                    ("allow".into(), "Allow".into()),
                    ("deny".into(), "Deny".into()),
                ],
                request_id: Some(1),
                reject_option: None,
            },
        );
        panel.selected_option = Some(1);
        panel.focused = false; // Tab → row nav
        let mut reply = test_reply();
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 8),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        // Both options render with the plain indent — no ▸ option marker.
        assert!(content.contains("  1. Allow"), "got: {content:?}");
        assert!(content.contains("  2. Deny"), "got: {content:?}");
        assert!(
            !content.contains("\u{25b8} 1.") && !content.contains("\u{25b8} 2."),
            "unfocused question must not mark any option selected, got: {content:?}",
        );
    }

    /// When the reject option is highlighted, the panel renders an inline
    /// feedback field (the typed text), hides the `❯ reply` row, and
    /// reports a caret into the feedback.
    #[test]
    fn render_peek_reject_option_shows_inline_feedback() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 8));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            PeekFields {
                label: "agent".to_string(),
                time_ago: String::new(),
                response_type: "Awaiting your input".to_string(),
                last_user_message: None,
                question: Some("Allow Edit?".to_string()),
                options: vec![
                    ("allow".into(), "Allow".into()),
                    ("reject".into(), "No".into()),
                ],
                request_id: Some(1),
                reject_option: Some(1),
            },
        );
        panel.selected_option = Some(1); // highlight the reject option
        panel.focused = true;
        let mut reply = test_reply();
        reply.set_text("do it differently");
        let res = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 8),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        // The typed feedback REPLACES the placeholder on the reject row.
        assert!(content.contains("do it differently"), "got: {content:?}");
        assert!(
            !content.contains("type to add feedback"),
            "typed text must replace the placeholder, got: {content:?}",
        );
        // The `❯ reply` row is hidden while answering.
        assert!(!content.contains("reply"), "got: {content:?}");
        // Caret reports into the feedback field.
        assert!(res.caret.is_some(), "feedback input must report a caret");
        assert!(
            res.reply_rect.is_some(),
            "feedback slot rect must be reported for mouse routing"
        );
    }

    /// An ask-tool question (no `request_id`) shows the "Other" free-text
    /// row with the ask placeholder rather than the permission one.
    #[test]
    fn render_peek_ask_other_uses_ask_placeholder() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 8));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            PeekFields {
                label: "agent".to_string(),
                time_ago: String::new(),
                response_type: "Awaiting your input".to_string(),
                last_user_message: None,
                question: Some("Which approach?".to_string()),
                options: vec![
                    ("Redis".into(), "Redis".into()),
                    ("__other__".into(), "Other".into()),
                ],
                // No request_id → this is an ask question, not a permission.
                request_id: None,
                reject_option: Some(1),
            },
        );
        panel.selected_option = Some(1); // highlight the "Other" row
        let mut reply = test_reply();
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 8),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        assert!(panel.is_ask_question());
        assert!(
            content.contains("Other (type your own answer)"),
            "got: {content:?}"
        );
        // Not the permission placeholder.
        assert!(!content.contains("reject"), "got: {content:?}");
    }

    /// With no feedback typed yet, the highlighted reject option shows a
    /// dim "(type to add feedback)" hint.
    #[test]
    fn render_peek_reject_option_shows_feedback_hint() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 8));
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            PeekFields {
                label: "agent".to_string(),
                time_ago: String::new(),
                response_type: "Awaiting your input".to_string(),
                last_user_message: None,
                question: Some("Allow Edit?".to_string()),
                options: vec![
                    ("allow".into(), "Allow".into()),
                    ("reject".into(), "No".into()),
                ],
                request_id: Some(1),
                reject_option: Some(1),
            },
        );
        panel.selected_option = Some(1);
        let mut reply = test_reply();
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 8),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        // Matches the chat permission panel's placeholder verbatim.
        assert!(
            content.contains("No, reject (type to add feedback)"),
            "got: {content:?}",
        );
    }

    /// A multi-line paste into the peek reply folds into a single
    /// `[Pasted: N lines]` chip (the shared `PromptWidget` pipeline),
    /// and the chip renders on the reply row.
    #[test]
    fn render_peek_reply_folds_long_paste_into_chip() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let theme = Theme::current();
        let panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
        let mut reply = test_reply();
        reply.set_compact(true);
        let pasted = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven";
        let _ = reply.handle_paste(pasted);
        assert_eq!(reply.text(), pasted, "raw paste text is preserved");
        let _ = render_peek_panel(
            &mut buf,
            Rect::new(0, 0, 80, 5),
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            None,
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        assert!(
            content.contains("[Pasted: 11 lines]"),
            "long paste must render as a folded chip, got: {content:?}",
        );
    }

    /// With an overlay rect, the paste chip preview paints the raw
    /// content above the reply (agent prompt parity). Without an
    /// overlay the chip alone is shown — the historical dashboard gap.
    #[test]
    fn render_peek_reply_paste_preview_uses_overlay() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        crate::appearance::cache::set_vim_mode(false);
        let theme = Theme::current();
        let mut panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
        panel.focused = true;
        let mut reply = test_reply();
        reply.set_compact(true);
        let pasted = "alpha_preview_line\nbeta\ngamma";
        let _ = reply.handle_paste(pasted);
        // Cursor sits after the chip → preview is shown (near-cursor).
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 14));
        // min_height for the preview overlay is 5.
        let overlay = Rect::new(0, 0, 80, 7);
        let panel_area = Rect::new(0, 7, 80, 7);
        let _ = render_peek_panel(
            &mut buf,
            panel_area,
            &panel,
            &mut reply,
            &theme,
            false,
            None,
            false,
            Some(overlay),
            None,
            None,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        assert!(
            content.contains("[Pasted: 3 lines]"),
            "chip must still render, got: {content:?}",
        );
        assert!(
            content.contains("alpha_preview_line"),
            "paste preview overlay must paint raw text, got: {content:?}",
        );
    }

    /// `reply_row_count` grows with newlines (one row per line) and is
    /// capped at the requested maximum.
    #[test]
    fn reply_row_count_grows_with_newlines_and_caps() {
        let mut reply = test_reply();
        assert_eq!(
            reply_row_count(&reply, 40, MAX_REPLY_ROWS),
            1,
            "empty reply wants a single row",
        );
        reply.set_text("line one\nline two\nline three");
        assert_eq!(
            reply_row_count(&reply, 40, MAX_REPLY_ROWS),
            3,
            "a 3-line reply wants 3 rows",
        );
        // Past the cap the count saturates (the widget scrolls instead).
        reply.set_text(&"x\n".repeat(20));
        assert_eq!(
            reply_row_count(&reply, 40, MAX_REPLY_ROWS),
            MAX_REPLY_ROWS,
            "reply rows are capped at MAX_REPLY_ROWS",
        );
    }

    /// A multi-line reply makes `render_peek_panel` place the reply on
    /// more than one row (the box must be tall enough to host it). The
    /// returned `reply_rect` spans every reply row, and each line of the
    /// draft is painted.
    #[test]
    fn render_peek_grows_reply_for_multiline_draft() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        // Tall box so the grown reply isn't clamped.
        let area = Rect::new(0, 0, 80, 14);
        let mut buf = Buffer::empty(area);
        let theme = Theme::current();
        let panel = PeekPanelState::new(DashboardRowId::TopLevel(AgentId(0)), fields("Idle"));
        let mut reply = test_reply();
        reply.set_text("alpha\nbravo\ncharlie");
        let res = render_peek_panel(
            &mut buf, area, &panel, &mut reply, &theme, false, None, false, None, None, None,
        );
        let rect = res.reply_rect.expect("reply rect must be reported");
        assert!(
            rect.height >= 3,
            "reply rect must span the multi-line draft, got height {}",
            rect.height,
        );
        let mut content = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                content.push_str(buf[(x, y)].symbol());
            }
            content.push('\n');
        }
        for line in ["alpha", "bravo", "charlie"] {
            assert!(
                content.contains(line),
                "every reply line must be painted, missing {line:?} in: {content:?}",
            );
        }
    }

    fn dashboard_with_peek(
        options: Vec<(String, String)>,
        request_id: Option<usize>,
        question: Option<String>,
    ) -> DashboardState {
        let mut s = DashboardState::new();
        s.peek = Some(PeekPanelState::new(
            DashboardRowId::TopLevel(AgentId(0)),
            PeekFields {
                label: "label".to_string(),
                time_ago: String::new(),
                response_type: "Idle".to_string(),
                last_user_message: None,
                question,
                options,
                request_id,
                reject_option: None,
            },
        ));
        s
    }

    /// peek_number_key returns the matching option id.
    #[test]
    fn peek_number_key_selects_option() {
        let opts = vec![
            ("allow".to_string(), "Allow".to_string()),
            ("deny".to_string(), "Deny".to_string()),
        ];
        let state = dashboard_with_peek(opts, Some(42), Some("Run rm?".into()));
        let action = peek_number_key(&state, 1).expect("should produce action");
        match action {
            Action::DashboardPermissionSelect {
                request_id,
                option_id,
                ..
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(option_id.0.as_ref(), "allow");
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    /// n=0 returns None (no off-by-one selection of option 1).
    #[test]
    fn peek_number_key_zero_is_none() {
        let opts = vec![("a".to_string(), "A".to_string())];
        let state = dashboard_with_peek(opts, Some(1), Some("q".into()));
        assert!(peek_number_key(&state, 0).is_none());
    }

    /// n past options is None.
    #[test]
    fn peek_number_key_past_options_is_none() {
        let opts = vec![("a".to_string(), "A".to_string())];
        let state = dashboard_with_peek(opts, Some(1), Some("q".into()));
        assert!(peek_number_key(&state, 5).is_none());
        assert!(peek_number_key(&state, 10).is_none());
    }

    /// Missing question → None even if options are non-empty.
    #[test]
    fn peek_number_key_no_question_is_none() {
        let opts = vec![("a".to_string(), "A".to_string())];
        let state = dashboard_with_peek(opts, Some(1), None);
        assert!(peek_number_key(&state, 1).is_none());
    }

    /// No peek panel → None.
    #[test]
    fn peek_number_key_no_panel_is_none() {
        let state = DashboardState::new();
        assert!(peek_number_key(&state, 1).is_none());
    }

    // Real `extract_recent_lines` coverage lives in dispatch.rs tests
    // (`extract_recent_lines_*`) because it needs a real `AgentView`
    // built via `test_app_with_agent()`.
}
