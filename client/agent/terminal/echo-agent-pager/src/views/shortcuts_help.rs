//! All-shortcuts cheatsheet modal (Ctrl+. / Ctrl+X).
//!
//! Registry-driven: `build_entries(registry)` pulls every `ActionDef` from
//! `ActionRegistry`, groups them by `Category` in onboarding-friendly order
//! (Essentials → Panes → Scrollback Navigation → View → Prompt → Agent),
//! and includes alt-key bindings inline. Search filters against key display,
//! description, and label.
//!
//! Two ways to read a binding's help: pattern A expands an inline help line under
//! the selected hint (e/Space/l/h/arrows); pattern B opens an in-modal man-style
//! detail page on Enter, where Esc (or h/Left/Backspace) returns to the browse list.
//! Section headers collapse/expand; close via Esc in browse or Ctrl+./Ctrl+X.
//! Rendered via `ModalWindow` chrome (same appearance as the command palette).
//!
//! Entry points from `AgentView`:
//! - `build_entries(registry)` + `build_initial_picker_state` →
//!   `ActiveModal::ShortcutsHelp`
//! - `handle_input` / `handle_mouse` for key/mouse dispatch
//! - Rendering is done inline in `AgentView` via `render_modal_window` +
//!   `render_picker_in_modal`.

use std::borrow::Cow;

use crate::actions::{ActionDef, ActionId, ActionRegistry, Category, When};
use crate::input::key::KeyShortcut;
use crate::views::picker::{PickerConfig, PickerOutcome, PickerState, handle_picker_input};
use crate::views::shortcuts_bar::HintItem;

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// Key for pattern-A inline expand state (`expanded_ids`).
///
/// Registry rows use [`ExpandKey::Action`]; display-only rows that ship
/// `long_help` (e.g. paste) use [`ExpandKey::Pseudo`] with a stable label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpandKey {
    Action(ActionId),
    Pseudo(&'static str),
}

/// One row in the all-shortcuts cheatsheet.
///
/// Headers are non-selectable section dividers; Hints are the actual key
/// bindings and are selectable / dispatchable on Enter.
pub enum ShortcutsHelpEntry {
    SectionHeader {
        label: &'static str,
        category_idx: usize,
        entry_count: usize,
    },
    Hint {
        item: HintItem,
        dimmed: bool,
        /// Registry action for expand/detail; `None` for display-only pseudo-rows.
        action_id: Option<ActionId>,
        /// Man-style help shown under the expanded row; falls back to the description.
        long_help: Option<&'static str>,
    },
}

impl ShortcutsHelpEntry {
    pub fn is_hint(&self) -> bool {
        matches!(self, Self::Hint { .. })
    }

    pub fn is_section_header(&self) -> bool {
        matches!(self, Self::SectionHeader { .. })
    }
}

// ---------------------------------------------------------------------------
// Modal state construction
// ---------------------------------------------------------------------------

/// Category display order and labels for the cheatsheet.
const CATEGORY_ORDER: &[(Category, &str)] = &[
    (Category::GettingStarted, "Essentials"),
    (Category::Input, "Input"),
    (Category::ConversationNav, "Conversation Navigation"),
    (Category::ConversationAction, "Conversation Actions"),
    (Category::Panels, "Panels"),
    (Category::Session, "Session"),
    (Category::Dashboard, "Dashboard"),
];

pub fn default_collapsed() -> std::collections::HashSet<usize> {
    (1..CATEGORY_ORDER.len()).collect()
}

// Man-page body for the paste pseudo-row (Enter detail). Keep claims that
// hold on every host (agent + dashboard); non-image file paths are agent-only.
#[cfg(target_os = "windows")]
const PASTE_LONG_HELP: &str = "\
Pastes clipboard images into the prompt as chips, and plain text as typed.\n\
Prefer Ctrl+V. Use Alt+V as a fallback when Ctrl+V fails (some terminals or \
configs drop image clipboards; older Windows Terminal versions only pasted \
text).\n\
You can also drag an image file from Explorer into the prompt.";
#[cfg(target_os = "macos")]
const PASTE_LONG_HELP: &str = "\
Pastes clipboard images into the prompt as chips, and plain text as typed.\n\
Use Ctrl+V for screenshots, browser \"Copy Image\", and file-manager image \
copies (many terminals swallow Cmd+V and never deliver it to the TUI).\n\
You can also drag an image file into the prompt.";
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
const PASTE_LONG_HELP: &str = "\
Pastes clipboard images into the prompt as chips, and plain text as typed.\n\
Use Ctrl+V for screenshots, browser \"Copy Image\", and file-manager image \
copies.\n\
You can also drag an image file into the prompt.";

// Undo/redo are textarea chords, not ActionRegistry entries. Super/Cmd also
// works where the terminal delivers it; list Ctrl only (hosts often swallow Super).
const UNDO_LONG_HELP: &str = "\
Undoes the last change in the prompt editor.\n\
Covers typing, deletes, line/word kills, and clearing a draft.";

const REDO_LONG_HELP: &str = "\
Redoes the last undone change in the prompt editor.\n\
Ctrl+Shift+Z is primary; Ctrl+R is an alternate.";

// Prompt history is not an ActionRegistry entry: Up is an inline key handler and
// /history is a slash command. Surface both here for discoverability.
const HISTORY_LONG_HELP: &str = "\
Recalls previously sent prompts.\n\
Press Up on an empty prompt to browse earlier prompts, newest first; each move \
live-populates the composer so you can edit and resend.\n\
With prompts queued, Up moves focus into the queue pane on the last row instead.\n\
Run /history to open a searchable history panel and filter by text.";

// Scrollback search has no ActionRegistry entry: it's the vim `/` inline handler,
// or the /find slash command in simple mode. Surface both triggers here.
const SCROLLBACK_SEARCH_LONG_HELP: &str = "\
Searches the conversation scrollback for text and jumps between matches.\n\
In the prompt input, run /find to search. In vim mode, you can also press / \
while the scrollback is focused.\n\
Type a query, then use n and N (or the arrow keys) to step through matches. \
Press Enter to jump to a match and Esc to dismiss.";

