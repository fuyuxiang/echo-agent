//! Shell command suggestion controller.
//!
//! Manages ghost text state, progressive matching, and ACP integration
//! for shell command suggestions. Ghost text is rendered as dimmed italic
//! text after the cursor. Progressive matching trims the ghost when the
//! user types a character that matches the ghost's prefix, avoiding
//! unnecessary network requests.
//!
//! ACP integration: on text change (after debounce), sends an
//! `echo.agent/suggest` request through the Effect pipeline. Stale responses
//! are discarded via generation tracking.

/// Source of a shell command suggestion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SuggestionSource {
    #[default]
    None,
    History,
    PathExecutable,
    FilePath,
    AI,
}

impl SuggestionSource {
    fn parse_source(s: &str) -> Self {
        match s {
            "history" => Self::History,
            "path" => Self::PathExecutable,
            "file" => Self::FilePath,
            "ai" => Self::AI,
            _ => Self::None,
        }
    }
}

/// Ghost text state for shell command suggestions.
#[derive(Debug, Clone, Default)]
pub(crate) struct GhostTextState {
    /// Current ghost text to render (may be trimmed by progressive matching).
    pub(crate) text: String,
    /// Where this suggestion came from.
    pub(crate) source: SuggestionSource,
    /// Original full suggestion text before progressive matching.
    pub(crate) full_text: String,
    /// Generation counter when this ghost was set.
    pub(crate) generation: u64,
}

/// Parsed ghost suggestion from an ACP `echo.agent/suggest` response.
#[derive(Debug, Clone)]
pub struct GhostSuggestionParsed {
    pub suffix: String,
    pub source: SuggestionSource,
}

/// A single completion item from an ACP `echo.agent/suggest` response.
// `Default` (empty item) exists for downstream test fixtures — functional-
// update construction (`..Default::default()`) keeps out-of-crate literals
// (e.g. echo-agent-pager-minimal's) compiling when optional fields are added.
#[derive(Debug, Clone, Default)]
pub struct CompletionItemParsed {
    pub display: String,
    pub description: String,
    /// Whole-line replacement — always safe to `set_text` (the shell keeps
    /// this backward-shaped for range-unaware pagers).
    pub insert_text: String,
    pub source: SuggestionSource,
    pub priority: i32,
    /// Byte range in the REQUEST text the completion targets. `None` (older
    /// shells, whole-line items, or malformed wire data) keeps the
    /// whole-line accept behavior. Parsed atomically with `token_text`:
    /// present only as a pair.
    pub replace_range: Option<std::ops::Range<usize>>,
    /// Replacement for `replace_range` (path/file token completions);
    /// `Some` exactly when `replace_range` is.
    pub token_text: Option<String>,
    /// The provider capped its scan/results — the set may be incomplete, so
    /// Tab must not conclude from it (dropdown-only). Absent on the wire
    /// (older shells) parses as `false`.
    pub truncated: bool,
}

impl CompletionItemParsed {
    /// The text that replaces `replace_range` on an in-place accept.
    pub fn span_replacement(&self) -> &str {
        self.token_text.as_deref().unwrap_or(&self.insert_text)
    }
}

/// Parsed response from an ACP `echo.agent/suggest` request.
#[derive(Debug, Clone)]
pub struct SuggestResponseParsed {
    pub ghost: Option<GhostSuggestionParsed>,
    pub completions: Vec<CompletionItemParsed>,
    pub generation: u64,
}

