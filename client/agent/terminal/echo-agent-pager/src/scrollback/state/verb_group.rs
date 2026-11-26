//! Verb-group aggregation: the "Read 10 files, Ran 2 subagents" header
//! label for a folded run of consecutive non-destructive tool calls and
//! subagent lifecycle rows, plus any finished collapsed thoughts the run
//! claims. Also home of the run classification ([`run_step`]) shared by the
//! layout fold, range resolution, and the label walk.
//!
//! The layout pass in `state/layout.rs` detects the runs and marks the header
//! via `EntryLayoutInfo::verb_group_header`; the render loop calls
//! [`verb_group_header_label`] to build the live label each frame (running
//! entries repaint every tick, so tense and counts update in place — no
//! per-call detail churns beside the label while the run executes).
//!
//! The same bucket vocabulary labels group-truncation ("N more") headers:
//! the render loop calls [`truncation_header_label`] with the fold's span,
//! and both walks feed the shared `BucketAccumulator` so the two label
//! families can't drift.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SubagentBlockKind;
use crate::scrollback::blocks::tool::hook::{
    HookRunCounts, render_group_hook_counts_inline_suffix,
};
use crate::scrollback::blocks::tool::{ToolCallBlock, VerbGroupKind};
use crate::scrollback::entry::ScrollbackEntry;
use crate::scrollback::types::DisplayMode;
use crate::theme::Theme;

/// One step of a verb-group run walk.
pub(crate) enum RunStep {
    /// A collapsed verb-groupable tool or subagent entry: joins the run and
    /// counts toward the fold threshold ([`RunScan::folds`]).
    Member(VerbGroupKind),
    /// A finished, collapsed, shown thinking entry: claims into the run
    /// (folds to height 0) but never counts toward the threshold and never
    /// appears in the header label.
    ThoughtMember,
    /// An entry that renders its own rows (or none) without joining or
    /// breaking the run: hidden, streaming, user-opened, or chrome-carrying
    /// thinking, and a manually-opened verb-groupable tool.
    Transparent,
    /// Anything else: ends the run.
    Break,
}

/// Classify one entry for run walking — the single source of truth shared by
/// the layout fold scan, `verb_group_range_of`, and the label walk.
///
/// Members are collapsed verb-groupable tool calls and subagent lifecycle
/// rows; pending-user-input rows stay standalone so their prompt remains
/// visible. Hook-decorated members still join: the group header summarizes
/// their runs while expanded members keep compact per-member suffixes. A
/// manually-opened member is [`RunStep::Transparent`] and keeps its own rows
/// without splitting the run. Thinking never breaks a run: a finished
/// collapsed thought without prompt or hook chrome folds in as
/// [`RunStep::ThoughtMember`]; hidden, still-streaming, opened, or
/// chrome-carrying thinking is transparent.
pub(crate) fn run_step(entry: &ScrollbackEntry, show_thinking: bool) -> RunStep {
    let is_claimable_thinking = entry.display_mode == DisplayMode::Collapsed
        && !entry.is_pending_user_input
        && entry.hook_data.is_none();
    if let RenderBlock::ToolCall(block) = &entry.block
        && let Some(kind) = block.verb_group_kind()
        && !entry.is_pending_user_input
    {
        if entry.display_mode == DisplayMode::Collapsed {
            RunStep::Member(kind)
        } else {
            // A manually-opened member keeps its own rows without splitting
            // the run — same treatment as opened thinking — so toggling a
            // member of an expanded group never dissolves the group.
            RunStep::Transparent
        }
    } else if matches!(entry.block, RenderBlock::Subagent(_)) {
        if entry.display_mode == DisplayMode::Collapsed && !entry.is_pending_user_input {
            RunStep::Member(VerbGroupKind::Subagent)
        } else {
            // Subagent rows are always collapsed single-row entries; prompt
            // chrome keeps them standalone and breaks the run.
            RunStep::Break
        }
    } else if entry.block.is_thinking() {
        if show_thinking && !entry.is_running && is_claimable_thinking {
            RunStep::ThoughtMember
        } else {
            RunStep::Transparent
        }
    } else {
        RunStep::Break
    }
}

