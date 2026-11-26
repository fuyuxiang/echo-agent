//! Derived group model for the scrollback's view-time folds.
//!
//! One scan owns every grouping decision: verb-group runs claim their
//! entries first, then group truncation ("N more") runs over the rest. The
//! scan produces [`GroupSpan`]s — the authoritative description of every
//! fold — and [`project_to_layout`] is the single writer that turns spans
//! into the per-entry `EntryLayoutInfo` flags the renderer and navigation
//! consume. Keeping the decision (scan) and the flag writes (projection)
//! in one module means a consumer can never observe a fold shape the model
//! doesn't describe.
//!
//! The spans are stored on the layout cache (see `LayoutCache::groups`) and
//! rebuilt whenever the folds are re-applied. Like the per-entry flags, they
//! go stale between an incremental entry append and the next structural
//! rebuild (`gaps_may_be_dirty` covers both).
//!
//! Per-entry run classification ([`run_step`]) and the rendered header label
//! stay in [`super::verb_group`]; this module owns run *shapes*.

use std::collections::HashSet;
use std::ops::Range;

use indexmap::IndexMap;

use super::types::EntryLayoutInfo;
use super::verb_group::{RunStep, run_step, scan_run_forward};
use crate::scrollback::block::BlockContent;
use crate::scrollback::entry::{EntryId, ScrollbackEntry};
use crate::scrollback::types::DisplayMode;

/// One folded region of the transcript, in entry indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpan {
    /// Entries the fold walked. For verb runs this ends one past the last
    /// claimed entry (trailing transparent entries stay outside). For
    /// truncation it is the whole dense run, visible tail included, and may
    /// end with trailing hidden-thinking entries the walk skipped over.
    pub range: Range<usize>,
    /// Which fold produced this span and its count data.
    pub kind: GroupKind,
    /// Whether the user manually expanded this group (keyed by the first
    /// entry's ID in `ScrollbackState::expanded_groups`).
    pub expanded: bool,
}

/// The two fold families. Both render a synthetic header row; they differ in
/// when they fold and what the header says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// Eagerly folded run of verb-groupable members (tool calls and subagent
    /// rows) — the aggregated "Read 2 skills" header. `members` counts
    /// label-bearing members only; claimed thoughts fold in but never count.
    VerbRun { members: usize },
    /// Budget truncation of an over-long dense run — the "N more" header.
    /// `participants` counts the entries eligible to hide (hidden thinking
    /// excluded); `hidden` is how many of them the collapsed state conceals
    /// (`participants - max_visible`, > 0 by the fold gate). The header row
    /// is the first hidden participant, so its plain count shows
    /// `hidden - 1` while its aggregated label describes all `hidden`.
    Truncation { participants: usize, hidden: usize },
}

/// Binary-search sorted, disjoint spans for the one containing `idx`.
pub fn span_containing(spans: &[GroupSpan], idx: usize) -> Option<&GroupSpan> {
    let pos = spans.partition_point(|s| s.range.end <= idx);
    spans.get(pos).filter(|s| s.range.contains(&idx))
}

/// Reset every entry's group flags, scan for spans, and project them onto
/// the layout slice. Returns the spans for the caller to store on the
/// layout cache. Reads the `group_tool_verbs` / `show_thinking_blocks`
/// settings once so both fold families and the projection agree within a
/// rebuild.
pub(super) fn apply(
    entries: &IndexMap<EntryId, ScrollbackEntry>,
    layout_cache: &mut [EntryLayoutInfo],
    max_visible: usize,
    expanded_groups: &HashSet<EntryId>,
) -> Vec<GroupSpan> {
    for info in layout_cache.iter_mut() {
        if info.is_expanded_verb_header() {
            info.height = info.height.saturating_sub(1);
        }
        info.group_header_count = 0;
        info.group_collapse_header = false;
        info.verb_group_header = false;
    }
    let group_tool_verbs = crate::appearance::cache::load_group_tool_verbs();
    let show_thinking = crate::appearance::cache::load_show_thinking_blocks();
    let spans = scan(
        entries,
        max_visible,
        expanded_groups,
        group_tool_verbs,
        show_thinking,
    );
    project_to_layout(&spans, entries, layout_cache, show_thinking);
    spans
}

