use std::ops::{Deref, Range};
use std::sync::Arc;

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation as _};
use unicode_width::UnicodeWidthStr as _;

#[path = "editor_keys.rs"]
mod keys;

pub use keys::classify_key_event;
pub(crate) use keys::resolve_movement;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordStyle {
    Small,
    WhitespaceDelimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditCommand {
    Insert(char),
    MoveGraphemeLeft,
    MoveGraphemeRight,
    MoveWordLeft(WordStyle),
    MoveWordRight(WordStyle),
    MoveLogicalLineStart,
    MoveLogicalLineEnd,
    DeleteGraphemeBackward,
    DeleteGraphemeForward,
    DeleteWordBackward(WordStyle),
    DeleteWordForward(WordStyle),
    DeleteToLineStart,
    DeleteToLineEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditCommandCategory {
    Insert,
    Navigation,
    Delete,
    Kill,
}

/// Which selection edge a movement collapses to before moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HorizontalEdge {
    Start,
    End,
}

/// A cursor movement: one vocabulary behind plain move, Shift-extend, and collapse.
/// `Command` carries its collapse edge from construction, so every movement is
/// directional by type (no fallible extraction later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Movement {
    Command(EditCommand, HorizontalEdge),
    VisualRowUp,
    VisualRowDown,
    VisualRowStart,
    VisualRowEnd,
    /// Home: logical line start, no already-at-BOL chain (unlike Ctrl+A).
    LogicalLineStart,
    /// End: logical line end, no already-at-EOL chain (unlike Ctrl+E).
    LogicalLineEnd,
}

impl Movement {
    /// The selection edge this movement collapses an active selection to.
    pub(crate) fn collapse_edge(self) -> HorizontalEdge {
        match self {
            Self::Command(_, edge) => edge,
            Self::VisualRowUp | Self::VisualRowStart | Self::LogicalLineStart => {
                HorizontalEdge::Start
            }
            Self::VisualRowDown | Self::VisualRowEnd | Self::LogicalLineEnd => HorizontalEdge::End,
        }
    }

    /// Grapheme moves stop at the collapse edge; every other movement continues from it.
    pub(crate) fn stops_at_collapse_edge(self) -> bool {
        matches!(
            self,
            Self::Command(
                EditCommand::MoveGraphemeLeft | EditCommand::MoveGraphemeRight,
                _
            )
        )
    }
}

impl EditCommand {
    /// The selection edge this command collapses an active selection to;
    /// `None` for non-directional commands (inserts, deletes, kills).
    pub(crate) fn selection_collapse_edge(self) -> Option<HorizontalEdge> {
        match self {
            Self::MoveGraphemeLeft | Self::MoveWordLeft(_) | Self::MoveLogicalLineStart => {
                Some(HorizontalEdge::Start)
            }
            Self::MoveGraphemeRight | Self::MoveWordRight(_) | Self::MoveLogicalLineEnd => {
                Some(HorizontalEdge::End)
            }
            _ => None,
        }
    }

