//! Persona detail/edit modal — structured view of a persona with inline editing.
//!
//! Opened by pressing Enter on a persona in the `/config-agents` Personas tab.
//! Renders all persona TOML fields in labeled sections. Editable personas
//! (user/project scope) support inline field editing; bundled personas are
//! read-only.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::theme::Theme;
use crate::views::modal_window::{
    self, ModalContentArea, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};

// ---------------------------------------------------------------------------
// Field enum
// ---------------------------------------------------------------------------

/// Navigable fields in the persona detail view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaField {
    Name,
    Description,
    Model,
    ReasoningEffort,
    Isolation,
    Instructions,
    InstructionsFile,
}

impl PersonaField {
    const ALL: &[PersonaField] = &[
        PersonaField::Name,
        PersonaField::Description,
        PersonaField::Model,
        PersonaField::ReasoningEffort,
        PersonaField::Isolation,
        PersonaField::Instructions,
        PersonaField::InstructionsFile,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Description => "Description",
            Self::Model => "Model",
            Self::ReasoningEffort => "Effort",
            Self::Isolation => "Isolation",
            Self::Instructions => "Instructions",
            Self::InstructionsFile => "Instr. file",
        }
    }

    fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|&f| f == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|&f| f == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// True for fields that support inline text editing.
    fn is_editable(self) -> bool {
        matches!(
            self,
            Self::Name | Self::Description | Self::Model | Self::ReasoningEffort | Self::Isolation
        )
    }
}

