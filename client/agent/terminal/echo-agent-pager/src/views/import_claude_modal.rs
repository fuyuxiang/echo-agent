//! Interactive modal for selectively importing Claude settings.
//!
//! Shown when the user runs `/import-claude` (in-session) or presses `i` on
//! the welcome screen with new Claude settings detected. Users review each
//! discovered item, toggle which to import, and confirm. Only checked items
//! are written to `.echo-agent/config.toml`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use echo_agent_runtime::claude_import::{ImportPlan, ImportableItem, PathKind, find_project_root};
use echo_agent_workspace::permission::types::RuleAction;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use std::path::PathBuf;

use crate::theme::Theme;
use crate::views::modal_window::{
    FoldInfo, ModalSizing, ModalWindowConfig, ModalWindowOutcome, ModalWindowState, Shortcut,
    handle_modal_key, handle_modal_mouse, render_modal_window,
};

/// Sentinel ID for non-clickable hint shortcuts. Not routable because
/// only `clickable: true` shortcuts generate hit-test areas.
const SHORTCUT_ID_HINT: usize = usize::MAX;

/// Shortcut IDs for the import-claude modal footer.
const SHORTCUT_ID_SELECT_ALL: usize = 0;
const SHORTCUT_ID_SELECT_NONE: usize = 1;
const SHORTCUT_ID_CONFIRM: usize = 2;
const SHORTCUT_ID_CANCEL: usize = 3;

/// State for the import-claude modal.
pub struct ImportClaudeModalState {
    /// The full plan as scanned from `.claude/` sources.
    pub plan: ImportPlan,
    /// Working directory used to resolve project paths.
    pub cwd: PathBuf,
    /// Per-item selection (parallel to flattened item list).
    pub selected: Vec<bool>,
    /// Currently focused row in the flattened list (0-indexed).
    pub focus: usize,
    /// Top of visible scroll window.
    pub scroll_offset: usize,
    /// Screen rect where rows are rendered -- populated by the renderer so
    /// `handle_mouse` can map click coordinates back to row indices. `None`
    /// until the first draw.
    pub content_area: Option<ratatui::layout::Rect>,
    /// Shared modal window chrome state (close button, shortcuts, hover).
    pub window: ModalWindowState,
    /// Whether the mouse is hovering over the fold indicator on the
    /// focused header row.
    pub fold_indicator_hovered: bool,
    /// Section keys currently collapsed. Uses stable string keys (not row
    /// indices) so they survive row-list rebuilds.
    pub collapsed: std::collections::HashSet<String>,
}

/// Outcome of an input event for the caller to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportClaudeModalOutcome {
    /// User pressed Esc — cancel without importing.
    Cancelled,
    /// User pressed Enter — import the items whose `selected` is true.
    Confirmed,
    /// State changed; caller should re-render.
    Changed,
    /// Key not consumed.
    Unchanged,
}

/// One row in the flattened display list. Used for navigation, rendering,
/// and translating the focused index back to a (scope, item-index) tuple.
#[derive(Debug, Clone)]
enum Row {
    /// Top-level scope header (Global / Project). Selectable: toggling toggles
    /// every item in the scope.
    ScopeHeader {
        label: String,
        flat_indices: Vec<usize>,
        section_key: String,
    },
    /// Type subgroup header (Permissions / Env vars / MCP servers / Hooks /
    /// Paths). Selectable: toggling toggles every item in the subgroup.
    TypeHeader {
        kind: ItemKind,
        flat_indices: Vec<usize>,
        section_key: String,
    },
    /// Item row, with index into the source vec.
    Item {
        scope: Scope,
        item_index: usize,
        flat_index: usize, // index into `selected`
    },
    /// Blank spacer.
    Blank,
}

/// Group items by type for nicer display + bulk-toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Permission,
    EnvVar,
    McpServer,
    Hook,
    PathEntry,
}

impl ItemKind {
    fn of(item: &ImportableItem) -> Self {
        match item {
            ImportableItem::Permission(_) => Self::Permission,
            ImportableItem::EnvVar { .. } => Self::EnvVar,
            ImportableItem::McpServer { .. } => Self::McpServer,
            ImportableItem::Hook { .. } => Self::Hook,
            ImportableItem::PathEntry { .. } => Self::PathEntry,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Permission => "Permissions",
            Self::EnvVar => "Env vars",
            Self::McpServer => "MCP servers",
            Self::Hook => "Hooks",
            Self::PathEntry => "Paths",
        }
    }

