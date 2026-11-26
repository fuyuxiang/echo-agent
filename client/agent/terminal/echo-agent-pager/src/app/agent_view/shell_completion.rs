//! Bash-mode shell completion: the always-on Tab surface (deterministic
//! fetch arming, terminal Tab semantics execution) and the dropdown accept
//! path shared by Tab/Enter/mouse.

#[cfg(test)]
use super::test_fixtures;
use super::{AgentView, PromptInputMode};
use crate::views::suggestion_controller::TabAction;

impl AgentView {
    /// Accept the selected completion-dropdown item into the prompt (the
    /// what-to-write policy lives in `CompletionSplice`). Returns whether
    /// the key was consumed; `false` only for the empty-items race (callers
    /// keep their close-and-fall-through arm).
    pub(in crate::app) fn accept_completion_dropdown_item(&mut self) -> bool {
        let had_items = !self.prompt.suggestions.dropdown.items.is_empty();
        // The SELECTED splice would clip an atomic element (paste chip):
        // committing would consume the candidates and then be declined by
        // the write path — honest no-op instead (nothing safe to write;
        // the dropdown stays up so another selection can still accept).
        if self.prompt.completion_accept_would_clip_element() {
            return true;
        }
        let Some(splice) = self.prompt.completion_dropdown_accept() else {
            // Stale-generation refusal closed the dropdown; swallow the key
            // (the refreshed fetch is in flight) instead of falling through
            // to focus-cycling or send.
            return had_items;
        };
        if self.prompt.apply_completion_splice(splice) {
            self.prompt_input_mode = PromptInputMode::Bash;
            // Re-fetch for the accepted text so accepting a directory
            // (trailing `/`) lets the NEXT Tab complete inside it.
            self.kick_shell_suggest_refetch();
        }
        true
    }

    /// Terminal-like Tab over a closed dropdown's completion items: decide
    /// via `SuggestionController::tab_decision`, then execute. Used by the
    /// pending-Tab landing (where `Nothing` — stale/empty items — must do
    /// nothing rather than fetch again).
    pub(in crate::app) fn shell_completion_tab(&mut self) {
        let action = self
            .prompt
            .suggestions
            .tab_decision(self.prompt.text(), self.prompt.cursor());
        self.execute_tab_action(action);
    }

    /// View-side executor for a [`TabAction`] (the policy lives in the
    /// controller's `tab_decision`).
    pub(super) fn execute_tab_action(&mut self, action: TabAction) {
        match action {
            TabAction::InstaAccept => {
                // A splice clipping an atomic element (paste chip) would be
                // declined AFTER the accept consumed the sole candidate —
                // every Tab would then refetch the same set. Show it instead.
                if self.prompt.completion_accept_would_clip_element() {
                    self.prompt.completion_dropdown_open_if_available();
                } else {
                    self.accept_completion_dropdown_item();
                }
            }
            TabAction::Fill(range, fill) => {
                if self.prompt.apply_completion_fill(range, &fill) {
                    // A fill is typing: refresh the candidate set for the longer
                    // token (the next Tab opens the dropdown on the refreshed set).
                    self.kick_shell_suggest_refetch();
                } else {
                    // Declined (range clips an atomic element): show the
                    // candidates instead of respinning fill+refetch every Tab.
                    self.prompt.completion_dropdown_open_if_available();
                }
            }
            TabAction::Open => {
                self.prompt.completion_dropdown_open_if_available();
            }
            TabAction::Nothing => {}
        }
    }