// ---------------------------------------------------------------------------
// Mode state machine
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum PersonaDetailMode {
    Browse,
    Editing {
        field: PersonaField,
        editor: LineEditor,
        original: String,
    },
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PersonaDetailOutcome {
    /// Normal handled event.
    Changed,
    /// Nothing to do.
    Unchanged,
    /// Close the detail modal, return to the list.
    Close,
    /// Open the file in $EDITOR.
    EditInEditor { path: PathBuf },
}

// ---------------------------------------------------------------------------
// I/O entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PersonaIOEntry {
    pub name: String,
    pub io_type: String,
    pub required: bool,
    pub description: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct PersonaDetailState {
    pub window: ModalWindowState,
    pub name: String,
    pub description: String,
    pub model: String,
    pub reasoning_effort: String,
    pub default_isolation: String,
    pub instructions: String,
    pub instructions_file: String,
    pub inputs: Vec<PersonaIOEntry>,
    pub outputs: Vec<PersonaIOEntry>,
    pub source_path: Option<PathBuf>,
    pub editable: bool,
    pub scope_label: String,
    pub selected_field: PersonaField,
    pub scroll_offset: usize,
    mode: PersonaDetailMode,
    pub dirty: bool,
    pub instructions_expanded: bool,
    /// Scroll offset within expanded instructions (line index of first visible line).
    pub instructions_scroll: usize,
    pub message: Option<String>,
}

impl PersonaDetailState {
    /// Load persona state from a TOML file on disk.
    pub fn from_toml_file(path: &Path, editable: bool, scope_label: &str) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let table: toml::Value = toml::from_str(&content).ok()?;

        let get_str = |key: &str| -> String {
            table
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned()
        };

        let parse_io = |key: &str| -> Vec<PersonaIOEntry> {
            table
                .get(key)
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|item| PersonaIOEntry {
                            name: item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_owned(),
                            io_type: item
                                .get("io_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("file")
                                .to_owned(),
                            required: item
                                .get("required")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            description: item
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        let name_from_file = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();

        Some(Self {
            window: ModalWindowState::new(),
            name: {
                let n = get_str("name");
                if n.is_empty() { name_from_file } else { n }
            },
            description: get_str("description"),
            model: get_str("model"),
            reasoning_effort: get_str("reasoning_effort"),
            default_isolation: get_str("default_isolation"),
            instructions: get_str("instructions"),
            instructions_file: get_str("instructions_file"),
            inputs: parse_io("inputs"),
            outputs: parse_io("outputs"),
            source_path: Some(path.to_path_buf()),
            editable,
            scope_label: scope_label.to_owned(),
            selected_field: PersonaField::Name,
            scroll_offset: 0,
            mode: PersonaDetailMode::Browse,
            dirty: false,
            instructions_expanded: false,
            instructions_scroll: 0,
            message: None,
        })
    }

    /// Create a minimal detail state for personas with no file on disk.
    pub fn from_name_only(name: &str) -> Self {
        Self {
            window: ModalWindowState::new(),
            name: name.to_owned(),
            description: String::new(),
            model: String::new(),
            reasoning_effort: String::new(),
            default_isolation: String::new(),
            instructions: String::new(),
            instructions_file: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            source_path: None,
            editable: false,
            scope_label: "bundled".to_owned(),
            selected_field: PersonaField::Name,
            scroll_offset: 0,
            mode: PersonaDetailMode::Browse,
            dirty: false,
            instructions_expanded: false,
            instructions_scroll: 0,
            message: None,
        }
    }

    fn field_value(&self, field: PersonaField) -> &str {
        match field {
            PersonaField::Name => &self.name,
            PersonaField::Description => &self.description,
            PersonaField::Model => &self.model,
            PersonaField::ReasoningEffort => &self.reasoning_effort,
            PersonaField::Isolation => &self.default_isolation,
            PersonaField::Instructions => &self.instructions,
            PersonaField::InstructionsFile => &self.instructions_file,
        }
    }

    fn set_field_value(&mut self, field: PersonaField, value: String) {
        match field {
            PersonaField::Name => self.name = value,
            PersonaField::Description => self.description = value,
            PersonaField::Model => self.model = value,
            PersonaField::ReasoningEffort => self.reasoning_effort = value,
            PersonaField::Isolation => self.default_isolation = value,
            PersonaField::Instructions => self.instructions = value,
            PersonaField::InstructionsFile => self.instructions_file = value,
        }
    }

    pub fn is_editing(&self) -> bool {
        matches!(&self.mode, PersonaDetailMode::Editing { .. })
    }

    #[cfg(test)]
    fn editing_editor(&self) -> Option<&LineEditor> {
        match &self.mode {
            PersonaDetailMode::Editing { editor, .. } => Some(editor),
            PersonaDetailMode::Browse => None,
        }
    }

    #[cfg(test)]
    fn editing_viewport(
        &self,
        width: usize,
    ) -> Option<echo_agent_ratatui_textarea::SingleLineViewport> {
        self.editing_editor().map(|editor| editor.viewport(width))
    }

    #[cfg(test)]
    fn editing_text(&self) -> Option<&str> {
        self.editing_editor().map(LineEditor::text)
    }

    #[cfg(test)]
    fn set_editing_text(&mut self, text: impl Into<String>) {
        if let PersonaDetailMode::Editing { editor, .. } = &mut self.mode {
            editor.set_text(text);
        }
    }

    #[cfg(test)]
    fn set_editing_cursor_byte(&mut self, cursor_byte: usize) -> LineEditOutcome {
        match &mut self.mode {
            PersonaDetailMode::Editing { editor, .. } => editor.set_cursor_byte(cursor_byte),
            PersonaDetailMode::Browse => LineEditOutcome::Unhandled,
        }
    }

    /// Save current state back to the TOML file using toml_edit to preserve formatting.
    fn save_to_file(&self) -> Result<(), String> {
        let Some(ref path) = self.source_path else {
            return Err("No source file to save to".to_string());
        };
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {e}"))?;
        let mut doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|e| format!("Failed to parse TOML: {e}"))?;

        // Update simple string fields.
        let fields: &[(&str, &str)] = &[
            ("name", &self.name),
            ("description", &self.description),
            ("instructions", &self.instructions),
            ("instructions_file", &self.instructions_file),
            ("model", &self.model),
            ("reasoning_effort", &self.reasoning_effort),
            ("default_isolation", &self.default_isolation),
        ];
        for &(key, value) in fields {
            if value.is_empty() {
                doc.remove(key);
            } else {
                doc[key] = toml_edit::value(value);
            }
        }

        std::fs::write(path, doc.to_string()).map_err(|e| format!("Failed to write file: {e}"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_detail_editor(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: usize,
    editor: &LineEditor,
    style: Style,
    theme: &Theme,
) {
    let viewport = editor.viewport(width);
    let visible = &editor.text()[viewport.visible_byte_range];
    buf.set_string(x, y, visible, style);
    if width > 0 {
        let cursor_x = x + viewport.cursor_display_column as u16;
        if let Some(cell) = buf.cell_mut((cursor_x, y)) {
            cell.set_style(Style::default().fg(theme.bg_base).bg(theme.text_primary));
        }
    }
}

/// Render the persona detail modal.
pub fn render_persona_detail(
    buf: &mut Buffer,
    area: Rect,
    state: &mut PersonaDetailState,
    theme: &Theme,
    compact: bool,
) {
    let title = format!("persona: {}", state.name);
    let shortcuts = build_shortcuts(state);
    let config = ModalWindowConfig {
        title: &title,
        tabs: None,
        shortcuts: &shortcuts,
        sizing: persona_detail_sizing(compact),
        fold_info: None,
    };
    let Some(ModalContentArea {
        content: content_area,
        ..
    }) = modal_window::render_modal_window(buf, area, &mut state.window, &config, theme)
    else {
        return;
    };

    let w = content_area.width as usize;
    let mut y = content_area.y;
    let max_y = content_area.y + content_area.height;
    let label_w = 14u16; // column width for field labels

    // Message line
    if let Some(ref msg) = state.message
        && y < max_y
    {
        buf.set_string(
            content_area.x,
            y,
            msg,
            Style::default().fg(theme.accent_error),
        );
        y += 2;
    }

    // Render each field row.
    for &field in PersonaField::ALL {
        if y >= max_y {
            break;
        }

        let is_selected = state.selected_field == field;
        let label = field.label();
        let value = state.field_value(field);

        // Background highlight for selected row.
        let row_bg = if is_selected {
            Some(theme.bg_highlight)
        } else {
            None
        };

        // Label
        let label_style = if is_selected {
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        if let Some(bg) = row_bg {
            // Fill the row background.
            let blank: String = " ".repeat(w);
            buf.set_string(content_area.x, y, &blank, Style::default().bg(bg));
        }
        buf.set_string(content_area.x, y, label, label_style);

        let value_x = content_area.x + label_w;
        let value_w = w.saturating_sub(label_w as usize);

        // Check if we're in editing mode for this field.
        if is_selected
            && let PersonaDetailMode::Editing {
                field: editing_field,
                editor,
                ..
            } = &state.mode
            && *editing_field == field
        {
            let field_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.text_primary).bg(bg)
            } else {
                Style::default().fg(theme.text_primary)
            };
            render_detail_editor(buf, value_x, y, value_w, editor, field_style, theme);
        } else if field == PersonaField::Instructions {
            // Multi-line instructions with expand/collapse and scroll.
            if value.is_empty() {
                let empty_style = if let Some(bg) = row_bg {
                    Style::default().fg(theme.gray_dim).bg(bg)
                } else {
                    Style::default().fg(theme.gray_dim)
                };
                buf.set_string(value_x, y, "(empty)", empty_style);
            } else {
                let lines = word_wrap_lines(value, value_w);
                let total = lines.len();
                let max_collapsed = 8usize;
                let is_long = total > max_collapsed;

                // Reserve 1 line for the hint at the bottom.
                let avail_lines = (max_y.saturating_sub(y)) as usize;
                let viewport_h = if is_long {
                    avail_lines.saturating_sub(1) // room for hint
                } else {
                    avail_lines
                };

                let val_style = if let Some(bg) = row_bg {
                    Style::default().fg(theme.text_secondary).bg(bg)
                } else {
                    Style::default().fg(theme.text_secondary)
                };

                if !state.instructions_expanded {
                    // Collapsed: show first max_collapsed lines (no scroll).
                    let show = total.min(max_collapsed).min(viewport_h);
                    for (i, line) in lines.iter().enumerate().take(show) {
                        let x_pos = if i == 0 { value_x } else { content_area.x + 2 };
                        buf.set_string(x_pos, y + i as u16, line, val_style);
                    }
                    y += show.saturating_sub(1) as u16;
                    if is_long {
                        y += 1;
                        if y < max_y {
                            let hint = format!(
                                "  ... ({} more lines: e to expand, j/k to scroll)",
                                total - max_collapsed
                            );
                            buf.set_string(
                                content_area.x + 2,
                                y,
                                hint,
                                Style::default().fg(theme.gray_dim),
                            );
                        }
                    }
                } else {
                    // Expanded: viewport with scroll offset.
                    let scroll = state
                        .instructions_scroll
                        .min(total.saturating_sub(viewport_h));
                    state.instructions_scroll = scroll;
                    let visible = &lines[scroll..total.min(scroll + viewport_h)];
                    for (i, line) in visible.iter().enumerate() {
                        let x_pos = if i == 0 && scroll == 0 {
                            value_x
                        } else {
                            content_area.x + 2
                        };
                        buf.set_string(x_pos, y + i as u16, line, val_style);
                    }
                    y += visible.len().saturating_sub(1) as u16;
                    // Hint line.
                    y += 1;
                    if y < max_y {
                        let pos_hint = if total > viewport_h {
                            format!(
                                " [{}\u{2013}{}/ {}]",
                                scroll + 1,
                                (scroll + viewport_h).min(total),
                                total
                            )
                        } else {
                            String::new()
                        };
                        let hint = format!("  (e to collapse, j/k to scroll{})", pos_hint);
                        buf.set_string(
                            content_area.x + 2,
                            y,
                            hint,
                            Style::default().fg(theme.gray_dim),
                        );
                    }
                }
            }
        } else if value.is_empty() {
            let empty_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.gray_dim).bg(bg)
            } else {
                Style::default().fg(theme.gray_dim)
            };
            buf.set_string(value_x, y, "-", empty_style);
        } else if value.width() <= value_w {
            // Fits on one line.
            let val_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.text_primary).bg(bg)
            } else {
                Style::default().fg(theme.text_primary)
            };
            buf.set_string(value_x, y, value, val_style);
        } else {
            // Word-wrap long values.
            let val_style = if let Some(bg) = row_bg {
                Style::default().fg(theme.text_primary).bg(bg)
            } else {
                Style::default().fg(theme.text_primary)
            };
            let lines = word_wrap_lines(value, value_w);
            for (i, line) in lines.iter().enumerate() {
                if y + i as u16 >= max_y {
                    break;
                }
                let x_pos = if i == 0 {
                    value_x
                } else {
                    content_area.x + label_w
                };
                buf.set_string(x_pos, y + i as u16, line, val_style);
            }
            y += lines.len().saturating_sub(1) as u16;
        }

        y += 2; // spacing between fields
    }

    // I/O sections
    for (section, items) in [("Inputs", &state.inputs), ("Outputs", &state.outputs)] {
        if items.is_empty() || y >= max_y {
            continue;
        }
        buf.set_string(
            content_area.x,
            y,
            section,
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        );
        y += 1;
        for entry in items {
            if y >= max_y {
                break;
            }
            let req = if entry.required { ", required" } else { "" };
            let header = format!("  \u{2022} {} ({}{})", entry.name, entry.io_type, req);
            buf.set_string(
                content_area.x,
                y,
                &header,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            );
            if !entry.description.is_empty() {
                // Wrap the description across multiple lines below the header.
                let indent = 4usize;
                let desc_w = w.saturating_sub(indent);
                if desc_w > 0 {
                    y += 1;
                    for desc_line in word_wrap_lines(&entry.description, desc_w) {
                        if y >= max_y {
                            break;
                        }
                        let padded = format!("{:indent$}{desc_line}", "", indent = indent);
                        buf.set_string(
                            content_area.x,
                            y,
                            &padded,
                            Style::default().fg(theme.text_secondary),
                        );
                        y += 1;
                    }
                } else {
                    y += 1;
                }
            } else {
                y += 1;
            }
        }
        y += 1;
    }

    // Source path
    if y < max_y
        && let Some(ref path) = state.source_path
    {
        let src = format!("Source: {}", path.display());
        let truncated: String = src.chars().take(w).collect();
        buf.set_string(
            content_area.x,
            y,
            &truncated,
            Style::default().fg(theme.gray_dim),
        );
    }
}

fn persona_detail_sizing(compact: bool) -> ModalSizing {
    ModalSizing {
        width_pct: 0.70,
        max_width: 100,
        min_width: 44,
        v_margin: 4,
        h_pad: 2,
        v_pad: 1,
        footer_lines: 2,
    }
    .with_compact(compact)
}

fn build_shortcuts(state: &PersonaDetailState) -> Vec<Shortcut<'static>> {
    if state.is_editing() {
        vec![
            Shortcut {
                label: "Enter save",
                clickable: false,
                id: 0,
            },
            Shortcut {
                label: "Esc cancel",
                clickable: false,
                id: 0,
            },
        ]
    } else {
        let mut shortcuts = vec![Shortcut {
            label: "j/k nav",
            clickable: false,
            id: 0,
        }];
        if state.editable {
            shortcuts.push(Shortcut {
                label: "e edit field",
                clickable: false,
                id: 0,
            });
        }
        if state.source_path.is_some() && state.editable {
            shortcuts.push(Shortcut {
                label: "i $EDITOR",
                clickable: false,
                id: 0,
            });
        }
        shortcuts.push(Shortcut {
            label: "Esc back",
            clickable: false,
            id: 0,
        });
        shortcuts
    }
}

