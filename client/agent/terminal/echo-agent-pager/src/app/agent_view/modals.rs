//! Modal input handlers: agents/persona modals and the extensions modal
//! (hooks, plugins, marketplace, skills, MCP servers) with its actions.

use super::AgentView;
#[cfg(test)]
use super::test_fixtures;
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::views::file_search::line_viewer::LineViewerState;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl AgentView {
    // -- Agents modal input handling --

    pub(super) fn handle_agents_modal_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
    ) -> InputOutcome {
        let Some(ref mut state) = self.agents_modal else {
            return InputOutcome::Unchanged;
        };
        match crate::views::agents_modal::handle_agents_key(state, key) {
            crate::views::agents_modal::AgentsModalOutcome::Close => {
                self.agents_modal = None;
                InputOutcome::Changed
            }
            crate::views::agents_modal::AgentsModalOutcome::ViewAgent {
                title,
                source_path,
                content,
            } => {
                // Open the agent definition in the line viewer on top of the
                // agents modal.  The line viewer has higher input priority
                // (0a1) so it takes focus; when the user presses Esc the
                // viewer closes and the agents modal is still there.
                let viewer = if let Some(ref path) = source_path {
                    LineViewerState::open_markdown(path, None)
                } else if let Some(content) = content {
                    LineViewerState::open_markdown_content(&title, content, None)
                } else {
                    None
                };
                if let Some(mut v) = viewer {
                    v.title_override = Some(title);
                    self.line_viewer = Some(v);
                }
                InputOutcome::Changed
            }
            crate::views::agents_modal::AgentsModalOutcome::OpenPersonaDetail {
                name,
                source_path,
                editable,
                scope_label,
            } => {
                use crate::views::persona_detail::PersonaDetailState;
                let detail = if let Some(ref path) = source_path {
                    PersonaDetailState::from_toml_file(path, editable, &scope_label)
                } else {
                    Some(PersonaDetailState::from_name_only(&name))
                };
                if detail.is_none()
                    && let Some(ref mut modal) = self.agents_modal
                {
                    modal.message = Some(crate::views::agents_modal::AgentsModalMessage::error(
                        format!("Failed to load persona '{name}'"),
                    ));
                }
                self.persona_detail = detail;
                InputOutcome::Changed
            }
            crate::views::agents_modal::AgentsModalOutcome::EditInEditor { path, tab } => {
                InputOutcome::Action(Action::SuspendForEditor {
                    path,
                    refresh_agents_modal: Some(tab),
                })
            }
            crate::views::agents_modal::AgentsModalOutcome::Changed => InputOutcome::Changed,
            crate::views::agents_modal::AgentsModalOutcome::Unchanged => InputOutcome::Unchanged,
        }
    }

    pub(super) fn handle_agents_modal_paste(&mut self, text: &str) -> InputOutcome {
        let Some(ref mut state) = self.agents_modal else {
            return InputOutcome::Unchanged;
        };
        match crate::views::agents_modal::handle_agents_paste(state, text) {
            crate::views::agents_modal::AgentsModalOutcome::Changed => InputOutcome::Changed,
            _ => InputOutcome::Unchanged,
        }
    }

    pub(super) fn handle_agents_modal_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
    ) -> InputOutcome {
        let Some(ref mut state) = self.agents_modal else {
            return InputOutcome::Unchanged;
        };
        match crate::views::agents_modal::handle_agents_mouse(state, mouse) {
            crate::views::agents_modal::AgentsModalOutcome::Close => {
                self.agents_modal = None;
                InputOutcome::Changed
            }
            crate::views::agents_modal::AgentsModalOutcome::ViewAgent { .. }
            | crate::views::agents_modal::AgentsModalOutcome::OpenPersonaDetail { .. }
            | crate::views::agents_modal::AgentsModalOutcome::EditInEditor { .. } => {
                // Mouse interactions don't trigger view/edit — ignore.
                InputOutcome::Unchanged
            }
            crate::views::agents_modal::AgentsModalOutcome::Changed => InputOutcome::Changed,
            crate::views::agents_modal::AgentsModalOutcome::Unchanged => InputOutcome::Unchanged,
        }
    }

    // -- Persona detail modal input handling --

    pub(super) fn handle_persona_detail_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
    ) -> InputOutcome {
        let Some(ref mut detail) = self.persona_detail else {
            return InputOutcome::Unchanged;
        };
        use crate::views::persona_detail::{PersonaDetailOutcome, handle_persona_detail_key};
        match handle_persona_detail_key(detail, key) {
            PersonaDetailOutcome::Close => {
                self.persona_detail = None;
                // Refresh the personas list in case edits were made.
                if let Some(ref mut modal) = self.agents_modal {
                    modal.refresh_personas();
                }
                InputOutcome::Changed
            }
            PersonaDetailOutcome::EditInEditor { path } => {
                self.persona_detail = None;
                InputOutcome::Action(Action::SuspendForEditor {
                    path,
                    refresh_agents_modal: Some(crate::views::agents_modal::AgentsTab::Personas),
                })
            }
            PersonaDetailOutcome::Changed => InputOutcome::Changed,
            PersonaDetailOutcome::Unchanged => InputOutcome::Unchanged,
        }
    }

    pub(super) fn handle_persona_detail_paste(&mut self, text: &str) -> InputOutcome {
        let Some(ref mut detail) = self.persona_detail else {
            return InputOutcome::Unchanged;
        };
        match crate::views::persona_detail::handle_persona_detail_paste(detail, text) {
            crate::views::persona_detail::PersonaDetailOutcome::Changed => InputOutcome::Changed,
            _ => InputOutcome::Unchanged,
        }
    }

    pub(super) fn handle_persona_detail_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
    ) -> InputOutcome {
        let Some(ref mut detail) = self.persona_detail else {
            return InputOutcome::Unchanged;
        };
        use crate::views::persona_detail::{PersonaDetailOutcome, handle_persona_detail_mouse};
        match handle_persona_detail_mouse(detail, mouse) {
            PersonaDetailOutcome::Close => {
                self.persona_detail = None;
                if let Some(ref mut modal) = self.agents_modal {
                    modal.refresh_personas();
                }
                InputOutcome::Changed
            }
            PersonaDetailOutcome::Changed => InputOutcome::Changed,
            PersonaDetailOutcome::EditInEditor { .. } | PersonaDetailOutcome::Unchanged => {
                InputOutcome::Unchanged
            }
        }
    }

    // -- Hooks/plugins modal input handling --

    fn log_extensions_modal_action(
        &self,
        action: &str,
        input_method: echo_agent_telemetry::events::ExtensionsInputMethod,
    ) {
        self.log_extensions_modal_action_with(action, input_method, None, None);
    }

    fn log_extensions_modal_action_with(
        &self,
        action: &str,
        input_method: echo_agent_telemetry::events::ExtensionsInputMethod,
        target: Option<String>,
        enabled: Option<bool>,
    ) {
        if let Some(ref state) = self.extensions_modal {
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::ExtensionsModalAction {
                    tab: state.active_tab.telemetry_tab(),
                    action: action.into(),
                    input_method,
                    target,
                    enabled,
                },
            );
        }
    }

    fn log_extensions_modal_resolved_action(
        &self,
        ch: char,
        action: &crate::views::extensions_modal::ButtonAction,
        input_method: echo_agent_telemetry::events::ExtensionsInputMethod,
    ) {
        if let Some(ref state) = self.extensions_modal
            && let Some(label) =
                crate::views::extensions_modal::action_telemetry_label(state.active_tab, ch)
        {
            let (target, enabled) = Self::extensions_action_target(state, action);
            self.log_extensions_modal_action_with(&label, input_method, target, enabled);
        }
    }

    fn extensions_action_target(
        state: &crate::views::extensions_modal::ExtensionsModalState,
        action: &crate::views::extensions_modal::ButtonAction,
    ) -> (Option<String>, Option<bool>) {
        use crate::views::extensions_modal::{ButtonAction, TabDataState};

        let next_enabled = matches!(
            action,
            ButtonAction::ToggleSelectedPlugin
                | ButtonAction::ToggleSelectedHook
                | ButtonAction::ToggleSelectedSkill
                | ButtonAction::ToggleSelectedMcpServer
        )
        .then(|| state.selected_item_enabled().map(|on| !on))
        .flatten();

        match action {
            ButtonAction::ToggleSelectedPlugin
            | ButtonAction::UninstallSelectedPlugin
            | ButtonAction::UpdateSelectedPlugin => {
                if let TabDataState::Loaded(ref data) = state.plugins_data
                    && let Some(idx) = state.selected_data_index()
                    && let Some(plugin) = data.plugins.get(idx)
                {
                    (Some(plugin.name.clone()), next_enabled)
                } else {
                    (None, None)
                }
            }
            ButtonAction::ToggleSelectedHook | ButtonAction::RemoveSelectedHook => {
                if let TabDataState::Loaded(ref data) = state.hooks_data
                    && let Some(idx) = state.selected_data_index()
                    && let Some(hook) = data.hooks.get(idx)
                {
                    let label = if matches!(action, ButtonAction::ToggleSelectedHook)
                        && state.hooks_collapsed_groups.contains(&hook.source_dir)
                    {
                        let (label, _) =
                            crate::views::extensions_modal::derive_source_label(&hook.source_dir);
                        label
                    } else {
                        hook.name.clone()
                    };
                    (Some(label), next_enabled)
                } else {
                    (None, None)
                }
            }
            ButtonAction::ToggleSelectedSkill => {
                if let TabDataState::Loaded(ref skills) = state.skills_data
                    && let Some(idx) = state.selected_data_index()
                    && let Some(skill) = skills.get(idx)
                {
                    (Some(skill.name.clone()), next_enabled)
                } else {
                    (None, None)
                }
            }
            ButtonAction::ToggleSelectedMcpServer
            | ButtonAction::RemoveSelectedMcpServer
            | ButtonAction::McpAuthTrigger => {
                let TabDataState::Loaded(ref servers) = state.mcps_data else {
                    return (None, None);
                };
                if matches!(action, ButtonAction::ToggleSelectedMcpServer)
                    && let Some((si, ti)) = state.selected_mcp_tool()
                {
                    return if let Some(server) = servers.get(si)
                        && let Some(tool) = server.tools.get(ti)
                    {
                        (Some(format!("{}/{}", server.name, tool.name)), next_enabled)
                    } else {
                        (None, None)
                    };
                }
                if let Some(idx) = state.selected_data_index()
                    && let Some(server) = servers.get(idx)
                {
                    (Some(server.name.clone()), next_enabled)
                } else {
                    (None, None)
                }
            }
            ButtonAction::InstallSelectedMarketplacePlugin
            | ButtonAction::UpdateSelectedMarketplacePlugin
            | ButtonAction::UninstallSelectedMarketplacePlugin => {
                if let TabDataState::Loaded(ref response) = state.marketplace_data
                    && let Some((si, Some(pi))) =
                        state.resolve_marketplace_selection(&response.sources)
                    && let Some(plugin) = response.sources.get(si).and_then(|s| s.plugins.get(pi))
                {
                    (Some(plugin.name.clone()), None)
                } else {
                    (None, None)
                }
            }
            ButtonAction::RemoveSelectedMarketplaceSource => {
                if let TabDataState::Loaded(ref response) = state.marketplace_data
                    && let Some(source) = state
                        .resolve_marketplace_selection(&response.sources)
                        .and_then(|(si, _)| response.sources.get(si))
                {
                    (Some(source.source_name.clone()), None)
                } else {
                    (None, None)
                }
            }
            _ => (None, None),
        }
    }

    pub(super) fn handle_extensions_modal_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
    ) -> InputOutcome {
        // Handle modal messages (errors and confirmations) FIRST, before
        // the pending_action guard. Some error paths (e.g. structured
        // OutcomeStatus::ValidationError) leave pending_action set when
        // they raise the error, so the in-flight guard would otherwise
        // swallow every key and prevent the user from dismissing the
        // error.
        if self
            .extensions_modal
            .as_ref()
            .is_some_and(|s| s.modal_message.is_some())
        {
            if let Some(ref mut state) = self.extensions_modal {
                use crate::views::extensions_modal::ModalMessage;
                match (&state.modal_message, key.code) {
                    (
                        Some(ModalMessage::Confirmation {
                            action,
                            pending_entry_index,
                            ..
                        }),
                        KeyCode::Char('y'),
                    ) => {
                        let action = action.clone();
                        let pending_entry_index = *pending_entry_index;
                        state.modal_message = None;
                        return self.confirm_extensions_modal_action(action, pending_entry_index);
                    }
                    _ => {
                        // Dismissing the error/confirmation also clears
                        // the in-flight "[processing]" badge — the action
                        // is done and the user has acknowledged.
                        state.modal_message = None;
                        state.pending_action = None;
                        state.pending_entry_index = None;
                    }
                }
            }
            return InputOutcome::Changed;
        }

        // Block all action keys while an action is in-flight (no error
        // overlay is showing — that case is handled above). Esc closes the
        // modal so a hung list/refresh cannot trap the user; background
        // work (auth, refresh) continues without the UI lock.
        if self
            .extensions_modal
            .as_ref()
            .is_some_and(|s| s.pending_action.is_some())
        {
            return match key.code {
                KeyCode::Esc => {
                    self.extensions_modal = None;
                    InputOutcome::Changed
                }
                _ => InputOutcome::Changed,
            };
        }

        // If in setup or input mode, route to the form handler.
        if self
            .extensions_modal
            .as_ref()
            .is_some_and(|s| s.mcp_setup.is_some())
        {
            return self.handle_mcp_setup_key(key);
        }
        if self
            .extensions_modal
            .as_ref()
            .is_some_and(|s| s.input.is_some())
        {
            return self.handle_modal_input_key(key);
        }

        // Ctrl+O opens cloud.echo-agent.invalid connectors on the MCP servers tab.
        if key.code == KeyCode::Char('o')
            && key.modifiers == KeyModifiers::CONTROL
            && self.extensions_modal.as_ref().is_some_and(|s| {
                s.active_tab == crate::views::extensions_modal::ExtensionsTab::McpServers
                    && !s.picker_state.search_active
            })
        {
            self.log_extensions_modal_action(
                "open_connectors",
                echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
            );
            return self.execute_modal_button_action(
                crate::views::extensions_modal::ButtonAction::OpenManagedConnectors,
            );
        }

        // Route chrome keys through ModalWindow first (mirrors the mouse path).
        // Handles Esc -> CloseRequested and h/l (or L/R when not tabs-focused)
        // -> fold outcomes when FoldInfo provided.
        // When the tab bar has been focused via Up/Down (`window.tabs_focused`),
        // Left/Right are left as Unhandled here so they reach picker input
        // which cycles tabs only while the tab list is selected.
        // This restores default L/R = expand/collapse on the selected item
        // unless the user has explicitly navigated focus to the tabs with arrows.
        {
            let state = self.extensions_modal.as_mut().unwrap();
            let labels: Vec<&str> = crate::views::extensions_modal::ExtensionsTab::ALL
                .iter()
                .map(|t| t.label())
                .collect();
            // Build FoldInfo from the focused entry's state. When search is
            // active or the tab bar is focused via Up/Down, fold_info is None
            // so h/l/L/R return Unhandled and fall through (picker handles
            // tabs or search cursor for arrows; L/R on content do expand/collapse).
            let fold_info = if state.picker_state.search_active || state.window.tabs_focused {
                None
            } else {
                let sel = state.picker_state.selected;
                if state
                    .entry_non_selectable
                    .get(sel)
                    .copied()
                    .unwrap_or(false)
                {
                    None
                } else {
                    let group_key = state
                        .entry_group_keys
                        .get(sel)
                        .and_then(|k| k.as_ref())
                        .cloned();
                    if let Some(ref gk) = group_key {
                        let is_expanded = state.is_group_expanded(sel, gk);
                        Some(crate::views::modal_window::FoldInfo {
                            collapsible: true,
                            expanded: is_expanded,
                            has_details: false,
                            details_expanded: false,
                            // Group headers are top-level in the extensions
                            // modal (no nesting).
                            parent_index: None,
                        })
                    } else {
                        // Leaf item: can have expandable detail fields.
                        let details_expanded = state.picker_state.expanded.contains(&sel);
                        let parent = (0..sel).rev().find(|&i| {
                            state
                                .entry_group_keys
                                .get(i)
                                .and_then(|k| k.as_ref())
                                .is_some()
                        });
                        Some(crate::views::modal_window::FoldInfo {
                            collapsible: false,
                            expanded: false,
                            has_details: true,
                            details_expanded,
                            parent_index: parent,
                        })
                    }
                }
            };
            let config = crate::views::modal_window::ModalWindowConfig {
                // Empty title — matches the renderer in extensions_modal.rs
                // which uses the tab bar to identify the modal contents.
                // Keep these in sync so future changes to handle_modal_key
                // that read `title` (e.g. for accessibility announcements)
                // see the same value the user sees.
                title: "",
                tabs: Some(&labels),
                shortcuts: &[],
                sizing: crate::views::modal_window::ModalSizing::default(),
                fold_info,
            };
            let outcome =
                crate::views::modal_window::handle_modal_key(&mut state.window, key, &config);
            match outcome {
                crate::views::modal_window::ModalWindowOutcome::CloseRequested => {
                    if state.picker_state.query().is_empty() && !state.picker_state.search_active {
                        self.extensions_modal = None;
                        return InputOutcome::Changed;
                    }
                }
                crate::views::modal_window::ModalWindowOutcome::CollapseGroup => {
                    let sel = state.picker_state.selected;
                    if let Some(gk) = state
                        .entry_group_keys
                        .get(sel)
                        .and_then(|k| k.as_ref())
                        .cloned()
                        && self.extensions_modal_set_collapsed(sel, &gk, true)
                    {
                        self.log_extensions_modal_action(
                            "collapse",
                            echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                        );
                    }
                    return InputOutcome::Changed;
                }
                crate::views::modal_window::ModalWindowOutcome::ExpandGroup => {
                    let sel = state.picker_state.selected;
                    if let Some(gk) = state
                        .entry_group_keys
                        .get(sel)
                        .and_then(|k| k.as_ref())
                        .cloned()
                    {
                        if state.mcp_auth_intercept_on_expand() {
                            let (target, enabled) = Self::extensions_action_target(
                                state,
                                &crate::views::extensions_modal::ButtonAction::McpAuthTrigger,
                            );
                            self.log_extensions_modal_action_with(
                                "auth",
                                echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                                target,
                                enabled,
                            );
                            return self.execute_modal_button_action(
                                crate::views::extensions_modal::ButtonAction::McpAuthTrigger,
                            );
                        }
                        if self.extensions_modal_set_collapsed(sel, &gk, false) {
                            self.log_extensions_modal_action(
                                "expand",
                                echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                            );
                        }
                    }
                    return InputOutcome::Changed;
                }
                crate::views::modal_window::ModalWindowOutcome::CollapseDetails => {
                    let sel = state.picker_state.selected;
                    state.picker_state.expanded.remove(&sel);
                    state.picker_state.scroll_offset = None;
                    self.log_extensions_modal_action(
                        "collapse",
                        echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                    );
                    return InputOutcome::Changed;
                }
                crate::views::modal_window::ModalWindowOutcome::ExpandDetails => {
                    let sel = state.picker_state.selected;
                    state.picker_state.expanded.insert(sel);
                    state.picker_state.scroll_offset = None;
                    self.log_extensions_modal_action(
                        "expand",
                        echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                    );
                    return InputOutcome::Changed;
                }
                crate::views::modal_window::ModalWindowOutcome::JumpToParent(idx) => {
                    state.picker_state.selected = idx;
                    state.picker_state.scroll_offset = None;
                    return InputOutcome::Changed;
                }
                _ => {
                    // Unhandled and other outcomes fall through to picker.
                }
            }
        }

        // Delegate navigation/search/tab/filter/action to handle_picker_input.
        let Some(state) = self.extensions_modal.as_mut() else {
            return InputOutcome::Changed;
        };

        // Build the same config as the renderer.
        let labels: Vec<&str> = crate::views::extensions_modal::ExtensionsTab::ALL
            .iter()
            .map(|t| t.label())
            .collect();
        let active_idx = crate::views::extensions_modal::ExtensionsTab::ALL
            .iter()
            .position(|t| *t == state.active_tab)
            .unwrap_or(0);
        let has_filter = matches!(
            state.active_tab,
            crate::views::extensions_modal::ExtensionsTab::Hooks
                | crate::views::extensions_modal::ExtensionsTab::Plugins
                | crate::views::extensions_modal::ExtensionsTab::McpServers
        );
        let filter = match state.active_tab {
            crate::views::extensions_modal::ExtensionsTab::Hooks => state.hooks_filter,
            crate::views::extensions_modal::ExtensionsTab::Plugins => state.plugins_filter,
            crate::views::extensions_modal::ExtensionsTab::McpServers => state.mcps_filter,
            _ => crate::views::extensions_modal::StatusFilter::All,
        };
        let action_keys = crate::views::extensions_modal::extensions_action_keys(state.active_tab);
        let entry_count = state.entry_data_indices.len();
        let non_selectable_owned = Self::extensions_modal_non_selectable_mask(state, entry_count);
        let non_selectable = &non_selectable_owned;
        let clickable_owned =
            Self::extensions_modal_non_selectable_clickable_mask(state, entry_count);
        let non_selectable_clickable = &clickable_owned;

        let config = crate::views::picker::PickerConfig {
            title: None,
            show_search_hint: true,
            expandable: true,
            esc_clears_query: true,
            shortcuts: Some(crate::views::picker::picker_shortcuts()),
            pending_hint: None,
            shortcuts_area: None,
            non_selectable,
            non_selectable_clickable,
            tabs: Some(&labels),
            active_tab: active_idx,
            filter_label: if has_filter {
                Some(filter.label())
            } else {
                None
            },
            filter_key_hint: if has_filter { Some("f") } else { None },
            filter_active: filter != crate::views::extensions_modal::StatusFilter::All,
            header_note: None,
            action_keys: &action_keys,
            disable_search: false,
            compact_bottom_bar: false,
            // Skills-tab letters double as quick keys today and the
            // tab feels noisy when typing a single letter immediately
            // commits a query. Require explicit `/` (or click) to
            // activate search there.
            search_only_on_slash: state.active_tab
                == crate::views::extensions_modal::ExtensionsTab::Skills,
            vim_normal_first: crate::appearance::cache::load_vim_mode(),
        };

        let ev = crossterm::event::Event::Key(*key);
        let outcome = crate::views::picker::handle_picker_input(
            &ev,
            &mut state.picker_state,
            entry_count,
            &config,
        );

        // Search state now lives directly in picker_state (no sync needed).

        match outcome {
            crate::views::picker::PickerOutcome::Closed => {
                self.extensions_modal = None;
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::TabChanged(idx) => {
                if let Some(ref mut state) = self.extensions_modal
                    && let Some(&tab) = crate::views::extensions_modal::ExtensionsTab::ALL.get(idx)
                {
                    // switch_tab also clears the Add form, error
                    // overlay, and pending [processing] badge so the
                    // new tab opens in a clean browse view.
                    state.switch_tab(tab);
                    state.window.tabs_focused = state.picker_state.tabs_focused;
                }
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::FilterCycled => {
                let mut cycled = false;
                if let Some(ref mut state) = self.extensions_modal {
                    cycled = match state.active_tab {
                        crate::views::extensions_modal::ExtensionsTab::Hooks => {
                            state.hooks_filter = state.hooks_filter.next();
                            true
                        }
                        crate::views::extensions_modal::ExtensionsTab::Plugins => {
                            state.plugins_filter = state.plugins_filter.next();
                            true
                        }
                        crate::views::extensions_modal::ExtensionsTab::McpServers => {
                            state.mcps_filter = state.mcps_filter.next();
                            true
                        }
                        _ => false,
                    };
                    // Reset selection after filter change.
                    state.picker_state.selected = 0;
                    state.picker_state.scroll_offset = None;
                    state.picker_state.tabs_focused = false;
                }
                if cycled {
                    self.log_extensions_modal_action(
                        "filter",
                        echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                    );
                }
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::Action(ch) => {
                if let Some(action) = self
                    .extensions_modal
                    .as_ref()
                    .and_then(|s| crate::views::extensions_modal::resolve_key(s.active_tab, ch))
                {
                    self.log_extensions_modal_resolved_action(
                        ch,
                        &action,
                        echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                    );
                    self.execute_modal_button_action(action)
                } else {
                    InputOutcome::Changed
                }
            }
            crate::views::picker::PickerOutcome::Selected(_)
            | crate::views::picker::PickerOutcome::Expand(_) => self
                .extensions_modal_expand_or_auth(
                    echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                ),
            crate::views::picker::PickerOutcome::Collapse(_) => {
                self.extensions_modal_toggle_fold(
                    echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                );
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::NonSelectableClick(idx) => {
                self.extensions_modal_toggle_mcp_section_at(
                    idx,
                    echo_agent_telemetry::events::ExtensionsInputMethod::Keyboard,
                );
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::Copy(_) => InputOutcome::Changed,
            crate::views::picker::PickerOutcome::SubmitQuery => InputOutcome::Changed,
            crate::views::picker::PickerOutcome::Changed
            | crate::views::picker::PickerOutcome::QueryChanged => InputOutcome::Changed,
            crate::views::picker::PickerOutcome::Unchanged => InputOutcome::Unchanged,
        }
    }

    fn handle_mcp_setup_key(&mut self, key: &KeyEvent) -> InputOutcome {
        use crate::views::extensions_modal::McpSetupOutcome;

        let Some(ref mut state) = self.extensions_modal else {
            return InputOutcome::Unchanged;
        };
        let Some(ref mut setup) = state.mcp_setup else {
            return InputOutcome::Unchanged;
        };

        match setup.handle_key(key) {
            McpSetupOutcome::Changed => InputOutcome::Changed,
            McpSetupOutcome::Unchanged => InputOutcome::Unchanged,
            McpSetupOutcome::Cancel => {
                state.mcp_setup = None;
                InputOutcome::Changed
            }
            McpSetupOutcome::Submit => {
                let Some(values) = setup.values() else {
                    setup.error = Some("Select an option".to_string());
                    return InputOutcome::Changed;
                };
                let server_name = setup.server_name.clone();
                state.mcp_setup = None;
                state.pending_action = Some(format!("Authenticating {server_name}..."));
                state.pending_entry_index = None;
                InputOutcome::Action(Action::McpSetupSubmit {
                    server_name,
                    values,
                })
            }
        }
    }

    /// Handle key events while the modal is in input mode (text field active).
    fn handle_modal_input_key(&mut self, key: &KeyEvent) -> InputOutcome {
        use crate::views::extensions_modal::ModalInputOutcome;

        let Some(ref mut state) = self.extensions_modal else {
            return InputOutcome::Unchanged;
        };
        let Some(ref mut input) = state.input else {
            return InputOutcome::Unchanged;
        };

        match input.handle_key(key) {
            ModalInputOutcome::Changed => InputOutcome::Changed,
            ModalInputOutcome::Unchanged => InputOutcome::Unchanged,
            ModalInputOutcome::Cancel => {
                state.input = None;
                InputOutcome::Changed
            }
            ModalInputOutcome::Submit {
                command_prefix,
                field_texts,
            } => {
                state.input = None;
                if let Some(action) = crate::views::extensions_modal::build_action_from_input(
                    &command_prefix,
                    &field_texts,
                ) {
                    self.execute_modal_button_action(action)
                } else {
                    InputOutcome::Changed
                }
            }
        }
    }

    /// Handle a bracketed-paste event while the hooks/plugins modal is open.
    ///
    /// Routes pasted text to the inline input field (when active) or the
    /// search query (when search mode is active). Without this, the native
    /// paste shortcut (Cmd-V / Shift-Insert) is swallowed because the modal
    /// intercept only routes `Event::Key` and `Event::Mouse` by default.
    pub(super) fn handle_extensions_modal_paste(&mut self, text: &str) -> InputOutcome {
        let Some(ref mut state) = self.extensions_modal else {
            return InputOutcome::Unchanged;
        };
        if state.modal_message.is_some() || state.pending_action.is_some() {
            return InputOutcome::Unchanged;
        }
        if state.apply_paste(text) {
            InputOutcome::Changed
        } else {
            InputOutcome::Unchanged
        }
    }

    /// Handle a mouse event while the hooks/plugins modal is open.
    ///
    /// - Clicks on tabs switch the active tab.
    /// - Clicks outside the popup close it.
    /// - Everything else is consumed.
    pub(super) fn handle_extensions_modal_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
    ) -> InputOutcome {
        use crossterm::event::MouseEventKind;

        // Route chrome events (close button, tabs, click-outside) through
        // the shared ModalWindow handler first.
        let chrome_shortcut_ch: Option<char> = {
            let state = self.extensions_modal.as_mut().unwrap();
            let outcome = crate::views::modal_window::handle_modal_mouse(
                &mut state.window,
                mouse.kind,
                mouse.column,
                mouse.row,
            );
            match outcome {
                crate::views::modal_window::ModalWindowOutcome::CloseRequested => {
                    self.extensions_modal = None;
                    return InputOutcome::Changed;
                }
                crate::views::modal_window::ModalWindowOutcome::TabChanged(idx) => {
                    if let Some(&tab) = crate::views::extensions_modal::ExtensionsTab::ALL.get(idx)
                    {
                        // Clears Add form, error overlay, and pending
                        // badge in addition to resetting picker state.
                        state.switch_tab(tab);
                        // Clicking a tab implies interaction with the tab list;
                        // show the focused highlight and keep arrow nav on tabs.
                        state.picker_state.tabs_focused = true;
                        state.window.tabs_focused = true;
                    }
                    return InputOutcome::Changed;
                }
                crate::views::modal_window::ModalWindowOutcome::Handled => {
                    return InputOutcome::Changed;
                }
                crate::views::modal_window::ModalWindowOutcome::ShortcutActivated(id) => {
                    // Footer shortcut IDs 100+ map to action_keys.
                    // Resolve the char here; dispatch after the borrow
                    // is released so execute_modal_button_action can
                    // take &mut self.
                    if id == 98 {
                        // "Tab/Shift+Tab tabs" hint — cycle to the next
                        // tab, mirroring the Tab keypress flow.
                        let all = crate::views::extensions_modal::ExtensionsTab::ALL;
                        let cur = all.iter().position(|&t| t == state.active_tab).unwrap_or(0);
                        let next = (cur + 1) % all.len();
                        if let Some(&tab) = all.get(next) {
                            // Clears Add form, error overlay, and
                            // pending badge in addition to resetting
                            // picker state.
                            state.switch_tab(tab);
                            state.picker_state.tabs_focused = true;
                            state.window.tabs_focused = true;
                        }
                        return InputOutcome::Changed;
                    } else if id == 99 {
                        // "Esc close" shortcut — signal close via sentinel.
                        Some('\x00')
                    } else if id >= 100 {
                        let keys = crate::views::extensions_modal::extensions_action_keys(
                            state.active_tab,
                        );
                        keys.get(id - 100).map(|&(ch, _)| ch)
                    } else {
                        None
                    }
                }
                _ => None, // Unhandled — fall through to picker
            }
        };
        // Dispatch shortcut click (if any) now that the &mut borrow is released.
        if let Some(ch) = chrome_shortcut_ch {
            if ch == '\x00' {
                // "Esc close" shortcut clicked.
                self.extensions_modal = None;
                return InputOutcome::Changed;
            }
            // Block action shortcuts while an action is in-flight
            // (mirrors the keyboard guard in handle_extensions_modal_key).
            if self
                .extensions_modal
                .as_ref()
                .is_some_and(|s| s.pending_action.is_some())
            {
                return InputOutcome::Changed;
            }
            if let Some(action) = self
                .extensions_modal
                .as_ref()
                .and_then(|s| crate::views::extensions_modal::resolve_key(s.active_tab, ch))
            {
                self.log_extensions_modal_resolved_action(
                    ch,
                    &action,
                    echo_agent_telemetry::events::ExtensionsInputMethod::Mouse,
                );
                return self.execute_modal_button_action(action);
            }
            return InputOutcome::Changed;
        }

        let Some(ref mut state) = self.extensions_modal else {
            return InputOutcome::Changed;
        };

        // Modal overlay covers picker rows but not their hit-rects: dismiss
        // on any mouse-down so a click-through doesn't re-trigger the row
        // underneath (which can re-fire OAuth on [needs auth] rows).
        if state.modal_message.is_some()
            && matches!(
                mouse.kind,
                MouseEventKind::Down(crossterm::event::MouseButton::Left)
                    | MouseEventKind::Down(crossterm::event::MouseButton::Right)
                    | MouseEventKind::Down(crossterm::event::MouseButton::Middle)
            )
        {
            // Mirror the keyboard dismissal path: clearing the
            // error/confirmation also clears the in-flight
            // "[processing]" badge so the mouse and keyboard paths
            // agree on what dismiss means.
            state.modal_message = None;
            state.pending_action = None;
            state.pending_entry_index = None;
            return InputOutcome::Changed;
        }

        // Build the same config as the renderer/key handler.
        let labels: Vec<&str> = crate::views::extensions_modal::ExtensionsTab::ALL
            .iter()
            .map(|t| t.label())
            .collect();
        let active_idx = crate::views::extensions_modal::ExtensionsTab::ALL
            .iter()
            .position(|t| *t == state.active_tab)
            .unwrap_or(0);
        let has_filter = matches!(
            state.active_tab,
            crate::views::extensions_modal::ExtensionsTab::Hooks
                | crate::views::extensions_modal::ExtensionsTab::Plugins
                | crate::views::extensions_modal::ExtensionsTab::McpServers
        );
        let filter = match state.active_tab {
            crate::views::extensions_modal::ExtensionsTab::Hooks => state.hooks_filter,
            crate::views::extensions_modal::ExtensionsTab::Plugins => state.plugins_filter,
            crate::views::extensions_modal::ExtensionsTab::McpServers => state.mcps_filter,
            _ => crate::views::extensions_modal::StatusFilter::All,
        };
        let action_keys: Vec<(char, &str)> = vec![]; // No action keys for mouse
        let entry_count = state.entry_data_indices.len();
        let non_selectable_owned = Self::extensions_modal_non_selectable_mask(state, entry_count);
        let non_selectable = &non_selectable_owned;
        let clickable_owned =
            Self::extensions_modal_non_selectable_clickable_mask(state, entry_count);
        let non_selectable_clickable = &clickable_owned;

        let config = crate::views::picker::PickerConfig {
            title: None,
            show_search_hint: true,
            expandable: true,
            esc_clears_query: true,
            shortcuts: Some(crate::views::picker::picker_shortcuts()),
            pending_hint: None,
            shortcuts_area: None,
            non_selectable,
            non_selectable_clickable,
            tabs: Some(&labels),
            active_tab: active_idx,
            filter_label: if has_filter {
                Some(filter.label())
            } else {
                None
            },
            filter_key_hint: if has_filter { Some("f") } else { None },
            filter_active: filter != crate::views::extensions_modal::StatusFilter::All,
            header_note: None,
            action_keys: &action_keys,
            disable_search: false,
            compact_bottom_bar: false,
            // Same gate as the keyboard handler — keep behavior
            // consistent so a mouse-driven tab switch doesn't change
            // the typing semantics on Skills.
            search_only_on_slash: state.active_tab
                == crate::views::extensions_modal::ExtensionsTab::Skills,
            vim_normal_first: crate::appearance::cache::load_vim_mode(),
        };

        let ev = crossterm::event::Event::Mouse(*mouse);
        let outcome = crate::views::picker::handle_picker_input(
            &ev,
            &mut state.picker_state,
            entry_count,
            &config,
        );

        // Open the connectors URL on mouse-down (parity with Ctrl+O). A section-row
        // click routes as Selected or NonSelectableClick, so intercept both here.
        let clicked_entry = match &outcome {
            crate::views::picker::PickerOutcome::Selected(i)
            | crate::views::picker::PickerOutcome::Expand(i)
            | crate::views::picker::PickerOutcome::NonSelectableClick(i) => Some(*i),
            _ => None,
        };
        if let Some(idx) = clicked_entry
            && self.extensions_modal_click_opens_connectors(idx, mouse.row)
        {
            self.log_extensions_modal_action(
                "open_connectors",
                echo_agent_telemetry::events::ExtensionsInputMethod::Mouse,
            );
            return self.execute_modal_button_action(
                crate::views::extensions_modal::ButtonAction::OpenManagedConnectors,
            );
        }

        // Hover states are managed by ModalWindow (close) and picker (filter).

        match outcome {
            crate::views::picker::PickerOutcome::Closed => {
                self.extensions_modal = None;
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::TabChanged(idx) => {
                if let Some(ref mut state) = self.extensions_modal
                    && let Some(&tab) = crate::views::extensions_modal::ExtensionsTab::ALL.get(idx)
                {
                    // Clears Add form, error overlay, and pending
                    // badge in addition to resetting picker state.
                    state.switch_tab(tab);
                    // Mouse-driven tab switch via picker hit area → treat tabs as focused.
                    state.picker_state.tabs_focused = true;
                    state.window.tabs_focused = true;
                }
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::FilterCycled => {
                let mut cycled = false;
                if let Some(ref mut state) = self.extensions_modal {
                    cycled = match state.active_tab {
                        crate::views::extensions_modal::ExtensionsTab::Hooks => {
                            state.hooks_filter = state.hooks_filter.next();
                            true
                        }
                        crate::views::extensions_modal::ExtensionsTab::Plugins => {
                            state.plugins_filter = state.plugins_filter.next();
                            true
                        }
                        crate::views::extensions_modal::ExtensionsTab::McpServers => {
                            state.mcps_filter = state.mcps_filter.next();
                            true
                        }
                        _ => false,
                    };
                    state.picker_state.selected = 0;
                    state.picker_state.scroll_offset = None;
                }
                if cycled {
                    self.log_extensions_modal_action(
                        "filter",
                        echo_agent_telemetry::events::ExtensionsInputMethod::Mouse,
                    );
                }
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::Selected(_)
            | crate::views::picker::PickerOutcome::Expand(_) => self
                .extensions_modal_expand_or_auth(
                    echo_agent_telemetry::events::ExtensionsInputMethod::Mouse,
                ),
            crate::views::picker::PickerOutcome::NonSelectableClick(idx) => {
                self.extensions_modal_toggle_mcp_section_at(
                    idx,
                    echo_agent_telemetry::events::ExtensionsInputMethod::Mouse,
                );
                InputOutcome::Changed
            }
            crate::views::picker::PickerOutcome::Changed
            | crate::views::picker::PickerOutcome::QueryChanged => InputOutcome::Changed,
            crate::views::picker::PickerOutcome::Unchanged => InputOutcome::Unchanged,
            _ => InputOutcome::Changed,
        }
    }

    /// Toggle fold on an MCP section header row (clicked, not keyboard-selected).
    fn extensions_modal_toggle_mcp_section_at(
        &mut self,
        entry_idx: usize,
        input_method: echo_agent_telemetry::events::ExtensionsInputMethod,
    ) {
        let Some(ref mut state) = self.extensions_modal else {
            return;
        };
        if state.active_tab != crate::views::extensions_modal::ExtensionsTab::McpServers {
            return;
        }
        let Some(gk) = state
            .entry_group_keys
            .get(entry_idx)
            .and_then(|k| k.as_ref())
            .map(|s| s.as_str())
        else {
            return;
        };
        if !gk.starts_with("mcp-section:") {
            return;
        }
        let expanded = if state.mcps_collapsed_sections.remove(gk) {
            // was collapsed → now expanded
            true
        } else {
            state.mcps_collapsed_sections.insert(gk.to_string());
            false
        };
        state.picker_state.scroll_offset = None;
        self.log_extensions_modal_action(
            if expanded { "expand" } else { "collapse" },
            input_method,
        );
    }

    /// Whether a click at `mouse_row` on entry `entry_idx` hit the connectors URL
    /// link band recorded at last paint (opens the URL instead of folding).
    fn extensions_modal_click_opens_connectors(&self, entry_idx: usize, mouse_row: u16) -> bool {
        self.extensions_modal.as_ref().is_some_and(|state| {
            // Parity with the Ctrl+O guard: don't open while the search bar has focus.
            !state.picker_state.search_active
                && state
                    .picker_state
                    .link_band
                    .as_ref()
                    .is_some_and(|(idx, band)| *idx == entry_idx && band.contains(&mouse_row))
        })
    }

    /// Non-selectable mask for the extensions modal picker (from last render).
    fn extensions_modal_non_selectable_mask(
        state: &crate::views::extensions_modal::ExtensionsModalState,
        entry_count: usize,
    ) -> Vec<bool> {
        if state.entry_non_selectable.len() == entry_count {
            return state.entry_non_selectable.clone();
        }
        (0..entry_count)
            .map(|i| {
                state
                    .entry_group_keys
                    .get(i)
                    .and_then(|k| k.as_deref())
                    .is_some_and(|k| k.starts_with("mcp-section:"))
            })
            .collect()
    }

    fn extensions_modal_non_selectable_clickable_mask(
        state: &crate::views::extensions_modal::ExtensionsModalState,
        entry_count: usize,
    ) -> Vec<bool> {
        if state.entry_non_selectable_clickable.len() == entry_count {
            return state.entry_non_selectable_clickable.clone();
        }
        (0..entry_count)
            .map(|i| {
                state
                    .entry_group_keys
                    .get(i)
                    .and_then(|k| k.as_deref())
                    .is_some_and(|k| k.starts_with("mcp-section:"))
            })
            .collect()
    }

    /// Expand/collapse the selected row, or trigger MCP OAuth when the server needs auth.
    fn extensions_modal_expand_or_auth(
        &mut self,
        input_method: echo_agent_telemetry::events::ExtensionsInputMethod,
    ) -> InputOutcome {
        if self
            .extensions_modal
            .as_ref()
            .is_some_and(|s| s.mcp_auth_intercept_on_expand())
        {
            let (target, enabled) = self
                .extensions_modal
                .as_ref()
                .map(|s| {
                    Self::extensions_action_target(
                        s,
                        &crate::views::extensions_modal::ButtonAction::McpAuthTrigger,
                    )
                })
                .unwrap_or((None, None));
            self.log_extensions_modal_action_with("auth", input_method, target, enabled);
            return self.execute_modal_button_action(
                crate::views::extensions_modal::ButtonAction::McpAuthTrigger,
            );
        }
        self.extensions_modal_toggle_fold(input_method);
        InputOutcome::Changed
    }

    /// Toggle the fold state of the selected entry in the extensions modal.
    ///
    /// Used by Enter/click/space to toggle expand/collapse. Group headers
    /// toggle their collapsed state; leaf items toggle detail-field expansion.
    fn extensions_modal_toggle_fold(
        &mut self,
        input_method: echo_agent_telemetry::events::ExtensionsInputMethod,
    ) {
        let Some(ref mut state) = self.extensions_modal else {
            return;
        };
        let sel = state.picker_state.selected;
        let group_key = state
            .entry_group_keys
            .get(sel)
            .and_then(|k| k.as_ref())
            .cloned();

        if let Some(gk) = group_key {
            let is_expanded = state.is_group_expanded(sel, &gk);
            // `set_collapsed`'s third arg is the NEW collapsed state.
            // When currently expanded → new state is collapsed (true);
            // when currently collapsed → new state is expanded (false).
            // That value equals `is_expanded` directly. Using `!is_expanded`
            // (the previous code) made `e`/Enter/Space/click into a no-op
            // for every collapsible header (MCP servers, marketplace
            // sources, hooks groups).
            if self.extensions_modal_set_collapsed(sel, &gk, is_expanded) {
                self.log_extensions_modal_action(
                    if is_expanded { "collapse" } else { "expand" },
                    input_method,
                );
            }
        } else {
            // Leaf item: toggle detail fields.
            state.picker_state.scroll_offset = None;
            let expanded = if state.picker_state.expanded.contains(&sel) {
                state.picker_state.expanded.remove(&sel);
                false
            } else {
                state.picker_state.expanded.insert(sel);
                true
            };
            self.log_extensions_modal_action(
                if expanded { "expand" } else { "collapse" },
                input_method,
            );
        }
    }

    /// Set the collapsed state for a group key in the extensions modal.
    fn extensions_modal_set_collapsed(
        &mut self,
        sel: usize,
        group_key: &str,
        collapsed: bool,
    ) -> bool {
        let Some(ref mut state) = self.extensions_modal else {
            return false;
        };
        state.picker_state.scroll_offset = None;
        match state.active_tab {
            crate::views::extensions_modal::ExtensionsTab::Hooks => {
                if collapsed {
                    state.hooks_collapsed_groups.insert(group_key.to_string())
                } else {
                    state.hooks_collapsed_groups.remove(group_key)
                }
            }
            crate::views::extensions_modal::ExtensionsTab::Plugins => {
                if collapsed {
                    state.plugins_collapsed_groups.insert(group_key.to_string())
                } else {
                    state.plugins_collapsed_groups.remove(group_key)
                }
            }
            crate::views::extensions_modal::ExtensionsTab::Skills => {
                if collapsed {
                    state.skills_collapsed_groups.insert(group_key.to_string())
                } else {
                    state.skills_collapsed_groups.remove(group_key)
                }
            }
            // Workflows tab has no collapsible groups.
            crate::views::extensions_modal::ExtensionsTab::Workflows => false,
            crate::views::extensions_modal::ExtensionsTab::Marketplace => {
                let source_has_error = group_key
                    .parse::<usize>()
                    .ok()
                    .and_then(|si| {
                        if let crate::views::extensions_modal::TabDataState::Loaded(ref data) =
                            state.marketplace_data
                        {
                            data.sources.get(si).and_then(|s| s.error.as_ref())
                        } else {
                            None
                        }
                    })
                    .is_some();
                if source_has_error {
                    if collapsed {
                        state.picker_state.expanded.remove(&sel)
                    } else {
                        state.picker_state.expanded.insert(sel)
                    }
                } else if let Ok(source_idx) = group_key.parse::<usize>() {
                    if collapsed {
                        state.marketplace_collapsed_sources.insert(source_idx)
                    } else {
                        state.marketplace_collapsed_sources.remove(&source_idx)
                    }
                } else {
                    false
                }
            }
            crate::views::extensions_modal::ExtensionsTab::McpServers => {
                if group_key.starts_with("mcp-section:") {
                    if collapsed {
                        state.mcps_collapsed_sections.insert(group_key.to_string())
                    } else {
                        state.mcps_collapsed_sections.remove(group_key)
                    }
                } else if let Some(si) =
                    crate::views::extensions_modal::parse_mcp_tools_server_index(group_key)
                {
                    if collapsed {
                        state.mcps_tools_expanded.remove(&si)
                    } else {
                        state.mcps_tools_expanded.insert(si)
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Execute a modal button action — dispatches to ACP effect.
    fn execute_modal_button_action(
        &mut self,
        action: crate::views::extensions_modal::ButtonAction,
    ) -> InputOutcome {
        use crate::views::extensions_modal::{ButtonAction, ModalInput, TabDataState};

        // A new user-initiated action supersedes any lingering result notice.
        // The chained auto-reload goes through `Effect`, not here, so it keeps
        // the triggering action's notice (see `dispatch_action_result`).
        if let Some(ref mut state) = self.extensions_modal {
            state.result_notice = None;
        }

        match action {
            ButtonAction::HooksAction(hooks_action) => {
                if let Some(ref mut state) = self.extensions_modal {
                    state.modal_message = None;
                    if matches!(
                        hooks_action,
                        echo_agent_hooks_plugins_types::HooksAction::Reload
                    ) {
                        // Reload rebuilds the entire plugin registry -- show
                        // tab-level "Loading..." instead of a single-entry badge.
                        state.pending_action = Some("Reloading...".into());
                        state.pending_entry_index = None;
                        state.hooks_data = TabDataState::Loading;
                        state.plugins_data = TabDataState::Loading;
                        state.marketplace_data = TabDataState::Loading;
                    } else {
                        state.pending_action = Some("Processing...".into());
                        state.pending_entry_index = Some(state.picker_state.selected);
                    }
                }
                InputOutcome::Action(Action::ExecuteHooksAction(hooks_action))
            }
            ButtonAction::PluginsAction(plugins_action) => {
                if let Some(ref mut state) = self.extensions_modal {
                    state.modal_message = None;
                    state.last_plugins_action = Some(plugins_action.clone());
                    if matches!(
                        plugins_action,
                        echo_agent_hooks_plugins_types::PluginsAction::Reload
                    ) {
                        // Reload rebuilds the entire plugin registry -- show
                        // tab-level "Loading..." instead of a single-entry badge.
                        state.pending_action = Some("Reloading...".into());
                        state.pending_entry_index = None;
                        state.plugins_data = TabDataState::Loading;
                        state.marketplace_data = TabDataState::Loading;
                        state.hooks_data = TabDataState::Loading;
                    } else {
                        // Per-plugin actions badge the selected row. Update gets
                        // its own verb (matching the Marketplace tab) so the user
                        // sees the fetch is underway, not a generic spinner.
                        let label = if matches!(
                            plugins_action,
                            echo_agent_hooks_plugins_types::PluginsAction::Update { .. }
                        ) {
                            "Updating..."
                        } else {
                            "Processing..."
                        };
                        state.pending_action = Some(label.into());
                        state.pending_entry_index = Some(state.picker_state.selected);
                    }
                }
                InputOutcome::Action(Action::ExecutePluginsAction(plugins_action))
            }
            ButtonAction::McpAuthTrigger => {
                if let Some(ref mut state) = self.extensions_modal {
                    state.modal_message = None;
                    // `selected_data_index()` resolves to the parent server for
                    // both server and tool rows, so `i` from a tool row
                    // intentionally auths the parent. (Mouse path is stricter
                    // to avoid accidental clicks on indented rows.)
                    if let TabDataState::Loaded(ref servers) = state.mcps_data
                        && let Some(idx) = state.selected_data_index()
                        && let Some(server) = servers.get(idx)
                    {
                        if server.is_managed_gateway {
                            return InputOutcome::Action(Action::OpenManagedConnectors);
                        }
                        if server.setup_required
                            && let Some(form) =
                                crate::views::extensions_modal::McpSetupFormState::new(server)
                        {
                            state.mcp_setup = Some(form);
                            state.picker_state.search_active = false;
                            return InputOutcome::Changed;
                        }
                        // Drop repeats while an action is in flight on the same
                        // row to avoid double-spawning the OAuth browser flow.
                        let sel = state.picker_state.selected;
                        if state.pending_action.is_some() && state.pending_entry_index == Some(sel)
                        {
                            return InputOutcome::Unchanged;
                        }
                        state.pending_action = Some("authenticating...".into());
                        state.pending_entry_index = Some(sel);
                        return InputOutcome::Action(Action::McpAuthTrigger {
                            server_name: server.name.clone(),
                        });
                    }
                }
                InputOutcome::Changed
            }
            // For the `r` reload, the router's ReloadSkills arm does the
            // Loading writes (once a session exists) and both refetches.
            ButtonAction::ReloadSkills => InputOutcome::Action(Action::ReloadSkills),
            ButtonAction::RefreshMcpList => InputOutcome::Action(Action::RefreshMcpList),
            ButtonAction::OpenManagedConnectors => {
                InputOutcome::Action(Action::OpenManagedConnectors)
            }
            ButtonAction::ToggleSelectedMcpServer => {
                if let Some(ref mut state) = self.extensions_modal {
                    use crate::views::extensions_modal::TabDataState;
                    if let TabDataState::Loaded(ref servers) = state.mcps_data {
                        // If the cursor is on a tool row, never fall through
                        // to the server-toggle branch — drop the press on a
                        // stale tool index instead.
                        if let Some((si, ti)) = state.selected_mcp_tool() {
                            if let Some(server) = servers.get(si)
                                && let Some(tool) = server.tools.get(ti)
                            {
                                let label = if !tool.enabled {
                                    "enabling..."
                                } else {
                                    "disabling..."
                                };
                                state.pending_action = Some(label.into());
                                state.pending_entry_index = Some(state.picker_state.selected);
                                return InputOutcome::Action(Action::ToggleMcpTool {
                                    server_name: server.name.clone(),
                                    tool_name: tool.name.clone(),
                                    enabled: !tool.enabled,
                                });
                            }
                            return InputOutcome::Changed;
                        }
                        if let Some(idx) = state.selected_data_index()
                            && let Some(server) = servers.get(idx)
                        {
                            let label = if !server.enabled {
                                "enabling..."
                            } else {
                                "disabling..."
                            };
                            state.pending_action = Some(label.into());
                            state.pending_entry_index = Some(state.picker_state.selected);
                            return InputOutcome::Action(Action::ToggleMcpServer {
                                server_name: server.name.clone(),
                                enabled: !server.enabled,
                            });
                        }
                    }
                }
                InputOutcome::Changed
            }
            ButtonAction::AddMcpServer { name, config } => {
                if let Some(ref mut state) = self.extensions_modal {
                    // No pending_entry_index: the new row doesn't exist yet,
                    // so any index would decorate an unrelated existing row.
                    state.pending_action = Some("adding...".into());
                }
                InputOutcome::Action(Action::UpsertMcpServer { name, config })
            }
            ButtonAction::RemoveSelectedMcpServer => {
                let resolved = self.extensions_modal.as_ref().and_then(|state| {
                    use crate::views::extensions_modal::TabDataState;
                    use crate::views::mcps_modal::is_removable;
                    let TabDataState::Loaded(ref servers) = state.mcps_data else {
                        return None;
                    };
                    let idx = state.selected_data_index()?;
                    let server = servers.get(idx)?;
                    if is_removable(server) {
                        Some(Ok(server.name.clone()))
                    } else {
                        Some(Err(server.name.clone()))
                    }
                });
                match resolved {
                    Some(Err(name)) => {
                        if let Some(ref mut s) = self.extensions_modal {
                            s.modal_message =
                                Some(crate::views::extensions_modal::ModalMessage::Error(
                                    format!("Cannot remove managed server '{name}'"),
                                ));
                        }
                        InputOutcome::Changed
                    }
                    Some(Ok(server_name)) => self.prompt_extensions_confirm(
                        format!("Remove MCP server \"{server_name}\"?"),
                        crate::views::extensions_modal::ConfirmationAction::DeleteMcpServer {
                            server_name,
                        },
                    ),
                    None => InputOutcome::Changed,
                }
            }
            ButtonAction::MarketplaceAction(marketplace_action) => {
                if let Some(ref mut state) = self.extensions_modal {
                    use crate::views::extensions_modal::TabDataState;
                    state.modal_message = None;
                    match &marketplace_action {
                        // Refresh re-syncs every source and reloads the whole
                        // list, so show a tab-level loading state instead of
                        // decorating the single row under the cursor.
                        echo_agent_hooks_plugins_types::MarketplaceAction::Refresh { .. } => {
                            state.pending_action = None;
                            state.pending_entry_index = None;
                            state.marketplace_data = TabDataState::Loading;
                        }
                        // No pending_entry_index: the new source doesn't exist
                        // yet, so any index would decorate an unrelated row.
                        echo_agent_hooks_plugins_types::MarketplaceAction::AddSource { .. } => {
                            state.pending_action = Some("Adding source...".into());
                        }
                        echo_agent_hooks_plugins_types::MarketplaceAction::Uninstall { .. } => {
                            state.pending_action = Some("Uninstalling...".into());
                            state.pending_entry_index = Some(state.picker_state.selected);
                        }
                        _ => {
                            state.pending_action = Some("Processing...".into());
                            state.pending_entry_index = Some(state.picker_state.selected);
                        }
                    }
                }
                InputOutcome::Action(Action::ExecuteMarketplaceAction(marketplace_action))
            }
            ButtonAction::RemoveSelectedHook => {
                if let Some(ref state) = self.extensions_modal {
                    use crate::views::extensions_modal::TabDataState;
                    if let TabDataState::Loaded(ref data) = state.hooks_data
                        && let Some(idx) = state.selected_data_index()
                        && let Some(hook) = data.hooks.get(idx)
                    {
                        let path = hook.source_dir.clone();
                        let (label, _) = crate::views::extensions_modal::derive_source_label(&path);
                        return self.prompt_extensions_confirm(
                            format!("Remove hook source \"{label}\"?"),
                            crate::views::extensions_modal::ConfirmationAction::Hooks(
                                echo_agent_hooks_plugins_types::HooksAction::Remove { path },
                            ),
                        );
                    }
                }
                InputOutcome::Changed
            }
            ButtonAction::ToggleSelectedHook => {
                if let Some(ref state) = self.extensions_modal
                    && let crate::views::extensions_modal::TabDataState::Loaded(ref data) =
                        state.hooks_data
                    && let Some(idx) = state.selected_data_index()
                    && let Some(hook) = data.hooks.get(idx)
                {
                    let source = &hook.source_dir;
                    let is_collapsed = state.hooks_collapsed_groups.contains(source);

                    if is_collapsed {
                        // Group toggle: collect all hooks in this source group.
                        let group_hooks: Vec<&echo_agent_hooks_plugins_types::HookInfo> = data
                            .hooks
                            .iter()
                            .filter(|h| h.source_dir == *source)
                            .collect();
                        let any_enabled = group_hooks.iter().any(|h| !h.disabled);
                        let hook_names: Vec<String> =
                            group_hooks.iter().map(|h| h.name.clone()).collect();
                        let action = echo_agent_hooks_plugins_types::HooksAction::ToggleSource {
                            hook_names,
                            disable: any_enabled,
                        };
                        return self.execute_modal_button_action(ButtonAction::HooksAction(action));
                    } else {
                        // Single hook toggle.
                        let action = if hook.disabled {
                            echo_agent_hooks_plugins_types::HooksAction::Enable {
                                hook_name: hook.name.clone(),
                            }
                        } else {
                            echo_agent_hooks_plugins_types::HooksAction::Disable {
                                hook_name: hook.name.clone(),
                            }
                        };
                        return self.execute_modal_button_action(ButtonAction::HooksAction(action));
                    }
                }
                InputOutcome::Changed
            }
            ButtonAction::ToggleSelectedPlugin => {
                if let Some(ref state) = self.extensions_modal
                    && let crate::views::extensions_modal::TabDataState::Loaded(ref data) =
                        state.plugins_data
                    && let Some(idx) = state.selected_data_index()
                    && let Some(plugin) = data.plugins.get(idx)
                {
                    let action = if plugin.enabled {
                        echo_agent_hooks_plugins_types::PluginsAction::Disable {
                            plugin_id: plugin.id.clone(),
                        }
                    } else {
                        echo_agent_hooks_plugins_types::PluginsAction::Enable {
                            plugin_id: plugin.id.clone(),
                        }
                    };
                    return self.execute_modal_button_action(ButtonAction::PluginsAction(action));
                }
                InputOutcome::Changed
            }
            ButtonAction::ToggleSelectedSkill => {
                if let Some(ref mut state) = self.extensions_modal {
                    use crate::views::extensions_modal::TabDataState;
                    if let TabDataState::Loaded(ref skills) = state.skills_data
                        && let Some(idx) = state.selected_data_index()
                        && let Some(skill) = skills.get(idx)
                    {
                        state.pending_action = Some("toggling...".into());
                        state.pending_entry_index = Some(state.picker_state.selected);
                        return InputOutcome::Action(Action::ToggleSkill {
                            skill_name: skill.name.clone(),
                            enabled: !skill.enabled,
                        });
                    }
                }
                InputOutcome::Changed
            }
            ButtonAction::UninstallSelectedPlugin => {
                if let Some(ref state) = self.extensions_modal
                    && let crate::views::extensions_modal::TabDataState::Loaded(ref data) =
                        state.plugins_data
                    && let Some(idx) = state.selected_data_index()
                    && let Some(plugin) = data.plugins.get(idx)
                {
                    let plugin_id = plugin.id.clone();
                    let name = plugin.name.clone();
                    return self.prompt_extensions_confirm(
                        format!("Uninstall plugin \"{name}\"?"),
                        crate::views::extensions_modal::ConfirmationAction::Plugins(
                            echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                                plugin_id,
                                // Server owns multi-plugin cascade text when count > 1.
                                confirmed: false,
                            },
                        ),
                    );
                }
                InputOutcome::Changed
            }
            ButtonAction::UpdateSelectedPlugin => {
                // Fetch latest from the plugin's source for the selected plugin
                // only (`plugin_id: Some(..)`) — distinct from `r` reload, which
                // re-copies installed plugins at their current version.
                if let Some(ref state) = self.extensions_modal
                    && let crate::views::extensions_modal::TabDataState::Loaded(ref data) =
                        state.plugins_data
                    && let Some(idx) = state.selected_data_index()
                    && let Some(plugin) = data.plugins.get(idx)
                {
                    let action = echo_agent_hooks_plugins_types::PluginsAction::Update {
                        plugin_id: Some(plugin.id.clone()),
                    };
                    return self.execute_modal_button_action(ButtonAction::PluginsAction(action));
                }
                InputOutcome::Changed
            }
            ButtonAction::ToggleExpand => {
                // Same logic as Space key — toggle collapse on current tab.
                if let Some(ref mut state) = self.extensions_modal {
                    use crate::views::extensions_modal::{ExtensionsTab, TabDataState};
                    match state.active_tab {
                        ExtensionsTab::Hooks => {
                            if let TabDataState::Loaded(ref data) = state.hooks_data
                                && let Some(idx) = state.selected_data_index()
                                && let Some(hook) = data.hooks.get(idx)
                            {
                                let key = hook.source_dir.clone();
                                if !state.hooks_collapsed_groups.remove(&key) {
                                    state.hooks_collapsed_groups.insert(key);
                                }
                            }
                        }
                        ExtensionsTab::Plugins => {
                            let sel = state.picker_state.selected;
                            if let Some(gk) = state
                                .entry_group_keys
                                .get(sel)
                                .and_then(|k| k.as_ref())
                                .cloned()
                            {
                                if !state.plugins_collapsed_groups.remove(&gk) {
                                    state.plugins_collapsed_groups.insert(gk);
                                }
                            } else if state.picker_state.expanded.contains(&sel) {
                                state.picker_state.expanded.remove(&sel);
                            } else {
                                state.picker_state.expanded.insert(sel);
                            }
                        }
                        ExtensionsTab::Marketplace => {
                            if let TabDataState::Loaded(ref data) = state.marketplace_data {
                                match state.resolve_marketplace_selection(&data.sources) {
                                    Some((source_index, None)) => {
                                        let has_error = data
                                            .sources
                                            .get(source_index)
                                            .and_then(|s| s.error.as_ref())
                                            .is_some();
                                        if has_error {
                                            let sel = state.picker_state.selected;
                                            if state.picker_state.expanded.contains(&sel) {
                                                state.picker_state.expanded.remove(&sel);
                                            } else {
                                                state.picker_state.expanded.insert(sel);
                                            }
                                        } else if !state
                                            .marketplace_collapsed_sources
                                            .remove(&source_index)
                                        {
                                            state
                                                .marketplace_collapsed_sources
                                                .insert(source_index);
                                        }
                                    }
                                    Some((_, Some(_))) => {
                                        let sel = state.picker_state.selected;
                                        if state.picker_state.expanded.contains(&sel) {
                                            state.picker_state.expanded.remove(&sel);
                                        } else {
                                            state.picker_state.expanded.insert(sel);
                                        }
                                    }
                                    None => {}
                                }
                            }
                        }
                        ExtensionsTab::McpServers => {
                            let sel = state.picker_state.selected;
                            if let Some(gk) = state
                                .entry_group_keys
                                .get(sel)
                                .and_then(|k| k.as_ref())
                                .map(|s| s.as_str())
                            {
                                if gk.starts_with("mcp-section:") {
                                    if !state.mcps_collapsed_sections.remove(gk) {
                                        state.mcps_collapsed_sections.insert(gk.to_string());
                                    }
                                } else if let Some(si) =
                                    crate::views::extensions_modal::parse_mcp_tools_server_index(gk)
                                {
                                    if state.mcps_tools_expanded.contains(&si) {
                                        state.mcps_tools_expanded.remove(&si);
                                    } else {
                                        state.mcps_tools_expanded.insert(si);
                                    }
                                }
                            }
                        }
                        // Workflows rows carry no group keys, so only the
                        // detail-expansion branch applies on that tab.
                        ExtensionsTab::Skills | ExtensionsTab::Workflows => {
                            let sel = state.picker_state.selected;
                            if let Some(gk) = state
                                .entry_group_keys
                                .get(sel)
                                .and_then(|k| k.as_ref())
                                .cloned()
                            {
                                if !state.skills_collapsed_groups.remove(&gk) {
                                    state.skills_collapsed_groups.insert(gk);
                                }
                            } else if state.picker_state.expanded.contains(&sel) {
                                state.picker_state.expanded.remove(&sel);
                            } else {
                                state.picker_state.expanded.insert(sel);
                            }
                        }
                    }
                }
                InputOutcome::Changed
            }
            ButtonAction::CycleFilter => {
                if let Some(ref mut state) = self.extensions_modal {
                    use crate::views::extensions_modal::{ExtensionsTab, TabDataState};
                    match state.active_tab {
                        ExtensionsTab::Plugins => {
                            state.plugins_filter = state.plugins_filter.next();
                            state.picker_state.selected = 0;
                        }
                        ExtensionsTab::McpServers => {
                            state.mcps_filter = state.mcps_filter.next();
                            state.picker_state.selected = 0;
                        }
                        ExtensionsTab::Hooks => {
                            let new_filter = state.hooks_filter.next();
                            state.hooks_filter = new_filter;
                            if let TabDataState::Loaded(ref data) = state.hooks_data {
                                state.picker_state.selected = data
                                    .hooks
                                    .iter()
                                    .position(|h| {
                                        crate::views::extensions_modal::fuzzy_matches_hook(
                                            h,
                                            state.picker_state.query(),
                                        ) && new_filter.matches(!h.disabled)
                                    })
                                    .unwrap_or(0);
                            }
                        }
                        ExtensionsTab::Skills => {
                            state.skills_filter = state.skills_filter.next();
                            state.picker_state.selected = 0;
                        }
                        _ => {}
                    }
                }
                InputOutcome::Changed
            }
            ButtonAction::InstallSelectedMarketplacePlugin => self
                .execute_selected_marketplace_plugin_action(
                    "Installing...",
                    |source_url_or_path, plugin_relative_path| {
                        echo_agent_hooks_plugins_types::MarketplaceAction::Install {
                            source_url_or_path,
                            plugin_relative_path,
                        }
                    },
                ),
            ButtonAction::UpdateSelectedMarketplacePlugin => self
                .execute_selected_marketplace_plugin_action(
                    "Updating...",
                    |source_url_or_path, plugin_relative_path| {
                        echo_agent_hooks_plugins_types::MarketplaceAction::Update {
                            source_url_or_path,
                            plugin_relative_path,
                        }
                    },
                ),
            ButtonAction::StartInput {
                command_prefix,
                fields,
            } => {
                if let Some(ref mut state) = self.extensions_modal {
                    state.modal_message = None;
                    state.input = Some(ModalInput::from_specs(command_prefix, fields));
                }
                InputOutcome::Changed
            }
            ButtonAction::UninstallSelectedMarketplacePlugin => {
                if let Some(ref state) = self.extensions_modal {
                    use crate::views::extensions_modal::TabDataState;
                    if let TabDataState::Loaded(ref response) = state.marketplace_data
                        && let Some((si, Some(pi))) =
                            state.resolve_marketplace_selection(&response.sources)
                    {
                        let source = &response.sources[si];
                        let plugin = &source.plugins[pi];
                        return self.prompt_extensions_confirm(
                            format!("Uninstall marketplace plugin \"{}\"?", plugin.name),
                            crate::views::extensions_modal::ConfirmationAction::Marketplace(
                                echo_agent_hooks_plugins_types::MarketplaceAction::Uninstall {
                                    source_url_or_path: source.source_url_or_path.clone(),
                                    plugin_relative_path: plugin.relative_path.clone(),
                                },
                            ),
                        );
                    }
                }
                InputOutcome::Changed
            }
            ButtonAction::RemoveSelectedMarketplaceSource => {
                if let Some(ref state) = self.extensions_modal {
                    use crate::views::extensions_modal::TabDataState;
                    if let TabDataState::Loaded(ref response) = state.marketplace_data {
                        let source = state
                            .resolve_marketplace_selection(&response.sources)
                            .and_then(|(si, _)| response.sources.get(si));
                        if let Some(source) = source {
                            return self.prompt_extensions_confirm(
                                format!(
                                    "Remove source \"{}\" and uninstall all its plugins?",
                                    source.source_name
                                ),
                                crate::views::extensions_modal::ConfirmationAction::Marketplace(
                                    echo_agent_hooks_plugins_types::MarketplaceAction::RemoveSource {
                                        source_url_or_path: source.source_url_or_path.clone(),
                                    },
                                ),
                            );
                        }
                    }
                }
                InputOutcome::Changed
            }
        }
    }

    fn prompt_extensions_confirm(
        &mut self,
        message: String,
        action: crate::views::extensions_modal::ConfirmationAction,
    ) -> InputOutcome {
        if let Some(ref mut state) = self.extensions_modal {
            let pending_entry_index = Some(state.picker_state.selected);
            state.modal_message =
                Some(crate::views::extensions_modal::ModalMessage::Confirmation {
                    message,
                    action,
                    pending_entry_index,
                });
            state.pending_action = None;
            state.pending_entry_index = None;
            state.picker_state.link_band = None;
        }
        InputOutcome::Changed
    }

    fn confirm_extensions_modal_action(
        &mut self,
        action: crate::views::extensions_modal::ConfirmationAction,
        pending_entry_index: Option<usize>,
    ) -> InputOutcome {
        use crate::views::extensions_modal::{ButtonAction, ConfirmationAction};

        let outcome = match action {
            ConfirmationAction::Hooks(hooks_action) => {
                self.execute_modal_button_action(ButtonAction::HooksAction(hooks_action))
            }
            ConfirmationAction::Plugins(plugins_action) => {
                self.execute_modal_button_action(ButtonAction::PluginsAction(plugins_action))
            }
            ConfirmationAction::Marketplace(marketplace_action) => self
                .execute_modal_button_action(ButtonAction::MarketplaceAction(marketplace_action)),
            ConfirmationAction::DeleteMcpServer { server_name } => {
                if let Some(ref mut s) = self.extensions_modal {
                    s.pending_action = Some("removing...".into());
                }
                InputOutcome::Action(Action::DeleteMcpServer { server_name })
            }
        };
        // Low-level arms stamp picker_state.selected; overwrite with the row
        // captured when the prompt opened (scroll can move selection under the overlay).
        if let Some(ref mut state) = self.extensions_modal {
            state.pending_entry_index = pending_entry_index;
        }
        outcome
    }

    fn execute_selected_marketplace_plugin_action(
        &mut self,
        pending_label: &'static str,
        make_action: impl FnOnce(String, String) -> echo_agent_hooks_plugins_types::MarketplaceAction,
    ) -> InputOutcome {
        if let Some(ref mut state) = self.extensions_modal {
            use crate::views::extensions_modal::TabDataState;
            if let TabDataState::Loaded(ref response) = state.marketplace_data
                && let Some((si, Some(pi))) = state.resolve_marketplace_selection(&response.sources)
            {
                let source = &response.sources[si];
                let plugin = &source.plugins[pi];
                state.pending_action = Some(pending_label.into());
                state.pending_entry_index = Some(state.picker_state.selected);
                let action = make_action(
                    source.source_url_or_path.clone(),
                    plugin.relative_path.clone(),
                );
                return InputOutcome::Action(Action::ExecuteMarketplaceAction(action));
            }
        }
        InputOutcome::Changed
    }
}

#[cfg(test)]
mod marketplace_modal_action_tests {
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crate::views::extensions_modal::{
        ButtonAction, ExtensionsModalState, ExtensionsTab, TabDataState,
    };

    pub(super) fn marketplace_plugin(
        name: &str,
        relative_path: &str,
    ) -> echo_agent_hooks_plugins_types::MarketplacePluginEntry {
        echo_agent_hooks_plugins_types::MarketplacePluginEntry {
            name: name.into(),
            version: Some("2.0.0".into()),
            description: None,
            category: None,
            author: None,
            tags: Vec::new(),
            keywords: Vec::new(),
            domains: Vec::new(),
            homepage: None,
            relative_path: relative_path.into(),
            skill_count: 0,
            has_hooks: false,
            has_agents: false,
            has_mcp: false,
            install_status: "update_available".into(),
            installed_version: Some("1.0.0".into()),
            components: None,
            remote_url: None,
            remote_ref: None,
            remote_sha: None,
            remote_subdir: None,
        }
    }

    #[test]
    fn update_selected_marketplace_plugin_dispatches_update_and_sets_pending_state() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        modal.marketplace_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::MarketplaceListResponse {
                sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
                    source_name: "test-source".into(),
                    source_kind: "git".into(),
                    source_url_or_path: "https://example.com/plugins.git".into(),
                    plugins: vec![marketplace_plugin("test-plugin", "plugins/test-plugin")],
                    error: None,
                }],
            });
        modal.entry_labels_cache = vec!["test-source".into(), "test-plugin".into()];
        modal.entry_group_keys = vec![Some("0".into()), None];
        modal.entry_data_indices = vec![None, Some(0)];
        modal.picker_state.selected = 1;
        agent.extensions_modal = Some(modal);

        let outcome =
            agent.execute_modal_button_action(ButtonAction::UpdateSelectedMarketplacePlugin);

        match outcome {
            InputOutcome::Action(Action::ExecuteMarketplaceAction(
                echo_agent_hooks_plugins_types::MarketplaceAction::Update {
                    source_url_or_path,
                    plugin_relative_path,
                },
            )) => {
                assert_eq!(source_url_or_path, "https://example.com/plugins.git");
                assert_eq!(plugin_relative_path, "plugins/test-plugin");
            }
            other => panic!("expected marketplace update action, got {other:?}"),
        }
        let state = agent
            .extensions_modal
            .as_ref()
            .expect("modal should remain open");
        assert_eq!(state.pending_action.as_deref(), Some("Updating..."));
        assert_eq!(state.pending_entry_index, Some(1));
    }

    #[test]
    fn refresh_marketplace_sets_tab_loading_state_not_row_pending() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        modal.marketplace_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::MarketplaceListResponse {
                sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
                    source_name: "test-source".into(),
                    source_kind: "git".into(),
                    source_url_or_path: "https://example.com/plugins.git".into(),
                    plugins: vec![marketplace_plugin("test-plugin", "plugins/test-plugin")],
                    error: None,
                }],
            });
        modal.entry_labels_cache = vec!["test-source".into(), "test-plugin".into()];
        modal.entry_group_keys = vec![Some("0".into()), None];
        modal.entry_data_indices = vec![None, Some(0)];
        modal.picker_state.selected = 1;
        agent.extensions_modal = Some(modal);

        let outcome = agent.execute_modal_button_action(ButtonAction::MarketplaceAction(
            echo_agent_hooks_plugins_types::MarketplaceAction::Refresh {
                source_url_or_path: None,
            },
        ));

        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::ExecuteMarketplaceAction(
                echo_agent_hooks_plugins_types::MarketplaceAction::Refresh {
                    source_url_or_path: None
                }
            ))
        ));
        let state = agent
            .extensions_modal
            .as_ref()
            .expect("modal should remain open");
        assert!(matches!(state.marketplace_data, TabDataState::Loading));
        assert_eq!(state.pending_entry_index, None);
    }
}