/// Build the entries vector for the modal, grouped by category.
///
/// All registered actions are included, grouped by category. Actions
/// whose `When` context is not in `active_contexts` are dimmed.
pub fn build_entries(
    active_contexts: &[When],
    registry: &ActionRegistry,
    vim_mode: bool,
) -> Vec<ShortcutsHelpEntry> {
    let mut entries: Vec<ShortcutsHelpEntry> = Vec::new();

    // Keys the dashboard session-overlay claims while it is up. The
    // overlay intercept consults `When::DashboardOverlay` before
    // forwarding a key to the agent, so a lit row from another context
    // advertising one of these keys would be lying (e.g. the
    // cheatsheet's Ctrl+X alt is shadowed by the overlay stop).
    let overlay_claimed: std::collections::HashSet<KeyShortcut> =
        if active_contexts.contains(&When::DashboardOverlay) {
            registry
                .all()
                .iter()
                .filter(|d| d.context == When::DashboardOverlay)
                .map(|d| d.default_key)
                .collect()
        } else {
            std::collections::HashSet::new()
        };

    for (cat_idx, &(cat, label)) in CATEGORY_ORDER.iter().enumerate() {
        // Dedup per category on the default key, preferring the def
        // whose `When` context is active: `DashboardStop` (list) and
        // `DashboardOverlayStop` (overlay) share Ctrl+X and category,
        // and whichever matches the current surface must win
        // regardless of registration order.
        let mut seen_in_cat: std::collections::HashMap<KeyShortcut, usize> =
            std::collections::HashMap::new();
        let defs: Vec<&ActionDef> = registry
            .all()
            .iter()
            .filter(|d| d.category == cat)
            .collect();
        if defs.is_empty() {
            continue;
        }
        let header_idx = entries.len();
        entries.push(ShortcutsHelpEntry::SectionHeader {
            label,
            category_idx: cat_idx,
            entry_count: 0,
        });
        for def in defs {
            // Slash-only actions with no real keybinding (e.g. `/voice`'s
            // EnableVoiceMode) don't belong in a keyboard cheatsheet.
            if def.default_key == crate::key!(Null) && def.alt_keys.is_empty() {
                continue;
            }
            // The voice chord (`Ctrl+Space`) is hidden when the voice gate is
            // off (remote kill switch / `ECHO_AGENT_VOICE_MODE=0`) or the user turned
            // the Voice shortcut setting off — don't advertise keys that do
            // nothing. `Ctrl+Space` decodes the same with or without the Kitty
            // keyboard protocol (it just toggles instead of hold-to-talk), so
            // it's shown on every terminal once the gates are on.
            // EnableVoiceMode is slash-only and already dropped above.
            if def.id == crate::actions::ActionId::VoiceToggle
                && (!crate::app::voice_mode_enabled() || !crate::app::voice_keybind_enabled())
            {
                continue;
            }
            let mut item = def.hint();
            if !def.alt_keys.is_empty() {
                item.keys.extend_from_slice(&def.alt_keys);
                // Alt keys can be terminal-encoding variants of the SAME
                // physical chord (Shift+Tab arrives as `BackTab`,
                // `BackTab`+SHIFT, or `Tab`+SHIFT depending on the
                // terminal). Collapse keys that render identically so the
                // row doesn't read "Shift+Tab / Shift+Tab / Shift+Tab".
                let mut seen_displays = std::collections::HashSet::new();
                item.keys
                    .retain(|k| seen_displays.insert(k.display_pretty()));
                item.custom_display = None;
            }
            // In non-vim mode, suppress bare-letter / Shift+letter keys
            // from any scrollback-context binding. If the row has at least
            // one non-vim key left (e.g. an arrow alt), show only those —
            // they still work, so don't dim. If every key was a vim key,
            // hide the row entirely (the binding is genuinely inert when
            // vim mode is off).
            if !vim_mode && def.context == When::ScrollbackFocused {
                let has_non_vim = item.keys.iter().any(|k| !k.is_letter_or_shift_letter());
                if has_non_vim {
                    item.keys.retain(|k| !k.is_letter_or_shift_letter());
                    // When we strip the default_key but keep an alt, the
                    // custom_display string (e.g. "Shift+l/h") no longer
                    // matches what's shown; drop it so the keys render
                    // verbatim.
                    item.custom_display = None;
                } else {
                    continue;
                }
            }
            let dimmed = !active_contexts.contains(&def.context);
            // Strip overlay-claimed keys from lit rows of other
            // contexts (the overlay intercept shadows them). Dimmed
            // rows already say "not applicable here", so they keep
            // their keys for discoverability.
            if !dimmed
                && def.context != When::DashboardOverlay
                && item.keys.iter().any(|k| overlay_claimed.contains(k))
            {
                item.keys.retain(|k| !overlay_claimed.contains(k));
                if item.keys.is_empty() {
                    // Every key is shadowed — the binding is
                    // genuinely unreachable inside the overlay.
                    continue;
                }
                // The custom display no longer matches the surviving
                // keys; render them verbatim.
                item.custom_display = None;
            }
            // Identical row for both arms; `item` moves in, `dimmed`/`def.id` are Copy.
            let hint = ShortcutsHelpEntry::Hint {
                item,
                dimmed,
                action_id: Some(def.id),
                long_help: def.long_help,
            };
            match seen_in_cat.entry(def.default_key) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(entries.len());
                    entries.push(hint);
                }
                std::collections::hash_map::Entry::Occupied(slot) => {
                    // Same key already rendered in this category —
                    // replace it only when the earlier row is dimmed
                    // and this one is lit (active context wins).
                    let prior = &mut entries[*slot.get()];
                    if !dimmed && matches!(prior, ShortcutsHelpEntry::Hint { dimmed: true, .. }) {
                        *prior = hint;
                    }
                }
            }
        }
        // Scrollback search (`/`) has no registered ActionDef yet — vim-only,
        // handled inline; surface it here for discoverability.
        if vim_mode && cat == Category::ConversationNav {
            let mut item = HintItem::new(crate::key!('/'), "search");
            item.description = Some("Search scrollback".into());
            let dimmed = !active_contexts.contains(&When::ScrollbackFocused);
            entries.push(ShortcutsHelpEntry::Hint {
                item,
                dimmed,
                action_id: None,
                long_help: Some(SCROLLBACK_SEARCH_LONG_HELP),
            });
        }
        // Simple mode reaches scrollback search via the `/find` slash command,
        // not a keystroke: use a null key + custom display so the raw key list
        // stays empty of `/`.
        if !vim_mode && cat == Category::ConversationNav {
            let mut item = HintItem::new(crate::key!(Null), "search");
            item.custom_display = Some("/find");
            item.description = Some("Search scrollback".into());
            // `/find` is a slash command typed at the prompt (not a scrollback
            // keystroke like the vim `/` above), so it is available when the
            // prompt is focused — dim on `!PromptFocused`, not scrollback.
            let dimmed = !active_contexts.contains(&When::PromptFocused);
            entries.push(ShortcutsHelpEntry::Hint {
                item,
                dimmed,
                action_id: None,
                long_help: Some(SCROLLBACK_SEARCH_LONG_HELP),
            });
        }
        // Clipboard + textarea chords not in ActionRegistry. Super/Cmd omitted
        // (often swallowed). Lit on agent prompt and dashboard reply hosts.
        if cat == Category::Input {
            let dimmed = !active_contexts.contains(&When::PromptFocused)
                && !active_contexts.contains(&When::DashboardFocused);
            let push_pseudo = |entries: &mut Vec<ShortcutsHelpEntry>,
                               item: HintItem,
                               long_help: Option<&'static str>| {
                entries.push(ShortcutsHelpEntry::Hint {
                    item,
                    dimmed,
                    action_id: None,
                    long_help,
                });
            };

            let mut paste = HintItem::new(crate::key!('v', CONTROL), "paste");
            paste.description = Some("Paste images (and text) from the clipboard".into());
            #[cfg(target_os = "windows")]
            paste.keys.push(crate::key!('v', ALT));
            push_pseudo(&mut entries, paste, Some(PASTE_LONG_HELP));

            let mut undo = HintItem::new(crate::key!('z', CONTROL), "undo");
            undo.description = Some("Undo the last prompt edit".into());
            push_pseudo(&mut entries, undo, Some(UNDO_LONG_HELP));

            // Textarea: Ctrl+Shift+Z (+ Ctrl+R alt). Ctrl+R is prompt-only;
            // scrollback may bind it to mouse reporting when that toggle is on.
            let mut redo = HintItem::new(crate::key!('z', CONTROL | SHIFT), "redo");
            redo.description = Some("Redo the last undone prompt edit".into());
            redo.keys.push(crate::key!('r', CONTROL));
            push_pseudo(&mut entries, redo, Some(REDO_LONG_HELP));

            // Prompt history (Up / /history). Not part of the shared paste/undo/redo
            // `dimmed`: that also lights on DashboardFocused, but Up-history is
            // prompt-only, so give it its own PromptFocused-scoped dim.
            let mut history = HintItem::new(crate::key!(Up), "history");
            history.description = Some("Prompt history".into());
            let history_dimmed = !active_contexts.contains(&When::PromptFocused);
            entries.push(ShortcutsHelpEntry::Hint {
                item: history,
                dimmed: history_dimmed,
                action_id: None,
                long_help: Some(HISTORY_LONG_HELP),
            });
        }
        let count = entries.len() - header_idx - 1;
        if count == 0 {
            // Every action in this category got filtered out (e.g. all
            // scrollback vim-only bindings in non-vim mode); drop the
            // empty header rather than render a dead section.
            entries.pop();
        } else if let Some(ShortcutsHelpEntry::SectionHeader { entry_count, .. }) =
            entries.get_mut(header_idx)
        {
            *entry_count = count;
        }
    }
    entries
}

