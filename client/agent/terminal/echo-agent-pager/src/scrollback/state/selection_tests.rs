use super::super::test_util::*;
use super::*;
use pretty_assertions::assert_eq;
use ratatui::style::Color;

// -----------------------------------------------------------------------
// Group gap tests (Phase 1c)
// -----------------------------------------------------------------------

/// Helper: create an expanded groupable stub block.
fn expanded_groupable(text: &str) -> ScrollbackEntry {
    ScrollbackEntry::new(RenderBlock::stub(text, Color::Blue))
        .with_display_mode(DisplayMode::Expanded)
}

/// Helper: create a collapsed non-groupable stub block (simulates AgentMessage).
fn non_groupable_entry(text: &str) -> ScrollbackEntry {
    ScrollbackEntry::new(RenderBlock::stub_non_groupable(text, Color::Blue))
        .with_display_mode(DisplayMode::Collapsed)
}

/// Helper: push entry into state and prepare layout, return gap_after values.
fn get_gap_after(state: &mut ScrollbackState) -> Vec<u16> {
    state.prepare_layout(80, 40);
    state
        .get_cached_entry_layouts()
        .unwrap()
        .iter()
        .map(|e| e.gap_after)
        .collect()
}

#[test]
fn test_gap_all_collapsed_groupable_dense() {
    // 3 collapsed groupable blocks → 0 gaps between them, 1 trailing
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(collapsed_groupable("b"));
    state.push(collapsed_groupable("c"));

    let gaps = get_gap_after(&mut state);
    assert_eq!(
        gaps,
        vec![0, 0, 1],
        "collapsed groupable neighbors should have gap=0"
    );
}

#[test]
fn test_gap_non_groupable_breaks_group() {
    // groupable, non-groupable, groupable → all gaps are 1
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(non_groupable_entry("msg"));
    state.push(collapsed_groupable("b"));

    let gaps = get_gap_after(&mut state);
    assert_eq!(gaps, vec![1, 1, 1], "non-groupable should break group");
}

#[test]
fn test_gap_expanded_within_group() {
    // collapsed, EXPANDED, collapsed → gaps around expanded
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(expanded_groupable("b"));
    state.push(collapsed_groupable("c"));

    let gaps = get_gap_after(&mut state);
    // a→b: both groupable, but b is expanded → 1
    // b→c: both groupable, but b is expanded → 1
    // c: trailing → 1
    assert_eq!(gaps, vec![1, 1, 1], "expanded block should get gaps");
}

#[test]
fn test_gap_mixed_group_with_expanded() {
    // Simulates: AgentMessage, Read(collapsed), Edit(expanded), List(collapsed),
    //            Run(collapsed), AgentMessage
    let mut state = ScrollbackState::new();
    state.push(non_groupable_entry("agent"));
    state.push(collapsed_groupable("read"));
    state.push(expanded_groupable("edit"));
    state.push(collapsed_groupable("list"));
    state.push(collapsed_groupable("run"));
    state.push(non_groupable_entry("agent2"));

    let gaps = get_gap_after(&mut state);
    // agent→read: not both groupable → 1
    // read→edit: both groupable, edit expanded → 1
    // edit→list: both groupable, edit expanded → 1
    // list→run: both groupable AND both collapsed → 0
    // run→agent2: not both groupable → 1
    // agent2: trailing → 1
    assert_eq!(gaps, vec![1, 1, 1, 0, 1, 1]);
}

#[test]
fn test_gap_single_groupable_block() {
    // Single groupable surrounded by non-groupable → all gaps 1
    let mut state = ScrollbackState::new();
    state.push(non_groupable_entry("before"));
    state.push(collapsed_groupable("tool"));
    state.push(non_groupable_entry("after"));

    let gaps = get_gap_after(&mut state);
    assert_eq!(
        gaps,
        vec![1, 1, 1],
        "single groupable block should have normal gaps"
    );
}

#[test]
fn test_gap_two_adjacent_expanded_groupable() {
    // Two expanded groupable blocks share 1 gap (not 2)
    let mut state = ScrollbackState::new();
    state.push(expanded_groupable("a"));
    state.push(expanded_groupable("b"));

    let gaps = get_gap_after(&mut state);
    // a→b: both groupable, but neither collapsed → 1 (shared gap)
    // b: trailing → 1
    assert_eq!(gaps, vec![1, 1]);
}

#[test]
fn test_gap_total_height_dense_group() {
    // 3 collapsed groupable blocks, each height=1 (collapsed stub)
    // Total = 1 + 0 + 1 + 0 + 1 + 1(trailing) = 4
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(collapsed_groupable("b"));
    state.push(collapsed_groupable("c"));
    state.prepare_layout(80, 40);

    let (_, _, total) = state.scroll_info();
    // Each collapsed stub = 1 line (no vpad since StubBlock has_vpad=true but
    // collapsed stubs render 1 line of text + 2 vpad = 3 lines)
    let layouts = state.get_cached_entry_layouts().unwrap();
    let expected: usize = layouts
        .iter()
        .map(|e| e.height as usize + e.gap_after as usize)
        .sum();
    assert_eq!(
        total, expected,
        "total_height should match sum of height+gap"
    );
}

#[test]
fn test_gap_after_toggle_fold() {
    // Start with 3 collapsed groupable → dense.
    // Toggle middle to expanded → gaps appear.
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(collapsed_groupable("b"));
    state.push(collapsed_groupable("c"));

    let gaps_before = get_gap_after(&mut state);
    assert_eq!(gaps_before, vec![0, 0, 1], "initially dense");

    // Expand entry 1
    state.set_selected(Some(1));
    state.toggle_fold_selected();
    state.prepare_layout(80, 40);

    let gaps_after = get_gap_after(&mut state);
    // a→b: b is now expanded → 1
    // b→c: b is expanded → 1
    // c: trailing → 1
    assert_eq!(
        gaps_after,
        vec![1, 1, 1],
        "after expanding middle, gaps appear"
    );
}

#[test]
fn test_gap_virtual_y_dense() {
    // 3 collapsed groupable stubs, verify virtual_y positions are contiguous
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(collapsed_groupable("b"));
    state.push(collapsed_groupable("c"));
    state.prepare_layout(80, 40);

    let virtual_y = state.get_cached_virtual_y().unwrap();
    let layouts = state.get_cached_entry_layouts().unwrap();

    // Entry 0: y=0
    assert_eq!(virtual_y[0], 0);
    // Entry 1: y=height[0] + gap[0] = height[0] + 0
    assert_eq!(virtual_y[1], layouts[0].height as usize);
    // Entry 2: y=height[0] + height[1] + 0 + 0
    assert_eq!(
        virtual_y[2],
        layouts[0].height as usize + layouts[1].height as usize
    );
}

#[test]
fn test_is_groupable_block_types() {
    // Verify is_groupable returns expected values for each block type
    assert!(
        RenderBlock::stub("x", Color::Blue).is_groupable(),
        "StubBlock should be groupable"
    );
    assert!(
        !RenderBlock::stub_non_groupable("x", Color::Blue).is_groupable(),
        "non-groupable stub"
    );
    assert!(
        !RenderBlock::user_prompt("x").is_groupable(),
        "UserPrompt not groupable"
    );
    assert!(
        !RenderBlock::agent_message("x").is_groupable(),
        "AgentMessage not groupable"
    );
    assert!(
        RenderBlock::thinking("x").is_groupable(),
        "Thinking should be groupable"
    );
    assert!(
        RenderBlock::execute("ls").is_groupable(),
        "Execute should be groupable"
    );
    assert!(
        RenderBlock::read("f.rs", None).is_groupable(),
        "Read should be groupable"
    );
    assert!(
        RenderBlock::list_dir(".").is_groupable(),
        "ListDir should be groupable"
    );
    assert!(
        RenderBlock::system("sys").is_groupable(),
        "System should be groupable"
    );
}

// -----------------------------------------------------------------------
// Group range tests (Phase 3a)
// -----------------------------------------------------------------------

#[test]
fn test_group_range_mode_a_all_collapsed() {
    // Mode A: all groupable blocks in one group regardless of display mode
    let mut state = ScrollbackState::new();
    state.push(non_groupable_entry("agent"));
    state.push(collapsed_groupable("a"));
    state.push(collapsed_groupable("b"));
    state.push(collapsed_groupable("c"));
    state.push(non_groupable_entry("agent2"));

    // Mode A (collapsed_only=false): full group
    assert_eq!(state.group_range_of(1, false), 1..4);
    assert_eq!(state.group_range_of(2, false), 1..4);
    assert_eq!(state.group_range_of(3, false), 1..4);
}

