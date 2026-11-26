//! Settings modal state, types, and filter cache.

use std::sync::Arc;

use ratatui::layout::Rect;

use crate::app::actions::Action;
use crate::input::line_editor::LineEditor;
use crate::settings::{
    CodingDataSharingLock, EnumChoice, OwnedEnumChoice, PagerLocalSnapshot, SettingCategory,
    SettingKey, SettingKind, SettingMeta, SettingValue, SettingsRegistry, StringValidator,
    current_value_for, dynamic_enum_choices,
};
use crate::views::modal_window::ModalWindowState;

use echo_agent_runtime::agent::config::UiConfig;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

/// Public display title of the modal — also used by
/// `views/modal.rs::ActiveModal::message` so renames stay in one place.
pub const MODAL_TITLE: &str = "Settings";

/// Width of the `"─ "` leading decoration before the title in the
/// modal's top border. Used to compute the breadcrumb hit-rect x offset.
pub(super) const TITLE_LEADING_DECORATION_W: u16 = 2; // `─ `: 1 cell box-drawing + 1 cell space.

// Descriptions are now expand-on-demand via Right/Left arrows;
// see `render_expanded_description`.

/// Below this width the row list is skipped (chrome renders empty).
pub(super) const CONTENT_MIN_WIDTH: u16 = 10;

/// Default max width for the modal. Keeps the row list compact on wide terminals.
pub(super) const STANDARD_MAX_WIDTH: u16 = 110;

/// Per-side margin when editing `max_thoughts_width` (modal widens
/// to `terminal_width - 2*margin` so the wrap preview is useful).
pub(super) const MAX_THOUGHTS_WIDTH_WIDENED_MARGIN: u16 = 8;