    /// Stable sort order for grouping within a scope.
    fn order(&self) -> u8 {
        match self {
            Self::Permission => 0,
            Self::EnvVar => 1,
            Self::McpServer => 2,
            Self::Hook => 3,
            Self::PathEntry => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Global,
    Project,
}

impl ImportClaudeModalState {
    /// Build a new modal with all items selected by default.
    pub fn new(plan: ImportPlan, cwd: PathBuf) -> Self {
        let total = plan.global_items.len() + plan.project_items.len();
        let selected = vec![true; total];
        // Focus the first selectable row (skip headers if any).
        let no_collapsed = std::collections::HashSet::new();
        let rows = build_rows(&plan, &cwd, &no_collapsed);
        let focus = rows
            .iter()
            .position(|r| matches!(r, Row::Item { .. }))
            .unwrap_or(0);
        Self {
            plan,
            cwd,
            selected,
            focus,
            scroll_offset: 0,
            content_area: None,
            window: ModalWindowState::new(),
            fold_indicator_hovered: false,
            collapsed: std::collections::HashSet::new(),
        }
    }

    /// Number of items currently selected for import.
    pub fn selected_count(&self) -> usize {
        self.selected.iter().filter(|&&b| b).count()
    }

    /// Total number of items in the plan.
    pub fn total_count(&self) -> usize {
        self.selected.len()
    }

    /// Build a filtered ImportPlan containing only the user-selected items.
    pub fn filtered_plan(&self) -> ImportPlan {
        let mut plan = ImportPlan::default();
        let global_count = self.plan.global_items.len();
        for (i, item) in self.plan.global_items.iter().enumerate() {
            if self.selected.get(i).copied().unwrap_or(false) {
                plan.global_items.push(item.clone());
            }
        }
        for (i, item) in self.plan.project_items.iter().enumerate() {
            let flat_idx = global_count + i;
            if self.selected.get(flat_idx).copied().unwrap_or(false) {
                plan.project_items.push(item.clone());
            }
        }
        plan
    }

    /// Handle a mouse event. Recognizes:
    /// - **Left click on an item row**: focus + toggle that item.
    /// - **Left click on a section/type header**: focus + tri-state toggle
    ///   (selects all if any unselected, deselects all if everything selected).
    /// - **Scroll wheel**: scroll up/down through the content.
    /// - Clicks outside the content area are ignored.
    pub fn handle_mouse(
        &mut self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
    ) -> ImportClaudeModalOutcome {
        use crossterm::event::{MouseButton, MouseEventKind};

        // Let the shared modal window chrome handle close button, shortcuts,
        // and click-outside-to-close first.
        let chrome_outcome = handle_modal_mouse(&mut self.window, kind, column, row);
        match chrome_outcome {
            ModalWindowOutcome::CloseRequested => {
                return ImportClaudeModalOutcome::Cancelled;
            }
            ModalWindowOutcome::ShortcutActivated(id) => {
                return self.dispatch_shortcut_by_id(id);
            }
            ModalWindowOutcome::Handled => {
                return ImportClaudeModalOutcome::Changed;
            }
            _ => {
                // Mouse handler never returns fold outcomes; fall
                // through to content handling.
            }
        }

        let Some(area) = self.content_area else {
            return ImportClaudeModalOutcome::Unchanged;
        };
        match kind {
            MouseEventKind::Moved => {
                // Move focus to the selectable row under the cursor for
                // hover feedback. Clicks outside the content area, header
                // gaps, or blank spacers don't move focus -- but we still
                // need to honour any hover-state transition recorded above.
                if column < area.x
                    || column >= area.x + area.width
                    || row < area.y
                    || row >= area.y + area.height
                {
                    return ImportClaudeModalOutcome::Unchanged;
                }
                let visible_offset = (row - area.y) as usize;
                let row_index = self.scroll_offset + visible_offset;
                let rows = build_rows(&self.plan, &self.cwd, &self.collapsed);
                let Some(target) = rows.get(row_index) else {
                    return ImportClaudeModalOutcome::Unchanged;
                };
                if matches!(target, Row::Blank) {
                    return ImportClaudeModalOutcome::Unchanged;
                }
                // Check if hovering over the fold indicator on a header.
                let on_fold = match target {
                    Row::ScopeHeader { .. } => column >= area.x && column < area.x + 2,
                    Row::TypeHeader { .. } => column >= area.x + 2 && column < area.x + 4,
                    _ => false,
                };
                let focus_changed = self.focus != row_index;
                let fold_changed = self.fold_indicator_hovered != on_fold;
                self.focus = row_index;
                self.fold_indicator_hovered = on_fold;
                if focus_changed || fold_changed {
                    ImportClaudeModalOutcome::Changed
                } else {
                    ImportClaudeModalOutcome::Unchanged
                }
            }
            MouseEventKind::ScrollUp => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            MouseEventKind::ScrollDown => {
                let rows = build_rows(&self.plan, &self.cwd, &self.collapsed);
                let visible = area.height as usize;
                let max_offset = rows.len().saturating_sub(visible);
                if self.scroll_offset < max_offset {
                    self.scroll_offset += 1;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Bail if the click is outside the content rect.
                if column < area.x
                    || column >= area.x + area.width
                    || row < area.y
                    || row >= area.y + area.height
                {
                    return ImportClaudeModalOutcome::Unchanged;
                }
                // Map row coordinate to a row index via the visible window.
                let visible_offset = (row - area.y) as usize;
                let row_index = self.scroll_offset + visible_offset;
                let rows = build_rows(&self.plan, &self.cwd, &self.collapsed);
                let Some(target) = rows.get(row_index) else {
                    return ImportClaudeModalOutcome::Unchanged;
                };
                self.focus = row_index;

                // Check if the click landed on the fold indicator (▶/▼).
                // ScopeHeaders have indent=0, TypeHeaders indent=2. The
                // indicator is 2 chars wide starting at content_area.x + indent.
                let fold_clicked = match target {
                    Row::ScopeHeader { section_key, .. } => {
                        let indicator_start = area.x;
                        (column >= indicator_start && column < indicator_start + 2)
                            .then(|| section_key.clone())
                    }
                    Row::TypeHeader { section_key, .. } => {
                        let indicator_start = area.x + 2; // indent=2
                        (column >= indicator_start && column < indicator_start + 2)
                            .then(|| section_key.clone())
                    }
                    _ => None,
                };
                if let Some(key) = fold_clicked {
                    if !self.collapsed.remove(&key) {
                        self.collapsed.insert(key);
                    }
                    let new_rows = build_rows(&self.plan, &self.cwd, &self.collapsed);
                    if self.focus >= new_rows.len() {
                        self.focus = new_rows.len().saturating_sub(1);
                    }
                    return ImportClaudeModalOutcome::Changed;
                }

                // Click elsewhere on a header: toggle selection.
                let group_indices: Option<Vec<usize>> = match target {
                    Row::ScopeHeader { flat_indices, .. }
                    | Row::TypeHeader { flat_indices, .. } => Some(flat_indices.clone()),
                    _ => None,
                };
                if let Some(indices) = group_indices {
                    let any_unselected = indices
                        .iter()
                        .any(|i| !self.selected.get(*i).copied().unwrap_or(false));
                    let new_state = any_unselected;
                    for i in indices {
                        if let Some(s) = self.selected.get_mut(i) {
                            *s = new_state;
                        }
                    }
                    return ImportClaudeModalOutcome::Changed;
                }
                if let Row::Item { flat_index, .. } = target
                    && let Some(s) = self.selected.get_mut(*flat_index)
                {
                    *s = !*s;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            _ => ImportClaudeModalOutcome::Unchanged,
        }
    }

    /// Apply the action associated with a clicked footer shortcut (by ID).
    fn dispatch_shortcut_by_id(&mut self, id: usize) -> ImportClaudeModalOutcome {
        match id {
            SHORTCUT_ID_SELECT_ALL => {
                self.selected.iter_mut().for_each(|s| *s = true);
                ImportClaudeModalOutcome::Changed
            }
            SHORTCUT_ID_SELECT_NONE => {
                self.selected.iter_mut().for_each(|s| *s = false);
                ImportClaudeModalOutcome::Changed
            }
            SHORTCUT_ID_CONFIRM => ImportClaudeModalOutcome::Confirmed,
            SHORTCUT_ID_CANCEL => ImportClaudeModalOutcome::Cancelled,
            _ => ImportClaudeModalOutcome::Unchanged,
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> ImportClaudeModalOutcome {
        // Build fold state from the focused row so handle_modal_key can
        // decide collapse/expand/jump-to-parent generically.
        let rows = build_rows(&self.plan, &self.cwd, &self.collapsed);
        let fold_info = rows.get(self.focus).and_then(|row| match row {
            Row::ScopeHeader { section_key, .. } => Some(FoldInfo {
                collapsible: true,
                expanded: !self.collapsed.contains(section_key.as_str()),
                has_details: false,
                details_expanded: false,
                parent_index: None,
            }),
            Row::TypeHeader { section_key, .. } => {
                let parent = rows[..self.focus]
                    .iter()
                    .rposition(|r| matches!(r, Row::ScopeHeader { .. }));
                Some(FoldInfo {
                    collapsible: true,
                    expanded: !self.collapsed.contains(section_key.as_str()),
                    has_details: false,
                    details_expanded: false,
                    parent_index: parent,
                })
            }
            Row::Item { .. } => {
                let parent = rows[..self.focus]
                    .iter()
                    .rposition(|r| matches!(r, Row::ScopeHeader { .. } | Row::TypeHeader { .. }));
                Some(FoldInfo {
                    collapsible: false,
                    expanded: false,
                    has_details: false,
                    details_expanded: false,
                    parent_index: parent,
                })
            }
            Row::Blank => None,
        });
        let config = ModalWindowConfig {
            title: "",
            tabs: None,
            shortcuts: &[],
            sizing: Default::default(),
            fold_info,
        };
        match handle_modal_key(&mut self.window, key, &config) {
            ModalWindowOutcome::CloseRequested => return ImportClaudeModalOutcome::Cancelled,
            ModalWindowOutcome::Handled => return ImportClaudeModalOutcome::Changed,
            ModalWindowOutcome::CollapseGroup => {
                if let Some(Row::ScopeHeader { section_key, .. })
                | Some(Row::TypeHeader { section_key, .. }) = rows.get(self.focus)
                {
                    self.collapsed.insert(section_key.clone());
                    let new_rows = build_rows(&self.plan, &self.cwd, &self.collapsed);
                    if self.focus >= new_rows.len() {
                        self.focus = new_rows.len().saturating_sub(1);
                    }
                }
                return ImportClaudeModalOutcome::Changed;
            }
            ModalWindowOutcome::ExpandGroup => {
                if let Some(Row::ScopeHeader { section_key, .. })
                | Some(Row::TypeHeader { section_key, .. }) = rows.get(self.focus)
                {
                    self.collapsed.remove(section_key);
                }
                return ImportClaudeModalOutcome::Changed;
            }
            ModalWindowOutcome::JumpToParent(idx) => {
                self.focus = idx;
                return ImportClaudeModalOutcome::Changed;
            }
            _ => {}
        }

        if (key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
            && !crate::input::key::is_altgr(key.modifiers)
        {
            return ImportClaudeModalOutcome::Unchanged;
        }
        match key.code {
            KeyCode::Enter => ImportClaudeModalOutcome::Confirmed,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(prev) = prev_selectable_row(&rows, self.focus) {
                    self.focus = prev;
                    ImportClaudeModalOutcome::Changed
                } else {
                    ImportClaudeModalOutcome::Unchanged
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(next) = next_selectable_row(&rows, self.focus) {
                    self.focus = next;
                    ImportClaudeModalOutcome::Changed
                } else {
                    ImportClaudeModalOutcome::Unchanged
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                let first = rows.iter().position(is_selectable);
                if let Some(idx) = first
                    && idx != self.focus
                {
                    self.focus = idx;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            KeyCode::End | KeyCode::Char('G') => {
                let last = rows.iter().rposition(is_selectable);
                if let Some(idx) = last
                    && idx != self.focus
                {
                    self.focus = idx;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            KeyCode::PageUp => {
                let step = self
                    .content_area
                    .map(|a| a.height.saturating_sub(1) as usize)
                    .unwrap_or(10)
                    .max(1);
                let mut target = self.focus;
                for _ in 0..step {
                    if let Some(prev) = prev_selectable_row(&rows, target) {
                        target = prev;
                    } else {
                        break;
                    }
                }
                if target != self.focus {
                    self.focus = target;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            KeyCode::PageDown => {
                let step = self
                    .content_area
                    .map(|a| a.height.saturating_sub(1) as usize)
                    .unwrap_or(10)
                    .max(1);
                let mut target = self.focus;
                for _ in 0..step {
                    if let Some(next) = next_selectable_row(&rows, target) {
                        target = next;
                    } else {
                        break;
                    }
                }
                if target != self.focus {
                    self.focus = target;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            KeyCode::Char(' ') => {
                let row = rows.get(self.focus);
                let group_indices: Option<Vec<usize>> = match row {
                    Some(Row::ScopeHeader { flat_indices, .. })
                    | Some(Row::TypeHeader { flat_indices, .. }) => Some(flat_indices.clone()),
                    _ => None,
                };
                if let Some(indices) = group_indices {
                    // Tri-state toggle for groups: if any item is unselected, select all;
                    // otherwise (all selected) deselect all.
                    let any_unselected = indices
                        .iter()
                        .any(|i| !self.selected.get(*i).copied().unwrap_or(false));
                    let new_state = any_unselected;
                    for i in indices {
                        if let Some(s) = self.selected.get_mut(i) {
                            *s = new_state;
                        }
                    }
                    return ImportClaudeModalOutcome::Changed;
                }
                if let Some(Row::Item { flat_index, .. }) = row
                    && let Some(s) = self.selected.get_mut(*flat_index)
                {
                    *s = !*s;
                    return ImportClaudeModalOutcome::Changed;
                }
                ImportClaudeModalOutcome::Unchanged
            }
            KeyCode::Char('a') => {
                self.selected.iter_mut().for_each(|s| *s = true);
                ImportClaudeModalOutcome::Changed
            }
            KeyCode::Char('n') => {
                self.selected.iter_mut().for_each(|s| *s = false);
                ImportClaudeModalOutcome::Changed
            }
            _ => ImportClaudeModalOutcome::Unchanged,
        }
    }
}

/// Render the modal centered in `area` using the shared `ModalWindow`
/// chrome. Content (the checkbox tree) is drawn into the content area
/// returned by [`render_modal_window`].
pub fn render_import_claude_modal(
    buf: &mut Buffer,
    area: Rect,
    state: &mut ImportClaudeModalState,
    theme: &Theme,
    compact: bool,
) {
    let confirm_label = format!("Enter import {}", state.selected_count());
    let shortcuts = [
        Shortcut {
            label: "\u{2191}\u{2193} navigate",
            clickable: false,
            id: SHORTCUT_ID_HINT,
        },
        Shortcut {
            label: "space toggle",
            clickable: false,
            id: SHORTCUT_ID_HINT,
        },
        Shortcut {
            label: "\u{2190}\u{2192} fold",
            clickable: false,
            id: SHORTCUT_ID_HINT,
        },
        Shortcut {
            label: "a all",
            clickable: true,
            id: SHORTCUT_ID_SELECT_ALL,
        },
        Shortcut {
            label: "n none",
            clickable: true,
            id: SHORTCUT_ID_SELECT_NONE,
        },
        Shortcut {
            label: &confirm_label,
            clickable: true,
            id: SHORTCUT_ID_CONFIRM,
        },
        Shortcut {
            label: "Esc cancel",
            clickable: true,
            id: SHORTCUT_ID_CANCEL,
        },
    ];
    let config = ModalWindowConfig {
        title: "Import Claude settings",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing::default().with_compact(compact),
        fold_info: None,
    };

    let Some(areas) = render_modal_window(buf, area, &mut state.window, &config, theme) else {
        return;
    };

    let content_area = areas.content;
    // Stash the rendered content rect so the mouse handler can map clicks
    // back to row indices without re-deriving the layout.
    state.content_area = Some(content_area);

    // Build rows + ensure focus is in viewport.
    let rows = build_rows(&state.plan, &state.cwd, &state.collapsed);
    if state.focus >= rows.len() {
        state.focus = rows.len().saturating_sub(1);
    }
    let visible = content_area.height as usize;
    if state.focus < state.scroll_offset {
        state.scroll_offset = state.focus;
    } else if state.focus >= state.scroll_offset + visible {
        state.scroll_offset = state.focus + 1 - visible;
    }

    // Render visible rows one at a time so each row can pre-fill its full
    // width with `bg_highlight` when focused. A single Paragraph render
    // would only colour the text spans; the welcome menu uses the same
    // per-row fill pattern and we mirror it here for visual consistency.
    for (row_idx, (i, row)) in rows
        .iter()
        .enumerate()
        .skip(state.scroll_offset)
        .take(visible)
        .enumerate()
    {
        let y = content_area.y + row_idx as u16;
        let is_focused = i == state.focus;
        let is_blank = matches!(row, Row::Blank);
        if is_focused && !is_blank {
            let hover_bg = Style::default().bg(theme.bg_highlight);
            for x in content_area.x..content_area.x + content_area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(hover_bg);
                }
            }
        }
        let line = render_row_dispatch(
            row,
            is_focused,
            &state.plan,
            &state.selected,
            theme,
            &state.collapsed,
            is_focused && state.fold_indicator_hovered,
        );
        let row_rect = Rect {
            x: content_area.x,
            y,
            width: content_area.width,
            height: 1,
        };
        Paragraph::new(line).render(row_rect, buf);
    }
}

/// Build the flat row list for navigation and rendering.
fn build_rows(
    plan: &ImportPlan,
    cwd: &std::path::Path,
    collapsed: &std::collections::HashSet<String>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut flat_index = 0;

    if !plan.global_items.is_empty() {
        let scope_start = flat_index;
        let scope_key = format!("scope:{:?}", Scope::Global);
        let label = "Global  ~/.echo-agent/config.toml".to_string();
        // Placeholder header; flat_indices filled after children are pushed.
        let scope_header_pos = rows.len();
        rows.push(Row::ScopeHeader {
            label,
            flat_indices: Vec::new(),
            section_key: scope_key.clone(),
        });
        let scope_collapsed = collapsed.contains(&scope_key);
        push_grouped_items(
            &mut rows,
            &mut flat_index,
            Scope::Global,
            &plan.global_items,
            collapsed,
            scope_collapsed,
        );
        // Backfill scope flat_indices.
        if let Row::ScopeHeader { flat_indices, .. } = &mut rows[scope_header_pos] {
            *flat_indices = (scope_start..flat_index).collect();
        }
        rows.push(Row::Blank);
    }

    if !plan.project_items.is_empty() {
        let scope_start = flat_index;
        let scope_key = format!("scope:{:?}", Scope::Project);
        let project_config = find_project_root(cwd)
            .join(".echo-agent")
            .join("config.toml");
        let label = format!("Project  {}", project_config.display());
        let scope_header_pos = rows.len();
        rows.push(Row::ScopeHeader {
            label,
            flat_indices: Vec::new(),
            section_key: scope_key.clone(),
        });
        let scope_collapsed = collapsed.contains(&scope_key);
        push_grouped_items(
            &mut rows,
            &mut flat_index,
            Scope::Project,
            &plan.project_items,
            collapsed,
            scope_collapsed,
        );
        if let Row::ScopeHeader { flat_indices, .. } = &mut rows[scope_header_pos] {
            *flat_indices = (scope_start..flat_index).collect();
        }
    }

    rows
}

/// Push a TypeHeader followed by Item rows for each ItemKind present in `items`.
/// Items keep their original order within each subgroup. The header tracks the
/// flat_indices of items in its subgroup for bulk-toggle.
fn push_grouped_items(
    rows: &mut Vec<Row>,
    flat_index: &mut usize,
    scope: Scope,
    items: &[ImportableItem],
    collapsed: &std::collections::HashSet<String>,
    scope_collapsed: bool,
) {
    // The `selected` Vec is indexed by ORIGINAL item position (so
    // `filtered_plan` can iterate items in their source-vec order and check
    // `selected[scope_offset + i]`). When we sort items into display groups
    // here we must therefore use `scope_offset + item_idx` as the flat_index,
    // not a monotonic counter — otherwise toggling a displayed row would mark
    // the wrong slot and unrelated items would import (or fail to import).
    let scope_offset = *flat_index;
    let mut indexed: Vec<(usize, ItemKind)> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (i, ItemKind::of(it)))
        .collect();
    indexed.sort_by_key(|(_, k)| k.order());

    let mut current_kind: Option<ItemKind> = None;
    let mut header_pos: Option<usize> = None;
    let mut group_indices: Vec<usize> = Vec::new();

    for (item_idx, kind) in indexed {
        if Some(kind) != current_kind {
            // Close out previous group's flat_indices.
            if let Some(pos) = header_pos
                && let Row::TypeHeader { flat_indices, .. } = &mut rows[pos]
            {
                *flat_indices = std::mem::take(&mut group_indices);
            }
            // Open new group.
            let type_key = format!("type:{scope:?}:{kind:?}");
            if !scope_collapsed {
                header_pos = Some(rows.len());
                rows.push(Row::TypeHeader {
                    kind,
                    flat_indices: Vec::new(),
                    section_key: type_key.clone(),
                });
            } else {
                header_pos = None;
            }
            current_kind = Some(kind);
        }
        let item_flat = scope_offset + item_idx;
        // Skip item rows when the scope is collapsed, or when the
        // type-group is collapsed.
        let type_key = format!("type:{:?}:{:?}", scope, current_kind.unwrap_or(kind));
        let type_collapsed = collapsed.contains(&type_key);
        if !scope_collapsed && !type_collapsed {
            rows.push(Row::Item {
                scope,
                item_index: item_idx,
                flat_index: item_flat,
            });
        }
        group_indices.push(item_flat);
    }

    // Close out final group.
    if let Some(pos) = header_pos
        && let Row::TypeHeader { flat_indices, .. } = &mut rows[pos]
    {
        *flat_indices = group_indices;
    }

    *flat_index = scope_offset + items.len();
}

/// Dispatch a row to the appropriate renderer.
fn render_row_dispatch<'a>(
    row: &Row,
    focused: bool,
    plan: &'a ImportPlan,
    selected: &[bool],
    theme: &Theme,
    collapsed_set: &std::collections::HashSet<String>,
    fold_hovered: bool,
) -> Line<'a> {
    match row {
        Row::ScopeHeader {
            label,
            flat_indices,
            section_key,
        } => render_header_line(
            label,
            flat_indices,
            selected,
            focused,
            theme,
            /* indent: */ 0,
            true,
            collapsed_set.contains(section_key),
            focused && fold_hovered,
        ),
        Row::TypeHeader {
            kind,
            flat_indices,
            section_key,
        } => {
            let label = format!("{} ({})", kind.label(), flat_indices.len());
            render_header_line(
                &label,
                flat_indices,
                selected,
                focused,
                theme,
                2,
                false,
                collapsed_set.contains(section_key),
                focused && fold_hovered,
            )
        }
        Row::Blank => Line::from(""),
        Row::Item {
            scope,
            item_index,
            flat_index,
        } => {
            let item = match scope {
                Scope::Global => &plan.global_items[*item_index],
                Scope::Project => &plan.project_items[*item_index],
            };
            let is_selected = selected.get(*flat_index).copied().unwrap_or(false);
            render_item_line(item, is_selected, focused, theme)
        }
    }
}

/// Render a group header (scope or type) with tri-state checkbox.
///
/// `bold_label` makes the label BOLD (for top-level scope headers); type
/// subgroup headers use a lighter style to give visual hierarchy.
#[allow(clippy::too_many_arguments)]
fn render_header_line<'a>(
    label: &str,
    flat_indices: &[usize],
    selected: &[bool],
    focused: bool,
    theme: &Theme,
    indent: usize,
    bold_label: bool,
    collapsed: bool,
    fold_hovered: bool,
) -> Line<'a> {
    let checked = flat_indices
        .iter()
        .filter(|i| selected.get(**i).copied().unwrap_or(false))
        .count();
    let total = flat_indices.len();
    // Brackets stay gray; only the inner mark is colored. This keeps the
    // visual weight of an unchecked group identical to a checked one and
    // lets the colored mark act as the actual signal.
    let (mark, mark_style) = if checked == 0 {
        (" ", Style::default().fg(theme.gray_dim))
    } else if checked == total {
        (
            crate::glyphs::check_mark(),
            Style::default().fg(theme.accent_success),
        )
    } else {
        ("~", Style::default().fg(theme.accent_running))
    };
    let bracket_style = with_bg(Style::default().fg(theme.gray_dim), focused, theme);
    let mark_style = with_bg(mark_style, focused, theme);
    let label_base = if bold_label {
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_secondary)
    };
    let label_style = with_bg(label_base, focused, theme);
    let pad = " ".repeat(indent);
    let fold_bg = if focused {
        Some(theme.bg_highlight)
    } else {
        None
    };
    let fold_span =
        crate::views::modal_window::fold_indicator_span(collapsed, fold_hovered, fold_bg, theme);
    Line::from(vec![
        Span::raw(pad),
        fold_span,
        Span::styled("[", bracket_style),
        Span::styled(mark, mark_style),
        Span::styled("]", bracket_style),
        Span::raw(" "),
        Span::styled(label.to_string(), label_style),
    ])
}

/// Render a single item row with checkbox + label + focus highlight.
fn render_item_line<'a>(
    item: &'a ImportableItem,
    selected: bool,
    focused: bool,
    theme: &Theme,
) -> Line<'a> {
    // Brackets stay gray; the mark itself is the only colored cell.
    let (mark, mark_base) = if selected {
        (
            crate::glyphs::check_mark(),
            Style::default().fg(theme.accent_success),
        )
    } else {
        (" ", Style::default().fg(theme.gray_dim))
    };
    let bracket_style = with_bg(Style::default().fg(theme.gray_dim), focused, theme);
    let mark_style = with_bg(mark_base, focused, theme);
    let label = format_item_label(item);
    let label_style = with_bg(Style::default().fg(theme.text_primary), focused, theme);
    // Items live under TypeHeaders (indent 2) under ScopeHeaders (indent 0).
    // Indent items at 4 spaces total so they visually nest below their group.
    Line::from(vec![
        Span::raw("      "),
        Span::styled("[", bracket_style),
        Span::styled(mark, mark_style),
        Span::styled("]", bracket_style),
        Span::raw(" "),
        Span::styled(label, label_style),
    ])
}

/// Conditionally apply the row-hover background to a span style.
///
/// When a row is focused the renderer pre-fills the row's cells with
/// `bg_highlight`. We also set bg explicitly on each span so the highlight
/// survives even if a span resets its background, and so that hovering
/// reads as a continuous bar across the full row width.
fn with_bg(style: Style, focused: bool, theme: &Theme) -> Style {
    if focused {
        style.bg(theme.bg_highlight)
    } else {
        style
    }
}

fn format_item_label(item: &ImportableItem) -> String {
    match item {
        ImportableItem::Permission(rule) => {
            let action = match rule.action {
                RuleAction::Allow => "allow",
                RuleAction::Deny => "deny",
                RuleAction::Ask => "ask",
            };
            let pattern = rule.pattern.as_deref().unwrap_or("*");
            let tool = format!("{:?}", rule.tool);
            format!("{action:5} {tool}({pattern})")
        }
        ImportableItem::EnvVar { key, value } => {
            format!("{key} = {value:?}")
        }
        ImportableItem::McpServer { name, .. } => name.to_string(),
        ImportableItem::Hook {
            event,
            matcher,
            command,
            timeout,
        } => {
            let m = matcher.as_deref().unwrap_or("*");
            let t = timeout
                .map(|t| format!(" timeout={}s", t))
                .unwrap_or_default();
            format!("{event}  matcher={m} → {command}{t}")
        }
        ImportableItem::PathEntry { kind, path } => {
            let kind_str = match kind {
                PathKind::Skill => "skill dir",
                PathKind::Rule => "rule dir",
            };
            format!("{kind_str}: {path}")
        }
    }
}

fn next_selectable_row(rows: &[Row], from: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .skip(from + 1)
        .find(|(_, r)| is_selectable(r))
        .map(|(i, _)| i)
}

fn prev_selectable_row(rows: &[Row], from: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .take(from)
        .rev()
        .find(|(_, r)| is_selectable(r))
        .map(|(i, _)| i)
}

/// Selectable = focusable for navigation. Includes headers (which can be
/// bulk-toggled) and items. Excludes Blank spacers.
fn is_selectable(row: &Row) -> bool {
    matches!(
        row,
        Row::ScopeHeader { .. } | Row::TypeHeader { .. } | Row::Item { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_runtime::claude_import::PathKind;
    use echo_agent_workspace::permission::types::{PatternMode, PermissionRule, ToolFilter};

    fn sample_plan() -> ImportPlan {
        ImportPlan {
            global_items: vec![
                ImportableItem::Permission(PermissionRule {
                    action: RuleAction::Allow,
                    tool: ToolFilter::Bash,
                    pattern: Some("cargo test *".into()),
                    pattern_mode: PatternMode::Glob,
                }),
                ImportableItem::EnvVar {
                    key: "RUST_LOG".into(),
                    value: "debug".into(),
                },
            ],
            project_items: vec![ImportableItem::PathEntry {
                kind: PathKind::Skill,
                path: ".claude/skills".into(),
            }],
        }
    }

    #[test]
    fn defaults_select_all() {
        let m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        assert_eq!(m.total_count(), 3);
        assert_eq!(m.selected_count(), 3);
    }

    #[test]
    fn select_none_then_some() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        let n_key = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.handle_key(&n_key), ImportClaudeModalOutcome::Changed);
        assert_eq!(m.selected_count(), 0);

        // Move focus to a single Item row (skip past headers).
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        let item_idx = rows
            .iter()
            .position(|r| matches!(r, Row::Item { .. }))
            .expect("sample_plan has items");
        m.focus = item_idx;

        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(m.handle_key(&space), ImportClaudeModalOutcome::Changed);
        assert_eq!(m.selected_count(), 1);
    }

    #[test]
    fn filtered_plan_drops_unselected() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        let n_key = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        m.handle_key(&n_key);
        // Select only the first global item.
        m.selected[0] = true;
        let filtered = m.filtered_plan();
        assert_eq!(filtered.global_items.len(), 1);
        assert_eq!(filtered.project_items.len(), 0);
    }

    /// Regression: when items are sorted into display groups (e.g.
    /// Permissions before MCP servers), de-selecting an MCP server in the
    /// modal must actually skip THAT MCP server in `filtered_plan` — not
    /// some other item that happens to share the deselected slot's index.
    #[test]
    fn filtered_plan_respects_per_item_selection_after_grouping() {
        use echo_agent_runtime::claude_import::ImportableItem;
        use echo_agent_runtime::util::config::{McpServerConfig, McpServerTransportConfig};
        use echo_agent_workspace::permission::types::{PatternMode, PermissionRule, ToolFilter};

        // Mix Permissions, MCP servers, and EnvVars in a non-sorted order so
        // the display order (sorted by ItemKind) differs from the source order.
        let mcp = |name: &str| ImportableItem::McpServer {
            name: name.into(),
            config: Box::new(McpServerConfig {
                desktop_instance_id: None,
                transport: McpServerTransportConfig::Stdio {
                    command: "true".into(),
                    args: vec![],
                    env: None,
                    cwd: None,
                },
                enabled: true,
                oauth: None,
                setup: None,
                startup_timeout_sec: None,
                tool_timeout_sec: None,
                tool_timeouts: None,
                expose_image_base64: None,
            }),
        };
        let plan = ImportPlan {
            global_items: vec![
                mcp("alpha"),
                ImportableItem::Permission(PermissionRule {
                    action: RuleAction::Allow,
                    tool: ToolFilter::Bash,
                    pattern: Some("true".into()),
                    pattern_mode: PatternMode::Glob,
                }),
                mcp("beta"),
                ImportableItem::EnvVar {
                    key: "X".into(),
                    value: "y".into(),
                },
                mcp("gamma"),
            ],
            project_items: vec![],
        };
        let mut m = ImportClaudeModalState::new(plan, PathBuf::from("/tmp"));
        // Deselect every MCP server. They are at original indices 0, 2, 4.
        m.selected[0] = false;
        m.selected[2] = false;
        m.selected[4] = false;
        let filtered = m.filtered_plan();
        // Should contain exactly the Permission (idx 1) and EnvVar (idx 3),
        // and none of alpha/beta/gamma.
        assert_eq!(filtered.global_items.len(), 2);
        for item in &filtered.global_items {
            assert!(
                !matches!(item, ImportableItem::McpServer { .. }),
                "de-selected MCP server slipped through: {:?}",
                item
            );
        }
    }

    #[test]
    fn esc_cancels() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(m.handle_key(&esc), ImportClaudeModalOutcome::Cancelled);
    }

    #[test]
    fn enter_confirms() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.handle_key(&enter), ImportClaudeModalOutcome::Confirmed);
    }

    #[test]
    fn navigation_lands_on_selectable_rows() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        let initial = m.focus;
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        m.handle_key(&down);
        assert!(m.focus > initial, "focus should advance");
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        // Headers are now selectable too — just verify it's not a Blank.
        assert!(matches!(
            rows.get(m.focus),
            Some(Row::Item { .. } | Row::ScopeHeader { .. } | Row::TypeHeader { .. })
        ));
    }

    /// Regression: clicking on an item row with the mouse must toggle that
    /// item's selection. After the inline-shortcut refactor, the handler
    /// now consults `state.shortcuts` first; verify a click on a row that
    /// is NOT a shortcut still falls through to the row-toggle path.
    #[test]
    fn mouse_click_on_item_row_toggles() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        // Simulate render: stash content_area so handle_mouse can map clicks.
        m.content_area = Some(ratatui::layout::Rect {
            x: 10,
            y: 5,
            width: 60,
            height: 20,
        });
        // Find the first item row's display offset within the visible window.
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        let item_row_index = rows
            .iter()
            .position(|r| matches!(r, Row::Item { .. }))
            .expect("sample plan has items");
        // With scroll_offset=0 the item appears at content_area.y + item_row_index.
        let click_y = m.content_area.unwrap().y + item_row_index as u16;
        let click_x = m.content_area.unwrap().x + 5; // anywhere in the row
        // Capture initial selection state for that item.
        let flat_idx = match &rows[item_row_index] {
            Row::Item { flat_index, .. } => *flat_index,
            _ => unreachable!(),
        };
        let before = m.selected[flat_idx];
        let outcome = m.handle_mouse(MouseEventKind::Down(MouseButton::Left), click_x, click_y);
        assert_eq!(outcome, ImportClaudeModalOutcome::Changed);
        assert_eq!(
            m.selected[flat_idx], !before,
            "click on item row should toggle its selection"
        );
    }

    /// Regression: clicking on a scope/type header row should bulk-toggle.
    #[test]
    fn mouse_click_on_header_row_bulk_toggles() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        m.content_area = Some(ratatui::layout::Rect {
            x: 10,
            y: 5,
            width: 60,
            height: 20,
        });
        // First row is the Global ScopeHeader (sample_plan has Global items).
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        assert!(matches!(rows.first(), Some(Row::ScopeHeader { .. })));
        let click_y = m.content_area.unwrap().y; // top row
        let click_x = m.content_area.unwrap().x + 5;
        // All items start selected.
        assert!(m.selected.iter().all(|&s| s));
        m.handle_mouse(MouseEventKind::Down(MouseButton::Left), click_x, click_y);
        // Global scope items (indices 0, 1) should now be deselected.
        assert!(!m.selected[0]);
        assert!(!m.selected[1]);
        // Project item (index 2) untouched.
        assert!(m.selected[2]);
    }