    /// Fire a deterministic (`includeAi: false`) completion fetch for the
    /// current draft, bypassing the env-gated as-you-type debounce — the
    /// always-on Tab path. `run_tab_on_load` makes the landing response run
    /// the terminal Tab semantics once (a Tab that found no usable items
    /// still completes when its candidates arrive).
    pub(super) fn request_shell_tab_completion(&mut self, run_tab_on_load: bool) {
        // Repeat Tab while the armed fetch is still in flight: keep the
        // marker (its landing runs the Tab semantics) — no second RPC.
        if run_tab_on_load && self.prompt.suggestions.tab_fetch_pending() {
            return;
        }
        let generation = self
            .prompt
            .suggestions
            .begin_tab_completion(run_tab_on_load);
        self.pending_effects
            .push(super::actions::Effect::FetchShellSuggestions {
                agent_id: self.session.id,
                text: self.prompt.text().to_owned(),
                cursor: self.prompt.cursor(),
                cwd: self.session.cwd.to_string_lossy().into_owned(),
                generation,
                limit: crate::views::suggestion_controller::SHELL_SUGGEST_WIRE_LIMIT,
                include_ai: false,
                ai_model: None,
                session_id: self.session.session_id.as_ref().map(|s| s.0.to_string()),
                // Deterministic Tab surface: token providers only (a
                // history row would make the set mixed and kill
                // insta-accept/LCP).
                token_only: true,
            });
    }

    /// Refresh the candidate set after an accept or a prefix fill changed
    /// the draft: through the debounced as-you-type pipeline when enabled,
    /// else a direct deterministic fetch. Either way the refreshed items
    /// land silently and the NEXT Tab consumes them.
    fn kick_shell_suggest_refetch(&mut self) {
        if self.prompt.suggestions.enabled {
            if let Some(eff) = self.notify_suggestion_text_changed() {
                self.pending_effects.push(eff);
            }
        } else {
            self.request_shell_tab_completion(false);
        }
    }
}

#[cfg(test)]
mod shell_suggestion_key_tests {
    use super::*;
    use crate::app::actions::{Action, Effect};
    use crate::app::app_view::InputOutcome;
    use crate::views::suggestion_controller::{CompletionItemParsed, SuggestionSource};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Wire-shaped token item: `insert_text` is the compat whole line,
    /// `token_text` the span replacement (what a new shell sends).
    fn token_item(line: &str, token: &str, range: std::ops::Range<usize>) -> CompletionItemParsed {
        CompletionItemParsed {
            display: token.to_owned(),
            description: String::new(),
            insert_text: line.to_owned(),
            source: SuggestionSource::PathExecutable,
            priority: 0,
            replace_range: Some(range),
            token_text: Some(token.to_owned()),
            truncated: false,
        }
    }

    fn item(insert: &str, range: Option<std::ops::Range<usize>>) -> CompletionItemParsed {
        CompletionItemParsed {
            display: insert.to_owned(),
            description: String::new(),
            insert_text: insert.to_owned(),
            source: SuggestionSource::PathExecutable,
            priority: 0,
            replace_range: range,
            token_text: None,
            truncated: false,
        }
    }

    /// Wire-shaped FILE token item (what the file provider sends).
    fn file_item(line: &str, token: &str, range: std::ops::Range<usize>) -> CompletionItemParsed {
        CompletionItemParsed {
            display: token.to_owned(),
            description: String::new(),
            insert_text: line.to_owned(),
            source: SuggestionSource::FilePath,
            priority: 0,
            replace_range: Some(range),
            token_text: Some(token.to_owned()),
            truncated: false,
        }
    }

    /// Whole-line history item (insert_text doubles as the span replacement).
    fn history_item(line: &str, range: std::ops::Range<usize>) -> CompletionItemParsed {
        CompletionItemParsed {
            display: line.to_owned(),
            description: String::new(),
            insert_text: line.to_owned(),
            source: SuggestionSource::History,
            priority: 10,
            replace_range: Some(range),
            token_text: None,
            truncated: false,
        }
    }

    /// Bash-mode agent with the env-gated as-you-type pipeline ON and
    /// `text` typed (the dropdown's request-text anchor pinned to it — the
    /// state right after a suggest response landed for the draft).
    fn bash_agent(text: &str) -> AgentView {
        let mut agent = bash_agent_always_on(text);
        agent.prompt.suggestions.enabled = true;
        agent
    }

    /// Same, with the pipeline OFF (`ECHO_AGENT_SUGGESTIONS` unset) — the
    /// always-on Tab surface under test.
    fn bash_agent_always_on(text: &str) -> AgentView {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt_input_mode = PromptInputMode::Bash;
        agent.prompt.suggestions.enabled = false;
        agent.prompt.textarea.insert_str(text);
        agent.prompt.suggestions.dropdown.request_text = text.to_owned();
        agent.prompt.suggestions.dropdown.request_cursor = text.len();
        agent
    }