#[test]
fn test_group_range_mode_a_with_expanded() {
    // Mode A: expanded block doesn't break the group
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(expanded_groupable("b")); // expanded
    state.push(collapsed_groupable("c"));

    // Mode A: all 3 in one group
    assert_eq!(state.group_range_of(0, false), 0..3);
    assert_eq!(state.group_range_of(1, false), 0..3);
    assert_eq!(state.group_range_of(2, false), 0..3);
}

#[test]
fn test_group_range_mode_b_with_expanded() {
    // Mode B: expanded block breaks the contiguous collapsed run
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(collapsed_groupable("b"));
    state.push(expanded_groupable("c")); // expanded — breaks run
    state.push(collapsed_groupable("d"));
    state.push(collapsed_groupable("e"));

    // Mode B: [a,b] and [d,e] are separate sub-groups, c is singleton
    assert_eq!(state.group_range_of(0, true), 0..2);
    assert_eq!(state.group_range_of(1, true), 0..2);
    assert_eq!(state.group_range_of(2, true), 2..3); // expanded → singleton
    assert_eq!(state.group_range_of(3, true), 3..5);
    assert_eq!(state.group_range_of(4, true), 3..5);
}

#[test]
fn test_group_range_non_groupable() {
    // Non-groupable entries always return singleton
    let mut state = ScrollbackState::new();
    state.push(non_groupable_entry("agent"));
    state.push(collapsed_groupable("tool"));
    state.push(non_groupable_entry("agent2"));

    assert_eq!(state.group_range_of(0, false), 0..1); // non-groupable
    assert_eq!(state.group_range_of(1, false), 1..2); // lone groupable
    assert_eq!(state.group_range_of(2, false), 2..3); // non-groupable
}

#[test]
fn test_group_range_mode_b_all_expanded() {
    // Mode B: if all groupable blocks are expanded, each is a singleton
    let mut state = ScrollbackState::new();
    state.push(expanded_groupable("a"));
    state.push(expanded_groupable("b"));
    state.push(expanded_groupable("c"));

    assert_eq!(state.group_range_of(0, true), 0..1);
    assert_eq!(state.group_range_of(1, true), 1..2);
    assert_eq!(state.group_range_of(2, true), 2..3);

    // Mode A: all in one group
    assert_eq!(state.group_range_of(0, false), 0..3);
}

#[test]
fn test_group_range_non_groupable_breaks_group() {
    // Non-groupable in the middle splits the group in both modes
    let mut state = ScrollbackState::new();
    state.push(collapsed_groupable("a"));
    state.push(non_groupable_entry("agent"));
    state.push(collapsed_groupable("b"));

    assert_eq!(state.group_range_of(0, false), 0..1);
    assert_eq!(state.group_range_of(2, false), 2..3);
    assert_eq!(state.group_range_of(0, true), 0..1);
    assert_eq!(state.group_range_of(2, true), 2..3);
}

#[test]
fn test_grow_fold_during_page_flip_drops_follow_and_preserve() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut h = ScrollTestHarness::new(80, 50);
    h.state.appearance.scrollback.scroll.respect_manual_folds = true;
    h.push_prompt("old");
    h.push_agent("old response");
    h.send_prompt("new question");
    let prompt_idx = h.state.len() - 1;

    let think_id = h.push_thinking("thinking...");
    assert!(h.is_preserve(), "preserve active before fold");

    h.select(h.state.len() - 1);
    h.toggle_fold();
    h.frame();

    assert!(!h.is_follow(), "grow fold while following drops follow");
    assert!(!h.is_preserve(), "preserve dropped along with follow");
    let entry = h.state.get_by_id(think_id).unwrap();
    assert!(entry.display_mode_pinned, "manual fold pins the entry");
    assert_eq!(entry.display_mode, DisplayMode::Expanded);
    h.assert_entry_at_top(prompt_idx, "prompt still at top after fold");
}

#[test]
fn test_grow_fold_during_page_flip_overflow_stays_anchored() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut h = ScrollTestHarness::new(80, 50);
    h.state.appearance.scrollback.scroll.respect_manual_folds = true;
    h.push_prompt("old");
    h.push_agent("old response");
    h.send_prompt("new question");
    let prompt_idx = h.state.len() - 1;

    let think_id = h.push_thinking("line1");
    for i in 0..100 {
        // `  \n` is a CommonMark hard break; bare `\n` would collapse
        // to a space and the thinking block wouldn't overflow.
        h.state
            .push_chunk_to_thinking(think_id, &format!("  \nline{}", i + 2));
    }
    h.frame();
    assert!(h.is_preserve(), "preserve active before fold");

    h.select(h.state.len() - 1);
    h.toggle_fold();
    h.frame();

    assert!(!h.is_follow(), "grow fold while following drops follow");
    assert!(!h.is_preserve(), "preserve dropped along with follow");
    h.assert_entry_at_top(prompt_idx, "no snap to bottom — viewport stays at the fold");
}

#[test]
fn test_fold_without_follow_keeps_follow_off() {
    let mut h = ScrollTestHarness::new(80, 40);
    h.push_prompt("Q1");
    h.push_tool("tool1");
    h.push_tool("tool2");
    h.frame();

    h.state.goto_top();
    assert!(!h.is_follow(), "not following after goto_top");

    h.select(1);
    h.state.expand_selected();
    h.frame();

    assert!(!h.is_follow(), "fold should not re-enable follow");
}

#[test]
fn test_shrink_fold_while_following_restores_follow() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut h = ScrollTestHarness::new(80, 40);
    h.state.appearance.scrollback.scroll.respect_manual_folds = true;
    h.push_prompt("Q1");
    let think_id = h.push_thinking("thinking...");
    assert!(h.is_follow(), "following before fold");

    h.select(h.state.len() - 1);
    h.toggle_fold();
    h.frame();
    assert!(!h.is_follow(), "grow fold dropped follow");

    h.state.goto_bottom();
    h.frame();
    assert!(h.is_follow(), "G re-engages follow");

    h.select(h.state.len() - 1);
    h.toggle_fold();
    h.frame();

    let entry = h.state.get_by_id(think_id).unwrap();
    assert_eq!(
        entry.display_mode,
        DisplayMode::Truncated,
        "shrink fold applied"
    );
    assert!(entry.display_mode_pinned, "shrink fold also pins");
    assert!(h.is_follow(), "shrink fold restores follow as before");
}

#[test]
fn test_grow_fold_on_finished_entry_drops_follow() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut h = ScrollTestHarness::new(80, 40);
    h.state.appearance.scrollback.scroll.respect_manual_folds = true;
    h.push_prompt("Q1");
    let think_id = h
        .state
        .push_block(RenderBlock::thinking_with_time("deep", 1300));
    if let Some(entry) = h.state.entries.get_mut(&think_id) {
        entry.set_display_mode(DisplayMode::Collapsed);
    }
    h.push_tool("later tool");
    h.frame();
    assert!(h.is_follow(), "following before fold");

    let think_idx = h.state.index_of_id(think_id).unwrap();
    h.select(think_idx);
    h.state.expand_selected();
    h.frame();

    let entry = h.state.get_by_id(think_id).unwrap();
    assert!(!entry.is_running, "entry is finished");
    assert_eq!(entry.display_mode, DisplayMode::Expanded);
    assert!(entry.display_mode_pinned);
    assert!(
        !h.is_follow(),
        "growing a finished entry while following drops follow too"
    );
}

#[test]
fn test_anchor_off_shrink_fold_does_not_reenable_follow() {
    let mut h = ScrollTestHarness::new(80, 10);
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.blocks.prompt.vpad = false;
    appearance.scrollback.scroll.anchor_on_fold = false;
    appearance.scrollback.scroll.respect_manual_folds = true;
    h.state.set_appearance(appearance);

    h.push_prompt("Q1");
    for i in 0..12 {
        h.state.push(expanded_groupable(&format!("tool{i}")));
    }
    h.frame();
    h.state.goto_bottom();
    h.frame();
    assert!(h.is_follow(), "following at the bottom");

    h.select(1);
    h.state.collapse_selected();
    h.frame();

    assert!(
        !h.is_follow(),
        "ensure_selected_visible scrolled away; shrink fold must not re-enable follow"
    );
}

