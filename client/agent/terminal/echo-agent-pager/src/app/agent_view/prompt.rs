//! Prompt-pane key handling: `handle_prompt_key`, the Esc policy, history
//! search, and the combined prompt history.

#[cfg(test)]
use super::test_fixtures;
use super::{
    AgentDeferredSend, AgentPane, AgentView, PromptInputMode, PromptMode, is_bang_key, is_hash_key,
    remember_mode_enabled, resolve_action,
};
use crate::actions::{ActionId, ActionRegistry, When};
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::key;
use crate::views::prompt_widget::PromptEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

impl AgentView {
    pub fn prompt_history_loading(&self) -> bool {
        self.session.prompt_history_loading && self.prompt.text().is_empty()
    }

    /// Unsent drafts first (the stash, then drafts it replaced), then `session.prompt_history` in order.
    ///
    /// `session.prompt_history` in order, and nothing else. A stashed draft is not history: `Ctrl+S`
    /// is how you get it back, and listing it here shows it before you ever sent it.
    pub fn combined_prompt_history(&self) -> Vec<crate::views::history_search::HistoryEntry> {
        use crate::views::history_search::HistoryEntry;

        fn push(out: &mut Vec<HistoryEntry>, text: String) {
            if !text.trim().is_empty() {
                out.push(HistoryEntry { text });
            }
        }

        let mut history = Vec::new();

        for prompt in &self.session.prompt_history {
            push(&mut history, prompt.clone());
        }

        history
    }

    /// Append the replayed transcript's `UserPrompt` blocks to `session.prompt_history`, newest first.
    ///
    /// The browse reads that list alone, and the `echo.agent/prompt_history` fetch delivers an empty list when it
    /// fails, so this is what makes a restored session's prompts recallable. Appended, not prepended: a prompt
    /// sent during the load is newer than anything the transcript holds. `PromptHistoryLoaded` skips fetched
    /// prompts whose trimmed text is already here.
    pub(in crate::app) fn seed_prompt_history_from_scrollback(&mut self) {
        use crate::scrollback::block::RenderBlock;

        let recorded: std::collections::HashSet<String> = self
            .session
            .prompt_history
            .iter()
            .map(|prompt| prompt.trim().to_owned())
            .collect();

        let restored: Vec<String> = (0..self.scrollback.len())
            .rev()
            .filter_map(|i| match &self.scrollback.entry(i)?.block {
                RenderBlock::UserPrompt(block) => Some(block.text.clone()),
                _ => None,
            })
            .filter(|text| !text.trim().is_empty() && !recorded.contains(text.trim()))
            .collect();

        self.session.prompt_history.extend(restored);
        self.session
            .prompt_history
            .truncate(crate::app::agent::PROMPT_HISTORY_CAP);
    }

    /// Prompt-focused key handling.
    ///
    /// Routes through the action registry FIRST for mapped actions (SendPrompt,
    /// FocusScrollback, etc.), then falls through to the widget for text editing.
    /// Agent-level and global actions are handled by the caller after this
    /// returns Unchanged.
    ///
    /// **Exception**: when the file search dropdown is visible, the widget gets
    /// first shot at Tab/Enter/Esc/arrows (for navigation and acceptance).
    /// Test-only wrapper around the private `handle_prompt_key` using a
    /// **non–VS Code** pinned registry so host `TERM_PROGRAM` cannot change
    /// InterjectPrompt / OpenExtensions chords under test.
    #[cfg(test)]
    pub(crate) fn handle_prompt_key_for_test(&mut self, key: &KeyEvent) -> InputOutcome {
        let registry = ActionRegistry::non_vscode_for_test();
        self.handle_prompt_key(key, &registry, false)
    }

    /// Like [`Self::handle_prompt_key_for_test`] with an explicit registry
    /// (e.g. [`ActionRegistry::vscode_family_for_test`]).
    #[cfg(test)]
    pub(crate) fn handle_prompt_key_with_registry_for_test(
        &mut self,
        key: &KeyEvent,
        registry: &ActionRegistry,
    ) -> InputOutcome {
        self.handle_prompt_key(key, registry, false)
    }

    // `pub(super)`: also called by `AppView::minimal_key_intercept` to route
    // Apple Terminal's Ctrl+O interject chord straight to the prompt path —
    // minimal's prompt is conceptually always focused, but `active_pane` can be
    // Scrollback, whose `When::AgentScreen` promotion would misroute the chord
    // to `ToggleYolo`.
    pub(in crate::app) fn handle_prompt_key(
        &mut self,
        key: &KeyEvent,
        registry: &ActionRegistry,
        prompt_paging: bool,
    ) -> InputOutcome {
        // Dismiss transient toasts on any keypress so error messages don't
        // linger while the user is already typing. Sticky status banners
        // (`sticky_toast`) are unaffected; ephemeral tips intentionally
        // survive typing (cleared by TTL, submit, or explicit clear).
        self.toast = None;

        // Any key reaching the prompt is the user interacting with it, so hand
        // focus back from the /btw panel (its scroll keys are consumed earlier).
        self.btw_focused = false;

        // ── History panel intercept (modal) ─────────────────────────────
        // Must run before the file-search / slash intercepts: a populated
        // entry can end on an `@` token (or start with `/`), and the
        // dropdown state derived from that text would otherwise steal the
        // arrows mid-browse.
        if self.prompt.history_search.is_active() {
            // Tear down the history overlay before opening the cheatsheet: it
            // renders unconditionally and would bleed around the popup, and Esc
            // would otherwise silently resume the browse instead of closing help.
            if registry.matches_id(ActionId::ShortcutsHelp, key) {
                self.close_history_restoring_saved();
                return self.handle_agent_action_with_registry(ActionId::ShortcutsHelp, registry);
            }
            return self.handle_history_search_key(key);
        }

        // Tab-family chords drop the prompt highlight no matter which layer
        // consumes them (dropdown accepts, registry actions, widget decline).
        let mut dropped_highlight = false;
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            dropped_highlight = self.prompt.textarea.selection_range().is_some();
            self.prompt.textarea.clear_selection();
        }

        // ── File search intercept ───────────────────────────────────────
        // When the @-completion dropdown is visible, the widget handles
        // Tab (accept), Enter (accept), Esc (dismiss), and arrow keys
        // BEFORE the action registry gets them. Otherwise Tab would
        // jump to scrollback and Enter would send the prompt.
        if self.prompt.file_search_visible() {
            match self.prompt.handle_key(key) {
                PromptEvent::Edited => {
                    // Check if the prompt wants to open a line viewer.
                    if let Some(req) = self.prompt.pending_viewer_request.take() {
                        self.open_line_viewer(&req.path, req.initial_range);
                    }
                    self.prompt.refresh_slash(&self.session.models);
                    if let Some(action) = self.take_prompt_tip_signal() {
                        return InputOutcome::Action(action);
                    }
                    return InputOutcome::Changed;
                }
                PromptEvent::Ignored => {} // fall through to normal routing
            }
        }