/// Scan the transcript for every fold, in the order the folds take
/// precedence: verb runs claim entries first, truncation runs over the
/// rest (claimed entries break truncation runs). Returns spans sorted by
/// start index; spans never overlap.
pub(super) fn scan(
    entries: &IndexMap<EntryId, ScrollbackEntry>,
    max_visible: usize,
    expanded_groups: &HashSet<EntryId>,
    group_tool_verbs: bool,
    show_thinking: bool,
) -> Vec<GroupSpan> {
    let (mut spans, claimed) =
        scan_verb_runs(entries, expanded_groups, group_tool_verbs, show_thinking);
    spans.extend(scan_truncations(
        entries,
        max_visible,
        expanded_groups,
        show_thinking,
        &claimed,
    ));
    // Both scans emit in ascending order over disjoint ranges; interleave.
    spans.sort_unstable_by_key(|s| s.range.start);
    spans
}

/// Find maximal runs of verb-groupable member entries — plus any finished
/// collapsed thoughts among them — that fold per `RunScan::folds`, gated on
/// the `group_tool_verbs` setting. Also returns the claimed-entry mask
/// (members and thought members of folding runs): the truncation scan
/// treats claimed entries as run breakers.
fn scan_verb_runs(
    entries: &IndexMap<EntryId, ScrollbackEntry>,
    expanded_groups: &HashSet<EntryId>,
    group_tool_verbs: bool,
    show_thinking: bool,
) -> (Vec<GroupSpan>, Vec<bool>) {
    let n = entries.len();
    let mut spans = Vec::new();
    let mut claimed = vec![false; n];
    if n == 0 || !group_tool_verbs {
        return (spans, claimed);
    }

    let entry_at = |i: usize| entries.get_index(i).map(|(_, e)| e);
    let mut i = 0;
    while i < n {
        // Trailing transparent thinking stays outside the run (`scan.end`).
        let Some(scan) = scan_run_forward(entry_at, i, show_thinking) else {
            i += 1;
            continue;
        };
        if !scan.folds() {
            i = scan.stop;
            continue;
        }

        // Which in-run entries claim must agree with the member arms in
        // `scan_run_forward`; transparent entries stay unclaimed inside the
        // span and keep rendering their own rows.
        for (offset, slot) in claimed[i..scan.end].iter_mut().enumerate() {
            if matches!(
                run_step(
                    entry_at(i + offset).expect("index within entries"),
                    show_thinking
                ),
                RunStep::Member(_) | RunStep::ThoughtMember
            ) {
                *slot = true;
            }
        }

        let first_id = *entries.get_index(i).expect("index within entries").0;
        spans.push(GroupSpan {
            range: i..scan.end,
            kind: GroupKind::VerbRun {
                members: scan.members,
            },
            expanded: expanded_groups.contains(&first_id),
        });
        i = scan.end;
    }
    (spans, claimed)
}

/// Collapsed+groupable entries that may join a truncation run.
/// Hidden thinking is excluded so tools elect their own "N more" header.
fn participates_in_truncation(entry: &ScrollbackEntry, show_thinking: bool) -> bool {
    entry.block.is_groupable()
        && entry.display_mode == DisplayMode::Collapsed
        && !entry.is_hidden_thinking(show_thinking)
}

/// Find consecutive runs of collapsed+groupable entries longer than
/// `max_visible + 1`. Hidden thinking is transparent (skipped, not a
/// run-breaker), mirroring the gap rule in `recompute_gap_after`, so an
/// interspersed thought can't split a run and suppress truncation. Entries
/// claimed by the verb scan break runs.
fn scan_truncations(
    entries: &IndexMap<EntryId, ScrollbackEntry>,
    max_visible: usize,
    expanded_groups: &HashSet<EntryId>,
    show_thinking: bool,
    claimed: &[bool],
) -> Vec<GroupSpan> {
    let mut spans = Vec::new();
    if max_visible == 0 || entries.is_empty() {
        return spans;
    }

    let n = entries.len();
    let mut i = 0;
    while i < n {
        let (_, entry) = entries.get_index(i).unwrap();
        if claimed[i] || !participates_in_truncation(entry, show_thinking) {
            i += 1;
            continue;
        }

        let group_start = i;
        let mut group_len = 1;
        let mut j = i + 1;
        while j < n {
            let (_, e) = entries.get_index(j).unwrap();
            if claimed[j] {
                break;
            }
            if participates_in_truncation(e, show_thinking) {
                group_len += 1;
            } else if !e.is_hidden_thinking(show_thinking) {
                break;
            }
            j += 1;
        }
        let group_end = j;

        if group_len <= max_visible + 1 {
            i = group_end;
            continue;
        }

        let first_id = *entries.get_index(group_start).unwrap().0;
        spans.push(GroupSpan {
            range: group_start..group_end,
            kind: GroupKind::Truncation {
                participants: group_len,
                hidden: group_len - max_visible,
            },
            expanded: expanded_groups.contains(&first_id),
        });
        i = group_end;
    }
    spans
}