/// Whether an in-place block swap changes the entry's verb-group kind (e.g.
/// the eager `Other` placeholder refining into a `Read`). Such swaps change
/// fold membership, so the swap site must mark the entry structurally dirty
/// for the layout fold to catch up on the next frame.
pub(crate) fn verb_group_kind_changed(old: &RenderBlock, new: &RenderBlock) -> bool {
    let kind_of = |block: &RenderBlock| match block {
        RenderBlock::ToolCall(tc) => tc.verb_group_kind(),
        _ => None,
    };
    kind_of(old) != kind_of(new)
}

/// Shape of one forward run walk, as reported by [`scan_run_forward`].
pub(crate) struct RunScan {
    /// Member entries counted (tool calls and subagent rows), including a
    /// member start entry. Thought members claim but never count — the fold
    /// threshold is members-only.
    pub(crate) members: usize,
    /// Exclusive run end: one past the last claimed entry (member or thought
    /// member), so trailing transparent entries stay outside the run.
    pub(crate) end: usize,
    /// Where the walk stopped: the breaking entry's index, or the first index
    /// where `entry_at` returned `None`.
    pub(crate) stop: usize,
}

impl RunScan {
    /// Whether the run folds into a verb-group header row. One member is
    /// enough — the compact label beats the member's own row, and the header
    /// appearing with the first streaming call avoids a fold-in jump when the
    /// second arrives. Members-only: thought members claim into runs but
    /// never count, so a pure-thought run (whose label would be empty) never
    /// folds. The single predicate shared by the layout fold and
    /// `verb_group_range_of` so the two can't drift.
    pub(crate) fn folds(&self) -> bool {
        self.members >= 1
    }
}

/// Walk a run forward from `start` until a breaking entry or the end of the
/// entries, and report the run's shape. Returns `None` when the entry at
/// `start` is missing or cannot anchor a run (members and thought members
/// can; transparent and breaking entries cannot) — anchor eligibility lives
/// in this function's matches, not in caller pre-checks — so a returned scan
/// always has `end > start` and `stop > start` (`members` may be 0 for a
/// thought-anchored walk with no members). The layout fold scan and
/// `verb_group_range_of` share this walk so both agree on the exact run
/// shape; the label walk needs per-member block data and stays its own loop,
/// kept in sync by its exhaustive `RunStep` match.
pub(crate) fn scan_run_forward<'e>(
    entry_at: impl Fn(usize) -> Option<&'e ScrollbackEntry>,
    start: usize,
    show_thinking: bool,
) -> Option<RunScan> {
    // Members and finished thoughts anchor runs; transparent thinking may
    // sit inside one but cannot start one.
    match run_step(entry_at(start)?, show_thinking) {
        RunStep::Member(_) | RunStep::ThoughtMember => {}
        RunStep::Transparent | RunStep::Break => return None,
    }
    let mut members = 0usize;
    let mut end = start;
    let mut i = start;
    while let Some(entry) = entry_at(i) {
        match run_step(entry, show_thinking) {
            RunStep::Member(_) => {
                members += 1;
                end = i + 1;
            }
            RunStep::ThoughtMember => end = i + 1,
            RunStep::Transparent => {}
            RunStep::Break => break,
        }
        i += 1;
    }
    Some(RunScan {
        members,
        end,
        stop: i,
    })
}

/// Aggregated header state for one verb-group run.
pub struct VerbGroupHeaderLabel {
    /// Styled label line rendered on the header row.
    pub line: Line<'static>,
    /// Plain-text label (selection/copy text for the header row).
    pub text: String,
    /// Any member still running (animated accent + present-tense verbs).
    pub running: bool,
    /// Any member or summarized hook failed (error accent).
    pub failed: bool,
}