        // ── Slash dropdown intercept ────────────────────────────────────
        // When the slash completion dropdown is open, intercept navigation
        // and accept keys BEFORE the action registry. Completion changes
        // text only — it does NOT execute commands.
        //
        // `slash_accepted_send` is set when Enter accepts a no-arg slash
        // command and needs to fall through to the send path — this flag
        // bypasses the multiline-mode Enter→newline swap so the command
        // is actually sent.
        let mut slash_accepted_send = false;
        if key.code == KeyCode::Enter
            && key.modifiers.is_empty()
            && !self.prompt.slash_open()
            && crate::slash::is_command_complete(
                self.prompt.text(),
                self.prompt.slash_controller.registry(),
            )
        {
            slash_accepted_send = true;
        }
        if self.prompt.slash_open() && !self.prompt.file_search_visible() {
            if prompt_paging && registry.matches_id(ActionId::PageUp, key) {
                self.prompt
                    .slash_scroll_selection(-(crate::slash::MAX_VISIBLE_SUGGESTIONS as isize));
                self.prompt.slash_preview_current_selection();
                return InputOutcome::Changed;
            }
            if prompt_paging && registry.matches_id(ActionId::PageDown, key) {
                self.prompt
                    .slash_scroll_selection(crate::slash::MAX_VISIBLE_SUGGESTIONS as isize);
                self.prompt.slash_preview_current_selection();
                return InputOutcome::Changed;
            }
            match key.code {
                // Up / Ctrl-P: move selection up.
                KeyCode::Up => {
                    self.prompt.slash_move_selection(-1);
                    self.prompt.slash_preview_current_selection();
                    return InputOutcome::Changed;
                }
                // Down / Ctrl-N: move selection down.
                KeyCode::Down => {
                    self.prompt.slash_move_selection(1);
                    self.prompt.slash_preview_current_selection();
                    return InputOutcome::Changed;
                }
                KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                    self.prompt.slash_move_selection(-1);
                    self.prompt.slash_preview_current_selection();
                    return InputOutcome::Changed;
                }
                KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                    self.prompt.slash_move_selection(1);
                    self.prompt.slash_preview_current_selection();
                    return InputOutcome::Changed;
                }
                // Tab: accept completion (text only, no execute).
                KeyCode::Tab => {
                    self.prompt.slash_commit_preview();
                    self.prompt.accept_slash_completion(&self.session.models);
                    return InputOutcome::Changed;
                }
                // Esc: close dropdown, revert any live preview.
                KeyCode::Esc => {
                    self.prompt.slash_cancel_preview();
                    self.prompt.slash_close();
                    return InputOutcome::Changed;
                }
                // Enter: accept completion, then send (terminal row) or
                // stay open (row's insert_text ends with space => chains).
                KeyCode::Enter if key.modifiers.is_empty() => {
                    let snap = self.prompt.slash_snapshot();
                    let exact_command = crate::slash::is_typed_slash_selected(
                        &snap,
                        self.prompt.text(),
                        self.prompt.slash_controller.registry(),
                    );
                    if exact_command {
                        self.prompt.slash_commit_preview();
                        self.prompt.slash_close();
                        slash_accepted_send = true;
                    } else {
                        let chains = snap
                            .selection()
                            .is_some_and(|row| row.insert_text.ends_with(' '));
                        self.prompt.slash_commit_preview();
                        self.prompt.accept_slash_completion(&self.session.models);
                        if chains {
                            return InputOutcome::Changed;
                        }
                        self.prompt.slash_close();
                        slash_accepted_send = true;
                    }
                }
                // Everything else: fall through to normal text editing
                // (which calls refresh_slash via PromptEvent::Edited).
                _ => {}
            }
        }

        // Mid-text inline ghost: Tab accepts the ghost completion when no
        // dropdown is open. Must come before the action registry so Tab
        // doesn't jump focus.
        if !self.prompt.slash_open() && !self.prompt.file_search_visible() && key!(Tab).matches(key)
        {
            let snap = self.prompt.slash_state.snapshot();
            if let Some(ref ghost) = snap.inline_ghost {
                let range = ghost.token_range.clone();
                let text_len = self.prompt.textarea.text().len();
                let valid = range.end <= text_len
                    && self.prompt.textarea.text().is_char_boundary(range.start)
                    && self.prompt.textarea.text().is_char_boundary(range.end);
                if valid {
                    let insert = format!("/{}", ghost.full_name);
                    self.prompt.textarea.replace_range(range, &insert);
                    self.prompt.refresh_slash(&self.session.models);
                    return InputOutcome::Changed;
                }
            }
        }

        // ── Completion dropdown intercept ────────────────────────────────
        // Priority 4-5 in the Tab chain: when the completion dropdown is
        // open, handle navigation/accept/dismiss. When closed, Tab in bash
        // mode is terminal-like completion (always on, no env gate).
        if !self.prompt.slash_open() && !self.prompt.file_search_visible() {
            if self.prompt.completion_dropdown_open() {
                if prompt_paging && registry.matches_id(ActionId::PageUp, key) {
                    self.prompt.completion_dropdown_scroll(
                        -(crate::views::completion_dropdown::MAX_VISIBLE_ROWS as isize),
                    );
                    return InputOutcome::Changed;
                }
                if prompt_paging && registry.matches_id(ActionId::PageDown, key) {
                    self.prompt.completion_dropdown_scroll(
                        crate::views::completion_dropdown::MAX_VISIBLE_ROWS as isize,
                    );
                    return InputOutcome::Changed;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('p')
                        if key.code == KeyCode::Up || key.modifiers == KeyModifiers::CONTROL =>
                    {
                        self.prompt.completion_dropdown_move(-1);
                        return InputOutcome::Changed;
                    }
                    KeyCode::Down | KeyCode::Char('n')
                        if key.code == KeyCode::Down || key.modifiers == KeyModifiers::CONTROL =>
                    {
                        self.prompt.completion_dropdown_move(1);
                        return InputOutcome::Changed;
                    }
                    KeyCode::Tab if key.modifiers == KeyModifiers::SHIFT => {
                        self.prompt.completion_dropdown_move(-1);
                        return InputOutcome::Changed;
                    }
                    KeyCode::Tab => {
                        if self.accept_completion_dropdown_item() {
                            return InputOutcome::Changed;
                        }
                        // Empty items (race): close and fall through.
                        self.prompt.completion_dropdown_close();
                    }
                    KeyCode::Enter if key.modifiers.is_empty() => {
                        if self.accept_completion_dropdown_item() {
                            return InputOutcome::Changed;
                        }
                        // Empty items (race): close and fall through to send.
                        self.prompt.completion_dropdown_close();
                    }
                    KeyCode::Esc => {
                        // Consumed Esc: disarm the Esc→d flight-recorder
                        // combo (same convention as `try_handle_esc_policy`).
                        self.esc_pressed_at = None;
                        self.prompt.completion_dropdown_close();
                        return InputOutcome::Changed;
                    }
                    _ => {}
                }
            } else if cfg!(not(windows))
                && key!(Tab).matches(key)
                && self.prompt_input_mode == PromptInputMode::Bash
                && !self.prompt.text().is_empty()
            {
                // Priority 5: terminal-like Tab in bash mode — always on.
                // Windows keeps the legacy focus-cycling Tab instead: the
                // completion stack's tokenizer/quoting is POSIX-only (see
                // the shell crate's `shell_token`), so the keystroke must
                // not be eaten by a surface that emits misparsed lines.
                // Usable candidates complete now (insta-accept / fill /
                // dropdown, per `tab_decision`); `Nothing` (none fetched, or
                // outdated by an edit or cursor move) fires a deterministic
                // fetch whose landing runs the same semantics. An empty
                // draft falls through to focus-cycling: no token to complete.
                use crate::views::suggestion_controller::TabAction;
                match self
                    .prompt
                    .suggestions
                    .tab_decision(self.prompt.text(), self.prompt.cursor())
                {
                    TabAction::Nothing => self.request_shell_tab_completion(true),
                    action => self.execute_tab_action(action),
                }
                return InputOutcome::Changed;
            }
        }

        if prompt_paging && self.prompt_mode == PromptMode::Normal {
            let page_action = [ActionId::PageUp, ActionId::PageDown]
                .into_iter()
                .find(|id| registry.matches_id(*id, key));
            if let Some(outcome) = resolve_action(page_action) {
                return outcome;
            }
        }

        // ── Predicted-next-prompt ghost (tab autocomplete) ──────────────
        // Tab or Right arrow accepts the suggestion (the ghost only renders
        // with the cursor at end-of-text, where Right is otherwise a no-op —
        // same convention as fish/zsh autosuggestions); Esc on an empty
        // prompt dismisses it for the rest of the turn. Must come before the
        // action registry so Tab doesn't jump focus to the scrollback.
        // Shell-command ghost/dropdown intercepts above win when present
        // (`prompt_suggestion_visible` yields false then).
        self.refresh_prompt_suggestion_gate();
        // Latch the `shown` impression for a ghost that became visible after
        // load (divergent draft cleared, gate re-opened). Runs before the
        // Tab/Esc intercepts below so `shown` is logged before this same key
        // can log `accepted`/`dismissed`.
        self.log_prompt_suggestion_shown_if_visible();
        if !self.prompt.slash_open()
            && !self.prompt.file_search_visible()
            && !self.prompt.completion_dropdown_open()
            && self.prompt.prompt_suggestion_visible()
        {
            if key!(Tab).matches(key) || key!(Right).matches(key) {
                if self.prompt.accept_prompt_suggestion() {
                    // Post-accept, the prompt text is the full suggestion
                    // (the ghost only shows when the text is a prefix of it).
                    let (chars, words) =
                        crate::views::prompt_suggestion::suggestion_size(self.prompt.text());
                    echo_agent_telemetry::session_ctx::log_event(
                        echo_agent_telemetry::events::PromptSuggestion {
                            action: echo_agent_telemetry::events::PromptSuggestionAction::Accepted,
                            chars,
                            words,
                        },
                    );
                    self.prompt.refresh_slash(&self.session.models);
                    return InputOutcome::Changed;
                }
            } else if key.code == KeyCode::Esc
                && key.modifiers.is_empty()
                && self.prompt.text().is_empty()
            {
                // Empty prompt → the visible ghost is the full suggestion.
                let (chars, words) = crate::views::prompt_suggestion::suggestion_size(
                    self.prompt.prompt_suggestion_ghost().unwrap_or_default(),
                );
                self.prompt.prompt_suggestion.dismiss();
                echo_agent_telemetry::session_ctx::log_event(
                    echo_agent_telemetry::events::PromptSuggestion {
                        action: echo_agent_telemetry::events::PromptSuggestionAction::Dismissed,
                        chars,
                        words,
                    },
                );
                return InputOutcome::Changed;
            }
        }

        // 0. Editing-mode intercepts (see `queue_edit.rs`). `None` falls
        //    through to the widget (Shift-Enter / Alt-Enter newline, typing).
        if let Some(outcome) = self.handle_editing_queued_key(key) {
            return outcome;
        }

        // 0b. (History-panel intercept lives at the top of this fn.)

        // 0c. Up on an empty prompt: the queue claims it, the prompt history panel takes it when the queue is empty. Two entry points, one panel:
        //   - /history: search mode, the composer is the filter query.
        //   - Up on an empty prompt: browse mode, opens with the newest prompt filled into the composer.
        //     Up/Down step with live-populate, Down at the newest closes, typing detaches to edit. Down never opens the panel.
        // Browse activation is Normal-input-mode only (recalling a chat prompt into a Bash/Remember composer would submit it under that mode).
        // A recalled `! cmd` entry flips the composer to Bash itself.
        if self.prompt_mode == PromptMode::Normal
            && self.prompt.text().is_empty()
            && !self.prompt.file_search_visible()
            && key!(Up).matches(key)
            && self.prompt_input_mode == PromptInputMode::Normal
        {
            // The queue holds the text just written, newer than any history entry.
            if let Some(outcome) = self.try_focus_queue_from_prompt() {
                return outcome;
            }

            self.open_history_browse_at_newest();
            // Consumed even with empty history (Up on an empty composer has no cursor motion to fall back to).
            return InputOutcome::Changed;
        }

        // 0d. Bash mode activation: `!` on empty prompt enters bash mode.
        if self.prompt_input_mode == PromptInputMode::Normal
            && is_bang_key(key)
            && self.prompt.text().is_empty()
            && !self.prompt.file_search_visible()
            && !self.prompt.history_search.is_active()
        {
            self.prompt_input_mode = PromptInputMode::Bash;
            return InputOutcome::Changed;
        }

        // 0d2. Remember mode activation: `#` on empty prompt enters remember mode.
        //     Gated by `[features] remember_mode = true` in config.toml (default: off).
        if self.prompt_input_mode == PromptInputMode::Normal
            && is_hash_key(key)
            && self.prompt.text().is_empty()
            && !self.prompt.file_search_visible()
            && !self.prompt.history_search.is_active()
            && remember_mode_enabled()
        {
            self.prompt_input_mode = PromptInputMode::Remember;
            return InputOutcome::Changed;
        }

        // 0e. Exit special input mode on empty prompt using per-mode exit keys (Bash/Remember: Backspace/Esc/Ctrl+W/U/C).
        //     With non-empty text, Esc falls through to Esc policy (cancel / mid-turn swallow / clear / rewind). Mode is preserved for re-focus.
        if self.prompt_input_mode.is_exit_key(key) && self.prompt.text().is_empty() {
            self.prompt_input_mode = PromptInputMode::Normal;
            return InputOutcome::Changed;
        }

        // 1. Element interaction: Enter on paste/file-ref → inline (expand).
        //    Enter on image chip → open preview (handled by caller).
        //    Must check before registry lookup since Enter is also SendPrompt.
        if let Some(interaction) = self.prompt.try_element_interaction(key) {
            use crate::views::prompt_widget::ElementInteraction;
            match interaction {
                ElementInteraction::Inlined => {
                    self.prompt.refresh_slash(&self.session.models);
                    return InputOutcome::Changed;
                }
                ElementInteraction::ImagePreview => {
                    if !self.guard_image_support() {
                        return InputOutcome::Changed;
                    }
                    if let Some(img) = self.prompt.image_at_cursor() {
                        if let Some(viewer) = crate::prompt_images::ImageViewerState::open(img) {
                            self.image_viewer = Some(viewer);
                        } else {
                            self.show_toast("Couldn't load image preview");
                        }
                    }
                    return InputOutcome::Changed;
                }
            }
        }

        // Shift+Tab (cycle session mode) is not special-cased here — the
        // `CycleMode` ActionDef carries all encodings; the registry lookup
        // below resolves it (same as `DashboardCycleMode`).

        // 2. Multiline mode: Shift+Enter (or Alt+Enter) sends.
        //    This must come BEFORE the action registry lookup so that
        //    Shift+Enter triggers send instead of inserting a newline.
        //    Apple Terminal: bare Enter may actually be Cmd/Opt+Enter.
        if self.multiline_mode && crate::input::is_mod_enter(key) {
            if let Some(text) = self.prompt.try_send() {
                let action = self.prompt_input_mode.send_action(text);
                self.prompt_input_mode = PromptInputMode::Normal;
                return InputOutcome::Action(action);
            }
            return InputOutcome::Changed;
        }

        // 3. Check ActionRegistry for prompt-scoped actions.
        //    SendPrompt is routed here — the widget's try_send() applies guards.
        if let Some(action_id) = registry.lookup(key, When::PromptFocused) {
            match action_id {
                ActionId::SendPrompt => {
                    // Apple Terminal: Shift+Enter arrives as bare Enter (no
                    // Kitty protocol). Poll CoreGraphics for real modifier
                    // state — if Shift/Option/Cmd is held, insert a newline
                    // instead of sending.
                    if key.code == KeyCode::Enter
                        && self.prompt_input_mode != PromptInputMode::Bash
                        && !slash_accepted_send
                        && crate::input::is_apple_terminal_newline_modifier_held()
                    {
                        self.prompt.insert_replacing_selection("\n");
                        return InputOutcome::Changed;
                    }

                    // Multiline mode: bare Enter inserts a newline instead of sending.
                    // Exceptions:
                    //  - slash_accepted_send: slash dropdown Enter accepted a no-arg
                    //    command and fell through — must send, not insert newline.
                    //  - bash mode: Enter should always send.
                    //  - empty composer + mid-turn queue: force-send the top row
                    //    (send-now discoverability). Inserting a blank line on an
                    //    empty prompt is never useful here; same path as normal mode.
                    if self.multiline_mode
                        && self.prompt_input_mode != PromptInputMode::Bash
                        && !slash_accepted_send
                    {
                        if matches!(self.prompt_mode, PromptMode::Normal)
                            && self.prompt.text().trim().is_empty()
                            && self.session.state.is_turn_running()
                            && let Some(outcome) = self.try_send_now_queued_from_prompt()
                        {
                            return outcome;
                        }
                        self.prompt.insert_replacing_selection("\n");
                        return InputOutcome::Changed;
                    }
                    if let Some(text) = self.prompt.try_send() {
                        // Remember + slash_accepted_send: treat as normal SendPrompt
                        // (the slash path accepted a no-arg command that fell through).
                        let action_mode = if self.prompt_input_mode == PromptInputMode::Remember
                            && slash_accepted_send
                        {
                            PromptInputMode::Normal
                        } else {
                            self.prompt_input_mode
                        };
                        let action = action_mode.send_action(text);
                        self.prompt_input_mode = PromptInputMode::Normal;
                        return InputOutcome::Action(action);
                    }
                    // Empty (or backslash continuation). Mid-turn + a queued
                    // follow-up: bare Enter force-sends the top queue row so
                    // users discover send-now without learning a chord.
                    // Skip while editing a queued row (edit-mode Enter is
                    // handled earlier for non-empty; empty must stay a no-op).
                    // Guard on an actually-empty composer: try_send() also
                    // returns None after a backslash continuation, which leaves
                    // the (non-empty) draft in place — that Enter must only
                    // insert the newline, not fire a queued follow-up.
                    if matches!(self.prompt_mode, PromptMode::Normal)
                        && self.prompt.text().trim().is_empty()
                        && self.session.state.is_turn_running()
                        && let Some(outcome) = self.try_send_now_queued_from_prompt()
                    {
                        return outcome;
                    }
                    // try_send() returned None (empty, backslash continuation)
                    // → backslash continuation mutates widget, need redraw
                    return InputOutcome::Changed;
                }
                ActionId::InterjectPrompt => {
                    crate::actions::log_shortcut_used(
                        key,
                        ActionId::InterjectPrompt,
                        When::PromptFocused.telemetry_name(),
                    );
                    // Editing-queued intercept lives in `queue_edit.rs`.
                    if let Some(outcome) = self.interject_editing_queued_intercept() {
                        return outcome;
                    }
                    // Mid-turn send-now (cancel-and-send):
                    // 1) Non-empty composer → cancel the running turn and send
                    //    that text as the next prompt.
                    // 2) Empty composer + a visible follow-up in the queue →
                    //    same as bare Enter: send the top row now.
                    // 3) Idle / nothing to send: promote to ToggleYolo when that chord
                    //    matches (Apple Terminal Ctrl+O opens YOLO / free-tier CTA).
                    let text = self.prompt.text().trim().to_string();
                    let turn_running = self.session.state.is_turn_running();
                    if !text.is_empty() {
                        if turn_running {
                            // Paste-then-immediate-send: an image probe is still
                            // off-thread. Stash (draft untouched) and re-issue on
                            // completion so the not-yet-attached chip isn't dropped.
                            if self.paste_probe_in_flight > 0 {
                                self.deferred_send = Some(AgentDeferredSend::Interject);
                                return InputOutcome::Changed;
                            }
                            // Drain images BEFORE set_text("") wipes the chip elements.
                            let images = self.prompt.drain_images();
                            self.prompt.set_text("");
                            self.note_draft_consumed();
                            return InputOutcome::Action(Action::SendPromptNow { text, images });
                        }
                    } else if turn_running
                        && let Some(outcome) = self.try_send_now_queued_from_prompt()
                    {
                        return outcome;
                    }
                    if registry.matches_id(ActionId::ToggleYolo, key) {
                        return self
                            .handle_agent_action_with_registry(ActionId::ToggleYolo, registry);
                    }
                    return InputOutcome::Changed;
                }
                ActionId::ToggleMultiline => {
                    return InputOutcome::Action(Action::SetMultilineMode(!self.multiline_mode));
                }
                ActionId::StashPrompt => {
                    let outcome = self.handle_stash_prompt_key();
                    // A declined chord falls through like an unclaimed key.
                    if !matches!(outcome, InputOutcome::Unchanged) {
                        crate::actions::log_shortcut_used(
                            key,
                            ActionId::StashPrompt,
                            When::PromptFocused.telemetry_name(),
                        );
                        return outcome;
                    }
                }
                other => {
                    if let Some(outcome) = resolve_action(Some(other)) {
                        return outcome;
                    }
                }
            }
        }

        // 2c. Ctrl+V / Cmd+V: read the pbpaste text once and route through the
        //     deferred paste pipeline. A file path wins synchronously; the
        //     clipboard image/file-url probe defers off the event loop. Reading
        //     once (vs the widget re-reading for text insertion) avoids the old
        //     double `pbpaste` subprocess on macOS.
        if crate::input::key::is_paste_key(key) {
            let clipboard_text = crate::app::actions::ClipboardTextRead::from_result(
                crate::clipboard::system_clipboard_read_text(),
            );
            return self.handle_paste_key_deferred(clipboard_text);
        }

        // 2d. Promote agent-screen actions past the textarea.
        //
        // Without this, keys like Ctrl+\ / Ctrl+/ (or, historically,
        // Alt-Left / Alt-Right as word-jump cursor moves) would be
        // eaten by the textarea and the When::AgentScreen bindings
        // (NavBack, NavForward, ExitSession, ...) would silently
        // no-op when the prompt is focused. PromptFocused-bound
        // actions still win above (see `When::PromptFocused` lookup).
        //
        // The promotion is for non-text bindings only (Ctrl/Alt
        // arrows, Ctrl+P, function keys, etc.). We must explicitly
        // skip `KeyCode::Char(_)` events whose modifiers are NONE or
        // SHIFT-only -- those are user text input under the Kitty
        // enhanced keyboard protocol (e.g. `?+SHIFT` for `?`,
        // `1+SHIFT` for `!`), and the `When::AgentScreen` registry
        // currently binds `?+SHIFT` as the alt key for CommandPalette.
        // See the parallel guard at the top-of-file `?` handler in
        // `handle_input` (`active_pane != Prompt`).
        let is_text_char = crate::input::key::is_text_input_key(key);
        // Mouse toggle is scrollback-only (Ctrl+R); the prompt leaves Ctrl+R unbound.
        if !is_text_char && let Some(action_id) = registry.lookup(key, When::AgentScreen) {
            // Ctrl+C is a two-step "clear, then cancel" gesture when the
            // prompt has a draft: the first press clears the textarea, the
            // second (now on an empty prompt) cancels the running turn.
            // Skipping the agent-screen promotion here lets Ctrl+C fall
            // through to the widget's clear path; an empty prompt re-enters
            // this block and runs CancelTurn as usual.
            let cancel_with_draft =
                matches!(action_id, ActionId::CancelTurn) && !self.prompt.text().is_empty();
            if !cancel_with_draft {
                let outcome = self.handle_agent_action_with_registry(action_id, registry);
                // Only consume the key if the agent action actually did
                // something. When idle, runtime-guarded actions like
                // CancelTurn return Unchanged so the key can fall through
                // to the prompt widget (e.g. Ctrl+C clears prompt text
                // when no turn is running).
                if !matches!(outcome, InputOutcome::Unchanged) {
                    return outcome;
                }
            }
        }

        // 3. Let the widget handle text editing (chars, paste, cursor, undo, newline).
        // (Skip if already handled by file search intercept above.)
        if !self.prompt.file_search_visible() {
            // The undo tip advertises ctrl+z; an undo keypress while it is on
            // screen is the user acting on it. Captured before the widget runs
            // so a bare ctrl+z (no tip up, or the tip disabled) emits nothing.
            let undo_tip_accepted = crate::input::key::is_undo_key(key)
                && self.ephemeral_tip.current_key()
                    == Some(crate::tips::clear_detector::UNDO_TIP_KEY);
            match self.prompt.handle_key(key) {
                PromptEvent::Edited => {
                    if undo_tip_accepted {
                        echo_agent_telemetry::session_ctx::log_event(
                            echo_agent_telemetry::events::ContextualTip {
                                tip: echo_agent_telemetry::events::ContextualTipKind::Undo,
                                action: echo_agent_telemetry::events::ContextualTipAction::Accepted,
                            },
                        );
                        // Retire the hint on the restore that consumed it (its
                        // "Input cleared" copy is now stale), mirroring the
                        // clipboard tip's clear-on-paste so one restore counts
                        // exactly one acceptance.
                        self.ephemeral_tip
                            .clear(crate::tips::clear_detector::UNDO_TIP_KEY);
                    }
                    // Check if the widget wants to open a line viewer (Ctrl-L or : on element).
                    if let Some(req) = self.prompt.pending_viewer_request.take() {
                        self.open_line_viewer(&req.path, req.initial_range);
                    }
                    self.prompt.refresh_slash(&self.session.models);
                    if let Some(eff) = self.notify_suggestion_text_changed() {
                        self.pending_effects.push(eff);
                    }
                    if let Some(eff) = self.notify_plugin_cta_text_changed() {
                        self.pending_effects.push(eff);
                    }
                    if let Some(action) = self.take_prompt_tip_signal() {
                        return InputOutcome::Action(action);
                    }
                    return InputOutcome::Changed;
                }
                PromptEvent::Ignored => {}
            }
        }

        // 4. Structural keys declined by the widget. Tab leaves the prompt only
        //    when this registry exposes the scrollback surface; minimal omits
        //    FocusScrollback, so an otherwise-unclaimed Tab stays with its
        //    logical composer. Dropdown/completion Tab paths already consumed
        //    their presses above. Esc remains owned by try_handle_esc_policy.
        match key.code {
            KeyCode::Tab if registry.find(ActionId::FocusScrollback).is_some() => {
                InputOutcome::Action(Action::FocusScrollback)
            }
            // A dropped highlight must repaint even when nothing claims the key.
            _ if dropped_highlight => InputOutcome::Changed,
            _ => InputOutcome::Unchanged,
        }
    }

    /// Browse mode fills the composer with the newest entry as it opens.
    fn open_history_browse_at_newest(&mut self) {
        // Every row goes to the off-thread fuzzy matcher.
        let built_at = std::time::Instant::now();
        let history = self.combined_prompt_history();
        let _span = tracing::info_span!(
            "history.browse_open",
            history.rows = history.len(),
            history.recall_entries = self.session.prompt_history.len(),
            history.build_micros = built_at.elapsed().as_micros() as u64,
        )
        .entered();

        // Read before any populate, or the panel saves the entry it just filled in as the composer to restore on Esc.
        let current_text = self.prompt.text().to_string();
        if !history.is_empty() {
            // Activation fails when the matcher thread can't start, and the panel can never populate then.
            // Filling the composer would only be undone by the next Down/Enter.
            let opened = self
                .prompt
                .history_search
                .activate_browse(&history, &current_text);
            // The daemon fills the panel later, so take the newest entry from the history already in hand.
            if opened && let Some(entry) = history.first() {
                let newest = entry.text.clone();
                self.populate_prompt_from_history(&newest);
            }
        }
    }

    /// How long after an Esc-fired cancel the idle rewind ARM stays
    /// suppressed (see [`Self::rewind_arm_suppressed`]). Must exceed
    /// `PendingAction::ESC_DOUBLE_PRESS_TTL` (800ms): the grace exists to
    /// absorb the double-press gesture itself, so it has to outlast one full
    /// arm-to-fire window or a mash could still arm-and-fire around it
    /// (invariant pinned by `esc_cancel_rewind_grace_outlives_double_press_ttl`).
    /// The pty-only `ECHO_AGENT_ESC_DOUBLE_PRESS_MS` override can exceed this; no
    /// pty case mashes Esc across a cancel.
    pub(crate) const ESC_CANCEL_REWIND_GRACE: std::time::Duration =
        std::time::Duration::from_millis(1000);

    /// Esc policy (Prompt/Scrollback after overlay steal).
    ///
    /// Call only after overlay / dropdown / search / selection declined Esc.
    /// Returns `None` when the key is not a bare Esc press.
    pub(super) fn try_handle_esc_policy(&mut self, key: &KeyEvent) -> Option<InputOutcome> {
        if key.kind == KeyEventKind::Release
            || key.code != KeyCode::Esc
            || !key.modifiers.is_empty()
        {
            return None;
        }

        // This bare Esc is now owned by the policy: every path below consumes the
        // event (cancel / mid-turn swallow / arm-clear / arm-rewind / idle
        // swallow). Disarm the Esc→d flight-recorder combo here, uniformly — the
        // `0-esc-d` block set `esc_pressed_at` on this same press, but since the
        // policy is handling the Esc, a following `d` is the user's text, not a
        // dump.
        self.esc_pressed_at = None;

        // A blocking card is still pending, parked behind the scrollback — the
        // only way its Esc reaches this policy. Swallow it like the idle arms
        // below: cancelling here would kill the turn the card is blocking on,
        // and `esc_would_cancel_turn`, which the bar reads, already promises
        // it will not.
        if !self.no_input_overlay_pending() {
            return Some(InputOutcome::Changed);
        }

        // Mid-turn running, fullscreen vim mode: swallow Esc (do not cancel or
        // arm clear/rewind — Ctrl+C stays the cancel gesture there).
        // `is_minimal_mode` is the per-agent injected screen mode, not the
        // process global, so tests stay race-free. A streaming wake turn
        // follows the same policy as a running turn (the pane state is Idle
        // only because wake turns are not adopted); once its cancel was sent
        // it follows the cancelling retry below instead, in every mode.
        if (self.session.state.is_turn_running()
            || (self.wake_turn_active() && !self.wake_turn_cancelling()))
            && !crate::app::esc_cancels_turn(self.is_minimal_mode(), self.vim_mode)
        {
            return Some(InputOutcome::Changed);
        }
        // Mid-turn (minimal / non-vim): cancel immediately from prompt or
        // scrollback, even with a draft. Also — in every mode — while already
        // cancelling, so a lost cancel notification is re-sent (Ctrl+C
        // escalates to Quit instead). Push the grace deadline out so an Esc
        // mash past the cancel cannot silently arm the rewind picker below.
        if self.session.state.is_turn_running()
            || self.wake_turn_active()
            || self.any_cancel_pending()
        {
            self.cancel_trigger_hint = Some(crate::app::actions::CancelTrigger::Esc);
            self.suppress_rewind_arm(std::time::Instant::now());
            return Some(InputOutcome::Action(Action::CancelTurn));
        }

        // The two idle arms split on pane ownership. CLEAR mutates the composer
        // (drops text/image chips), so it fires only while the PROMPT pane owns
        // keys — clearing a draft the reader has scrolled past would be a
        // surprising cross-pane side effect. REWIND requires an EMPTY prompt
        // (checked below), so there is no draft to clobber or silently stash and
        // it may arm from EITHER pane. The mid-turn cancel or swallow /
        // cancel-retry (above) stays cross-pane; any other idle Esc swallows
        // (below).
        let has_content = !self.prompt.text().is_empty() || !self.prompt.images.is_empty();

        // Idle + non-empty (text and/or image chips) + prompt pane → arm clear (2× Esc).
        if has_content && self.active_pane == AgentPane::Prompt {
            return Some(InputOutcome::ArmPending {
                action: Action::ClearPrompt,
                shortcut: crate::input::key::KeyShortcut::from(*key),
                label: Some("clear"),
                ttl: crate::app::app_view::esc_double_press_ttl(),
            });
        }

        // Idle + empty + at least one user turn to rewind to → arm rewind
        // picker (silent first press), from either pane. `turn_count` counts
        // UserPrompt-started turns (the scrollback's own notion of "has user
        // messages"), so a scrollback of only system/info/error blocks swallows
        // Esc instead of arming a picker the server would answer with "No
        // undoable prompts". The last three guards restate shields that the
        // PROMPT pane gets upstream but that the SCROLLBACK pane bypasses (so
        // they are vacuously true on the prompt pane): step 0e exits a latent
        // Bash/Remember mode on an empty-composer Esc before the policy runs
        // (without the mode guard a rewind restore would drop conversation
        // text into a still-armed `!` composer); the needs-input overlay
        // intercepts exempt the scrollback pane while the open
        // picker's own intercept does not, so arming under a pending
        // permission/plan/cancel-turn/question overlay would let the picker
        // key-starve it (and a rewind mutate the session out from under it);
        // and the step 0b history-search intercept is prompt-pane-only, so
        // arming would stack the rewind picker on the open search overlay.
        // The grace guard holds only this ARM (never modal/other Esc handling)
        // right after an Esc-fired cancel — see `rewind_arm_suppressed`.
        if !has_content
            && self.scrollback.turn_count() > 0
            && self.prompt_input_mode == PromptInputMode::Normal
            && self.no_input_overlay_pending()
            && !self.prompt.history_search.is_active()
            && !self.rewind_arm_suppressed(std::time::Instant::now())
        {
            return Some(InputOutcome::ArmPending {
                action: Action::RewindShowPicker,
                shortcut: crate::input::key::KeyShortcut::from(*key),
                label: None,
                ttl: crate::app::app_view::esc_double_press_ttl(),
            });
        }

        // Idle with nothing to arm (scrollback pane with a draft, empty prompt
        // + no turns, a scrollback Esc under a latent composer mode / pending
        // needs-input overlay / open history search, or the post-cancel grace):
        // swallow Esc (not FocusScrollback, and not a bubble-up to global quit).
        Some(InputOutcome::Changed)
    }

    /// Arm the post-cancel grace: push the rewind-ARM suppression deadline
    /// out to `now + ESC_CANCEL_REWIND_GRACE`. After an Esc-fired cancel the
    /// session goes Cancelling → Idle with (typically) an empty composer, so
    /// a user mashing Esc would otherwise immediately arm-and-fire the
    /// silent double-Esc rewind picker. Takes `now` so tests are
    /// deterministic (no fabricated `Instant`s).
    pub(crate) fn suppress_rewind_arm(&mut self, now: std::time::Instant) {
        self.rewind_suppress_deadline = Some(now + Self::ESC_CANCEL_REWIND_GRACE);
    }

    /// Check-and-retire the post-cancel grace: true while `now` is before
    /// the deadline set by [`Self::suppress_rewind_arm`]; an expired
    /// deadline is cleared on this consult so no stale `Instant` lingers.
    pub(crate) fn rewind_arm_suppressed(&mut self, now: std::time::Instant) -> bool {
        match self.rewind_suppress_deadline {
            Some(deadline) if now < deadline => true,
            Some(_) => {
                self.rewind_suppress_deadline = None;
                false
            }
            None => false,
        }
    }

    /// Put a history entry into the composer (browse-mode live populate and
    /// the accept paths' shared semantics): `! cmd` entries restore bash
    /// mode with the prefix stripped; other entries force Normal — a Bash
    /// flip mid-browse is always the browse's own doing, since browse only
    /// activates from Normal input mode.
    fn populate_prompt_from_history(&mut self, text: &str) {
        if let Some(cmd) = text.strip_prefix("! ") {
            self.prompt_input_mode = PromptInputMode::Bash;
            self.prompt.set_text(cmd);
        } else {
            self.prompt_input_mode = PromptInputMode::Normal;
            self.prompt.set_text(text);
        }
        let len = self.prompt.textarea.text().len();
        self.prompt.textarea.set_cursor(len);
        // `set_text` recomputed the `@`-completion context from the populated
        // text. The user is browsing history, not completing a path — drop it
        // so the file dropdown neither renders over the panel nor lingers.
        self.prompt.file_search.clear_context();
    }

    /// Live-populate the composer from the panel's current selection
    /// (browse mode: every selection move lands in the textbox).
    pub(super) fn populate_prompt_from_history_selection(&mut self) {
        if let Some(text) = self
            .prompt
            .history_search
            .selected_text()
            .map(str::to_owned)
        {
            self.populate_prompt_from_history(&text);
        }
    }

    /// Commit a recalled history entry into the composer. Shared by the keyboard accept and the
    /// mouse click, which drifted apart once and left the mouse path holding a duplicate draft.
    pub(in crate::app) fn accept_history_entry(&mut self, text: &str) {
        // Restore bash mode from a `! ` history entry unless Remember is active.
        if self.prompt_input_mode != PromptInputMode::Remember
            && let Some(cmd) = text.strip_prefix("! ")
        {
            self.prompt_input_mode = PromptInputMode::Bash;
            self.prompt.set_text(cmd);
        } else if self.prompt_input_mode == PromptInputMode::Bash {
            self.prompt_input_mode = PromptInputMode::Normal;
            self.prompt.set_text(text);
        } else {
            self.prompt.set_text(text);
        }

        let len = self.prompt.textarea.text().len();
        self.prompt.textarea.set_cursor(len);
        self.reclaim_stash_recalled_into_composer();
        // Drop the recomputed `@`-completion context (same suppression as populate).
        self.prompt.file_search.clear_context();
    }

    /// Close the history panel and restore the pre-open composer (Esc, and
    /// browse-mode Down past the newest entry).
    fn close_history_restoring_saved(&mut self) {
        let saved = self.prompt.history_search.saved_text().to_string();
        let was_browse = self.prompt.history_search.is_browse();
        self.prompt.history_search.deactivate();
        self.prompt.set_text(&saved);
        if was_browse {
            // Populate may have flipped the composer to Bash for a `! `
            // entry; the restored (pre-open, empty) composer is Normal.
            self.prompt_input_mode = PromptInputMode::Normal;
        }
    }

    /// Handle a key press while the history panel is active.
    ///
    /// All keys are intercepted: navigation (Up/Down), accept (Enter/Tab),
    /// cancel (Esc/Ctrl-C). Editing keys depend on the mode: search mode
    /// forwards them to the textarea and re-filters (the composer is the
    /// query); browse mode detaches — the panel closes and the key lands in
    /// the populated text as a normal edit.
    fn handle_history_search_key(&mut self, key: &KeyEvent) -> InputOutcome {
        let browse = self.prompt.history_search.is_browse();

        // Esc / Ctrl-C: cancel, restore the pre-open composer text.
        if key!(Esc).matches(key) || key!('c', CONTROL).matches(key) {
            self.close_history_restoring_saved();
            return InputOutcome::Changed;
        }

        // Enter / Tab: accept selected result.
        if key!(Enter).matches(key) || key!(Tab).matches(key) {
            if let Some(text) = self
                .prompt
                .history_search
                .selected_text()
                .map(str::to_owned)
            {
                self.prompt.history_search.deactivate();
                self.accept_history_entry(&text);
            } else {
                // No results, just deactivate.
                self.close_history_restoring_saved();
            }
            return InputOutcome::Changed;
        }

        // Navigation (matching file search: Up/Ctrl-P/Ctrl-K, Down/Ctrl-N/Ctrl-J,
        // PageUp/Ctrl-U, PageDown/Ctrl-D). Browse mode live-populates the
        // composer on every selection move.
        if key!(Up).matches(key)
            || key!('p', CONTROL).matches(key)
            || key!('k', CONTROL).matches(key)
        {
            if self.prompt.history_search.move_up() && browse {
                self.populate_prompt_from_history_selection();
            }
            return InputOutcome::Changed;
        }
        if key!(Down).matches(key)
            || key!('n', CONTROL).matches(key)
            || key!('j', CONTROL).matches(key)
        {
            if self.prompt.history_search.move_down() {
                if browse {
                    self.populate_prompt_from_history_selection();
                }
            } else {
                // Already at the newest (bottom) entry — the position the
                // panel opens on — so Down backs out of history entirely.
                self.close_history_restoring_saved();
            }
            return InputOutcome::Changed;
        }
        if key!(PageUp).matches(key) || key!('u', CONTROL).matches(key) {
            self.prompt.history_search.page_move(-1, 8);
            if browse {
                self.populate_prompt_from_history_selection();
            }
            return InputOutcome::Changed;
        }
        if key!(PageDown).matches(key) || key!('d', CONTROL).matches(key) {
            self.prompt.history_search.page_move(1, 8);
            if browse {
                self.populate_prompt_from_history_selection();
            }
            return InputOutcome::Changed;
        }

        // All other keys, browse mode: an edit detaches — close the panel,
        // keep the populated text, and apply the key as a normal edit.
        if browse {
            self.prompt.history_search.deactivate();
            self.reclaim_stash_recalled_into_composer();
            self.prompt.textarea.input(*key);
            // The populated text may be a slash command — refresh completion.
            self.prompt.refresh_slash(&self.session.models);
            return InputOutcome::Changed;
        }

        // All other keys, search mode: forward to textarea for editing, then
        // refresh the query against the cached history (no scrollback
        // iteration per keystroke).
        self.prompt.textarea.input(*key);
        let query = self.prompt.textarea.text().to_string();
        self.prompt.history_search.update_query(&query);
        InputOutcome::Changed
    }
}