/// Build the initial `PickerState` for the modal. Width/height are wider
/// than the default Floating popup so the cheatsheet has room for the
/// key + label columns.
pub fn build_initial_picker_state(entries: &[ShortcutsHelpEntry]) -> PickerState {
    use crate::views::picker::{PickerMode, PopupConfig};
    let mut state = PickerState::with_mode(PickerMode::Popup(PopupConfig {
        width_pct: 0.6,
        height_pct: 0.7,
        min_width: 60,
        min_height: 16,
    }));
    state.selected = entries.iter().position(|e| e.is_hint()).unwrap_or(0);
    state
}

// ---------------------------------------------------------------------------
// Search filtering
// ---------------------------------------------------------------------------

/// Filter ShortcutsHelp entries by search query.
///
/// Returns the original-index list of entries that pass the filter.
/// Section headers are kept only when at least one hint in their section
/// matches; this mirrors the palette's `filter_palette_entries` behavior.
pub fn filter_entries(
    entries: &[ShortcutsHelpEntry],
    query: &str,
    hide_dimmed: bool,
    collapsed: &std::collections::HashSet<usize>,
) -> Vec<usize> {
    let searching = !query.is_empty();
    if !searching && !hide_dimmed && collapsed.is_empty() {
        return (0..entries.len()).collect();
    }
    let q = query.to_lowercase();
    let mut result: Vec<usize> = Vec::new();
    let mut pending_header: Option<usize> = None;
    let mut section_has_match = false;
    let mut current_section_collapsed = false;
    for (i, entry) in entries.iter().enumerate() {
        match entry {
            ShortcutsHelpEntry::SectionHeader { category_idx, .. } => {
                if let Some(h) = pending_header.take()
                    && (section_has_match || current_section_collapsed)
                {
                    result.push(h);
                }
                pending_header = Some(i);
                section_has_match = false;
                current_section_collapsed = !searching && collapsed.contains(category_idx);
            }
            ShortcutsHelpEntry::Hint {
                item: h, dimmed, ..
            } => {
                if current_section_collapsed {
                    continue;
                }
                if hide_dimmed && *dimmed {
                    continue;
                }
                let key_text = hint_key_display(h);
                let key_pretty = hint_key_pretty(h);
                let desc = hint_description(h);
                let q_matches = q.is_empty()
                    || h.label.to_lowercase().contains(&q)
                    || key_text.to_lowercase().contains(&q)
                    || key_pretty.to_lowercase().contains(&q)
                    || desc.to_lowercase().contains(&q);
                if q_matches {
                    if let Some(idx) = pending_header.take() {
                        result.push(idx);
                    }
                    section_has_match = true;
                    result.push(i);
                }
            }
        }
    }
    if let Some(h) = pending_header
        && (section_has_match || current_section_collapsed)
    {
        result.push(h);
    }
    result
}