#[cfg(test)]
mod extensions_action_target_tests {
    use super::AgentView;
    use crate::views::extensions_modal::{
        ButtonAction, ExtensionsModalState, ExtensionsTab, TabDataState,
    };

    fn plugin_info(name: &str, enabled: bool) -> echo_agent_hooks_plugins_types::PluginInfo {
        echo_agent_hooks_plugins_types::PluginInfo {
            name: name.into(),
            id: format!("user/abcd1234/{name}"),
            root: "/tmp/p".into(),
            scope: echo_agent_hooks_plugins_types::PluginScope::User,
            trusted: true,
            enabled,
            version: None,
            description: None,
            skill_count: 0,
            skill_names: Vec::new(),
            agent_count: 0,
            agent_names: Vec::new(),
            hook_status: echo_agent_hooks_plugins_types::HookStatus::None,
            hook_count: 0,
            mcp_server_count: 0,
            mcp_status: echo_agent_hooks_plugins_types::McpStatus::None,
            marketplace_source: None,
            origin: None,
            conflict: None,
        }
    }

    fn server_info(name: &str, enabled: bool) -> crate::views::mcps_modal::McpServerInfo {
        crate::views::mcps_modal::McpServerInfo {
            name: name.into(),
            display_name: None,
            status: crate::views::mcps_modal::McpServerDisplayStatus::Initializing,
            tool_count: 0,
            auth_required: false,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: Vec::new(),
            enabled,
            source: "local".into(),
            wire_source: crate::views::mcps_modal::McpWireSource::Local,
            plugin_name: None,
            is_managed_gateway: false,
        }
    }

