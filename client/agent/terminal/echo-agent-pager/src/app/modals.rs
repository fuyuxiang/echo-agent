//! Modal dialog handling for [`AgentView`]: the `handle_modal_key` /
//! `handle_modal_mouse` input dispatchers, the command palette / arg picker /
//! doc picker input handlers, and the active-modal draw dispatch.
//!
//! Extracted from `agent_view.rs` as a sibling `impl AgentView` block (same
//! pattern as `queue_edit.rs` and `mouse.rs`).

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use super::actions::Action;
use super::agent_view::{AgentView, active_contexts_for_pane, apply_settings_outcome};
use super::app_view::InputOutcome;

use crate::theme::Theme;
use crate::views::modal::{self, ActiveModal};

impl AgentView {
    /// `suggest_args` falls back to model rows when the query is not in effort
    /// phase. Model-phase reasoning rows use a trailing space in `insert_text`;
    /// effort rows do not. Require a non-empty list with no trailing-space
    /// rows before treating the picker as effort phase.
    fn arg_items_look_like_effort_phase(items: &[crate::slash::command::ArgItem]) -> bool {
        !items.is_empty()
            && items
                .iter()
                .all(|item| !item.insert_text.ends_with(char::is_whitespace))
    }

    /// Step the model ArgPicker from effort phase back to the model list.
    /// Returns `true` if the modal was updated (caller should not fully close).
    fn try_arg_picker_step_back_from_effort(&mut self) -> bool {
        Self::try_arg_picker_step_back_from_effort_modal(
            &mut self.active_modal,
            &self.prompt.slash_controller,
            &self.session.models,
            &self.session.cwd,
        )
    }

    fn try_arg_picker_step_back_from_effort_modal(
        active_modal: &mut Option<ActiveModal>,
        slash_controller: &crate::slash::SlashController,
        models: &crate::acp::model_state::ModelState,
        cwd: &std::path::Path,
    ) -> bool {
        let Some(ActiveModal::ArgPicker {
            command,
            args_query,
            ..
        }) = active_modal.as_ref()
        else {
            return false;
        };
        if args_query.is_empty() || !matches!(command.as_str(), "model" | "m") {
            return false;
        }
        let command = command.clone();
        let Some(cmd) = slash_controller.registry().get(&command) else {
            return false;
        };
        let ctx = crate::slash::command::AppCtx {
            models,
            cwd,
            has_session_announcements: slash_controller.has_session_announcements(),
            billing_surface_visible: slash_controller.billing_surface_visible(),
            usage_command_visible: slash_controller.usage_command_visible(),
            workflows_available: slash_controller.workflows_available(),
            saved_workflows: slash_controller.registry().saved_workflows(),
            workflow_runs: slash_controller.workflow_runs(),
            screen_mode: slash_controller.screen_mode(),
            current_title: slash_controller.current_title(),
        };
        let Some(model_items) = cmd.suggest_args(&ctx, "") else {
            return false;
        };
        if model_items.is_empty() {
            return false;
        }
        if let Some(ActiveModal::ArgPicker {
            args_query,
            items,
            original_items,
            state,
            ..
        }) = active_modal.as_mut()
        {
            args_query.clear();
            *items = model_items.clone();
            *original_items = model_items;
            // Model list is type-to-find: reopen input-default like the initial /model open.
            *state = crate::views::picker::PickerState::input_active();
        }
        true
    }

    /// Handle a key press while a modal dialog is active.
    ///
    /// Matches the pressed character against the modal's options and resolves
    /// the result. All non-matching keys are consumed (blocked).
    #[cfg(test)]
    pub(super) fn handle_modal_key(&mut self, key: &KeyEvent) -> InputOutcome {
        let registry = crate::actions::ActionRegistry::defaults();
        self.handle_modal_key_with_registry(key, &registry)
    }