#[cfg(test)]
mod shift_tab_cycle_mode_tests {
    use super::*;
    use crate::app::app_view::InputOutcome;
    use crate::input::key::shift_tab_keys;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Guards the full routed path, not just registry resolution.
    #[test]
    fn shift_tab_emits_cycle_mode_through_prompt_key_routing() {
        for shortcut in shift_tab_keys() {
            let mut agent = super::test_fixtures::make_agent();
            let outcome = agent.handle_prompt_key_for_test(&shortcut.to_key_event());
            assert!(
                matches!(outcome, InputOutcome::Action(Action::CycleMode)),
                "{shortcut:?} must resolve to Action::CycleMode, got {outcome:?}",
            );
        }
    }

    /// `is_mod_enter` must not treat Shift+Tab as send when multiline is on.
    #[test]
    fn multiline_shift_tab_still_cycles_mode_with_non_empty_draft() {
        for shortcut in shift_tab_keys() {
            let mut agent = super::test_fixtures::make_agent();
            agent.multiline_mode = true;
            agent.prompt.set_text("draft text");
            let outcome = agent.handle_prompt_key_for_test(&shortcut.to_key_event());
            assert!(
                matches!(outcome, InputOutcome::Action(Action::CycleMode)),
                "multiline + {shortcut:?} must CycleMode, not send, got {outcome:?}",
            );
            assert_eq!(
                agent.prompt.text(),
                "draft text",
                "draft must not be consumed by Shift+Tab"
            );
        }
    }