#[test]
fn test_respect_manual_folds_off_keeps_old_fold_follow_contract() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut h = ScrollTestHarness::new(80, 50);
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.blocks.prompt.vpad = false;
    appearance.scrollback.scroll.respect_manual_folds = false;
    h.state.set_appearance(appearance);

    h.push_prompt("old");
    h.send_prompt("new question");
    let think_id = h.push_thinking("thinking...");
    assert!(h.is_preserve(), "preserve active before fold");

    h.select(h.state.len() - 1);
    h.toggle_fold();
    h.frame();

    let entry = h.state.get_by_id(think_id).unwrap();
    assert_eq!(entry.display_mode, DisplayMode::Expanded);
    assert!(!entry.display_mode_pinned, "flag off: no pin recorded");
    assert!(
        h.is_follow(),
        "flag off: follow restored (pre-fix behavior)"
    );
    assert!(h.is_preserve(), "flag off: preserve restored");
}

#[test]
fn test_expand_while_streaming_keeps_viewport_until_follow_resumed() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut h = ScrollTestHarness::new(80, 20);
    h.state.appearance.scrollback.scroll.respect_manual_folds = true;
    h.push_prompt("Q1");
    let think_id = h.push_thinking("line1");
    for i in 0..50 {
        h.state
            .push_chunk_to_thinking(think_id, &format!("  \nline{}", i + 2));
    }
    h.frame();
    h.state.goto_bottom();
    h.frame();
    assert!(h.is_follow());

    h.select(h.state.len() - 1);
    h.toggle_fold();
    h.frame();
    assert!(!h.is_follow(), "expanding while following drops follow");
    let anchored = h.scroll_offset();

    for i in 0..30 {
        h.stream_thinking(think_id, &format!("  \nmore{i}"));
    }
    assert_eq!(
        h.scroll_offset(),
        anchored,
        "streaming chunks must not move the viewport while follow is off"
    );

    h.state.goto_bottom();
    h.frame();
    assert!(h.is_follow(), "G re-engages follow");
    h.assert_at_bottom("back at the tail after G");
}

#[test]
fn test_global_fold_ops_clear_pins_scoped() {
    let mut h = ScrollTestHarness::new(80, 40);
    h.state.appearance.scrollback.scroll.respect_manual_folds = true;
    h.push_prompt("Q1");
    let think_id = h
        .state
        .push_block(RenderBlock::thinking_with_time("deep", 1300));
    let tool_id = h.push_tool("tool");
    h.frame();

    h.select(h.state.index_of_id(think_id).unwrap());
    h.state.expand_selected();
    h.select(h.state.index_of_id(tool_id).unwrap());
    h.state.expand_selected();
    assert!(h.state.get_by_id(think_id).unwrap().display_mode_pinned);
    assert!(h.state.get_by_id(tool_id).unwrap().display_mode_pinned);

    h.state.expand_all_thinking();
    assert!(
        !h.state.get_by_id(think_id).unwrap().display_mode_pinned,
        "Ctrl+E clears thinking pins"
    );
    assert!(
        h.state.get_by_id(tool_id).unwrap().display_mode_pinned,
        "Ctrl+E leaves tool pins alone"
    );

    h.state.expand_all();
    assert!(
        !h.state.get_by_id(tool_id).unwrap().display_mode_pinned,
        "expand-all clears all pins"
    );

    h.select(h.state.index_of_id(tool_id).unwrap());
    h.state.collapse_selected();
    assert!(h.state.get_by_id(tool_id).unwrap().display_mode_pinned);
    h.state.collapse_all();
    assert!(
        !h.state.get_by_id(tool_id).unwrap().display_mode_pinned,
        "collapse-all clears all pins"
    );
}

#[test]
fn test_anchor_gap_delta_mid_group() {
    let mut h = ScrollTestHarness::new(80, 40);
    h.push_prompt("Q1");
    h.push_tool("a");
    h.push_tool("b");
    h.push_tool("c");

    let gaps_before = get_gap_after(&mut h.state);
    assert_eq!(gaps_before[1], 0, "a→b gap should be 0");

    if let Some((id, entry)) = h.state.entries.get_index_mut(2) {
        entry.set_display_mode(DisplayMode::Expanded);
        h.state.dirty_heights.insert(*id);
    }
    h.state.layout_cache = None;
    h.frame();

    let gaps_after = get_gap_after(&mut h.state);
    assert_eq!(gaps_after[1], 1, "a→b gap should be 1 after b expanded");
}

#[test]
fn test_anchor_no_gap_delta_first_in_group() {
    let mut h = ScrollTestHarness::new(80, 40);
    h.push_prompt("Q1");
    h.push_tool("a");
    h.push_tool("b");

    let gaps_before = get_gap_after(&mut h.state);
    assert_eq!(gaps_before[0], 1, "prompt→a gap should be 1");

    if let Some((id, entry)) = h.state.entries.get_index_mut(1) {
        entry.set_display_mode(DisplayMode::Expanded);
        h.state.dirty_heights.insert(*id);
    }
    h.state.layout_cache = None;
    h.frame();

    let gaps_after = get_gap_after(&mut h.state);
    assert_eq!(gaps_after[0], 1, "prompt→a gap still 1 after expand");
}

// ── Group truncation tests ──

#[test]
fn truncation_disabled_when_max_visible_zero() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 0;
    state.set_appearance(appearance);

    push_tool_calls(&mut state, 20);
    state.prepare_layout(80, 40);

    // No entries should be hidden
    for i in 0..20 {
        assert!(
            cached_height_at(&state, i) > 0,
            "entry {i} should be visible when truncation is disabled"
        );
    }
}

#[test]
fn truncation_skips_short_groups() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 10;
    state.set_appearance(appearance);

    // Group of exactly 11 (max_visible + 1) should NOT be truncated
    push_tool_calls(&mut state, 11);
    state.prepare_layout(80, 40);

    for i in 0..11 {
        assert_eq!(
            header_count_at(&mut state, i),
            0,
            "group of 11 should not be truncated with max_visible=10"
        );
    }
}

#[test]
fn truncation_applies_to_groups_exceeding_threshold() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 10;
    state.set_appearance(appearance);

    // Group of 12 (max_visible + 2) should have 2 hidden entries
    push_tool_calls(&mut state, 12);
    state.prepare_layout(80, 40);

    // Entry 0 = group header showing "1 more" (hidden_count - 1, don't count self)
    assert_eq!(header_count_at(&mut state, 0), 1);
    assert_eq!(cached_height_at(&state, 0), 1);

    // Entry 1 = hidden (height=0)
    assert_eq!(cached_height_at(&state, 1), 0);

    // Entries 2..12 = visible (the last 10)
    for i in 2..12 {
        assert!(
            cached_height_at(&state, i) > 0,
            "entry {i} should be visible in the tail"
        );
    }
}

#[test]
fn truncation_group_at_start_and_end() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    // Group of 5 tool calls at the start
    push_tool_calls(&mut state, 5);
    // Break with a non-groupable entry
    state.push_block(RenderBlock::agent_message("break"));
    // Group of 6 tool calls at the end
    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    // First group: 5 entries, max_visible=3, hidden=2, header count=1
    assert_eq!(header_count_at(&mut state, 0), 1);
    assert_eq!(cached_height_at(&state, 1), 0);
    for i in 2..5 {
        assert!(cached_height_at(&state, i) > 0);
    }

    // Agent message at index 5 is visible
    assert!(cached_height_at(&state, 5) > 0);

    // Second group: 6 entries (indices 6..12), max_visible=3, hidden=3, header count=2
    assert_eq!(header_count_at(&mut state, 6), 2);
    assert_eq!(cached_height_at(&state, 7), 0);
    assert_eq!(cached_height_at(&state, 8), 0);
    for i in 9..12 {
        assert!(cached_height_at(&state, i) > 0);
    }
}

#[test]
fn navigation_skips_hidden_entries() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    // 6 entries → 3 hidden (header + 2 hidden), 3 visible
    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    // Select first visible entry
    state.selected = None;
    state.select_next(); // should land on entry 0 (group header, height=1)
    assert_eq!(state.selected, Some(0));

    state.select_next(); // should skip entries 1,2 (height=0) → entry 3
    assert_eq!(state.selected, Some(3));

    state.select_next(); // entry 4
    assert_eq!(state.selected, Some(4));

    // Go back
    state.select_prev(); // entry 3
    assert_eq!(state.selected, Some(3));

    state.select_prev(); // should skip entries 2,1 (height=0) → entry 0
    assert_eq!(state.selected, Some(0));
}

// ── Verb-group fold tests ──

fn verb_state() -> ScrollbackState {
    crate::appearance::cache::set_group_tool_verbs(true);
    crate::appearance::cache::set_show_thinking_blocks(false);
    ScrollbackState::new()
}

fn push_reads(state: &mut ScrollbackState, n: usize) -> Vec<EntryId> {
    (0..n)
        .map(|i| state.push_block(RenderBlock::read(format!("f{i}.rs"), None)))
        .collect()
}