/// The single writer of group heights, gaps, and header flags. Every layout
/// consequence of a fold happens here, driven only by the spans (plus
/// per-entry `run_step` classification for verb runs, whose transparent
/// entries keep their own rows).
pub(super) fn project_to_layout(
    spans: &[GroupSpan],
    entries: &IndexMap<EntryId, ScrollbackEntry>,
    layout_cache: &mut [EntryLayoutInfo],
    show_thinking: bool,
) {
    for span in spans {
        match span.kind {
            GroupKind::VerbRun { members } => {
                project_verb_run(span, members, entries, layout_cache, show_thinking);
            }
            GroupKind::Truncation {
                participants,
                hidden,
            } => {
                project_truncation(
                    span,
                    participants,
                    hidden,
                    entries,
                    layout_cache,
                    show_thinking,
                );
            }
        }
    }
}

/// Collapsed: the header is `height=1`; other claimed entries are `height=0`.
/// Expanded: every member keeps its measured rows while the header stacks
/// above member 0. Gaps zero only within the run; the last claimed entry keeps
/// the boundary gap to the following entry. Transparent entries keep their
/// rows but donate their gap while collapsed.
fn project_verb_run(
    span: &GroupSpan,
    members: usize,
    entries: &IndexMap<EntryId, ScrollbackEntry>,
    layout_cache: &mut [EntryLayoutInfo],
    show_thinking: bool,
) {
    let last_claimed = span.range.end - 1;
    for idx in span.range.clone() {
        let (_, e) = entries.get_index(idx).unwrap();
        let cached = &mut layout_cache[idx];
        match run_step(e, show_thinking) {
            RunStep::Member(_) | RunStep::ThoughtMember => {}
            RunStep::Transparent => {
                if !span.expanded {
                    cached.gap_after = 0;
                }
                continue;
            }
            // Unreachable: a span's range never spans a Break (arm kept for
            // match exhaustiveness).
            RunStep::Break => continue,
        }
        if idx == span.range.start {
            cached.verb_group_header = true;
            cached.group_collapse_header = span.expanded;
            cached.group_header_count = members.min(u16::MAX as usize) as u16;
            if span.expanded {
                cached.height = cached.with_verb_header_row(cached.height);
            } else {
                cached.height = 1;
            }
            // A singleton run's header is also its last claimed entry: it
            // keeps the pairwise boundary gap, else the header glues to what
            // follows.
            if idx != last_claimed {
                cached.gap_after = 0;
            }
        } else if !span.expanded {
            cached.height = 0;
            if idx != last_claimed {
                cached.gap_after = 0;
            }
        }
    }
}