/// The single channel a group-header row's aggregated label travels
/// through, mirroring the fold families of `groups::GroupKind`. A header
/// row belongs to exactly one fold, so a row carries at most one label —
/// the exclusivity is structural. The variant picks the header chrome
/// (verb-run headers wear run-state accents; truncation headers keep the
/// dimmed fold chrome); the label payload is shared.
pub enum GroupHeaderLabel {
    /// Verb-group run header ("Read 3 files, Searched 2 patterns").
    VerbRun(VerbGroupHeaderLabel),
    /// Labeled truncation ("N more") header ("Ran 6 commands").
    Truncation(VerbGroupHeaderLabel),
}

impl GroupHeaderLabel {
    /// The aggregated label payload, whichever fold family produced it.
    pub fn label(&self) -> &VerbGroupHeaderLabel {
        match self {
            GroupHeaderLabel::VerbRun(label) | GroupHeaderLabel::Truncation(label) => label,
        }
    }
}

/// Per-kind aggregation bucket, ordered by first appearance in the run.
/// Borrows citation strings from the walked blocks (per-frame, no allocation).
struct Bucket<'e> {
    kind: VerbGroupKind,
    calls: usize,
    /// Distinct-count override: when non-empty its size replaces `calls` as
    /// the displayed count. Holds WebSearch citation URLs (distinct result
    /// websites) and subagent child session ids (started + terminal rows of
    /// one subagent count once; a burst of terminal rows counts each
    /// distinct subagent).
    sources: std::collections::HashSet<&'e str>,
}

/// Walk the verb-group run starting at `header_idx` (same [`run_step`] rules
/// as the layout fold: thinking and hidden entries are skipped, anything
/// else ends the run) and build the aggregated label. The label counts
/// members only: folded thoughts contribute nothing here and surface as
/// their own member rows only when the group is expanded.
///
/// `end` is the run's exclusive upper bound in `entries` indices. Callers
/// with the fold's span (see `state::groups`) pass its exact end so the
/// label counts precisely the entries the fold claimed; callers without one
/// pass `entries.len()` and rely on the [`RunStep::Break`] arm, which is
/// kept as the in-bound stop in either case.
pub fn verb_group_header_label(
    entries: &[&ScrollbackEntry],
    header_idx: usize,
    end: usize,
    show_thinking: bool,
    theme: &Theme,
) -> VerbGroupHeaderLabel {
    let mut acc = BucketAccumulator::default();

    let end = end.min(entries.len());
    for &entry in &entries[header_idx.min(end)..end] {
        let kind = match run_step(entry, show_thinking) {
            RunStep::Member(kind) => kind,
            RunStep::Break => break,
            RunStep::ThoughtMember | RunStep::Transparent => continue,
        };
        acc.push(kind, entry, true);
    }

    acc.into_label(theme)
}