/// Outcome of a key or mouse event. Separate from `InputOutcome`
/// because the modal doesn't own `agent.active_modal` — close is
/// the caller's responsibility.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum SettingsKeyOutcome {
    /// Close the modal.
    Close,
    /// Forward to dispatch.
    Action(Action),
    /// Forward two actions in order (first must resolve before second).
    /// Used by `d`-reset-in-picker to revert preview before opening
    /// the reset-confirm overlay.
    ActionPair(Action, Action),
    /// Close the modal and dispatch `Action` (deep-link Esc revert or Enter commit).
    ActionThenClose(Action),
    /// Internal state mutation, no action.
    Changed,
    /// No-op.
    Unchanged,
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One row in the visible flat list — either a category header (non-
/// selectable) or a setting row (selectable, dispatchable).
#[derive(Debug, Clone)]
pub enum RowEntry {
    Header { category: SettingCategory },
    Setting { key: SettingKey, meta_index: usize },
}

/// Read-only projection of the modal's private discriminated state.
#[derive(Debug, Clone)]
pub enum SettingsModalMode {
    Browse,
    /// `/` was pressed; chars filter the visible rows.
    FilterFocused,
    /// Enum chooser sub-pane. `supports_preview` is cached at open
    /// time to avoid per-keystroke registry lookups.
    PickingEnum {
        key: SettingKey,
        choices_idx: usize,
        original_value: SettingValue,
        supports_preview: bool,
    },
    /// Group sub-sheet: a list of the group's child Bool toggles. `child_idx`
    /// is the focused child within the group. Space/Enter toggles in place
    /// (the sheet stays open); Esc returns to Browse. Mirrors `PickingEnum`'s
    /// open/render/commit flow but for independent toggles.
    PickingGroup {
        key: SettingKey,
        child_idx: usize,
    },
    /// Inline string/int editor. No live preview; Esc is a pure cancel.
    EditingValue {
        key: SettingKey,
    },
}

#[derive(Debug)]
pub(super) struct SettingsState {
    pub(super) filter: LineEditor,
    pub(super) mode: SettingsMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsModeKind {
    Browse,
    FilterFocused,
    PickingEnum,
    PickingGroup,
    EditingString,
    EditingInt,
}

impl SettingsState {
    pub(super) fn mode_kind(&self) -> SettingsModeKind {
        match &self.mode {
            SettingsMode::Browse => SettingsModeKind::Browse,
            SettingsMode::FilterFocused => SettingsModeKind::FilterFocused,
            SettingsMode::PickingEnum { .. } => SettingsModeKind::PickingEnum,
            SettingsMode::PickingGroup { .. } => SettingsModeKind::PickingGroup,
            SettingsMode::EditingString { .. } => SettingsModeKind::EditingString,
            SettingsMode::EditingInt { .. } => SettingsModeKind::EditingInt,
        }
    }
}

#[derive(Debug)]
pub(super) enum SettingsMode {
    Browse,
    FilterFocused,
    PickingEnum {
        key: SettingKey,
        choices_idx: usize,
        original_value: SettingValue,
        supports_preview: bool,
    },
    PickingGroup {
        key: SettingKey,
        child_idx: usize,
    },
    EditingString {
        key: SettingKey,
        editor: LineEditor,
        validator: StringValidator,
        validation_error: Option<String>,
    },
    EditingInt {
        key: SettingKey,
        buffer: String,
        min: i64,
        max: i64,
    },
}

/// Is the open sub-pane a [`crate::settings::is_consent_chooser`] pane?
pub(super) fn mode_is_consent_chooser(mode: &SettingsMode) -> bool {
    matches!(
        mode,
        SettingsMode::PickingEnum { key, .. } if crate::settings::is_consent_chooser(key)
    )
}

/// Settings modal state. Boxed inside `ActiveModal::Settings` to
/// avoid clippy `large_enum_variant`.
pub struct SettingsModalState {
    pub window: ModalWindowState,
    pub registry: Arc<SettingsRegistry>,
    /// `UiConfig` snapshot, refreshed by the dispatcher on mutations.
    pub ui_snapshot: UiConfig,
    pub pager_snapshot: PagerLocalSnapshot,
    /// Computed row layout (headers + settings, in render order).
    pub rows: Vec<RowEntry>,
    /// Index into `rows` of the focused row.
    pub selected: usize,
    /// Vertical scroll offset (line-granular).
    pub scroll_offset: usize,
    pub(super) state: SettingsState,
    /// Row indices matching `query`, recomputed per mutation (not per frame).
    pub(super) filtered_cache: Vec<usize>,

    // -- Mouse hit-test rects (populated by render) --
    pub list_area: Rect,
    /// Click-hit rect per row, parallel to `rows`.
    pub row_rects: Vec<Rect>,
    /// Click-hit rect for the value column on each row. Bool rows
    /// toggle on click; Enum/String/Int rows open the sub-pane.
    pub value_hit_rects: Vec<Rect>,
    /// `(decrement_rect, increment_rect)` for the Int stepper's
    /// `‹`/`›` glyphs. Zero-sized when not in Int editing mode.
    pub editor_adornment_rects: (Rect, Rect),
    /// Click-hit rect per choice in `PickingEnum`. Each rect spans the
    /// full height of a choice (including wrapped description lines).
    pub picker_choice_rects: Vec<Rect>,
    /// Hit-rect for the breadcrumb title in sub-pane modes
    /// (`PickingEnum`/`EditingValue`). Clicking anywhere on
    /// `Settings › <label>` cancels back to Browse. `None` in
    /// Browse/FilterFocused. Cleared on mode transitions.
    pub settings_breadcrumb_rect: Option<Rect>,
    /// Hover flag for the breadcrumb — adds underline affordance.
    pub breadcrumb_hovered: bool,
    /// Keys whose description is expanded (Right/l to expand, Left/h
    /// to collapse). Multiple rows can be expanded simultaneously.
    pub expanded_keys: std::collections::HashSet<&'static str>,
    /// Row under the mouse cursor for hover highlighting. Indexes
    /// `rows` in Browse, `picker_choice_rects` in PickingEnum,
    /// always `None` in EditingValue.
    pub hover_row: Option<usize>,
    /// When true, Esc/Enter from `PickingEnum` close the modal instead of
    /// returning to Browse. Set by deep-link open (`OpenSettingsFocus`
    /// / `/privacy`); cleared on leave from the picker.
    pub close_on_picker_exit: bool,
}

impl SettingsModalState {
    /// Construct a new modal state from a registry + snapshots.
    pub fn new(
        registry: Arc<SettingsRegistry>,
        ui_snapshot: UiConfig,
        pager_snapshot: PagerLocalSnapshot,
    ) -> Self {
        let rows = build_rows(&registry);
        // Start on the first selectable (non-header) row.
        let selected = rows
            .iter()
            .position(|r| matches!(r, RowEntry::Setting { .. }))
            .unwrap_or(0);
        let filtered_cache = compute_filtered(&rows, &registry, "");
        Self {
            window: ModalWindowState::new(),
            registry,
            ui_snapshot,
            pager_snapshot,
            rows,
            selected,
            scroll_offset: 0,
            state: SettingsState {
                filter: LineEditor::default(),
                mode: SettingsMode::Browse,
            },
            filtered_cache,
            list_area: Rect::default(),
            row_rects: Vec::new(),
            value_hit_rects: Vec::new(),
            editor_adornment_rects: (Rect::default(), Rect::default()),
            picker_choice_rects: Vec::new(),
            settings_breadcrumb_rect: None,
            breadcrumb_hovered: false,
            expanded_keys: std::collections::HashSet::new(),
            hover_row: None,
            close_on_picker_exit: false,
        }
    }

    /// Why a Browse row cannot be edited (`None` = editable). Consulted by
    /// both render and input.
    pub fn row_lock(&self, key: SettingKey) -> Option<CodingDataSharingLock> {
        if key == "coding_data_sharing" {
            self.pager_snapshot.coding_data_sharing_lock
        } else {
            None
        }
    }

    /// The currently-focused setting row, if any.
    pub fn focused_setting(&self) -> Option<(SettingKey, &SettingMeta)> {
        match self.rows.get(self.selected)? {
            RowEntry::Setting { key, meta_index } => {
                let meta = self.registry.all().get(*meta_index)?;
                Some((*key, meta))
            }
            RowEntry::Header { .. } => None,
        }
    }

    /// Focus a setting by registry key (Browse mode). Returns whether the
    /// key was found; no-op if missing.
    pub fn focus_key(&mut self, key: &str) -> bool {
        if let Some(idx) = self
            .rows
            .iter()
            .position(|r| matches!(r, RowEntry::Setting { key: k, .. } if *k == key))
        {
            self.selected = idx;
            self.clamp_selected_to_visible();
            return true;
        }
        false
    }

    /// Filtered row indices in render order.
    pub fn filtered_indices(&self) -> &[usize] {
        &self.filtered_cache
    }

    /// Rebuild rows from current process gates (voice / kitty / minimal).
    /// Keeps focus on the same key when possible; exits sub-panes if the key vanished.
    pub fn rebuild_rows(&mut self) {
        let prev_key = self.focused_setting().map(|(k, _)| k);
        let subpane_key = match &self.state.mode {
            SettingsMode::PickingEnum { key, .. }
            | SettingsMode::PickingGroup { key, .. }
            | SettingsMode::EditingString { key, .. }
            | SettingsMode::EditingInt { key, .. } => Some(*key),
            SettingsMode::Browse | SettingsMode::FilterFocused => None,
        };

        self.rows = build_rows(&self.registry);
        self.invalidate_filter();

        if let Some(key) = subpane_key {
            let still_visible = self
                .rows
                .iter()
                .any(|r| matches!(r, RowEntry::Setting { key: k, .. } if *k == key));
            if !still_visible {
                self.transition_to_browse();
            }
        }

        if let Some(key) = prev_key {
            if let Some(idx) = self
                .rows
                .iter()
                .position(|r| matches!(r, RowEntry::Setting { key: k, .. } if *k == key))
            {
                self.selected = idx;
            } else {
                self.selected = self
                    .rows
                    .iter()
                    .position(|r| matches!(r, RowEntry::Setting { .. }))
                    .unwrap_or(0);
            }
        } else {
            self.clamp_selected_to_visible();
        }
    }

    pub fn mode(&self) -> SettingsModalMode {
        match &self.state.mode {
            SettingsMode::Browse => SettingsModalMode::Browse,
            SettingsMode::FilterFocused => SettingsModalMode::FilterFocused,
            SettingsMode::PickingEnum {
                key,
                choices_idx,
                original_value,
                supports_preview,
            } => SettingsModalMode::PickingEnum {
                key,
                choices_idx: *choices_idx,
                original_value: original_value.clone(),
                supports_preview: *supports_preview,
            },
            SettingsMode::PickingGroup { key, child_idx } => SettingsModalMode::PickingGroup {
                key,
                child_idx: *child_idx,
            },
            SettingsMode::EditingString { key, .. } | SettingsMode::EditingInt { key, .. } => {
                SettingsModalMode::EditingValue { key }
            }
        }
    }

    pub fn query(&self) -> &str {
        self.state.filter.text()
    }

    pub fn query_cursor(&self) -> usize {
        self.state.filter.cursor_byte()
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.state.filter.set_text(query);
        self.invalidate_filter();
        self.clamp_selected_to_visible();
    }

    pub fn editing_buffer(&self) -> Option<&str> {
        match &self.state.mode {
            SettingsMode::EditingString { editor, .. } => Some(editor.text()),
            SettingsMode::EditingInt { buffer, .. } => Some(buffer),
            _ => None,
        }
    }

    pub fn editing_cursor_byte(&self) -> Option<usize> {
        match &self.state.mode {
            SettingsMode::EditingString { editor, .. } => Some(editor.cursor_byte()),
            _ => None,
        }
    }

    pub fn editing_validation_error(&self) -> Option<&str> {
        match &self.state.mode {
            SettingsMode::EditingString {
                validation_error, ..
            } => validation_error.as_deref(),
            _ => None,
        }
    }

    /// Recompute `filtered_cache` from the current `query`.
    pub(super) fn invalidate_filter(&mut self) {
        self.filtered_cache =
            compute_filtered(&self.rows, &self.registry, self.state.filter.text());
    }

    /// Snap `selected` to the first visible setting if filtered out.
    pub(super) fn clamp_selected_to_visible(&mut self) {
        if self.filtered_cache.is_empty() {
            return;
        }
        if self.filtered_cache.contains(&self.selected) {
            return;
        }
        // Snap to first selectable row in the visible filter.
        for &row_idx in &self.filtered_cache {
            if matches!(self.rows[row_idx], RowEntry::Setting { .. }) {
                self.selected = row_idx;
                return;
            }
        }
    }

    /// Read the current value for a setting key.
    pub fn value_for(&self, key: SettingKey) -> Option<SettingValue> {
        current_value_for(key, &self.ui_snapshot, &self.pager_snapshot)
    }

    /// Move `selected` forward, skipping headers and filtered-out rows.
    pub(super) fn advance_next(&mut self) -> bool {
        let cur_pos = self.filtered_cache.iter().position(|&i| i == self.selected);
        let mut next = match cur_pos {
            Some(p) => p + 1,
            // Defensive: resume from top if `selected` is hidden.
            None => 0,
        };
        while next < self.filtered_cache.len() {
            let row_idx = self.filtered_cache[next];
            if matches!(self.rows[row_idx], RowEntry::Setting { .. }) {
                self.selected = row_idx;
                return true;
            }
            next += 1;
        }
        false
    }

    /// Move `selected` backward, skipping headers and filtered-out rows.
    pub(super) fn advance_prev(&mut self) -> bool {
        if self.filtered_cache.is_empty() {
            return false;
        }
        let cur_pos = self.filtered_cache.iter().position(|&i| i == self.selected);
        let mut prev = match cur_pos {
            Some(p) if p > 0 => p - 1,
            Some(_) => return false,
            // Defensive: resume from bottom if `selected` is hidden.
            None => self.filtered_cache.len() - 1,
        };
        loop {
            let row_idx = self.filtered_cache[prev];
            if matches!(self.rows[row_idx], RowEntry::Setting { .. }) {
                self.selected = row_idx;
                return true;
            }
            if prev == 0 {
                break;
            }
            prev -= 1;
        }
        false
    }

    /// Set selection to `idx` if it's a selectable row.
    pub fn select_at(&mut self, idx: usize) -> bool {
        if idx >= self.rows.len() {
            return false;
        }
        if !matches!(self.rows[idx], RowEntry::Setting { .. }) {
            return false;
        }
        if self.selected == idx {
            return false;
        }
        self.selected = idx;
        true
    }

    /// Reset hit-test geometry so mouse handlers degrade gracefully
    /// when render is aborted. Does NOT clear `hover_row` — that's
    /// cleared on mode transitions instead to avoid per-frame flicker.
    pub(crate) fn reset_hit_rects(&mut self) {
        self.list_area = Rect::default();
        self.row_rects.clear();
        self.value_hit_rects.clear();
        self.editor_adornment_rects = (Rect::default(), Rect::default());
        self.picker_choice_rects.clear();
        self.settings_breadcrumb_rect = None;
        self.breadcrumb_hovered = false;
    }

    /// Transition to Browse, clearing sub-pane hover/breadcrumb state
    /// to prevent stale hit-rects across mode changes.
    pub(crate) fn transition_to_browse(&mut self) {
        self.state.mode = SettingsMode::Browse;
        self.hover_row = None;
        self.settings_breadcrumb_rect = None;
        self.breadcrumb_hovered = false;
        self.close_on_picker_exit = false;
    }

    pub fn focus_filter(&mut self) {
        self.state.mode = SettingsMode::FilterFocused;
    }

    pub(super) fn transition_to_picking_enum(
        &mut self,
        key: SettingKey,
        choices_idx: usize,
        original_value: SettingValue,
        supports_preview: bool,
    ) {
        self.state.mode = SettingsMode::PickingEnum {
            key,
            choices_idx,
            original_value,
            supports_preview,
        };
    }

    pub(super) fn transition_to_picking_group(&mut self, key: SettingKey, child_idx: usize) {
        self.state.mode = SettingsMode::PickingGroup { key, child_idx };
    }

    pub(super) fn transition_to_editing_string(
        &mut self,
        key: SettingKey,
        editor: LineEditor,
        validator: StringValidator,
        validation_error: Option<String>,
    ) {
        self.state.mode = SettingsMode::EditingString {
            key,
            editor,
            validator,
            validation_error,
        };
    }

    pub(super) fn transition_to_editing_int(
        &mut self,
        key: SettingKey,
        buffer: String,
        min: i64,
        max: i64,
    ) {
        self.state.mode = SettingsMode::EditingInt {
            key,
            buffer,
            min,
            max,
        };
    }

    /// Transition to `PickingEnum` if the focused row is Enum/DynamicEnum.
    /// Returns `false` if the focused row is another kind.
    pub fn try_enter_picking_enum(&mut self) -> bool {
        let (key, first_canonical, current_value, supports_preview, resolved_choices) = {
            let Some((key, meta)) = self.focused_setting() else {
                return false;
            };
            if self.row_lock(key).is_some() {
                return false;
            }
            // Handles both static `Enum` and `DynamicEnum` catalogs.
            let (supports_preview, resolved): (bool, Vec<OwnedEnumChoice>) = match &meta.kind {
                SettingKind::Enum {
                    choices,
                    supports_preview,
                    ..
                } => (
                    *supports_preview,
                    effective_enum_choices(key, choices, &self.pager_snapshot)
                        .into_iter()
                        .map(|c| OwnedEnumChoice {
                            canonical: c.canonical.to_string(),
                            display: c.display.to_string(),
                            description: c.description.to_string(),
                        })
                        .collect(),
                ),
                SettingKind::DynamicEnum {
                    source,
                    supports_preview,
                    ..
                } => (
                    *supports_preview,
                    dynamic_enum_choices(*source, &self.pager_snapshot),
                ),
                _ => return false,
            };
            // Soft-fail if a static catalog exceeds the product cap. DynamicEnum
            // (e.g. models) is exempt — those lists are runtime-sized and always
            // scroll. The chooser itself scrolls static lists too; this assert is
            // a design guard, not a render requirement.
            debug_assert!(
                resolved.len() <= MAX_PICKER_CHOICES
                    || matches!(meta.kind, SettingKind::DynamicEnum { .. }),
                "Static Enum setting `{}` has {} choices, exceeds MAX_PICKER_CHOICES ({}). \
                 Raise the cap deliberately if a larger curated catalog is required.",
                key,
                resolved.len(),
                MAX_PICKER_CHOICES,
            );
            let first = resolved
                .first()
                .map(|c| c.canonical.clone())
                .unwrap_or_default();
            let cur = self.value_for(key);
            (key, first, cur, supports_preview, resolved)
        };

        // Resolve choices_idx from current value. For DynamicEnum,
        // if the current value no longer exists in the catalog,
        // fall back to index 1 (first real entry past sentinel)
        // to avoid accidentally wiping the user's preference.
        let is_dynamic_enum = matches!(
            self.registry.find(key).map(|m| &m.kind),
            Some(SettingKind::DynamicEnum { .. })
        );
        let unknown_dynamic_fallback_idx = if is_dynamic_enum && resolved_choices.len() > 1 {
            1
        } else {
            0
        };
        let choices_idx = match &current_value {
            Some(SettingValue::Enum(cur)) => resolved_choices
                .iter()
                .position(|c| c.canonical == *cur)
                .unwrap_or(0),
            Some(SettingValue::String(cur)) if !cur.is_empty() => resolved_choices
                .iter()
                .position(|c| c.canonical == *cur)
                .unwrap_or(unknown_dynamic_fallback_idx),
            Some(SettingValue::String(_)) => 0,
            _ => 0,
        };
        if is_dynamic_enum
            && choices_idx == unknown_dynamic_fallback_idx
            && unknown_dynamic_fallback_idx != 0
        {
            // Telemetry: log when a DynamicEnum value is stale.
            tracing::warn!(
                target: "settings",
                key = key,
                ?current_value,
                "DynamicEnum picker entered with a current value that no longer resolves \
                 in the live catalog — focusing first real choice instead of the \
                 (no override) sentinel to defend against accidental destructive Enter",
            );
        }
        let original_value = current_value.unwrap_or_else(|| {
            // Fallback to first choice, using the right value carrier.
            match self.registry.find(key).map(|m| &m.kind) {
                Some(SettingKind::DynamicEnum { .. }) => SettingValue::String(first_canonical),
                Some(SettingKind::Enum { choices, .. }) => {
                    let first_static = choices.first().map(|c| c.canonical).unwrap_or("");
                    SettingValue::Enum(first_static)
                }
                _ => SettingValue::Enum(""),
            }
        });
        self.transition_to_picking_enum(key, choices_idx, original_value, supports_preview);
        self.hover_row = None;
        true
    }

    /// Transition to `PickingGroup` if the focused row is a `Group`. Returns
    /// `false` for any other kind so the caller can fall through to the
    /// enum/editor entry points.
    pub fn try_enter_picking_group(&mut self) -> bool {
        let Some((key, meta)) = self.focused_setting() else {
            return false;
        };
        if !matches!(meta.kind, SettingKind::Group { .. }) {
            return false;
        }
        self.transition_to_picking_group(key, 0);
        self.hover_row = None;
        true
    }

    /// Transition to `EditingValue` if the focused row is String or Int.
    pub fn try_enter_editing_value(&mut self) -> bool {
        let Some((key, meta)) = self.focused_setting() else {
            return false;
        };
        let kind = meta.kind.clone();
        let value = self.value_for(key);
        match kind {
            SettingKind::String {
                default, validator, ..
            } => {
                let text = match value {
                    Some(SettingValue::String(text)) => text,
                    _ => default.to_string(),
                };
                let mut editor = LineEditor::default();
                editor.set_text(text);
                let validation_error = validate_string(
                    validator,
                    editor.text(),
                    &self.pager_snapshot.available_models,
                );
                self.transition_to_editing_string(key, editor, validator, validation_error);
            }
            SettingKind::Int {
                default, min, max, ..
            } => {
                let buffer = match value {
                    Some(SettingValue::Int(value)) => value.to_string(),
                    _ => default.to_string(),
                };
                self.transition_to_editing_int(key, buffer, min, max);
            }
            _ => return false,
        }
        self.hover_row = None;
        true
    }

    /// Build the Action that toggles the focused Bool row. Returns
    /// `None` with an error log on registry skew (caught by CI tests).
    pub fn toggle_focused_bool(&self) -> Option<Action> {
        let (key, meta) = self.focused_setting()?;
        if !matches!(meta.kind, SettingKind::Bool { .. }) {
            return None;
        }
        let cur = match self.value_for(key) {
            Some(SettingValue::Bool(b)) => b,
            Some(other) => {
                tracing::error!(
                    target: "settings",
                    ?key,
                    ?other,
                    "Bool-kind setting resolved to non-Bool value — registry skew",
                );
                return None;
            }
            None => {
                tracing::error!(
                    target: "settings",
                    ?key,
                    "Bool-kind setting has no current_value_for arm — registry skew",
                );
                return None;
            }
        };
        let action = action_for_bool(key, !cur);
        if action.is_none() {
            tracing::error!(
                target: "settings",
                ?key,
                "Bool-kind setting has no action_for_bool arm — registry skew",
            );
        }
        action
    }
}

/// Compute filtered row indices for a query. Headers are emitted only
/// when ≥1 setting in their section matches. Returns all indices when
/// `query` is empty.
pub(super) fn compute_filtered(
    rows: &[RowEntry],
    registry: &SettingsRegistry,
    query: &str,
) -> Vec<usize> {
    if query.is_empty() {
        return (0..rows.len()).collect();
    }
    let matched_keys: Vec<SettingKey> = registry.search(query).iter().map(|m| m.key).collect();
    let mut result = Vec::new();
    let mut pending_header: Option<usize> = None;
    for (i, row) in rows.iter().enumerate() {
        match row {
            RowEntry::Header { .. } => {
                // Emit header only when section has a match.
                pending_header = Some(i);
            }
            RowEntry::Setting { key, .. } => {
                if matched_keys.contains(key) {
                    if let Some(h) = pending_header.take() {
                        result.push(h);
                    }
                    result.push(i);
                }
            }
        }
    }
    result
}

/// Row visibility: voice rows need the voice gate; capture needs key releases;
/// `hidden_in_minimal` rows are dropped in minimal mode. Pure for unit tests.
pub(super) fn setting_row_visible(
    meta: &SettingMeta,
    kitty_releases: bool,
    minimal: bool,
    voice_mode: bool,
) -> bool {
    if !voice_mode
        && matches!(
            meta.key,
            "voice_keybind_enabled" | "voice_capture_mode" | "voice_stt_language"
        )
    {
        return false;
    }
    if meta.key == "voice_capture_mode" && !kitty_releases {
        return false;
    }
    if minimal && meta.hidden_in_minimal {
        return false;
    }
    true
}

fn build_rows(registry: &SettingsRegistry) -> Vec<RowEntry> {
    let kitty_releases = crate::app::kitty_releases_reported();
    let minimal = crate::app::minimal_mode_active();
    let voice_mode = crate::app::voice_mode_enabled();
    // Keys that belong to a group sub-sheet are rendered only inside that
    // sheet, never as their own top-level rows.
    let group_children: std::collections::HashSet<SettingKey> = registry
        .all()
        .iter()
        .filter_map(|m| match &m.kind {
            SettingKind::Group { children } => Some(*children),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    let mut rows = Vec::new();
    for cat in SettingCategory::ALL {
        let mut emitted_header = false;
        for (meta_index, meta) in registry.all().iter().enumerate() {
            if meta.category != *cat {
                continue;
            }
            if !setting_row_visible(meta, kitty_releases, minimal, voice_mode) {
                continue;
            }
            if group_children.contains(meta.key) {
                continue;
            }
            if !emitted_header {
                rows.push(RowEntry::Header { category: *cat });
                emitted_header = true;
            }
            rows.push(RowEntry::Setting {
                key: meta.key,
                meta_index,
            });
        }
    }
    rows
}

/// Construct the typed `Action::Set*` for a Bool setting.
pub(super) fn action_for_bool(key: SettingKey, new: bool) -> Option<Action> {
    match key {
        "compact_mode" => Some(Action::SetCompactMode(new)),
        "show_timestamps" => Some(Action::SetTimestamps(new)),
        "show_timeline" => Some(Action::SetTimeline(new)),
        "simple_mode" => Some(Action::SetSimpleMode(new)),
        "contextual_hints.undo" => Some(Action::SetContextualHintUndo(new)),
        "contextual_hints.plan_mode" => Some(Action::SetContextualHintPlanMode(new)),
        "contextual_hints.image_input" => Some(Action::SetContextualHintImageInput(new)),
        "contextual_hints.send_now" => Some(Action::SetContextualHintSendNow(new)),
        "contextual_hints.small_screen" => Some(Action::SetContextualHintSmallScreen(new)),
        "contextual_hints.word_select" => Some(Action::SetContextualHintWordSelect(new)),
        "contextual_hints.ssh_wrap" => Some(Action::SetContextualHintSshWrap(new)),
        "multiline_mode" => Some(Action::SetMultilineMode(new)),
        "vim_mode" => Some(Action::SetVimMode(new)),
        "voice_keybind_enabled" => Some(Action::SetVoiceKeybindEnabled(new)),
        "remember_tool_approvals" => Some(Action::SetRememberToolApprovals(new)),
        "toolset.ask_user_question.timeout_enabled" => {
            Some(Action::SetAskUserQuestionTimeoutEnabled(new))
        }
        "show_thinking_blocks" => Some(Action::SetShowThinkingBlocks(new)),
        "group_tool_verbs" => Some(Action::SetGroupToolVerbs(new)),
        "collapsed_edit_blocks" => Some(Action::SetCollapsedEditBlocks(new)),
        "prompt_suggestions" => Some(Action::SetPromptSuggestions(new)),
        "respect_manual_folds" => Some(Action::SetRespectManualFolds(new)),
        "page_flip_on_send" => Some(Action::SetPageFlipOnSend(new)),
        "confirm_before_rewind" => Some(Action::SetConfirmBeforeRewind(new)),
        "combine_queued_prompts" => Some(Action::SetCombineQueuedPrompts(new)),

        "invert_scroll" => Some(Action::SetInvertScroll(new)),
        "show_tips" => Some(Action::SetShowTips(new)),
        "auto_update" => Some(Action::SetAutoUpdate(new)),
        "display_refresh_auto_cadence" => Some(Action::SetDisplayRefreshAutoCadence(new)),
        _ => None,
    }
}

/// Construct `Action::Preview*` for an Enum setting — used by the
/// picker's Up/Down (live preview) and Esc (revert). Preview actions
/// never persist; they only mutate the live visual.
pub(super) fn action_for_enum(key: SettingKey, choice: &'static str) -> Option<Action> {
    match key {
        "theme" => Some(Action::PreviewTheme(choice.to_string())),
        "auto_dark_theme" => Some(Action::PreviewAutoDarkTheme(choice.to_string())),
        "auto_light_theme" => Some(Action::PreviewAutoLightTheme(choice.to_string())),
        // No preview for settings with irreversible side effects.
        "permission_mode" => None,
        "coding_data_sharing" => None,
        "plan_mode" => None,
        "render_mermaid" => None,
        "keep_text_selection" => None,
        "scroll_mode" => None,
        _ => None,
    }
}

/// Construct `Action::Set*` commit variant for an Enum setting.
/// Commit actions persist to disk and fire a toast.
pub(super) fn action_for_enum_commit(key: SettingKey, choice: &'static str) -> Option<Action> {
    match key {
        "theme" => Some(Action::SetTheme(choice.to_string())),
        "auto_dark_theme" => Some(Action::SetAutoDarkTheme(choice.to_string())),
        "auto_light_theme" => Some(Action::SetAutoLightTheme(choice.to_string())),
        // Canonical strings from settings/defs.rs are the source of truth.
        "permission_mode" => match choice {
            "always-approve" => Some(Action::SetPermissionMode(
                crate::app::actions::PermissionModeKind::AlwaysApprove,
            )),
            // Auto's feature gate is enforced in `set_permission_mode`
            // (via `app.auto_mode_gate`, the same source the Shift+Tab cycle
            // uses), so the modal and the cycle never disagree. Committing Auto
            // when the gate is off degrades to Ask there.
            "auto" => Some(Action::SetPermissionMode(
                crate::app::actions::PermissionModeKind::Auto,
            )),
            "ask" => Some(Action::SetPermissionMode(
                crate::app::actions::PermissionModeKind::Ask,
            )),
            "default" => Some(Action::SetPermissionMode(
                crate::app::actions::PermissionModeKind::Default,
            )),
            _ => None,
        },
        "coding_data_sharing" => match choice {
            "opt-in" => Some(Action::SetCodingDataSharing { opted_in: true }),
            "opt-out" => Some(Action::SetCodingDataSharing { opted_in: false }),
            _ => None,
        },
        "plan_mode" => match choice {
            "on" => Some(Action::SetPlanMode(crate::app::actions::PlanModeKind::On)),
            "off" => Some(Action::SetPlanMode(crate::app::actions::PlanModeKind::Off)),
            _ => None,
        },
        "hunk_tracker_mode" => Some(Action::SetHunkTrackerMode(choice.to_string())),
        "screen_mode" => Some(Action::SetScreenMode(choice.to_string())),
        "voice_capture_mode" => Some(Action::SetVoiceCaptureMode(choice.to_string())),
        "voice_stt_language" => Some(Action::SetVoiceSttLanguage(choice.to_string())),
        "render_mermaid" => {
            crate::appearance::RenderMermaid::from_canonical(choice).map(Action::SetRenderMermaid)
        }
        "keep_text_selection" => crate::appearance::TextSelection::from_canonical(choice)
            .map(Action::SetKeepTextSelection),
        // Junk canonicals fold to None — Enter no-ops instead of mis-mapping.
        "scroll_mode" => {
            crate::appearance::ScrollMode::from_canonical(choice).map(Action::SetScrollMode)
        }
        "follow_up_behavior" => crate::appearance::FollowUpBehavior::from_canonical(choice)
            .map(Action::SetFollowUpBehavior),
        "default_selected_permission" => {
            Some(Action::SetDefaultSelectedPermission(choice.to_string()))
        }
        _ => None,
    }
}

/// Construct `Action::Set*` commit variant for a String setting.
/// Resolves model names via the snapshot before producing the action.
/// Empty buffer maps to `Action::Clear*` for model settings.
pub(super) fn action_for_string(
    key: SettingKey,
    value: String,
    snapshot: &PagerLocalSnapshot,
) -> Option<Action> {
    match key {
        "default_model" => {
            if value.is_empty() {
                Some(Action::ClearDefaultModel)
            } else {
                snapshot
                    .resolve_model_name(&value)
                    .map(Action::SetDefaultModel)
            }
        }
        "fork_secondary_model" => {
            if value.is_empty() {
                Some(Action::ClearForkSecondaryModel)
            } else {
                snapshot
                    .resolve_model_name(&value)
                    .map(Action::SetForkSecondaryModel)
            }
        }

        _ => {
            let _ = value;
            let _ = snapshot;
            None
        }
    }
}

/// Construct `Action::Set*` commit variant for an Int setting.
pub(super) fn action_for_int(key: SettingKey, value: i64) -> Option<Action> {
    match key {
        "max_thoughts_width" => Some(Action::SetMaxThoughtsWidth(value)),
        "scroll_speed" => Some(Action::SetScrollSpeed(value)),
        "scroll_lines" => Some(Action::SetScrollLines(value)),
        _ => None,
    }
}

/// Validate a String buffer against the registered `StringValidator`.
/// Returns `Some(error_message)` on failure, `None` on success.
pub(super) fn validate_string(
    validator: StringValidator,
    buffer: &str,
    available_models: &[(String, agent_client_protocol::ModelId)],
) -> Option<String> {
    match validator {
        StringValidator::Any => None,
        StringValidator::NonEmptyToken => {
            if buffer.is_empty() {
                Some("Value cannot be empty".to_string())
            } else if buffer.chars().any(|c| c.is_whitespace()) {
                Some("Value cannot contain whitespace".to_string())
            } else {
                None
            }
        }
        StringValidator::KnownModel => {
            // Empty = "clear default" sentinel.
            if buffer.is_empty() {
                return None;
            }
            // Reject if the model catalog hasn't loaded yet.
            if available_models.is_empty() {
                return Some("Model catalog still loading, try again".to_string());
            }
            let matched = available_models
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(buffer));
            if matched {
                None
            } else {
                Some(format!("Unknown model: \"{buffer}\""))
            }
        }
    }
}

/// Soft product cap on static Enum choices (settings unit tests enforce it).
///
/// The chooser already scrolls within the viewport when the focused choice
/// falls off-screen (`picker_scroll_offset`); this limit exists so catalogs
/// stay intentionally curated rather than unbounded. Sized to fit the full
/// EchoAgent STT language list (25 codes + client-only `auto` = 26) with headroom.
pub(crate) const MAX_PICKER_CHOICES: usize = 32;

/// The children of a group setting, or an empty slice if `key` is not a group.
pub(super) fn group_children(state: &SettingsModalState, key: SettingKey) -> &'static [SettingKey] {
    match state.registry.find(key).map(|m| &m.kind) {
        Some(SettingKind::Group { children }) => children,
        _ => &[],
    }
}

/// Whether `(key, canonical)` is gated off and must not be offered as a choice:
/// `permission_mode`'s "auto" when the auto gate is off, and
/// `voice_capture_mode`'s "hold" without key-release reporting. Pure (gates
/// passed as args) so it's unit-testable without touching process globals.
pub(super) fn enum_choice_gated_off(
    key: SettingKey,
    canonical: &str,
    auto_mode_gate: bool,
    kitty_releases: bool,
) -> bool {
    (key == "permission_mode" && canonical == "auto" && !auto_mode_gate)
        || (key == "voice_capture_mode" && canonical == "hold" && !kitty_releases)
}

/// The effective static Enum choices for a picker, hiding gated-off options so
/// the modal never offers a choice the setter would silently no-op. Every
/// index-based picker path (len / at / render / seed) routes through this.
pub(super) fn effective_enum_choices<'a>(
    key: SettingKey,
    choices: &'a [EnumChoice],
    snapshot: &PagerLocalSnapshot,
) -> Vec<&'a EnumChoice> {
    let kitty_releases = crate::app::kitty_releases_reported();
    choices
        .iter()
        .filter(|c| {
            !enum_choice_gated_off(key, c.canonical, snapshot.auto_mode_gate, kitty_releases)
        })
        .collect()
}