/// Collapsed: the first participating entry becomes the "N more" header
/// (count excludes the header itself), older participants hide, and the
/// last `max_visible` stay untouched. Expanded: entry 0 becomes a
/// standalone collapse header (`height=1`, content replaced) counting the
/// `participants - 1` entries below it, which all keep their own rows.
/// Hidden thinking is skipped in both states.
/// `verb_group::truncation_header_label` mirrors this walk's participant
/// rule for the header's aggregated label; a new transparency category here
/// must update that walk too.
fn project_truncation(
    span: &GroupSpan,
    participants: usize,
    hidden: usize,
    entries: &IndexMap<EntryId, ScrollbackEntry>,
    layout_cache: &mut [EntryLayoutInfo],
    show_thinking: bool,
) {
    if span.expanded {
        let cached = &mut layout_cache[span.range.start];
        cached.group_collapse_header = true;
        cached.group_header_count = (participants - 1).min(u16::MAX as usize) as u16;
        cached.height = 1;
        cached.gap_after = 0;
        return;
    }

    let mut seen = 0;
    for idx in span.range.clone() {
        let (_, e) = entries.get_index(idx).unwrap();
        if e.is_hidden_thinking(show_thinking) {
            continue;
        }
        let cached = &mut layout_cache[idx];
        if seen == 0 {
            cached.height = 1;
            cached.gap_after = 0;
            cached.group_header_count = (hidden - 1).min(u16::MAX as usize) as u16;
        } else if seen < hidden {
            cached.height = 0;
            cached.gap_after = 0;
            cached.group_header_count = 0;
        }
        seen += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::blocks::tool::{ReadToolCallBlock, ToolCallBlock};

    fn skill_read() -> ScrollbackEntry {
        ScrollbackEntry::new(RenderBlock::ToolCall(ToolCallBlock::Read(
            ReadToolCallBlock::new("/x/skills/deploy/SKILL.md"),
        )))
        .with_display_mode(DisplayMode::Collapsed)
    }

    fn execute() -> ScrollbackEntry {
        ScrollbackEntry::new(RenderBlock::execute("ls")).with_display_mode(DisplayMode::Collapsed)
    }

    fn thought() -> ScrollbackEntry {
        ScrollbackEntry::new(RenderBlock::thinking("hmm")).with_display_mode(DisplayMode::Collapsed)
    }

    fn map(entries: Vec<ScrollbackEntry>) -> IndexMap<EntryId, ScrollbackEntry> {
        entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| (EntryId::new(i as u64), e))
            .collect()
    }

    /// Seed layout distinct from anything the projection writes, so a
    /// changed entry is distinguishable from an untouched one.
    fn seeded_layout(n: usize) -> Vec<EntryLayoutInfo> {
        vec![
            EntryLayoutInfo {
                height: 5,
                gap_after: 1,
                ..Default::default()
            };
            n
        ]
    }

    fn scan_and_project(
        entries: &IndexMap<EntryId, ScrollbackEntry>,
        layout: &mut [EntryLayoutInfo],
        max_visible: usize,
        expanded: &HashSet<EntryId>,
    ) -> Vec<GroupSpan> {
        let spans = scan(entries, max_visible, expanded, true, true);
        project_to_layout(&spans, entries, layout, true);
        spans
    }

    /// The "Read 2 skills / 8 more" transcript shape: a verb run followed by
    /// a 19-row dense run of commands and thoughts with the default budget.
    #[test]
    fn verb_run_breaks_truncation_and_both_spans_project() {
        let mut list = vec![skill_read(), skill_read()];
        for i in 0..19 {
            list.push(if i % 3 == 2 { thought() } else { execute() });
        }
        let entries = map(list);
        let mut layout = seeded_layout(entries.len());
        let spans = scan_and_project(&entries, &mut layout, 10, &HashSet::new());

        assert_eq!(
            spans,
            vec![
                GroupSpan {
                    range: 0..2,
                    kind: GroupKind::VerbRun { members: 2 },
                    expanded: false,
                },
                GroupSpan {
                    range: 2..21,
                    kind: GroupKind::Truncation {
                        participants: 19,
                        hidden: 9,
                    },
                    expanded: false,
                },
            ]
        );

        // Verb header row plus its folded member.
        assert!(layout[0].verb_group_header);
        assert_eq!(layout[0].group_header_count, 2);
        assert_eq!(layout[0].height, 1);
        assert_eq!(layout[1].height, 0);

        // Truncation header reads "8 more" and hides the 8 rows behind it.
        assert!(!layout[2].verb_group_header);
        assert_eq!(layout[2].group_header_count, 8);
        assert_eq!(layout[2].height, 1);
        for info in &layout[3..11] {
            assert_eq!((info.height, info.group_header_count), (0, 0));
        }
        // The newest `max_visible` rows keep their seeded layout.
        for info in &layout[11..21] {
            assert_eq!((info.height, info.gap_after), (5, 1));
        }
    }

    #[test]
    fn runs_at_or_under_budget_produce_no_truncation_span() {
        let entries = map((0..11).map(|_| execute()).collect());
        let mut layout = seeded_layout(entries.len());
        let spans = scan_and_project(&entries, &mut layout, 10, &HashSet::new());
        assert!(spans.is_empty());
        assert!(layout.iter().all(|i| i.height == 5));
    }

    #[test]
    fn verb_toggle_off_feeds_members_to_truncation() {
        let mut list = vec![skill_read(), skill_read()];
        list.extend((0..12).map(|_| execute()));
        let entries = map(list);
        let spans = scan(&entries, 10, &HashSet::new(), false, true);
        assert_eq!(
            spans,
            vec![GroupSpan {
                range: 0..14,
                kind: GroupKind::Truncation {
                    participants: 14,
                    hidden: 4,
                },
                expanded: false,
            }]
        );
    }

    #[test]
    fn expanded_verb_run_stacks_header_and_keeps_member_rows() {
        let entries = map(vec![skill_read(), skill_read()]);
        let mut layout = seeded_layout(entries.len());
        let expanded: HashSet<EntryId> = [EntryId::new(0)].into();
        let spans = apply(&entries, &mut layout, 10, &expanded);
        assert!(spans[0].expanded);
        assert!(layout[0].group_collapse_header);
        assert_eq!(layout[0].height, 6, "header stacks above measured rows");
        assert_eq!(layout[1].height, 5, "expanded members keep their rows");

        apply(&entries, &mut layout, 10, &expanded);
        assert_eq!(layout[0].height, 6, "reapplying the fold is idempotent");
        assert_eq!(layout[1].height, 5, "member height stays stable");
    }

    #[test]
    fn expanded_truncation_becomes_collapse_header_counting_rest() {
        let entries = map((0..13).map(|_| execute()).collect());
        let mut layout = seeded_layout(entries.len());
        let expanded: HashSet<EntryId> = [EntryId::new(0)].into();
        let spans = scan_and_project(&entries, &mut layout, 10, &expanded);
        assert_eq!(
            spans[0].kind,
            GroupKind::Truncation {
                participants: 13,
                hidden: 3,
            }
        );
        assert!(layout[0].group_collapse_header);
        assert_eq!(layout[0].group_header_count, 12);
        assert_eq!(layout[0].height, 1);
        assert!(layout[1..].iter().all(|i| i.height == 5));
    }

    #[test]
    fn hidden_thinking_flows_through_truncation_without_participating() {
        // 12 executes with a hidden thought interleaved: the run still
        // truncates, the thought neither counts nor gets written.
        let mut list: Vec<ScrollbackEntry> = (0..6).map(|_| execute()).collect();
        list.push(thought());
        list.extend((0..6).map(|_| execute()));
        let entries = map(list);
        let mut layout = seeded_layout(entries.len());
        let spans = scan(
            &entries,
            10,
            &HashSet::new(),
            true,
            /*show_thinking=*/ false,
        );
        project_to_layout(&spans, &entries, &mut layout, false);
        assert_eq!(
            spans[0].kind,
            GroupKind::Truncation {
                participants: 12,
                hidden: 2,
            }
        );
        assert_eq!(layout[6].height, 5, "hidden thought layout untouched");
        // Header + one hidden row land on the participating executes around it.
        assert_eq!(layout[0].group_header_count, 1);
        assert_eq!(layout[1].height, 0);
        assert_eq!(layout[2].height, 5);
    }

    #[test]
    fn span_containing_hits_inside_and_misses_gaps_and_ends() {
        let span = |start: usize, end: usize| GroupSpan {
            range: start..end,
            kind: GroupKind::VerbRun { members: 1 },
            expanded: false,
        };
        let spans = [span(2, 5), span(9, 12)];
        assert!(span_containing(&spans, 1).is_none());
        assert_eq!(span_containing(&spans, 2), Some(&spans[0]));
        assert_eq!(span_containing(&spans, 4), Some(&spans[0]));
        assert!(span_containing(&spans, 5).is_none(), "end is exclusive");
        assert!(span_containing(&spans, 7).is_none(), "gap between spans");
        assert_eq!(span_containing(&spans, 11), Some(&spans[1]));
        assert!(span_containing(&spans, 12).is_none());
        assert!(span_containing(&[], 0).is_none());
    }

    #[test]
    fn spans_are_sorted_and_disjoint() {
        let mut list = vec![skill_read(), skill_read(), execute()];
        list.extend((0..12).map(|_| execute()));
        list.push(skill_read());
        list.push(skill_read());
        let entries = map(list);
        let spans = scan(&entries, 10, &HashSet::new(), true, true);
        assert!(spans.len() >= 2);
        for pair in spans.windows(2) {
            assert!(pair[0].range.end <= pair[1].range.start);
        }
    }
}