/// Aggregated label for a truncation ("N more") header, describing the rows
/// the fold hid — "Ran 6 commands, Read 2 files" — through the same bucket
/// vocabulary as verb-group headers.
///
/// Walks the span's participants (skipping hidden thinking exactly like the
/// fold's projection) from `range.start`, stopping after `limit`
/// participants when given — the collapsed header describes only its hidden
/// prefix; the expanded collapse header passes `None` and describes the
/// whole run. Thoughts occupy participant slots but are NEVER bucketed:
/// like verb-group labels, group labels stay tools-only. Returns `None` —
/// the caller keeps the plain "N more" count — when nothing was bucketed (a
/// pure-thought prefix) or when any walked participant has no bucket
/// (System/SessionEvent rows, lifecycle chrome): thoughts are the only
/// participants a label may silently omit, anything else would make it
/// under-describe what the fold conceals.
pub fn truncation_header_label(
    entries: &[&ScrollbackEntry],
    range: std::ops::Range<usize>,
    limit: Option<usize>,
    show_thinking: bool,
    theme: &Theme,
) -> Option<VerbGroupHeaderLabel> {
    let mut acc = BucketAccumulator::default();
    let end = range.end.min(entries.len());
    let mut participants = 0usize;

    for &entry in &entries[range.start.min(end)..end] {
        if limit.is_some_and(|n| participants >= n) {
            break;
        }
        if entry.is_hidden_thinking(show_thinking) {
            continue;
        }
        participants += 1;
        if entry.block.is_thinking() {
            continue;
        }
        match &entry.block {
            RenderBlock::ToolCall(block) => acc.push(block.label_kind()?, entry, false),
            RenderBlock::Subagent(_) => acc.push(VerbGroupKind::Subagent, entry, false),
            // A participant the vocabulary can't name would leave the label
            // dishonest about what's hidden; decline so the numerically
            // exact plain count renders instead.
            _ => return None,
        }
    }

    if acc.is_empty() {
        return None;
    }
    Some(acc.into_label(theme))
}

/// Shared bucket accumulation + label rendering for the aggregated group
/// headers. Callers own the walk (which entries join and under what
/// classification); this owns per-kind counting, distinct-source overrides,
/// tool/hook outcome counting, and the rendered line.
#[derive(Default)]
struct BucketAccumulator<'e> {
    buckets: Vec<Bucket<'e>>,
    running: bool,
    failed_count: usize,
    hook_counts: HookRunCounts,
}

impl<'e> BucketAccumulator<'e> {
    fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    fn push(&mut self, kind: VerbGroupKind, entry: &'e ScrollbackEntry, include_hook_counts: bool) {
        let pos = match self.buckets.iter().position(|b| b.kind == kind) {
            Some(pos) => pos,
            None => {
                self.buckets.push(Bucket {
                    kind,
                    calls: 0,
                    sources: std::collections::HashSet::new(),
                });
                self.buckets.len() - 1
            }
        };
        let bucket = &mut self.buckets[pos];
        bucket.calls += 1;
        // Bucketed entries are tool-call or subagent rows by construction
        // (both walks); the block feeds the distinct-count override and
        // failure detection.
        match &entry.block {
            RenderBlock::ToolCall(block) => {
                if let ToolCallBlock::WebSearch(b) = block
                    && b.is_success()
                {
                    bucket
                        .sources
                        .extend(b.citations.iter().map(String::as_str));
                }
                if block_failed(block) {
                    self.failed_count += 1;
                }
            }
            RenderBlock::Subagent(sb) => {
                bucket.sources.insert(sb.child_session_id.as_str());
                // Cancelled is deliberate, not an error — only Failed feeds
                // the red suffix.
                if matches!(sb.kind, SubagentBlockKind::Failed { .. }) {
                    self.failed_count += 1;
                }
            }
            // Unreachable today; release keeps the generic count so the
            // label can't desync from the fold that claimed the entry.
            _ => debug_assert!(false, "bucketed entry has a block with no label-extras arm"),
        }

        if include_hook_counts && let Some(hook_data) = &entry.hook_data {
            self.hook_counts.add_data(hook_data);
        }
        if entry.is_running {
            self.running = true;
        }
    }