// ---------------------------------------------------------------------------
// Input handling
// ---------------------------------------------------------------------------

pub fn handle_persona_detail_key(
    state: &mut PersonaDetailState,
    key: &KeyEvent,
) -> PersonaDetailOutcome {
    state.message = None;

    if state.is_editing() {
        handle_editing_key(state, key)
    } else {
        handle_browse_key(state, key)
    }
}

pub fn handle_persona_detail_paste(
    state: &mut PersonaDetailState,
    text: &str,
) -> PersonaDetailOutcome {
    if !state.is_editing() {
        return PersonaDetailOutcome::Unchanged;
    }
    state.message = None;
    let outcome = match &mut state.mode {
        PersonaDetailMode::Editing { editor, .. } => editor.insert_paste(text),
        PersonaDetailMode::Browse => unreachable!("editing mode changed before paste"),
    };
    finish_edit(outcome)
}

fn handle_browse_key(state: &mut PersonaDetailState, key: &KeyEvent) -> PersonaDetailOutcome {
    // When instructions are expanded and selected, j/k scrolls within them.
    let instr_scrolling =
        state.selected_field == PersonaField::Instructions && state.instructions_expanded;

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            // If instructions are expanded, collapse first instead of closing.
            if instr_scrolling {
                state.instructions_expanded = false;
                state.instructions_scroll = 0;
                return PersonaDetailOutcome::Changed;
            }
            PersonaDetailOutcome::Close
        }
        KeyCode::Char('j') | KeyCode::Down if instr_scrolling => {
            state.instructions_scroll = state.instructions_scroll.saturating_add(1);
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('k') | KeyCode::Up if instr_scrolling => {
            state.instructions_scroll = state.instructions_scroll.saturating_sub(1);
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.selected_field = state.selected_field.next();
            PersonaDetailOutcome::Changed
        }
        // Instructions: e/Enter toggles expand/collapse.
        KeyCode::Char('e') | KeyCode::Enter
            if state.selected_field == PersonaField::Instructions =>
        {
            state.instructions_expanded = !state.instructions_expanded;
            state.instructions_scroll = 0;
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.selected_field = state.selected_field.prev();
            PersonaDetailOutcome::Changed
        }
        // Other fields: e/Enter opens inline editor.
        KeyCode::Char('e') | KeyCode::Enter => {
            if !state.editable {
                state.message = Some("Bundled personas are read-only".to_string());
                return PersonaDetailOutcome::Changed;
            }
            let field = state.selected_field;
            if !field.is_editable() {
                state.message = Some("This field cannot be edited inline".to_string());
                return PersonaDetailOutcome::Changed;
            }
            let current = state.field_value(field).to_owned();
            if current.contains(['\n', '\r']) {
                state.message =
                    Some("Multiline values must be edited in the source file".to_string());
                return PersonaDetailOutcome::Changed;
            }
            let mut editor = LineEditor::default();
            editor.set_text(&current);
            let original = current;
            state.mode = PersonaDetailMode::Editing {
                field,
                editor,
                original,
            };
            PersonaDetailOutcome::Changed
        }
        KeyCode::Char('i') => {
            if let Some(ref path) = state.source_path {
                if state.editable {
                    return PersonaDetailOutcome::EditInEditor { path: path.clone() };
                }
                state.message = Some("Bundled personas are read-only".to_string());
            } else {
                state.message = Some("No source file".to_string());
            }
            PersonaDetailOutcome::Changed
        }
        _ => PersonaDetailOutcome::Unchanged,
    }
}

