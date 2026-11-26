//! Shared session picker helpers.
//!
//! Centralises data types, entry building, and index-mapping logic used by
//! both the welcome-screen session picker (`welcome/mod.rs` + `app_view.rs`)
//! and the modal session picker (`ActiveModal::SessionPicker` in
//! `agent_view.rs`).

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::app::app_view::SessionPickerEntry;
use crate::views::picker::{PickerEntry, PickerField, PickerRow, PickerState};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Offset added to content-hit indices in the picker `expanded` set so
/// they don't collide with fuzzy-entry indices.
pub const CONTENT_EXPAND_OFFSET: usize = 100_000;

/// Session id for free-text Enter (`SubmitQuery` with no selectable rows).
///
/// Only a trimmed UUID is loadable — pasted garbage must not call
/// `LoadSession` (that left the TUI stuck mid-load).
pub fn session_id_for_direct_load(query: &str) -> Option<&str> {
    let q = query.trim();
    // `Uuid::try_parse` rejects empty, multi-line, and non-UUID text.
    uuid::Uuid::try_parse(q).ok()?;
    Some(q)
}

/// Derive a short repo display name from a CWD path.
///
/// Uses the last 2 normal path components joined by `-`. For paths with
/// only one normal component (e.g., `/echoagent`), returns that component alone.
/// Does not perform tilde expansion — callers provide absolute paths.
/// Returns `"unknown"` for empty input.
///
/// Examples: `/home/user/fw/1` → `"fw-1"`, `/echoagent` → `"echoagent"`, `/` → `"/"`.
///
/// Shared by the session-list builder (which stamps each entry's `repo_name`)
/// and the picker pinning below, so the current-cwd key matches a group key.
/// Callers pass the *live* cwd (`app.cwd` / `agent.session.cwd`) so a project
/// switch (`Effect::SetWorkingDir`) is reflected immediately.
pub(crate) fn repo_name_from_cwd(cwd: &str) -> String {
    let path = std::path::Path::new(cwd);
    let components: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => os.to_str(),
            _ => None,
        })
        .collect();
    if components.is_empty() {
        return if cwd.is_empty() {
            "unknown".to_string()
        } else {
            cwd.to_string()
        };
    }
    let start = components.len().saturating_sub(2);
    let tail = &components[start..];
    tail.join("-")
}