    #[test]
    fn plugins_toggle_and_uninstall_resolve_name_and_state() {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        modal.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![plugin_info("my-plugin", true)],
            });
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::ToggleSelectedPlugin);
        assert_eq!(target.as_deref(), Some("my-plugin"));
        assert_eq!(enabled, Some(false));

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::UninstallSelectedPlugin);
        assert_eq!(target.as_deref(), Some("my-plugin"));
        assert_eq!(enabled, None);
    }

    #[test]
    fn update_selected_plugin_dispatches_update_with_selected_id_and_pending_state() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        modal.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![plugin_info("my-plugin", true)],
            });
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;

        // Telemetry target resolves to the selected plugin (parity with toggle/uninstall).
        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::UpdateSelectedPlugin);
        assert_eq!(target.as_deref(), Some("my-plugin"));
        assert_eq!(enabled, None);

        agent.extensions_modal = Some(modal);
        let outcome = agent.execute_modal_button_action(ButtonAction::UpdateSelectedPlugin);
        match outcome {
            crate::app::app_view::InputOutcome::Action(
                crate::app::actions::Action::ExecutePluginsAction(
                    echo_agent_hooks_plugins_types::PluginsAction::Update { plugin_id },
                ),
            ) => assert_eq!(plugin_id.as_deref(), Some("user/abcd1234/my-plugin")),
            other => panic!("expected plugins update action, got {other:?}"),
        }
        let state = agent.extensions_modal.as_ref().expect("modal stays open");
        assert_eq!(state.pending_action.as_deref(), Some("Updating..."));
        assert_eq!(state.pending_entry_index, Some(0));
    }

    #[test]
    fn plugins_cycle_filter_resets_selection_to_top() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        modal.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![plugin_info("my-plugin", true)],
            });
        modal.picker_state.selected = 5;
        agent.extensions_modal = Some(modal);

        agent.execute_modal_button_action(ButtonAction::CycleFilter);
        let state = agent.extensions_modal.as_ref().unwrap();
        assert_eq!(
            state.plugins_filter,
            crate::views::extensions_modal::StatusFilter::Enabled
        );
        assert_eq!(state.picker_state.selected, 0);
    }

    #[test]
    fn plugins_toggle_expand_folds_group_header_and_expands_row_details() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        modal.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![plugin_info("my-plugin", true)],
            });
        modal.entry_data_indices = vec![None, Some(0)];
        modal.entry_group_keys = vec![Some("origin:user".into()), None];
        modal.picker_state.selected = 0;
        agent.extensions_modal = Some(modal);

        agent.execute_modal_button_action(ButtonAction::ToggleExpand);
        assert!(
            agent
                .extensions_modal
                .as_ref()
                .unwrap()
                .plugins_collapsed_groups
                .contains("origin:user"),
            "toggle on a header collapses its group"
        );

        agent.execute_modal_button_action(ButtonAction::ToggleExpand);
        assert!(
            !agent
                .extensions_modal
                .as_ref()
                .unwrap()
                .plugins_collapsed_groups
                .contains("origin:user"),
            "second toggle re-expands the group"
        );

        agent
            .extensions_modal
            .as_mut()
            .unwrap()
            .picker_state
            .selected = 1;
        agent.execute_modal_button_action(ButtonAction::ToggleExpand);
        let state = agent.extensions_modal.as_ref().unwrap();
        assert!(
            state.picker_state.expanded.contains(&1),
            "toggle on a plugin row expands its detail fields"
        );
        assert!(state.plugins_collapsed_groups.is_empty());
    }

    #[test]
    fn skills_toggle_resolves_name_and_resulting_state() {
        let skill = echo_agent_tools::implementations::skills::types::SkillInfo {
            name: "my-skill".into(),
            enabled: false,
            ..Default::default()
        };
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Skills);
        modal.skills_data = TabDataState::Loaded(vec![skill]);
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::ToggleSelectedSkill);
        assert_eq!(target.as_deref(), Some("my-skill"));
        assert_eq!(enabled, Some(true));
    }

    #[test]
    fn mcp_toggle_auth_remove_resolve_server_name() {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::McpServers);
        modal.mcps_data = TabDataState::Loaded(vec![server_info("my-server", false)]);
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::ToggleSelectedMcpServer);
        assert_eq!(target.as_deref(), Some("my-server"));
        assert_eq!(enabled, Some(true));

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::McpAuthTrigger);
        assert_eq!(target.as_deref(), Some("my-server"));
        assert_eq!(enabled, None);

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::RemoveSelectedMcpServer);
        assert_eq!(target.as_deref(), Some("my-server"));
        assert_eq!(enabled, None);
    }

    #[test]
    fn marketplace_actions_resolve_plugin_and_source_names() {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        modal.marketplace_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::MarketplaceListResponse {
                sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
                    source_name: "test-source".into(),
                    source_kind: "git".into(),
                    source_url_or_path: "https://example.com/plugins.git".into(),
                    plugins: vec![super::marketplace_modal_action_tests::marketplace_plugin(
                        "test-plugin",
                        "plugins/test-plugin",
                    )],
                    error: None,
                }],
            });
        modal.entry_labels_cache = vec!["test-source".into(), "test-plugin".into()];
        modal.entry_group_keys = vec![Some("0".into()), None];
        modal.entry_data_indices = vec![None, Some(0)];
        modal.picker_state.selected = 1;

        for action in [
            ButtonAction::InstallSelectedMarketplacePlugin,
            ButtonAction::UpdateSelectedMarketplacePlugin,
            ButtonAction::UninstallSelectedMarketplacePlugin,
        ] {
            let (target, enabled) = AgentView::extensions_action_target(&modal, &action);
            assert_eq!(target.as_deref(), Some("test-plugin"), "{action:?}");
            assert_eq!(enabled, None);
        }

        let (target, enabled) = AgentView::extensions_action_target(
            &modal,
            &ButtonAction::RemoveSelectedMarketplaceSource,
        );
        assert_eq!(target.as_deref(), Some("test-source"));
        assert_eq!(enabled, None);
    }

    fn hook_info(
        name: &str,
        source_dir: &str,
        disabled: bool,
    ) -> echo_agent_hooks_plugins_types::HookInfo {
        echo_agent_hooks_plugins_types::HookInfo {
            name: name.into(),
            event: echo_agent_hooks_plugins_types::HookEvent::PreToolUse,
            handler_type: echo_agent_hooks_plugins_types::HookHandlerType::Command,
            matcher: None,
            command: None,
            url: None,
            timeout_ms: 0,
            source_dir: source_dir.into(),
            disabled,
        }
    }

    fn hooks_modal(hooks: Vec<echo_agent_hooks_plugins_types::HookInfo>) -> ExtensionsModalState {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Hooks);
        modal.hooks_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::HooksListResponse {
                hooks,
                project_trusted: true,
                load_errors: Vec::new(),
            });
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;
        modal
    }

    #[test]
    fn hook_toggle_resolves_single_hook_when_group_expanded() {
        let modal = hooks_modal(vec![
            hook_info("src/hook-a", "/tmp/hooks", true),
            hook_info("src/hook-b", "/tmp/hooks", false),
        ]);

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::ToggleSelectedHook);
        assert_eq!(target.as_deref(), Some("src/hook-a"));
        assert_eq!(enabled, Some(true));

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::RemoveSelectedHook);
        assert_eq!(target.as_deref(), Some("src/hook-a"));
        assert_eq!(enabled, None);
    }

    #[test]
    fn hook_toggle_resolves_group_state_when_group_collapsed() {
        let mut modal = hooks_modal(vec![
            hook_info("src/hook-a", "/tmp/hooks", true),
            hook_info("src/hook-b", "/tmp/hooks", false),
        ]);
        modal.hooks_collapsed_groups.insert("/tmp/hooks".into());

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::ToggleSelectedHook);
        let expected_label = crate::views::extensions_modal::derive_source_label("/tmp/hooks").0;
        assert_eq!(target.as_deref(), Some(expected_label.as_str()));
        assert_eq!(enabled, Some(false));
    }

    #[test]
    fn mcp_tool_row_with_stale_indices_yields_no_target() {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::McpServers);
        modal.mcps_data = TabDataState::Loaded(vec![server_info("my-server", true)]);
        modal.entry_data_indices = vec![Some(0), Some(0)];
        modal.entry_group_keys = vec![Some("mcp-tools:0".into()), None];
        modal.mcps_tools_expanded.insert(0);
        modal.picker_state.selected = 1;
        assert!(modal.selected_mcp_tool().is_some());

        let (target, enabled) =
            AgentView::extensions_action_target(&modal, &ButtonAction::ToggleSelectedMcpServer);
        assert_eq!(target, None);
        assert_eq!(enabled, None);
    }

    #[test]
    fn loading_data_yields_no_target() {
        let modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        for action in [
            ButtonAction::ToggleSelectedPlugin,
            ButtonAction::ToggleSelectedSkill,
            ButtonAction::ToggleSelectedMcpServer,
            ButtonAction::InstallSelectedMarketplacePlugin,
            ButtonAction::PluginsAction(echo_agent_hooks_plugins_types::PluginsAction::Reload),
        ] {
            let (target, enabled) = AgentView::extensions_action_target(&modal, &action);
            assert_eq!(target, None, "{action:?}");
            assert_eq!(enabled, None, "{action:?}");
        }
    }
}