    fn into_label(self, theme: &Theme) -> VerbGroupHeaderLabel {
        let text_style = theme.fg(theme.gray_bright).add_modifier(Modifier::BOLD);
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut text = String::new();
        for (i, bucket) in self.buckets.iter().enumerate() {
            let count = if bucket.sources.is_empty() {
                bucket.calls
            } else {
                bucket.sources.len()
            };
            let segment = format!(
                "{}{} {} {}",
                if i == 0 { "" } else { ", " },
                bucket.kind.verb(self.running),
                count,
                bucket.kind.noun(count)
            );
            text.push_str(&segment);
            spans.push(Span::styled(segment, text_style));
        }
        if self.failed_count > 0 {
            let suffix = format!(" · {} failed", self.failed_count);
            text.push_str(&suffix);
            spans.push(Span::styled(suffix, theme.fg(theme.accent_error)));
        }
        if let Some(hook_spans) = render_group_hook_counts_inline_suffix(self.hook_counts, theme) {
            for span in &hook_spans {
                text.push_str(span.content.as_ref());
            }
            spans.extend(hook_spans);
        }

        VerbGroupHeaderLabel {
            line: Line::from(spans),
            text,
            running: self.running,
            failed: self.failed_count > 0 || self.hook_counts.has_failures(),
        }
    }
}