fn push_subagent(state: &mut ScrollbackState, child_sid: &str) -> EntryId {
    state.push_block(RenderBlock::Subagent(
        crate::scrollback::blocks::SubagentBlock::started(
            "task", child_sid, "explore", None, None, None, /*is_background=*/ false,
        ),
    ))
}

fn verb_header_at(state: &ScrollbackState, idx: usize) -> bool {
    state
        .layout_cache
        .as_ref()
        .and_then(|c| c.entries.get(idx))
        .map(|e| e.verb_group_header)
        .unwrap_or(false)
}

#[test]
fn verb_group_folds_multi_member_and_singleton_runs() {
    let mut state = verb_state();
    push_reads(&mut state, 3);
    state.push_block(RenderBlock::agent_message("break"));
    push_reads(&mut state, 1);
    state.prepare_layout(80, 40);

    // Run of 3: header row + two hidden members.
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 3);
    assert_eq!(cached_height_at(&state, 0), 1);
    assert_eq!(cached_height_at(&state, 1), 0);
    assert_eq!(cached_height_at(&state, 2), 0);

    // A lone read past the break folds too: one compact header row.
    assert!(verb_header_at(&state, 4));
    assert_eq!(header_count_at(&mut state, 4), 1);
    assert_eq!(cached_height_at(&state, 4), 1);
}

#[test]
fn verb_group_separators_break_runs() {
    let mut state = verb_state();
    push_reads(&mut state, 2);
    state.push_block(RenderBlock::execute("cargo test"));
    push_reads(&mut state, 2);
    state.push_block(RenderBlock::edit("f.rs", None));
    push_reads(&mut state, 1);
    state.prepare_layout(80, 40);

    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 2);
    assert!(cached_height_at(&state, 2) > 0, "execute stays standalone");
    assert!(verb_header_at(&state, 3));
    assert!(cached_height_at(&state, 5) > 0, "edit stays standalone");
    // The singleton fold's shape (count/height) is owned by
    // `verb_group_folds_multi_member_and_singleton_runs`.
    assert!(verb_header_at(&state, 6), "run re-anchors after the Edit");
}

/// Push a finished thought: collapsed and not running — the shape
/// `finish_thinking`'s auto-collapse leaves behind.
fn push_thought(state: &mut ScrollbackState, text: &str) -> EntryId {
    let id = state.push_block(RenderBlock::thinking(text));
    state
        .get_by_id_mut(id)
        .unwrap()
        .set_display_mode(DisplayMode::Collapsed);
    id
}

#[test]
fn verb_group_hidden_thinking_is_transparent() {
    let mut state = verb_state();
    push_reads(&mut state, 1);
    state.push_block(RenderBlock::thinking("hmm"));
    push_reads(&mut state, 1);
    state.prepare_layout(80, 40);

    // Hidden thinking doesn't break the run; both reads fold.
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 2);
    assert_eq!(cached_height_at(&state, 2), 0);

    // Shown non-collapsed thinking keeps its own rows; the run folds
    // around it instead of splitting.
    crate::appearance::cache::set_show_thinking_blocks(true);
    state.rebuild_layout();
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert!(cached_height_at(&state, 1) > 0, "thinking renders its rows");
    assert_eq!(cached_height_at(&state, 2), 0, "run continues across it");
    crate::appearance::cache::set_show_thinking_blocks(false);
}

#[test]
fn verb_group_leading_thought_anchors_run_and_expands() {
    let mut state = verb_state();
    crate::appearance::cache::set_show_thinking_blocks(true);
    let thought = push_thought(&mut state, "planned the reads");
    push_reads(&mut state, 2);
    state.prepare_layout(80, 40);

    // The finished thought hosts the header slot; tools fold behind it.
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 2, "count is tools-only");
    assert_eq!(cached_height_at(&state, 0), 1);
    assert_eq!(cached_height_at(&state, 1), 0);
    assert_eq!(cached_height_at(&state, 2), 0);

    // Expand: the slot stacks the header above the thought's own 1-row
    // "Thought" line; every tool member reveals below.
    state.set_selected(Some(0));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert!(state.expanded_groups.contains(&thought));
    assert_eq!(cached_height_at(&state, 0), 2);
    assert!(cached_height_at(&state, 1) > 0);
    assert!(cached_height_at(&state, 2) > 0);

    // Collapse from a member refolds behind the thought header.
    state.set_selected(Some(1));
    assert!(state.collapse_group_if_expanded());
    state.prepare_layout(80, 40);
    assert!(!state.expanded_groups.contains(&thought));
    assert_eq!(cached_height_at(&state, 1), 0);

    // Ctrl+E opens the anchoring thought (it goes transparent and the
    // run re-anchors on the first tool): the expansion key must migrate
    // so the members stay expanded instead of snapping collapsed.
    state.set_selected(Some(0));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    state.expand_all_thinking();
    state.prepare_layout(80, 40);
    assert!(cached_height_at(&state, 0) > 1, "anchor thought opened");
    assert!(
        cached_height_at(&state, 2) > 0,
        "members stay expanded across Ctrl+E on the anchoring thought"
    );
    let cache = state.layout_cache.as_ref().unwrap();
    let tool_height = EntryRenderer::new(state.entries.get_index(1).unwrap().1, &Theme::current())
        .with_appearance_ref(&state.appearance)
        .desired_height(state.entry_area_width(80));
    assert_eq!(
        cache.entries[1].height,
        tool_height.saturating_add(1),
        "re-anchored expanded header reserves its synthetic row"
    );
    crate::appearance::cache::set_show_thinking_blocks(false);
}

#[test]
fn group_range_keeps_hooked_members_in_rendered_fold() {
    let mut state = verb_state();
    let ids = push_reads(&mut state, 2);
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(state.group_range_of(0, true), 0..2);

    state.attach_hooks(
        ids[1],
        crate::scrollback::blocks::tool::HookPhase::Post,
        Vec::new(),
    );
    assert_eq!(state.group_range_of(0, true), 0..2);

    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(state.group_range_of(1, true), 0..2);
    assert_eq!(cached_height_at(&state, 1), 0, "hooked member stays folded");
}

#[test]
fn verb_group_interior_thought_claims_into_fold() {
    let mut state = verb_state();
    crate::appearance::cache::set_show_thinking_blocks(true);
    push_reads(&mut state, 1);
    push_thought(&mut state, "midway");
    push_reads(&mut state, 1);
    state.prepare_layout(80, 40);

    // One run: the thought folds to height 0 between the tools.
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 2, "thought never counts");
    assert_eq!(cached_height_at(&state, 1), 0, "thought claims into fold");
    assert_eq!(cached_height_at(&state, 2), 0);

    // The folded thought resolves to the run for toggle/reveal paths.
    assert_eq!(state.group_range_of(1, true), 0..3);
    crate::appearance::cache::set_show_thinking_blocks(false);
}

#[test]
fn verb_group_trailing_thought_claims_and_keeps_boundary_gap() {
    let gap = |state: &ScrollbackState, idx: usize| {
        state.layout_cache.as_ref().unwrap().entries[idx].gap_after
    };
    let mut state = verb_state();
    crate::appearance::cache::set_show_thinking_blocks(true);
    push_reads(&mut state, 2);
    push_thought(&mut state, "wrapped up");
    state.push_block(RenderBlock::agent_message("after"));
    state.prepare_layout(80, 40);

    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 2), 0, "trailing thought claims");
    // The LAST claimed entry (the thought) carries the boundary gap.
    assert_eq!(gap(&state, 0), 0);
    assert_eq!(gap(&state, 1), 0);
    assert_eq!(gap(&state, 2), 1, "boundary blank before agent text");
    crate::appearance::cache::set_show_thinking_blocks(false);
}

#[test]
fn verb_group_thoughts_never_count_but_fold_behind_one_tool() {
    // Pure-thought runs never fold: with no tool member the aggregated
    // header label would be empty.
    let mut state = verb_state();
    crate::appearance::cache::set_show_thinking_blocks(true);
    push_thought(&mut state, "one");
    push_thought(&mut state, "two");
    state.prepare_layout(80, 40);
    for i in 0..2 {
        assert!(!verb_header_at(&state, i));
        assert!(cached_height_at(&state, i) > 0);
    }

    // One tool plus finished thoughts is a run: it folds into a single
    // header row that swallows the thoughts, and the count stays
    // tools-only.
    let mut state = verb_state();
    crate::appearance::cache::set_show_thinking_blocks(true);
    state.push_block(RenderBlock::list_dir("src"));
    push_thought(&mut state, "scanned the tree");
    push_thought(&mut state, "picked a file");
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 1, "thoughts never count");
    assert_eq!(cached_height_at(&state, 0), 1);
    assert_eq!(cached_height_at(&state, 1), 0, "thought folds behind");
    assert_eq!(cached_height_at(&state, 2), 0, "thought folds behind");
    // The folded thought resolves to the singleton run for toggle/reveal.
    assert_eq!(state.group_range_of(1, true), 0..3);
    crate::appearance::cache::set_show_thinking_blocks(false);
}

