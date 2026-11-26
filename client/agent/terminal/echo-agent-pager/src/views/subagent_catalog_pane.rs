//! Subagent catalog pane — browseable list of bundled personas/roles/agents.
//!
//! Read-only pane that renders grouped entries from [`BundleState`]. Headers
//! (Personas, Roles, Agents) are non-selectable; items below each header
//! are selectable and scrollable via the standard [`ListPane`] machinery.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crossterm::event::{KeyEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::StatefulWidget;

use crate::app::bundle::BundleState;
use crate::appearance::LayoutConfig;
use crate::scrollback::layout::HorizontalLayout;
use crate::theme::Theme;

use super::list_pane::{
    ListItem, ListPane, ListPaneConfig, ListPaneState, ListPaneStyle, WrapMode,
};
use super::overlay::OverlayState;

// ---------------------------------------------------------------------------
// CatalogEntry
// ---------------------------------------------------------------------------

struct CatalogEntry {
    id: u64,
    label: String,
    styled: Line<'static>,
    is_header: bool,
    kind: Option<&'static str>,
}

impl ListItem for CatalogEntry {
    fn content(&self) -> &Line<'_> {
        &self.styled
    }

    fn stable_id(&self) -> u64 {
        self.id
    }

    fn is_selectable(&self) -> bool {
        !self.is_header
    }

    fn search_text(&self) -> &str {
        &self.label
    }
}

