use std::ops::Range;

/// Status of a turn in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TurnStatus {
    /// Turn is currently running (agent is responding).
    #[default]
    Running,
    /// Turn completed successfully.
    Completed,
    /// Turn failed (error occurred).
    Failed,
    /// Turn was cancelled by user.
    Cancelled,
}

/// Navigation direction for block selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NavDirection {
    /// Moving down (j key) - should show top of block first.
    #[default]
    Down,
    /// Moving up (k key) - should show bottom of block first.
    Up,
}

/// A turn in the conversation (user prompt + all responses until next prompt).
#[derive(Debug, Clone)]
pub struct Turn {
    /// Index of the UserPrompt entry that starts this turn.
    pub prompt_index: usize,
    /// Index past the last entry (exclusive, like Range).
    pub end_index: usize,
    /// Current status.
    pub status: TurnStatus,
}

impl Turn {
    /// Get the range of entry indices in this turn.
    pub fn range(&self) -> Range<usize> {
        self.prompt_index..self.end_index
    }

    /// Number of entries in this turn.
    pub fn len(&self) -> usize {
        self.end_index - self.prompt_index
    }

    /// Whether this turn is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// How the scrollback is displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Show all turns in a single timeline.
    #[default]
    AllTurns,
    /// Show only a single turn.
    SingleTurn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportSnapshot {
    pub(crate) scroll_offset: usize,
    pub(crate) follow_mode: bool,
    pub(crate) follow_preserve_scroll: bool,
    pub(crate) viewport_height: u16,
    pub(crate) last_width: u16,
    pub(crate) selected: Option<usize>,
    pub(crate) current_turn: Option<usize>,
    pub(crate) view_mode: ViewMode,
    pub(crate) total_height: usize,
}

/// Maximum truncated header height for AllTurns sticky headers.
/// (vpad + 3 content lines + ellipsis if needed + vpad)
pub(super) const MAX_TRUNCATED_HEADER_HEIGHT: u16 = 6;

/// Duration (ms) an entry's accent stays bright after finishing.
/// Used by the renderer to flash the accent on recently-finished entries.
pub const FINISH_FLASH_DURATION_MS: u64 = 400;

/// Extra entries measured EXACTLY just beyond the visible viewport edge when
/// settling lazy heights. A small below-margin means a just-off-screen entry is
/// already exact before it scrolls in, so small estimate errors don't leave a
/// visible entry sized from an estimate.
pub(super) const MEASURE_MARGIN_ENTRIES: usize = 8;

/// Entries kept (not swept) on each side of the measurement window by
/// off-screen render-cache eviction — several screens' worth, so normal
/// paging never touches a cold entry, while the bulk of a long session's
/// rendered output can be reclaimed.
pub(super) const EVICT_KEEP_MARGIN_ENTRIES: usize = 128;

/// On a bottom-pinned full rebuild (resume) we eagerly measure this many pages
/// of entries ABOVE the viewport, so an immediate scroll-up lands on already
/// exact heights (no estimate->exact rebuild, hence no jump) and the scrollbar
/// is accurate right away. Bounded — keeps resume at O(viewport), not O(history).
pub(super) const RESUME_WARM_PAGES: u16 = 3;

/// Per-entry layout info, cached for rendering and navigation.
///
/// Combines height and gap data in a single struct for cache-friendliness
/// (they're always accessed together during layout and rendering).
#[derive(Debug, Clone, Copy, Default)]
pub struct EntryLayoutInfo {
    /// Rendered height at current width.
    pub height: u16,
    /// Gap rows after this entry (0 for dense group members, 1 otherwise).
    pub gap_after: u16,
    /// When non-zero, this entry renders as a group header instead of its
    /// normal block content. For N-more truncation headers it is the number
    /// of hidden entries (drives the plain "╶╶ N more" fallback text; frames
    /// with fold spans render the aggregated bucket label instead). For
    /// verb-group headers it is the run's MEMBER count — tool calls and
    /// subagent rows; folded thoughts never count — and the value is never
    /// rendered — header gates go through [`Self::is_group_header`].
    pub group_header_count: u16,
    /// When true, this entry renders as an expanded-group collapse header.
    /// Set on the first entry of a manually-expanded group. N-more headers
    /// replace the entry's content; verb-group headers stack above member 0.
    pub group_collapse_header: bool,
    /// When true, this entry heads a verb-group run: it renders the aggregated
    /// "Verb N noun" label instead of its own content (collapsed state) or
    /// marks the collapse header of an expanded verb group. The run's other
    /// claimed entries — members and folded thoughts — hide behind it
    /// (height 0) until the group is expanded.
    pub verb_group_header: bool,
}

impl EntryLayoutInfo {
    /// Whether this entry renders as any kind of group header (N-more
    /// truncation, expanded-group collapse, or verb) in place of its own
    /// block content. The single gate shared by every consumer, so no site
    /// re-derives it from the raw fields and silently drops one header
    /// family (the count's meaning differs per family; see
    /// [`Self::group_header_count`]).
    pub fn is_group_header(&self) -> bool {
        self.group_header_count > 0 || self.group_collapse_header
    }

    /// Whether the slot stacks an expanded verb-run header above member 0.
    pub fn is_expanded_verb_header(&self) -> bool {
        self.verb_group_header && self.group_collapse_header
    }

    /// Add the synthetic verb-run header row to a member's own measured height.
    pub fn with_verb_header_row(&self, member_height: u16) -> u16 {
        member_height.saturating_add(u16::from(self.is_expanded_verb_header()))
    }
}