impl SuggestResponseParsed {
    /// Parse a raw JSON value from an ACP `echo.agent/suggest` response.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let result = value.get("result").unwrap_or(value);
        let generation = result.get("generation")?.as_u64()?;
        let ghost = result.get("ghost").and_then(|g| {
            if g.is_null() {
                return None;
            }
            let suffix = g.get("suffix")?.as_str()?;
            if suffix.is_empty() {
                return None;
            }
            let source_str = g.get("source").and_then(|s| s.as_str()).unwrap_or("");
            Some(GhostSuggestionParsed {
                suffix: suffix.to_owned(),
                source: SuggestionSource::parse_source(source_str),
            })
        });
        let completions = result
            .get("completions")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let display = item.get("display")?.as_str()?.to_owned();
                        let insert_text = item.get("insertText")?.as_str()?.to_owned();
                        let description = item
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_owned();
                        let source_str = item.get("source").and_then(|s| s.as_str()).unwrap_or("");
                        let priority =
                            item.get("priority").and_then(|p| p.as_i64()).unwrap_or(0) as i32;
                        // Optional `[start, end]`; anything malformed
                        // degrades to the legacy whole-line accept.
                        let replace_range = item.get("replaceRange").and_then(|r| {
                            let arr = r.as_array()?;
                            let (start, end) = match arr.as_slice() {
                                [s, e] => (s.as_u64()? as usize, e.as_u64()? as usize),
                                _ => return None,
                            };
                            (start <= end).then_some(start..end)
                        });
                        let token_text = item
                            .get("tokenText")
                            .and_then(|t| t.as_str())
                            .map(str::to_owned);
                        // The pair is atomic: a range without its token
                        // would splice the whole-line `insertText` into a
                        // token span (`cat no` → `cat cat notes.md`), a
                        // token without its range has nowhere to go — half
                        // pairs degrade to the rangeless whole-line accept.
                        let (replace_range, token_text) = match (replace_range, token_text) {
                            (Some(r), Some(t)) => (Some(r), Some(t)),
                            _ => (None, None),
                        };
                        let truncated = item
                            .get("truncated")
                            .and_then(|t| t.as_bool())
                            .unwrap_or(false);
                        Some(CompletionItemParsed {
                            display,
                            description,
                            insert_text,
                            source: SuggestionSource::parse_source(source_str),
                            priority,
                            replace_range,
                            token_text,
                            truncated,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            ghost,
            completions,
            generation,
        })
    }
}

/// How much of the ghost text to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptMode {
    /// Accept the entire ghost text (Right arrow).
    Full,
    /// Accept one word of ghost text (Ctrl+Right).
    OneWord,
}

/// Action requested by `text_changed` for the caller to dispatch.
#[derive(Debug, PartialEq, Eq)]
pub enum SuggestionAction {
    /// Text progressively matched the ghost — no network request needed.
    Matched,
    /// Spawn a debounce timer. On expiry, call `on_debounce_expired`.
    Debounce { generation: u64 },
}

/// Wire `limit` for `echo.agent/suggest` fetches. Matches the shell file
/// provider's ranked-result cap (`MAX_RESULTS` in the shell crate's
/// `file_provider.rs`): the provider ranks BEFORE capping, the dropdown
/// renders 6 rows and scrolls the rest. Both fetch sites (Tab and the
/// as-you-type debounce) must send the same value or their candidate sets
/// diverge.
pub const SHELL_SUGGEST_WIRE_LIMIT: usize = 50;

/// Terminal-Tab decision over the current dropdown items — computed by
/// [`SuggestionController::tab_decision`], executed by the view. Owning the
/// whole policy here (staleness, source shape, single-candidate, LCP) keeps
/// the view from reading item internals.
#[derive(Debug, PartialEq, Eq)]
pub enum TabAction {
    /// Exactly one token candidate: accept it immediately, no dropdown flash.
    InstaAccept,
    /// Write the shared prefix over the validated span (bash's first Tab),
    /// then re-fetch for the longer token.
    Fill(std::ops::Range<usize>, String),
    /// Ambiguous (or whole-line/mixed sources): open the dropdown.
    Open,
    /// No usable candidates: none fetched, or outdated by an edit / cursor
    /// move. The key path fetches on this; the landing path does nothing.
    Nothing,
}