#[test]
fn verb_group_opened_thought_stays_transparent_inside_fold() {
    let mut state = verb_state();
    crate::appearance::cache::set_show_thinking_blocks(true);
    push_reads(&mut state, 1);
    let thought = push_thought(&mut state, "midway");
    push_reads(&mut state, 1);
    state.prepare_layout(80, 40);
    assert_eq!(cached_height_at(&state, 1), 0);

    // Opening the thought drops it from the fold; the run keeps folding
    // around its rendered rows (thinking never breaks a run).
    state
        .get_by_id_mut(thought)
        .unwrap()
        .set_display_mode(DisplayMode::Expanded);
    state.rebuild_layout();
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0), "run still folds");
    assert!(cached_height_at(&state, 1) > 0, "opened thought renders");
    assert_eq!(cached_height_at(&state, 2), 0, "run continues across it");
    let gap = state.layout_cache.as_ref().unwrap().entries[1].gap_after;
    assert_eq!(gap, 0, "in-run transparent keeps no trailing blank");
    crate::appearance::cache::set_show_thinking_blocks(false);
}

#[test]
fn verb_group_streaming_thinking_keeps_one_run_then_folds() {
    let mut state = verb_state();
    crate::appearance::cache::set_show_thinking_blocks(true);
    push_reads(&mut state, 1);
    let live = state.push_block(RenderBlock::thinking("streaming"));
    state.get_by_id_mut(live).unwrap().is_running = true;
    push_reads(&mut state, 1);
    state.prepare_layout(80, 40);

    // One run around the live panel: header, panel rows, hidden tool.
    assert!(verb_header_at(&state, 0));
    assert!(!verb_header_at(&state, 2), "no second run after the panel");
    assert!(cached_height_at(&state, 1) > 0, "live panel renders");
    assert_eq!(cached_height_at(&state, 2), 0);

    // Finishing (running off + auto-collapse) folds the thought in.
    let entry = state.get_by_id_mut(live).unwrap();
    entry.is_running = false;
    entry.set_display_mode(DisplayMode::Collapsed);
    state.rebuild_layout();
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 1), 0, "finished thought folds");
    crate::appearance::cache::set_show_thinking_blocks(false);
}

#[test]
fn verb_group_excludes_pending_input() {
    let mut state = verb_state();
    push_reads(&mut state, 3);
    state.entry_mut(1).unwrap().is_pending_user_input = true;
    state.prepare_layout(80, 40);
    // The pending entry stays standalone — its prompt chrome must remain
    // visible — while the reads around it fold as singleton runs.
    assert!(verb_header_at(&state, 0));
    assert!(!verb_header_at(&state, 1), "pending row never claims");
    assert!(verb_header_at(&state, 2));
    for i in 0..3 {
        assert!(cached_height_at(&state, i) > 0);
    }
}

#[test]
fn verb_group_refolds_on_pending_input_transitions() {
    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 1), 0);

    // A permission prompt lands on an already-hidden member: the flag flip
    // alone must re-run the folds so the prompt row surfaces (the run
    // splits into singleton folds around it).
    assert!(state.set_pending_user_input(ids[1], true));
    state.prepare_layout(80, 40);
    assert_eq!(
        header_count_at(&mut state, 0),
        1,
        "run splits at the prompt"
    );
    assert!(
        cached_height_at(&state, 1) > 0,
        "pending row must surface out of the fold"
    );

    // Resolving the prompt refolds the run.
    assert!(state.set_pending_user_input(ids[1], false));
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 1), 0, "resolved row refolds");
}

#[test]
fn verb_group_refolds_when_clear_all_resolves_pending_input() {
    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    assert!(state.set_pending_user_input(ids[1], true));
    state.prepare_layout(80, 40);
    assert!(cached_height_at(&state, 1) > 0);

    // The per-frame sync resolves prompts by clearing without re-marking —
    // that false-ward path must refold on its own.
    state.clear_all_pending_user_input();
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 1), 0, "cleared row refolds");
}

#[test]
fn verb_group_stays_folded_on_attach_hooks() {
    use crate::scrollback::blocks::tool::{HookPhase, HookRunEntry, HookRunStatus};

    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    state.prepare_layout(80, 40);
    assert_eq!(cached_height_at(&state, 2), 0);

    state.attach_hooks(
        ids[2],
        HookPhase::Post,
        vec![HookRunEntry {
            name: "fmt".to_owned(),
            status: HookRunStatus::Success {
                elapsed: std::time::Duration::from_millis(1),
            },
            output: None,
        }],
    );
    assert!(state.gaps_may_be_dirty, "hook attachment reapplies folds");
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 3);
    assert_eq!(cached_height_at(&state, 2), 0, "hooked row remains folded");
}

#[test]
fn verb_group_folds_incremental_push_with_truncation_disabled() {
    // `group_max_visible = 0` disables the N-more pass, but the verb fold
    // is gated independently on `group_tool_verbs` — the incremental push
    // path must mark dirtiness for it too (the old gate keyed only off
    // `group_max_visible > 0`, so the fold lagged until a full rebuild).
    let mut state = verb_state();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 0;
    state.set_appearance(appearance);

    // Build a valid cache first so the pushes below take the incremental
    // extend path (pre-layout pushes are handled by the Case 1 full build).
    state.push_block(RenderBlock::agent_message("break"));
    state.prepare_layout(80, 40);

    push_reads(&mut state, 2);
    // Extend must have succeeded — a failed extend falls back to full
    // invalidation, which would fold even with the gate broken and mask
    // the regression this test pins.
    assert!(
        state.layout_cache.is_some(),
        "pushes must take the incremental extend path"
    );
    state.prepare_layout(80, 40);

    assert!(verb_header_at(&state, 1), "run of 2 folds behind the break");
    assert_eq!(cached_height_at(&state, 1), 1);
    assert_eq!(
        cached_height_at(&state, 2),
        0,
        "second member must fold without any full invalidation"
    );
}

#[test]
fn verb_group_boundary_gap_follows_pairwise_rule() {
    let gap = |state: &ScrollbackState, idx: usize| {
        state.layout_cache.as_ref().unwrap().entries[idx].gap_after
    };

    // Controls with folding OFF: the pairwise rule's verdict at the
    // run→neighbor boundary, for both neighbor kinds.
    crate::appearance::cache::set_show_thinking_blocks(false);
    crate::appearance::cache::set_group_tool_verbs(false);
    let mut control_text = ScrollbackState::new();
    push_reads(&mut control_text, 3);
    control_text.push_block(RenderBlock::agent_message("after"));
    control_text.prepare_layout(80, 40);
    let text_boundary = gap(&control_text, 2);
    assert_eq!(
        text_boundary, 1,
        "pairwise rule: blank row before agent text"
    );

    let mut control_exec = ScrollbackState::new();
    push_reads(&mut control_exec, 3);
    control_exec.push_block(RenderBlock::execute("cargo test"));
    control_exec.prepare_layout(80, 40);
    let exec_boundary = gap(&control_exec, 2);

    // Folded: gaps zero WITHIN the run; the last member keeps the
    // pairwise boundary gap so the header never glues to what follows.
    let mut state = verb_state();
    push_reads(&mut state, 3);
    state.push_block(RenderBlock::agent_message("after"));
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(gap(&state, 0), 0);
    assert_eq!(gap(&state, 1), 0);
    assert_eq!(
        gap(&state, 2),
        text_boundary,
        "boundary gap before agent text"
    );

    let mut state = verb_state();
    push_reads(&mut state, 3);
    state.push_block(RenderBlock::execute("cargo test"));
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(
        gap(&state, 2),
        exec_boundary,
        "boundary against a tool row matches the unfolded dense rule"
    );

    // Expanded: the last member's boundary gap survives too.
    let mut state = verb_state();
    push_reads(&mut state, 3);
    state.push_block(RenderBlock::agent_message("after"));
    state.prepare_layout(80, 40);
    state.set_selected(Some(0));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert_eq!(gap(&state, 2), text_boundary, "expanded boundary gap");

    // Singleton: the header IS the run's last claimed entry, so it keeps
    // the pairwise boundary gap itself.
    let mut state = verb_state();
    push_reads(&mut state, 1);
    state.push_block(RenderBlock::agent_message("after"));
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(
        gap(&state, 0),
        text_boundary,
        "singleton header keeps the boundary gap before agent text"
    );
}