fn handle_editing_key(state: &mut PersonaDetailState, key: &KeyEvent) -> PersonaDetailOutcome {
    if key.code == KeyCode::Esc {
        state.mode = PersonaDetailMode::Browse;
        return PersonaDetailOutcome::Changed;
    }
    if key.code == KeyCode::Enter {
        let mode = std::mem::replace(&mut state.mode, PersonaDetailMode::Browse);
        let PersonaDetailMode::Editing {
            field,
            editor,
            original,
        } = mode
        else {
            return PersonaDetailOutcome::Unchanged;
        };
        let new_value = editor.text().to_owned();
        let changed = new_value != original;
        if changed {
            state.set_field_value(field, new_value);
            state.dirty = true;
            if let Err(e) = state.save_to_file() {
                state.message = Some(format!("Save failed: {e}"));
            } else {
                state.message = Some("Saved".to_string());
            }
        }
        return PersonaDetailOutcome::Changed;
    }

    let outcome = match &mut state.mode {
        PersonaDetailMode::Editing { editor, .. } => editor.handle_key(key),
        PersonaDetailMode::Browse => return PersonaDetailOutcome::Unchanged,
    };
    finish_edit(outcome)
}

fn finish_edit(outcome: LineEditOutcome) -> PersonaDetailOutcome {
    match outcome {
        LineEditOutcome::TextChanged
        | LineEditOutcome::CursorChanged
        | LineEditOutcome::HandledNoChange => PersonaDetailOutcome::Changed,
        LineEditOutcome::Unhandled => PersonaDetailOutcome::Unchanged,
    }
}

pub fn handle_persona_detail_mouse(
    state: &mut PersonaDetailState,
    mouse: &MouseEvent,
) -> PersonaDetailOutcome {
    let chrome =
        modal_window::handle_modal_mouse(&mut state.window, mouse.kind, mouse.column, mouse.row);
    match chrome {
        modal_window::ModalWindowOutcome::CloseRequested => PersonaDetailOutcome::Close,
        modal_window::ModalWindowOutcome::Handled => PersonaDetailOutcome::Changed,
        _ => PersonaDetailOutcome::Unchanged,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn word_wrap_lines(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        if raw_line.width() <= max_width {
            lines.push(raw_line.to_string());
        } else {
            let mut current = String::new();
            for word in raw_line.split_whitespace() {
                if current.is_empty() {
                    current = word.to_string();
                } else if current.width() + 1 + word.width() <= max_width {
                    current.push(' ');
                    current.push_str(word);
                } else {
                    lines.push(current);
                    current = word.to_string();
                }
            }
            if !current.is_empty() {
                lines.push(current);
            }
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests;