fn lookup_description<'a>(kind: &str, name: &str, state: &'a BundleState) -> Option<&'a str> {
    match kind {
        "persona" => state
            .persona_details
            .iter()
            .find(|d| d.name == name)
            .and_then(|d| d.description.as_deref())
            .filter(|d| !d.is_empty()),
        "role" => state
            .role_details
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.description.as_str())
            .filter(|d| !d.is_empty()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SubagentCatalogPane
// ---------------------------------------------------------------------------

const MAX_CATALOG_HEIGHT: u16 = 8;
const MAX_CATALOG_FRACTION: f32 = 0.15;

pub struct SubagentCatalogPane {
    entries: Vec<CatalogEntry>,
    pub list_state: ListPaneState,
    list_style: ListPaneStyle,
    pub overlay: OverlayState,
}

impl Default for SubagentCatalogPane {
    fn default() -> Self {
        Self::new()
    }
}

impl SubagentCatalogPane {
    pub fn new() -> Self {
        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: false,
            search_enabled: true,
            copy_enabled: false,
            show_selection_when_unfocused: false,
            visual_select_enabled: false,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let list_state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
        Self {
            entries: Vec::new(),
            list_state,
            list_style: ListPaneStyle::default(),
            overlay: OverlayState::hidden(),
        }
    }

    // -- Data sync -----------------------------------------------------------

    pub fn sync_from_bundle(&mut self, state: &BundleState) {
        self.entries.clear();
        if !state.has_cache {
            return;
        }

        let theme = Theme::current();
        let header_style = Style::default()
            .fg(theme.gray_bright)
            .add_modifier(Modifier::BOLD);
        let item_style = Style::default().fg(theme.text_primary);
        let desc_style = Style::default().fg(theme.gray_bright);

        let groups: [(&str, &'static str, &[String]); 3] = [
            ("Personas", "persona", &state.personas),
            ("Roles", "role", &state.roles),
            ("Agents", "agent", &state.agents),
        ];

        for (name, kind, items) in &groups {
            if items.is_empty() {
                continue;
            }
            let mut hasher = DefaultHasher::new();
            name.hash(&mut hasher);
            let owned_name = name.to_string();
            self.entries.push(CatalogEntry {
                id: hasher.finish(),
                styled: Line::from(Span::styled(owned_name.clone(), header_style)),
                label: owned_name,
                is_header: true,
                kind: None,
            });
            for item in *items {
                let mut hasher = DefaultHasher::new();
                name.hash(&mut hasher);
                item.hash(&mut hasher);
                let desc = lookup_description(kind, item, state);
                let spans = if let Some(d) = &desc {
                    vec![
                        Span::styled(format!("  {item}"), item_style),
                        Span::styled(format!(" \u{00b7} {d}"), desc_style),
                    ]
                } else {
                    vec![Span::styled(format!("  {item}"), item_style)]
                };
                self.entries.push(CatalogEntry {
                    id: hasher.finish(),
                    label: item.clone(),
                    styled: Line::from(spans),
                    is_header: false,
                    kind: Some(kind),
                });
            }
        }
    }

    // -- Visibility ----------------------------------------------------------

    pub fn is_visible(&self) -> bool {
        self.overlay.visible
    }

    pub fn on_state_change(&mut self) {
        if !self.overlay.visible {
            self.list_state.close_input_bar();
        }
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
        let fraction_cap = (view_height as f32 * MAX_CATALOG_FRACTION).floor() as u16;
        let max = MAX_CATALOG_HEIGHT.min(fraction_cap).max(1);
        (count as u16).min(max).max(1)
    }

    /// Returns `(kind, name)` of the currently selected non-header entry.
    ///
    /// `kind` is the lowercase singular form (`"persona"`, `"role"`, `"agent"`).
    pub fn selected_entry(&self) -> Option<(&str, &str)> {
        let selected_id = self.list_state.selected_id()?;
        let entry = self.entries.iter().find(|e| e.id == selected_id)?;
        if entry.is_header {
            return None;
        }
        Some((entry.kind?, &entry.label))
    }

    // -- Input handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
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

    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        focused: bool,
        layout_cfg: &LayoutConfig,
    ) {
        let inner = Self::content_area(area, layout_cfg);
        if self.entries.is_empty() {
            if inner.height > 0 && inner.width > 0 {
                let theme = Theme::current();
                let span =
                    Span::styled("No bundled items.", Style::default().fg(theme.gray_bright));
                buf.set_span(inner.x, inner.y, &span, inner.width);
            }
            return;
        }
        self.list_state
            .prepare_layout(&self.entries, inner.width, inner.height);
        ListPane::new(&self.entries)
            .focused(focused)
            .style(self.list_style)
            .render(inner, buf, &mut self.list_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(personas: &[&str], roles: &[&str], agents: &[&str]) -> BundleState {
        BundleState {
            has_cache: true,
            version: "v2".into(),
            personas: personas.iter().map(|s| s.to_string()).collect(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
            agents: agents.iter().map(|s| s.to_string()).collect(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        }
    }

    #[test]
    fn sync_empty_state_produces_no_entries() {
        let mut pane = SubagentCatalogPane::new();
        pane.sync_from_bundle(&BundleState::default());
        assert!(pane.entries.is_empty());
    }

    #[test]
    fn sync_no_cache_produces_no_entries() {
        let mut pane = SubagentCatalogPane::new();
        let state = BundleState {
            has_cache: false,
            personas: vec!["researcher".into()],
            ..Default::default()
        };
        pane.sync_from_bundle(&state);
        assert!(pane.entries.is_empty());
    }

    #[test]
    fn sync_with_data_produces_grouped_entries() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["researcher", "implementer"], &["reviewer"], &["default"]);
        pane.sync_from_bundle(&state);
        // 3 headers + 4 items = 7 entries
        assert_eq!(pane.entries.len(), 7);
        assert!(pane.entries[0].is_header);
        assert_eq!(pane.entries[0].label, "Personas");
        assert!(!pane.entries[1].is_header);
        assert_eq!(pane.entries[1].label, "researcher");
        assert!(!pane.entries[2].is_header);
        assert_eq!(pane.entries[2].label, "implementer");
        assert!(pane.entries[3].is_header);
        assert_eq!(pane.entries[3].label, "Roles");
        assert!(!pane.entries[4].is_header);
        assert_eq!(pane.entries[4].label, "reviewer");
        assert!(pane.entries[5].is_header);
        assert_eq!(pane.entries[5].label, "Agents");
        assert!(!pane.entries[6].is_header);
        assert_eq!(pane.entries[6].label, "default");
    }

    #[test]
    fn sync_partial_data_skips_empty_groups() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["researcher", "auditor"], &[], &[]);
        pane.sync_from_bundle(&state);
        // 1 header + 2 items = 3 (no Roles/Agents headers)
        assert_eq!(pane.entries.len(), 3);
        assert!(pane.entries[0].is_header);
        assert_eq!(pane.entries[0].label, "Personas");
        assert!(!pane.entries[1].is_header);
        assert!(!pane.entries[2].is_header);
    }

    #[test]
    fn headers_are_not_selectable() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["researcher"], &["reviewer"], &[]);
        pane.sync_from_bundle(&state);
        for entry in &pane.entries {
            assert_eq!(entry.is_selectable(), !entry.is_header);
        }
    }

    #[test]
    fn desired_height_zero_when_hidden() {
        let pane = SubagentCatalogPane::new();
        assert!(!pane.overlay.visible);
        assert_eq!(pane.desired_height(40), 0);
    }

    #[test]
    fn desired_height_capped_by_entry_count() {
        let mut pane = SubagentCatalogPane::new();
        pane.overlay.visible = true;
        let state = make_state(&["a", "b"], &[], &[]);
        pane.sync_from_bundle(&state);
        // 1 header + 2 items = 3 entries, should cap at 3
        assert_eq!(pane.desired_height(80), 3);
    }

    #[test]
    fn desired_height_zero_for_tiny_terminal() {
        let mut pane = SubagentCatalogPane::new();
        pane.overlay.visible = true;
        let state = make_state(&["a"], &[], &[]);
        pane.sync_from_bundle(&state);
        assert_eq!(pane.desired_height(10), 0);
    }

    #[test]
    fn stable_ids_are_unique() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["a", "b"], &["a"], &["a"]);
        pane.sync_from_bundle(&state);
        let ids: Vec<u64> = pane.entries.iter().map(|e| e.stable_id()).collect();
        let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "all stable IDs must be unique");
    }

    #[test]
    fn sync_replaces_previous_entries() {
        let mut pane = SubagentCatalogPane::new();
        let state1 = make_state(&["a", "b", "c"], &[], &[]);
        pane.sync_from_bundle(&state1);
        assert_eq!(pane.entries.len(), 4); // 1 header + 3

        let state2 = make_state(&["x"], &[], &[]);
        pane.sync_from_bundle(&state2);
        assert_eq!(pane.entries.len(), 2); // 1 header + 1
        assert_eq!(pane.entries[1].label, "x");
    }

    #[test]
    fn selected_entry_returns_kind_and_name() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["researcher"], &["reviewer"], &["default"]);
        pane.sync_from_bundle(&state);

        // [0]=Personas, [1]=researcher, [2]=Roles, [3]=reviewer, [4]=Agents, [5]=default
        pane.list_state.select_by_id(pane.entries[1].id);
        assert_eq!(pane.selected_entry(), Some(("persona", "researcher")));

        pane.list_state.select_by_id(pane.entries[3].id);
        assert_eq!(pane.selected_entry(), Some(("role", "reviewer")));

        pane.list_state.select_by_id(pane.entries[5].id);
        assert_eq!(pane.selected_entry(), Some(("agent", "default")));
    }

    #[test]
    fn selected_entry_returns_none_for_header() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["researcher"], &[], &[]);
        pane.sync_from_bundle(&state);

        // Select the "Personas" header (entries[0])
        pane.list_state.select_by_id(pane.entries[0].id);
        assert!(pane.selected_entry().is_none());
    }

    #[test]
    fn selected_entry_returns_none_when_empty() {
        let pane = SubagentCatalogPane::new();
        assert!(pane.selected_entry().is_none());
    }

    #[test]
    fn sync_with_descriptions_appends_to_styled_line() {
        use crate::app::bundle::{PersonaDetail, RoleDetail};
        let mut pane = SubagentCatalogPane::new();
        let mut state = make_state(&["researcher"], &["reviewer"], &[]);
        state.persona_details = vec![PersonaDetail {
            name: "researcher".into(),
            description: Some("thorough researcher".into()),
            has_inputs: false,
            has_outputs: false,
            source_path: None,
            scope_label: None,
        }];
        state.role_details = vec![RoleDetail {
            name: "reviewer".into(),
            description: "code reviewer".into(),
        }];
        pane.sync_from_bundle(&state);

        // researcher entry should have 2 spans (name + description)
        assert_eq!(pane.entries[1].styled.spans.len(), 2);
        // reviewer entry should have 2 spans
        assert_eq!(pane.entries[3].styled.spans.len(), 2);
    }

    #[test]
    fn sync_without_descriptions_has_single_span() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["researcher"], &[], &[]);
        pane.sync_from_bundle(&state);

        // No detail → single span
        assert_eq!(pane.entries[1].styled.spans.len(), 1);
    }

    #[test]
    fn empty_persona_description_renders_no_separator() {
        use crate::app::bundle::PersonaDetail;
        let mut pane = SubagentCatalogPane::new();
        let mut state = make_state(&["researcher"], &[], &[]);
        state.persona_details = vec![PersonaDetail {
            name: "researcher".into(),
            description: Some(String::new()),
            has_inputs: false,
            has_outputs: false,
            source_path: None,
            scope_label: None,
        }];
        pane.sync_from_bundle(&state);

        // Empty description should be filtered — single span, no dangling separator.
        assert_eq!(pane.entries[1].styled.spans.len(), 1);
    }

    #[test]
    fn entries_store_kind() {
        let mut pane = SubagentCatalogPane::new();
        let state = make_state(&["researcher"], &["reviewer"], &["default"]);
        pane.sync_from_bundle(&state);

        assert_eq!(pane.entries[0].kind, None); // header
        assert_eq!(pane.entries[1].kind, Some("persona"));
        assert_eq!(pane.entries[2].kind, None); // header
        assert_eq!(pane.entries[3].kind, Some("role"));
        assert_eq!(pane.entries[4].kind, None); // header
        assert_eq!(pane.entries[5].kind, Some("agent"));
    }
}