    /// THE acceptance regression: accepting a $PATH item after `ls | gr`
    /// edits the token in place — never replaces the whole line with `grep`.
    #[test]
    fn dropdown_tab_accept_replaces_token_in_place() {
        let mut agent = bash_agent("ls | gr");
        agent.prompt.suggestions.dropdown.open = true;
        agent.prompt.suggestions.dropdown.items = vec![token_item("ls | grep", "grep", 5..7)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "ls | grep");
        assert_eq!(agent.prompt.cursor(), "ls | grep".len());
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Bash);
        assert!(!agent.prompt.completion_dropdown_open());
    }

    /// Enter accepts the same way (both arms share the accept helper).
    #[test]
    fn dropdown_enter_accept_replaces_token_in_place() {
        let mut agent = bash_agent("ls | gr");
        agent.prompt.suggestions.dropdown.open = true;
        agent.prompt.suggestions.dropdown.items = vec![token_item("ls | grep", "grep", 5..7)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Enter));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "ls | grep");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Bash);
    }

    /// The accept works identically with the as-you-type pipeline OFF —
    /// in-place acceptance is not env-gated.
    #[test]
    fn dropdown_accept_works_without_env_flag() {
        let mut agent = bash_agent_always_on("ls | gr");
        agent.prompt.suggestions.dropdown.open = true;
        agent.prompt.suggestions.dropdown.items = vec![token_item("ls | grep", "grep", 5..7)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "ls | grep");
    }

    /// A ranged item whose range no longer fits the draft is a NO-OP accept
    /// — the draft survives untouched, the dropdown closes, the key is
    /// consumed (never a whole-line clobber, never a send).
    #[test]
    fn dropdown_accept_stale_range_is_a_draft_preserving_noop() {
        let mut agent = bash_agent("ls | gr");
        agent.prompt.set_text("totally different");
        agent.prompt.suggestions.dropdown.open = true;
        agent.prompt.suggestions.dropdown.items = vec![token_item("ls | grep", "grep", 5..7)];
        // Pass the generation gate (`set_text` bumped it) so this pins the
        // range-validation no-op, not the staleness gate. The "ls | gr"
        // anchor from `bash_agent` survives the swap (close() keeps it).
        agent.prompt.suggestions.dropdown.generation = agent.prompt.suggestions.generation();

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "totally different");
        assert!(!agent.prompt.completion_dropdown_open());
    }

    /// Items populated for a superseded generation refuse the accept
    /// wholesale: dropdown closes, draft untouched, Enter does not fall
    /// through to send.
    #[test]
    fn dropdown_accept_stale_generation_is_a_noop() {
        let mut agent = bash_agent("ls | gr");
        agent.prompt.suggestions.dropdown.open = true;
        agent.prompt.suggestions.dropdown.items = vec![token_item("ls | grep", "grep", 5..7)];
        // A newer edit bumped the controller past the items' generation.
        agent.prompt.suggestions.dropdown.generation = 3;

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Enter));
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "stale accept must consume the key, got {outcome:?}"
        );
        assert_eq!(agent.prompt.text(), "ls | gr");
        assert!(!agent.prompt.completion_dropdown_open());
    }

    /// Rangeless items (older shells) keep the whole-line behavior.
    #[test]
    fn dropdown_accept_without_range_sets_whole_line() {
        let mut agent = bash_agent("git st");
        agent.prompt.suggestions.dropdown.open = true;
        agent.prompt.suggestions.dropdown.items = vec![item("git status --porcelain", None)];

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert_eq!(agent.prompt.text(), "git status --porcelain");
        assert_eq!(agent.prompt.cursor(), agent.prompt.text().len());
    }

    /// Tab opens the dropdown whenever items exist — a ghost is NOT required
    /// (pure path/file completions never carry one). Two candidates with no
    /// shared prefix beyond the typed token = the plain-open path (a single
    /// candidate insta-accepts instead — see the terminal-Tab tests below).
    #[test]
    fn tab_opens_dropdown_without_ghost() {
        let mut agent = bash_agent("ls | gr");
        agent.prompt.suggestions.dropdown.items =
            vec![item("grep", Some(5..7)), item("grip", Some(5..7))];
        assert!(!agent.prompt.has_ghost_text());
        assert!(!agent.prompt.completion_dropdown_open());

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(
            agent.prompt.text(),
            "ls | gr",
            "no fill without a longer LCP"
        );
    }

    // -- always-on Tab fetch (no ECHO_AGENT_SUGGESTIONS) --------------------------

    /// Tab in bash mode with no fetched candidates fires a deterministic
    /// fetch — no env flag, no AI, dropdown-scale limit.
    #[test]
    fn tab_without_items_fires_deterministic_fetch() {
        let mut agent = bash_agent_always_on("cat no");
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));

        let fetch = agent.pending_effects.iter().find_map(|e| match e {
            Effect::FetchShellSuggestions {
                include_ai,
                generation,
                limit,
                text,
                token_only,
                ..
            } => Some((*include_ai, *generation, *limit, text.clone(), *token_only)),
            _ => None,
        });
        let (include_ai, generation, limit, text, token_only) =
            fetch.expect("Tab must fire a fetch");
        assert!(!include_ai, "Tab completion is deterministic (no AI)");
        assert!(token_only, "Tab fetches run only the token providers");
        assert_eq!(limit, 50);
        assert_eq!(text, "cat no");
        assert_eq!(generation, agent.prompt.suggestions.generation());
    }

    /// Repeat Tab while the armed fetch is still in flight is a no-op: one
    /// RPC, one landing that runs the Tab semantics once.
    #[test]
    fn repeat_tab_fires_single_fetch_while_pending() {
        let mut agent = bash_agent_always_on("cat no");
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));

        let fetches = agent
            .pending_effects
            .iter()
            .filter(|e| matches!(e, Effect::FetchShellSuggestions { .. }))
            .count();
        assert_eq!(fetches, 1, "the second Tab must not fire a second RPC");
        assert!(
            agent.prompt.suggestions.tab_fetch_pending(),
            "the pending-Tab marker survives the repeat press"
        );
    }

    /// Items outdated by an edit (stale generation) refetch instead of
    /// completing over the old candidate set.
    #[test]
    fn tab_with_stale_items_refetches() {
        let mut agent = bash_agent_always_on("cat no");
        agent.prompt.suggestions.dropdown.items = vec![file_item("cat notes.md", "notes.md", 4..6)];
        agent.prompt.suggestions.dropdown.generation = 7;

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert_eq!(agent.prompt.text(), "cat no", "no accept from stale items");
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::FetchShellSuggestions { .. })),
            "stale items must refetch"
        );
    }

    /// An empty bash draft has no token to complete: Tab keeps its
    /// focus-cycling fallthrough.
    #[test]
    fn tab_on_empty_bash_draft_falls_through_to_focus_scrollback() {
        let mut agent = bash_agent_always_on("");
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::FocusScrollback)
        ));
        assert!(agent.pending_effects.is_empty());
    }

    /// The normal (chat) prompt keeps its Tab behavior: no fetch, no
    /// completion — the surface is bash-mode-only.
    #[test]
    fn tab_in_normal_mode_does_not_fetch() {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.textarea.insert_str("cat no");

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(
            !agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::FetchShellSuggestions { .. })),
            "normal-mode Tab must not fetch completions"
        );
    }

    // -- terminal-like Tab (single-candidate accept / common-prefix fill) --

    /// Exactly one token candidate: Tab accepts it immediately — no
    /// dropdown flash — and the accept re-fetch keeps the pipeline alive.
    #[test]
    fn tab_single_token_candidate_accepts_without_dropdown_flash() {
        let mut agent = bash_agent("cat no");
        agent.prompt.suggestions.dropdown.items = vec![file_item("cat notes.md", "notes.md", 4..6)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "cat notes.md");
        assert_eq!(agent.prompt.cursor(), "cat notes.md".len());
        assert!(!agent.prompt.completion_dropdown_open());
    }

    /// The same insta-accept with the pipeline OFF: the refetch kick is a
    /// direct deterministic fetch instead of a debounce.
    #[test]
    fn tab_single_candidate_accepts_and_kicks_fetch_always_on() {
        let mut agent = bash_agent_always_on("cat no");
        agent.prompt.suggestions.dropdown.items = vec![file_item("cat notes.md", "notes.md", 4..6)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "cat notes.md");
        assert!(
            agent.pending_effects.iter().any(|e| matches!(
                e,
                Effect::FetchShellSuggestions {
                    include_ai: false,
                    ..
                }
            )),
            "accept must kick a deterministic refetch"
        );
    }

    /// A single HISTORY item keeps the plain dropdown-open behavior:
    /// terminal Tab semantics apply to token completions only.
    #[test]
    fn tab_single_history_item_opens_dropdown() {
        let mut agent = bash_agent("git st");
        agent.prompt.suggestions.dropdown.items =
            vec![history_item("git status --porcelain", 0..6)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(agent.prompt.text(), "git st");
    }

    /// THE legacy-shell compatibility case: a rangeless `path` row (old
    /// shells send `insertText: "grep"`, no range) must never insta-accept
    /// — its whole-line fallback would replace `ls | gr` with `grep`. Tab
    /// plain-opens instead, sole match or not.
    #[test]
    fn tab_sole_rangeless_path_row_opens_dropdown_never_accepts() {
        let mut agent = bash_agent("ls | gr");
        agent.prompt.suggestions.dropdown.items = vec![item("grep", None)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "ls | gr", "draft must survive");
        assert!(agent.prompt.completion_dropdown_open());
    }

    /// Any rangeless row in a MIXED set (legacy PATH row next to a ranged
    /// file row) forces plain-open too — no insta-accept, no fill.
    #[test]
    fn tab_mixed_rangeless_and_ranged_rows_open_dropdown() {
        let mut agent = bash_agent("ls | gr");
        agent.prompt.suggestions.dropdown.items = vec![
            item("grep", None),
            file_item("ls | echoagentfile", "echoagentfile", 5..7),
        ];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(agent.prompt.text(), "ls | gr", "no accept, no fill");
    }

    /// A MIXED set (any non-token item alongside file/path rows) disables
    /// terminal-Tab semantics wholesale: no insta-accept, no fill — Tab
    /// plain-opens so the user sees every candidate, history included.
    #[test]
    fn tab_mixed_file_and_history_items_opens_dropdown() {
        let mut agent = bash_agent("cat no");
        agent.prompt.suggestions.dropdown.items = vec![
            history_item("cat notes.md --verbose", 0..6),
            file_item("cat notes.md", "notes.md", 4..6),
        ];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(agent.prompt.text(), "cat no", "no accept, no fill");
    }

    /// Whole-line history sets never prefix-fill (half a history line is
    /// not a command) — Tab plain-opens.
    #[test]
    fn tab_whole_line_history_items_open_dropdown_not_fill() {
        let mut agent = bash_agent("git st");
        agent.prompt.suggestions.dropdown.items = vec![
            history_item("git status --porcelain-A", 0..6),
            history_item("git status --porcelain-B", 0..6),
        ];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(agent.prompt.text(), "git st");
    }

    /// Multiple candidates sharing a prefix longer than the typed token:
    /// the first Tab fills the common prefix in place (no dropdown) and
    /// re-fetches; when the refreshed items land, the second Tab opens the
    /// dropdown.
    #[test]
    fn tab_fills_common_prefix_then_opens_dropdown_on_refresh() {
        let mut agent = bash_agent("cat al");
        agent.prompt.suggestions.dropdown.items = vec![
            file_item("cat alpha_one.txt", "alpha_one.txt", 4..6),
            file_item("cat alpha_two.txt", "alpha_two.txt", 4..6),
        ];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "cat alpha_");
        assert_eq!(agent.prompt.cursor(), "cat alpha_".len());
        assert!(
            !agent.prompt.completion_dropdown_open(),
            "first Tab fills; the dropdown waits for the second"
        );
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::DebounceSuggestions { .. })),
            "the fill re-fetches candidates for the longer prefix"
        );

        // The refreshed response lands for the filled text…
        let generation = agent.prompt.suggestions.generation();
        agent.prompt.suggestions.on_suggestions_loaded(
            crate::views::suggestion_controller::SuggestResponseParsed {
                ghost: None,
                completions: vec![
                    file_item("cat alpha_one.txt", "alpha_one.txt", 4..10),
                    file_item("cat alpha_two.txt", "alpha_two.txt", 4..10),
                ],
                generation,
            },
            "cat alpha_",
            "cat alpha_".len(),
        );

        // …and the second Tab opens the dropdown (LCP no longer extends).
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(agent.prompt.text(), "cat alpha_");
    }

    /// The fill's refetch with the pipeline OFF is a direct deterministic
    /// fetch (no debounce to ride on).
    #[test]
    fn tab_fill_kicks_deterministic_fetch_always_on() {
        let mut agent = bash_agent_always_on("cat al");
        agent.prompt.suggestions.dropdown.items = vec![
            file_item("cat alpha_one.txt", "alpha_one.txt", 4..6),
            file_item("cat alpha_two.txt", "alpha_two.txt", 4..6),
        ];

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert_eq!(agent.prompt.text(), "cat alpha_");
        assert!(
            agent.pending_effects.iter().any(|e| matches!(
                e,
                Effect::FetchShellSuggestions {
                    include_ai: false,
                    ..
                }
            )),
            "fill must kick a deterministic refetch"
        );
    }

    /// Bash-mode agent whose draft is a paste CHIP (atomic element), with
    /// the dropdown anchor pinned to it — the state a landing would leave
    /// when the shell's token range points into the chip's raw text.
    fn chip_agent(items: Vec<CompletionItemParsed>) -> (AgentView, String) {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt_input_mode = PromptInputMode::Bash;
        agent.prompt.suggestions.enabled = false;
        agent
            .prompt
            .handle_paste("line one\nline two\nline three\nline four");
        let text = agent.prompt.text().to_owned();
        agent.prompt.suggestions.dropdown.request_text = text.clone();
        agent.prompt.suggestions.dropdown.request_cursor = agent.prompt.cursor();
        agent.prompt.suggestions.dropdown.items = items;
        (agent, text)
    }

    fn suggest_fetch_count(agent: &AgentView) -> usize {
        agent
            .pending_effects
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::FetchShellSuggestions { .. } | Effect::DebounceSuggestions { .. }
                )
            })
            .count()
    }

    /// BugBot: a Fill whose range clips a paste chip used to no-op the
    /// write and STILL kick a refetch — every Tab spun fill+refetch with no
    /// draft change. The declined fill now degrades to opening the
    /// dropdown: candidates visible, nothing fetched, chip intact, and the
    /// second Tab rides the normal open-dropdown handling.
    #[test]
    fn tab_fill_clipping_paste_chip_opens_dropdown_without_refetch() {
        // Two candidates whose shared range (chip bytes 0..2, "li") fills
        // to "lima_" — a valid Fill decision over an unwritable span.
        let (mut agent, text) = chip_agent(vec![
            file_item("lima_one.txt", "lima_one.txt", 0..2),
            file_item("lima_two.txt", "lima_two.txt", 0..2),
        ]);
        let gen_before = agent.prompt.suggestions.generation();

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), text, "chip must survive the fill");
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(
            agent.prompt.suggestions.generation(),
            gen_before,
            "a declined fill must not invalidate anything"
        );
        assert_eq!(suggest_fetch_count(&agent), 0, "no refetch kick");

        // Second Tab goes through the open dropdown (accept path), never
        // the fetch arm — no spin.
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert_eq!(agent.prompt.text(), text);
        assert_eq!(suggest_fetch_count(&agent), 0);
    }

    /// Same hole on the insta-accept arm: committing would consume the
    /// sole candidate and THEN decline the splice, leaving every Tab to
    /// refetch the same set. The probe degrades to showing the candidate.
    #[test]
    fn tab_insta_accept_clipping_paste_chip_opens_dropdown_without_refetch() {
        let (mut agent, text) = chip_agent(vec![file_item("lima_one.txt", "lima_one.txt", 0..2)]);

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), text, "chip must survive");
        assert!(agent.prompt.completion_dropdown_open());
        assert_eq!(
            agent.prompt.suggestions.dropdown.items.len(),
            1,
            "the candidate must not be consumed"
        );
        assert_eq!(suggest_fetch_count(&agent), 0, "no refetch kick");
    }

    /// BugBot sibling hole: the OPEN-dropdown accept (Tab/Enter/mouse all
    /// share the helper) used to consume the candidates and close before
    /// the write path declined the chip-clipping splice — leaving nothing.
    /// The probe now makes it an honest no-op: nothing consumed, dropdown
    /// up, chip/draft/generation untouched, no kick — and Enter must not
    /// fall through to send.
    #[test]
    fn dropdown_accept_clipping_paste_chip_keeps_candidates() {
        let (mut agent, text) = chip_agent(vec![
            file_item("lima_one.txt", "lima_one.txt", 0..2),
            file_item("lima_two.txt", "lima_two.txt", 0..2),
        ]);
        agent.prompt.suggestions.dropdown.open = true;
        let gen_before = agent.prompt.suggestions.generation();

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Enter));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), text, "chip must survive");
        assert!(
            agent.prompt.completion_dropdown_open(),
            "candidates stay up"
        );
        assert_eq!(
            agent.prompt.suggestions.dropdown.items.len(),
            2,
            "nothing consumed"
        );
        assert_eq!(agent.prompt.suggestions.generation(), gen_before);
        assert_eq!(suggest_fetch_count(&agent), 0, "no refetch kick");

        // Tab rides the same helper.
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert_eq!(agent.prompt.suggestions.dropdown.items.len(), 2);
        assert_eq!(agent.prompt.text(), text);
        assert_eq!(suggest_fetch_count(&agent), 0);
    }

    /// The probe peeks the SELECTED item: with a chip-clipping row next to
    /// a plain-text row, acceptance follows the selection — no-op on the
    /// clipping one, normal accept after Down moves to the safe one.
    #[test]
    fn dropdown_accept_respects_selection_over_mixed_clip_ranges() {
        let (mut agent, _) = chip_agent(vec![]);
        agent.prompt.textarea.insert_str(" li");
        let text = agent.prompt.text().to_owned();
        agent.prompt.suggestions.dropdown.request_text = text.clone();
        agent.prompt.suggestions.dropdown.request_cursor = agent.prompt.cursor();
        let tok = text.len() - 2;
        agent.prompt.suggestions.dropdown.items = vec![
            file_item("lima_one.txt", "lima_one.txt", 0..2),
            file_item("lima_two.txt", "lima_two.txt", tok..text.len()),
        ];
        agent.prompt.suggestions.dropdown.open = true;

        // Selected = the chip-clipping row: honest no-op.
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Enter));
        assert_eq!(agent.prompt.suggestions.dropdown.items.len(), 2);
        assert_eq!(agent.prompt.text(), text);

        // Down selects the plain-text row: accepts normally.
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Down));
        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.prompt.text().ends_with(" lima_two.txt"),
            "safe selection must splice: {}",
            agent.prompt.text()
        );
        assert!(!agent.prompt.completion_dropdown_open());
    }

    /// Accepting a directory completion (trailing `/`) must re-fetch so the
    /// NEXT Tab completes inside it — drill-down chaining.
    #[test]
    fn dir_accept_kicks_refetch_for_drill_down() {
        let mut agent = bash_agent("cat no");
        agent.prompt.suggestions.dropdown.open = true;
        agent.prompt.suggestions.dropdown.items =
            vec![file_item("cat Notes\\ Archive/", "Notes\\ Archive/", 4..6)];

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "cat Notes\\ Archive/");
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::DebounceSuggestions { .. })),
            "dir accept must kick a fresh fetch for the drill-down"
        );
    }

    // -- Bash-mode gating of the as-you-type pipeline ------------------------

    /// Typing in the normal (chat) prompt never fires the suggest pipeline;
    /// the same keystroke in bash mode debounces a request.
    #[test]
    fn pipeline_fires_only_in_bash_mode() {
        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.suggestions.enabled = true;

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Char('g')));
        assert!(
            !agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::DebounceSuggestions { .. })),
            "normal-mode typing must not reach the suggest pipeline"
        );

        let mut agent = super::test_fixtures::make_agent();
        agent.prompt.suggestions.enabled = true;
        agent.prompt_input_mode = PromptInputMode::Bash;

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Char('g')));
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::DebounceSuggestions { .. })),
            "bash-mode typing debounces a suggest request"
        );
    }

    /// Esc closes a dropdown the Tab-armed landing opened (the always-on
    /// dismissal path), and the draft survives.
    #[test]
    fn esc_closes_tab_fetched_dropdown() {
        let mut agent = bash_agent_always_on("git st");
        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        let generation = agent.prompt.suggestions.generation();
        agent.prompt.suggestions.on_suggestions_loaded(
            crate::views::suggestion_controller::SuggestResponseParsed {
                ghost: None,
                completions: vec![
                    history_item("git status --porcelain-A", 0..6),
                    history_item("git status --porcelain-B", 0..6),
                ],
                generation,
            },
            "git st",
            "git st".len(),
        );
        assert!(agent.prompt.suggestions.take_pending_tab(generation));
        agent.shell_completion_tab();
        assert!(agent.prompt.completion_dropdown_open());

        let outcome = agent.handle_prompt_key_for_test(&key(KeyCode::Esc));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.prompt.completion_dropdown_open());
        assert_eq!(agent.prompt.text(), "git st");
        assert_eq!(agent.prompt_input_mode, PromptInputMode::Bash);
    }

    /// With the pipeline OFF, typing invalidates Tab-fetched state instead:
    /// the landing response for the pre-edit text is stale.
    #[test]
    fn typing_invalidates_tab_state_always_on() {
        let mut agent = bash_agent_always_on("cat no");
        agent.prompt.suggestions.dropdown.items = vec![file_item("cat notes.md", "notes.md", 4..6)];
        let gen_before = agent.prompt.suggestions.generation();

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Char('x')));
        assert!(
            agent.prompt.suggestions.generation() > gen_before,
            "the edit must invalidate Tab-fetched state"
        );
        assert!(agent.prompt.suggestions.dropdown.items.is_empty());
        assert!(
            !agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::DebounceSuggestions { .. })),
            "no as-you-type fetch without the env flag"
        );
    }

    /// THE stale-anchor regression: a mouse click repositions the cursor
    /// with no text change, so it must invalidate cached completion items
    /// exactly like a typed edit — the next Tab fetches for the token under
    /// the clicked cursor instead of completing the old one.
    #[test]
    fn prompt_click_invalidates_cached_items_before_tab() {
        use crate::app::agent_view::AgentPane;
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut agent = bash_agent_always_on("cat no");
        agent.prompt.suggestions.dropdown.items = vec![file_item("cat notes.md", "notes.md", 4..6)];
        agent.pane_areas.prompt = ratatui::layout::Rect::new(0, 40, 80, 5);
        // Already focused: an unfocused-collapse click only refocuses and
        // never reaches the textarea (the exact bug needs a focused click).
        agent.active_pane = AgentPane::Prompt;
        let gen_before = agent.prompt.suggestions.generation();

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 41,
            modifiers: KeyModifiers::NONE,
        };
        let _ = agent.handle_mouse(&click);
        assert!(
            agent.prompt.suggestions.generation() > gen_before,
            "a prompt click must invalidate cached completion state"
        );
        assert!(agent.prompt.suggestions.dropdown.items.is_empty());

        let _ = agent.handle_prompt_key_for_test(&key(KeyCode::Tab));
        assert_eq!(agent.prompt.text(), "cat no", "old token must not complete");
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::FetchShellSuggestions { .. })),
            "Tab must refetch for the clicked position"
        );
    }
}