    /// Handle modal input using the live action registry that dispatched it.
    pub(super) fn handle_modal_key_with_registry(
        &mut self,
        key: &KeyEvent,
        registry: &crate::actions::ActionRegistry,
    ) -> InputOutcome {
        use crate::views::modal::ActiveModal;
        use crate::views::modal_window::{self as mw, ModalWindowOutcome};

        // Peek at the modal type to decide dispatch strategy.
        let Some(ref mut modal) = self.active_modal else {
            return InputOutcome::Changed;
        };

        // Picker-based modals: route Esc through ModalWindow chrome first,
        // then delegate remaining keys to the picker input handler.
        if matches!(
            modal,
            ActiveModal::CommandPalette { .. }
                | ActiveModal::ArgPicker { .. }
                | ActiveModal::SessionPicker { .. }
                | ActiveModal::DocPicker { .. }
        ) {
            // Extract window state for handle_modal_key.
            let (window, query_empty, esc_clears) = match modal {
                ActiveModal::CommandPalette { window, state, .. } => {
                    (window, state.query().is_empty(), true)
                }
                ActiveModal::ArgPicker { window, state, .. } => {
                    (window, state.query().is_empty(), false)
                }
                ActiveModal::SessionPicker { window, state, .. } => {
                    (window, state.query().is_empty(), false)
                }
                ActiveModal::DocPicker { window, state, .. } => {
                    (window, state.query().is_empty(), true)
                }
                _ => unreachable!(),
            };
            // These modals don't use fold; fold_info is None so
            // Left/Right/h/l return Unhandled and reach the picker.
            let chrome_cfg = mw::ModalWindowConfig {
                title: "",
                tabs: None,
                shortcuts: &[],
                sizing: mw::ModalSizing::default(),
                fold_info: None,
            };
            let outcome = mw::handle_modal_key(window, key, &chrome_cfg);
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    // If query non-empty and esc_clears_query: clear query first.
                    if esc_clears && !query_empty {
                        match modal {
                            ActiveModal::CommandPalette { state, .. } => {
                                state.set_query("");
                                state.selected = 0;
                                state.scroll_offset = None;
                            }
                            ActiveModal::DocPicker { state, .. } => {
                                state.set_query("");
                                state.selected = 0;
                                state.scroll_offset = None;
                            }
                            _ => {}
                        }
                        return InputOutcome::Changed;
                    }
                    // Otherwise delegate close to the picker handler which
                    // knows about palette snapshots / restore logic.
                    if matches!(self.active_modal, Some(ActiveModal::DocPicker { .. })) {
                        let ev = crossterm::event::Event::Key(*key);
                        return self.handle_doc_input(&ev);
                    }
                    let ev = crossterm::event::Event::Key(*key);
                    return self.handle_palette_or_arg_input_with_registry(&ev, registry);
                }
                ModalWindowOutcome::Unhandled => {
                    // Non-Esc key (including Left/Right/h/l):
                    // forward to picker input handler.
                    if matches!(self.active_modal, Some(ActiveModal::DocPicker { .. })) {
                        let ev = crossterm::event::Event::Key(*key);
                        return self.handle_doc_input(&ev);
                    }
                    let ev = crossterm::event::Event::Key(*key);
                    return self.handle_palette_or_arg_input_with_registry(&ev, registry);
                }
                _ => return InputOutcome::Changed,
            }
        }

        // RememberNoteReview: modal preview for # remember notes.
        if let ActiveModal::RememberNoteReview {
            ref mut scroll,
            ref mut showing_enhanced,
            ref enhanced_content,
            ref mut cached_lines,
            ref mut window,
            ..
        } = *modal
        {
            let chrome_cfg = mw::ModalWindowConfig {
                title: "",
                tabs: None,
                shortcuts: &[],
                sizing: mw::ModalSizing::default(),
                fold_info: None,
            };
            match mw::handle_modal_key(window, key, &chrome_cfg) {
                mw::ModalWindowOutcome::CloseRequested => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                mw::ModalWindowOutcome::Handled => return InputOutcome::Changed,
                mw::ModalWindowOutcome::Unhandled => {}
                _ => {}
            }

            match key.code {
                KeyCode::Enter => {
                    return InputOutcome::Action(Action::SaveRememberNoteFromModal);
                }
                KeyCode::Char('y') if key.modifiers.is_empty() => {
                    return InputOutcome::Action(Action::SaveRememberNoteFromModal);
                }
                KeyCode::Tab => {
                    if enhanced_content.is_some() {
                        *showing_enhanced = !*showing_enhanced;
                        *cached_lines = None;
                        *scroll = 0;
                        return InputOutcome::Changed;
                    }
                    return InputOutcome::Unchanged;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = scroll.saturating_add(1);
                    return InputOutcome::Changed;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    return InputOutcome::Changed;
                }
                KeyCode::PageDown => {
                    *scroll = scroll.saturating_add(10);
                    return InputOutcome::Changed;
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(10);
                    return InputOutcome::Changed;
                }
                _ => return InputOutcome::Unchanged,
            }
        }

        // DocViewer: route through ModalWindow chrome, then handle scroll.
        if let ActiveModal::DocViewer {
            window,
            previous_palette,
            standalone,
            ..
        } = modal
        {
            let chrome_cfg = mw::ModalWindowConfig {
                title: "",
                tabs: None,
                shortcuts: &[],
                sizing: mw::ModalSizing::default(),
                fold_info: None,
            };
            let outcome = mw::handle_modal_key(window, key, &chrome_cfg);
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    if *standalone {
                        self.active_modal = None;
                    } else {
                        // Esc in DocViewer -> back to DocPicker list.
                        // Shuttle the palette snapshot so DocPicker can restore it on its own Esc.
                        let prev = previous_palette.take();
                        self.active_modal = Some(crate::views::modal::howto_list_modal(prev));
                    }
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Unhandled => {
                    // Scroll keys (no fold for doc viewer).
                    let ev = crossterm::event::Event::Key(*key);
                    return self.handle_doc_input(&ev);
                }
                _ => return InputOutcome::Changed,
            }
        }
        // ShortcutsHelp: modal window chrome first, then picker / detail.
        if let ActiveModal::ShortcutsHelp {
            entries,
            state,
            window,
            filter_active,
            collapsed_sections,
            expanded_ids,
            mode,
        } = modal
        {
            use crate::views::shortcuts_help::{self, ShortcutsHelpOutcome};
            let searching = state.search_active || !state.query().is_empty();
            if mode.is_browse() && searching && key.code == KeyCode::Esc {
                state.set_query("");
                state.search_active = false;
                state.selected = 0;
                return InputOutcome::Changed;
            }
            // Detail owns Esc (back to browse); skip chrome so it doesn't close the modal.
            let footer = if mode.is_detail() {
                shortcuts_help::modal_footer_detail()
            } else {
                shortcuts_help::modal_footer(*filter_active)
            };
            let chrome_cfg = mw::ModalWindowConfig {
                title: "Keyboard Shortcuts",
                tabs: None,
                shortcuts: &footer,
                sizing: crate::views::shortcuts_help::modal_sizing(
                    self.scrollback.appearance().prompt.compact,
                ),
                fold_info: None,
            };
            if mode.is_browse() {
                match mw::handle_modal_key(window, key, &chrome_cfg) {
                    mw::ModalWindowOutcome::CloseRequested => {
                        self.active_modal = None;
                        return InputOutcome::Changed;
                    }
                    mw::ModalWindowOutcome::Unhandled => {}
                    _ => return InputOutcome::Changed,
                }
            }
            match shortcuts_help::handle_input(
                key,
                entries,
                state,
                *filter_active,
                collapsed_sections,
                expanded_ids,
                mode,
            ) {
                ShortcutsHelpOutcome::Close => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                ShortcutsHelpOutcome::ToggleFilter => {
                    *filter_active = !*filter_active;
                    state.selected = 0;
                    return InputOutcome::Changed;
                }
                ShortcutsHelpOutcome::ToggleSection(idx) => {
                    shortcuts_help::toggle_membership(collapsed_sections, idx);
                    return InputOutcome::Changed;
                }
                ShortcutsHelpOutcome::ToggleExpand(action_id) => {
                    shortcuts_help::toggle_membership(expanded_ids, action_id);
                    return InputOutcome::Changed;
                }
                ShortcutsHelpOutcome::Changed => return InputOutcome::Changed,
                ShortcutsHelpOutcome::Unchanged => return InputOutcome::Unchanged,
            }
        }

        // MemoryBrowser: route through ModalWindow chrome, then delegate.
        if let ActiveModal::MemoryBrowser { state } = modal {
            // When the filter input is focused, Esc exits filter mode
            // instead of closing the modal. Handle before modal chrome.
            if matches!(
                state.mode,
                crate::views::memory_modal::MemoryModalMode::FilterFocused
            ) {
                return crate::views::memory_modal::handle_memory_key(state, key);
            }
            let chrome_cfg = mw::ModalWindowConfig {
                title: "",
                tabs: None,
                shortcuts: &[],
                sizing: mw::ModalSizing::default(),
                fold_info: None,
            };
            let outcome = mw::handle_modal_key(&mut state.window, key, &chrome_cfg);
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Unhandled => {
                    return crate::views::memory_modal::handle_memory_key(state, key);
                }
                _ => return InputOutcome::Changed,
            }
        }

        // Settings: route through ModalWindow chrome, then delegate.
        if let ActiveModal::Settings { state } = modal {
            // Sub-mode short-circuit: FilterFocused, PickingEnum, PickingGroup,
            // and EditingValue own their own Esc/keystroke semantics.
            if matches!(
                state.mode(),
                crate::views::settings_modal::SettingsModalMode::FilterFocused
                    | crate::views::settings_modal::SettingsModalMode::PickingEnum { .. }
                    | crate::views::settings_modal::SettingsModalMode::PickingGroup { .. }
                    | crate::views::settings_modal::SettingsModalMode::EditingValue { .. }
            ) {
                let out = crate::views::settings_modal::handle_settings_key(state, key);
                return apply_settings_outcome(self, out);
            }
            let chrome_cfg = mw::ModalWindowConfig {
                title: "",
                tabs: None,
                shortcuts: &[],
                sizing: mw::ModalSizing::default(),
                fold_info: None,
            };
            let chrome_outcome = mw::handle_modal_key(&mut state.window, key, &chrome_cfg);
            match chrome_outcome {
                ModalWindowOutcome::CloseRequested => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Unhandled => {
                    let out = crate::views::settings_modal::handle_settings_key(state, key);
                    return apply_settings_outcome(self, out);
                }
                _ => return InputOutcome::Changed,
            }
        }

        // UsageInfo: chrome (Esc/close) first, then tabs / scroll / copy.
        if let ActiveModal::UsageInfo { state } = modal {
            let chrome_cfg = mw::ModalWindowConfig {
                title: "",
                tabs: None,
                shortcuts: &[],
                sizing: mw::ModalSizing::default(),
                fold_info: None,
            };
            match mw::handle_modal_key(&mut state.window, key, &chrome_cfg) {
                ModalWindowOutcome::CloseRequested => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Unhandled => {
                    use crate::views::usage_modal::{self, UsageModalOutcome};
                    return match usage_modal::handle_usage_modal_key(state, key) {
                        UsageModalOutcome::CopySessionId => {
                            self.copy_usage_modal_session_id();
                            InputOutcome::Changed
                        }
                        UsageModalOutcome::CopyText(text) => {
                            self.copy_usage_modal_text(&text);
                            InputOutcome::Changed
                        }
                        UsageModalOutcome::Changed => InputOutcome::Changed,
                        UsageModalOutcome::Unchanged => InputOutcome::Unchanged,
                    };
                }
                _ => return InputOutcome::Changed,
            }
        }

        // ResetSettingsConfirm: y/n routing. Handled before generic
        // char-match so Esc/F2/Ctrl+, route to Cancel (not modal close).
        if let Some(ActiveModal::ResetSettingsConfirm { modal, .. }) = self.active_modal.as_ref() {
            let resolved = match key.code {
                KeyCode::Esc => Some(crate::views::modal::ResetSettingsResult::Cancel),
                KeyCode::F(2) => Some(crate::views::modal::ResetSettingsResult::Cancel),
                KeyCode::Char(',')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::SUPER) =>
                {
                    Some(crate::views::modal::ResetSettingsResult::Cancel)
                }
                // Only bare keystrokes — Ctrl+Y must not fire Reset.
                KeyCode::Char(c) if key.modifiers.is_empty() => modal.resolve(c).copied(),
                _ => None,
            };
            return match resolved {
                Some(choice) => InputOutcome::Action(Action::ConfirmResetSetting { choice }),
                None => InputOutcome::Changed,
            };
        }

        // EditConfirm: single char matching.
        let ch = match key.code {
            KeyCode::Char(c) => c,
            KeyCode::Esc => {
                self.active_modal = None;
                return InputOutcome::Changed;
            }
            _ => return InputOutcome::Changed, // consume, ignore
        };

        // Take the modal so we can match on it and modify self.
        let Some(modal) = self.active_modal.take() else {
            return InputOutcome::Changed;
        };

        match modal {
            ActiveModal::EditConfirm {
                modal: confirm,
                pending_target,
            } => self.handle_edit_confirm_choice(confirm, pending_target, ch),
            ActiveModal::CommandPalette { .. }
            | ActiveModal::ArgPicker { .. }
            | ActiveModal::SessionPicker { .. }
            | ActiveModal::DocPicker { .. }
            | ActiveModal::DocViewer { .. }
            | ActiveModal::ShortcutsHelp { .. }
            | ActiveModal::MemoryBrowser { .. }
            | ActiveModal::Settings { .. }
            | ActiveModal::UsageInfo { .. }
            | ActiveModal::ResetSettingsConfirm { .. }
            | ActiveModal::RememberNoteReview { .. } => unreachable!(),
        }
    }

    pub(super) fn handle_modal_paste(
        &mut self,
        text: &str,
        registry: &crate::actions::ActionRegistry,
    ) -> InputOutcome {
        use crate::views::modal::ActiveModal;

        let event = crossterm::event::Event::Paste(text.to_owned());
        if matches!(self.active_modal, Some(ActiveModal::DocPicker { .. })) {
            return self.handle_doc_input(&event);
        }
        if matches!(
            self.active_modal,
            Some(
                ActiveModal::CommandPalette { .. }
                    | ActiveModal::ArgPicker { .. }
                    | ActiveModal::SessionPicker { .. }
            )
        ) {
            return self.handle_palette_or_arg_input_with_registry(&event, registry);
        }

        if let Some(ActiveModal::ShortcutsHelp { state, mode, .. }) = self.active_modal.as_mut() {
            return match crate::views::shortcuts_help::handle_paste(text, state, mode) {
                crate::views::shortcuts_help::ShortcutsHelpOutcome::Changed => {
                    InputOutcome::Changed
                }
                _ => InputOutcome::Unchanged,
            };
        }
        if let Some(ActiveModal::MemoryBrowser { state }) = self.active_modal.as_mut() {
            return crate::views::memory_modal::handle_memory_paste(state, text);
        }
        let settings_outcome = match self.active_modal.as_mut() {
            Some(ActiveModal::Settings { state }) => Some(
                crate::views::settings_modal::handle_settings_paste(state, text),
            ),
            _ => None,
        };
        if let Some(outcome) = settings_outcome {
            return apply_settings_outcome(self, outcome);
        }
        if self.active_modal.is_some() {
            InputOutcome::Changed
        } else {
            InputOutcome::Unchanged
        }
    }

    /// Arg picker input (separate from command palette to avoid borrow conflicts
    /// when stepping back from the model effort phase via slash registry + session).
    fn handle_arg_picker_input(&mut self, ev: &crossterm::event::Event) -> InputOutcome {
        use crate::views::picker::{PickerConfig, PickerOutcome, handle_picker_input};

        enum ArgPickerStep {
            Selected(crate::slash::command::ArgItem),
            Closed,
            FilterChanged,
        }

        let (command_clone, in_effort_phase, entry_count) = match self.active_modal.as_ref() {
            Some(ActiveModal::ArgPicker {
                command,
                args_query,
                items,
                ..
            }) => (command.clone(), !args_query.is_empty(), items.len()),
            _ => return InputOutcome::Changed,
        };

        let config = PickerConfig {
            title: None,
            show_search_hint: false,
            expandable: false,
            esc_clears_query: false,
            shortcuts: Some(crate::views::picker::picker_shortcuts()),
            pending_hint: None,
            non_selectable: &[],
            non_selectable_clickable: &[],
            shortcuts_area: None,
            tabs: None,
            active_tab: 0,
            filter_label: None,
            filter_key_hint: None,
            filter_active: false,
            header_note: None,
            action_keys: &[],
            disable_search: false,
            compact_bottom_bar: false,
            search_only_on_slash: false,
            vim_normal_first: crate::appearance::cache::load_vim_mode(),
        };

        let step = {
            let Some(ActiveModal::ArgPicker { items, state, .. }) = self.active_modal.as_mut()
            else {
                return InputOutcome::Changed;
            };
            match handle_picker_input(ev, state, entry_count, &config) {
                PickerOutcome::Selected(i) => match items.get(i).cloned() {
                    Some(item) => ArgPickerStep::Selected(item),
                    None => return InputOutcome::Changed,
                },
                PickerOutcome::Closed => ArgPickerStep::Closed,
                PickerOutcome::QueryChanged => ArgPickerStep::FilterChanged,
                PickerOutcome::Changed => return InputOutcome::Changed,
                PickerOutcome::Unchanged => return InputOutcome::Unchanged,
                _ => return InputOutcome::Changed,
            }
        };

        match step {
            ArgPickerStep::FilterChanged => {
                if let Some(ActiveModal::ArgPicker {
                    items,
                    original_items,
                    state,
                    ..
                }) = self.active_modal.as_mut()
                {
                    let q = state.query().to_lowercase();
                    *items = original_items
                        .iter()
                        .filter(|item| {
                            q.is_empty()
                                || item.match_text.to_lowercase().contains(&q)
                                || item.display.to_lowercase().contains(&q)
                                || item.description.to_lowercase().contains(&q)
                        })
                        .cloned()
                        .collect();
                    state.selected = state.selected.min(items.len().saturating_sub(1));
                }
                InputOutcome::Changed
            }
            ArgPickerStep::Closed => {
                if in_effort_phase && self.try_arg_picker_step_back_from_effort() {
                    return InputOutcome::Changed;
                }
                let snapshot = match self.active_modal.as_mut() {
                    Some(ActiveModal::ArgPicker {
                        previous_palette, ..
                    }) => previous_palette.take(),
                    _ => None,
                };
                if let Some(snapshot) = snapshot {
                    self.active_modal = Some(ActiveModal::CommandPalette {
                        entries: snapshot.entries,
                        state: snapshot.state,
                        window: crate::views::modal_window::ModalWindowState::new(),
                    });
                } else {
                    self.active_modal = None;
                }
                InputOutcome::Changed
            }
            ArgPickerStep::Selected(item) => {
                let chains_to_effort = matches!(command_clone.as_str(), "model" | "m")
                    && item.insert_text.ends_with(char::is_whitespace);
                if chains_to_effort {
                    let next_query = item.insert_text.clone();
                    if let Some(cmd) = self.prompt.slash_controller.registry().get(&command_clone) {
                        let ctx = self.prompt.slash_controller.app_ctx(&self.session.models);
                        if let Some(effort_items) = cmd.suggest_args(&ctx, &next_query)
                            && Self::arg_items_look_like_effort_phase(&effort_items)
                        {
                            if let Some(ActiveModal::ArgPicker {
                                args_query,
                                items,
                                original_items,
                                state,
                                ..
                            }) = self.active_modal.as_mut()
                            {
                                *args_query = next_query;
                                *items = effort_items.clone();
                                *original_items = effort_items;
                                // Effort sub-step is part of the type-to-find /model picker: open input-focused (cursor + type-to-filter), matching the rest of the flow.
                                *state = crate::views::picker::PickerState::input_active();
                            }
                            return InputOutcome::Changed;
                        }
                    }
                }
                let full = format!("/{} {}", command_clone, item.insert_text.trim_end());
                self.active_modal = None;
                InputOutcome::Action(Action::SendSlashCommandPreservingDraft(full))
            }
        }
    }

    /// Unified input handler for command palette and arg picker modals.
    #[cfg(test)]
    fn handle_palette_or_arg_input(&mut self, ev: &crossterm::event::Event) -> InputOutcome {
        let registry = crate::actions::ActionRegistry::defaults();
        self.handle_palette_or_arg_input_with_registry(ev, &registry)
    }

    fn handle_palette_or_arg_input_with_registry(
        &mut self,
        ev: &crossterm::event::Event,
        registry: &crate::actions::ActionRegistry,
    ) -> InputOutcome {
        use crate::views::modal::{ActiveModal, PaletteCommand};
        use crate::views::picker::{PickerConfig, PickerOutcome, handle_picker_input};

        if matches!(self.active_modal, Some(ActiveModal::ArgPicker { .. })) {
            return self.handle_arg_picker_input(ev);
        }

        let Some(ref mut modal) = self.active_modal else {
            return InputOutcome::Changed;
        };

        match modal {
            ActiveModal::CommandPalette {
                entries: _, state, ..
            } => {
                // Build filtered entries for count and non-selectable indices.
                let filtered = crate::views::modal::filter_palette_entries(
                    state.query(),
                    self.sharing_enabled,
                    &self.prompt.slash_controller,
                );
                let non_sel: Vec<bool> = filtered
                    .iter()
                    .map(|e| matches!(e.command, PaletteCommand::SectionHeader(_)))
                    .collect();
                let entry_count = filtered.len();

                let config = PickerConfig {
                    title: None,
                    show_search_hint: false,
                    expandable: false,
                    esc_clears_query: true,
                    shortcuts: Some(crate::views::picker::picker_shortcuts()),
                    pending_hint: None,
                    non_selectable: &non_sel,
                    non_selectable_clickable: &[],
                    shortcuts_area: None,
                    tabs: None,
                    active_tab: 0,
                    filter_label: None,
                    filter_key_hint: None,
                    filter_active: false,
                    header_note: None,
                    action_keys: &[],
                    disable_search: false,
                    compact_bottom_bar: false,
                    search_only_on_slash: false,
                    vim_normal_first: crate::appearance::cache::load_vim_mode(),
                };

                match handle_picker_input(ev, state, entry_count, &config) {
                    PickerOutcome::Selected(i) => {
                        if i >= filtered.len() {
                            return InputOutcome::Changed;
                        }
                        if matches!(filtered[i].command, PaletteCommand::SectionHeader(_)) {
                            return InputOutcome::Changed;
                        }
                        let cmd = filtered[i].command.clone();
                        match cmd {
                            PaletteCommand::NewSession => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::NewSession)
                            }
                            PaletteCommand::NewSessionInWorktree => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::NewWorktreeSession {
                                    load_session_id: None,
                                    label: None,
                                    git_ref: None,
                                })
                            }
                            PaletteCommand::Home => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::ExitSessionConfirmed)
                            }
                            PaletteCommand::Quit => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::QuitConfirmed)
                            }
                            PaletteCommand::HowTo => {
                                // Save palette state for Esc restore (same pattern as /resume).
                                let prev = {
                                    let ActiveModal::CommandPalette { entries, state, .. } =
                                        self.active_modal.as_ref().unwrap()
                                    else {
                                        unreachable!()
                                    };
                                    Some(crate::views::modal::PaletteSnapshot {
                                        entries: entries.clone(),
                                        state: state.clone(),
                                    })
                                };
                                self.active_modal =
                                    Some(crate::views::modal::howto_list_modal(prev));
                                InputOutcome::Changed
                            }
                            PaletteCommand::KeyboardShortcuts => {
                                use crate::views::shortcuts_help;
                                let mut contexts = active_contexts_for_pane(self.active_pane);
                                // Same overlay-context push as the Ctrl+.
                                // path (`handle_agent_action`,
                                // `ActionId::ShortcutsHelp`).
                                if self.in_dashboard_overlay {
                                    contexts.push(crate::actions::When::DashboardOverlay);
                                }
                                let entries = shortcuts_help::build_entries(
                                    &contexts,
                                    registry,
                                    self.vim_mode,
                                );
                                let state = shortcuts_help::build_initial_picker_state(&entries);
                                self.active_modal = Some(ActiveModal::ShortcutsHelp {
                                    entries,
                                    state,
                                    window: Default::default(),
                                    filter_active: false,
                                    collapsed_sections:
                                        crate::views::shortcuts_help::default_collapsed(),
                                    expanded_ids: std::collections::HashSet::new(),
                                    mode: crate::views::shortcuts_help::ShortcutsHelpMode::Browse,
                                });
                                InputOutcome::Changed
                            }
                            PaletteCommand::Memory => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::OpenMemoryModal)
                            }
                            PaletteCommand::OpenExtensionsTab(tab) => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::OpenExtensionsModal {
                                    tab,
                                    trigger: echo_agent_telemetry::events::ExtensionsModalTrigger::CommandPalette,
                                })
                            }
                            PaletteCommand::OpenSettings => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::OpenSettings)
                            }
                            PaletteCommand::OpenAgentsModal => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::OpenConfigAgentsModal(None))
                            }
                            PaletteCommand::EditPromptExternal => {
                                self.active_modal = None;
                                InputOutcome::Action(Action::EditPromptExternal)
                            }
                            PaletteCommand::SlashCommand(text) => {
                                let trimmed = text
                                    .trim_start_matches('/')
                                    .trim_end_matches(' ')
                                    .to_string();

                                if trimmed == "resume" {
                                    let prev = {
                                        let ActiveModal::CommandPalette { entries, state, .. } =
                                            self.active_modal.as_ref().unwrap()
                                        else {
                                            unreachable!()
                                        };
                                        Some(crate::views::modal::PaletteSnapshot {
                                            entries: entries.clone(),
                                            state: state.clone(),
                                        })
                                    };
                                    self.active_modal = Some(ActiveModal::SessionPicker {
                                        state: crate::views::picker::PickerState::default(),
                                        entries: None,
                                        loading: true,
                                        lanes: Default::default(),
                                        previous_palette: prev,
                                        window: crate::views::modal_window::ModalWindowState::new(),
                                        content_results: None,
                                        content_loading: false,
                                        deep_search_seq: 0,
                                        entries_query: None,
                                        source_filter:
                                            crate::views::session_picker::SourceFilter::default(),
                                        pending_delete: None,
                                    });
                                    return InputOutcome::Action(Action::FetchSessionList);
                                }

                                let is_picker =
                                    matches!(trimmed.as_str(), "model" | "m" | "theme" | "t");
                                if is_picker
                                    && let Some(command) =
                                        self.prompt.slash_controller.registry().get(&trimmed)
                                {
                                    let ctx =
                                        self.prompt.slash_controller.app_ctx(&self.session.models);
                                    if let Some(items) = command.suggest_args(&ctx, "")
                                        && !items.is_empty()
                                    {
                                        // Save palette state for Esc restore.
                                        let prev = {
                                            let ActiveModal::CommandPalette {
                                                entries, state, ..
                                            } = self.active_modal.as_ref().unwrap()
                                            else {
                                                unreachable!()
                                            };
                                            Some(crate::views::modal::PaletteSnapshot {
                                                entries: entries.clone(),
                                                state: state.clone(),
                                            })
                                        };
                                        self.active_modal = Some(ActiveModal::ArgPicker {
                                            command: trimmed,
                                            args_query: String::new(),
                                            items: items.clone(),
                                            original_items: items,
                                            // Type-to-find: open in input mode (vim: Esc→nav, i→input).
                                            state: crate::views::picker::PickerState::input_active(
                                            ),
                                            previous_palette: prev,
                                            window:
                                                crate::views::modal_window::ModalWindowState::new(),
                                        });
                                        return InputOutcome::Changed;
                                    }
                                }
                                self.active_modal = None;
                                InputOutcome::Action(Action::SendSlashCommandPreservingDraft(text))
                            }
                            PaletteCommand::SectionHeader(_) => InputOutcome::Changed,
                        }
                    }
                    PickerOutcome::Closed => {
                        self.active_modal = None;
                        InputOutcome::Changed
                    }
                    PickerOutcome::QueryChanged => {
                        // Re-filter entries based on updated query.
                        let sharing_enabled = self.sharing_enabled;
                        if let Some(ActiveModal::CommandPalette { entries, state, .. }) =
                            self.active_modal.as_mut()
                        {
                            *entries = crate::views::modal::filter_palette_entries(
                                state.query(),
                                sharing_enabled,
                                &self.prompt.slash_controller,
                            );
                            state.selected = state.selected.min(entries.len().saturating_sub(1));
                        }
                        InputOutcome::Changed
                    }
                    PickerOutcome::Changed => InputOutcome::Changed,
                    PickerOutcome::Unchanged => InputOutcome::Unchanged,
                    _ => InputOutcome::Changed,
                }
            }
            ActiveModal::ArgPicker { .. } => unreachable!("routed via handle_arg_picker_input"),
            ActiveModal::SessionPicker {
                entries,
                state,
                loading: _,
                previous_palette,
                content_results,
                content_loading,
                entries_query,
                source_filter,
                pending_delete,
                ..
            } => {
                use crate::views::session_picker::{
                    CONTENT_EXPAND_OFFSET, PickerItem, SessionPickerWorktreeSelection,
                    build_entry_map, effective_filter_query, session_picker_worktree_selection,
                    sync_session_picker_query_expansion,
                };

                // Build grouped mapping using shared helper (now with content).
                // Pin the current session's repo group using the live agent cwd.
                let current_repo = crate::views::session_picker::repo_name_from_cwd(
                    &self.session.cwd.to_string_lossy(),
                );
                let entry_map = build_entry_map(
                    entries.as_deref(),
                    content_results.as_deref(),
                    effective_filter_query(state.query(), entries_query.as_deref()),
                    true,
                    *content_loading,
                    *source_filter,
                    Some(current_repo.as_str()),
                );
                let entry_count = entry_map.len();
                let non_sel: Vec<bool> = entry_map.iter().map(|e| e.is_none()).collect();
                let focused_is_foreign = match entry_map
                    .get(state.selected)
                    .and_then(|entry| entry.as_ref())
                {
                    Some(PickerItem::Fuzzy { original_index }) => entries
                        .as_ref()
                        .and_then(|entries| entries.get(*original_index))
                        .is_some_and(|entry| {
                            crate::app::foreign_sessions::is_foreign_picker_source(&entry.source)
                        }),
                    _ => false,
                };

                // Chat-mode picker lists conversations only: the source
                // filter and local-disk delete are dead weight there.
                let chat_mode = self.app_chat_mode;
                let config = PickerConfig {
                    title: Some("Resume session"),
                    show_search_hint: true,
                    expandable: true,
                    esc_clears_query: false, // Esc returns to palette or closes
                    shortcuts: Some(crate::views::picker::picker_shortcuts()),
                    pending_hint: None,
                    non_selectable: &non_sel,
                    non_selectable_clickable: &[],
                    shortcuts_area: None,
                    tabs: None,
                    active_tab: 0,
                    filter_label: (!chat_mode).then(|| source_filter.label()),
                    filter_key_hint: (!chat_mode).then_some("f"),
                    filter_active: !chat_mode && source_filter.is_active(),
                    header_note: None,
                    action_keys: if chat_mode || focused_is_foreign {
                        &[]
                    } else {
                        &[('d', "delete")]
                    },
                    disable_search: false,
                    compact_bottom_bar: false,
                    search_only_on_slash: false,
                    vim_normal_first: crate::appearance::cache::load_vim_mode(),
                };

                match crate::views::session_picker::handle_pending_delete_key(pending_delete, ev) {
                    crate::views::session_picker::PendingDeleteKey::Confirm(pd) => {
                        return InputOutcome::Action(Action::DeleteSession {
                            source: pd.source,
                            session_id: pd.session_id,
                            cwd: pd.cwd,
                        });
                    }
                    crate::views::session_picker::PendingDeleteKey::Cancel => {
                        return InputOutcome::Changed;
                    }
                    crate::views::session_picker::PendingDeleteKey::Disarmed
                    | crate::views::session_picker::PendingDeleteKey::NotArmed => {}
                }

                if let crossterm::event::Event::Key(key) = ev
                    && let Some(selection) = session_picker_worktree_selection(
                        key,
                        state,
                        &entry_map,
                        &non_sel,
                        entries.as_deref(),
                        content_results.as_deref(),
                    )
                {
                    return InputOutcome::Action(match selection {
                        SessionPickerWorktreeSelection::Fuzzy(original_index) => {
                            Action::PickSessionInWorktree(original_index)
                        }
                        SessionPickerWorktreeSelection::Content { session_id, cwd } => {
                            Action::PickContentSessionInWorktree { session_id, cwd }
                        }
                        SessionPickerWorktreeSelection::Unavailable => {
                            return InputOutcome::Changed;
                        }
                    });
                }

                let selected_before = state.selected;
                let outcome = handle_picker_input(ev, state, entry_count, &config);
                if pending_delete.is_some() && state.selected != selected_before {
                    *pending_delete = None;
                }
                match outcome {
                    PickerOutcome::Selected(i) => {
                        match entry_map.get(i).and_then(|e| e.as_ref()) {
                            Some(PickerItem::Fuzzy { original_index }) => {
                                // Don't clear active_modal here — dispatch_pick_session
                                // reads entries from it before clearing.
                                InputOutcome::Action(Action::PickSession(*original_index))
                            }
                            Some(PickerItem::Content { hit_index }) => {
                                if let Some(hits) = content_results.as_ref()
                                    && let Some(hit) = hits.get(*hit_index)
                                {
                                    InputOutcome::Action(Action::PickContentSession {
                                        session_id: hit.session_id.clone(),
                                        cwd: hit.cwd.clone(),
                                    })
                                } else {
                                    InputOutcome::Changed
                                }
                            }
                            None => InputOutcome::Changed,
                        }
                    }
                    PickerOutcome::SubmitQuery => {
                        // Free-text load only for a UUID session id.
                        // Own the id before clearing the modal (state is a
                        // reborrow of `active_modal`).
                        let load_id =
                            crate::views::session_picker::session_id_for_direct_load(state.query())
                                .map(str::to_owned);
                        if let Some(sid) = load_id {
                            self.active_modal = None;
                            InputOutcome::Action(Action::LoadSession(sid, None, false))
                        } else {
                            InputOutcome::Unchanged
                        }
                    }
                    PickerOutcome::Closed => {
                        if let Some(snapshot) = previous_palette.take() {
                            self.active_modal = Some(ActiveModal::CommandPalette {
                                entries: snapshot.entries,
                                state: snapshot.state,
                                window: crate::views::modal_window::ModalWindowState::new(),
                            });
                        } else {
                            self.active_modal = None;
                        }
                        // A search/list fetch may still be in flight; the
                        // dispatch layer must invalidate it now that the
                        // modal (its landing surface) is gone.
                        InputOutcome::Action(Action::SessionPickerClosed)
                    }
                    PickerOutcome::Expand(i) => match entry_map.get(i).and_then(|e| e.as_ref()) {
                        Some(PickerItem::Fuzzy { original_index }) => {
                            if let Some(ents) = entries.as_ref()
                                && let Some(entry) = ents.get(*original_index)
                                && !crate::app::foreign_sessions::is_foreign_picker_source(
                                    &entry.source,
                                )
                                && !state.expanded.contains(original_index)
                            {
                                InputOutcome::Action(Action::ExpandSessionCard {
                                    source: entry.source.clone(),
                                    session_id: entry.id.clone(),
                                })
                            } else {
                                InputOutcome::Changed
                            }
                        }
                        Some(PickerItem::Content { hit_index }) => {
                            if let Some(hits) = content_results.as_ref()
                                && let Some(hit) = hits.get(*hit_index)
                            {
                                InputOutcome::Action(Action::ExpandSessionCard {
                                    source: "local".into(),
                                    session_id: hit.session_id.clone(),
                                })
                            } else {
                                InputOutcome::Changed
                            }
                        }
                        None => InputOutcome::Changed,
                    },
                    PickerOutcome::Collapse(i) => match entry_map.get(i).and_then(|e| e.as_ref()) {
                        Some(PickerItem::Fuzzy { original_index }) => {
                            if let Some(ents) = entries.as_ref()
                                && let Some(entry) = ents.get(*original_index)
                                && state.expanded.contains(original_index)
                            {
                                InputOutcome::Action(Action::ExpandSessionCard {
                                    source: entry.source.clone(),
                                    session_id: entry.id.clone(),
                                })
                            } else {
                                InputOutcome::Changed
                            }
                        }
                        Some(PickerItem::Content { hit_index }) => {
                            let key = CONTENT_EXPAND_OFFSET + hit_index;
                            if state.expanded.contains(&key)
                                && let Some(hits) = content_results.as_ref()
                                && let Some(hit) = hits.get(*hit_index)
                            {
                                InputOutcome::Action(Action::ExpandSessionCard {
                                    source: "local".into(),
                                    session_id: hit.session_id.clone(),
                                })
                            } else {
                                InputOutcome::Changed
                            }
                        }
                        None => InputOutcome::Changed,
                    },
                    PickerOutcome::Copy(i) => {
                        if let Some(Some(PickerItem::Fuzzy { original_index })) = entry_map.get(i) {
                            InputOutcome::Action(Action::CopySessionId(*original_index))
                        } else {
                            InputOutcome::Changed
                        }
                    }
                    PickerOutcome::QueryChanged => {
                        sync_session_picker_query_expansion(
                            entries.as_deref(),
                            content_results.as_deref(),
                            entries_query.as_deref(),
                            state,
                            true,
                            *content_loading,
                            *source_filter,
                            Some(current_repo.as_str()),
                        );
                        InputOutcome::Action(Action::TriggerDeepSearch)
                    }
                    PickerOutcome::Changed => InputOutcome::Changed,
                    PickerOutcome::Unchanged => {
                        if let crossterm::event::Event::Key(key) = ev
                            && key.kind == KeyEventKind::Press
                            && crate::key!('/', CONTROL).matches(key)
                            && !state.query().trim().is_empty()
                        {
                            return InputOutcome::Action(Action::ForceDeepSearch);
                        }
                        InputOutcome::Unchanged
                    }
                    PickerOutcome::FilterCycled => {
                        InputOutcome::Action(Action::CycleSessionSourceFilter)
                    }
                    PickerOutcome::Action('d') => {
                        *pending_delete =
                            crate::views::session_picker::pending_delete_from_selection(
                                state.selected,
                                &entry_map,
                                entries.as_deref(),
                                content_results.as_deref(),
                            );
                        InputOutcome::Changed
                    }
                    PickerOutcome::NonSelectableClick(_)
                    | PickerOutcome::TabChanged(_)
                    | PickerOutcome::Action(_) => InputOutcome::Changed,
                }
            }
            _ => InputOutcome::Changed,
        }
    }

    /// Basic input handler for documentation modals (DocPicker list / DocViewer panel).
    fn handle_doc_input(&mut self, ev: &crossterm::event::Event) -> InputOutcome {
        use crate::views::modal::ActiveModal;
        use crate::views::picker::{
            PickerConfig, PickerEntry, PickerOutcome, PickerRow, handle_picker_input,
        };

        // DocPicker list: use unified picker for nav/select
        if let Some(ActiveModal::DocPicker {
            entries,
            state,
            previous_palette,
            ..
        }) = &mut self.active_modal
        {
            // Filter entries based on search query
            let filtered: Vec<_> = if state.query().is_empty() {
                entries.iter().enumerate().collect()
            } else {
                let q = state.query().to_lowercase();
                entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        e.title.to_lowercase().contains(&q)
                            || e.description.to_lowercase().contains(&q)
                    })
                    .collect()
            };
            let entry_count = filtered.len();
            let non_sel: Vec<bool> = vec![false; entry_count];
            let _picker_entries: Vec<PickerEntry> = filtered
                .iter()
                .map(|(i, e)| {
                    PickerEntry::Row(PickerRow {
                        label: &e.title,
                        right_label: &e.description,
                        selected: filtered
                            .get(state.selected)
                            .map(|(o, _)| *o)
                            .unwrap_or(usize::MAX)
                            == *i,
                        expanded: false,
                        dimmed: false,
                        indent: 0,
                        badge: "",
                        badge_color: None,
                        collapsible: false,
                        underline_last_desc: false,
                        fields: &[],
                        description_lines: &[],
                        summary_lines: &[],
                    })
                })
                .collect();
            let config = PickerConfig {
                title: Some("How-to Guides"),
                show_search_hint: false,
                expandable: false,
                esc_clears_query: true,
                shortcuts: Some(crate::views::picker::picker_shortcuts()),
                pending_hint: None,
                non_selectable: &non_sel,
                non_selectable_clickable: &[],
                shortcuts_area: None,
                tabs: None,
                active_tab: 0,
                filter_label: None,
                filter_key_hint: None,
                filter_active: false,
                header_note: None,
                action_keys: &[],
                disable_search: false,
                compact_bottom_bar: false,
                search_only_on_slash: false,
                vim_normal_first: crate::appearance::cache::load_vim_mode(),
            };
            // Handle input
            match handle_picker_input(ev, state, entry_count, &config) {
                PickerOutcome::Selected(i) => {
                    if let Some((orig_idx, _)) = filtered.get(i)
                        && let Some(doc) = entries.get(*orig_idx)
                    {
                        let title = doc.title.clone();
                        let content = doc.content.to_string();
                        // Shuttle the palette snapshot through DocViewer so it can
                        // be passed back to DocPicker when the user presses Esc.
                        let prev = previous_palette.take();
                        self.active_modal = Some(ActiveModal::DocViewer {
                            title,
                            content,
                            scroll: 0,
                            window: crate::views::modal_window::ModalWindowState::new(),
                            cached_lines: None,
                            previous_palette: prev,
                            standalone: false,
                        });
                    }
                    InputOutcome::Changed
                }
                PickerOutcome::Closed => {
                    // Restore the command palette if we have a saved snapshot
                    // (same pattern as ArgPicker / SessionPicker).
                    if let Some(snapshot) = previous_palette.take() {
                        self.active_modal = Some(ActiveModal::CommandPalette {
                            entries: snapshot.entries,
                            state: snapshot.state,
                            window: crate::views::modal_window::ModalWindowState::new(),
                        });
                    } else {
                        self.active_modal = None;
                    }
                    InputOutcome::Changed
                }
                _ => InputOutcome::Changed,
            }
        } else if let Some(ActiveModal::DocViewer { scroll, .. }) = &mut self.active_modal {
            if let Event::Key(KeyEvent { code, .. }) = ev
                && modal::apply_doc_scroll(*code, scroll)
            {
                return InputOutcome::Changed;
            }
            InputOutcome::Changed
        } else {
            InputOutcome::Changed
        }
    }
    /// Handle mouse events while a modal is active.
    ///
    /// Click on a button → same as pressing that key.
    /// Hover → update `modal_hovered_key` for highlight.
    pub(super) fn handle_modal_mouse_with_registry(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
        registry: &crate::actions::ActionRegistry,
    ) -> InputOutcome {
        use crate::views::modal::ActiveModal;
        use crate::views::modal_window::{self as mw, ModalWindowOutcome};
        use crossterm::event::MouseEventKind;

        // Picker-based modals: route through ModalWindow chrome first,
        // then delegate content events to the picker input handler.
        if matches!(
            self.active_modal,
            Some(
                ActiveModal::CommandPalette { .. }
                    | ActiveModal::ArgPicker { .. }
                    | ActiveModal::SessionPicker { .. }
                    | ActiveModal::DocPicker { .. }
                    | ActiveModal::DocViewer { .. }
                    | ActiveModal::ShortcutsHelp { .. }
                    | ActiveModal::RememberNoteReview { .. }
            )
        ) {
            // Extract window for handle_modal_mouse.
            let window = match self.active_modal.as_mut() {
                Some(ActiveModal::CommandPalette { window, .. }) => window,
                Some(ActiveModal::ArgPicker { window, .. }) => window,
                Some(ActiveModal::SessionPicker { window, .. }) => window,
                Some(ActiveModal::DocPicker { window, .. }) => window,
                Some(ActiveModal::DocViewer { window, .. }) => window,
                Some(ActiveModal::ShortcutsHelp { window, .. }) => window,
                Some(ActiveModal::RememberNoteReview { window, .. }) => window,
                _ => unreachable!(),
            };
            let outcome = mw::handle_modal_mouse(window, mouse.kind, mouse.column, mouse.row);
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    // Match keyboard Esc: step back from model effort phase
                    // before fully dismissing the ArgPicker.
                    if self.try_arg_picker_step_back_from_effort() {
                        return InputOutcome::Changed;
                    }
                    // Match keyboard Esc: a closed SessionPicker may still
                    // have a list/search fetch in flight — the dispatch
                    // layer must invalidate it (its landing surface is gone).
                    let closed_session_picker =
                        matches!(self.active_modal, Some(ActiveModal::SessionPicker { .. }));
                    // Single take() handles all modal types to avoid the
                    // double-take bug where the first consume drops the value
                    // before the second branch can match.
                    match self.active_modal.take() {
                        Some(ActiveModal::DocViewer {
                            previous_palette,
                            standalone,
                            ..
                        }) => {
                            if !standalone {
                                self.active_modal =
                                    Some(crate::views::modal::howto_list_modal(previous_palette));
                            }
                        }
                        Some(
                            ActiveModal::ArgPicker {
                                previous_palette: Some(snap),
                                ..
                            }
                            | ActiveModal::SessionPicker {
                                previous_palette: Some(snap),
                                ..
                            }
                            | ActiveModal::DocPicker {
                                previous_palette: Some(snap),
                                ..
                            },
                        ) => {
                            // Restore previous command palette from snapshot.
                            self.active_modal = Some(ActiveModal::CommandPalette {
                                entries: snap.entries,
                                state: snap.state,
                                window: crate::views::modal_window::ModalWindowState::new(),
                            });
                        }
                        _ => {
                            // No snapshot — close entirely (take() already set to None).
                        }
                    }
                    if closed_session_picker {
                        return InputOutcome::Action(Action::SessionPickerClosed);
                    }
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Handled => {
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Unhandled => {
                    // DocViewer / RememberNoteReview: wheel scrolls the markdown body.
                    if let Some(
                        ActiveModal::DocViewer { scroll, .. }
                        | ActiveModal::RememberNoteReview { scroll, .. },
                    ) = self.active_modal.as_mut()
                    {
                        if modal::apply_doc_mouse_scroll(mouse.kind, scroll) {
                            return InputOutcome::Changed;
                        }
                        return InputOutcome::Changed;
                    }
                    // Content area events — delegate to picker input.
                    if matches!(self.active_modal, Some(ActiveModal::DocPicker { .. })) {
                        let ev = crossterm::event::Event::Mouse(*mouse);
                        return self.handle_doc_input(&ev);
                    }
                    if let Some(ActiveModal::ShortcutsHelp {
                        entries,
                        state,
                        filter_active,
                        collapsed_sections,
                        expanded_ids,
                        mode,
                        ..
                    }) = &mut self.active_modal
                    {
                        use crate::views::shortcuts_help::{self, ShortcutsHelpOutcome};
                        return match shortcuts_help::handle_mouse(
                            mouse,
                            entries,
                            state,
                            *filter_active,
                            collapsed_sections,
                            mode,
                        ) {
                            ShortcutsHelpOutcome::Close => {
                                self.active_modal = None;
                                InputOutcome::Changed
                            }
                            ShortcutsHelpOutcome::ToggleFilter => {
                                *filter_active = !*filter_active;
                                state.selected = 0;
                                InputOutcome::Changed
                            }
                            ShortcutsHelpOutcome::ToggleSection(idx) => {
                                shortcuts_help::toggle_membership(collapsed_sections, idx);
                                InputOutcome::Changed
                            }
                            // Unreachable today: handle_mouse never yields ToggleExpand (a row click opens detail); kept for exhaustiveness.
                            ShortcutsHelpOutcome::ToggleExpand(action_id) => {
                                shortcuts_help::toggle_membership(expanded_ids, action_id);
                                InputOutcome::Changed
                            }
                            ShortcutsHelpOutcome::Changed => InputOutcome::Changed,
                            ShortcutsHelpOutcome::Unchanged => InputOutcome::Unchanged,
                        };
                    }
                    let ev = crossterm::event::Event::Mouse(*mouse);
                    return self.handle_palette_or_arg_input_with_registry(&ev, registry);
                }
                _ => return InputOutcome::Changed,
            }
        }

        // MemoryBrowser: route through ModalWindow chrome, then delegate.
        if let Some(ActiveModal::MemoryBrowser { state }) = &mut self.active_modal {
            let outcome =
                mw::handle_modal_mouse(&mut state.window, mouse.kind, mouse.column, mouse.row);
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Handled => return InputOutcome::Changed,
                ModalWindowOutcome::Unhandled => {
                    return crate::views::memory_modal::handle_memory_mouse(
                        state,
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                    );
                }
                _ => return InputOutcome::Changed,
            }
        }

        // Settings: route through ModalWindow chrome, then delegate.
        if let Some(ActiveModal::Settings { state }) = &mut self.active_modal {
            let outcome =
                mw::handle_modal_mouse(&mut state.window, mouse.kind, mouse.column, mouse.row);
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Handled => {
                    if matches!(mouse.kind, MouseEventKind::Moved) {
                        state.hover_row = None;
                    }
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Unhandled => {
                    let out = crate::views::settings_modal::handle_settings_mouse(
                        state,
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                    );
                    return apply_settings_outcome(self, out);
                }
                _ => return InputOutcome::Changed,
            }
        }

        // UsageInfo: chrome first (tabs / close / footer stay clickable), then drag / wheel.
        if let Some(ActiveModal::UsageInfo { state }) = &mut self.active_modal {
            use crate::views::usage_modal::{
                self, COPY_ALL_SESSION_INFO_SHORTCUT, COPY_SESSION_ID_SHORTCUT, UsageModalOutcome,
            };
            let outcome =
                mw::handle_modal_mouse(&mut state.window, mouse.kind, mouse.column, mouse.row);
            match outcome {
                ModalWindowOutcome::CloseRequested => {
                    self.active_modal = None;
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::TabChanged(idx) => {
                    state.set_tab(usage_modal::UsageInfoTab::from_index(idx));
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::ShortcutActivated(id) => {
                    // Footer click: drop gesture + hover.
                    state.clear_text_drag();
                    if id == COPY_SESSION_ID_SHORTCUT {
                        self.copy_usage_modal_session_id();
                    } else if id == COPY_ALL_SESSION_INFO_SHORTCUT {
                        let text = match self.active_modal.as_ref() {
                            Some(ActiveModal::UsageInfo { state }) => state.session_info_copy_all(),
                            _ => None,
                        };
                        if let Some(text) = text {
                            self.copy_usage_modal_text(&text);
                        }
                    }
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Handled => {
                    match mouse.kind {
                        // Same rule as content: bare Moved with an active drag is a
                        // lost Up. Pending press is left alone for click-to-copy.
                        MouseEventKind::Moved => {
                            if state.has_active_drag() {
                                return match state.finish_lost_drag() {
                                    UsageModalOutcome::CopyText(text) => {
                                        self.copy_usage_modal_text(&text);
                                        InputOutcome::Changed
                                    }
                                    _ => {
                                        state.hovered_copy_line = None;
                                        InputOutcome::Changed
                                    }
                                };
                            }
                            state.hovered_copy_line = None;
                        }
                        // Same-tab click and other chrome Downs: drop gesture + hover.
                        _ => {
                            state.clear_text_drag();
                        }
                    }
                    return InputOutcome::Changed;
                }
                ModalWindowOutcome::Unhandled => {
                    return match usage_modal::handle_usage_modal_mouse(
                        state,
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                    ) {
                        UsageModalOutcome::CopySessionId => {
                            self.copy_usage_modal_session_id();
                            InputOutcome::Changed
                        }
                        UsageModalOutcome::CopyText(text) => {
                            self.copy_usage_modal_text(&text);
                            InputOutcome::Changed
                        }
                        UsageModalOutcome::Changed => InputOutcome::Changed,
                        UsageModalOutcome::Unchanged => InputOutcome::Unchanged,
                    };
                }
                _ => return InputOutcome::Changed,
            }
        }

        // ResetSettingsConfirm: route mouse events through the
        // modal-window chrome.
        if let Some(ActiveModal::ResetSettingsConfirm { settings_state, .. }) =
            &mut self.active_modal
        {
            let outcome = mw::handle_modal_mouse(
                &mut settings_state.window,
                mouse.kind,
                mouse.column,
                mouse.row,
            );
            return match outcome {
                ModalWindowOutcome::CloseRequested => {
                    // Close-button (X) click → Cancel. Mirrors Esc /
                    // F2 / Ctrl+, keyboard semantics.
                    InputOutcome::Action(Action::ConfirmResetSetting {
                        choice: crate::views::modal::ResetSettingsResult::Cancel,
                    })
                }
                ModalWindowOutcome::ShortcutActivated(id) => {
                    use crate::views::modal::{
                        RESET_CONFIRM_NO_ID, RESET_CONFIRM_YES_ID, ResetSettingsResult,
                    };
                    let choice = if id == RESET_CONFIRM_YES_ID {
                        ResetSettingsResult::Reset
                    } else if id == RESET_CONFIRM_NO_ID {
                        ResetSettingsResult::Cancel
                    } else {
                        return InputOutcome::Changed;
                    };
                    InputOutcome::Action(Action::ConfirmResetSetting { choice })
                }
                ModalWindowOutcome::Handled => InputOutcome::Changed,
                _ => InputOutcome::Unchanged,
            };
        }

        // Standard modal mouse handling (EditConfirm).
        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                for btn in &self.modal_buttons {
                    if btn.rect.contains((mouse.column, mouse.row).into()) {
                        let key = KeyEvent::new(KeyCode::Char(btn.key), KeyModifiers::NONE);
                        return self.handle_modal_key_with_registry(&key, registry);
                    }
                }
                InputOutcome::Changed
            }
            MouseEventKind::Moved => {
                let new_hover = self
                    .modal_buttons
                    .iter()
                    .find(|btn| btn.rect.contains((mouse.column, mouse.row).into()))
                    .map(|btn| btn.key);
                if new_hover != self.modal_hovered_key {
                    self.modal_hovered_key = new_hover;
                    InputOutcome::Changed
                } else {
                    InputOutcome::Unchanged
                }
            }
            _ => InputOutcome::Changed,
        }
    }

    /// Copy the usage modal's session ID and toast the delivery outcome.
    fn copy_usage_modal_session_id(&mut self) {
        let Some(ActiveModal::UsageInfo { state }) = self.active_modal.as_ref() else {
            return;
        };
        let Some(id) = state.ctx.session_id.clone() else {
            return;
        };
        let delivery = crate::clipboard::copy_text_or_file(&id);
        self.show_toast(delivery.toast_message().as_ref());
    }

    /// Copy Session-info text (`y` / footer "copy all") and toast the
    /// delivery outcome. Mirrors [`Self::copy_usage_modal_session_id`].
    fn copy_usage_modal_text(&mut self, text: &str) {
        let delivery = crate::clipboard::copy_text_or_file(text);
        self.show_toast(delivery.toast_message().as_ref());
    }

    /// Draw the active modal overlay: the per-`ActiveModal`-variant render
    /// dispatch, called from `draw` which early-returns afterwards.
    ///
    /// `pub(crate)` so minimal mode's overlay host can reuse the exact same
    /// centered-popup rendering (hosting the command palette / shortcuts help /
    /// settings / pickers in its grown live viewport — see
    /// `crate::minimal::overlay::render_app_modal`).
    // Allow inherited from `draw`: covers the nested picker render helpers.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_active_modal(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        theme: Theme,
        compact: bool,
    ) {
        if let Some(ref mut active_modal) = self.active_modal {
            use crate::views::modal_window::{
                self as mw, ModalSizing, ModalWindowConfig, Shortcut,
            };
            use crate::views::picker::{self, PickerEntry, PickerRow};

            // Standard footer shortcuts for picker-style modals.
            let mut picker_shortcuts: Vec<Shortcut> = vec![
                Shortcut {
                    label: "\u{2191}/\u{2193} nav",
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: "Enter select",
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: "Esc close",
                    clickable: false,
                    id: 0,
                },
            ];

            // EditConfirm has no draw arm and is no longer armed anywhere (the
            // dirty pane-switch lock blocks instead) — arming it would capture
            // all input invisibly.
            if let modal::ActiveModal::CommandPalette {
                entries: _,
                state,
                window,
            } = active_modal
            {
                // Command palette: ModalWindow chrome + picker content.
                let filtered = modal::filter_palette_entries(
                    state.query(),
                    self.sharing_enabled,
                    &self.prompt.slash_controller,
                );
                let non_sel: Vec<bool> = filtered
                    .iter()
                    .map(|e| matches!(e.command, modal::PaletteCommand::SectionHeader(_)))
                    .collect();
                let picker_entries: Vec<PickerEntry> = filtered
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        if matches!(e.command, modal::PaletteCommand::SectionHeader(_)) {
                            PickerEntry::Header { label: &e.label }
                        } else {
                            PickerEntry::Row(PickerRow {
                                label: &e.label,
                                right_label: &e.shortcut,
                                selected: state.hovered == Some(i)
                                    || (state.hovered.is_none() && i == state.selected),
                                expanded: false,
                                fields: &[],
                                description_lines: &[],
                                summary_lines: &[],
                                dimmed: false,
                                indent: 0,
                                badge: "",
                                badge_color: None,
                                collapsible: false,
                                underline_last_desc: false,
                            })
                        }
                    })
                    .collect();
                let compact = self.scrollback.appearance().prompt.compact;
                // Surface `i search` in the footer when vim nav mode is active.
                mw::push_vim_nav_search_hint(&mut picker_shortcuts, state.search_active);
                let modal_config = ModalWindowConfig {
                    title: "Commands",
                    tabs: None,
                    shortcuts: &picker_shortcuts,
                    sizing: ModalSizing {
                        width_pct: 0.50,
                        max_width: 80,
                        min_width: 44,
                        v_margin: 4,
                        h_pad: 2,
                        v_pad: 1,
                        footer_lines: 2,
                    }
                    .with_compact(compact),
                    fold_info: None,
                };
                if let Some(mca) = mw::render_modal_window(buf, area, window, &modal_config, &theme)
                {
                    picker::render_picker_in_modal(
                        buf,
                        mca.content,
                        mca.inner_x,
                        mca.inner_width,
                        &theme,
                        state,
                        &picker_entries,
                        &non_sel,
                        false,
                    );
                }
            } else if let modal::ActiveModal::ArgPicker {
                command,
                args_query,
                items,
                state,
                window,
                ..
            } = active_modal
            {
                // Arg picker: ModalWindow chrome + picker content.
                let title = match command.as_str() {
                    "model" | "m" if !args_query.is_empty() => "Pick reasoning effort",
                    "model" | "m" => "Pick model",
                    "theme" | "t" => "Pick theme",
                    _ => "Pick option",
                };
                let picker_entries: Vec<PickerEntry> = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        PickerEntry::Row(PickerRow {
                            label: &item.display,
                            right_label: &item.description,
                            selected: state.hovered == Some(i)
                                || (state.hovered.is_none() && i == state.selected),
                            expanded: false,
                            fields: &[],
                            description_lines: &[],
                            summary_lines: &[],
                            dimmed: false,
                            indent: 0,
                            badge: "",
                            badge_color: None,
                            collapsible: false,
                            underline_last_desc: false,
                        })
                    })
                    .collect();
                let compact = self.scrollback.appearance().prompt.compact;
                // Surface `i search` in the footer when vim nav mode is active.
                mw::push_vim_nav_search_hint(&mut picker_shortcuts, state.search_active);
                let modal_config = ModalWindowConfig {
                    title,
                    tabs: None,
                    shortcuts: &picker_shortcuts,
                    sizing: ModalSizing {
                        width_pct: 0.50,
                        max_width: 80,
                        min_width: 44,
                        v_margin: 4,
                        h_pad: 2,
                        v_pad: 1,
                        footer_lines: 2,
                    }
                    .with_compact(compact),
                    fold_info: None,
                };
                if let Some(mca) = mw::render_modal_window(buf, area, window, &modal_config, &theme)
                {
                    picker::render_picker_in_modal(
                        buf,
                        mca.content,
                        mca.inner_x,
                        mca.inner_width,
                        &theme,
                        state,
                        &picker_entries,
                        &[],
                        false,
                    );
                }
            } else if let modal::ActiveModal::SessionPicker {
                entries,
                state,
                loading,
                lanes,
                window,
                content_results,
                content_loading,
                entries_query,
                source_filter,
                pending_delete,
                ..
            } = active_modal
            {
                // Session picker: ModalWindow chrome + picker content.
                use crate::app::app_view::filter_session_entries;
                use crate::views::picker::PickerField;
                use crate::views::session_picker::{
                    build_content_entry_data, build_content_header_label,
                };
                // While a delete confirmation is armed, the footer swaps to a
                // "y confirm / n cancel" prompt. Otherwise show the normal
                // hints plus the `d delete` action. Chat mode drops the
                // deep-search / filter / delete hints (local-disk-row actions).
                let chat_mode = self.app_chat_mode;
                let mut session_shortcuts: Vec<Shortcut> = if pending_delete.is_some() {
                    vec![
                        Shortcut {
                            label: "y confirm delete",
                            clickable: false,
                            id: 0,
                        },
                        Shortcut {
                            label: "n cancel",
                            clickable: false,
                            id: 0,
                        },
                    ]
                } else {
                    let external =
                        *source_filter == crate::views::session_picker::SourceFilter::External;
                    let mut shortcuts = vec![Shortcut {
                        label: "\u{2191}\u{2193} nav",
                        clickable: false,
                        id: 0,
                    }];
                    if !external {
                        shortcuts.extend([
                            Shortcut {
                                label: "e expand",
                                clickable: false,
                                id: 0,
                            },
                            Shortcut {
                                label: "/ search",
                                clickable: false,
                                id: 0,
                            },
                        ]);
                    }
                    if !chat_mode {
                        shortcuts.push(Shortcut {
                            label: "f filter",
                            clickable: false,
                            id: 0,
                        });
                        if !external {
                            shortcuts.push(Shortcut {
                                label: "d delete",
                                clickable: false,
                                id: 0,
                            });
                        }
                    }
                    shortcuts
                };
                // Surface `i search` in the footer when vim nav mode is active.
                if pending_delete.is_none() {
                    mw::push_vim_nav_search_hint(&mut session_shortcuts, state.search_active);
                }
                let compact = self.scrollback.appearance().prompt.compact;
                let modal_config = ModalWindowConfig {
                    title: "Resume session",
                    tabs: None,
                    shortcuts: &session_shortcuts,
                    sizing: ModalSizing {
                        width_pct: 0.65,
                        max_width: 120,
                        min_width: 48,
                        v_margin: 4,
                        h_pad: 2,
                        v_pad: 1,
                        footer_lines: 2,
                    }
                    .with_compact(compact),
                    fold_info: None,
                };
                if let Some(mca) = mw::render_modal_window(buf, area, window, &modal_config, &theme)
                {
                    let content_area = mca.content;
                    picker::render_picker_search_bar(
                        buf,
                        content_area.x,
                        content_area.y,
                        content_area.width,
                        &theme,
                        state,
                        state.search_active,
                        true,
                        Some(theme.bg_base),
                    );
                    // Render filter indicator on the search bar row (hidden in
                    // chat mode — every row is a conversation).
                    if chat_mode {
                        state.filter_area = None;
                    } else {
                        let filter_rect = picker::render_filter_indicator(
                            buf,
                            content_area.x,
                            content_area.y,
                            content_area.width,
                            &theme,
                            source_filter.label(),
                            "f",
                            source_filter.is_active(),
                            state.filter_hovered,
                        );
                        state.filter_area = Some(filter_rect);
                    }
                    // Divider — spans full inner width.
                    let sep_y = content_area.y + 1;
                    if sep_y < content_area.y + content_area.height {
                        picker::render_divider(
                            buf,
                            mca.inner_x,
                            sep_y,
                            mca.inner_width,
                            &theme,
                            Some(theme.bg_base),
                        );
                    }
                    let entries_start_y = sep_y + 1;
                    let search_bar_rect =
                        Rect::new(content_area.x, content_area.y, content_area.width, 1);

                    // Build session picker entries (shared helper). The same
                    // effective query must drive filtering AND the content
                    // header/rows gates below, or this render disagrees with
                    // the input handler's `build_entry_map` (which receives
                    // the effective query) on row indices.
                    let filter_query = crate::views::session_picker::effective_filter_query(
                        state.query(),
                        entries_query.as_deref(),
                    );
                    let entries_data = entries.as_deref().unwrap_or(&[]);
                    let filtered_indices =
                        filter_session_entries(entries.as_deref(), filter_query, *source_filter);
                    let built = crate::views::session_picker::build_session_entry_data(
                        entries_data,
                        &filtered_indices,
                        state,
                        content_area.width,
                    );
                    let fields_vecs: Vec<Vec<PickerField>> = built
                        .iter()
                        .map(|b| {
                            b.field_data
                                .iter()
                                .map(|(l, v)| PickerField { label: l, value: v })
                                .collect()
                        })
                        .collect();
                    let current_repo = crate::views::session_picker::repo_name_from_cwd(
                        &self.session.cwd.to_string_lossy(),
                    );
                    let (mut picker_entries, mut non_sel_flags) =
                        crate::views::session_picker::build_grouped_picker_entries(
                            entries_data,
                            &filtered_indices,
                            &built,
                            &fields_vecs,
                            state,
                            Some(current_repo.as_str()),
                        );

                    // Append content search result rows (same pattern as welcome).
                    let content_start = picker_entries.len() + 1;
                    let content_entry_data = if let Some(hits) = content_results.as_deref()
                        && *source_filter != crate::views::session_picker::SourceFilter::External
                        && !filter_query.is_empty()
                    {
                        build_content_entry_data(
                            hits,
                            entries_data,
                            &filtered_indices,
                            state,
                            content_start,
                        )
                    } else {
                        Vec::new()
                    };
                    let has_content_rows = !content_entry_data.is_empty();
                    let effective_content_loading = *content_loading
                        && *source_filter != crate::views::session_picker::SourceFilter::External;
                    let spinner_label = build_content_header_label(
                        effective_content_loading,
                        has_content_rows,
                        self.scrollback.tick_count(),
                    );
                    let show_content_header = has_content_rows
                        || (effective_content_loading && !filter_query.trim().is_empty());
                    if show_content_header {
                        picker_entries.push(PickerEntry::Header {
                            label: &spinner_label,
                        });
                        non_sel_flags.push(true);
                    }
                    let content_fields: Vec<Vec<PickerField>> = content_entry_data
                        .iter()
                        .map(|b| {
                            b.field_data
                                .iter()
                                .map(|(l, v)| PickerField { label: l, value: v })
                                .collect()
                        })
                        .collect();
                    let content_snippets: Vec<[&str; 1]> = content_entry_data
                        .iter()
                        .map(|b| [b.snippet_preview.as_deref().unwrap_or("")])
                        .collect();

                    for (i, (b, fields)) in content_entry_data
                        .iter()
                        .zip(content_fields.iter())
                        .enumerate()
                    {
                        let has_snippet = b.snippet_preview.is_some();
                        picker_entries.push(PickerEntry::Row(PickerRow {
                            label: &b.summary,
                            right_label: &b.right_text,
                            selected: b.is_selected,
                            expanded: b.is_expanded,
                            fields,
                            description_lines: if has_snippet {
                                &content_snippets[i]
                            } else {
                                &[]
                            },
                            summary_lines: &[],
                            dimmed: false,
                            indent: 1,
                            badge: if has_snippet { "match" } else { "" },
                            badge_color: Some(theme.accent_user),
                            collapsible: true,
                            underline_last_desc: false,
                        }));
                        non_sel_flags.push(false);
                    }

                    let hidden_hint = if chat_mode {
                        None
                    } else {
                        crate::views::session_picker::hidden_external_hint(
                            entries.as_deref(),
                            *source_filter,
                        )
                    };

                    let mut entries_area = Rect {
                        x: content_area.x,
                        y: entries_start_y,
                        width: content_area.width,
                        height: content_area
                            .height
                            .saturating_sub(entries_start_y.saturating_sub(content_area.y)),
                    };
                    // Pinned above the list so it stays visible regardless of scroll.
                    if let Some(hint) = hidden_hint.as_deref()
                        && entries_area.height > 0
                    {
                        buf.set_stringn(
                            entries_area.x + 1,
                            entries_area.y,
                            hint,
                            entries_area.width.saturating_sub(1) as usize,
                            ratatui::style::Style::default()
                                .fg(theme.gray_dim)
                                .bg(theme.bg_base),
                        );
                        entries_area.y += 1;
                        entries_area.height -= 1;
                    }
                    let content_hit = picker::render_picker_content_with_scrollbar_x(
                        buf,
                        entries_area,
                        &theme,
                        state,
                        &picker_entries,
                        &non_sel_flags,
                        &[],
                        Some(theme.bg_base),
                        crate::views::session_picker::loading_spinner_active(
                            entries.as_deref(),
                            *source_filter,
                            *loading,
                            lanes,
                        ),
                        self.scrollback.tick_count(),
                        mca.inner_x + mca.inner_width - 1,
                    );
                    state.hit_areas = Some(picker::PickerHitAreas {
                        close_button: Rect::default(),
                        search_bar: search_bar_rect,
                        item_rects: content_hit.item_rects,
                        entry_indices: content_hit.entry_indices,
                        tab_rects: vec![],
                        filter_rect: None,
                    });
                }
            } else if let modal::ActiveModal::DocPicker {
                entries,
                state,
                window,
                ..
            } = active_modal
            {
                let compact = self.scrollback.appearance().prompt.compact;
                modal::render_doc_picker_overlay(
                    buf, area, window, entries, state, compact, &theme,
                );
            } else if let modal::ActiveModal::DocViewer {
                title,
                content,
                scroll,
                window,
                cached_lines,
                ..
            } = active_modal
            {
                let compact = self.scrollback.appearance().prompt.compact;
                modal::render_doc_viewer_overlay(
                    buf,
                    area,
                    window,
                    title,
                    content,
                    scroll,
                    cached_lines,
                    compact,
                    &theme,
                );
            } else if let modal::ActiveModal::RememberNoteReview {
                ref raw_content,
                ref enhanced_content,
                showing_enhanced,
                ref mut scroll,
                ref mut window,
                ref mut cached_lines,
                ..
            } = *active_modal
            {
                use crate::views::modal_window::{self as mw, Shortcut};

                let has_enhanced = enhanced_content.is_some();
                let tab_label = if showing_enhanced {
                    "Tab raw"
                } else if has_enhanced {
                    "Tab enhanced"
                } else {
                    "enhancing\u{2026}"
                };

                let shortcuts: Vec<Shortcut> = vec![
                    Shortcut {
                        label: "\u{2191}/\u{2193} scroll",
                        clickable: false,
                        id: 0,
                    },
                    Shortcut {
                        label: "Enter save",
                        clickable: false,
                        id: 0,
                    },
                    Shortcut {
                        label: tab_label,
                        clickable: false,
                        id: 0,
                    },
                    Shortcut {
                        label: "Esc cancel",
                        clickable: false,
                        id: 0,
                    },
                ];

                let compact = self.scrollback.appearance().prompt.compact;
                let modal_config = mw::ModalWindowConfig {
                    title: "Memory Note",
                    tabs: None,
                    shortcuts: &shortcuts,
                    sizing: mw::ModalSizing {
                        width_pct: 0.65,
                        max_width: 100,
                        min_width: 40,
                        v_margin: 4,
                        h_pad: 2,
                        v_pad: 1,
                        footer_lines: 2,
                    }
                    .with_compact(compact),
                    fold_info: None,
                };

                if let Some(mw::ModalContentArea {
                    content: content_area,
                    ..
                }) = mw::render_modal_window(buf, area, window, &modal_config, &theme)
                {
                    let display_content = if showing_enhanced {
                        enhanced_content.as_deref().unwrap_or(raw_content)
                    } else {
                        raw_content
                    };

                    let w = content_area.width;
                    let needs_reparse = cached_lines
                        .as_ref()
                        .is_none_or(|(cached_w, _)| *cached_w != w);
                    if needs_reparse {
                        let mc = crate::scrollback::blocks::markdown_content::MarkdownContent::new(
                            display_content.to_string(),
                        );
                        let output = mc.output(w as usize);
                        let lines: Vec<ratatui::text::Line<'static>> =
                            output.lines.into_iter().map(|b| b.content).collect();
                        *cached_lines = Some((w, lines));
                    }
                    let all_lines = &cached_lines.as_ref().unwrap().1;
                    let max_scroll = all_lines.len().saturating_sub(content_area.height as usize);
                    *scroll = (*scroll as usize).min(max_scroll) as u16;
                    let start = *scroll as usize;
                    let visible: Vec<Line> = all_lines
                        .iter()
                        .skip(start)
                        .take(content_area.height as usize)
                        .cloned()
                        .collect();
                    let para = ratatui::widgets::Paragraph::new(visible)
                        .wrap(ratatui::widgets::Wrap { trim: false });
                    para.render(content_area, buf);
                }
            } else if let modal::ActiveModal::ShortcutsHelp {
                entries,
                state,
                window,
                filter_active,
                collapsed_sections,
                expanded_ids,
                mode,
            } = active_modal
            {
                use crate::views::shortcuts_help;
                // Detail screen reuses the same modal chrome with a different footer (pattern B).
                if mode.is_detail() {
                    shortcuts_help::render_detail(buf, area, window, mode, &theme, compact);
                    return;
                }
                let rows = shortcuts_help::CheatsheetRows::build(
                    entries,
                    state.query(),
                    *filter_active,
                    collapsed_sections,
                );
                let help_refs = rows.help_refs();
                let picker_entries = rows.picker_entries(state, expanded_ids, &help_refs);
                let non_sel: Vec<bool> = vec![false; picker_entries.len()];
                let footer = shortcuts_help::modal_footer(*filter_active);
                let modal_config = mw::ModalWindowConfig {
                    title: "Keyboard Shortcuts",
                    tabs: None,
                    shortcuts: &footer,
                    sizing: shortcuts_help::modal_sizing(compact),
                    fold_info: None,
                };
                if let Some(mca) = mw::render_modal_window(buf, area, window, &modal_config, &theme)
                {
                    let searching = state.search_active || !state.query().is_empty();
                    picker::render_picker_in_modal_inner(
                        buf,
                        mca.content,
                        mca.inner_x,
                        mca.inner_width,
                        &theme,
                        state,
                        &picker_entries,
                        &non_sel,
                        false,
                        searching,
                        !searching,
                    );
                }
            } else if let modal::ActiveModal::UsageInfo { state } = active_modal {
                crate::views::usage_modal::render_usage_modal(
                    buf,
                    area,
                    state,
                    self.credit_balance.as_ref(),
                    compact,
                    &theme,
                );
            } else if let modal::ActiveModal::MemoryBrowser { state: mem_state } = active_modal {
                crate::views::memory_modal::render_memory_modal(buf, area, mem_state, compact);
            } else if let modal::ActiveModal::Settings {
                state: settings_state,
            } = active_modal
            {
                crate::views::settings_modal::render_settings_modal(
                    buf,
                    area,
                    settings_state,
                    compact,
                    None,
                );
            } else if matches!(
                active_modal,
                modal::ActiveModal::ResetSettingsConfirm { .. }
            ) {
                // Render settings modal with reset-confirm overlay.
                let prompt = crate::views::modal::reset_confirm_prompt(active_modal)
                    .unwrap_or_else(|| "Reset setting to default?".to_owned());
                let breadcrumb = crate::views::modal::reset_confirm_breadcrumb(active_modal)
                    .unwrap_or_else(|| "Reset setting".to_owned());
                if let modal::ActiveModal::ResetSettingsConfirm { settings_state, .. } =
                    active_modal
                {
                    let overlay = crate::views::settings_modal::ResetConfirmOverlay {
                        prompt: &prompt,
                        breadcrumb_suffix: &breadcrumb,
                    };
                    crate::views::settings_modal::render_settings_modal(
                        buf,
                        area,
                        settings_state,
                        compact,
                        Some(&overlay),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod session_picker_delete_tests {
    use crate::app::actions::Action;
    use crate::app::agent_view::AgentView;
    use crate::app::agent_view::test_fixtures::make_agent;
    use crate::app::app_view::{InputOutcome, SessionPickerEntry};
    use crate::views::modal::ActiveModal;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

    fn entry(id: &str) -> SessionPickerEntry {
        SessionPickerEntry {
            id: id.into(),
            summary: id.into(),
            updated_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            cwd: "/repo".into(),
            hostname: None,
            source: "local".into(),
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

    fn open_picker(agent: &mut AgentView, entries: Vec<SessionPickerEntry>) {
        agent.active_modal = Some(ActiveModal::SessionPicker {
            state: crate::views::picker::PickerState::default(),
            entries: Some(entries),
            loading: false,
            lanes: Default::default(),
            previous_palette: None,
            window: crate::views::modal_window::ModalWindowState::new(),
            content_results: None,
            content_loading: false,
            deep_search_seq: 0,
            entries_query: None,
            source_filter: crate::views::session_picker::SourceFilter::default(),
            pending_delete: None,
        });
    }

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn pending(agent: &AgentView) -> Option<String> {
        match agent.active_modal.as_ref() {
            Some(ActiveModal::SessionPicker { pending_delete, .. }) => {
                pending_delete.as_ref().map(|pd| pd.session_id.clone())
            }
            _ => None,
        }
    }

    #[test]
    fn d_arms_then_y_confirms_delete() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0"), entry("s1")]);

        // `d` arms the confirmation on the first selectable row (s0).
        let out = agent.handle_palette_or_arg_input(&key('d'));
        assert!(matches!(out, InputOutcome::Changed));
        assert_eq!(pending(&agent).as_deref(), Some("s0"));

        // `y` confirms — fires DeleteSession for the armed session.
        let out = agent.handle_palette_or_arg_input(&key('y'));
        assert!(
            matches!(
                out,
                InputOutcome::Action(Action::DeleteSession {
                    ref source,
                    ref session_id,
                    ref cwd,
                }) if source == "local" && session_id == "s0" && cwd == "/repo"
            ),
            "y must confirm deletion of the armed session"
        );
        assert!(pending(&agent).is_none(), "pending cleared after confirm");
    }

    #[test]
    fn d_arms_then_n_cancels() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0")]);

        agent.handle_palette_or_arg_input(&key('d'));
        assert_eq!(pending(&agent).as_deref(), Some("s0"));

        let out = agent.handle_palette_or_arg_input(&key('n'));
        assert!(matches!(out, InputOutcome::Changed));
        assert!(pending(&agent).is_none(), "n cancels the confirmation");
    }

    #[test]
    fn other_key_cancels_pending_delete() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0"), entry("s1")]);

        agent.handle_palette_or_arg_input(&key('d'));
        assert_eq!(pending(&agent).as_deref(), Some("s0"));

        // A navigation key cancels the armed confirmation.
        agent.handle_palette_or_arg_input(&key('j'));
        assert!(
            pending(&agent).is_none(),
            "any non-y/d key cancels the pending delete"
        );
    }

    #[test]
    fn mouse_move_keeps_pending_delete() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0"), entry("s1")]);
        agent.handle_palette_or_arg_input(&key('d'));
        agent.handle_palette_or_arg_input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(pending(&agent).as_deref(), Some("s0"));
    }

    #[test]
    fn y_without_armed_confirmation_does_not_delete() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0")]);

        // No `d` first — `y` is the copy hotkey, never a delete.
        let out = agent.handle_palette_or_arg_input(&key('y'));
        assert!(
            !matches!(out, InputOutcome::Action(Action::DeleteSession { .. })),
            "y alone must not delete"
        );
        assert!(pending(&agent).is_none());
    }

    /// Plain close (Esc) must surface `SessionPickerClosed` so the dispatch
    /// layer can invalidate an in-flight list/search fetch — its landing
    /// surface (the modal) is gone.
    #[test]
    fn esc_close_emits_session_picker_closed_action() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0")]);
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let out = agent.handle_palette_or_arg_input(&esc);
        assert!(
            matches!(out, InputOutcome::Action(Action::SessionPickerClosed)),
            "close must emit the fetch-invalidation action, got {out:?}"
        );
        assert!(agent.active_modal.is_none(), "modal cleared on close");
    }

    /// Chat-mode picker is conversations-only: `d` (local delete) must not
    /// arm a confirmation and `f` must not cycle the hidden source filter.
    #[test]
    fn chat_mode_disables_delete_and_filter_keys() {
        let mut agent = make_agent();
        agent.app_chat_mode = true;
        open_picker(&mut agent, vec![entry("c0"), entry("c1")]);

        agent.handle_palette_or_arg_input(&key('d'));
        assert!(
            pending(&agent).is_none(),
            "d must not arm delete under chat mode"
        );

        agent.handle_palette_or_arg_input(&key('f'));
        let filter = match agent.active_modal.as_ref() {
            Some(ActiveModal::SessionPicker { source_filter, .. }) => *source_filter,
            _ => panic!("expected open session picker"),
        };
        assert_eq!(
            filter,
            crate::views::session_picker::SourceFilter::EchoAgent,
            "f must not cycle the hidden source filter under chat mode"
        );
    }

    #[test]
    fn ctrl_w_resumes_session_while_search_is_focused() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0")]);
        if let Some(ActiveModal::SessionPicker { state, .. }) = agent.active_modal.as_mut() {
            state.selected = 1;
            state.search_active = true;
            state.set_query("s");
        }

        let worktree = Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let outcome = agent.handle_palette_or_arg_input(&worktree);
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::PickSessionInWorktree(0))
        ));
    }

    #[test]
    fn foreign_row_refuses_delete_detail_and_worktree_actions() {
        let mut agent = make_agent();
        let mut foreign = entry("codex-session");
        foreign.source = "codex".into();
        open_picker(&mut agent, vec![foreign]);
        // Pin All: the refusals only fire when the foreign row is focusable.
        if let Some(ActiveModal::SessionPicker { source_filter, .. }) = agent.active_modal.as_mut()
        {
            *source_filter = crate::views::session_picker::SourceFilter::All;
        }

        let delete = agent.handle_palette_or_arg_input(&key('d'));
        assert!(matches!(delete, InputOutcome::Changed));
        assert!(pending(&agent).is_none(), "foreign delete must not arm");

        let expand = agent.handle_palette_or_arg_input(&key('e'));
        assert!(
            !matches!(
                expand,
                InputOutcome::Action(Action::ExpandSessionCard { .. })
            ),
            "foreign rows have no transcript detail"
        );

        let worktree = Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let outcome = agent.handle_palette_or_arg_input(&worktree);
        assert!(
            !matches!(
                outcome,
                InputOutcome::Action(Action::PickSessionInWorktree(_))
            ),
            "foreign rows cannot be resumed in worktrees"
        );
        assert!(
            agent.active_modal.is_some(),
            "refused actions keep picker open"
        );
    }

    /// A server search matches conversation *content* too: a hit whose title
    /// doesn't fuzzy-match the query must stay pickable in the modal
    /// (`effective_filter_query` skips the local re-filter).
    #[test]
    fn server_search_hit_with_unrelated_title_is_pickable() {
        let mut agent = make_agent();
        agent.app_chat_mode = true;
        let mut e = entry("conv-content-1");
        e.summary = "Quarterly roadmap notes".into(); // no "hit" in the title
        e.source = "conversation".into();
        open_picker(&mut agent, vec![e]);
        if let Some(ActiveModal::SessionPicker {
            state,
            entries_query,
            content_loading,
            ..
        }) = agent.active_modal.as_mut()
        {
            state.set_query("hit");
            *entries_query = Some("hit".into());
            // A re-search of the stamped query may be in flight: with the
            // effective query empty, the input map appends NO "Searching…"
            // header (same gate the renders use), so indices don't shift.
            *content_loading = true;
            // Grouped map: [repo header, row] — the row sits at index 1.
            state.selected = 1;
        }
        let out = agent.handle_palette_or_arg_input(&key_code(KeyCode::Enter));
        assert!(
            matches!(out, InputOutcome::Action(Action::PickSession(0))),
            "content-only search hit must be pickable, got {out:?}"
        );
    }

    /// Canary: entries WITHOUT a matching fetch-query stamp keep the local
    /// fuzzy filter — an unrelated title stays hidden from Enter.
    #[test]
    fn unstamped_entries_keep_local_fuzzy_filter() {
        let mut agent = make_agent();
        agent.app_chat_mode = true;
        let mut e = entry("conv-content-1");
        e.summary = "Quarterly roadmap notes".into();
        e.source = "conversation".into();
        open_picker(&mut agent, vec![e]);
        if let Some(ActiveModal::SessionPicker { state, .. }) = agent.active_modal.as_mut() {
            state.set_query("hit");
            state.selected = 1;
        }
        let out = agent.handle_palette_or_arg_input(&key_code(KeyCode::Enter));
        assert!(
            !matches!(out, InputOutcome::Action(Action::PickSession(_))),
            "unstamped entries must still be fuzzy-filtered, got {out:?}"
        );
    }

    fn key_code(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Borrow the picker state of the open session picker for assertions.
    fn picker_state(agent: &AgentView) -> &crate::views::picker::PickerState {
        match agent.active_modal.as_ref() {
            Some(ActiveModal::SessionPicker { state, .. }) => state,
            _ => panic!("expected open session picker"),
        }
    }

    #[test]
    fn up_at_top_focuses_search_and_clears_selection() {
        // Pin vim-mode off; this test asserts the non-vim picker path.
        crate::appearance::cache::set_vim_mode(false);
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0"), entry("s1")]);

        // Up from the first row moves focus to the search bar and hides the
        // list selection highlight (the row should no longer look selected).
        agent.handle_palette_or_arg_input(&key_code(KeyCode::Up));
        let st = picker_state(&agent);
        assert!(st.search_active, "search bar takes focus");
        assert!(st.selection_hidden, "list selection highlight is cleared");

        // Down from the search bar returns focus to the list and restores
        // the highlight.
        agent.handle_palette_or_arg_input(&key_code(KeyCode::Down));
        let st = picker_state(&agent);
        assert!(!st.search_active, "focus returns to the list");
        assert!(!st.selection_hidden, "selection highlight is restored");
    }

    #[test]
    fn down_at_bottom_focuses_search_and_clears_selection() {
        // Pin vim-mode off; this test asserts the non-vim picker path.
        crate::appearance::cache::set_vim_mode(false);
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0"), entry("s1")]);

        // Move to the last selectable row, then Down again to reach search.
        agent.handle_palette_or_arg_input(&key_code(KeyCode::Down));
        agent.handle_palette_or_arg_input(&key_code(KeyCode::Down));
        let st = picker_state(&agent);
        assert!(st.search_active, "search bar takes focus from the bottom");
        assert!(st.selection_hidden, "list selection highlight is cleared");
    }

    #[test]
    fn typing_a_query_restores_selection() {
        // Pin vim-mode off; this test asserts the non-vim picker path.
        crate::appearance::cache::set_vim_mode(false);
        let mut agent = make_agent();
        open_picker(&mut agent, vec![entry("s0"), entry("s1")]);

        // Arrow into the search bar (selection hidden), then type — a query
        // makes the top match meaningful again, so the highlight returns.
        agent.handle_palette_or_arg_input(&key_code(KeyCode::Up));
        assert!(picker_state(&agent).selection_hidden);

        agent.handle_palette_or_arg_input(&key('s'));
        let st = picker_state(&agent);
        assert!(st.search_active, "still typing in the search bar");
        assert!(
            !st.selection_hidden,
            "typing a query restores the selection highlight"
        );
    }

    /// Paste garbage + Enter with no rows must not LoadSession.
    #[test]
    fn enter_with_garbage_query_does_not_load_session() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![]);
        if let Some(ActiveModal::SessionPicker { state, .. }) = agent.active_modal.as_mut() {
            state.set_query("this is pasted garbage!!!");
        }
        let out = agent.handle_palette_or_arg_input(&key_code(KeyCode::Enter));
        assert!(
            matches!(out, InputOutcome::Unchanged),
            "garbage query must be a no-op, got {out:?}"
        );
        assert!(
            matches!(agent.active_modal, Some(ActiveModal::SessionPicker { .. })),
            "picker must stay open"
        );
    }

    #[test]
    fn enter_with_uuid_query_loads_session() {
        let mut agent = make_agent();
        open_picker(&mut agent, vec![]);
        let sid = "019fb61a-85a5-7ba0-a4ec-24647dca1893";
        if let Some(ActiveModal::SessionPicker { state, .. }) = agent.active_modal.as_mut() {
            state.set_query(sid);
        }
        let out = agent.handle_palette_or_arg_input(&key_code(KeyCode::Enter));
        assert!(
            matches!(
                out,
                InputOutcome::Action(Action::LoadSession(ref id, None, false)) if id == sid
            ),
            "UUID query should direct-load, got {out:?}"
        );
    }
}