fn hint_key_display(h: &HintItem) -> String {
    if let Some(d) = h.custom_display {
        d.to_string()
    } else {
        h.keys
            .iter()
            .map(|k| k.display())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Pretty key display for the cheatsheet modal.
///
/// Uses `custom_display` when set (for special representations like
/// "Esc Esc" that can't be derived from the key list), otherwise renders
/// the actual keys with pretty formatting (e.g. "Ctrl+Q", "Tab / i / Space").
fn hint_key_pretty(h: &HintItem) -> String {
    if let Some(d) = h.custom_display {
        return d.to_string();
    }
    h.keys
        .iter()
        .map(|k| k.display_pretty())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Get the long description for a hint, falling back to the short label.
pub fn entry_display(entries: &[ShortcutsHelpEntry], idx: usize) -> (String, String) {
    match entries.get(idx) {
        Some(ShortcutsHelpEntry::Hint { item: h, .. }) => (hint_description(h), hint_key_pretty(h)),
        Some(ShortcutsHelpEntry::SectionHeader { label, .. }) => {
            ((*label).to_string(), String::new())
        }
        None => (String::new(), String::new()),
    }
}

fn hint_description(h: &HintItem) -> String {
    h.description
        .as_ref()
        .map(|d| d.to_string())
        .unwrap_or_else(|| {
            // Capitalize the short label as a fallback.
            let label = h.label.as_ref();
            let mut chars = label.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn selected_original_entry<'a>(
    filtered: &[usize],
    entries: &'a [ShortcutsHelpEntry],
    selected: usize,
) -> Option<&'a ShortcutsHelpEntry> {
    filtered.get(selected).and_then(|&i| entries.get(i))
}

fn non_selectable_mask(filtered: &[usize], _entries: &[ShortcutsHelpEntry]) -> Vec<bool> {
    filtered.iter().map(|_| false).collect()
}

fn picker_config(non_sel: &[bool]) -> PickerConfig<'_> {
    PickerConfig {
        title: None,
        show_search_hint: false,
        expandable: false,
        esc_clears_query: true,
        shortcuts: None,
        pending_hint: None,
        non_selectable: non_sel,
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
    }
}

// ---------------------------------------------------------------------------
// Input dispatch
// ---------------------------------------------------------------------------

/// Outcome of an input event delivered to the cheatsheet modal.
///
/// The caller is responsible for mutating `AgentView` state — closing the
/// modal, re-dispatching a synthesized key into `handle_input`, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutsHelpOutcome {
    /// User asked to close the modal (Esc in browse, Ctrl+./Ctrl+X, [x] click).
    Close,
    /// Toggle the filter (show all vs hide dimmed).
    ToggleFilter,
    /// Toggle a section's collapsed state (by category index).
    ToggleSection(usize),
    /// Toggle inline help expand for a hint row (registry or long_help pseudo).
    ToggleExpand(ExpandKey),
    /// Visual state changed (selection, hover, or detail enter/scroll/back) — redraw.
    Changed,
    /// Nothing changed.
    Unchanged,
}

/// Browse list vs in-modal man-style detail (pattern B, same modal chrome).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ShortcutsHelpMode {
    #[default]
    Browse,
    Detail {
        title: String,
        keys_line: String,
        body: String,
        dimmed_note: bool,
        scroll: u16,
    },
}

impl ShortcutsHelpMode {
    pub fn is_browse(&self) -> bool {
        matches!(self, Self::Browse)
    }

    pub fn is_detail(&self) -> bool {
        !self.is_browse()
    }
}

/// Build detail mode state from a cheatsheet entry (title/keys/body for the man page).
///
/// Registry rows always open. Pseudo-rows (`action_id: None`) open only when they
/// ship `long_help`; one without it stays list-only (browse-only).
pub fn detail_from_entry(entry: &ShortcutsHelpEntry) -> Option<ShortcutsHelpMode> {
    let ShortcutsHelpEntry::Hint {
        item,
        dimmed,
        action_id,
        long_help,
    } = entry
    else {
        return None;
    };
    if action_id.is_none() && long_help.is_none() {
        return None;
    }
    let title = item
        .description
        .as_deref()
        .unwrap_or(item.label.as_ref())
        .to_string();
    let keys_line = item
        .custom_display
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            item.keys
                .iter()
                .map(|k| k.display())
                .collect::<Vec<_>>()
                .join(" / ")
        });
    // Body prefers long_help; falls back to the one-line description.
    let body = long_help
        .as_deref()
        .or(item.description.as_deref())
        .unwrap_or(item.label.as_ref())
        .to_string();
    Some(ShortcutsHelpMode::Detail {
        title,
        keys_line,
        body,
        dimmed_note: *dimmed,
        scroll: 0,
    })
}

/// Open the detail page for `entry`, dropping any committed search so Esc from
/// detail returns to an unfiltered browse and closes with one more press.
fn enter_detail(state: &mut PickerState, entry: &ShortcutsHelpEntry) -> Option<ShortcutsHelpMode> {
    let detail = detail_from_entry(entry)?;
    state.set_query("");
    state.search_active = false;
    Some(detail)
}

/// Footer shortcuts while viewing a shortcut detail page.
pub fn modal_footer_detail() -> Vec<crate::views::modal_window::Shortcut<'static>> {
    use crate::views::modal_window::Shortcut;
    vec![
        Shortcut {
            label: "Esc back",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "\u{2191}/\u{2193} scroll",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Ctrl+./X close",
            clickable: false,
            id: 0,
        },
    ]
}

/// Paint the in-modal detail page (title, keys, body) into the content rect.
#[allow(clippy::too_many_arguments)]
pub fn render_detail_body<'a>(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    title: &'a str,
    keys_line: &'a str,
    body: &'a str,
    dimmed_note: bool,
    scroll: u16,
    theme: &crate::theme::Theme,
) {
    use crate::render::wrapping::word_wrap_lines;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Paragraph, Widget};

    if area.width == 0 || area.height == 0 {
        return;
    }
    // Borrow from the owned detail payload — no allocation while building the rows.
    let mut lines: Vec<Line<'a>> = Vec::new();
    lines.push(Line::from(Span::styled(
        title,
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
    )));
    if !keys_line.is_empty() {
        lines.push(Line::from(Span::styled(
            keys_line,
            Style::default().fg(theme.gray_bright),
        )));
    }
    // Skip the body when it merely repeats the title (no long_help yet) to avoid a duplicate line.
    if !body.is_empty() && body != title {
        lines.push(Line::from(""));
        // Blank line between paragraphs so the detail page reads as spaced blocks; the inline expand (arrows) keeps them tight.
        for (i, para) in body.split('\n').enumerate() {
            if i > 0 {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                para,
                Style::default().fg(theme.text_primary),
            )));
        }
    }
    if dimmed_note {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(not active in current context)",
            Style::default().fg(theme.gray_dim),
        )));
    }
    // Pre-wrap to the content width so scrolling counts wrapped rows, not logical lines.
    let wrapped = word_wrap_lines(lines, area.width as usize);
    // Clamp the displayed offset so over-scroll shows the last wrapped row, never a blank body.
    let max_scroll = wrapped.len().saturating_sub(area.height as usize);
    let skip = (scroll as usize).min(max_scroll);
    // Rows are already wrapped to width, so render verbatim (no second wrap pass).
    let visible: Vec<Line<'static>> = wrapped.into_iter().skip(skip).collect();
    Paragraph::new(visible).render(area, buf);
}

/// Render the detail page (pattern B) with its modal chrome + footer. Shared by
/// both hosts so the chrome orchestration lives in one place (like `CheatsheetRows`).
pub fn render_detail(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    window: &mut crate::views::modal_window::ModalWindowState,
    mode: &ShortcutsHelpMode,
    theme: &crate::theme::Theme,
    compact: bool,
) {
    use crate::views::modal_window as mw;
    let ShortcutsHelpMode::Detail {
        title,
        keys_line,
        body,
        dimmed_note,
        scroll,
    } = mode
    else {
        return;
    };
    let footer = modal_footer_detail();
    let modal_config = mw::ModalWindowConfig {
        title: "Keyboard Shortcuts",
        tabs: None,
        shortcuts: &footer,
        sizing: modal_sizing(compact),
        fold_info: None,
    };
    if let Some(mca) = mw::render_modal_window(buf, area, window, &modal_config, theme) {
        render_detail_body(
            buf,
            mca.content,
            title,
            keys_line,
            body,
            *dimmed_note,
            *scroll,
            theme,
        );
    }
}