    #[test]
    fn plain_tab_follows_focus_scrollback_registration() {
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        for mode in [
            crate::app::ScreenMode::Fullscreen,
            crate::app::ScreenMode::Inline,
        ] {
            let mut agent = super::test_fixtures::make_agent();
            let registry = ActionRegistry::defaults_for(mode);
            let outcome = agent.handle_prompt_key_with_registry_for_test(&tab, &registry);
            assert!(
                matches!(outcome, InputOutcome::Action(Action::FocusScrollback)),
                "{mode:?} plain Tab must focus scrollback, got {outcome:?}",
            );
        }

        let mut minimal = super::test_fixtures::make_agent();
        let registry = ActionRegistry::defaults_for(crate::app::ScreenMode::Minimal);
        let outcome = minimal.handle_prompt_key_with_registry_for_test(&tab, &registry);
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(minimal.active_pane, AgentPane::Prompt);
    }

    #[test]
    fn exact_optional_arg_slash_enter_sends_without_accepting_completion() {
        let mut agent = super::test_fixtures::make_agent();
        agent.multiline_mode = true;
        agent.prompt.set_text("/doctor");
        agent.prompt.set_cursor(agent.prompt.text().len());
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(agent.prompt.slash_open());

        let outcome =
            agent.handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::SendPrompt(ref text)) if text == "/doctor"
            ),
            "got {outcome:?}; prompt={:?}",
            agent.prompt.text()
        );
    }

    #[test]
    fn minimal_slash_dropdown_still_consumes_tab() {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.set_text("/");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(
            agent.prompt.slash_open(),
            "precondition: slash dropdown open"
        );

        let registry = ActionRegistry::defaults_for(crate::app::ScreenMode::Minimal);
        let outcome = agent.handle_prompt_key_with_registry_for_test(
            &KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &registry,
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.active_pane, super::AgentPane::Prompt);
    }
}