#[cfg(test)]
mod extensions_modal_search_key_tests {
    use crate::app::app_view::InputOutcome;
    use crate::views::extensions_modal::{ExtensionsModalState, ExtensionsTab};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[test]
    fn esc_on_empty_search_exits_search_keeps_modal_open() {
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        let outcome = agent.handle_extensions_modal_key(&key(KeyCode::Char('/')));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent
                .extensions_modal
                .as_ref()
                .unwrap()
                .picker_state
                .search_active,
            "`/` should activate search"
        );

        let outcome = agent.handle_extensions_modal_key(&key(KeyCode::Esc));
        assert!(matches!(outcome, InputOutcome::Changed));
        let state = agent
            .extensions_modal
            .as_ref()
            .expect("Esc on an empty search must not close the modal");
        assert!(
            !state.picker_state.search_active,
            "Esc should deactivate search"
        );
        assert!(state.picker_state.query().is_empty());
    }

    #[test]
    fn esc_after_canceling_empty_search_closes_modal() {
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        agent.handle_extensions_modal_key(&key(KeyCode::Char('/')));
        agent.handle_extensions_modal_key(&key(KeyCode::Esc));
        assert!(agent.extensions_modal.is_some(), "first Esc cancels search");

        agent.handle_extensions_modal_key(&key(KeyCode::Esc));
        assert!(
            agent.extensions_modal.is_none(),
            "Esc with search inactive and empty query closes the modal"
        );
    }

    #[test]
    fn esc_without_search_closes_modal_immediately() {
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        let outcome = agent.handle_extensions_modal_key(&key(KeyCode::Esc));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.extensions_modal.is_none());
    }

    #[test]
    fn esc_with_typed_query_exits_search_keeps_modal_open() {
        // Pin vim-mode off; this test asserts the non-vim picker path.
        crate::appearance::cache::set_vim_mode(false);
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        agent.handle_extensions_modal_key(&key(KeyCode::Char('/')));
        agent.handle_extensions_modal_key(&key(KeyCode::Char('a')));
        {
            let state = agent.extensions_modal.as_ref().unwrap();
            assert!(state.picker_state.search_active);
            assert_eq!(state.picker_state.query(), "a");
        }

        agent.handle_extensions_modal_key(&key(KeyCode::Esc));
        {
            let state = agent
                .extensions_modal
                .as_ref()
                .expect("modal stays open while a query is present");
            assert!(!state.picker_state.search_active);
            assert_eq!(state.picker_state.query(), "a");
        }

        agent.handle_extensions_modal_key(&key(KeyCode::Esc));
        {
            let state = agent
                .extensions_modal
                .as_ref()
                .expect("clearing the retained query keeps the modal open");
            assert!(!state.picker_state.search_active);
            assert!(state.picker_state.query().is_empty());
        }

        agent.handle_extensions_modal_key(&key(KeyCode::Esc));
        assert!(
            agent.extensions_modal.is_none(),
            "Esc with no search and no query closes the modal"
        );
    }

    #[test]
    fn tab_during_active_search_switches_tab_and_keeps_query() {
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        agent.handle_extensions_modal_key(&key(KeyCode::Char('/')));
        agent.handle_extensions_modal_key(&key(KeyCode::Char('g')));

        let outcome = agent.handle_extensions_modal_key(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        let state = agent
            .extensions_modal
            .as_ref()
            .expect("Tab during search keeps the modal open");
        assert_eq!(state.active_tab, ExtensionsTab::Marketplace);
        assert!(
            state.picker_state.search_active,
            "search stays active across a tab switch"
        );
        assert_eq!(
            state.picker_state.query(),
            "g",
            "the query carries over to the new tab"
        );
    }

    #[test]
    fn back_tab_during_active_search_switches_to_previous_tab() {
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        agent.handle_extensions_modal_key(&key(KeyCode::Char('/')));
        agent.handle_extensions_modal_key(&key(KeyCode::Char('g')));

        agent.handle_extensions_modal_key(&key(KeyCode::BackTab));
        let state = agent.extensions_modal.as_ref().unwrap();
        assert_eq!(state.active_tab, ExtensionsTab::Hooks);
        assert!(state.picker_state.search_active);
        assert_eq!(state.picker_state.query(), "g");
    }

    #[test]
    fn shift_tab_during_active_search_switches_to_previous_tab() {
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::Plugins));

        agent.handle_extensions_modal_key(&key(KeyCode::Char('/')));
        agent.handle_extensions_modal_key(&key(KeyCode::Char('g')));

        agent.handle_extensions_modal_key(&shift_key(KeyCode::Tab));
        let state = agent.extensions_modal.as_ref().unwrap();
        assert_eq!(state.active_tab, ExtensionsTab::Hooks);
        assert!(state.picker_state.search_active);
        assert_eq!(state.picker_state.query(), "g");
    }

    #[test]
    fn tab_during_search_wraps_around_tabs() {
        let mut agent = super::test_fixtures::make_agent();
        agent.extensions_modal = Some(ExtensionsModalState::new(ExtensionsTab::McpServers));

        agent.handle_extensions_modal_key(&key(KeyCode::Char('/')));
        agent.handle_extensions_modal_key(&key(KeyCode::Tab));
        let state = agent.extensions_modal.as_ref().unwrap();
        assert_eq!(state.active_tab, ExtensionsTab::Hooks);
        assert!(state.picker_state.search_active);
    }
}