/// Help line(s) shown under an expanded hint: prefers the action's `long_help`,
/// falling back to the palette description. Callers split on `\n` for multi-line.
pub fn hint_inline_help(entry: &ShortcutsHelpEntry) -> Option<&str> {
    match entry {
        ShortcutsHelpEntry::Hint {
            item, long_help, ..
        } => long_help.as_deref().or(item.description.as_deref()),
        _ => None,
    }
}

/// Expand key for pattern A (e/Space/l/→). Registry rows use their ActionId;
/// pseudo-rows with `long_help` and a static label use [`ExpandKey::Pseudo`].
pub fn expand_key(entry: &ShortcutsHelpEntry) -> Option<ExpandKey> {
    match entry {
        ShortcutsHelpEntry::Hint {
            action_id: Some(id),
            ..
        } => Some(ExpandKey::Action(*id)),
        ShortcutsHelpEntry::Hint {
            action_id: None,
            long_help: Some(_),
            item,
            ..
        } => match item.label {
            Cow::Borrowed(s) => Some(ExpandKey::Pseudo(s)),
            Cow::Owned(_) => None,
        },
        _ => None,
    }
}

/// Whether this hint can participate in inline expand (registry-backed rows only).
/// Prefer [`expand_key`] for new code; kept for call sites that need an ActionId.
pub fn hint_expand_action_id(entry: &ShortcutsHelpEntry) -> Option<crate::actions::ActionId> {
    match expand_key(entry) {
        Some(ExpandKey::Action(id)) => Some(id),
        _ => None,
    }
}

/// Flip `value`'s membership in `set`: insert when absent, remove when present.
/// Shared by both modal hosts for the section-collapse and inline-expand toggles.
pub fn toggle_membership<T: Eq + std::hash::Hash>(
    set: &mut std::collections::HashSet<T>,
    value: T,
) {
    if !set.remove(&value) {
        set.insert(value);
    }
}

/// Dispatch a key event to the cheatsheet picker. Mutates `state`.
///
/// When `mode` is `Detail`, keys scroll the man page or return to browse; global
/// close chords still dismiss the whole modal.
pub fn handle_input(
    key: &crossterm::event::KeyEvent,
    entries: &[ShortcutsHelpEntry],
    state: &mut PickerState,
    hide_dimmed: bool,
    collapsed: &std::collections::HashSet<usize>,
    expanded_ids: &std::collections::HashSet<ExpandKey>,
    mode: &mut ShortcutsHelpMode,
) -> ShortcutsHelpOutcome {
    use crossterm::event::{Event, KeyCode, KeyModifiers};

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('.') | KeyCode::Char('x'))
    {
        return ShortcutsHelpOutcome::Close;
    }

    if mode.is_detail() {
        // Back-to-browse keys handled before borrowing `scroll` so we can replace `mode`.
        // Vim keys (h/j/k/g) are intentionally NOT bound here — vim modal bindings are owned separately.
        if matches!(key.code, KeyCode::Esc | KeyCode::Left | KeyCode::Backspace) {
            *mode = ShortcutsHelpMode::Browse;
            return ShortcutsHelpOutcome::Changed;
        }
        if let ShortcutsHelpMode::Detail { scroll, .. } = mode {
            return match key.code {
                KeyCode::Down | KeyCode::PageDown => {
                    *scroll = scroll.saturating_add(1);
                    ShortcutsHelpOutcome::Changed
                }
                KeyCode::Up | KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(1);
                    ShortcutsHelpOutcome::Changed
                }
                KeyCode::Home => {
                    *scroll = 0;
                    ShortcutsHelpOutcome::Changed
                }
                _ => ShortcutsHelpOutcome::Unchanged,
            };
        }
        return ShortcutsHelpOutcome::Unchanged;
    }

    let searching = state.search_active || !state.query().is_empty();
    let vim_mode = crate::appearance::cache::load_vim_mode();

    if !searching {
        // `i` mirrors the vim-nav pickers' "press i to search" affordance.
        if key.code == KeyCode::Char('/')
            || (key.code == KeyCode::Char('i') && key.modifiers.is_empty())
        {
            state.search_active = true;
            return ShortcutsHelpOutcome::Changed;
        }
        if key.code == KeyCode::Char('f') {
            return ShortcutsHelpOutcome::ToggleFilter;
        }
        let filtered = filter_entries(entries, state.query(), hide_dimmed, collapsed);
        if let Some(ShortcutsHelpEntry::SectionHeader { category_idx, .. }) =
            selected_original_entry(&filtered, entries, state.selected)
        {
            let is_collapsed = collapsed.contains(category_idx);
            let toggle = match key.code {
                KeyCode::Char('e') | KeyCode::Char(' ') | KeyCode::Enter => true,
                KeyCode::Right => is_collapsed,
                KeyCode::Char('l') if vim_mode && key.modifiers.is_empty() => is_collapsed,
                KeyCode::Char('E') | KeyCode::Left => !is_collapsed,
                KeyCode::Char('h') if vim_mode && key.modifiers.is_empty() => !is_collapsed,
                _ => false,
            };
            if toggle {
                return ShortcutsHelpOutcome::ToggleSection(*category_idx);
            }
        } else if let Some(entry) = selected_original_entry(&filtered, entries, state.selected)
            && let Some(key_id) = expand_key(entry)
        {
            let is_expanded = expanded_ids.contains(&key_id);
            let toggle = match key.code {
                KeyCode::Char('e') | KeyCode::Char(' ') | KeyCode::Right => true,
                KeyCode::Char('l') if vim_mode && key.modifiers.is_empty() => true,
                KeyCode::Char('E') | KeyCode::Left => is_expanded,
                KeyCode::Char('h') if vim_mode && key.modifiers.is_empty() => is_expanded,
                _ => false,
            };
            if toggle {
                return ShortcutsHelpOutcome::ToggleExpand(key_id);
            }
        }
        // Enter on a registry hint opens in-modal detail (pattern B); section handled above.
        if key.code == KeyCode::Enter {
            if let Some(entry) = selected_original_entry(&filtered, entries, state.selected)
                && let Some(detail) = detail_from_entry(entry)
            {
                *mode = detail;
                return ShortcutsHelpOutcome::Changed;
            }
            return ShortcutsHelpOutcome::Unchanged;
        }
        if matches!(
            key.code,
            KeyCode::Char('h')
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::Char('l')
                | KeyCode::Down
                | KeyCode::Up
                | KeyCode::Char('g')
                | KeyCode::Char('G')
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Esc
                | KeyCode::Tab
        ) {
            let non_sel: Vec<bool> = non_selectable_mask(&filtered, entries);
            let config = picker_config(&non_sel);
            let ev = Event::Key(*key);
            return match handle_picker_input(&ev, state, filtered.len(), &config) {
                PickerOutcome::Selected(_) | PickerOutcome::Closed => ShortcutsHelpOutcome::Close,
                PickerOutcome::Unchanged => ShortcutsHelpOutcome::Unchanged,
                PickerOutcome::Changed | PickerOutcome::QueryChanged => {
                    ShortcutsHelpOutcome::Changed
                }
                _ => ShortcutsHelpOutcome::Changed,
            };
        }
        return ShortcutsHelpOutcome::Unchanged;
    }

    if key.code == KeyCode::Esc {
        state.set_query("");
        state.search_active = false;
        state.selected = 0;
        return ShortcutsHelpOutcome::Changed;
    }

    let filtered = filter_entries(entries, state.query(), hide_dimmed, collapsed);
    let non_sel: Vec<bool> = non_selectable_mask(&filtered, entries);
    let config = picker_config(&non_sel);

    let ev = Event::Key(*key);
    match handle_picker_input(&ev, state, filtered.len(), &config) {
        PickerOutcome::Selected(idx) => {
            state.search_active = false;
            match selected_original_entry(&filtered, entries, idx) {
                Some(ShortcutsHelpEntry::SectionHeader { category_idx, .. }) => {
                    ShortcutsHelpOutcome::ToggleSection(*category_idx)
                }
                Some(entry) => {
                    if let Some(detail) = enter_detail(state, entry) {
                        *mode = detail;
                        ShortcutsHelpOutcome::Changed
                    } else {
                        ShortcutsHelpOutcome::Unchanged
                    }
                }
                None => ShortcutsHelpOutcome::Unchanged,
            }
        }
        PickerOutcome::Closed => ShortcutsHelpOutcome::Close,
        PickerOutcome::Unchanged => ShortcutsHelpOutcome::Unchanged,
        PickerOutcome::Changed | PickerOutcome::QueryChanged => ShortcutsHelpOutcome::Changed,
        _ => ShortcutsHelpOutcome::Changed,
    }
}