#[test]
fn verb_group_expand_and_collapse_round_trip() {
    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));

    // Enter on the header expands: the first slot stacks the header line
    // above entry 0's own row (height 2), so EVERY member — including the
    // first — renders below the header.
    state.set_selected(Some(0));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert!(state.expanded_groups.contains(&ids[0]));
    assert!(verb_header_at(&state, 0));
    let info = state.layout_cache.as_ref().unwrap().entries[0];
    assert!(info.group_collapse_header);
    assert_eq!(
        cached_height_at(&state, 0),
        2,
        "header slot = header line + first member's own row"
    );
    assert!(cached_height_at(&state, 1) > 0);
    assert!(cached_height_at(&state, 2) > 0);

    // group_range_of covers the run from any member while expanded.
    assert_eq!(state.group_range_of(1, true), 0..3);

    // h from a member collapses the group again.
    state.set_selected(Some(1));
    assert!(state.collapse_group_if_expanded());
    state.prepare_layout(80, 40);
    assert!(!state.expanded_groups.contains(&ids[0]));
    assert_eq!(cached_height_at(&state, 1), 0);
}

#[test]
fn verb_group_singleton_expand_and_collapse_round_trip() {
    let mut state = verb_state();
    let ids = push_reads(&mut state, 1);
    state.push_block(RenderBlock::agent_message("after"));
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 0), 1);

    // The folded singleton carries the full header interaction surface.
    state.set_selected(Some(0));
    assert!(state.is_selected_group_header());
    assert_eq!(state.selected_group_header_fold_label(), Some("expand"));

    // Expand: the slot stacks the header line above the member's own row.
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert!(state.expanded_groups.contains(&ids[0]));
    let info = state.layout_cache.as_ref().unwrap().entries[0];
    assert!(info.verb_group_header && info.group_collapse_header);
    assert_eq!(cached_height_at(&state, 0), 2, "header line + member row");
    // The expanded slot acts as member 0, exactly like a multi-member run.
    assert!(!state.is_selected_group_header());
    assert!(!state.toggle_group_expansion());

    // Left from the header collapses back to the folded header row.
    assert!(state.collapse_group_if_expanded());
    state.prepare_layout(80, 40);
    assert!(!state.expanded_groups.contains(&ids[0]));
    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 0), 1);
}

#[test]
fn verb_group_left_collapses_from_selected_header() {
    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    state.prepare_layout(80, 40);
    state.set_selected(Some(0));

    // Expanding from the header KEEPS the header selected (unlike N-more
    // groups), so an immediate Collapse re-folds without re-selecting.
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert_eq!(
        state.selected(),
        Some(0),
        "verb header stays selected on expand"
    );

    assert!(state.collapse_group_if_expanded());
    state.prepare_layout(80, 40);
    assert!(!state.expanded_groups.contains(&ids[0]));
    assert_eq!(cached_height_at(&state, 1), 0, "run refolds from header");
    assert_eq!(
        state.selected(),
        Some(0),
        "selection stays on the header after collapse"
    );
}

#[test]
fn verb_group_expand_keeps_preserved_scroll_pin() {
    use crate::scrollback::blocks::tool::{ReadToolCallBlock, ToolCallBlock};

    let mut state = verb_state();
    // Turn-1 filler so the pinned prompt sits at virtual_y > 0, then the
    // prompt and a foldable run below it (the page-flip repro shape).
    for i in 0..8 {
        state.push_block(RenderBlock::agent_message(format!("history {i}")));
    }
    state.push_block(RenderBlock::user_prompt("go"));
    push_reads(&mut state, 8);
    state.prepare_layout(80, 12);

    // Mimic dispatch_send_prompt's page flip: prompt pinned at the
    // viewport top, follow+preserve armed, content below fits on screen.
    let pin = state.layout_cache.as_ref().unwrap().virtual_y[8];
    state.scroll_offset = pin;
    state.follow_mode = true;
    state.follow_preserve_scroll = true;
    state.prepare_layout(80, 12);
    assert_eq!(state.scroll_offset, pin, "preserve pin holds before toggle");

    // Expand the group (header at idx 9, right below the prompt).
    state.set_selected(Some(9));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 12);
    // Setup sanity: the growth really pushes max_offset past the pin —
    // the exact shape whose next follow pass used to snap to the bottom.
    let max_offset = state
        .total_height
        .saturating_sub(state.viewport_height as usize);
    assert!(max_offset > pin, "setup must overflow the preserve pin");
    assert_eq!(
        state.scroll_offset, pin,
        "expanding a group must not snap the view to the bottom"
    );

    // The same invariant holds for a plain block fold in the same shape.
    state.collapse_group_if_expanded();
    state.prepare_layout(80, 12);
    state.scroll_offset = pin;
    state.follow_mode = true;
    state.follow_preserve_scroll = true;
    state.entry_mut(9).unwrap().block = RenderBlock::ToolCall(ToolCallBlock::Read(
        ReadToolCallBlock::new("f9.rs")
            .with_content("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl".to_owned(), 12),
    ));
    state.set_selected(Some(9));
    state.expand_selected();
    state.prepare_layout(80, 12);
    assert_eq!(
        state.scroll_offset, pin,
        "expanding a plain block must not snap the view to the bottom"
    );
}

#[test]
fn expanded_verb_slot_routes_to_member_zero() {
    use crate::scrollback::blocks::tool::{ReadToolCallBlock, ToolCallBlock};

    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    // Member 0 needs body content: a contentless Read is not foldable
    // (`ReadToolCallBlock::is_foldable == has_content`), so Right below
    // would no-op instead of opening the block.
    state.entry_mut(0).unwrap().block = RenderBlock::ToolCall(ToolCallBlock::Read(
        ReadToolCallBlock::new("f0.rs").with_content("member zero body".to_owned(), 1),
    ));
    state.prepare_layout(80, 40);
    state.set_selected(Some(0));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);

    // The expanded slot acts as member 0: no header semantics, no
    // re-toggle — Expand/ToggleFold/Enter fall through to the block, and
    // copy/fullscreen still see member 0's visible content.
    assert!(!state.is_selected_group_header());
    assert!(!state.entry_content_hidden_by_group(0));
    assert_eq!(state.selected_group_header_fold_label(), None);
    assert!(!state.toggle_group_expansion());

    // Right opens member 0's own block: it goes transparent, the run
    // re-anchors on the next member, and the expansion re-keys with it —
    // the remaining pair stays an EXPANDED group behind the open block.
    state.expand_selected();
    state.prepare_layout(80, 40);
    assert!(!verb_header_at(&state, 0));
    assert!(cached_height_at(&state, 0) > 1, "member 0 block is open");
    assert!(
        verb_header_at(&state, 1),
        "run re-anchors on the next member"
    );
    assert!(
        cached_height_at(&state, 2) > 0,
        "the re-anchored group stays expanded"
    );

    // Collapsing the block re-forms the still-expanded group.
    state.collapse_selected();
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(cached_height_at(&state, 0), 2);
    assert!(state.expanded_groups.contains(&ids[0]));

    // Left from the slot still collapses the group (member path).
    assert!(state.collapse_group_if_expanded());
    state.prepare_layout(80, 40);
    assert_eq!(cached_height_at(&state, 1), 0);
}

/// The e/e round trip on the expanded slot: e opens member 0's block,
/// e closes it back into the still-expanded group — and while any member
/// is open the OTHER members must keep their rows instead of snapping
/// into a fresh collapsed fold (the run's expansion follows its anchor).
#[test]
fn verb_group_expanded_slot_member_toggle_round_trips() {
    use crate::scrollback::blocks::tool::{ReadToolCallBlock, ToolCallBlock};

    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    for i in 0..3 {
        // Members need body content to be foldable at all.
        state.entry_mut(i).unwrap().block = RenderBlock::ToolCall(ToolCallBlock::Read(
            ReadToolCallBlock::new(format!("f{i}.rs")).with_content("body".to_owned(), 1),
        ));
    }
    state.prepare_layout(80, 40);
    state.set_selected(Some(0));
    // Right on the collapsed fold expands the group (first toggle).
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert_eq!(cached_height_at(&state, 0), 2);

    // e: the slot routes to member 0's own block, which opens. The
    // surviving run must stay EXPANDED behind it — members keep rows.
    assert!(!state.toggle_group_expansion());
    state.toggle_fold_selected();
    state.prepare_layout(80, 40);
    assert!(cached_height_at(&state, 0) > 1, "member 0 block is open");
    assert!(
        cached_height_at(&state, 2) > 0,
        "members must not snap into a collapsed fold while one is open"
    );

    // e again: closing member 0 returns to the original expanded group.
    assert!(!state.toggle_group_expansion());
    state.toggle_fold_selected();
    state.prepare_layout(80, 40);
    assert!(state.expanded_groups.contains(&ids[0]));
    assert_eq!(cached_height_at(&state, 0), 2, "expanded slot re-forms");
    assert!(cached_height_at(&state, 1) > 0);
    assert!(cached_height_at(&state, 2) > 0);

    // Opening an INTERIOR member keeps the group intact around it: one
    // run, same header, opened rows inline (like opened thinking).
    state.set_selected(Some(1));
    state.toggle_fold_selected();
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0), "group survives an interior open");
    assert_eq!(cached_height_at(&state, 0), 2);
    assert!(cached_height_at(&state, 1) > 1, "member 1 block is open");
    assert!(cached_height_at(&state, 2) > 0, "member 2 keeps its row");
    // Closing it restores the member row within the same group.
    state.toggle_fold_selected();
    state.prepare_layout(80, 40);
    assert!(state.expanded_groups.contains(&ids[0]));
    assert_eq!(cached_height_at(&state, 1), 1);
}