#[cfg(test)]
mod slash_menu_enter_tests {
    use super::*;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn agent_with_slash(text: &str) -> crate::app::agent_view::AgentView {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.set_text(text);
        agent.prompt.set_cursor(text.len());
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(agent.prompt.slash_open());
        agent
    }

    fn select_display(agent: &mut crate::app::agent_view::AgentView, display: &str) {
        let idx = agent
            .prompt
            .slash_snapshot()
            .matches
            .iter()
            .position(|row| row.display == display)
            .unwrap_or_else(|| panic!("{display} in slash menu"));
        for _ in 0..idx {
            agent.prompt.slash_move_selection(1);
        }
    }

    #[test]
    fn enter_sends_highlighted_command_not_typed_prefix() {
        let mut agent = agent_with_slash("/log");
        select_display(&mut agent, "/login");
        let outcome = agent.handle_prompt_key_for_test(&enter());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::SendPrompt(ref text)) if text == "/login"),
            "got {outcome:?}; prompt={:?}",
            agent.prompt.text()
        );
    }

    #[test]
    fn enter_keeps_typed_alias_when_that_command_is_highlighted() {
        let mut agent = agent_with_slash("/log");
        let display = agent
            .prompt
            .slash_snapshot()
            .matches
            .iter()
            .find(|row| row.display == "/log" || row.display == "/transcript")
            .map(|row| row.display.clone())
            .expect("/log or /transcript in slash menu");
        select_display(&mut agent, &display);
        let outcome = agent.handle_prompt_key_for_test(&enter());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::SendPrompt(ref text)) if text == "/log"),
            "got {outcome:?}; prompt={:?}",
            agent.prompt.text()
        );
    }
}