    pub(crate) fn category(self) -> EditCommandCategory {
        match self {
            Self::Insert(_) => EditCommandCategory::Insert,
            Self::MoveGraphemeLeft
            | Self::MoveGraphemeRight
            | Self::MoveWordLeft(_)
            | Self::MoveWordRight(_)
            | Self::MoveLogicalLineStart
            | Self::MoveLogicalLineEnd => EditCommandCategory::Navigation,
            Self::DeleteGraphemeBackward | Self::DeleteGraphemeForward => {
                EditCommandCategory::Delete
            }
            Self::DeleteWordBackward(_)
            | Self::DeleteWordForward(_)
            | Self::DeleteToLineStart
            | Self::DeleteToLineEnd => EditCommandCategory::Kill,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDelta {
    pub replaced_byte_range: Range<usize>,
    pub inserted_byte_range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Unchanged,
    CursorOnly,
    TextOnly(EditDelta),
    TextAndCursor(EditDelta),
}

impl EditOutcome {
    fn from_changes(delta: Option<EditDelta>, cursor_changed: bool) -> Self {
        match (delta, cursor_changed) {
            (None, false) => Self::Unchanged,
            (None, true) => Self::CursorOnly,
            (Some(delta), false) => Self::TextOnly(delta),
            (Some(delta), true) => Self::TextAndCursor(delta),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostEditCursorAffinity {
    Exact,
    Right,
}

#[derive(Debug, Clone)]
pub struct EditPlan {
    replaced_byte_range: Range<usize>,
    replacement: String,
    removed_text: String,
    cursor_byte: usize,
    cursor_affinity: PostEditCursorAffinity,
    source_identity: Arc<BufferIdentity>,
    source_generation: u64,
}

impl EditPlan {
    pub fn replaced_byte_range(&self) -> Range<usize> {
        self.replaced_byte_range.clone()
    }

    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    pub fn removed_text(&self) -> &str {
        &self.removed_text
    }

    pub fn cursor_byte(&self) -> usize {
        self.cursor_byte
    }

    pub fn cursor_affinity(&self) -> PostEditCursorAffinity {
        self.cursor_affinity
    }

    pub fn into_removed_text(self) -> String {
        self.removed_text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyEditPlanError {
    StalePlan,
    InvalidRange,
    RemovedTextMismatch,
    InvalidCursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleLineViewport {
    pub visible_byte_range: Range<usize>,
    pub cursor_display_column: usize,
}

#[derive(Debug)]
struct BufferIdentity;

#[derive(Debug)]
pub struct EditBuffer {
    text: String,
    cursor_byte: usize,
    identity: Arc<BufferIdentity>,
    generation: u64,
}

impl Default for EditBuffer {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor_byte: 0,
            identity: Arc::new(BufferIdentity),
            generation: 0,
        }
    }
}

impl Clone for EditBuffer {
    fn clone(&self) -> Self {
        Self {
            text: self.text.clone(),
            cursor_byte: self.cursor_byte,
            identity: Arc::new(BufferIdentity),
            generation: 0,
        }
    }
}

impl PartialEq for EditBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.cursor_byte == other.cursor_byte
    }
}

impl Eq for EditBuffer {}

impl Deref for EditBuffer {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.text()
    }
}

impl EditBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor_byte = text.len();
        Self {
            text,
            cursor_byte,
            identity: Arc::new(BufferIdentity),
            generation: 0,
        }
    }

    /// External cursor requests use nearest grapheme boundaries; ties go left for determinism.
    pub fn from_parts(text: impl Into<String>, cursor_byte: usize) -> Self {
        let text = text.into();
        let cursor_byte = normalize_external_cursor(&text, cursor_byte);
        Self {
            text,
            cursor_byte,
            identity: Arc::new(BufferIdentity),
            generation: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    pub fn cursor_byte(&self) -> usize {
        self.cursor_byte
    }

    /// External cursor requests use nearest grapheme boundaries; ties go left for determinism.
    #[must_use]
    pub fn set_cursor_byte(&mut self, cursor_byte: usize) -> EditOutcome {
        let old_cursor = self.cursor_byte;
        self.cursor_byte = normalize_external_cursor(&self.text, cursor_byte);
        let cursor_changed = self.cursor_byte != old_cursor;
        if cursor_changed {
            self.advance_generation();
        }
        EditOutcome::from_changes(None, cursor_changed)
    }

    #[must_use]
    pub fn insert_str(&mut self, text: &str) -> EditOutcome {
        let plan = self.plan_replace_byte_range(self.cursor_byte..self.cursor_byte, text, &[]);
        self.apply_validated_plan(&plan)
    }

    /// Edit-result cursors keep right affinity when adjacent text merges into one grapheme.
    #[must_use]
    pub fn replace_byte_range(&mut self, range: Range<usize>, replacement: &str) -> EditOutcome {
        let plan = self.plan_replace_byte_range(range, replacement, &[]);
        self.apply_validated_plan(&plan)
    }

    pub fn plan_replace_byte_range(
        &self,
        range: Range<usize>,
        replacement: &str,
        atomic_byte_ranges: &[Range<usize>],
    ) -> EditPlan {
        let atomic_byte_ranges = normalize_atomic_ranges(&self.text, atomic_byte_ranges);
        let range = normalize_replacement_range(&self.text, range, &atomic_byte_ranges);
        let cursor_byte = normalize_cursor_for_atomic_ranges(self.cursor_byte, &atomic_byte_ranges);
        let next_cursor = if cursor_byte < range.start {
            cursor_byte
        } else if cursor_byte <= range.end {
            range.start + replacement.len()
        } else {
            cursor_byte - (range.end - range.start) + replacement.len()
        };
        self.make_plan(
            range,
            replacement.to_owned(),
            next_cursor,
            PostEditCursorAffinity::Right,
        )
    }

    pub fn plan_command(
        &self,
        command: EditCommand,
        atomic_byte_ranges: &[Range<usize>],
    ) -> EditPlan {
        let atomic_byte_ranges = normalize_atomic_ranges(&self.text, atomic_byte_ranges);
        let cursor_byte = normalize_cursor_for_atomic_ranges(self.cursor_byte, &atomic_byte_ranges);
        match command {
            EditCommand::Insert(character) => {
                let replacement = character.to_string();
                self.make_plan(
                    cursor_byte..cursor_byte,
                    replacement,
                    cursor_byte + character.len_utf8(),
                    PostEditCursorAffinity::Right,
                )
            }
            EditCommand::MoveGraphemeLeft => self.make_plan(
                cursor_byte..cursor_byte,
                String::new(),
                previous_atomic_boundary(&self.text, cursor_byte, &atomic_byte_ranges),
                PostEditCursorAffinity::Exact,
            ),
            EditCommand::MoveGraphemeRight => self.make_plan(
                cursor_byte..cursor_byte,
                String::new(),
                next_atomic_boundary(&self.text, cursor_byte, &atomic_byte_ranges),
                PostEditCursorAffinity::Exact,
            ),
            EditCommand::MoveWordLeft(style) => {
                let target = self.previous_word_boundary(style, cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    cursor_byte..cursor_byte,
                    String::new(),
                    target,
                    PostEditCursorAffinity::Exact,
                )
            }
            EditCommand::MoveWordRight(style) => {
                let target = self.next_word_boundary(style, cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    cursor_byte..cursor_byte,
                    String::new(),
                    target,
                    PostEditCursorAffinity::Exact,
                )
            }
            EditCommand::MoveLogicalLineStart => {
                let target = self.logical_line_start_target(cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    cursor_byte..cursor_byte,
                    String::new(),
                    target,
                    PostEditCursorAffinity::Exact,
                )
            }
            EditCommand::MoveLogicalLineEnd => {
                let target = self.logical_line_end_target(cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    cursor_byte..cursor_byte,
                    String::new(),
                    target,
                    PostEditCursorAffinity::Exact,
                )
            }
            EditCommand::DeleteGraphemeBackward => {
                let start = previous_atomic_boundary(&self.text, cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    start..cursor_byte,
                    String::new(),
                    start,
                    PostEditCursorAffinity::Right,
                )
            }
            EditCommand::DeleteGraphemeForward => {
                let end = next_atomic_boundary(&self.text, cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    cursor_byte..end,
                    String::new(),
                    cursor_byte,
                    PostEditCursorAffinity::Right,
                )
            }
            EditCommand::DeleteWordBackward(style) => {
                let start = self.previous_word_boundary(style, cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    start..cursor_byte,
                    String::new(),
                    start,
                    PostEditCursorAffinity::Right,
                )
            }
            EditCommand::DeleteWordForward(style) => {
                let end = self.next_word_boundary(style, cursor_byte, &atomic_byte_ranges);
                self.make_plan(
                    cursor_byte..end,
                    String::new(),
                    cursor_byte,
                    PostEditCursorAffinity::Right,
                )
            }
            EditCommand::DeleteToLineStart => {
                let line_start = self.line_start_at(cursor_byte, &atomic_byte_ranges);
                let start = if cursor_byte == line_start {
                    previous_atomic_boundary(&self.text, line_start, &atomic_byte_ranges)
                } else {
                    line_start
                };
                self.make_plan(
                    start..cursor_byte,
                    String::new(),
                    start,
                    PostEditCursorAffinity::Right,
                )
            }
            EditCommand::DeleteToLineEnd => {
                let line_end = self.line_end_from(cursor_byte, &atomic_byte_ranges);
                let start = cursor_byte.min(line_end);
                let end = if cursor_byte >= line_end {
                    self.line_ending_at(line_end)
                        .map_or(line_end, |range| range.end)
                } else {
                    line_end
                };
                self.make_plan(
                    start..end,
                    String::new(),
                    start,
                    PostEditCursorAffinity::Right,
                )
            }
        }
    }

    pub fn apply_plan(&mut self, plan: &EditPlan) -> Result<EditOutcome, ApplyEditPlanError> {
        self.validate_plan(plan)?;
        Ok(self.apply_validated_plan(plan))
    }

    #[must_use]
    pub fn apply(&mut self, command: EditCommand) -> EditOutcome {
        let plan = self.plan_command(command, &[]);
        self.apply_validated_plan(&plan)
    }

    fn make_plan(
        &self,
        replaced_byte_range: Range<usize>,
        replacement: String,
        cursor_byte: usize,
        cursor_affinity: PostEditCursorAffinity,
    ) -> EditPlan {
        let removed_text = self.text[replaced_byte_range.clone()].to_owned();
        EditPlan {
            replaced_byte_range,
            replacement,
            removed_text,
            cursor_byte,
            cursor_affinity,
            source_identity: Arc::clone(&self.identity),
            source_generation: self.generation,
        }
    }

    pub(crate) fn validate_plan(&self, plan: &EditPlan) -> Result<(), ApplyEditPlanError> {
        if !Arc::ptr_eq(&plan.source_identity, &self.identity)
            || plan.source_generation != self.generation
        {
            return Err(ApplyEditPlanError::StalePlan);
        }
        let range = &plan.replaced_byte_range;
        if range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
            || !is_grapheme_boundary(&self.text, range.start)
            || !is_grapheme_boundary(&self.text, range.end)
        {
            return Err(ApplyEditPlanError::InvalidRange);
        }
        if self.text.get(range.clone()) != Some(plan.removed_text.as_str()) {
            return Err(ApplyEditPlanError::RemovedTextMismatch);
        }
        let Some(resulting_len) = self
            .text
            .len()
            .checked_sub(range.end - range.start)
            .and_then(|len| len.checked_add(plan.replacement.len()))
        else {
            return Err(ApplyEditPlanError::InvalidCursor);
        };
        if plan.cursor_byte > resulting_len {
            return Err(ApplyEditPlanError::InvalidCursor);
        }
        if plan.cursor_affinity == PostEditCursorAffinity::Exact
            && (plan.replacement != plan.removed_text
                || !is_grapheme_boundary(&self.text, plan.cursor_byte))
        {
            return Err(ApplyEditPlanError::InvalidCursor);
        }
        Ok(())
    }

    pub(crate) fn apply_validated_plan(&mut self, plan: &EditPlan) -> EditOutcome {
        let old_cursor = self.cursor_byte;
        let text_changed = plan.removed_text != plan.replacement;
        let inserted_len = plan.replacement.len();
        if text_changed {
            self.text
                .replace_range(plan.replaced_byte_range.clone(), &plan.replacement);
        }
        self.cursor_byte = match plan.cursor_affinity {
            PostEditCursorAffinity::Exact => plan.cursor_byte,
            PostEditCursorAffinity::Right => ceil_grapheme_boundary(&self.text, plan.cursor_byte),
        };
        let cursor_changed = self.cursor_byte != old_cursor;
        if text_changed || cursor_changed {
            self.advance_generation();
        }
        let delta = text_changed.then_some(EditDelta {
            inserted_byte_range: plan.replaced_byte_range.start
                ..(plan.replaced_byte_range.start + inserted_len),
            replaced_byte_range: plan.replaced_byte_range.clone(),
        });
        EditOutcome::from_changes(delta, cursor_changed)
    }

    fn advance_generation(&mut self) {
        if let Some(generation) = self.generation.checked_add(1) {
            self.generation = generation;
        } else {
            self.identity = Arc::new(BufferIdentity);
            self.generation = 0;
        }
    }

    pub fn single_line_viewport(&self, display_width: usize) -> SingleLineViewport {
        self.single_line_viewport_with_atomic_ranges(display_width, &[])
    }

    pub fn single_line_viewport_with_atomic_ranges(
        &self,
        display_width: usize,
        atomic_byte_ranges: &[Range<usize>],
    ) -> SingleLineViewport {
        let atomic_byte_ranges = normalize_atomic_ranges(&self.text, atomic_byte_ranges);
        let cursor_byte = self.cursor_byte;
        if display_width == 0 {
            return SingleLineViewport {
                visible_byte_range: cursor_byte..cursor_byte,
                cursor_display_column: 0,
            };
        }

        let line_start = self.line_start_at(cursor_byte, &atomic_byte_ranges);
        let line_end = self.line_end_from(cursor_byte, &atomic_byte_ranges);
        let left_budget = display_width - 1;
        let mut start = cursor_byte;
        let mut left_width = 0usize;
        while start > line_start {
            let previous = previous_atomic_boundary(&self.text, start, &atomic_byte_ranges);
            let grapheme_width = self.text[previous..start].width();
            let next_width = left_width.saturating_add(grapheme_width);
            if next_width > left_budget {
                break;
            }
            start = previous;
            left_width = next_width;
        }

        let mut end = start;
        let mut visible_width = 0usize;
        while end < line_end {
            let next = next_atomic_boundary(&self.text, end, &atomic_byte_ranges);
            let grapheme_width = self.text[end..next].width();
            let next_width = visible_width.saturating_add(grapheme_width);
            if next_width > display_width {
                if end < cursor_byte {
                    end = next;
                }
                break;
            }
            end = next;
            visible_width = next_width;
        }

        SingleLineViewport {
            visible_byte_range: start..end,
            cursor_display_column: self.text[start..cursor_byte].width(),
        }
    }

    fn previous_word_boundary(
        &self,
        style: WordStyle,
        cursor_byte: usize,
        atomic_byte_ranges: &[Range<usize>],
    ) -> usize {
        let mut position = cursor_byte;
        while position > 0 {
            let previous = previous_atomic_boundary(&self.text, position, atomic_byte_ranges);
            if atomic_word_class(&self.text, previous, position, style, atomic_byte_ranges)
                == Some(WordClass::Whitespace)
            {
                position = previous;
            } else {
                break;
            }
        }

        if position == 0 {
            return 0;
        }

        let previous = previous_atomic_boundary(&self.text, position, atomic_byte_ranges);
        let target_class =
            atomic_word_class(&self.text, previous, position, style, atomic_byte_ranges);
        while position > 0 {
            let previous = previous_atomic_boundary(&self.text, position, atomic_byte_ranges);
            if atomic_word_class(&self.text, previous, position, style, atomic_byte_ranges)
                != target_class
            {
                break;
            }
            position = previous;
        }
        position
    }

    fn next_word_boundary(
        &self,
        style: WordStyle,
        cursor_byte: usize,
        atomic_byte_ranges: &[Range<usize>],
    ) -> usize {
        let mut position = cursor_byte;
        while position < self.text.len() {
            let next = next_atomic_boundary(&self.text, position, atomic_byte_ranges);
            if atomic_word_class(&self.text, position, next, style, atomic_byte_ranges)
                == Some(WordClass::Whitespace)
            {
                position = next;
            } else {
                break;
            }
        }

        if position == self.text.len() {
            return position;
        }

        let next = next_atomic_boundary(&self.text, position, atomic_byte_ranges);
        let target_class = atomic_word_class(&self.text, position, next, style, atomic_byte_ranges);
        while position < self.text.len() {
            let next = next_atomic_boundary(&self.text, position, atomic_byte_ranges);
            if atomic_word_class(&self.text, position, next, style, atomic_byte_ranges)
                != target_class
            {
                break;
            }
            position = next;
        }
        position
    }

    fn logical_line_start_target(
        &self,
        cursor_byte: usize,
        atomic_byte_ranges: &[Range<usize>],
    ) -> usize {
        let line_start = self.line_start_at(cursor_byte, atomic_byte_ranges);
        if cursor_byte == line_start && line_start > 0 {
            let previous_line_end =
                previous_atomic_boundary(&self.text, line_start, atomic_byte_ranges);
            self.line_start_at(previous_line_end, atomic_byte_ranges)
        } else {
            line_start
        }
    }

    fn logical_line_end_target(
        &self,
        cursor_byte: usize,
        atomic_byte_ranges: &[Range<usize>],
    ) -> usize {
        let line_end = self.line_end_from(cursor_byte, atomic_byte_ranges);
        if cursor_byte == line_end {
            self.line_ending_at(line_end).map_or(line_end, |range| {
                self.line_end_from(range.end, atomic_byte_ranges)
            })
        } else {
            line_end
        }
    }

    fn line_start_at(&self, cursor_byte: usize, atomic_byte_ranges: &[Range<usize>]) -> usize {
        let cursor_byte = cursor_byte.min(self.text.len());
        (0..cursor_byte)
            .rev()
            .find(|position| {
                self.text.as_bytes()[*position] == b'\n'
                    && !byte_is_inside_atomic_range(*position, atomic_byte_ranges)
            })
            .map_or(0, |position| position + 1)
    }

    fn line_end_from(&self, cursor_byte: usize, atomic_byte_ranges: &[Range<usize>]) -> usize {
        let cursor_byte = cursor_byte.min(self.text.len());
        (cursor_byte..self.text.len())
            .find(|position| {
                self.text.as_bytes()[*position] == b'\n'
                    && !byte_is_inside_atomic_range(*position, atomic_byte_ranges)
            })
            .map_or(self.text.len(), |line_feed| {
                if line_feed > 0 && self.text.as_bytes()[line_feed - 1] == b'\r' {
                    line_feed - 1
                } else {
                    line_feed
                }
            })
    }

    fn line_ending_at(&self, line_end: usize) -> Option<Range<usize>> {
        let remaining = self.text.get(line_end..)?;
        if remaining.starts_with("\r\n") {
            Some(line_end..line_end + 2)
        } else if remaining.starts_with('\n') {
            Some(line_end..line_end + 1)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Whitespace,
    Word,
    Punctuation,
    Atomic(usize),
}

fn word_class(grapheme: &str, style: WordStyle) -> Option<WordClass> {
    let character = grapheme.chars().next()?;
    if character.is_whitespace() {
        Some(WordClass::Whitespace)
    } else if style == WordStyle::WhitespaceDelimited
        || character.is_alphanumeric()
        || character == '_'
    {
        Some(WordClass::Word)
    } else {
        Some(WordClass::Punctuation)
    }
}

fn atomic_word_class(
    text: &str,
    start: usize,
    end: usize,
    style: WordStyle,
    atomic_byte_ranges: &[Range<usize>],
) -> Option<WordClass> {
    if let Some(index) = atomic_byte_ranges
        .iter()
        .position(|range| range.start == start && range.end == end)
    {
        match style {
            WordStyle::Small => Some(WordClass::Atomic(index)),
            WordStyle::WhitespaceDelimited => Some(WordClass::Word),
        }
    } else {
        word_class(&text[start..end], style)
    }
}

fn normalize_atomic_ranges(text: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut normalized = ranges
        .iter()
        .filter_map(|range| {
            let raw_start = range.start.min(range.end).min(text.len());
            let raw_end = range.start.max(range.end).min(text.len());
            if raw_start == raw_end {
                return None;
            }
            let start = floor_grapheme_boundary(text, raw_start);
            let end = ceil_grapheme_boundary(text, raw_end);
            (start < end).then_some(start..end)
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|range| (range.start, range.end));

    let mut merged: Vec<Range<usize>> = Vec::with_capacity(normalized.len());
    for range in normalized {
        if let Some(previous) = merged.last_mut()
            && range.start < previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn normalize_replacement_range(
    text: &str,
    range: Range<usize>,
    atomic_byte_ranges: &[Range<usize>],
) -> Range<usize> {
    let raw_start = range.start.min(range.end).min(text.len());
    let raw_end = range.start.max(range.end).min(text.len());
    if raw_start == raw_end {
        let cursor = normalize_external_cursor(text, raw_start);
        let cursor = normalize_cursor_for_atomic_ranges(cursor, atomic_byte_ranges);
        return cursor..cursor;
    }

    let mut normalized =
        floor_grapheme_boundary(text, raw_start)..ceil_grapheme_boundary(text, raw_end);
    loop {
        let mut changed = false;
        for atomic in atomic_byte_ranges {
            if atomic.start < normalized.end && atomic.end > normalized.start {
                let start = normalized.start.min(atomic.start);
                let end = normalized.end.max(atomic.end);
                changed |= start != normalized.start || end != normalized.end;
                normalized = start..end;
            }
        }
        if !changed {
            return normalized;
        }
    }
}

fn normalize_cursor_for_atomic_ranges(
    cursor_byte: usize,
    atomic_byte_ranges: &[Range<usize>],
) -> usize {
    let Some(range) = atomic_byte_ranges
        .iter()
        .find(|range| cursor_byte > range.start && cursor_byte < range.end)
    else {
        return cursor_byte;
    };
    if cursor_byte - range.start <= range.end - cursor_byte {
        range.start
    } else {
        range.end
    }
}

fn previous_atomic_boundary(text: &str, byte: usize, atomic_byte_ranges: &[Range<usize>]) -> usize {
    if let Some(range) = atomic_byte_ranges
        .iter()
        .find(|range| byte > range.start && byte <= range.end)
    {
        return range.start;
    }
    let boundary = previous_grapheme_boundary(text, byte);
    atomic_byte_ranges
        .iter()
        .find(|range| boundary > range.start && boundary < range.end)
        .map_or(boundary, |range| range.start)
}

fn next_atomic_boundary(text: &str, byte: usize, atomic_byte_ranges: &[Range<usize>]) -> usize {
    if let Some(range) = atomic_byte_ranges
        .iter()
        .find(|range| byte >= range.start && byte < range.end)
    {
        return range.end;
    }
    let boundary = next_grapheme_boundary(text, byte);
    atomic_byte_ranges
        .iter()
        .find(|range| boundary > range.start && boundary < range.end)
        .map_or(boundary, |range| range.end)
}

fn byte_is_inside_atomic_range(byte: usize, atomic_byte_ranges: &[Range<usize>]) -> bool {
    atomic_byte_ranges
        .iter()
        .any(|range| byte >= range.start && byte < range.end)
}

fn is_grapheme_boundary(text: &str, byte: usize) -> bool {
    byte == text.len()
        || text
            .grapheme_indices(true)
            .any(|(boundary, _)| boundary == byte)
}

fn floor_grapheme_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    if byte == text.len() {
        return byte;
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index <= byte)
        .last()
        .unwrap_or(0)
}

fn ceil_grapheme_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    if byte == text.len() {
        return byte;
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index >= byte)
        .unwrap_or(text.len())
}

fn normalize_external_cursor(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    let before = floor_grapheme_boundary(text, byte);
    let after = ceil_grapheme_boundary(text, byte);
    if byte - before <= after - byte {
        before
    } else {
        after
    }
}

fn previous_grapheme_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    if byte == 0 {
        return 0;
    }
    let mut cursor = GraphemeCursor::new(byte, text.len(), true);
    match cursor.prev_boundary(text, 0) {
        Ok(Some(boundary)) => boundary,
        Ok(None) => 0,
        Err(_) => floor_grapheme_boundary(text, byte.saturating_sub(1)),
    }
}

fn next_grapheme_boundary(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    if byte == text.len() {
        return byte;
    }
    let mut cursor = GraphemeCursor::new(byte, text.len(), true);
    match cursor.next_boundary(text, 0) {
        Ok(Some(boundary)) => boundary,
        Ok(None) => text.len(),
        Err(_) => ceil_grapheme_boundary(text, byte.saturating_add(1)),
    }
}

#[cfg(test)]
#[path = "editor_tests/mod.rs"]
mod tests;