/// Dispatch a mouse event to the cheatsheet picker. Mutates `state`.
pub fn handle_mouse(
    mouse: &crossterm::event::MouseEvent,
    entries: &[ShortcutsHelpEntry],
    state: &mut PickerState,
    hide_dimmed: bool,
    collapsed: &std::collections::HashSet<usize>,
    mode: &mut ShortcutsHelpMode,
) -> ShortcutsHelpOutcome {
    if mode.is_detail() {
        use crossterm::event::MouseEventKind;
        if let ShortcutsHelpMode::Detail { scroll, .. } = mode {
            return match mouse.kind {
                MouseEventKind::ScrollDown => {
                    *scroll = scroll.saturating_add(1);
                    ShortcutsHelpOutcome::Changed
                }
                MouseEventKind::ScrollUp => {
                    *scroll = scroll.saturating_sub(1);
                    ShortcutsHelpOutcome::Changed
                }
                _ => ShortcutsHelpOutcome::Unchanged,
            };
        }
    }

    let filtered = filter_entries(entries, state.query(), hide_dimmed, collapsed);
    let non_sel: Vec<bool> = non_selectable_mask(&filtered, entries);
    let config = picker_config(&non_sel);

    let ev = crossterm::event::Event::Mouse(*mouse);
    match handle_picker_input(&ev, state, filtered.len(), &config) {
        PickerOutcome::Selected(idx) => {
            // Clicking a section header toggles it; hint opens detail (pattern B).
            if let Some(ShortcutsHelpEntry::SectionHeader { category_idx, .. }) =
                selected_original_entry(&filtered, entries, idx)
            {
                ShortcutsHelpOutcome::ToggleSection(*category_idx)
            } else if let Some(entry) = selected_original_entry(&filtered, entries, idx) {
                // enter_detail drops the committed search so click matches the keyboard path.
                if let Some(detail) = enter_detail(state, entry) {
                    *mode = detail;
                    ShortcutsHelpOutcome::Changed
                } else {
                    ShortcutsHelpOutcome::Unchanged
                }
            } else {
                ShortcutsHelpOutcome::Unchanged
            }
        }
        PickerOutcome::Closed => ShortcutsHelpOutcome::Close,
        PickerOutcome::Unchanged => ShortcutsHelpOutcome::Unchanged,
        PickerOutcome::Changed | PickerOutcome::QueryChanged => ShortcutsHelpOutcome::Changed,
        _ => ShortcutsHelpOutcome::Changed,
    }
}

// ---------------------------------------------------------------------------
// Modal rendering + chrome integration
// ---------------------------------------------------------------------------

/// Footer hints painted along the bottom border of the cheatsheet
/// modal. Identical visual vocabulary for the agent view and the
/// dashboard so muscle memory ports across surfaces.
pub fn modal_footer(filter_active: bool) -> Vec<crate::views::modal_window::Shortcut<'static>> {
    use crate::views::modal_window::Shortcut;
    let mut shortcuts = vec![
        Shortcut {
            label: "\u{2191}/\u{2193} nav",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: if filter_active {
                "f show all"
            } else {
                "f filter"
            },
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "e/Space/\u{2192} expand",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "\u{2190} collapse",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Enter details",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "/ search",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Esc close",
            clickable: false,
            id: 0,
        },
    ];
    // Append the `i search` alias last for vim users (matching the other pickers).
    crate::views::modal_window::push_vim_nav_search_hint(&mut shortcuts, false);
    shortcuts
}

/// Modal-window sizing for the cheatsheet. The `compact` knob lets
/// callers honour the user's compact-prompt setting (smaller
/// margins + tighter padding) without re-deriving the sizing rules.
pub fn modal_sizing(compact: bool) -> crate::views::modal_window::ModalSizing {
    crate::views::modal_window::ModalSizing {
        width_pct: 0.70,
        max_width: 80,
        min_width: 44,
        v_margin: 4,
        h_pad: 2,
        v_pad: 1,
        footer_lines: 2,
    }
    .with_compact(compact)
}

/// Per-row kind captured during [`CheatsheetRows::build`] so the borrowed
/// picker rows need only the owned buffers, not the source `entries`.
enum CheatsheetRowKind {
    Header {
        is_collapsed: bool,
    },
    Hint {
        dimmed: bool,
        expand: Option<ExpandKey>,
    },
    Other,
}