#[cfg(test)]
mod connectors_url_click_tests {
    use super::AgentView;
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crate::views::extensions_modal::{
        ExtensionsModalState, ExtensionsTab, TabDataState, render_extensions_modal,
    };
    use crate::views::mcps_modal::{McpServerDisplayStatus, McpServerInfo, McpWireSource};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn managed_server() -> McpServerInfo {
        McpServerInfo {
            name: "echo_agent_com_linear".into(),
            display_name: None,
            status: McpServerDisplayStatus::Ready,
            tool_count: 0,
            auth_required: false,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: vec![],
            enabled: true,
            source: "managed".into(),
            wire_source: McpWireSource::Managed,
            plugin_name: None,
            is_managed_gateway: false,
        }
    }

    // Build an agent whose extensions modal shows an expanded Managed section,
    // then paint it so `hit_areas` + `link_band` reflect the real layout.
    fn rendered_agent() -> AgentView {
        let mut agent = super::test_fixtures::make_agent();
        let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
        state.mcps_data = TabDataState::Loaded(vec![managed_server()]);
        agent.extensions_modal = Some(state);
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_extensions_modal(
            &mut buf,
            area,
            agent.extensions_modal.as_mut().unwrap(),
            None,
            false,
            0,
        );
        agent
    }

    fn left_down(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    // (column inside the Managed row, its recorded URL band) from the last paint.
    fn managed_url_hit(agent: &AgentView) -> (u16, std::ops::Range<u16>) {
        let state = agent.extensions_modal.as_ref().unwrap();
        let (entry_idx, band) = state
            .picker_state
            .link_band
            .clone()
            .expect("expanded Managed section records a connectors URL band");
        let hit = state.picker_state.hit_areas.as_ref().unwrap();
        let pos = hit
            .entry_indices
            .iter()
            .position(|&e| e == entry_idx)
            .unwrap();
        (hit.item_rects[pos].x + 2, band)
    }

    #[test]
    fn mouse_down_on_url_row_opens_connectors() {
        let mut agent = rendered_agent();
        let (col, band) = managed_url_hit(&agent);
        let outcome = agent.handle_extensions_modal_mouse(&left_down(col, band.start));
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::OpenManagedConnectors)
        ));
        // The section stays expanded (opened, did not fold).
        assert!(
            !agent
                .extensions_modal
                .as_ref()
                .unwrap()
                .mcps_collapsed_sections
                .contains("mcp-section:managed")
        );
    }

    #[test]
    fn mouse_down_on_label_row_folds_not_opens() {
        let mut agent = rendered_agent();
        let (col, label_row) = {
            let state = agent.extensions_modal.as_ref().unwrap();
            let (entry_idx, _band) = state.picker_state.link_band.clone().unwrap();
            let hit = state.picker_state.hit_areas.as_ref().unwrap();
            let pos = hit
                .entry_indices
                .iter()
                .position(|&e| e == entry_idx)
                .unwrap();
            let rect = hit.item_rects[pos];
            (rect.x + 2, rect.y) // first row of the item rect is the fold-toggle label
        };
        let outcome = agent.handle_extensions_modal_mouse(&left_down(col, label_row));
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::OpenManagedConnectors)
        ));
        // Fold happened: the Managed section is now collapsed.
        assert!(
            agent
                .extensions_modal
                .as_ref()
                .unwrap()
                .mcps_collapsed_sections
                .contains("mcp-section:managed")
        );
    }

    #[test]
    fn mouse_down_on_url_row_while_searching_does_not_open() {
        // Parity with the Ctrl+O guard: opening is suppressed while search is active.
        let mut agent = rendered_agent();
        let (col, band) = managed_url_hit(&agent);
        agent
            .extensions_modal
            .as_mut()
            .unwrap()
            .picker_state
            .search_active = true;
        let outcome = agent.handle_extensions_modal_mouse(&left_down(col, band.start));
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::OpenManagedConnectors)
        ));
    }
}