/// A resolved dropdown accept: what to write, decided against the draft
/// BEFORE the dropdown closed (so nothing depends on state surviving
/// `close()`). Produced by [`SuggestionController::accept_completion`],
/// applied by `PromptWidget::apply_completion_splice`.
#[derive(Debug, PartialEq, Eq)]
pub enum CompletionSplice {
    /// Rangeless (legacy-shell) item: replace the whole line — safe because
    /// wire `insert_text` is always a full line by protocol contract.
    WholeLine(String),
    /// Token item with a still-valid span: replace that range in place.
    Token(std::ops::Range<usize>, String),
    /// The item's span no longer fits the draft: accept is a
    /// draft-preserving no-op — never a clobber.
    Stale,
}

/// State for the shell command completion dropdown.
#[derive(Debug, Default)]
pub struct CompletionDropdownState {
    pub open: bool,
    pub items: Vec<CompletionItemParsed>,
    pub selected: usize,
    pub hovered: Option<usize>,
    pub generation: u64,
    /// The request text `items` were computed for — set atomically with the
    /// items when a response lands, so item `replace_range` offsets always
    /// validate against the text they actually index into.
    pub request_text: String,
    /// Cursor position the request was built at. Items target the token AT
    /// this cursor; [`SuggestionController::tab_decision`] refuses items
    /// when the live cursor has moved anywhere else (e.g. a mouse click) —
    /// the only tolerated drift is typing at the end.
    pub request_cursor: usize,
}

impl CompletionDropdownState {
    /// Move the selection by `delta` (negative = up, positive = down),
    /// wrapping around at the ends. Used for keyboard arrow nav.
    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let new = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.selected = new;
    }

    /// Move the selection by `delta`, clamping at the first and last item
    /// (no wrap-around). Used for mouse-wheel scrolling.
    pub fn scroll_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let new = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        self.selected = new;
    }

    /// Accept the currently selected item, or `None` when there are no
    /// items. Moves the item out to avoid cloning and closes the dropdown.
    /// Deliberately independent of [`open`](Self::open) (a render flag):
    /// the single-candidate insta-accept consumes an item that was never
    /// rendered.
    pub fn accept(&mut self) -> Option<CompletionItemParsed> {
        if self.items.is_empty() {
            return None;
        }
        let idx = self.selected.min(self.items.len() - 1);
        let item = self.items.swap_remove(idx);
        self.close();
        Some(item)
    }

    // The `request_text`/`request_cursor` anchor is left in place (inert
    // without items); a landing overwrites it atomically with the items.
    pub fn close(&mut self) {
        self.open = false;
        self.selected = 0;
        self.hovered = None;
        self.items.clear();
    }
}

/// Manages ghost text state, progressive matching, and ACP integration.
#[derive(Default)]
pub struct SuggestionController {
    ghost: GhostTextState,
    generation: u64,
    last_request_text: String,
    /// Generation of a Tab-triggered fetch whose landing should run the
    /// terminal Tab semantics (armed by [`Self::begin_tab_completion`],
    /// consumed by [`Self::take_pending_tab`]).
    tab_pending: Option<u64>,
    /// Whether the as-you-type suggestion pipeline (debounced fetches +
    /// ghost rendering) is enabled. Resolved at construction from the
    /// `ECHO_AGENT_SUGGESTIONS` env var. Tab-triggered completion in bash mode
    /// deliberately does NOT consult this — it is always on.
    pub enabled: bool,
    /// Completion dropdown state (populated from `SuggestResponse.completions`).
    pub dropdown: CompletionDropdownState,
    /// Whether AI-powered suggestions are enabled.
    /// Resolved at construction from `ECHO_AGENT_SUGGESTIONS_AI` env var.
    pub ai_enabled: bool,
    /// Model to use for AI suggestions. Sent in the `echo.agent/suggest` request.
    /// Resolved at construction from `ECHO_AGENT_SUGGESTIONS_AI_MODEL` env var.
    pub ai_model: Option<String>,
}