#[cfg(test)]
mod prompt_page_scroll_tests {
    use super::*;
    use crate::app::agent_view::AgentPane;
    use crate::views::suggestion_controller::{
        CompletionDropdownState, CompletionItemParsed, SuggestionSource,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn prompt_focused_agent() -> AgentView {
        let mut agent = super::test_fixtures::make_agent();
        agent.set_active_pane(AgentPane::Prompt, true);
        agent
    }

    #[test]
    fn modified_page_keys_do_not_page_conversation() {
        let registry = ActionRegistry::non_vscode_for_test();

        for modifiers in [
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
        ] {
            for code in [KeyCode::PageUp, KeyCode::PageDown] {
                let mut agent = prompt_focused_agent();
                let outcome =
                    agent.handle_input_with_prompt_paging(&key(code, modifiers), &registry);
                assert!(
                    !matches!(
                        outcome,
                        InputOutcome::Action(Action::PageUp | Action::PageDown)
                    ),
                    "{modifiers:?}+{code:?} must keep its existing behavior",
                );
            }
        }
    }

    #[test]
    fn active_prompt_dropdowns_own_page_keys() {
        let registry = ActionRegistry::non_vscode_for_test();
        let mut slash = prompt_focused_agent();
        slash.prompt.set_text("/");
        slash.prompt.refresh_slash(&slash.session.models);
        assert!(
            slash.prompt.slash_open(),
            "precondition: slash dropdown open"
        );
        assert!(
            slash.prompt.slash_snapshot().matches.len() > 1,
            "precondition: slash dropdown has multiple rows",
        );

        let outcome = slash.handle_input_with_prompt_paging(
            &key(KeyCode::PageDown, KeyModifiers::NONE),
            &registry,
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(slash.prompt.slash_snapshot().selected > 0);
        let outcome = slash
            .handle_input_with_prompt_paging(&key(KeyCode::PageUp, KeyModifiers::NONE), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(slash.prompt.slash_snapshot().selected, 0);

        let mut completion = prompt_focused_agent();
        completion.prompt.suggestions.dropdown = CompletionDropdownState {
            open: true,
            items: (0..8)
                .map(|i| CompletionItemParsed {
                    display: format!("item {i}"),
                    description: String::new(),
                    insert_text: format!("item {i}"),
                    source: SuggestionSource::History,
                    priority: 0,
                    ..Default::default()
                })
                .collect(),
            selected: 0,
            ..Default::default()
        };

        let outcome = completion.handle_input_with_prompt_paging(
            &key(KeyCode::PageDown, KeyModifiers::NONE),
            &registry,
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(completion.prompt.suggestions.dropdown.selected > 0);
        let outcome = completion
            .handle_input_with_prompt_paging(&key(KeyCode::PageUp, KeyModifiers::NONE), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(completion.prompt.suggestions.dropdown.selected, 0);
    }
}

#[cfg(test)]
mod combined_prompt_history_tests {
    use super::*;
    use crate::scrollback::block::RenderBlock;
    use crate::test_util::make_agent_view;

    fn texts(agent: &AgentView) -> Vec<String> {
        agent
            .combined_prompt_history()
            .into_iter()
            .map(|e| e.text)
            .collect()
    }

    /// Every kind of entry sorts against every other.
    #[test]
    fn every_kind_of_entry_sorts_against_every_other() {
        let mut agent = make_agent_view(None, "/tmp");
        agent.session.prompt_history = vec![
            "/session-info".into(),
            "! ls".into(),
            "/pwd".into(),
            "hello".into(),
        ];
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("hello"));

        assert_eq!(texts(&agent), ["/session-info", "! ls", "/pwd", "hello"]);
    }

    /// Rows keep their position when a text repeats.
    #[test]
    fn a_repeat_keeps_both_of_its_places() {
        let mut agent = make_agent_view(None, "/tmp");
        agent.session.prompt_history = vec!["/docs".into(), "fix the bug".into(), "/docs".into()];

        assert_eq!(texts(&agent), ["/docs", "fix the bug", "/docs"]);
    }

    #[test]
    fn skips_empty_trimmed_keys() {
        let mut agent = make_agent_view(None, "/tmp");
        agent.session.prompt_history = vec!["   ".into(), "ok".into(), "".into()];

        assert_eq!(texts(&agent), ["ok"]);
    }
}

#[cfg(test)]
mod history_browse_panel_tests {
    use super::*;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn agent_with_history(prompts: &[&str]) -> AgentView {
        let mut agent = super::test_fixtures::make_agent();
        agent.session.prompt_history = prompts.iter().map(|s| (*s).to_string()).collect();
        agent
    }

    /// Spin-poll the panel's background matcher until `expect` results are
    /// visible (the daemon fills the snapshot asynchronously after open).
    fn poll_results(agent: &mut AgentView, expect: usize) {
        for _ in 0..500 {
            let _ = agent.prompt.history_search.poll();
            if agent.prompt.history_search.result_count() == expect {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("history daemon did not deliver {expect} results");
    }

    /// Open the browse panel (Up) and wait for the matcher.
    fn open_browse(agent: &mut AgentView, expect: usize) {
        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        poll_results(agent, expect);
    }

    /// Up on an empty prompt opens the panel with the newest prompt
    /// selected AND already filled into the composer.
    #[test]
    fn up_opens_browse_panel_and_populates_newest() {
        let mut agent = agent_with_history(&["say cherry", "say apple"]);
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.history_search.is_active());
        assert!(agent.prompt.history_search.is_browse());
        assert_eq!(agent.prompt.text(), "say cherry");
    }

    /// While browsing, Up/Down move the selection and live-populate the
    /// composer; Up at the oldest stays put.
    #[test]
    fn selection_moves_live_populate_the_composer() {
        let mut agent = agent_with_history(&["say cherry", "say apple"]);
        open_browse(&mut agent, 2);

        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert_eq!(agent.prompt.text(), "say apple");
        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert_eq!(agent.prompt.text(), "say apple", "no wrap at the oldest");

        agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        assert_eq!(agent.prompt.text(), "say cherry");
        assert!(agent.prompt.history_search.is_active());
    }

    /// Down at the newest entry — the position the panel opens on — closes
    /// the panel and restores the empty composer.
    #[test]
    fn down_immediately_after_open_closes_the_panel() {
        let mut agent = agent_with_history(&["say cherry", "say apple"]);
        open_browse(&mut agent, 2);

        agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Normal);
    }

    #[test]
    fn down_never_opens_the_panel() {
        let mut agent = agent_with_history(&["say cherry"]);
        agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "");
    }

    /// Typing while browsing detaches: the panel closes and the key lands in
    /// the populated text as a normal edit.
    #[test]
    fn typing_detaches_and_edits_the_populated_text() {
        let mut agent = agent_with_history(&["say cherry"]);
        open_browse(&mut agent, 1);

        agent.handle_prompt_key_for_test(&key(KeyCode::Char('!')));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "say cherry!");
    }

    /// `! cmd` entries populate into bash mode (prefix stripped); the
    /// composer returns to Normal on other entries and on close.
    #[test]
    fn bash_entries_populate_bash_mode_and_back() {
        let mut agent = agent_with_history(&["! ls -la", "say apple"]);
        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert_eq!(agent.prompt.text(), "ls -la");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Bash);
        poll_results(&mut agent, 2);

        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert_eq!(agent.prompt.text(), "say apple");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Normal);

        agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        assert_eq!(agent.prompt.text(), "ls -la");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Bash);

        agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "", "past newest → empty composer");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Normal);
    }

    /// Esc while browsing restores the pre-open (empty, Normal) composer,
    /// even off a bash entry that flipped the input mode.
    #[test]
    fn esc_restores_empty_normal_composer() {
        let mut agent = agent_with_history(&["! ls -la"]);
        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Bash);

        agent.handle_prompt_key_for_test(&key(KeyCode::Esc));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Normal);
    }

    /// Ctrl+R is deliberately unbound: it must not open the history panel
    /// (search mode is reachable via /history only).
    #[test]
    fn ctrl_r_is_unbound_and_does_not_open_history() {
        let mut agent = agent_with_history(&["say cherry"]);
        agent.handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "");
    }

    #[test]
    fn up_with_a_draft_does_not_open() {
        let mut agent = agent_with_history(&["say cherry"]);
        agent.handle_prompt_key_for_test(&key(KeyCode::Char('d')));
        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "d");
    }

    #[test]
    fn bash_mode_up_does_not_open() {
        let mut agent = agent_with_history(&["say cherry"]);
        agent.prompt_input_mode = PromptInputMode::Bash;
        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "");
    }

    /// Up with no history consumes the key without opening an empty panel.
    #[test]
    fn up_with_empty_history_is_a_quiet_no_op() {
        let mut agent = agent_with_history(&[]);
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.prompt.history_search.is_active());
        assert_eq!(agent.prompt.text(), "");
    }

    /// Regression: ACCEPTING (Enter) an entry ending on an `@path` token
    /// must not re-arm the file-completion dropdown after the panel closes
    /// — same suppression the populate path applies.
    #[test]
    fn accepted_at_token_does_not_rearm_file_dropdown() {
        let mut agent = agent_with_history(&["run a subagent for @crates/codegen"]);
        open_browse(&mut agent, 1);
        agent.handle_prompt_key_for_test(&key(KeyCode::Enter));
        assert!(!agent.prompt.history_search.is_active(), "accept closes");
        assert_eq!(agent.prompt.text(), "run a subagent for @crates/codegen");
        assert!(
            !agent.prompt.file_search_visible(),
            "accept must not leave the @-completion dropdown armed"
        );
    }

    /// Regression: a populated entry ending on an `@path` token must not
    /// hand the arrows to the file-search dropdown — the panel is modal
    /// (its intercept runs first) and populate drops the `@` context, so
    /// Up/Down keep browsing and Down at the newest still closes.
    #[test]
    fn populated_at_token_does_not_steal_arrows() {
        let mut agent = agent_with_history(&["run a subagent for @crates/codegen", "older"]);
        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert_eq!(agent.prompt.text(), "run a subagent for @crates/codegen");
        assert!(
            !agent.prompt.file_search_visible(),
            "populate must not leave the @-completion dropdown armed"
        );
        poll_results(&mut agent, 2);

        agent.handle_prompt_key_for_test(&key(KeyCode::Up));
        assert_eq!(agent.prompt.text(), "older", "Up must keep browsing");
        agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        assert_eq!(agent.prompt.text(), "run a subagent for @crates/codegen");
        agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        assert!(
            !agent.prompt.history_search.is_active(),
            "Down at the newest must still close the panel"
        );
        assert_eq!(agent.prompt.text(), "");
    }
}

#[cfg(test)]
mod rewind_grace_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Pure deadline semantics with an injected `now`: suppressed strictly
    /// before the deadline, expired (and retired) at it.
    #[test]
    fn suppress_rewind_arm_holds_until_deadline_then_retires() {
        let mut agent = super::test_fixtures::make_agent();
        let t0 = Instant::now();
        assert!(!agent.rewind_arm_suppressed(t0), "no cancel yet — no grace");

        agent.suppress_rewind_arm(t0);
        assert!(agent.rewind_arm_suppressed(t0));
        assert!(agent.rewind_arm_suppressed(
            t0 + AgentView::ESC_CANCEL_REWIND_GRACE - Duration::from_millis(1)
        ));

        assert!(
            !agent.rewind_arm_suppressed(t0 + AgentView::ESC_CANCEL_REWIND_GRACE),
            "the deadline itself is expiry"
        );
        assert!(
            agent.rewind_suppress_deadline.is_none(),
            "the expired deadline is cleared on the consult"
        );
    }

    /// A later Esc-fired cancel (e.g. a cancel-retry mash) pushes the
    /// deadline out; the grace is measured from the LAST cancel press.
    #[test]
    fn suppress_rewind_arm_refreshes_on_later_cancel() {
        let mut agent = super::test_fixtures::make_agent();
        let t0 = Instant::now();
        agent.suppress_rewind_arm(t0);
        let t1 = t0 + Duration::from_millis(500);
        agent.suppress_rewind_arm(t1);
        assert!(agent.rewind_arm_suppressed(t0 + AgentView::ESC_CANCEL_REWIND_GRACE));
        assert!(!agent.rewind_arm_suppressed(t1 + AgentView::ESC_CANCEL_REWIND_GRACE));
    }

    /// The grace must outlast one full idle double-press window — see the
    /// constant's doc for why.
    #[test]
    fn esc_cancel_rewind_grace_outlives_double_press_ttl() {
        assert!(
            AgentView::ESC_CANCEL_REWIND_GRACE
                > crate::app::app_view::PendingAction::ESC_DOUBLE_PRESS_TTL
        );
    }
}

#[cfg(test)]
mod prompt_suggestion_key_tests {
    use super::*;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Idle agent with the gate open and a loaded suggestion — the state
    /// right after a turn ends with `echo.agent/suggestPrompt` resolved. Pins the
    /// settings cache so `resolve_enabled()` never reads the dev machine's
    /// config.toml (thread-local, so per-test).
    fn suggestion_agent(text: &str) -> AgentView {
        crate::appearance::cache::set_prompt_suggestions(true);
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.prompt_suggestion.set_suggestion_for_test(text);
        agent.refresh_prompt_suggestion_gate();
        agent
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn tab_accepts_suggestion_into_prompt() {
        let mut agent = suggestion_agent("run the tests");
        assert!(agent.prompt.prompt_suggestion_visible());

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "run the tests");
        assert!(!agent.prompt.prompt_suggestion_visible());
    }