    #[test]
    fn space_on_scope_header_toggles_all_in_scope() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        // Focus the first row (should be the Global ScopeHeader).
        m.focus = 0;
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        assert!(matches!(rows.first(), Some(Row::ScopeHeader { .. })));
        // Initially all selected. Space should deselect everything in Global scope.
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        m.handle_key(&space);
        // Global has 2 items (indices 0, 1). Project has 1 (index 2).
        assert!(!m.selected[0]);
        assert!(!m.selected[1]);
        assert!(m.selected[2], "project should be unaffected");
        // Space again: re-selects all in Global.
        m.handle_key(&space);
        assert!(m.selected[0]);
        assert!(m.selected[1]);
    }

    #[test]
    fn left_on_expanded_scope_header_collapses_it() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        // Focus on the first ScopeHeader (Global).
        m.focus = 0;
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        let key = match &rows[0] {
            Row::ScopeHeader { section_key, .. } => section_key.clone(),
            _ => panic!("first row should be ScopeHeader"),
        };
        assert!(m.collapsed.is_empty(), "nothing collapsed initially");

        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        let outcome = m.handle_key(&left);
        assert_eq!(outcome, ImportClaudeModalOutcome::Changed);
        assert!(
            m.collapsed.contains(&key),
            "section_key should now be in collapsed set"
        );
    }

    #[test]
    fn right_on_collapsed_scope_header_expands_it() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        m.focus = 0;
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        let key = match &rows[0] {
            Row::ScopeHeader { section_key, .. } => section_key.clone(),
            _ => panic!("first row should be ScopeHeader"),
        };
        // Collapse it first.
        m.collapsed.insert(key.clone());
        assert!(m.collapsed.contains(&key));

        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        let outcome = m.handle_key(&right);
        assert_eq!(outcome, ImportClaudeModalOutcome::Changed);
        assert!(
            !m.collapsed.contains(&key),
            "scope should be expanded again"
        );
    }

    #[test]
    fn left_on_item_jumps_to_parent_header() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        // Find the first Item row.
        let item_idx = rows
            .iter()
            .position(|r| matches!(r, Row::Item { .. }))
            .expect("sample_plan has items");
        m.focus = item_idx;
        assert!(item_idx > 0, "item should not be the first row");

        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        let outcome = m.handle_key(&left);
        assert_eq!(outcome, ImportClaudeModalOutcome::Changed);
        // Focus should have jumped to the nearest header above the item.
        assert!(m.focus < item_idx);
        assert!(matches!(
            rows[m.focus],
            Row::ScopeHeader { .. } | Row::TypeHeader { .. }
        ));
    }

    #[test]
    fn collapse_from_last_header_clamps_focus() {
        let mut m = ImportClaudeModalState::new(sample_plan(), PathBuf::from("/tmp"));
        let rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        // Find the last TypeHeader -- it's the deepest header near the end.
        let last_type_idx = rows
            .iter()
            .rposition(|r| matches!(r, Row::TypeHeader { .. }))
            .expect("sample_plan has type headers");
        // Verify it has children after it (will shrink on collapse).
        let rows_after = rows.len() - last_type_idx - 1;
        assert!(rows_after > 0, "TypeHeader should have items after it");
        let row_count_before = rows.len();

        m.focus = last_type_idx;
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        let outcome = m.handle_key(&left);
        assert_eq!(outcome, ImportClaudeModalOutcome::Changed);
        // Row count should have shrunk (children hidden).
        let new_rows = build_rows(&m.plan, &m.cwd, &m.collapsed);
        assert!(
            new_rows.len() < row_count_before,
            "collapsing should remove child rows"
        );
        // Focus must be valid (within the new row list). The
        // focus-clamping guard in CollapseGroup ensures this.
        assert!(
            m.focus < new_rows.len(),
            "focus should be within new row bounds"
        );
        // The focused row should still be the TypeHeader (it survives).
        assert!(matches!(new_rows[m.focus], Row::TypeHeader { .. }));
    }

    #[test]
    fn collapse_hides_children_from_rows() {
        let plan = sample_plan();
        let mut collapsed = std::collections::HashSet::new();
        let full_rows = build_rows(&plan, &PathBuf::from("/tmp"), &collapsed);
        let full_count = full_rows.len();

        // Find the first ScopeHeader's key
        let key = match &full_rows[0] {
            Row::ScopeHeader { section_key, .. } => section_key.clone(),
            _ => panic!("first row should be ScopeHeader"),
        };
        collapsed.insert(key);
        let collapsed_rows = build_rows(&plan, &PathBuf::from("/tmp"), &collapsed);
        assert!(
            collapsed_rows.len() < full_count,
            "collapsed should have fewer rows"
        );
        // The ScopeHeader itself should still be present
        assert!(matches!(collapsed_rows[0], Row::ScopeHeader { .. }));
    }
}