impl SuggestionController {
    pub fn new() -> Self {
        Self {
            ghost: GhostTextState::default(),
            generation: 0,
            last_request_text: String::new(),
            tab_pending: None,
            enabled: echo_agent_config::env_bool("ECHO_AGENT_SUGGESTIONS").unwrap_or(false),
            dropdown: CompletionDropdownState::default(),
            ai_enabled: echo_agent_config::env_bool("ECHO_AGENT_SUGGESTIONS_AI").unwrap_or(false),
            ai_model: std::env::var("ECHO_AGENT_SUGGESTIONS_AI_MODEL")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }

    /// Set ghost text with a source. Moves `text` into the controller.
    pub fn set_ghost(&mut self, text: String, source: SuggestionSource) {
        self.generation += 1;
        self.set_ghost_fields(text, source);
    }

    /// Write ghost fields without touching the generation counter.
    fn set_ghost_fields(&mut self, text: String, source: SuggestionSource) {
        self.ghost.full_text.clear();
        self.ghost.full_text.push_str(&text);
        self.ghost.text = text;
        self.ghost.source = source;
        self.ghost.generation = self.generation;
    }

    pub fn clear_ghost(&mut self) {
        self.ghost.text.clear();
        self.ghost.full_text.clear();
        self.ghost.source = SuggestionSource::None;
        self.dropdown.close();
    }

    /// Wholesale suggestion-state discard (prompt emptied, `set_text` swap):
    /// the ghost and the dropdown items belonged to the OLD draft, and any
    /// in-flight fetch was for it, so clear both, disarm a pending Tab, and
    /// bump the generation so late responses are discarded instead of
    /// resurrecting stale state.
    pub fn invalidate_draft(&mut self) {
        self.clear_ghost();
        self.last_request_text.clear();
        self.tab_pending = None;
        self.generation += 1;
    }

    pub fn has_ghost(&self) -> bool {
        !self.ghost.text.is_empty()
    }

    /// Returns the current ghost text if non-empty.
    pub fn ghost_text(&self) -> Option<&str> {
        if self.ghost.text.is_empty() {
            None
        } else {
            Some(&self.ghost.text)
        }
    }

    /// Accept ghost text. Returns the accepted portion, or `None` if empty.
    /// Closes the completion dropdown (accepted ghost text supersedes it)
    /// and bumps the generation so in-flight responses for the pre-accept
    /// text are discarded when they land.
    pub fn accept_ghost(&mut self, mode: AcceptMode) -> Option<String> {
        if self.ghost.text.is_empty() {
            return None;
        }

        self.dropdown.close();

        match mode {
            AcceptMode::Full => {
                let accepted = std::mem::take(&mut self.ghost.text);
                self.ghost.full_text.clear();
                self.ghost.source = SuggestionSource::None;
                self.generation += 1;
                Some(accepted)
            }
            AcceptMode::OneWord => {
                let accept_end = one_word_end(&self.ghost.text);
                if accept_end == 0 {
                    return None;
                }
                let accepted = self.ghost.text[..accept_end].to_owned();
                self.ghost.text.drain(..accept_end);
                if self.ghost.text.is_empty() {
                    self.ghost.full_text.clear();
                    self.ghost.source = SuggestionSource::None;
                }
                self.generation += 1;
                Some(accepted)
            }
        }
    }

    /// Resolve what accepting the selected item WOULD write, without
    /// consuming it or touching any state. The view probes this before an
    /// insta-accept: a splice into an atomic prompt element must degrade to
    /// opening the dropdown, not consume the candidate.
    /// [`Self::accept_completion`] delegates here so the two can never
    /// resolve differently.
    pub fn peek_completion_splice(&self, current_text: &str) -> Option<CompletionSplice> {
        if self.dropdown.generation != self.generation || self.dropdown.items.is_empty() {
            return None;
        }
        let idx = self.dropdown.selected.min(self.dropdown.items.len() - 1);
        let item = &self.dropdown.items[idx];
        Some(match item.replace_range.clone() {
            None => CompletionSplice::WholeLine(item.insert_text.clone()),
            Some(range) => {
                match self.validated_replace_range(range, item.span_replacement(), current_text) {
                    Some(range) => {
                        CompletionSplice::Token(range, item.span_replacement().to_owned())
                    }
                    None => CompletionSplice::Stale,
                }
            }
        })
    }

    /// Accept the selected completion-dropdown item, refusing stale state:
    /// items populated for a superseded generation just close the dropdown
    /// and accept nothing (the refreshed fetch is already in flight). The
    /// item's span is resolved against `current_text` BEFORE the dropdown
    /// closes (see [`CompletionSplice`]). A successful accept bumps the
    /// generation so in-flight responses for the pre-accept text are
    /// discarded when they land.
    pub fn accept_completion(&mut self, current_text: &str) -> Option<CompletionSplice> {
        if self.dropdown.generation != self.generation {
            self.dropdown.close();
            return None;
        }
        let resolved = self.peek_completion_splice(current_text)?;
        self.dropdown.accept()?;
        self.generation += 1;
        Some(resolved)
    }

    /// Try progressive matching: if `new_text` extends `last_request_text`
    /// by exactly one character that matches the ghost's first character,
    /// trim the ghost and return `true`. Otherwise clear the ghost and
    /// return `false`.
    pub fn try_progressive_match(&mut self, new_text: &str) -> bool {
        if self.ghost.text.is_empty() {
            return false;
        }

        let suffix = match new_text.strip_prefix(self.last_request_text.as_str()) {
            Some(s) => s,
            None => {
                self.clear_ghost();
                return false;
            }
        };

        let mut chars = suffix.chars();
        let typed_char = match (chars.next(), chars.next()) {
            (Some(c), None) => c,
            _ => {
                self.clear_ghost();
                return false;
            }
        };

        if !self.ghost.text.starts_with(typed_char) {
            self.clear_ghost();
            return false;
        }

        self.ghost.text.drain(..typed_char.len_utf8());
        self.last_request_text.clear();
        self.last_request_text.push_str(new_text);

        if self.ghost.text.is_empty() {
            self.ghost.full_text.clear();
            self.ghost.source = SuggestionSource::None;
        }

        true
    }

    /// Update the progressive-match anchor: the text the on-screen ghost is
    /// relative to (reset to the CURRENT text whenever a response lands).
    /// Distinct from [`CompletionDropdownState::request_text`], which pins
    /// the fetch-time text the dropdown items' ranges index into.
    pub fn set_last_request_text(&mut self, text: &str) {
        self.last_request_text.clear();
        self.last_request_text.push_str(text);
    }

    /// The whole terminal-Tab policy over the current dropdown items:
    /// staleness (generation AND cursor consistency), source shape, the
    /// single-candidate rule, and the shared-prefix rule — one seam, so the
    /// view executes without reading item internals. Only complete token
    /// edits (path/file source AND a range+token pair AND an exhaustive
    /// scan) get shell semantics; everything else — whole-line, mixed, or
    /// degraded sets — always [`TabAction::Open`].
    pub fn tab_decision(&self, current_text: &str, current_cursor: usize) -> TabAction {
        if self.dropdown.generation != self.generation || self.dropdown.items.is_empty() {
            return TabAction::Nothing;
        }
        // Items target the token at the FETCH-time cursor. The only
        // tolerated drift is typing at the end (the same growth the range
        // stretch rule accepts); any other cursor move — a mouse click in
        // particular reports no text change — makes them stale, and Tab
        // must fetch for the token actually under the cursor.
        let grown = current_text
            .len()
            .saturating_sub(self.dropdown.request_text.len());
        if current_cursor != self.dropdown.request_cursor + grown {
            return TabAction::Nothing;
        }
        // Source alone is not enough: old shells send rangeless `path` rows
        // whose whole-line fallback would clobber the draft on insta-accept
        // (`ls | gr` → `grep`), and a truncated (capped) scan may hide the
        // row that disproves a sole match or an LCP.
        let token_shaped = self.dropdown.items.iter().all(|i| {
            matches!(
                i.source,
                SuggestionSource::FilePath | SuggestionSource::PathExecutable
            ) && i.replace_range.is_some()
                && i.token_text.is_some()
                && !i.truncated
        });
        if token_shaped {
            if self.dropdown.items.len() == 1 {
                return TabAction::InstaAccept;
            }
            if let Some((range, fill)) = self.common_prefix_fill(current_text) {
                return TabAction::Fill(range, fill);
            }
        }
        TabAction::Open
    }

    /// Shared-prefix fill for terminal-like Tab: when every dropdown item
    /// targets the SAME span and their replacements share a common prefix
    /// that strictly extends the typed token, return the validated span and
    /// the prefix to write (bash's first-Tab behavior). `None` on any
    /// ambiguity — stale generation, mixed or missing ranges, no shared
    /// prefix, or one that doesn't extend what's typed (e.g. candidates
    /// differing in case) — and [`Self::tab_decision`] falls back to
    /// opening the dropdown.
    fn common_prefix_fill(&self, current_text: &str) -> Option<(std::ops::Range<usize>, String)> {
        if self.dropdown.generation != self.generation {
            return None;
        }
        let items = &self.dropdown.items;
        if items.len() < 2 {
            return None;
        }
        let range = items[0].replace_range.clone()?;
        if items[1..]
            .iter()
            .any(|i| i.replace_range.as_ref() != Some(&range))
        {
            return None;
        }
        let mut lcp = items[0].span_replacement();
        for item in &items[1..] {
            lcp = common_str_prefix(lcp, item.span_replacement());
            if lcp.is_empty() {
                return None;
            }
        }
        // Token texts are rendered shell literals (`a b`/`a$c` arrive as
        // `a\ b`/`a\$c`), so their byte LCP can end mid-escape (`a\`) —
        // filling that would write a dangling backslash (line
        // continuation). Trim the incomplete escape; the strict-extension
        // check below then decides whether anything is left to fill.
        if lcp.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1 {
            lcp = &lcp[..lcp.len() - 1];
        }
        let range = self.validated_replace_range(range, lcp, current_text)?;
        let typed = &current_text[range.clone()];
        (lcp.len() > typed.len() && lcp.starts_with(typed)).then(|| (range, lcp.to_owned()))
    }

    /// Re-validate a completion item's `replace_range` against the current
    /// text. Offsets index into [`CompletionDropdownState::request_text`];
    /// the only drift a live dropdown survives is progressive typing, so a
    /// range that reached the request text's end absorbs the typed tail —
    /// but ONLY while the grown span still extends toward `replacement` (a
    /// prefix of it). Anything else returns `None`: no-op, never a clobber.
    fn validated_replace_range(
        &self,
        range: std::ops::Range<usize>,
        replacement: &str,
        current_text: &str,
    ) -> Option<std::ops::Range<usize>> {
        let request = self.dropdown.request_text.as_str();
        if range.start > range.end || range.end > request.len() {
            return None;
        }
        if !current_text.starts_with(request) {
            return None;
        }
        let mut end = range.end;
        if range.end == request.len() && current_text.len() > request.len() {
            if !current_text.is_char_boundary(range.start)
                || !replacement.starts_with(&current_text[range.start..])
            {
                return None;
            }
            end = current_text.len();
        }
        (current_text.is_char_boundary(range.start) && current_text.is_char_boundary(end))
            .then_some(range.start..end)
    }

    /// Arm a Tab-triggered deterministic completion fetch. Deliberately
    /// independent of [`enabled`](Self::enabled): Tab in bash mode always
    /// completes. Bumps the generation (discarding any in-flight response)
    /// and returns it for the fetch effect; `run_tab_on_load` marks the
    /// landing response to run the terminal Tab semantics once (the
    /// post-accept/fill refreshes pass `false` so their items land
    /// silently and wait for the next Tab).
    pub fn begin_tab_completion(&mut self, run_tab_on_load: bool) -> u64 {
        self.generation += 1;
        self.tab_pending = run_tab_on_load.then_some(self.generation);
        self.generation
    }

    /// A Tab-armed fetch is still in flight for the current draft (nothing
    /// invalidated it since arming): a repeat Tab keeps the marker and lets
    /// that landing run the Tab semantics once — no second RPC.
    pub fn tab_fetch_pending(&self) -> bool {
        self.tab_pending == Some(self.generation)
    }

    /// Consume the pending-Tab mark when the response for `generation`
    /// lands. `true` only when this is the fetch Tab armed AND it is still
    /// current (an edit since the Tab makes it stale).
    pub fn take_pending_tab(&mut self, generation: u64) -> bool {
        if self.tab_pending == Some(generation) {
            self.tab_pending = None;
            return generation == self.generation;
        }
        false
    }

    /// Current generation counter (for callers that need to pass it to effects).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Called on each text change. Returns the action the caller should take.
    ///
    /// If slash is active (or has an inline ghost), suppresses the pipeline
    /// entirely. Otherwise, tries progressive matching first; if no match,
    /// increments generation and requests a debounce.
    pub fn text_changed(
        &mut self,
        text: &str,
        slash_active: bool,
        slash_has_inline_ghost: bool,
    ) -> Option<SuggestionAction> {
        if !self.enabled {
            // No as-you-type pipeline, but Tab-fetched completion state is
            // text-specific: the edit outdates the dropdown items and any
            // in-flight Tab fetch (the generation bump makes them stale).
            self.invalidate_draft();
            return None;
        }

        if slash_active || slash_has_inline_ghost {
            // Suppression must invalidate, not just hide: an already-armed
            // debounce or pending Tab landing would otherwise repopulate
            // suggestion state behind the slash UI.
            self.invalidate_draft();
            return None;
        }

        if text.is_empty() {
            self.invalidate_draft();
            return None;
        }

        if self.try_progressive_match(text) {
            return Some(SuggestionAction::Matched);
        }

        // Non-matching edit: `try_progressive_match` clears ghost+dropdown
        // on a ghost mismatch, but a ghost-less dropdown (pure path/file
        // items) would leak through its empty-ghost early return — this
        // edit outdated those items, so tear the dropdown down here.
        self.dropdown.close();
        self.generation += 1;
        Some(SuggestionAction::Debounce {
            generation: self.generation,
        })
    }

    /// Called when a debounce timer expires. If the generation still matches,
    /// returns `true` and the caller should fire the ACP request.
    pub fn on_debounce_expired(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// Called when an ACP `echo.agent/suggest` response arrives, with the text and
    /// cursor the request was built from (the anchor item `replace_range`
    /// offsets index into, and the position Tab targets). Takes ownership to
    /// avoid copying strings. Discards stale responses.
    pub fn on_suggestions_loaded(
        &mut self,
        response: SuggestResponseParsed,
        request_text: &str,
        request_cursor: usize,
    ) {
        if response.generation != self.generation {
            return;
        }

        match response.ghost {
            // The ghost is the env-gated as-you-type surface: Tab-triggered
            // (always-on) fetches feed only the dropdown items.
            Some(ghost) if self.enabled => self.set_ghost_fields(ghost.suffix, ghost.source),
            _ => self.clear_ghost(),
        }

        self.dropdown.items = response.completions;
        self.dropdown.generation = response.generation;
        self.dropdown.selected = 0;
        self.dropdown.request_text.clear();
        self.dropdown.request_text.push_str(request_text);
        self.dropdown.request_cursor = request_cursor;
        // Don't auto-open; Tab opens it whenever items exist.
    }
}

/// Longest common prefix of two strings, trimmed to a char boundary.
fn common_str_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let mut n = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    while n > 0 && !a.is_char_boundary(n) {
        n -= 1;
    }
    &a[..n]
}

/// Find the byte offset after the first word in `s`. A "word" is optional
/// leading whitespace followed by a run of non-whitespace characters.
fn one_word_end(s: &str) -> usize {
    let leading_ws = s.len() - s.trim_start().len();
    let after_ws = &s[leading_ws..];
    let word_len = after_ws.find(char::is_whitespace).unwrap_or(after_ws.len());
    leading_ws + word_len
}

#[cfg(test)]
mod tests;