#[test]
fn verb_group_range_excludes_adjacent_dense_neighbors() {
    let mut state = verb_state();
    state.push_block(RenderBlock::edit("f.rs", None));
    push_reads(&mut state, 3);
    state.prepare_layout(80, 40);

    // Edit is groupable+collapsed (dense packing) but not verb-groupable;
    // the verb range must stop at it.
    assert_eq!(state.group_range_of(2, true), 1..4);
    assert!(verb_header_at(&state, 1));
}

#[test]
fn verb_group_claimed_entries_skip_n_more_truncation() {
    let mut state = verb_state();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    // 4 Other tools + 3 reads are one dense run of 7 (> max_visible + 1),
    // but the claimed reads break the truncation scan: the Others alone
    // (4 <= max_visible + 1) stay untruncated.
    push_tool_calls(&mut state, 4);
    push_reads(&mut state, 3);
    state.prepare_layout(80, 40);

    for i in 0..4 {
        assert_eq!(header_count_at(&mut state, i), 0, "no N-more header");
        assert!(cached_height_at(&state, i) > 0);
    }
    assert!(verb_header_at(&state, 4));
    assert_eq!(header_count_at(&mut state, 4), 3);
    assert_eq!(cached_height_at(&state, 5), 0);
    assert_eq!(cached_height_at(&state, 6), 0);
}

#[test]
fn verb_group_flag_off_matches_todays_layout() {
    crate::appearance::cache::set_group_tool_verbs(false);
    crate::appearance::cache::set_show_thinking_blocks(false);
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    // Short read run: fully visible, no verb header.
    push_reads(&mut state, 3);
    state.push_block(RenderBlock::agent_message("break"));
    // Long read run: plain N-more truncation applies as today.
    push_reads(&mut state, 5);
    state.prepare_layout(80, 40);

    for i in 0..3 {
        assert!(!verb_header_at(&state, i));
        assert!(cached_height_at(&state, i) > 0);
    }
    assert_eq!(header_count_at(&mut state, 4), 1, "N-more still applies");
    assert_eq!(cached_height_at(&state, 5), 0);
    crate::appearance::cache::set_group_tool_verbs(true);
}

#[test]
fn verb_group_search_reveal_unhides_member() {
    use crate::scrollback::blocks::tool::{ReadToolCallBlock, ToolCallBlock};

    let mut state = verb_state();
    let ids = push_reads(&mut state, 3);
    state.prepare_layout(80, 40);
    assert_eq!(cached_height_at(&state, 1), 0);

    state.reveal_entry_line(1, 0);
    state.prepare_layout(80, 40);
    assert!(state.expanded_groups.contains(&ids[0]));
    assert!(
        cached_height_at(&state, 1) > 0,
        "reveal must un-hide the verb-grouped member"
    );

    // Reveal on the expanded group's HEAD opens it (transparent; run
    // re-anchors on the next member): the expansion key must migrate so
    // the members stay expanded instead of snapping collapsed.
    state.entry_mut(0).unwrap().block = RenderBlock::ToolCall(ToolCallBlock::Read(
        ReadToolCallBlock::new("f0.rs").with_content("head body".to_owned(), 1),
    ));
    state.reveal_entry_line(0, 0);
    state.prepare_layout(80, 40);
    assert!(cached_height_at(&state, 0) > 1, "revealed head opened");
    assert!(
        cached_height_at(&state, 2) > 0,
        "members stay expanded across a head reveal"
    );
}

#[test]
fn verb_group_subagent_head_expand_and_collapse_round_trip() {
    let mut state = verb_state();
    let sub_id = push_subagent(&mut state, "child-A");
    push_reads(&mut state, 2);
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 3);
    assert_eq!(cached_height_at(&state, 0), 1);
    assert_eq!(cached_height_at(&state, 1), 0);

    // Expand from the header: the subagent hosts the shared slot (header
    // line stacked above its own row); every member reveals below.
    state.set_selected(Some(0));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert!(state.expanded_groups.contains(&sub_id));
    assert_eq!(cached_height_at(&state, 0), 2);
    assert!(cached_height_at(&state, 1) > 0);
    assert!(cached_height_at(&state, 2) > 0);

    // Collapse from a member refolds behind the subagent-anchored header.
    state.set_selected(Some(1));
    assert!(state.collapse_group_if_expanded());
    state.prepare_layout(80, 40);
    assert!(!state.expanded_groups.contains(&sub_id));
    assert_eq!(cached_height_at(&state, 1), 0);
}

#[test]
fn verb_group_subagent_mid_run_member_folds_and_round_trips() {
    let mut state = verb_state();
    let ids = push_reads(&mut state, 1);
    push_subagent(&mut state, "child-A");
    push_reads(&mut state, 1);
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 0));
    assert_eq!(header_count_at(&mut state, 0), 3);
    assert_eq!(
        cached_height_at(&state, 1),
        0,
        "subagent row folds into the run"
    );

    // Expand reveals the subagent row; collapse from it refolds the run.
    state.set_selected(Some(0));
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert!(cached_height_at(&state, 1) > 0);
    state.set_selected(Some(1));
    assert!(state.collapse_group_if_expanded());
    state.prepare_layout(80, 40);
    assert!(!state.expanded_groups.contains(&ids[0]));
    assert_eq!(cached_height_at(&state, 1), 0);
}

/// Ctrl+E's expand-all re-derives dense runs itself; a verb-claimed lone
/// read must neither start nor extend that walk, else the inserted id
/// misses the truncation header — the dense group stays hidden while the
/// read's fold spuriously pops open.
#[test]
fn expand_all_thinking_untruncates_dense_run_across_adjacent_verb_fold() {
    let mut state = verb_state();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);
    // A collapsed thought steers the toggle onto its expand leg (hidden
    // while `show_thinking` is off, so it stays outside both runs).
    push_thought(&mut state, "steer");
    let read_id = state.push_block(RenderBlock::read("lone.rs", None));
    let other_ids = push_tool_calls(&mut state, 12);
    state.prepare_layout(80, 40);
    assert!(verb_header_at(&state, 1), "lone read folds");
    assert_eq!(cached_height_at(&state, 3), 0, "dense member truncated");

    state.expand_all_thinking();
    state.prepare_layout(80, 40);

    assert!(
        state.expanded_groups.contains(&other_ids[0]),
        "expand-all must key the dense run on its truncation header"
    );
    assert!(
        !state.expanded_groups.contains(&read_id),
        "the claimed read must not head the dense walk"
    );
    assert!(
        cached_height_at(&state, 3) > 0,
        "dense members surface behind the collapse header"
    );
    let info = state.layout_cache.as_ref().unwrap().entries[1];
    assert!(info.verb_group_header && !info.group_collapse_header);
    assert_eq!(cached_height_at(&state, 1), 1, "read fold stays collapsed");
}