/// Owned per-frame buffers backing the cheatsheet picker rows, shared by both
/// modal hosts (agent inline render + dashboard [`render_modal`]). The
/// [`crate::views::picker::PickerEntry`] list from [`Self::picker_entries`]
/// borrows these buffers, so this value must outlive the render call.
pub struct CheatsheetRows {
    row_strs: Vec<(String, String)>,
    // Inline-help per row, newlines collapsed to spaces so the collapsible view renders one
    // wrap-flowed block; empty string when the row has no help. Owned (it's a transform of the source).
    help_text: Vec<String>,
    kinds: Vec<CheatsheetRowKind>,
}

impl CheatsheetRows {
    /// Build the row buffers for the current filter/collapse state. Both hosts
    /// call this so the row/expand construction lives in exactly one place.
    pub fn build(
        entries: &[ShortcutsHelpEntry],
        query: &str,
        filter_active: bool,
        collapsed_sections: &std::collections::HashSet<usize>,
    ) -> Self {
        let filtered = filter_entries(entries, query, filter_active, collapsed_sections);
        let mut row_strs = Vec::with_capacity(filtered.len());
        let mut help_text = Vec::with_capacity(filtered.len());
        let mut kinds = Vec::with_capacity(filtered.len());
        for &i in &filtered {
            match entries.get(i) {
                Some(ShortcutsHelpEntry::SectionHeader {
                    label,
                    entry_count,
                    category_idx,
                }) => {
                    let is_collapsed = collapsed_sections.contains(category_idx);
                    let display = if is_collapsed {
                        format!("{label} ({entry_count})")
                    } else {
                        (*label).to_string()
                    };
                    row_strs.push((display, String::new()));
                    help_text.push(String::new());
                    kinds.push(CheatsheetRowKind::Header { is_collapsed });
                }
                Some(entry @ ShortcutsHelpEntry::Hint { dimmed, .. }) => {
                    row_strs.push(entry_display(entries, i));
                    // Collapse newlines to spaces so the collapsible view shows one wrap-flowed block (no hard breaks).
                    let help = hint_inline_help(entry)
                        .map(|s| s.replace('\n', " "))
                        .unwrap_or_default();
                    help_text.push(help);
                    kinds.push(CheatsheetRowKind::Hint {
                        dimmed: *dimmed,
                        expand: expand_key(entry),
                    });
                }
                _ => {
                    row_strs.push(entry_display(entries, i));
                    help_text.push(String::new());
                    kinds.push(CheatsheetRowKind::Other);
                }
            }
        }
        Self {
            row_strs,
            help_text,
            kinds,
        }
    }

    /// Borrowed views of the per-row inline help, in row order. The caller holds
    /// these so the picker's description slices can borrow them across the render.
    pub fn help_refs(&self) -> Vec<&str> {
        self.help_text.iter().map(String::as_str).collect()
    }

    /// Build the borrowed picker rows, reading selection + expand state. The
    /// returned list borrows `self` and `help` (from [`Self::help_refs`]), so it
    /// lives only as long as both.
    pub fn picker_entries<'a>(
        &'a self,
        state: &PickerState,
        expanded_ids: &std::collections::HashSet<ExpandKey>,
        help: &'a [&'a str],
    ) -> Vec<crate::views::picker::PickerEntry<'a>> {
        use crate::views::picker::{PickerEntry, PickerRow};
        debug_assert_eq!(
            help.len(),
            self.kinds.len(),
            "help must be 1:1 with rows (pass CheatsheetRows::help_refs)"
        );
        self.kinds
            .iter()
            .enumerate()
            .map(|(idx, kind)| {
                let selected = state.hovered == Some(idx)
                    || (state.hovered.is_none() && idx == state.selected);
                match kind {
                    CheatsheetRowKind::Header { is_collapsed } => PickerEntry::Row(PickerRow {
                        label: self.row_strs[idx].0.as_str(),
                        right_label: "",
                        selected,
                        expanded: !is_collapsed,
                        fields: &[],
                        description_lines: &[],
                        summary_lines: &[],
                        dimmed: false,
                        indent: 0,
                        badge: "",
                        badge_color: None,
                        collapsible: true,
                        underline_last_desc: false,
                    }),
                    CheatsheetRowKind::Hint { dimmed, expand } => {
                        let is_expanded =
                            expand.map(|id| expanded_ids.contains(&id)).unwrap_or(false);
                        let description_lines: &[&str] = if is_expanded && !help[idx].is_empty() {
                            std::slice::from_ref(&help[idx])
                        } else {
                            &[]
                        };
                        PickerEntry::Row(PickerRow {
                            label: self.row_strs[idx].0.as_str(),
                            right_label: self.row_strs[idx].1.as_str(),
                            selected,
                            expanded: is_expanded,
                            fields: &[],
                            description_lines,
                            summary_lines: &[],
                            dimmed: *dimmed,
                            indent: 1,
                            badge: "",
                            badge_color: None,
                            collapsible: false,
                            underline_last_desc: false,
                        })
                    }
                    CheatsheetRowKind::Other => PickerEntry::Row(PickerRow {
                        label: self.row_strs[idx].0.as_str(),
                        right_label: self.row_strs[idx].1.as_str(),
                        selected: false,
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
                    }),
                }
            })
            .collect()
    }
}