/// Whether a bucketed block completed with an error. Variants are listed
/// explicitly so a new `ToolCallBlock` variant must decide here too. The
/// action kinds reach labels only through truncation buckets (verb folds
/// exclude them), where their failures count like any other member's.
fn block_failed(block: &ToolCallBlock) -> bool {
    match block {
        ToolCallBlock::Read(b) => !b.is_success(),
        ToolCallBlock::ListDir(b) => !b.is_success(),
        ToolCallBlock::Search(b) => !b.is_success(),
        ToolCallBlock::WebFetch(b) => !b.is_success(),
        ToolCallBlock::WebSearch(b) => !b.is_success(),
        ToolCallBlock::MemorySearch(b) => !b.is_success(),
        ToolCallBlock::IntegrationSearch(b) => !b.is_success(),
        ToolCallBlock::Skill(b) => !b.is_success(),
        ToolCallBlock::Execute(b) => !b.is_success(),
        ToolCallBlock::Edit(b) => !b.is_success(),
        ToolCallBlock::UseTool(b) => !b.is_success(),
        ToolCallBlock::Other(b) => !b.is_success(),
        ToolCallBlock::Lifecycle(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::blocks::SubagentBlock;
    use crate::scrollback::blocks::tool::{
        HookRunEntry, HookRunStatus, ListDirToolCallBlock, ReadToolCallBlock, SearchToolCallBlock,
        ToolCallHookData, WebSearchToolCallBlock,
    };

    fn entry(block: ToolCallBlock) -> ScrollbackEntry {
        ScrollbackEntry::new(RenderBlock::ToolCall(block)).with_display_mode(DisplayMode::Collapsed)
    }

    fn read(path: &str) -> ScrollbackEntry {
        entry(ToolCallBlock::Read(ReadToolCallBlock::new(path)))
    }

    fn hook(name: &str, status: HookRunStatus) -> HookRunEntry {
        HookRunEntry {
            name: name.to_owned(),
            status,
            output: None,
        }
    }

    fn hooked(mut entry: ScrollbackEntry, post_hooks: Vec<HookRunEntry>) -> ScrollbackEntry {
        entry.hook_data = Some(ToolCallHookData {
            post_hooks,
            ..ToolCallHookData::default()
        });
        entry
    }

    fn subagent(block: SubagentBlock) -> ScrollbackEntry {
        ScrollbackEntry::new(RenderBlock::Subagent(block))
    }

    fn sub_started(child_sid: &str) -> ScrollbackEntry {
        subagent(SubagentBlock::started(
            "task", child_sid, "explore", None, None, None, /*is_background=*/ true,
        ))
    }

    fn sub_completed(child_sid: &str) -> ScrollbackEntry {
        subagent(SubagentBlock::completed(
            "task",
            child_sid,
            std::time::Duration::from_secs(3),
        ))
    }

    fn label(entries: &[ScrollbackEntry]) -> VerbGroupHeaderLabel {
        let refs: Vec<&ScrollbackEntry> = entries.iter().collect();
        verb_group_header_label(
            &refs,
            0,
            refs.len(),
            /*show_thinking=*/ true,
            &Theme::current(),
        )
    }

    #[test]
    fn buckets_in_first_appearance_order_with_plurality() {
        let entries = vec![
            read("a.rs"),
            entry(ToolCallBlock::Search(SearchToolCallBlock::new("todo"))),
            read("b.rs"),
            entry(ToolCallBlock::ListDir(ListDirToolCallBlock::new("src"))),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Read 2 files, Searched 1 pattern, Listed 1 dir");
        assert!(!l.running);
        assert!(!l.failed);
    }

    #[test]
    fn skill_reads_bucket_separately_from_files() {
        let entries = vec![
            read("a.rs"),
            read("/x/skills/deploy/SKILL.md"),
            read("b.rs"),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Read 2 files, Read 1 skill");
    }

    #[test]
    fn failed_members_append_suffix_and_flag() {
        let entries = vec![
            read("a.rs"),
            entry(ToolCallBlock::Read(
                ReadToolCallBlock::new("gone.rs").with_error("no such file"),
            )),
            entry(ToolCallBlock::Read(
                ReadToolCallBlock::new("also-gone.rs").with_error("no such file"),
            )),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Read 3 files · 2 failed");
        assert!(l.failed);
    }

    #[test]
    fn hooked_members_aggregate_non_skipped_outcomes_once() {
        let elapsed = std::time::Duration::from_millis(1);
        let entries = vec![
            hooked(
                read("a.rs"),
                vec![
                    hook("ok", HookRunStatus::Success { elapsed }),
                    hook("skip", HookRunStatus::Skipped),
                ],
            ),
            hooked(
                read("b.rs"),
                vec![hook(
                    "blocked",
                    HookRunStatus::Blocked {
                        detail: "denied".to_owned(),
                        elapsed,
                    },
                )],
            ),
            hooked(
                read("c.rs"),
                vec![hook(
                    "bad",
                    HookRunStatus::Failed {
                        error: "exit 1".to_owned(),
                        elapsed,
                    },
                )],
            ),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Read 3 files  [hooks: 1 ok, 1 blocked, 1 failed]");
        assert!(l.failed, "failed hooks give the group error accent");
        let dimmed = Modifier::DIM;
        assert_eq!(
            l.line.spans[2].style.fg,
            Some(Theme::current().accent_success)
        );
        assert!(l.line.spans[2].style.add_modifier.contains(dimmed));
        assert_eq!(
            l.line.spans[4].style.fg,
            Some(Theme::current().accent_running)
        );
        assert_eq!(
            l.line.spans[6].style.fg,
            Some(Theme::current().accent_error)
        );
    }

    #[test]
    fn running_flips_tense_only() {
        let mut entries = vec![
            read("a.rs"),
            entry(ToolCallBlock::Search(SearchToolCallBlock::new("todo"))),
        ];
        entries[1].is_running = true;
        let l = label(&entries);
        assert_eq!(l.text, "Reading 1 file, Searching 1 pattern");
        assert!(l.running);

        entries[1].is_running = false;
        let l = label(&entries);
        assert_eq!(l.text, "Read 1 file, Searched 1 pattern");
        assert!(!l.running);
    }

    #[test]
    fn web_search_counts_distinct_sources_with_call_fallback() {
        let searched = |query: &str, citations: &[&str]| {
            let mut b = WebSearchToolCallBlock::new(query);
            b.citations = citations.iter().map(|s| s.to_string()).collect();
            b.content = Some("results".into());
            entry(ToolCallBlock::WebSearch(b))
        };
        // Three distinct URLs across two searches, one duplicated.
        let entries = vec![
            searched("echoagent", &["https://a.com", "https://b.com"]),
            searched("pager", &["https://b.com", "https://c.com"]),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Searched 3 websites");

        // No citations yet (still running / no results): fall back to call count.
        let entries = vec![
            entry(ToolCallBlock::WebSearch(WebSearchToolCallBlock::new("a"))),
            entry(ToolCallBlock::WebSearch(WebSearchToolCallBlock::new("b"))),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Searched 2 websites");
    }

    #[test]
    fn run_ends_at_separator_and_skips_hidden_thinking() {
        let entries = [
            read("a.rs"),
            ScrollbackEntry::new(RenderBlock::thinking("hmm")),
            read("b.rs"),
            ScrollbackEntry::new(RenderBlock::execute("ls")),
            read("c.rs"),
        ];
        let refs: Vec<&ScrollbackEntry> = entries.iter().collect();
        let l = verb_group_header_label(
            &refs,
            0,
            refs.len(),
            /*show_thinking=*/ false,
            &Theme::current(),
        );
        assert_eq!(l.text, "Read 2 files");
    }

    #[test]
    fn label_stays_tools_only_across_shown_thinking_states() {
        let mut streaming = ScrollbackEntry::new(RenderBlock::thinking("live"));
        streaming.is_running = true;
        let entries = [
            read("a.rs"),
            // Finished + collapsed: folds into the run, never labeled.
            ScrollbackEntry::new(RenderBlock::thinking("done"))
                .with_display_mode(DisplayMode::Collapsed),
            read("b.rs"),
            // Streaming: transparent, keeps its own live panel.
            streaming,
            read("c.rs"),
            // User-opened: transparent, keeps its own rows.
            ScrollbackEntry::new(RenderBlock::thinking("opened"))
                .with_display_mode(DisplayMode::Expanded),
            read("d.rs"),
            // Non-thinking separators still end the run.
            ScrollbackEntry::new(RenderBlock::execute("ls")),
            read("e.rs"),
        ];
        let refs: Vec<&ScrollbackEntry> = entries.iter().collect();
        let l = verb_group_header_label(
            &refs,
            0,
            refs.len(),
            /*show_thinking=*/ true,
            &Theme::current(),
        );
        assert_eq!(l.text, "Read 4 files");
    }

    fn execute() -> ScrollbackEntry {
        ScrollbackEntry::new(RenderBlock::execute("ls")).with_display_mode(DisplayMode::Collapsed)
    }

    fn thought() -> ScrollbackEntry {
        ScrollbackEntry::new(RenderBlock::thinking("hmm")).with_display_mode(DisplayMode::Collapsed)
    }

    fn trunc_label(
        entries: &[ScrollbackEntry],
        limit: Option<usize>,
    ) -> Option<VerbGroupHeaderLabel> {
        let refs: Vec<&ScrollbackEntry> = entries.iter().collect();
        truncation_header_label(
            &refs,
            0..refs.len(),
            limit,
            /*show_thinking=*/ true,
            &Theme::current(),
        )
    }

    #[test]
    fn truncation_label_buckets_commands_and_never_thoughts() {
        // 3 commands + 2 thoughts hidden: thoughts occupy participant slots
        // but the label stays tools-only.
        let entries = vec![execute(), thought(), execute(), thought(), execute()];
        let l = trunc_label(&entries, None).expect("commands bucket");
        assert_eq!(l.text, "Ran 3 commands");
    }

    #[test]
    fn truncation_label_limit_counts_participants_not_buckets() {
        // limit=3 covers [execute, thought, execute]: the thought consumes a
        // participant slot without appearing in the label.
        let entries = vec![execute(), thought(), execute(), execute(), execute()];
        let l = trunc_label(&entries, Some(3)).expect("prefix buckets");
        assert_eq!(l.text, "Ran 2 commands");
    }

    #[test]
    fn truncation_label_mixes_kinds_in_first_appearance_order() {
        let entries = vec![
            execute(),
            read("a.rs"),
            ScrollbackEntry::new(RenderBlock::edit("src/main.rs", None))
                .with_display_mode(DisplayMode::Collapsed),
            execute(),
        ];
        let l = trunc_label(&entries, None).expect("buckets");
        assert_eq!(l.text, "Ran 2 commands, Read 1 file, Edited 1 file");
    }

    #[test]
    fn truncation_label_none_for_pure_thought_prefix() {
        let entries = vec![thought(), thought(), execute()];
        assert!(
            trunc_label(&entries, Some(2)).is_none(),
            "a prefix of only thoughts buckets nothing; caller falls back to 'N more'"
        );
    }

    #[test]
    fn truncation_label_none_for_prefix_with_unbucketable_rows() {
        let system = ScrollbackEntry::new(RenderBlock::system("hook ran"))
            .with_display_mode(DisplayMode::Collapsed);
        let entries = vec![execute(), system, execute()];
        assert!(
            trunc_label(&entries, None).is_none(),
            "a hidden System row has no bucket; the plain count stays numerically honest"
        );
        // The unbucketable row past the limit never walks: the prefix labels.
        let entries = vec![
            execute(),
            ScrollbackEntry::new(RenderBlock::system("hook ran"))
                .with_display_mode(DisplayMode::Collapsed),
        ];
        let l = trunc_label(&entries, Some(1)).expect("prefix buckets");
        assert_eq!(l.text, "Ran 1 command");
    }

    #[test]
    fn truncation_label_counts_failed_commands() {
        let failed = RenderBlock::execute_with_output("false", "", Some("exit 1"));
        let entries = vec![
            execute(),
            ScrollbackEntry::new(failed).with_display_mode(DisplayMode::Collapsed),
        ];
        let l = trunc_label(&entries, None).expect("buckets");
        assert_eq!(l.text, "Ran 2 commands · 1 failed");
        assert!(l.failed);
    }

    #[test]
    fn truncation_label_skips_hidden_thinking_without_consuming_limit() {
        let mut hidden_thought = ScrollbackEntry::new(RenderBlock::thinking("hidden"));
        hidden_thought.set_display_mode(DisplayMode::Collapsed);
        let entries = [execute(), hidden_thought, execute()];
        let refs: Vec<&ScrollbackEntry> = entries.iter().collect();
        // show_thinking=false: the thought is hidden chrome, not a
        // participant — both commands fit in a limit of 2.
        let l = truncation_header_label(&refs, 0..refs.len(), Some(2), false, &Theme::current())
            .expect("buckets");
        assert_eq!(l.text, "Ran 2 commands");
    }

    #[test]
    fn subagent_rows_bucket_with_tools_and_count_distinct_subagents() {
        // A background subagent leaves BOTH its started row and a terminal
        // row in the run; the child-session-id source override collapses
        // them to one displayed subagent.
        let entries = vec![
            read("a.rs"),
            sub_started("child-A"),
            read("b.rs"),
            sub_completed("child-A"),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Read 2 files, Ran 1 subagent");
        assert!(!l.failed);
    }

    #[test]
    fn subagent_completion_burst_counts_each_subagent() {
        let entries = vec![sub_completed("child-A"), sub_completed("child-B")];
        let l = label(&entries);
        assert_eq!(l.text, "Ran 2 subagents");
    }

    #[test]
    fn subagent_failed_feeds_suffix_cancelled_does_not() {
        let entries = vec![
            subagent(SubagentBlock::failed(
                "task",
                "child-A",
                std::time::Duration::from_secs(3),
                Some("boom".into()),
            )),
            subagent(SubagentBlock::cancelled(
                "task",
                "child-B",
                std::time::Duration::from_secs(3),
            )),
        ];
        let l = label(&entries);
        assert_eq!(l.text, "Ran 2 subagents · 1 failed");
        assert!(l.failed);
    }

    #[test]
    fn running_subagent_flips_group_tense() {
        let mut entries = vec![read("a.rs"), sub_started("child-A")];
        entries[1].is_running = true;
        let l = label(&entries);
        assert_eq!(l.text, "Reading 1 file, Running 1 subagent");
        assert!(l.running);
    }
}