#[cfg(test)]
mod command_palette_vim_input_tests {
    use crate::actions::ActionRegistry;
    use crate::app::agent_view::AgentView;
    use crate::app::agent_view::test_fixtures::make_agent;
    use crate::app::app_view::InputOutcome;
    use crate::views::modal::ActiveModal;
    use crate::views::picker::PickerState;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    // Open the command palette exactly as the Ctrl+P handler does: type-to-find
    // INPUT mode (`input_active`) over the full palette entries.
    fn open_command_palette(agent: &mut AgentView) {
        agent.active_modal = Some(ActiveModal::CommandPalette {
            entries: crate::views::modal::default_palette_entries(
                agent.sharing_enabled,
                &agent.prompt.slash_controller,
            ),
            state: PickerState::input_active(),
            window: crate::views::modal_window::ModalWindowState::new(),
        });
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn esc() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    // Borrow the open command palette's picker state for assertions.
    fn palette_state(agent: &AgentView) -> &PickerState {
        match agent.active_modal.as_ref() {
            Some(ActiveModal::CommandPalette { state, .. }) => state,
            _ => panic!("expected open command palette"),
        }
    }

    #[test]
    fn minimal_palette_shortcuts_uses_live_configured_registry() {
        let mut agent = make_agent();
        agent
            .prompt
            .set_screen_mode(crate::app::ScreenMode::Minimal);
        agent.active_modal = Some(ActiveModal::CommandPalette {
            entries: crate::views::modal::default_palette_entries(
                agent.sharing_enabled,
                &agent.prompt.slash_controller,
            ),
            state: {
                let mut state = PickerState::input_active();
                state.set_query("keyboard shortcuts");
                state.selected = 1; // matching section header is row 0
                state
            },
            window: crate::views::modal_window::ModalWindowState::new(),
        });
        // Start from the real minimal set, then inject the existing config-gated
        // action in a supported context. This pins that modal dispatch preserves
        // the exact live registry rather than reconstructing any defaults.
        let mut actions =
            crate::actions::ActionRegistry::defaults_for(crate::app::ScreenMode::Minimal)
                .all()
                .to_vec();
        let mut config_gated = crate::actions::ActionRegistry::defaults_with_config(true)
            .find(crate::actions::ActionId::ToggleMouseCapture)
            .expect("config-gated action")
            .clone();
        config_gated.context = crate::actions::When::AgentScreen;
        actions.push(config_gated);
        let registry = crate::actions::ActionRegistry::new(actions);
        let out = agent.handle_modal_key_with_registry(
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &registry,
        );
        assert!(matches!(out, InputOutcome::Changed));

        let Some(ActiveModal::ShortcutsHelp { entries, .. }) = &agent.active_modal else {
            panic!("expected shortcuts help modal");
        };
        let action_ids: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                crate::views::shortcuts_help::ShortcutsHelpEntry::Hint {
                    action_id: Some(id),
                    ..
                } => Some(*id),
                _ => None,
            })
            .collect();
        assert!(action_ids.contains(&crate::actions::ActionId::EditPromptExternal));
        assert!(!action_ids.contains(&crate::actions::ActionId::ToggleTasks));
        assert!(action_ids.contains(&crate::actions::ActionId::ToggleMouseCapture));
        assert!(!action_ids.contains(&crate::actions::ActionId::OpenDashboard));
    }

    #[test]
    fn minimal_edit_prompt_palette_selection_preserves_draft() {
        let mut agent = make_agent();
        agent
            .prompt
            .set_screen_mode(crate::app::ScreenMode::Minimal);
        agent.prompt.set_text("keep this draft");
        agent.active_modal = Some(ActiveModal::CommandPalette {
            entries: crate::views::modal::default_palette_entries(
                agent.sharing_enabled,
                &agent.prompt.slash_controller,
            ),
            state: {
                let mut state = PickerState::input_active();
                // Contiguous substring of the label ("Edit Prompt in External Editor").
                state.set_query("external editor");
                state.selected = 1; // matching section header is row 0
                state
            },
            window: crate::views::modal_window::ModalWindowState::new(),
        });
        let out = agent.handle_modal_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            out,
            InputOutcome::Action(crate::app::actions::Action::EditPromptExternal)
        ));
        assert_eq!(agent.prompt.text(), "keep this draft");
        assert!(agent.active_modal.is_none());
    }

    /// Headline command-palette vim flow — a CI-runnable mirror of the ignored
    /// PTY scenario `vim_modal_command_palette.yaml`. Drives the real modal seam
    /// (`handle_modal_key`) so both the chrome Esc handling and the picker's
    /// `vim_normal_first: load_vim_mode()` wiring are exercised end to end.
    #[test]
    fn vim_command_palette_input_then_esc_to_nav_then_i_reenters() {
        // CI defaults vim off and this dev machine's config sets it on, so pin.
        crate::appearance::cache::set_vim_mode(true);
        let mut agent = make_agent();
        open_command_palette(&mut agent);

        // Opens in INPUT mode: a letter types/filters immediately.
        assert!(palette_state(&agent).search_active, "opens in input mode");
        agent.handle_modal_key(&key('a'));
        let st = palette_state(&agent);
        assert_eq!(st.query(), "a", "input mode: a letter filters");
        assert!(st.search_active);

        // First Esc clears the query via the modal chrome but stays in input.
        agent.handle_modal_key(&esc());
        let st = palette_state(&agent);
        assert!(st.query().is_empty(), "Esc clears the query");
        assert!(st.search_active, "still input after the first Esc");

        // Second Esc (empty query) drops to NAV via the picker's vim Esc.
        agent.handle_modal_key(&esc());
        let st = palette_state(&agent);
        assert!(!st.search_active, "second Esc drops to nav");
        assert!(st.query().is_empty());

        // NAV: a bare printable key must NOT type.
        let out = agent.handle_modal_key(&key('b'));
        let st = palette_state(&agent);
        assert!(st.query().is_empty(), "nav: a bare letter does not filter");
        assert!(!st.search_active);
        assert!(
            matches!(out, InputOutcome::Unchanged),
            "nav letter is inert"
        );

        // `i` re-enters INPUT without typing; a letter then filters again.
        agent.handle_modal_key(&key('i'));
        assert!(palette_state(&agent).search_active, "i re-enters search");
        assert!(palette_state(&agent).query().is_empty(), "i does not type");
        agent.handle_modal_key(&key('c'));
        assert_eq!(palette_state(&agent).query(), "c", "typing filters again");
        // Reset the global vim pin so it can't leak to later tests (libtest reuses threads).
        crate::appearance::cache::set_vim_mode(false);
    }

    /// `/` is the other vim search-entry key: from NAV it re-enters INPUT.
    #[test]
    fn vim_command_palette_slash_reenters_search_from_nav() {
        crate::appearance::cache::set_vim_mode(true);
        let mut agent = make_agent();
        open_command_palette(&mut agent);

        // Drop to nav: type, then two Escs (clear query, then nav).
        agent.handle_modal_key(&key('a'));
        agent.handle_modal_key(&esc());
        agent.handle_modal_key(&esc());
        assert!(!palette_state(&agent).search_active, "in nav mode");

        agent.handle_modal_key(&key('/'));
        assert!(palette_state(&agent).search_active, "/ re-enters search");
        assert!(palette_state(&agent).query().is_empty(), "/ does not type");
        // Reset the global vim pin so it can't leak to later tests (libtest reuses threads).
        crate::appearance::cache::set_vim_mode(false);
    }

    /// Vim OFF: the command palette stays type-to-filter — there is no nav mode,
    /// so a letter keeps filtering even after Esc clears the query.
    #[test]
    fn non_vim_command_palette_stays_type_to_filter() {
        crate::appearance::cache::set_vim_mode(false);
        let mut agent = make_agent();
        open_command_palette(&mut agent);

        agent.handle_modal_key(&key('a'));
        let st = palette_state(&agent);
        assert_eq!(st.query(), "a", "a letter filters");
        assert!(st.search_active);

        // Esc clears the query (chrome) but never drops to a nav mode.
        agent.handle_modal_key(&esc());
        assert!(
            palette_state(&agent).query().is_empty(),
            "Esc clears the query"
        );

        // A bare letter still types — no vim nav-mode suppression.
        agent.handle_modal_key(&key('b'));
        let st = palette_state(&agent);
        assert_eq!(st.query(), "b", "still type-to-filter (no nav mode)");
        assert!(st.search_active);
    }

    #[test]
    fn command_palette_bracketed_paste_targets_only_active_query() {
        crate::appearance::cache::set_vim_mode(false);
        let mut agent = make_agent();
        agent.prompt.set_text("hidden prompt");
        open_command_palette(&mut agent);
        if let Some(ActiveModal::CommandPalette { state, .. }) = agent.active_modal.as_mut() {
            state.set_query("ab");
        }
        let registry = ActionRegistry::defaults();
        let _ = agent.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            &registry,
        );
        let outcome = agent.handle_input(&Event::Paste("中\r\n".to_owned()), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(palette_state(&agent).query(), "a中b");
        assert_eq!(agent.prompt.text(), "hidden prompt");

        if let Some(ActiveModal::CommandPalette { state, .. }) = agent.active_modal.as_mut() {
            state.set_query("");
            state.search_active = false;
        }
        crate::appearance::cache::set_vim_mode(true);
        let outcome = agent.handle_input(&Event::Paste("ignored".to_owned()), &registry);
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert!(palette_state(&agent).query().is_empty());
        assert_eq!(agent.prompt.text(), "hidden prompt");
        crate::appearance::cache::set_vim_mode(false);
    }

    // Drives the REAL command-palette render seam (draw_active_modal →
    // picker::render_picker_in_modal → render_search_bar)
    // — the path the bug was on — and asserts the cursor tracks focus.
    #[test]
    fn command_palette_search_bar_cursor_only_when_focused() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let render_palette_search_row = |search_active: bool| -> (bool, String) {
            let mut agent = make_agent();
            open_command_palette(&mut agent);
            if let Some(ActiveModal::CommandPalette { state, .. }) = agent.active_modal.as_mut() {
                state.search_active = search_active;
            }
            let area = Rect::new(0, 0, 80, 24);
            let mut buf = Buffer::empty(area);
            agent.draw_active_modal(area, &mut buf, crate::theme::Theme::current(), false);

            let theme = crate::theme::Theme::current();
            let search_bar = match agent.active_modal.as_ref() {
                Some(ActiveModal::CommandPalette { state, .. }) => {
                    state
                        .hit_areas
                        .as_ref()
                        .expect("render sets hit_areas")
                        .search_bar
                }
                _ => panic!("expected an open command palette"),
            };
            let y = search_bar.y;
            let mut has_cursor = false;
            let mut text = String::new();
            for x in search_bar.x..search_bar.x + search_bar.width {
                if let Some(cell) = buf.cell((x, y)) {
                    text.push_str(cell.symbol());
                    // The cursor is an inverse-video cell (bg == text_primary).
                    if cell.bg == theme.text_primary {
                        has_cursor = true;
                    }
                }
            }
            (has_cursor, text)
        };

        let (focused_cursor, _) = render_palette_search_row(true);
        assert!(
            focused_cursor,
            "command palette search bar should render a cursor when search_active",
        );

        let (unfocused_cursor, unfocused_text) = render_palette_search_row(false);
        assert!(
            !unfocused_cursor,
            "command palette search bar must not render a cursor when not search_active",
        );
        assert!(
            unfocused_text.contains("/ to search"),
            "unfocused command palette should show the `/ to search` placeholder, got {unfocused_text:?}",
        );
    }
}