/// Render the cheatsheet modal in full (chrome + picker content).
///
/// Pulled out of `AgentView::draw` so the dashboard can paint the
/// exact same modal without re-plumbing `ModalWindowConfig` /
/// picker-inner glue. The agent view continues to drive its own
/// modal via `views::modal::ActiveModal::ShortcutsHelp`; this
/// function consumes the same fields by reference.
///
/// The signature mirrors the destructured `ActiveModal::ShortcutsHelp`
/// fields one-to-one so callers can splat them directly — packing
/// these into a wrapper struct would force every call site to
/// build an intermediate just to take it apart again at the
/// chrome / picker boundary.
#[allow(clippy::too_many_arguments)]
pub fn render_modal(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    entries: &[ShortcutsHelpEntry],
    state: &mut PickerState,
    window: &mut crate::views::modal_window::ModalWindowState,
    filter_active: bool,
    collapsed_sections: &std::collections::HashSet<usize>,
    expanded_ids: &std::collections::HashSet<ExpandKey>,
    mode: &ShortcutsHelpMode,
    theme: &crate::theme::Theme,
    compact: bool,
) {
    use crate::views::modal_window as mw;
    use crate::views::picker::{self, PickerHitAreas};
    use ratatui::layout::Rect;

    // Detail screen reuses the same modal chrome with a different footer.
    if mode.is_detail() {
        render_detail(buf, area, window, mode, theme, compact);
        return;
    }

    let rows = CheatsheetRows::build(entries, state.query(), filter_active, collapsed_sections);
    let help_refs = rows.help_refs();
    let picker_entries = rows.picker_entries(state, expanded_ids, &help_refs);
    let non_sel: Vec<bool> = vec![false; picker_entries.len()];
    let footer = modal_footer(filter_active);
    let modal_config = mw::ModalWindowConfig {
        title: "Keyboard Shortcuts",
        tabs: None,
        shortcuts: &footer,
        sizing: modal_sizing(compact),
        fold_info: None,
    };
    let Some(mca) = mw::render_modal_window(buf, area, window, &modal_config, theme) else {
        return;
    };
    let content_area = mca.content;
    let inner_x = mca.inner_x;
    let inner_width = mca.inner_width;
    let searching = state.search_active || !state.query().is_empty();
    let show_search_hint = !searching;

    picker::render_picker_search_bar(
        buf,
        content_area.x,
        content_area.y,
        content_area.width,
        theme,
        state,
        searching,
        show_search_hint,
        Some(theme.bg_base),
    );
    let sep_y = content_area.y + 1;
    if sep_y < content_area.y + content_area.height {
        picker::render_divider(buf, inner_x, sep_y, inner_width, theme, Some(theme.bg_base));
    }
    let entries_start_y = sep_y + 1;
    let search_bar_rect = Rect::new(content_area.x, content_area.y, content_area.width, 1);
    let entries_area = Rect {
        x: content_area.x,
        y: entries_start_y,
        width: content_area.width,
        height: content_area
            .height
            .saturating_sub(entries_start_y.saturating_sub(content_area.y)),
    };
    let content_hit = picker::render_picker_content_with_scrollbar_x(
        buf,
        entries_area,
        theme,
        state,
        &picker_entries,
        &non_sel,
        &[],
        Some(theme.bg_base),
        false,
        0,
        inner_x + inner_width - 1,
    );
    state.hit_areas = Some(PickerHitAreas {
        close_button: Rect::default(),
        search_bar: search_bar_rect,
        item_rects: content_hit.item_rects,
        entry_indices: content_hit.entry_indices,
        tab_rects: vec![],
        filter_rect: None,
    });
}

/// Outcome of routing a key through the cheatsheet's
/// chrome + picker pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKeyOutcome {
    /// User asked to close the modal (Esc in browse, Ctrl+./Ctrl+X,
    /// or the close chrome button).
    Close,
    /// `f` was pressed — caller should flip `filter_active`.
    ToggleFilter,
    /// User toggled a section header (collapse / expand).
    ToggleSection(usize),
    /// Toggle inline help for a hint row (registry or long_help pseudo).
    ToggleExpand(ExpandKey),
    /// Visual state changed (cursor, query, scroll, or detail enter/back).
    Changed,
    /// Nothing changed.
    Unchanged,
}

/// Route a key through the cheatsheet's modal-window chrome + the
/// picker `handle_input`. Mirrors the agent view's per-modal
/// handler so the dashboard can reuse the exact same key
/// semantics. Caller owns `filter_active` / `collapsed_sections`
/// so the result mutations stay local to the wrapping struct.
///
/// Args follow the same one-to-one shape as the field set behind
/// `ActiveModal::ShortcutsHelp` so dashboards and agents can call
/// it via plain destructuring instead of building / unpacking a
/// wrapper struct.
#[allow(clippy::too_many_arguments)]
pub fn handle_modal_key(
    key: &crossterm::event::KeyEvent,
    entries: &[ShortcutsHelpEntry],
    state: &mut PickerState,
    window: &mut crate::views::modal_window::ModalWindowState,
    filter_active: bool,
    collapsed_sections: &std::collections::HashSet<usize>,
    expanded_ids: &std::collections::HashSet<ExpandKey>,
    mode: &mut ShortcutsHelpMode,
    compact: bool,
) -> ModalKeyOutcome {
    use crate::views::modal_window as mw;
    use crossterm::event::KeyCode;

    let searching = state.search_active || !state.query().is_empty();
    if mode.is_browse() && searching && key.code == KeyCode::Esc {
        state.set_query("");
        state.search_active = false;
        state.selected = 0;
        return ModalKeyOutcome::Changed;
    }
    let footer = if mode.is_detail() {
        modal_footer_detail()
    } else {
        modal_footer(filter_active)
    };
    let chrome_cfg = mw::ModalWindowConfig {
        title: "Keyboard Shortcuts",
        tabs: None,
        shortcuts: &footer,
        sizing: modal_sizing(compact),
        fold_info: None,
    };
    // Detail owns Esc (back to browse); skip chrome so it doesn't close the modal.
    if mode.is_browse() {
        match mw::handle_modal_key(window, key, &chrome_cfg) {
            mw::ModalWindowOutcome::CloseRequested => return ModalKeyOutcome::Close,
            mw::ModalWindowOutcome::Unhandled => {}
            _ => return ModalKeyOutcome::Changed,
        }
    }
    match handle_input(
        key,
        entries,
        state,
        filter_active,
        collapsed_sections,
        expanded_ids,
        mode,
    ) {
        ShortcutsHelpOutcome::Close => ModalKeyOutcome::Close,
        ShortcutsHelpOutcome::ToggleFilter => ModalKeyOutcome::ToggleFilter,
        ShortcutsHelpOutcome::ToggleSection(idx) => ModalKeyOutcome::ToggleSection(idx),
        ShortcutsHelpOutcome::ToggleExpand(id) => ModalKeyOutcome::ToggleExpand(id),
        ShortcutsHelpOutcome::Changed => ModalKeyOutcome::Changed,
        ShortcutsHelpOutcome::Unchanged => ModalKeyOutcome::Unchanged,
    }
}

pub fn handle_paste(
    text: &str,
    state: &mut PickerState,
    mode: &ShortcutsHelpMode,
) -> ShortcutsHelpOutcome {
    if mode.is_detail() || !state.search_active {
        return ShortcutsHelpOutcome::Unchanged;
    }
    match state.paste_query(text) {
        crate::input::line_editor::LineEditOutcome::TextChanged => {
            state.selected = 0;
            state.selection_hidden = false;
            state.scroll_offset = None;
            ShortcutsHelpOutcome::Changed
        }
        crate::input::line_editor::LineEditOutcome::HandledNoChange
        | crate::input::line_editor::LineEditOutcome::CursorChanged => {
            ShortcutsHelpOutcome::Changed
        }
        crate::input::line_editor::LineEditOutcome::Unhandled => ShortcutsHelpOutcome::Unchanged,
    }
}

#[cfg(test)]
#[path = "shortcuts_help_tests.rs"]
mod tests;