#[cfg(test)]
mod editor_paste_routing_tests {
    use std::collections::HashMap;

    use super::test_fixtures::make_agent;
    use crate::actions::ActionRegistry;
    use crate::app::bundle::BundleState;
    use crate::views::agents_modal::{AgentsModalState, AgentsTab};
    use crate::views::extensions_modal::{
        ExtensionsModalState, ExtensionsTab, FieldSpec, ModalInput,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn persona_and_extensions_paste_only_into_active_forms() {
        let registry = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.prompt.set_text("hidden prompt");

        let cwd = tempfile::tempdir().expect("temp cwd");
        let mut agents = AgentsModalState::new(
            cwd.path(),
            &HashMap::new(),
            &BundleState::default(),
            None,
            None,
            None,
        );
        agents.active_tab = AgentsTab::Personas;
        agent.agents_modal = Some(agents);
        let _ = agent.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
            &registry,
        );
        let _ = agent.handle_input(&Event::Paste("na\r\nme".to_owned()), &registry);
        assert_eq!(
            agent
                .agents_modal
                .as_ref()
                .and_then(|state| state.persona_input.as_ref())
                .map(|input| input.name()),
            Some("name")
        );
        assert_eq!(agent.prompt.text(), "hidden prompt");

        agent.agents_modal = None;
        let mut extensions = ExtensionsModalState::new(ExtensionsTab::McpServers);
        extensions.input = Some(ModalInput::from_specs(
            "mcp add".to_owned(),
            vec![FieldSpec {
                label: "URL".to_owned(),
                required: true,
                placeholder: None,
            }],
        ));
        agent.extensions_modal = Some(extensions);
        let _ = agent.handle_input(
            &Event::Paste("https://example.test\r\n".to_owned()),
            &registry,
        );
        assert_eq!(
            agent
                .extensions_modal
                .as_ref()
                .and_then(|state| state.input.as_ref())
                .and_then(|input| input.field(0))
                .map(|field| field.text()),
            Some("https://example.test")
        );
        assert_eq!(agent.prompt.text(), "hidden prompt");
    }
}