#[test]
fn toggle_group_expansion_and_collapse_round_trip() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    let ids = push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    // Entry 0 is the group header (hidden_count - 1 = 2)
    assert_eq!(header_count_at(&mut state, 0), 2);
    state.selected = Some(0);

    // Expand the group
    let toggled = state.toggle_group_expansion();
    assert!(toggled, "should toggle on group header");
    assert_eq!(
        state.selected, None,
        "selection should be cleared after expanding a group"
    );
    state.prepare_layout(80, 40);

    // Entry 0 = standalone collapse header (height=1).
    // Entries 1..6 keep their normal content.
    assert_eq!(cached_height_at(&state, 0), 1);
    assert_eq!(
        header_count_at(&mut state, 0),
        5,
        "collapse header count = visible entries below (group_len - 1)"
    );
    // Entry 0 IS a group header (Enter/click collapses).
    state.selected = Some(0);
    assert!(state.is_selected_group_header());
    // Entry 1 is NOT a group header (Enter/click folds it).
    state.selected = Some(1);
    assert!(!state.is_selected_group_header());
    for i in 1..6 {
        assert!(
            cached_height_at(&state, i) > 0,
            "entry {i} should be visible"
        );
    }
    assert!(
        state.layout_cache.as_ref().unwrap().entries[0].group_collapse_header,
        "first entry should be a collapse header"
    );
    assert!(state.expanded_groups.contains(&ids[0]));

    // Collapse via toggle on the collapse header (Enter/e key).
    state.selected = Some(0);
    let toggled_back = state.toggle_group_expansion();
    assert!(toggled_back, "should collapse via toggle_group_expansion");
    state.prepare_layout(80, 40);

    // Should be truncated again
    assert_eq!(header_count_at(&mut state, 0), 2);
    assert!(!state.expanded_groups.contains(&ids[0]));

    // Also test collapse from inside the group (entry 3):
    state.selected = Some(0);
    state.toggle_group_expansion(); // re-expand via expand header
    state.prepare_layout(80, 40);
    state.selected = Some(3);
    let collapsed = state.collapse_group_if_expanded();
    assert!(collapsed, "should collapse expanded group from entry 3");
    state.prepare_layout(80, 40);

    // Should be truncated again
    assert_eq!(header_count_at(&mut state, 0), 2);
    assert!(!state.expanded_groups.contains(&ids[0]));
}

#[test]
fn entry_content_hidden_by_group_tracks_truncation() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    // Non-group entry, then a group of 6: header + 2 hidden + 3 visible.
    state.push_block(RenderBlock::agent_message("intro"));
    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    assert!(!state.entry_content_hidden_by_group(0), "non-group entry");
    assert!(
        state.entry_content_hidden_by_group(1),
        "truncation header's own content is replaced by 'N more'"
    );
    assert!(state.entry_content_hidden_by_group(2), "hidden member");
    assert!(state.entry_content_hidden_by_group(3), "hidden member");
    for i in 4..7 {
        assert!(
            !state.entry_content_hidden_by_group(i),
            "visible tail entry {i}"
        );
    }
    assert!(
        !state.entry_content_hidden_by_group(99),
        "out-of-range index is not hidden"
    );

    // Expanded generic group: the collapse header still replaces its own
    // content; all members below are visible.
    state.selected = Some(1);
    state.toggle_group_expansion();
    state.prepare_layout(80, 40);
    assert!(
        state.entry_content_hidden_by_group(1),
        "generic collapse header's own content stays replaced when expanded"
    );
    for i in 2..7 {
        assert!(
            !state.entry_content_hidden_by_group(i),
            "expanded group member {i}"
        );
    }
}

#[test]
fn collapse_header_is_independent_selectable_entry() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    // Expand the group.
    state.selected = Some(0);
    state.toggle_group_expansion();
    state.prepare_layout(80, 40);

    // Entry 0 = standalone collapse header (height=1, own index).
    assert_eq!(cached_height_at(&state, 0), 1);
    assert!(state.layout_cache.as_ref().unwrap().entries[0].group_collapse_header);

    // Entry 0 IS a group header → Enter/e collapses the group.
    state.selected = Some(0);
    assert!(state.is_selected_group_header());

    // Entry 1 is NOT a group header → Enter/e folds it.
    state.selected = Some(1);
    assert!(!state.is_selected_group_header());

    // toggle_group_expansion on entry 0 → collapses group.
    state.selected = Some(0);
    assert!(state.toggle_group_expansion());
    state.prepare_layout(80, 40);
    assert!(
        !state
            .expanded_groups
            .contains(state.entries.get_index(0).unwrap().0)
    );

    // Re-expand, then verify entry 1 does NOT toggle group.
    state.selected = Some(0);
    state.toggle_group_expansion();
    state.prepare_layout(80, 40);
    state.selected = Some(1);
    assert!(
        !state.toggle_group_expansion(),
        "entry 1 must not toggle group"
    );
}

#[test]
fn expanded_group_with_thinking_first_shows_all_entries() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    // First entry is a finished thinking block (Collapsed), rest are tool calls.
    let id0 = state.push_block(RenderBlock::thinking_with_time("deep thoughts", 1300));
    if let Some(entry) = state.entries.get_mut(&id0) {
        entry.set_display_mode(DisplayMode::Collapsed);
    }
    let mut ids = vec![id0];
    for i in 0..5 {
        ids.push(state.push_block(RenderBlock::tool_call(format!("Tool{i}"), "info", true)));
    }
    state.prepare_layout(80, 40);

    // Collapsed: entry 0 = "2 more" header, entries 1-2 hidden, 3-5 visible.
    assert_eq!(header_count_at(&mut state, 0), 2);

    // Expand.
    state.selected = Some(0);
    state.toggle_group_expansion();
    state.prepare_layout(80, 40);

    // Entry 0 = standalone collapse header (height=1).
    // Entry 0's thinking content is behind the header (same as collapsed state).
    assert_eq!(cached_height_at(&state, 0), 1);

    // Entries 1-5 are visible with normal heights.
    for i in 1..6 {
        assert!(
            cached_height_at(&state, i) > 0,
            "entry {i} should be visible"
        );
    }

    // Entry 0 is a group header → e/Enter collapses the group.
    state.selected = Some(0);
    assert!(state.is_selected_group_header());

    // Entry 1 is the first tool call → e/Enter folds it.
    state.selected = Some(1);
    assert!(!state.is_selected_group_header());
}

#[test]
fn fixup_hidden_selection_moves_to_header() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    // Manually place selection on a hidden entry
    state.selected = Some(1); // height=0
    state.fixup_hidden_selection();

    // Should move to entry 0 (the group header)
    assert_eq!(state.selected, Some(0));
}

#[test]
fn collapse_all_clears_expanded_groups() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    // Expand the group
    state.selected = Some(0);
    state.toggle_group_expansion();
    assert!(!state.expanded_groups.is_empty());

    // Collapse all
    state.collapse_all();
    assert!(
        state.expanded_groups.is_empty(),
        "collapse_all should clear expanded_groups"
    );
}

#[test]
fn clear_resets_expanded_groups() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);
    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);
    state.selected = Some(0);
    state.toggle_group_expansion();
    assert!(!state.expanded_groups.is_empty());

    state.clear();
    assert!(
        state.expanded_groups.is_empty(),
        "clear should reset expanded_groups"
    );
}

#[test]
fn toggle_on_non_header_returns_false() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);

    push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);

    // Select a visible tail entry (not the header)
    state.selected = Some(4);
    let toggled = state.toggle_group_expansion();
    assert!(!toggled, "toggle on non-header should return false");
}

#[test]
fn remove_entry_cleans_expanded_groups() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    state.set_appearance(appearance);
    let ids = push_tool_calls(&mut state, 6);
    state.prepare_layout(80, 40);
    state.selected = Some(0);
    state.toggle_group_expansion();
    assert!(state.expanded_groups.contains(&ids[0]));

    state.remove_entry(ids[0]);
    assert!(
        !state.expanded_groups.contains(&ids[0]),
        "removed entry should be cleaned from expanded_groups"
    );
}

#[test]
fn expanded_group_shows_all_entries_including_first() {
    let mut state = ScrollbackState::new();
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 10;
    state.set_appearance(appearance);

    // Create a large group (>100 entries)
    let count = 120;
    push_tool_calls(&mut state, count);
    state.prepare_layout(80, 40);

    // Should be truncated: 110 hidden + 10 visible, header count = 109
    assert_eq!(header_count_at(&mut state, 0), count as u16 - 10 - 1);

    // Expand the group
    state.selected = Some(0);
    let toggled = state.toggle_group_expansion();
    assert!(toggled);
    state.prepare_layout(80, 40);

    // Entry 0 = standalone collapse header (height=1).
    // Entries 1..count are visible with normal heights.
    assert_eq!(cached_height_at(&state, 0), 1);
    assert_eq!(
        header_count_at(&mut state, 0),
        (count - 1) as u16,
        "collapse header count = visible entries below (group_len - 1)"
    );
    assert!(
        state.layout_cache.as_ref().unwrap().entries[0].group_collapse_header,
        "first entry should be collapse header"
    );
    for i in 1..count {
        let h = cached_height_at(&state, i);
        assert!(h > 0, "entry {i} should be visible, got height={h}");
    }
}