    #[test]
    fn tab_accepts_remainder_after_matching_prefix_typed() {
        let mut agent = suggestion_agent("run the tests");
        agent.prompt.textarea.insert_str("run ");
        assert_eq!(agent.prompt.prompt_suggestion_ghost(), Some("the tests"));

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "run the tests");
    }

    #[test]
    fn tab_falls_through_to_focus_scrollback_without_suggestion() {
        crate::appearance::cache::set_prompt_suggestions(true);
        let mut agent = super::test_fixtures::make_agent();
        agent.refresh_prompt_suggestion_gate();

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(
            matches!(outcome, InputOutcome::Action(Action::FocusScrollback)),
            "Tab keeps its focus-cycling behavior when no ghost is visible: {outcome:?}"
        );
    }

    /// Tab drops the highlight even though the registry consumes the chord.
    #[test]
    fn tab_with_selection_clears_highlight_and_focuses_scrollback() {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.textarea.insert_str("alpha beta");
        agent.prompt.textarea.set_selection(0, 5);

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::FocusScrollback)
        ));
        assert_eq!(agent.prompt.textarea.selection_range(), None);
    }

    /// Shift+Tab (CycleMode) drops the highlight on the same press too.
    #[test]
    fn shift_tab_with_selection_clears_highlight_and_cycles_mode() {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.textarea.insert_str("alpha beta");
        agent.prompt.textarea.set_selection(0, 5);

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::BackTab));
        assert!(!matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(agent.prompt.textarea.selection_range(), None);
    }

    #[test]
    fn right_arrow_accepts_suggestion() {
        // Right at end-of-text is otherwise a no-op, so it doubles as
        // accept whenever the ghost is visible — in any mode.
        let mut agent = suggestion_agent("commit this");
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Right));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "commit this");
        assert!(!agent.prompt.prompt_suggestion_visible());
    }

    #[test]
    fn right_arrow_accepts_remainder_after_matching_prefix() {
        let mut agent = suggestion_agent("run the tests");
        agent.prompt.textarea.insert_str("run ");
        assert_eq!(agent.prompt.prompt_suggestion_ghost(), Some("the tests"));

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Right));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "run the tests");
    }

    #[test]
    fn right_arrow_mid_text_stays_cursor_movement() {
        // Cursor away from end-of-text hides the ghost, so Right falls
        // through to the widget as plain cursor movement.
        let mut agent = suggestion_agent("run the tests");
        agent.prompt.textarea.insert_str("run ");
        agent.prompt.textarea.set_cursor(1);
        assert_eq!(agent.prompt.prompt_suggestion_ghost(), None);

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Right));
        assert_eq!(agent.prompt.text(), "run ", "no acceptance mid-text");
        assert_eq!(agent.prompt.cursor(), 2, "Right moved the cursor");
        assert!(agent.prompt.prompt_suggestion.has_suggestion());
    }

    #[test]
    fn left_arrow_never_accepts() {
        let mut agent = suggestion_agent("commit this");
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Left));
        assert_eq!(agent.prompt.text(), "", "Left must not accept");
        assert!(agent.prompt.prompt_suggestion.has_suggestion());
    }

    #[test]
    fn esc_dismisses_suggestion_on_empty_prompt() {
        let mut agent = suggestion_agent("run the tests");

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Esc));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.prompt.prompt_suggestion_visible());
        assert!(!agent.prompt.prompt_suggestion.has_suggestion());
    }

    #[test]
    fn running_turn_gates_ghost_off() {
        let mut agent = suggestion_agent("run the tests");
        agent.session.start_turn(&mut agent.scrollback);
        agent.refresh_prompt_suggestion_gate();
        assert!(!agent.prompt.prompt_suggestion_visible());

        // Tab while running falls through instead of accepting.
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(
            !matches!(outcome, InputOutcome::Changed if agent.prompt.text() == "run the tests")
        );
        assert_eq!(agent.prompt.text(), "");
    }

    #[test]
    fn bash_mode_gates_ghost_off() {
        let mut agent = suggestion_agent("run the tests");
        agent.prompt_input_mode = PromptInputMode::Bash;
        agent.refresh_prompt_suggestion_gate();
        assert!(!agent.prompt.prompt_suggestion_visible());
    }

    #[test]
    fn typing_matching_prefix_shrinks_ghost() {
        let mut agent = suggestion_agent("run the tests");
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Char('r')));
        assert_eq!(agent.prompt.text(), "r");
        assert_eq!(agent.prompt.prompt_suggestion_ghost(), Some("un the tests"));

        // Divergent char hides it; suggestion stays loaded for backspace.
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Char('x')));
        assert_eq!(agent.prompt.prompt_suggestion_ghost(), None);
        assert!(agent.prompt.prompt_suggestion.has_suggestion());
    }

    /// A suggestion that loads behind a divergent draft logs no `shown`
    /// impression at load; once the draft is cleared the ghost becomes
    /// visible and the next key event latches `shown` *before* its Tab
    /// intercept can log `accepted` — the funnel can't record an accept
    /// without an impression.
    #[test]
    fn shown_latches_at_first_visibility_after_divergent_draft_clears() {
        crate::appearance::cache::set_prompt_suggestions(true);
        let mut agent = super::test_fixtures::make_agent();
        // Divergent draft typed while the suggestion fetch was in flight.
        agent.prompt.textarea.insert_str("x");
        // The load path: install + gate refresh + latch-if-visible.
        let generation = agent.prompt.prompt_suggestion.begin_fetch();
        assert!(
            agent
                .prompt
                .prompt_suggestion
                .on_loaded(Some("run the tests".to_owned()), generation)
        );
        agent.refresh_prompt_suggestion_gate();
        agent.log_prompt_suggestion_shown_if_visible();
        assert!(!agent.prompt.prompt_suggestion_visible());
        assert!(
            !agent.prompt.prompt_suggestion.shown_logged(),
            "a ghost hidden by a divergent draft is not an impression"
        );

        // Backspace empties the draft. The intercept ran before the edit
        // (ghost still hidden then), so this event doesn't latch either.
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Backspace));
        assert_eq!(agent.prompt.text(), "");
        assert!(agent.prompt.prompt_suggestion_visible());
        assert!(!agent.prompt.prompt_suggestion.shown_logged());

        // The next key event sees the visible ghost and latches `shown`
        // first; the same event's Tab intercept then accepts.
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(agent.prompt.prompt_suggestion.shown_logged());
        assert_eq!(agent.prompt.text(), "run the tests");
    }

    /// Esc-dismiss on a late-visible ghost also latches `shown` first (same
    /// key event), so `dismissed` never outruns `shown` either.
    #[test]
    fn shown_latches_before_dismiss_on_same_key_event() {
        let mut agent = suggestion_agent("run the tests");
        assert!(!agent.prompt.prompt_suggestion.shown_logged());

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Esc));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.prompt.prompt_suggestion.shown_logged(),
            "the key event latches the impression before dismissing"
        );
        assert!(!agent.prompt.prompt_suggestion.has_suggestion());
    }
}

#[cfg(test)]
mod apple_terminal_ctrl_o_upgrade_cta_tests {
    use super::*;
    use crate::app::agent::AgentState;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use echo_agent_telemetry::events::AnnouncementCtaSurface;

    fn ctrl_o() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)
    }

    #[test]
    fn apple_terminal_idle_ctrl_o_opens_pinned_upgrade_cta() {
        let mut agent = super::test_fixtures::make_agent();
        agent.pinned_upgrade_cta_live = true;
        let registry = ActionRegistry::apple_terminal_for_test();

        let outcome = agent.handle_prompt_key_with_registry_for_test(&ctrl_o(), &registry);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(
                    AnnouncementCtaSurface::Keyboard
                ))
            ),
            "idle Apple-Terminal Ctrl+O must open pinned CTA, got {outcome:?}"
        );
    }

    #[test]
    fn apple_terminal_idle_ctrl_o_without_promo_toggles_yolo() {
        let mut agent = super::test_fixtures::make_agent();
        agent.pinned_upgrade_cta_live = false;
        let was_yolo = agent.session.is_yolo();
        let registry = ActionRegistry::apple_terminal_for_test();

        let outcome = agent.handle_prompt_key_with_registry_for_test(&ctrl_o(), &registry);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::SetYoloMode(m)) if m != was_yolo
            ),
            "idle Apple-Terminal Ctrl+O without promo must toggle YOLO, got {outcome:?}"
        );
    }

    #[test]
    fn apple_terminal_running_ctrl_o_still_interjects() {
        let mut agent = super::test_fixtures::make_agent();
        agent.session.state = AgentState::TurnRunning;
        agent.prompt.set_text("steer mid-turn");
        agent.pinned_upgrade_cta_live = true;
        let registry = ActionRegistry::apple_terminal_for_test();

        let outcome = agent.handle_prompt_key_with_registry_for_test(&ctrl_o(), &registry);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::SendPromptNow { ref text, .. })
                    if text == "steer mid-turn"
            ),
            "running + payload: interject must win over CTA, got {outcome:?}"
        );
    }

    #[test]
    fn non_apple_ctrl_enter_idle_stays_noop() {
        let mut agent = super::test_fixtures::make_agent();
        agent.pinned_upgrade_cta_live = true;
        let registry = ActionRegistry::non_vscode_for_test();
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);

        let outcome = agent.handle_prompt_key_with_registry_for_test(&ctrl_enter, &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "idle Ctrl+Enter must stay a silent interject no-op, got {outcome:?}"
        );
    }
}

#[cfg(test)]
mod queue_recall_tests {
    use super::*;
    use crate::app::agent_view::test_fixtures::make_running_agent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn up(agent: &mut AgentView) -> InputOutcome {
        agent.handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
    }

    /// Focus lands on the bottom row, not the top one send-now takes.
    #[test]
    fn up_focuses_the_queue_on_its_bottom_row() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;

        up(&mut agent);

        assert_eq!(agent.active_pane, AgentPane::Queue);
        assert!(agent.queue.overlay.focused);
        let bottom = *agent.queue.entry_ids().last().unwrap();
        assert_eq!(agent.queue.selected_id(), Some(bottom));
    }

    #[test]
    fn up_leaves_the_composer_and_history_untouched() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.session.prompt_history = vec!["an older prompt".into()];

        up(&mut agent);

        assert!(agent.prompt.text().is_empty());
        assert!(!matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));
        assert!(!agent.prompt.history_search.is_browse());
    }

    /// Up stays cursor motion while the composer holds text.
    #[test]
    fn up_with_a_non_empty_composer_leaves_the_queue_alone() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("half a thought");

        up(&mut agent);

        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert_eq!(agent.prompt.text(), "half a thought");
    }

    #[test]
    fn up_falls_back_to_history_with_an_empty_queue() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.session.pending_prompts.clear();
        agent.shared_queue.clear();
        agent.sync_queue_pane();
        agent.session.prompt_history = vec!["an older prompt".into()];

        up(&mut agent);

        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert_eq!(agent.prompt.text(), "an older prompt");
    }
}