/// Order repo groups alphabetically, then pin the current working
/// directory's repo group (if present) to the front. Shared by
/// [`build_entry_map`] and [`build_grouped_picker_entries`] so the
/// index-mapping and rendering paths stay in lock-step.
fn order_repo_groups(groups: &mut IndexMap<&str, Vec<usize>>, current_repo: Option<&str>) {
    groups.sort_keys();
    if let Some(cur) = current_repo
        && let Some(pos) = groups.keys().position(|k| *k == cur)
    {
        groups.move_index(pos, 0);
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Which underlying data a picker position maps to.
#[derive(Debug, Clone)]
pub enum PickerItem {
    Fuzzy { original_index: usize },
    Content { hit_index: usize },
}

/// A session armed for deletion, captured on `d` so the `y` confirm keeps
/// a valid `(source, session_id, cwd)` even if the lists shift. Shared by
/// the welcome and modal `/resume` pickers so they can't drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelete {
    pub source: String,
    pub session_id: String,
    pub cwd: String,
}

/// Outcome of routing a key through an armed [`PendingDelete`] confirm.
pub(crate) enum PendingDeleteKey {
    /// `y`: caller should delete this session.
    Confirm(PendingDelete),
    /// `n`: arm cleared; caller should redraw.
    Cancel,
    /// Other key: arm cleared, but the key should still be processed.
    Disarmed,
    /// Nothing armed, or not an unmodified key press.
    NotArmed,
}

/// Arm a [`PendingDelete`] from the selected row, or `None` if it can't be
/// deleted (foreign source or non-selectable position).
pub(crate) fn pending_delete_from_selection(
    selected: usize,
    entry_map: &[Option<PickerItem>],
    entries: Option<&[SessionPickerEntry]>,
    content_results: Option<&[echo_agent_runtime::extensions::session_search::SearchSessionHit]>,
) -> Option<PendingDelete> {
    match entry_map.get(selected).and_then(|e| e.as_ref())? {
        PickerItem::Fuzzy { original_index } => entries
            .and_then(|e| e.get(*original_index))
            .filter(|entry| !crate::app::is_foreign_picker_source(&entry.source))
            .map(|e| PendingDelete {
                source: e.source.clone(),
                session_id: e.id.clone(),
                cwd: e.cwd.clone(),
            }),
        PickerItem::Content { hit_index } => {
            content_results
                .and_then(|h| h.get(*hit_index))
                .map(|h| PendingDelete {
                    source: "local".into(),
                    session_id: h.session_id.clone(),
                    cwd: h.cwd.clone(),
                })
        }
    }
}

/// Route a key through an armed [`PendingDelete`]: `y` confirms, `n`
/// cancels, any other unmodified key disarms and falls through.
pub(crate) fn handle_pending_delete_key(
    pending: &mut Option<PendingDelete>,
    ev: &crossterm::event::Event,
) -> PendingDeleteKey {
    use crossterm::event::{Event, KeyCode, KeyEventKind};
    if pending.is_none() {
        return PendingDeleteKey::NotArmed;
    }
    let Event::Key(k) = ev else {
        return PendingDeleteKey::NotArmed;
    };
    if k.kind != KeyEventKind::Press || !k.modifiers.is_empty() {
        return PendingDeleteKey::NotArmed;
    }
    match k.code {
        KeyCode::Char('y') => pending
            .take()
            .map(PendingDeleteKey::Confirm)
            .unwrap_or(PendingDeleteKey::Cancel),
        KeyCode::Char('n') => {
            *pending = None;
            PendingDeleteKey::Cancel
        }
        _ => {
            *pending = None;
            PendingDeleteKey::Disarmed
        }
    }
}

/// Owned data for a single session picker row. Built once per frame and
/// then borrowed by `PickerEntry` / `PickerField` slices. Shared between
/// the welcome-screen `render_session_picker` and the
/// `ActiveModal::SessionPicker` rendering in `agent_view.rs`.
pub struct SessionEntryData {
    pub summary: String,
    pub right_text: String,
    pub is_selected: bool,
    pub is_expanded: bool,
    pub field_data: Vec<(String, String)>,
    /// Short snippet preview for content search hits (always visible).
    pub snippet_preview: Option<String>,
    pub badge: &'static str,
    pub collapsible: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum SessionPickerPendingNotice {
    Empty(String),
    Error(String),
}

/// Native/foreign completion state shared by session picker surfaces.
#[derive(Debug, Clone, Default)]
pub struct SessionPickerLanes {
    pub(crate) foreign_loading: bool,
    pub(crate) pending_notice: Option<SessionPickerPendingNotice>,
}

impl SessionPickerLanes {
    pub(crate) fn take_ready_notice(&mut self, has_entries: bool) -> Option<String> {
        match self.pending_notice.take() {
            Some(SessionPickerPendingNotice::Empty(message)) if !has_entries => Some(message),
            Some(SessionPickerPendingNotice::Error(message)) => Some(message),
            _ => None,
        }
    }
}

/// Loading gate for a session picker surface's spinner: nothing to show yet —
/// no loaded entry passes the source filter — while the native fetch or
/// foreign scan is still in flight. The filter check (not `entries.is_none()`)
/// matters because the fast foreign scan can land rows the default EchoAgent view
/// hides before the native list arrives; the empty state must wait until both
/// lanes settle. Shared by rendering, redraw forcing, and tick demand so the
/// three cannot drift (a spinner that renders without demanding ticks parks
/// on its first frame).
pub(crate) fn loading_spinner_active(
    entries: Option<&[SessionPickerEntry]>,
    source_filter: SourceFilter,
    loading: bool,
    lanes: &SessionPickerLanes,
) -> bool {
    let nothing_visible = entries.is_none_or(|entries| {
        !entries
            .iter()
            .any(|entry| source_filter.matches(&entry.source))
    });
    nothing_visible && (loading || lanes.foreign_loading)
}

// ---------------------------------------------------------------------------
// Source filter
// ---------------------------------------------------------------------------

/// Filter session entries by native, remote, or external source.
///
/// Default is [`Self::EchoAgent`]: native EchoAgent sessions only (local / remote /
/// conversation), so `/resume` does not mix Claude/Codex/Cursor foreign
/// sessions into the list. `f` cycles EchoAgent → External → All → Local →
/// Remote — External first so one press from the default reveals foreign
/// sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFilter {
    /// Native EchoAgent sessions only — excludes Claude/Codex/Cursor foreign rows.
    #[default]
    EchoAgent,
    Local,
    Remote,
    External,
    /// Every source, including foreign agent sessions.
    All,
}

impl SourceFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::EchoAgent => "EchoAgent",
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::External => "External",
            Self::All => "All",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::EchoAgent => Self::External,
            Self::External => Self::All,
            Self::All => Self::Local,
            Self::Local => Self::Remote,
            Self::Remote => Self::EchoAgent,
        }
    }

    /// Returns `true` when a non-default filter is selected.
    pub fn is_active(self) -> bool {
        self != Self::EchoAgent
    }

    /// Returns `true` if a session with the given `source` string passes the filter.
    ///
    /// cloud.echo-agent.invalid conversations carry `source == "conversation"` and live remotely,
    /// so they pass the `Remote` filter (and `EchoAgent` / `All`) but not `Local`.
    /// Foreign sources (`claude` / `codex` / `cursor`) only pass `External` and
    /// `All`.
    pub fn matches(self, source: &str) -> bool {
        match self {
            Self::EchoAgent => !crate::app::is_foreign_picker_source(source),
            Self::Local => source == "local" || source == "both",
            Self::Remote => source == "remote" || source == "both" || source == "conversation",
            Self::External => crate::app::is_foreign_picker_source(source),
            Self::All => true,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PickerSelectionAnchor {
    key: Option<PickerSelectionKey>,
    fallback_index: usize,
    scroll_delta: Option<isize>,
}

#[derive(Debug, Clone)]
enum PickerSelectionKey {
    Fuzzy { source: String, id: String },
    Content { id: String },
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_picker_selection(
    entries: Option<&[SessionPickerEntry]>,
    content_results: Option<&[echo_agent_runtime::extensions::session_search::SearchSessionHit]>,
    state: &PickerState,
    query: &str,
    grouped: bool,
    content_loading: bool,
    source_filter: SourceFilter,
    current_repo: Option<&str>,
) -> PickerSelectionAnchor {
    let map = build_entry_map(
        entries,
        content_results,
        query,
        grouped,
        content_loading,
        source_filter,
        current_repo,
    );
    let key = map
        .get(state.selected)
        .and_then(|item| item.as_ref())
        .and_then(|item| match item {
            PickerItem::Fuzzy { original_index } => entries
                .and_then(|entries| entries.get(*original_index))
                .map(|entry| PickerSelectionKey::Fuzzy {
                    source: entry.source.clone(),
                    id: entry.id.clone(),
                }),
            PickerItem::Content { hit_index } => content_results
                .and_then(|results| results.get(*hit_index))
                .map(|hit| PickerSelectionKey::Content {
                    id: hit.session_id.clone(),
                }),
        });
    PickerSelectionAnchor {
        key,
        fallback_index: state.selected,
        scroll_delta: state
            .scroll_offset
            .map(|offset| state.selected as isize - offset as isize),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn restore_picker_selection(
    anchor: PickerSelectionAnchor,
    entries: Option<&[SessionPickerEntry]>,
    content_results: Option<&[echo_agent_runtime::extensions::session_search::SearchSessionHit]>,
    state: &mut PickerState,
    query: &str,
    grouped: bool,
    content_loading: bool,
    source_filter: SourceFilter,
    current_repo: Option<&str>,
) {
    let map = build_entry_map(
        entries,
        content_results,
        query,
        grouped,
        content_loading,
        source_filter,
        current_repo,
    );
    let selected = anchor
        .key
        .as_ref()
        .and_then(|key| {
            map.iter().position(|item| match (key, item.as_ref()) {
                (
                    PickerSelectionKey::Fuzzy { source, id },
                    Some(PickerItem::Fuzzy { original_index }),
                ) => entries
                    .and_then(|entries| entries.get(*original_index))
                    .is_some_and(|entry| &entry.source == source && &entry.id == id),
                (PickerSelectionKey::Content { id }, Some(PickerItem::Content { hit_index })) => {
                    content_results
                        .and_then(|results| results.get(*hit_index))
                        .is_some_and(|hit| &hit.session_id == id)
                }
                _ => false,
            })
        })
        .or_else(|| selectable_fallback(&map, anchor.fallback_index))
        .unwrap_or(0);
    state.selected = selected;
    state.scroll_offset = anchor.scroll_delta.map(|delta| {
        (selected as isize - delta)
            .max(0)
            .min(map.len().saturating_sub(1) as isize) as usize
    });
}

fn selectable_fallback<T>(map: &[Option<T>], preferred: usize) -> Option<usize> {
    if map.is_empty() {
        return None;
    }
    let preferred = preferred.min(map.len() - 1);
    (preferred..map.len())
        .find(|index| map[*index].is_some())
        .or_else(|| (0..preferred).rev().find(|index| map[*index].is_some()))
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

/// Case-insensitive substring match (callers pass a pre-lowercased query).
///
/// Deliberately not an ordered-chars subsequence match: that matched so
/// loosely (e.g. "rc" hitting "rust-check") that spurious title rows
/// drowned out the results users actually searched for.
pub(crate) fn fuzzy_matches_session(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(query)
}

/// The query the picker's local fuzzy filter should apply on top of the
/// current entries.
///
/// When `entries_query` (the query the entries were server-fetched with)
/// matches the live query, the entries are already filtered server-side — by
/// message content as well as title — so the local fuzzy match is skipped:
/// re-applying it would hide content-only hits. Every consumer of
/// [`filter_session_entries`] / [`build_entry_map`] on picker state must use
/// this so input handling, rendering, and cursor re-anchoring agree on row
/// indices.
pub(crate) fn effective_filter_query<'a>(
    live_query: &'a str,
    entries_query: Option<&str>,
) -> &'a str {
    if entries_query.is_some_and(|q| q.trim() == live_query.trim()) {
        ""
    } else {
        live_query
    }
}

/// Filter session entries by query and source filter, returning indices of matching entries.
///
/// When the query is empty, all entries match the text filter. The
/// `source_filter` is always applied (entries whose `source` field
/// does not pass [`SourceFilter::matches`] are excluded).
pub(crate) fn filter_session_entries(
    entries: Option<&[SessionPickerEntry]>,
    query: &str,
    source_filter: SourceFilter,
) -> Vec<usize> {
    let Some(entries) = entries else {
        return vec![];
    };
    let q = query.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            source_filter.matches(&e.source)
                && (query.is_empty()
                    || fuzzy_matches_session(&e.id, &q)
                    || fuzzy_matches_session(&e.summary, &q))
        })
        .map(|(i, _)| i)
        .collect()
}

// ---------------------------------------------------------------------------
// Entry map building
// ---------------------------------------------------------------------------

/// Build a flat list of picker items from fuzzy + content results,
/// deduplicating content hits that already appear in the fuzzy list.
pub(crate) fn build_virtual_list(
    entries: Option<&[SessionPickerEntry]>,
    content_results: Option<&[echo_agent_runtime::extensions::session_search::SearchSessionHit]>,
    query: &str,
    source_filter: SourceFilter,
) -> Vec<PickerItem> {
    let fuzzy_indices = filter_session_entries(entries, query, source_filter);
    let fuzzy_ids: HashSet<&str> = entries
        .map(|ents| {
            fuzzy_indices
                .iter()
                .filter_map(|&i| {
                    ents.get(i)
                        .filter(|entry| !crate::app::is_foreign_picker_source(&entry.source))
                        .map(|entry| entry.id.as_str())
                })
                .collect()
        })
        .unwrap_or_default();
    let mut items: Vec<PickerItem> = fuzzy_indices
        .into_iter()
        .map(|i| PickerItem::Fuzzy { original_index: i })
        .collect();
    if source_filter != SourceFilter::External
        && let Some(hits) = content_results
    {
        for (hit_idx, hit) in hits.iter().enumerate() {
            if !fuzzy_ids.contains(hit.session_id.as_str()) {
                items.push(PickerItem::Content { hit_index: hit_idx });
            }
        }
    }
    items
}

/// Rebuild expansion keys in the backing-data index space used by session rendering.
pub(crate) fn expand_all_mapped_session_items(
    state: &mut PickerState,
    entry_map: &[Option<PickerItem>],
) {
    state.expanded.clear();
    if state.query().is_empty() {
        return;
    }
    for item in entry_map.iter().flatten() {
        let key = match item {
            PickerItem::Fuzzy { original_index } => *original_index,
            PickerItem::Content { hit_index } => CONTENT_EXPAND_OFFSET + hit_index,
        };
        state.expanded.insert(key);
    }
}

/// Build the position-indexed session map, including non-selectable headers.
pub(crate) fn build_entry_map(
    entries: Option<&[SessionPickerEntry]>,
    content_results: Option<&[echo_agent_runtime::extensions::session_search::SearchSessionHit]>,
    query: &str,
    grouped: bool,
    content_loading: bool,
    source_filter: SourceFilter,
    current_repo: Option<&str>,
) -> Vec<Option<PickerItem>> {
    if grouped {
        let entries_data = entries.unwrap_or(&[]);
        let filtered = filter_session_entries(entries, query, source_filter);
        let mut map: Vec<Option<PickerItem>> = Vec::new();
        {
            let mut groups: IndexMap<&str, Vec<usize>> = IndexMap::new();
            for &orig_idx in &filtered {
                groups
                    .entry(entries_data[orig_idx].repo_name.as_str())
                    .or_default()
                    .push(orig_idx);
            }
            order_repo_groups(&mut groups, current_repo);
            for (_repo, members) in &groups {
                map.push(None); // repo group header
                for &orig_idx in members {
                    map.push(Some(PickerItem::Fuzzy {
                        original_index: orig_idx,
                    }));
                }
            }
        }
        // Append deduplicated content results (only when searching).
        let fuzzy_ids: HashSet<&str> = filtered
            .iter()
            .filter_map(|&i| {
                entries_data
                    .get(i)
                    .filter(|entry| !crate::app::is_foreign_picker_source(&entry.source))
                    .map(|entry| entry.id.as_str())
            })
            .collect();
        let content_items: Vec<usize> = if source_filter != SourceFilter::External
            && let Some(hits) = content_results
            && !query.is_empty()
        {
            hits.iter()
                .enumerate()
                .filter(|(_, h)| !fuzzy_ids.contains(h.session_id.as_str()))
                .map(|(idx, _)| idx)
                .collect()
        } else {
            Vec::new()
        };
        let show_content_header = !content_items.is_empty()
            || (source_filter != SourceFilter::External
                && content_loading
                && !query.trim().is_empty());
        if show_content_header {
            map.push(None); // content header
        }
        for hit_idx in content_items {
            map.push(Some(PickerItem::Content { hit_index: hit_idx }));
        }
        map
    } else {
        let content_for_flat = if query.is_empty() || source_filter == SourceFilter::External {
            None
        } else {
            content_results
        };
        let virtual_list = build_virtual_list(entries, content_for_flat, query, source_filter);
        let fuzzy_count = virtual_list
            .iter()
            .filter(|i| matches!(i, PickerItem::Fuzzy { .. }))
            .count();
        let content_count = virtual_list.len() - fuzzy_count;
        let has_header = content_count > 0
            || (source_filter != SourceFilter::External
                && content_loading
                && !query.trim().is_empty());
        let mut map = Vec::with_capacity(virtual_list.len() + usize::from(has_header));
        for (i, item) in virtual_list.into_iter().enumerate() {
            if has_header && i == fuzzy_count {
                map.push(None); // content header
            }
            map.push(Some(item));
        }
        if has_header && content_count == 0 {
            map.push(None); // loading header with no results yet
        }
        map
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionPickerWorktreeSelection {
    Fuzzy(usize),
    Content { session_id: String, cwd: String },
    Unavailable,
}

/// Resolve Ctrl+W before generic editing because the line editor binds it to delete-word.
pub(crate) fn session_picker_worktree_selection(
    key: &crossterm::event::KeyEvent,
    state: &mut PickerState,
    entry_map: &[Option<PickerItem>],
    non_selectable: &[bool],
    entries: Option<&[SessionPickerEntry]>,
    content_results: Option<&[echo_agent_runtime::extensions::session_search::SearchSessionHit]>,
) -> Option<SessionPickerWorktreeSelection> {
    if key.kind != crossterm::event::KeyEventKind::Press || !crate::key!('w', CONTROL).matches(key)
    {
        return None;
    }
    if entry_map.is_empty() {
        return Some(SessionPickerWorktreeSelection::Unavailable);
    }
    crate::views::picker::clamp_picker_selection(state, entry_map.len(), non_selectable);
    Some(
        match entry_map
            .get(state.selected)
            .and_then(|entry| entry.as_ref())
        {
            Some(PickerItem::Fuzzy { original_index }) => entries
                .and_then(|entries| entries.get(*original_index))
                .filter(|entry| !crate::app::is_foreign_picker_source(&entry.source))
                .map_or(SessionPickerWorktreeSelection::Unavailable, |_| {
                    SessionPickerWorktreeSelection::Fuzzy(*original_index)
                }),
            Some(PickerItem::Content { hit_index }) => content_results
                .and_then(|results| results.get(*hit_index))
                .map_or(SessionPickerWorktreeSelection::Unavailable, |hit| {
                    SessionPickerWorktreeSelection::Content {
                        session_id: hit.session_id.clone(),
                        cwd: hit.cwd.clone(),
                    }
                }),
            None => SessionPickerWorktreeSelection::Unavailable,
        },
    )
}

/// Rebuild backing-index expansion after a session query changes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sync_session_picker_query_expansion(
    entries: Option<&[SessionPickerEntry]>,
    content_results: Option<&[echo_agent_runtime::extensions::session_search::SearchSessionHit]>,
    entries_query: Option<&str>,
    state: &mut PickerState,
    grouped: bool,
    content_loading: bool,
    source_filter: SourceFilter,
    current_repo: Option<&str>,
) {
    let entry_map = build_entry_map(
        entries,
        content_results,
        effective_filter_query(state.query(), entries_query),
        grouped,
        content_loading,
        source_filter,
        current_repo,
    );
    expand_all_mapped_session_items(state, &entry_map);
}

// ---------------------------------------------------------------------------
// Session entry data building
// ---------------------------------------------------------------------------

/// Build owned rendering data for each session entry in the filtered list.
///
/// The caller zips the result with `PickerField` slices and builds
/// `PickerEntry` items that borrow from the returned data.
pub(crate) fn build_session_entry_data(
    entries_data: &[SessionPickerEntry],
    filtered_indices: &[usize],
    state: &PickerState,
    content_width: u16,
) -> Vec<SessionEntryData> {
    use crate::render::line_utils::truncate_str;

    filtered_indices
        .iter()
        .enumerate()
        .map(|(fi, &orig_idx)| {
            let entry = &entries_data[orig_idx];
            let summary = if entry.summary.is_empty() {
                "(no prompt)".to_string()
            } else {
                entry.summary.clone()
            };
            // Prefer last_active_at; fall back to updated_at (not created_at)
            // so pre-migration sessions don't jump to their creation date.
            let right_text = format_time_ago(entry.last_active_at.unwrap_or(entry.updated_at));
            let is_selected = !state.selection_hidden && fi == state.selected;
            let is_foreign = crate::app::is_foreign_picker_source(&entry.source);
            let is_expanded = !is_foreign && state.expanded.contains(&orig_idx);

            let mut field_data: Vec<(String, String)> = Vec::new();
            if is_expanded {
                field_data.push(("ID".into(), entry.id.clone()));
                field_data.push(("CWD".into(), entry.cwd.clone()));
                if let Some(ref model) = entry.model_id {
                    field_data.push(("Model".into(), model.clone()));
                }
                let fmt_time = |dt: chrono::DateTime<chrono::Utc>| {
                    dt.with_timezone(&chrono::Local)
                        .format("%b %d, %l:%M%P")
                        .to_string()
                };
                field_data.push(("Created".into(), fmt_time(entry.created_at)));
                field_data.push(("Updated".into(), fmt_time(entry.updated_at)));
                field_data.push(("Source".into(), entry.source.clone()));
                if let Some(ref host) = entry.hostname {
                    field_data.push(("Host".into(), host.clone()));
                }
                if entry.num_messages > 0 {
                    field_data.push(("Messages".into(), entry.num_messages.to_string()));
                }
                // Recap ("where was I") and the last-turn summary, whenever
                // available. Truncated to the card width like the Prompt line.
                let max_w = content_width.saturating_sub(4 + 12) as usize;
                if let Some(recap) = entry.last_recap.as_deref().map(str::trim)
                    && !recap.is_empty()
                {
                    field_data.push(("Recap".into(), truncate_str(recap, max_w)));
                }
                if let Some(last_turn) = entry.last_turn_summary.as_deref().map(str::trim)
                    && !last_turn.is_empty()
                {
                    field_data.push(("Last turn".into(), truncate_str(last_turn, max_w)));
                }
                if let Some(ref detail) = entry.card_detail {
                    field_data.push((
                        "Turns".into(),
                        format!("{}    Tools  {}", detail.turn_count, detail.tool_call_count),
                    ));
                    if !detail.first_prompt_preview.is_empty() {
                        let preview = truncate_str(&detail.first_prompt_preview, max_w);
                        field_data.push(("Prompt".into(), preview));
                    }
                }
            }

            SessionEntryData {
                summary,
                right_text,
                is_selected,
                is_expanded,
                field_data,
                snippet_preview: None,
                badge: crate::app::badge_for_picker_source(&entry.source),
                collapsible: !is_foreign,
            }
        })
        .collect()
}

/// Build grouped picker entries: sessions grouped by `repo_name` (the
/// current working directory's repo pinned first, the rest alphabetical)
/// with non-selectable `Header` rows separating each group. Returns the
/// entry list and a boolean mask where `true` marks non-selectable header rows.
pub(crate) fn build_grouped_picker_entries<'a>(
    entries_data: &'a [SessionPickerEntry],
    filtered_indices: &[usize],
    built: &'a [SessionEntryData],
    fields_vecs: &'a [Vec<PickerField<'a>>],
    state: &PickerState,
    current_repo: Option<&str>,
) -> (Vec<PickerEntry<'a>>, Vec<bool>) {
    // Group filtered entries by repo_name, sort alphabetically, then pin the
    // current working directory's repo group to the top.
    let mut groups: IndexMap<&str, Vec<usize>> = IndexMap::new();
    for (fi, &orig_idx) in filtered_indices.iter().enumerate() {
        let repo = entries_data[orig_idx].repo_name.as_str();
        groups.entry(repo).or_default().push(fi);
    }
    order_repo_groups(&mut groups, current_repo);

    let mut result: Vec<PickerEntry<'a>> = Vec::new();
    let mut non_selectable: Vec<bool> = Vec::new();

    // Track the grouped position (including headers) to correctly compute selection.
    let mut grouped_pos: usize = 0;
    for (repo_name, member_indices) in &groups {
        // Insert a non-selectable header for this repo group.
        non_selectable.push(true);
        result.push(PickerEntry::Header { label: repo_name });
        grouped_pos += 1;

        // Insert each session row indented under the header.
        for &fi in member_indices {
            let b = &built[fi];
            let fields = &fields_vecs[fi];
            non_selectable.push(false);
            // Use grouped position for selection, not flat filtered index.
            let selected = !state.selection_hidden && grouped_pos == state.selected;
            result.push(PickerEntry::Row(PickerRow {
                label: &b.summary,
                right_label: &b.right_text,
                selected,
                expanded: b.is_expanded,
                fields,
                description_lines: &[],
                summary_lines: &[],
                dimmed: false,
                indent: 1,
                badge: b.badge,
                badge_color: None,
                collapsible: b.collapsible,
                underline_last_desc: false,
            }));
            grouped_pos += 1;
        }
    }

    (result, non_selectable)
}

// ---------------------------------------------------------------------------
// Content search helpers
// ---------------------------------------------------------------------------

/// Build owned rendering data for content search (deep search) result rows.
///
/// Deduplicates hits that already appear in the fuzzy results. The returned
/// entries should be appended after the fuzzy section (and its header row).
pub(crate) fn build_content_entry_data(
    hits: &[echo_agent_runtime::extensions::session_search::SearchSessionHit],
    entries_data: &[SessionPickerEntry],
    filtered_indices: &[usize],
    state: &PickerState,
    content_start: usize,
) -> Vec<SessionEntryData> {
    let fuzzy_ids: HashSet<&str> = entries_data
        .iter()
        .enumerate()
        .filter(|(i, entry)| {
            filtered_indices.contains(i) && !crate::app::is_foreign_picker_source(&entry.source)
        })
        .map(|(_, e)| e.id.as_str())
        .collect();
    let mut row_offset = 0usize;
    hits.iter()
        .enumerate()
        .filter(|(_, h)| !fuzzy_ids.contains(h.session_id.as_str()))
        .map(|(hit_idx, h)| {
            let summary = if h.summary.is_empty() {
                h.snippet
                    .as_deref()
                    .unwrap_or("(no summary)")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string()
            } else {
                h.summary.clone()
            };
            let right_text = if let Ok(dt) = h.updated_at.parse::<chrono::DateTime<chrono::Utc>>() {
                format_time_ago(dt)
            } else {
                String::new()
            };
            let is_selected =
                !state.selection_hidden && state.selected == content_start + row_offset;
            let is_expanded = state.expanded.contains(&(CONTENT_EXPAND_OFFSET + hit_idx));
            row_offset += 1;

            let mut field_data = Vec::new();
            if is_expanded {
                field_data.push(("ID".into(), h.session_id.clone()));
                field_data.push(("CWD".into(), h.cwd.clone()));
            }

            let snippet_preview = h.snippet.as_deref().and_then(|s| {
                let line = s.lines().find(|l| !l.trim().is_empty())?;
                if line.chars().count() > 80 {
                    let truncated: String = line.chars().take(77).collect();
                    Some(format!("{truncated}..."))
                } else {
                    Some(line.to_string())
                }
            });

            SessionEntryData {
                summary,
                right_text,
                is_selected,
                is_expanded,
                field_data,
                snippet_preview,
                badge: "",
                collapsible: true,
            }
        })
        .collect()
}

/// Build the content header label (spinner or "Content matches").
///
/// Returns an empty string when there are no content rows and no loading
/// spinner — the caller should skip rendering the header in that case.
pub(crate) fn build_content_header_label(
    content_loading: bool,
    has_content_rows: bool,
    tick: u64,
) -> String {
    if content_loading {
        let spinner_frames = crate::glyphs::dot_spinner_frames();
        let frame_idx = (tick / 4) as usize % spinner_frames.len();
        format!(
            "{} Searching session content\u{2026}",
            spinner_frames[frame_idx]
        )
    } else if has_content_rows {
        "Extended search results (remote and local sessions)".to_string()
    } else {
        String::new()
    }
}

/// Hint shown on the default `EchoAgent` view when the foreign-session scan loaded
/// Claude/Codex/Cursor entries it hides. EchoAgent-only: `next(EchoAgent) == External`
/// makes the copy literally true, and reaching Local/Remote already cycles
/// through External/All, so the discovery hint is only needed on the default
/// state.
pub(crate) fn hidden_external_hint(
    entries: Option<&[SessionPickerEntry]>,
    source_filter: SourceFilter,
) -> Option<String> {
    if source_filter != SourceFilter::EchoAgent {
        return None;
    }
    let hidden = entries?
        .iter()
        .filter(|entry| crate::app::is_foreign_picker_source(&entry.source))
        .count();
    (hidden > 0).then(|| {
        let plural = if hidden == 1 { "" } else { "s" };
        format!("{hidden} external session{plural} hidden \u{b7} f to show")
    })
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Format a timestamp as a human-readable relative time.
pub(crate) fn format_time_ago(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);

    let raw = if duration.num_minutes() < 1 {
        "just now".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_days() < 30 {
        format!("{}d ago", duration.num_days())
    } else {
        format!("{}mo ago", duration.num_days() / 30)
    };
    // Right-align to fixed width so the column doesn't jump
    format!("{:>8}", raw)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_name_from_cwd_two_components() {
        assert_eq!(repo_name_from_cwd("/home/user/fw/1"), "fw-1");
    }

    #[test]
    fn repo_name_from_cwd_standard_path() {
        assert_eq!(
            repo_name_from_cwd("/home/user/echoagent"),
            "user-echo-agent"
        );
    }

    #[test]
    fn repo_name_from_cwd_empty() {
        assert_eq!(repo_name_from_cwd(""), "unknown");
    }

    #[test]
    fn repo_name_from_cwd_deep_path() {
        assert_eq!(
            repo_name_from_cwd("/home/user/projects/rust/myapp"),
            "rust-myapp"
        );
    }

    #[test]
    fn repo_name_from_cwd_root() {
        assert_eq!(repo_name_from_cwd("/"), "/");
    }

    #[test]
    fn repo_name_from_cwd_single_dir() {
        assert_eq!(repo_name_from_cwd("/echoagent"), "echoagent");
    }

    /// Substring-only title matching: the old ordered-chars fallback let
    /// short queries match most titles, drowning real hits in junk rows.
    #[test]
    fn fuzzy_matches_session_is_substring_only() {
        assert!(fuzzy_matches_session("Rust-Check pipeline", "rust-check"));
        assert!(fuzzy_matches_session("Fix session picker", "picker"));
        assert!(fuzzy_matches_session("anything", ""));
        assert!(
            !fuzzy_matches_session("rust-check", "rc"),
            "ordered-chars subsequence must no longer match"
        );
    }

    /// Entries stamped as server search results for the live query skip the
    /// local title/id fuzzy match: the backend also matches message content,
    /// so a hit titled nothing like the query must survive filtering.
    #[test]
    fn effective_filter_query_skips_fuzzy_for_server_search_results() {
        let mut hit = make_entry("conv-content-1", "r");
        // Content-only match: title has nothing in common with "hit".
        hit.summary = "Quarterly roadmap notes".into();
        let entries = vec![hit];

        // Without the stamp (plain fetch / stale stamp): fuzzy filter hides it.
        assert!(
            filter_session_entries(
                Some(&entries),
                effective_filter_query("hit", None),
                SourceFilter::All,
            )
            .is_empty(),
            "unstamped entries keep the local fuzzy filter"
        );
        assert!(
            filter_session_entries(
                Some(&entries),
                effective_filter_query("hit", Some("older query")),
                SourceFilter::All,
            )
            .is_empty(),
            "a stale stamp (newer fetch in flight) keeps the local filter"
        );

        // Stamped with the live query: server already filtered — row visible.
        assert_eq!(
            filter_session_entries(
                Some(&entries),
                effective_filter_query("hit", Some("hit")),
                SourceFilter::All,
            ),
            vec![0],
            "server search results must render even with an unrelated title"
        );
        // Trim-insensitive: the picker trims before fetching.
        assert_eq!(effective_filter_query(" hit ", Some("hit")), "");
        // Unfiltered fetch stamp (None) + empty live query: no-op.
        assert_eq!(effective_filter_query("", None), "");
    }

    fn make_entry(id: &str, repo: &str) -> SessionPickerEntry {
        SessionPickerEntry {
            id: id.into(),
            summary: id.into(),
            updated_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            cwd: format!("/{repo}"),
            hostname: None,
            source: String::new(),
            model_id: None,
            num_messages: 0,
            last_active_at: None,
            branch: None,
            repo_name: repo.into(),
            worktree_label: None,
            last_turn_summary: None,
            last_recap: None,
            card_detail: None,
        }
    }

    fn make_content_hit(
        session_id: &str,
    ) -> echo_agent_runtime::extensions::session_search::SearchSessionHit {
        echo_agent_runtime::extensions::session_search::SearchSessionHit {
            session_id: session_id.into(),
            summary: session_id.into(),
            cwd: "/r".into(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            snippet: None,
            score: 1.0,
            matched_fields: vec![],
        }
    }

    /// Grouped entry map places repo headers and resolves mouse-click
    /// indices to the correct original session index.
    #[test]
    fn grouped_entry_map_resolves_correct_session() {
        let entries = vec![
            make_entry("s0", "repo-b"),
            make_entry("s1", "repo-a"),
            make_entry("s2", "repo-a"),
        ];

        let map = build_entry_map(
            Some(&entries),
            None,
            "",
            true,
            false,
            SourceFilter::All,
            None,
        );

        // Expected layout (sorted by repo_name):
        //   0: None          (header "repo-a")
        //   1: Fuzzy(orig=1)  (s1 under repo-a)
        //   2: Fuzzy(orig=2)  (s2 under repo-a)
        //   3: None          (header "repo-b")
        //   4: Fuzzy(orig=0)  (s0 under repo-b)
        assert_eq!(map.len(), 5);
        assert!(map[0].is_none(), "repo-a header");
        assert!(
            matches!(map[1], Some(PickerItem::Fuzzy { original_index: 1 })),
            "first session under repo-a"
        );
        assert!(
            matches!(map[2], Some(PickerItem::Fuzzy { original_index: 2 })),
            "second session under repo-a"
        );
        assert!(map[3].is_none(), "repo-b header");
        assert!(
            matches!(map[4], Some(PickerItem::Fuzzy { original_index: 0 })),
            "session under repo-b"
        );

        // Mouse-click index 1 (first data row) must resolve to s1, not s0.
        match &map[1] {
            Some(PickerItem::Fuzzy { original_index }) => {
                assert_eq!(*original_index, 1);
                assert_eq!(entries[*original_index].id, "s1");
            }
            other => panic!("expected Fuzzy, got {other:?}"),
        }

        // Mouse-click index 4 (under repo-b header) resolves to s0.
        match &map[4] {
            Some(PickerItem::Fuzzy { original_index }) => {
                assert_eq!(*original_index, 0);
                assert_eq!(entries[*original_index].id, "s0");
            }
            other => panic!("expected Fuzzy, got {other:?}"),
        }
    }

    /// Grouped entry map with content search results: deduplicates content
    /// hits that overlap fuzzy results, places content header after all
    /// repo groups, and resolves content indices correctly.
    #[test]
    fn grouped_entry_map_with_content_results() {
        let entries = vec![make_entry("s0", "repo-a"), make_entry("s1", "repo-b")];
        // Two content hits: s0 overlaps fuzzy (should be deduped), s_new is unique.
        let content_hits = vec![make_content_hit("s0"), make_content_hit("s_new")];

        // query must be non-empty for content results to be included.
        // Use a query that matches all entries so fuzzy list is preserved.
        let map = build_entry_map(
            Some(&entries),
            Some(&content_hits),
            "s",
            true,
            false,
            SourceFilter::All,
            None,
        );

        // Expected:
        //   0: None            (header "repo-a")
        //   1: Fuzzy(orig=0)   (s0 under repo-a)
        //   2: None            (header "repo-b")
        //   3: Fuzzy(orig=1)   (s1 under repo-b)
        //   4: None            (content header)
        //   5: Content(idx=1)  (s_new — s0 deduped)
        assert_eq!(map.len(), 6);
        assert!(map[0].is_none(), "repo-a header");
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
        assert!(map[2].is_none(), "repo-b header");
        assert!(matches!(
            map[3],
            Some(PickerItem::Fuzzy { original_index: 1 })
        ));
        assert!(map[4].is_none(), "content header");
        assert!(
            matches!(map[5], Some(PickerItem::Content { hit_index: 1 })),
            "s_new at content hit index 1 (s0 deduped)"
        );
    }

    /// An in-flight search with an EMPTY effective query (stamp==live)
    /// appends no "Searching…" header row — a header only one of input map /
    /// render has would shift row indices.
    #[test]
    fn grouped_entry_map_empty_query_with_loading_has_no_header() {
        let entries = vec![make_entry("s0", "r")];
        let map = build_entry_map(
            Some(&entries),
            None,
            "",
            true,
            /* content_loading */ true,
            SourceFilter::All,
            None,
        );
        assert_eq!(map.len(), 2, "repo header + row only, no content header");
        assert!(map[0].is_none(), "repo header");
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
    }

    /// Empty query in grouped mode must not include content results,
    /// matching the renderer's guard.
    #[test]
    fn grouped_entry_map_empty_query_excludes_content() {
        let entries = vec![make_entry("s0", "r")];
        let content_hits = vec![make_content_hit("s_new")];

        let map = build_entry_map(
            Some(&entries),
            Some(&content_hits),
            "",
            true,
            false,
            SourceFilter::All,
            None,
        );

        // Only the repo header + fuzzy entry; no content header/items.
        assert_eq!(map.len(), 2);
        assert!(map[0].is_none());
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
    }

    /// Flat entry map inserts content header at the correct position
    /// and resolution matches the old `to_virtual` semantics.
    #[test]
    fn flat_entry_map_matches_to_virtual_semantics() {
        let entries = vec![make_entry("s0", "r"), make_entry("s1", "r")];
        let content_hits = vec![make_content_hit("s_content")];

        // Empty query: content results excluded (matches renderer guard).
        let map = build_entry_map(
            Some(&entries),
            Some(&content_hits),
            "",
            false,
            false,
            SourceFilter::All,
            None,
        );
        assert_eq!(map.len(), 2);
        assert!(matches!(
            map[0],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 1 })
        ));

        // Non-empty query: content results included with header.
        let map = build_entry_map(
            Some(&entries),
            Some(&content_hits),
            "s",
            false,
            false,
            SourceFilter::All,
            None,
        );
        assert_eq!(map.len(), 4);
        assert!(matches!(
            map[0],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 1 })
        ));
        assert!(map[2].is_none(), "content header");
        assert!(matches!(map[3], Some(PickerItem::Content { hit_index: 0 })));
    }

    #[test]
    fn expand_all_mapped_session_items_uses_backing_indices() {
        let entries = vec![make_entry("zero", "repo-a"), make_entry("needle", "repo-b")];
        let hits = vec![make_content_hit("content")];
        let map = build_entry_map(
            Some(&entries),
            Some(&hits),
            "needle",
            true,
            false,
            SourceFilter::All,
            None,
        );
        let mut state = PickerState::default();
        state.set_query("needle");

        expand_all_mapped_session_items(&mut state, &map);

        assert_eq!(state.expanded, HashSet::from([1, CONTENT_EXPAND_OFFSET]),);
        assert!(!state.expanded.contains(&0), "group header is not an item");
    }

    #[test]
    fn foreign_id_does_not_suppress_native_content_result() {
        let mut foreign = make_entry("shared", "repo");
        foreign.source = "codex".into();
        let entries = vec![foreign];
        let hits = vec![make_content_hit("shared")];

        let flat = build_entry_map(
            Some(&entries),
            Some(&hits),
            "shared",
            false,
            false,
            SourceFilter::All,
            None,
        );
        assert!(matches!(
            flat.as_slice(),
            [
                Some(PickerItem::Fuzzy { original_index: 0 }),
                None,
                Some(PickerItem::Content { hit_index: 0 }),
            ]
        ));

        let grouped = build_entry_map(
            Some(&entries),
            Some(&hits),
            "shared",
            true,
            false,
            SourceFilter::All,
            None,
        );
        assert!(matches!(
            grouped.as_slice(),
            [
                None,
                Some(PickerItem::Fuzzy { original_index: 0 }),
                None,
                Some(PickerItem::Content { hit_index: 0 }),
            ]
        ));

        let state = PickerState::default();
        let content = build_content_entry_data(&hits, &entries, &[0], &state, 2);
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].summary, "shared");
    }

    #[test]
    fn content_header_label_loading() {
        let label = build_content_header_label(true, false, 0);
        assert!(label.contains("Searching"));
    }

    #[test]
    fn content_header_label_has_rows() {
        let label = build_content_header_label(false, true, 0);
        assert_eq!(label, "Extended search results (remote and local sessions)");
    }

    #[test]
    fn content_header_label_empty() {
        let label = build_content_header_label(false, false, 0);
        assert!(label.is_empty());
    }

    /// Empty entries list produces empty map.
    #[test]
    fn empty_entries_produces_empty_map() {
        let map = build_entry_map(None, None, "", true, false, SourceFilter::All, None);
        assert!(map.is_empty());

        let map = build_entry_map(Some(&[]), None, "", false, false, SourceFilter::All, None);
        assert!(map.is_empty());
    }

    #[test]
    fn source_filter_matches() {
        // Default EchoAgent filter: native only (not Claude/Codex/Cursor).
        assert!(SourceFilter::EchoAgent.matches("local"));
        assert!(SourceFilter::EchoAgent.matches("remote"));
        assert!(SourceFilter::EchoAgent.matches("both"));
        assert!(SourceFilter::EchoAgent.matches("conversation"));
        assert!(!SourceFilter::EchoAgent.matches("claude"));
        assert!(!SourceFilter::EchoAgent.matches("codex"));
        assert!(!SourceFilter::EchoAgent.matches("cursor"));

        assert!(SourceFilter::All.matches("local"));
        assert!(SourceFilter::All.matches("remote"));
        assert!(SourceFilter::All.matches("both"));
        assert!(SourceFilter::All.matches("claude"));
        assert!(SourceFilter::All.matches("codex"));
        assert!(SourceFilter::All.matches("cursor"));

        assert!(SourceFilter::Local.matches("local"));
        assert!(SourceFilter::Local.matches("both"));
        assert!(!SourceFilter::Local.matches("remote"));
        assert!(!SourceFilter::Local.matches("claude"));

        assert!(SourceFilter::Remote.matches("remote"));
        assert!(SourceFilter::Remote.matches("both"));
        assert!(!SourceFilter::Remote.matches("local"));
        assert!(!SourceFilter::Remote.matches("cursor"));

        // cloud.echo-agent.invalid conversations are remote: visible under EchoAgent + All + Remote, not Local.
        assert!(SourceFilter::All.matches("conversation"));
        assert!(SourceFilter::Remote.matches("conversation"));
        assert!(!SourceFilter::Local.matches("conversation"));

        assert!(SourceFilter::External.matches("claude"));
        assert!(SourceFilter::External.matches("codex"));
        assert!(SourceFilter::External.matches("cursor"));
        assert!(!SourceFilter::External.matches("local"));
        assert!(!SourceFilter::External.matches("remote"));
        assert!(!SourceFilter::External.matches("both"));
        assert!(!SourceFilter::External.matches("conversation"));
    }

    #[test]
    fn source_filter_cycles() {
        // External first: one press from the default reveals foreign sessions.
        assert_eq!(SourceFilter::EchoAgent.next(), SourceFilter::External);
        assert_eq!(SourceFilter::External.next(), SourceFilter::All);
        assert_eq!(SourceFilter::All.next(), SourceFilter::Local);
        assert_eq!(SourceFilter::Local.next(), SourceFilter::Remote);
        assert_eq!(SourceFilter::Remote.next(), SourceFilter::EchoAgent);
        assert_eq!(SourceFilter::EchoAgent.label(), "EchoAgent");
        assert_eq!(SourceFilter::External.label(), "External");
        assert_eq!(SourceFilter::default(), SourceFilter::EchoAgent);
    }

    #[test]
    fn source_filter_filters_entries() {
        fn entry_with_source(id: &str, source: &str) -> SessionPickerEntry {
            let mut e = make_entry(id, "r");
            e.source = source.into();
            e
        }
        let entries = vec![
            entry_with_source("s0", "local"),
            entry_with_source("s1", "remote"),
            entry_with_source("s2", "both"),
            entry_with_source("s3", "claude"),
            entry_with_source("s4", "codex"),
            entry_with_source("s5", "cursor"),
        ];

        let echoagent = filter_session_entries(Some(&entries), "", SourceFilter::EchoAgent);
        assert_eq!(echoagent, vec![0, 1, 2]); // local + remote + both, no foreign

        let all = filter_session_entries(Some(&entries), "", SourceFilter::All);
        assert_eq!(all, vec![0, 1, 2, 3, 4, 5]);

        let local = filter_session_entries(Some(&entries), "", SourceFilter::Local);
        assert_eq!(local, vec![0, 2]); // local + both

        let remote = filter_session_entries(Some(&entries), "", SourceFilter::Remote);
        assert_eq!(remote, vec![1, 2]); // remote + both

        let external = filter_session_entries(Some(&entries), "", SourceFilter::External);
        assert_eq!(external, vec![3, 4, 5]);
    }

    #[test]
    fn source_filter_empty_and_unknown_source() {
        // Empty / unknown source (e.g. from old data or test fixtures) is not
        // foreign, so it passes EchoAgent + All but never Local, Remote, or External.
        assert!(SourceFilter::EchoAgent.matches(""));
        assert!(SourceFilter::All.matches(""));
        assert!(!SourceFilter::Local.matches(""));
        assert!(!SourceFilter::Remote.matches(""));
        assert!(!SourceFilter::External.matches(""));

        assert!(SourceFilter::EchoAgent.matches("unknown"));
        assert!(SourceFilter::All.matches("unknown"));
        assert!(!SourceFilter::Local.matches("unknown"));
        assert!(!SourceFilter::Remote.matches("unknown"));
        assert!(!SourceFilter::External.matches("unknown"));
    }

    #[test]
    fn source_filter_is_active() {
        assert!(!SourceFilter::EchoAgent.is_active());
        assert!(SourceFilter::Local.is_active());
        assert!(SourceFilter::Remote.is_active());
        assert!(SourceFilter::External.is_active());
        assert!(SourceFilter::All.is_active());
    }

    #[test]
    fn hidden_external_hint_visibility() {
        fn entry_with_source(id: &str, source: &str) -> SessionPickerEntry {
            let mut e = make_entry(id, "r");
            e.source = source.into();
            e
        }
        let entries = vec![
            entry_with_source("s0", "local"),
            entry_with_source("s1", "claude"),
            entry_with_source("s2", "codex"),
        ];

        // Only the default EchoAgent view surfaces the hint (with the count).
        assert_eq!(
            hidden_external_hint(Some(&entries), SourceFilter::EchoAgent).as_deref(),
            Some("2 external sessions hidden \u{b7} f to show")
        );
        assert!(hidden_external_hint(Some(&entries), SourceFilter::Local).is_none());
        assert!(hidden_external_hint(Some(&entries), SourceFilter::Remote).is_none());

        // Singular count.
        let one = vec![
            entry_with_source("s0", "local"),
            entry_with_source("s1", "cursor"),
        ];
        assert_eq!(
            hidden_external_hint(Some(&one), SourceFilter::EchoAgent).as_deref(),
            Some("1 external session hidden \u{b7} f to show")
        );

        // External / All show foreign rows — no hint.
        assert!(hidden_external_hint(Some(&entries), SourceFilter::External).is_none());
        assert!(hidden_external_hint(Some(&entries), SourceFilter::All).is_none());

        // No foreign entries loaded (native-only or no scan) — no hint.
        let native = vec![entry_with_source("s0", "local")];
        assert!(hidden_external_hint(Some(&native), SourceFilter::EchoAgent).is_none());
        assert!(hidden_external_hint(None, SourceFilter::EchoAgent).is_none());
    }

    /// The expanded resume card surfaces the recap and last-turn summary when
    /// present, and omits those rows when absent.
    #[test]
    fn expanded_card_shows_recap_and_last_turn_when_present() {
        let mut entry = make_entry("s_recap", "repo");
        entry.source = "local".into();
        entry.last_recap = Some("Where we left off: auth refactor".into());
        entry.last_turn_summary = Some("Wired retries into billing".into());
        let mut state = PickerState::default();
        state.expanded.insert(0);

        let built = build_session_entry_data(&[entry], &[0], &state, 80);
        let has = |label: &str, value: &str| {
            built[0]
                .field_data
                .iter()
                .any(|(l, v)| l == label && v.contains(value))
        };
        assert!(has("Recap", "auth refactor"), "recap row missing");
        assert!(has("Last turn", "billing"), "last-turn row missing");

        // Absent when the entry has neither.
        let bare = make_entry("s_bare", "repo");
        let mut state = PickerState::default();
        state.expanded.insert(0);
        let built = build_session_entry_data(&[bare], &[0], &state, 80);
        assert!(
            !built[0]
                .field_data
                .iter()
                .any(|(l, _)| l == "Recap" || l == "Last turn"),
            "recap/last-turn rows must be omitted when absent"
        );
    }

    #[test]
    fn foreign_entry_uses_source_badge_and_has_no_detail_expansion() {
        let mut entry = make_entry("foreign", "repo");
        entry.source = "codex".into();
        let mut state = PickerState::default();
        state.expanded.insert(0);

        let built = build_session_entry_data(&[entry], &[0], &state, 80);

        assert_eq!(built[0].badge, "codex");
        assert!(!built[0].collapsible);
        assert!(!built[0].is_expanded);
        assert!(built[0].field_data.is_empty());
    }

    #[test]
    fn source_filter_combined_with_text_query() {
        fn entry_with_source(id: &str, source: &str) -> SessionPickerEntry {
            let mut e = make_entry(id, "r");
            e.source = source.into();
            e
        }
        let entries = vec![
            entry_with_source("alpha", "local"),
            entry_with_source("beta", "remote"),
            entry_with_source("gamma", "both"),
        ];

        // Text query "alpha" + Local filter: only alpha matches both criteria.
        let result = filter_session_entries(Some(&entries), "alpha", SourceFilter::Local);
        assert_eq!(result, vec![0]);

        // Text query "alpha" + Remote filter: alpha is local-only, so no matches.
        let result = filter_session_entries(Some(&entries), "alpha", SourceFilter::Remote);
        assert!(result.is_empty());

        // Text query matching all + Local filter: local + both pass.
        let result = filter_session_entries(Some(&entries), "", SourceFilter::Local);
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn grouped_entry_map_with_source_filter() {
        fn entry_with_source(id: &str, repo: &str, source: &str) -> SessionPickerEntry {
            let mut e = make_entry(id, repo);
            e.source = source.into();
            e
        }
        let entries = vec![
            entry_with_source("s0", "repo-a", "local"),
            entry_with_source("s1", "repo-a", "remote"),
            entry_with_source("s2", "repo-b", "both"),
        ];

        // Local filter: s0 (local) + s2 (both), grouped by repo.
        let map = build_entry_map(
            Some(&entries),
            None,
            "",
            true,
            false,
            SourceFilter::Local,
            None,
        );
        // repo-a header + s0 + repo-b header + s2 = 4
        assert_eq!(map.len(), 4);
        assert!(map[0].is_none()); // repo-a header
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
        assert!(map[2].is_none()); // repo-b header
        assert!(matches!(
            map[3],
            Some(PickerItem::Fuzzy { original_index: 2 })
        ));

        // Remote filter: s1 (remote) + s2 (both).
        let map = build_entry_map(
            Some(&entries),
            None,
            "",
            true,
            false,
            SourceFilter::Remote,
            None,
        );
        assert_eq!(map.len(), 4);
        assert!(map[0].is_none()); // repo-a header
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 1 })
        ));
        assert!(map[2].is_none()); // repo-b header
        assert!(matches!(
            map[3],
            Some(PickerItem::Fuzzy { original_index: 2 })
        ));
    }

    /// `current_repo` pins the matching group to the top; remaining groups
    /// stay alphabetical. Without it, groups are purely alphabetical.
    #[test]
    fn grouped_entry_map_pins_current_repo_first() {
        let entries = vec![
            make_entry("s0", "repo-a"),
            make_entry("s1", "repo-b"),
            make_entry("s2", "repo-c"),
        ];

        // Pin repo-c: its group leads, then repo-a, repo-b alphabetically.
        let map = build_entry_map(
            Some(&entries),
            None,
            "",
            true,
            false,
            SourceFilter::All,
            Some("repo-c"),
        );
        // [repo-c hdr, s2, repo-a hdr, s0, repo-b hdr, s1]
        assert_eq!(map.len(), 6);
        assert!(map[0].is_none(), "repo-c header pinned first");
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 2 })
        ));
        assert!(map[2].is_none(), "repo-a header");
        assert!(matches!(
            map[3],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
        assert!(map[4].is_none(), "repo-b header");
        assert!(matches!(
            map[5],
            Some(PickerItem::Fuzzy { original_index: 1 })
        ));

        // A current_repo with no matching group is a no-op (pure alphabetical).
        let map = build_entry_map(
            Some(&entries),
            None,
            "",
            true,
            false,
            SourceFilter::All,
            Some("repo-zzz"),
        );
        assert!(map[0].is_none(), "repo-a header");
        assert!(matches!(
            map[1],
            Some(PickerItem::Fuzzy { original_index: 0 })
        ));
    }

    #[test]
    fn session_id_for_direct_load_accepts_uuid_only() {
        let sid = "019fb61a-85a5-7ba0-a4ec-24647dca1893";
        assert_eq!(session_id_for_direct_load(sid), Some(sid));
        assert_eq!(session_id_for_direct_load(&format!("  {sid}  ")), Some(sid));
        assert_eq!(session_id_for_direct_load("not-a-uuid"), None);
        assert_eq!(session_id_for_direct_load(""), None);
        assert_eq!(session_id_for_direct_load("pasted garbage!!!"), None);
        assert_eq!(session_id_for_direct_load("hello\nworld"), None);
        assert_eq!(session_id_for_direct_load(&format!("{sid}\nextra")), None);
    }
}