#[cfg(test)]
mod extensions_modal_confirmation_tests {
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crate::views::extensions_modal::{
        ButtonAction, ConfirmationAction, ExtensionsModalState, ExtensionsTab, ModalMessage,
        TabDataState,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn plugin_info(name: &str) -> echo_agent_hooks_plugins_types::PluginInfo {
        echo_agent_hooks_plugins_types::PluginInfo {
            name: name.into(),
            id: format!("user/abcd1234/{name}"),
            root: "/tmp/p".into(),
            scope: echo_agent_hooks_plugins_types::PluginScope::User,
            trusted: true,
            enabled: true,
            version: None,
            description: None,
            skill_count: 0,
            skill_names: Vec::new(),
            agent_count: 0,
            agent_names: Vec::new(),
            hook_status: echo_agent_hooks_plugins_types::HookStatus::None,
            hook_count: 0,
            mcp_server_count: 0,
            mcp_status: echo_agent_hooks_plugins_types::McpStatus::None,
            marketplace_source: None,
            origin: None,
            conflict: None,
        }
    }

    fn server_info(
        name: &str,
        wire_source: crate::views::mcps_modal::McpWireSource,
    ) -> crate::views::mcps_modal::McpServerInfo {
        crate::views::mcps_modal::McpServerInfo {
            name: name.into(),
            display_name: None,
            status: crate::views::mcps_modal::McpServerDisplayStatus::Initializing,
            tool_count: 0,
            auth_required: false,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: Vec::new(),
            enabled: true,
            source: "local".into(),
            wire_source,
            plugin_name: None,
            is_managed_gateway: false,
        }
    }

    fn hook_info(name: &str, source_dir: &str) -> echo_agent_hooks_plugins_types::HookInfo {
        echo_agent_hooks_plugins_types::HookInfo {
            name: name.into(),
            event: echo_agent_hooks_plugins_types::HookEvent::PreToolUse,
            handler_type: echo_agent_hooks_plugins_types::HookHandlerType::Command,
            matcher: None,
            command: None,
            url: None,
            timeout_ms: 0,
            source_dir: source_dir.into(),
            disabled: false,
        }
    }

    fn marketplace_loaded() -> TabDataState<echo_agent_hooks_plugins_types::MarketplaceListResponse>
    {
        TabDataState::Loaded(echo_agent_hooks_plugins_types::MarketplaceListResponse {
            sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: "test-source".into(),
                source_kind: "git".into(),
                source_url_or_path: "https://example.com/plugins.git".into(),
                plugins: vec![
                    super::marketplace_modal_action_tests::marketplace_plugin(
                        "plug-a",
                        "plugins/plug-a",
                    ),
                    super::marketplace_modal_action_tests::marketplace_plugin(
                        "plug-b",
                        "plugins/plug-b",
                    ),
                ],
                error: None,
            }],
        })
    }

    fn assert_prompt(
        state: &ExtensionsModalState,
        message_sub: &str,
        expected: &ConfirmationAction,
        row: usize,
    ) {
        match &state.modal_message {
            Some(ModalMessage::Confirmation {
                message,
                action,
                pending_entry_index,
            }) => {
                assert!(
                    message.contains(message_sub),
                    "message {message:?} missing {message_sub:?}"
                );
                assert_eq!(action, expected);
                assert_eq!(*pending_entry_index, Some(row));
            }
            other => panic!("expected Confirmation, got {other:?}"),
        }
        assert!(state.pending_action.is_none());
        assert!(state.pending_entry_index.is_none());
    }

    fn assert_no_action(outcome: InputOutcome) {
        assert!(
            matches!(outcome, InputOutcome::Changed | InputOutcome::Unchanged),
            "expected no dispatch, got {outcome:?}"
        );
    }

    struct PromptCase {
        modal: ExtensionsModalState,
        button: ButtonAction,
        message_sub: String,
        expected: ConfirmationAction,
        row: usize,
    }

    fn all_prompt_cases() -> Vec<PromptCase> {
        let mut mcp = ExtensionsModalState::new(ExtensionsTab::McpServers);
        mcp.mcps_data = TabDataState::Loaded(vec![
            server_info("alpha", crate::views::mcps_modal::McpWireSource::Local),
            server_info("beta", crate::views::mcps_modal::McpWireSource::Local),
        ]);
        mcp.entry_data_indices = vec![Some(0), Some(1)];
        mcp.entry_group_keys = vec![None, None];
        mcp.picker_state.selected = 0;

        let mut plugins = ExtensionsModalState::new(ExtensionsTab::Plugins);
        plugins.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![plugin_info("my-plugin")],
            });
        plugins.entry_data_indices = vec![Some(0)];
        plugins.entry_group_keys = vec![None];
        plugins.picker_state.selected = 0;

        let mut market_plugin = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        market_plugin.marketplace_data = marketplace_loaded();
        market_plugin.entry_labels_cache =
            vec!["test-source".into(), "plug-a".into(), "plug-b".into()];
        market_plugin.entry_group_keys = vec![Some("0".into()), None, None];
        market_plugin.entry_data_indices = vec![None, Some(0), Some(1)];
        market_plugin.picker_state.selected = 1;

        let mut market_source = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        market_source.marketplace_data = marketplace_loaded();
        market_source.entry_labels_cache = vec!["test-source".into(), "plug-a".into()];
        market_source.entry_group_keys = vec![Some("0".into()), None];
        market_source.entry_data_indices = vec![None, Some(0)];
        market_source.picker_state.selected = 0;

        let source = "/tmp/my-hooks-dir";
        let mut hooks = ExtensionsModalState::new(ExtensionsTab::Hooks);
        hooks.hooks_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::HooksListResponse {
                hooks: vec![hook_info("hook-a", source)],
                project_trusted: true,
                load_errors: Vec::new(),
            });
        hooks.entry_data_indices = vec![Some(0)];
        hooks.entry_group_keys = vec![None];
        hooks.picker_state.selected = 0;
        let hook_label = crate::views::extensions_modal::derive_source_label(source).0;

        vec![
            PromptCase {
                modal: mcp,
                button: ButtonAction::RemoveSelectedMcpServer,
                message_sub: "Remove MCP server \"alpha\"?".into(),
                expected: ConfirmationAction::DeleteMcpServer {
                    server_name: "alpha".into(),
                },
                row: 0,
            },
            PromptCase {
                modal: plugins,
                button: ButtonAction::UninstallSelectedPlugin,
                message_sub: "Uninstall plugin \"my-plugin\"?".into(),
                expected: ConfirmationAction::Plugins(
                    echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                        plugin_id: "user/abcd1234/my-plugin".into(),
                        confirmed: false,
                    },
                ),
                row: 0,
            },
            PromptCase {
                modal: market_plugin,
                button: ButtonAction::UninstallSelectedMarketplacePlugin,
                message_sub: "Uninstall marketplace plugin \"plug-a\"?".into(),
                expected: ConfirmationAction::Marketplace(
                    echo_agent_hooks_plugins_types::MarketplaceAction::Uninstall {
                        source_url_or_path: "https://example.com/plugins.git".into(),
                        plugin_relative_path: "plugins/plug-a".into(),
                    },
                ),
                row: 1,
            },
            PromptCase {
                modal: market_source,
                button: ButtonAction::RemoveSelectedMarketplaceSource,
                message_sub: "Remove source \"test-source\" and uninstall all its plugins?".into(),
                expected: ConfirmationAction::Marketplace(
                    echo_agent_hooks_plugins_types::MarketplaceAction::RemoveSource {
                        source_url_or_path: "https://example.com/plugins.git".into(),
                    },
                ),
                row: 0,
            },
            PromptCase {
                modal: hooks,
                button: ButtonAction::RemoveSelectedHook,
                message_sub: format!("Remove hook source \"{hook_label}\"?"),
                expected: ConfirmationAction::Hooks(
                    echo_agent_hooks_plugins_types::HooksAction::Remove {
                        path: source.into(),
                    },
                ),
                row: 0,
            },
        ]
    }

    #[test]
    fn all_destructive_actions_prompt_without_dispatching() {
        for case in all_prompt_cases() {
            let mut agent = super::test_fixtures::make_agent();
            agent.extensions_modal = Some(case.modal);
            let outcome = agent.execute_modal_button_action(case.button);
            assert_no_action(outcome);
            assert_prompt(
                agent.extensions_modal.as_ref().unwrap(),
                &case.message_sub,
                &case.expected,
                case.row,
            );
        }
    }

    #[test]
    fn y_dispatches_captured_target_after_selection_moves() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::McpServers);
        modal.mcps_data = TabDataState::Loaded(vec![
            server_info("alpha", crate::views::mcps_modal::McpWireSource::Local),
            server_info("beta", crate::views::mcps_modal::McpWireSource::Local),
        ]);
        modal.entry_data_indices = vec![Some(0), Some(1)];
        modal.entry_group_keys = vec![None, None];
        modal.picker_state.selected = 0;
        agent.extensions_modal = Some(modal);

        assert_no_action(agent.execute_modal_button_action(ButtonAction::RemoveSelectedMcpServer));
        agent
            .extensions_modal
            .as_mut()
            .unwrap()
            .picker_state
            .selected = 1;

        match agent.handle_extensions_modal_key(&key(KeyCode::Char('y'))) {
            InputOutcome::Action(Action::DeleteMcpServer { server_name }) => {
                assert_eq!(server_name, "alpha");
            }
            other => panic!("expected DeleteMcpServer alpha, got {other:?}"),
        }
        let state = agent.extensions_modal.as_ref().unwrap();
        assert_eq!(state.pending_action.as_deref(), Some("removing..."));
        assert_eq!(state.pending_entry_index, Some(0));
        assert!(state.modal_message.is_none());
    }

    #[test]
    fn plugin_y_sends_confirmed_false_so_server_can_gate_multi() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        modal.plugins_data =
            TabDataState::Loaded(echo_agent_hooks_plugins_types::PluginsListResponse {
                plugins: vec![plugin_info("my-plugin")],
            });
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;
        agent.extensions_modal = Some(modal);

        assert_no_action(agent.execute_modal_button_action(ButtonAction::UninstallSelectedPlugin));
        match agent.handle_extensions_modal_key(&key(KeyCode::Char('y'))) {
            InputOutcome::Action(Action::ExecutePluginsAction(
                echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                    plugin_id,
                    confirmed: false,
                },
            )) => assert_eq!(plugin_id, "user/abcd1234/my-plugin"),
            other => panic!("expected unconfirmed uninstall, got {other:?}"),
        }
        let state = agent.extensions_modal.as_ref().unwrap();
        assert_eq!(
            state.last_plugins_action,
            Some(echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                plugin_id: "user/abcd1234/my-plugin".into(),
                confirmed: false,
            })
        );
        assert!(state.modal_message.is_none());
    }

    #[test]
    fn marketplace_y_keeps_uninstalling_label_on_captured_row() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Marketplace);
        modal.marketplace_data = marketplace_loaded();
        modal.entry_labels_cache = vec!["test-source".into(), "plug-a".into(), "plug-b".into()];
        modal.entry_group_keys = vec![Some("0".into()), None, None];
        modal.entry_data_indices = vec![None, Some(0), Some(1)];
        modal.picker_state.selected = 1;
        agent.extensions_modal = Some(modal);

        assert_no_action(
            agent.execute_modal_button_action(ButtonAction::UninstallSelectedMarketplacePlugin),
        );
        agent
            .extensions_modal
            .as_mut()
            .unwrap()
            .picker_state
            .selected = 2;
        match agent.handle_extensions_modal_key(&key(KeyCode::Char('y'))) {
            InputOutcome::Action(Action::ExecuteMarketplaceAction(
                echo_agent_hooks_plugins_types::MarketplaceAction::Uninstall {
                    plugin_relative_path,
                    ..
                },
            )) => assert_eq!(plugin_relative_path, "plugins/plug-a"),
            other => panic!("expected marketplace uninstall, got {other:?}"),
        }
        let state = agent.extensions_modal.as_ref().unwrap();
        assert_eq!(state.pending_action.as_deref(), Some("Uninstalling..."));
        assert_eq!(state.pending_entry_index, Some(1));
    }

    #[test]
    fn managed_mcp_errors_without_prompt() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::McpServers);
        modal.mcps_data = TabDataState::Loaded(vec![server_info(
            "managed-one",
            crate::views::mcps_modal::McpWireSource::Managed,
        )]);
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;
        agent.extensions_modal = Some(modal);

        assert_no_action(agent.execute_modal_button_action(ButtonAction::RemoveSelectedMcpServer));
        match &agent.extensions_modal.as_ref().unwrap().modal_message {
            Some(ModalMessage::Error(msg)) => {
                assert!(msg.contains("Cannot remove managed server 'managed-one'"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn cancel_keys_dismiss_without_dispatch() {
        let mut agent = super::test_fixtures::make_agent();
        let mut modal = ExtensionsModalState::new(ExtensionsTab::McpServers);
        modal.mcps_data = TabDataState::Loaded(vec![server_info(
            "alpha",
            crate::views::mcps_modal::McpWireSource::Local,
        )]);
        modal.entry_data_indices = vec![Some(0)];
        modal.entry_group_keys = vec![None];
        modal.picker_state.selected = 0;
        agent.extensions_modal = Some(modal);

        for code in [KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('Y')] {
            agent.execute_modal_button_action(ButtonAction::RemoveSelectedMcpServer);
            assert!(
                agent
                    .extensions_modal
                    .as_ref()
                    .unwrap()
                    .modal_message
                    .is_some()
            );
            assert_no_action(agent.handle_extensions_modal_key(&key(code)));
            assert!(
                agent
                    .extensions_modal
                    .as_ref()
                    .unwrap()
                    .modal_message
                    .is_none(),
                "key {code:?} must dismiss confirmation"
            );
        }
    }
}