#[cfg(test)]
mod settings_memory_paste_routing_tests {
    use std::sync::Arc;

    use crate::actions::ActionRegistry;
    use crate::app::agent_view::test_fixtures::make_agent;
    use crate::app::app_view::InputOutcome;
    use crate::settings::{PagerLocalSnapshot, SettingsRegistry};
    use crate::views::memory_modal::{MemoryModalMode, MemoryModalState};
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalState;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use echo_agent_runtime::agent::config::UiConfig;

    fn left() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
    }

    #[test]
    fn settings_and_memory_paste_only_into_focused_filters() {
        let registry = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.prompt.set_text("hidden prompt");

        let mut settings = SettingsModalState::new(
            Arc::new(SettingsRegistry::defaults()),
            UiConfig::default(),
            PagerLocalSnapshot::default(),
        );
        settings.focus_filter();
        settings.set_query("ab");
        agent.active_modal = Some(ActiveModal::Settings {
            state: Box::new(settings),
        });
        let _ = agent.handle_input(&left(), &registry);
        let outcome = agent.handle_input(&Event::Paste("中\r\n".to_owned()), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        let Some(ActiveModal::Settings { state }) = agent.active_modal.as_ref() else {
            panic!("settings modal remains open");
        };
        assert_eq!(state.query(), "a中b");
        assert_eq!(agent.prompt.text(), "hidden prompt");

        let mut memory = MemoryModalState::new(Vec::new());
        memory.mode = MemoryModalMode::FilterFocused;
        agent.active_modal = Some(ActiveModal::MemoryBrowser {
            state: Box::new(memory),
        });
        let _ = agent.handle_input(&Event::Paste("ab".to_owned()), &registry);
        let _ = agent.handle_input(&left(), &registry);
        let outcome = agent.handle_input(&Event::Paste("中\r\n".to_owned()), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        let Some(ActiveModal::MemoryBrowser { state }) = agent.active_modal.as_ref() else {
            panic!("memory modal remains open");
        };
        assert_eq!(state.query(), "a中b");
        assert_eq!(agent.prompt.text(), "hidden prompt");
    }
}
