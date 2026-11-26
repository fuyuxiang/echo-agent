use super::*;
use crate::key;

/// Shorthand for creating a state with follow enabled but search disabled.
/// Tests that need search should use `ListPaneConfig::streaming()` directly.
fn new_streaming(wrap: WrapMode, follow: bool) -> ListPaneState {
    ListPaneState::new_with_config(
        wrap,
        follow,
        ListPaneConfig {
            follow_enabled: true,
            search_enabled: false,
            ..ListPaneConfig::default()
        },
    )
}

/// A simple test item.
#[derive(Debug, Clone)]
struct TestItem {
    id: u64,
    height: u16,
    selectable: bool,
    text: String,
    styled: ratatui::text::Line<'static>,
}

impl TestItem {
    fn new(id: u64) -> Self {
        let text = format!("item-{id}");
        Self {
            id,
            height: 1,
            selectable: true,
            styled: ratatui::text::Line::from(text.clone()),
            text,
        }
    }

    fn with_text(mut self, text: &str) -> Self {
        self.text = text.to_string();
        self.styled = ratatui::text::Line::from(text.to_string());
        self
    }

    fn with_height(mut self, h: u16) -> Self {
        self.height = h;
        self
    }

    fn not_selectable(mut self) -> Self {
        self.selectable = false;
        self
    }
}

impl ListItem for TestItem {
    fn content(&self) -> &ratatui::text::Line<'_> {
        &self.styled
    }
    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }
    fn stable_id(&self) -> u64 {
        self.id
    }
    fn is_selectable(&self) -> bool {
        self.selectable
    }
    fn search_text(&self) -> &str {
        &self.text
    }
}

// -- Scroll tests ---------------------------------------------------------

#[test]
fn scroll_basics() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    assert_eq!(state.total_height(), 20);
    assert_eq!(state.scroll_offset(), 0);

    state.scroll_down(5);
    assert_eq!(state.scroll_offset(), 5);

    state.scroll_up(3);
    assert_eq!(state.scroll_offset(), 2);

    // Can't scroll past content
    state.scroll_down(100);
    assert_eq!(state.scroll_offset(), 10); // 20 - 10

    state.scroll_up(100);
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn scroll_half_page() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.half_page_down(&items);
    assert_eq!(state.scroll_offset(), 5);

    state.half_page_up(&items);
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn goto_top_bottom() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.goto_bottom();
    assert_eq!(state.scroll_offset(), 10);
    assert!(state.follow_mode);

    state.goto_top();
    assert_eq!(state.scroll_offset(), 0);
    assert!(!state.follow_mode);
}

#[test]
fn follow_mode_auto_scrolls() {
    let mut items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 5);
    // follow mode → scroll to bottom
    assert_eq!(state.scroll_offset(), 5); // 10 - 5

    // Add more items
    items.extend((10..15).map(TestItem::new));
    state.prepare_layout(&items, 80, 5);
    assert_eq!(state.scroll_offset(), 10); // 15 - 5

    // Manual scroll breaks follow mode
    state.scroll_up(3);
    assert!(!state.follow_mode);
    assert_eq!(state.scroll_offset(), 7);
}

#[test]
fn scroll_empty_list() {
    let items: Vec<TestItem> = vec![];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.scroll_offset(), 0);
    assert_eq!(state.total_height(), 0);

    // Scrolling on empty list is a no-op
    state.scroll_down(5);
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn scroll_content_fits_viewport() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);
    // Content (5) fits in viewport (10) → scroll is always 0
    state.scroll_down(5);
    assert_eq!(state.scroll_offset(), 0);
}

// -- Selection tests ------------------------------------------------------

#[test]
fn select_next_prev() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Auto-selected first item
    assert_eq!(state.selected_index(), Some(0));
    assert_eq!(state.selected_id(), Some(0));

    // Select next
    state.select_next(&items);
    assert_eq!(state.selected_index(), Some(1));
    assert_eq!(state.selected_id(), Some(1));

    // Select prev
    state.select_prev(&items);
    assert_eq!(state.selected_index(), Some(0));
}

#[test]
fn select_skips_non_selectable() {
    let items = vec![
        TestItem::new(0),
        TestItem::new(1).not_selectable(),
        TestItem::new(2),
        TestItem::new(3).not_selectable(),
        TestItem::new(4),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Auto-selected first selectable (item 0)
    assert_eq!(state.selected_index(), Some(0));

    state.select_next(&items);
    // Skips item 1 (not selectable) → item 2
    assert_eq!(state.selected_index(), Some(2));

    state.select_next(&items);
    // Skips item 3 → item 4
    assert_eq!(state.selected_index(), Some(4));

    state.select_prev(&items);
    // Back to 2 (skips 3)
    assert_eq!(state.selected_index(), Some(2));
}

#[test]
fn select_first_last() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // G / select_last now engages follow (no cursor).
    state.select_last(&items);
    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None); // no cursor in follow

    // g / select_first exits follow, selects first.
    state.select_first(&items);
    assert!(!state.follow_mode);
    assert_eq!(state.selected_index(), Some(0));
}

#[test]
fn selection_survives_insert() {
    let mut items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Select item with id=2
    state.select_at(2, &items);
    assert_eq!(state.selected_id(), Some(2));

    // Insert at front — item id=2 is now at physical index 3
    items.insert(0, TestItem::new(99));
    state.prepare_layout(&items, 80, 10);

    // Selection should resolve to the new index of id=2
    assert_eq!(state.selected_id(), Some(2));
    assert_eq!(state.selected_index(), Some(3));
}

#[test]
fn selection_survives_removal_of_other() {
    let mut items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.select_at(3, &items);
    assert_eq!(state.selected_id(), Some(3));

    // Remove item at index 1 (id=1) — item id=3 moves to index 2
    items.remove(1);
    state.prepare_layout(&items, 80, 10);

    assert_eq!(state.selected_id(), Some(3));
    assert_eq!(state.selected_index(), Some(2));
}

#[test]
fn selection_clears_when_selected_removed() {
    let mut items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.select_at(2, &items);
    assert_eq!(state.selected_id(), Some(2));

    // Remove the selected item
    items.remove(2);
    state.prepare_layout(&items, 80, 10);

    // ID 2 is gone — auto-select picks the first item instead.
    assert_ne!(state.selected_id(), Some(2));
    assert!(state.selected_index().is_some());
}

// -- Filter + selection tests ---------------------------------------------

#[test]
fn select_with_filter() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alphabet"),
        TestItem::new(3).with_text("gamma"),
    ];
    let mut state = new_streaming(WrapMode::NoWrap, false);

    // Filter "alph" → items 0 (alpha) and 2 (alphabet)
    state.set_filter(Some(FilterMatcher::substring("alph")));
    state.prepare_layout(&items, 80, 10);

    assert_eq!(state.visible_count(), 2);

    // Auto-select picked visible item 0 (physical 0, "alpha").
    assert_eq!(state.selected_index(), Some(0));
    assert_eq!(state.selected_id(), Some(0));

    // select_next → visible item 1 (physical item 2, "alphabet")
    state.select_next(&items);
    assert_eq!(state.selected_index(), Some(1));
    assert_eq!(state.selected_id(), Some(2));

    // select_prev → back to visible item 0
    state.select_prev(&items);
    assert_eq!(state.selected_index(), Some(0));
    assert_eq!(state.selected_id(), Some(0));

    // select_last → engages follow (no cursor)
    state.select_last(&items);
    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None);

    // select_first → exits follow, selects visible item 0
    state.select_first(&items);
    assert!(!state.follow_mode);
    assert_eq!(state.selected_index(), Some(0));
    assert_eq!(state.selected_id(), Some(0));

    // select_at(1) → visible item 1
    state.select_at(1, &items);
    assert_eq!(state.selected_index(), Some(1));
    assert_eq!(state.selected_id(), Some(2));
}

// -- Visible range tests --------------------------------------------------

#[test]
fn visible_range_fixed_height() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    assert_eq!(state.visible_range(), 0..5);
    assert_eq!(state.first_item_skip_rows(), 0);

    state.scroll_down(3);
    assert_eq!(state.visible_range(), 3..8);
}

#[test]
fn visible_range_variable_height() {
    // Heights: 3, 1, 2, 4, 1  → prefix_sums: [0, 3, 4, 6, 10, 11]
    let items = vec![
        TestItem::new(0).with_height(3),
        TestItem::new(1).with_height(1),
        TestItem::new(2).with_height(2),
        TestItem::new(3).with_height(4),
        TestItem::new(4).with_height(1),
    ];
    let mut state = ListPaneState::new(WrapMode::Wrap, false);
    state.prepare_layout(&items, 80, 5);

    // Viewport is 5 lines, scroll_offset=0
    // Items visible: 0 (y=0, h=3), 1 (y=3, h=1), 2 (y=4, h=2 but only 1 line visible)
    assert_eq!(state.visible_range(), 0..3);

    // Scroll down by 1 — now y=1 is the top
    state.scroll_down(1);
    // First visible item is still 0 (its y range is 0..3, and 1 is within it)
    assert_eq!(state.visible_range(), 0..3);
    assert_eq!(state.first_item_skip_rows(), 1);
}

// -- Filter tests ---------------------------------------------------------

#[test]
fn filter_reduces_visible_items() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alphabet"),
        TestItem::new(3).with_text("gamma"),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);

    // No filter → all 4 visible
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 4);
    assert_eq!(state.total_height(), 4);

    // Filter "alph" → items 0 and 2
    state.set_filter(Some(FilterMatcher::substring("alph")));
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 2);
    assert_eq!(state.total_height(), 2);
    assert_eq!(state.filter().unwrap().match_count(), 2);

    // Clear filter
    state.set_filter(None);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 4);
}

// -- Wrap mode tests ------------------------------------------------------

#[test]
fn wrap_mode_variable_heights() {
    let items = vec![
        TestItem::new(0).with_height(3),
        TestItem::new(1).with_height(2),
        TestItem::new(2).with_height(1),
    ];
    let mut state = ListPaneState::new(WrapMode::Wrap, false);
    state.prepare_layout(&items, 80, 10);

    assert_eq!(state.total_height(), 6); // 3 + 2 + 1
}

#[test]
fn nowrap_mode_all_height_one() {
    let items = vec![
        TestItem::new(0).with_height(3),
        TestItem::new(1).with_height(2),
        TestItem::new(2).with_height(1),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // NoWrap forces height 1 regardless of desired_height
    assert_eq!(state.total_height(), 3);
}

// -- Keyboard input tests -------------------------------------------------

#[test]
fn key_j_k_selects() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    let j = key!('j').to_key_event();
    let k = key!('k').to_key_event();

    // Auto-selected item 0.
    assert_eq!(state.selected_index(), Some(0));

    assert!(state.handle_key_event(&j, &items));
    assert_eq!(state.selected_index(), Some(1));

    assert!(state.handle_key_event(&k, &items));
    assert_eq!(state.selected_index(), Some(0));
}

#[test]
fn key_arrow_selects() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    // Auto-selected item 0.
    assert_eq!(state.selected_index(), Some(0));

    assert!(state.handle_key_event(&key!(Down).to_key_event(), &items));
    assert_eq!(state.selected_index(), Some(1));

    assert!(state.handle_key_event(&key!(Up).to_key_event(), &items));
    assert_eq!(state.selected_index(), Some(0));
}

#[test]
fn key_ctrl_j_k_scrolls_viewport() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    assert_eq!(state.scroll_offset(), 0);
    assert!(state.handle_key_event(&key!('j', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 1);

    assert!(state.handle_key_event(&key!('k', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn key_ctrl_d_u_half_page() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    assert!(state.handle_key_event(&key!('d', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 5); // half of 10

    assert!(state.handle_key_event(&key!('u', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn key_page_up_down() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    assert!(state.handle_key_event(&key!(PageDown).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 10);

    assert!(state.handle_key_event(&key!(PageUp).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn key_g_and_shift_g() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    // G → engage follow (no cursor)
    assert!(state.handle_key_event(&key!('G').to_key_event(), &items));
    assert_eq!(state.selected_index(), None);
    assert!(state.follow_mode);

    // g → exit follow, select first
    assert!(state.handle_key_event(&key!('g').to_key_event(), &items));
    assert_eq!(state.selected_index(), Some(0));
    assert!(!state.follow_mode);
}

#[test]
fn key_home_end() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    // End → engage follow (no cursor)
    assert!(state.handle_key_event(&key!(End).to_key_event(), &items));
    assert_eq!(state.selected_index(), None);
    assert!(state.follow_mode);

    // Home → exit follow, select first
    assert!(state.handle_key_event(&key!(Home).to_key_event(), &items));
    assert_eq!(state.selected_index(), Some(0));
    assert!(!state.follow_mode);
}

#[test]
fn key_w_toggles_wrap_mode() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    let w = key!('w').to_key_event();

    assert_eq!(state.wrap_mode(), WrapMode::NoWrap);
    assert!(state.handle_key_event(&w, &items));
    assert_eq!(state.wrap_mode(), WrapMode::Wrap);
    assert!(state.handle_key_event(&w, &items));
    assert_eq!(state.wrap_mode(), WrapMode::NoWrap);
}

#[test]
fn unrecognized_key_returns_false() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // 'x' is not a navigation key
    assert!(!state.handle_key_event(&key!('x').to_key_event(), &items));
}

// -- Selection follows scroll (vim/lnav screen-y preservation) ----------

#[test]
fn ctrl_d_selection_stays_at_same_screen_y() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Select item 3 (screen-y = 3 since scroll_offset = 0)
    state.select_at(3, &items);
    assert_eq!(state.selected_index(), Some(3));

    // Ctrl-d scrolls viewport down by 5 lines.
    // Selection should move from 3 to 8 (same screen-y = 3).
    assert!(state.handle_key_event(&key!('d', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 5);
    assert_eq!(state.selected_index(), Some(8)); // 3 + 5
}

#[test]
fn ctrl_u_selection_stays_at_same_screen_y() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to offset 10, select item 13 (screen-y = 3)
    state.scroll_down(10);
    state.select_at(13, &items);
    assert_eq!(state.selected_index(), Some(13));

    // Ctrl-u scrolls up by 5.
    // Selection should move from 13 to 8 (same screen-y = 3).
    assert!(state.handle_key_event(&key!('u', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 5);
    assert_eq!(state.selected_index(), Some(8)); // 13 - 5
}

#[test]
fn scroll_without_explicit_selection_uses_auto_selected() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Auto-select picks the first item (follow_mode = false).
    assert_eq!(state.selected_index(), Some(0));
    // Ctrl-d scrolls — selection moves with it.
    assert!(state.handle_key_event(&key!('d', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 5);
    assert_eq!(state.selected_index(), Some(5));
}

#[test]
fn scroll_lines_from_mouse_wheel() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Select item 5
    state.select_at(5, &items);

    // Mouse wheel down 3 — selection follows at same screen-y.
    state.scroll_lines(3, &items);
    assert_eq!(state.scroll_offset(), 3);
    assert_eq!(state.selected_index(), Some(8)); // 5 + 3

    // Mouse wheel up 2
    state.scroll_lines(-2, &items);
    assert_eq!(state.scroll_offset(), 1);
    assert_eq!(state.selected_index(), Some(6)); // 8 - 2
}

#[test]
fn scroll_lines_small_list_stable() {
    // 8 items, 5 visible, 3 below. Selection follows viewport.
    let items: Vec<TestItem> = (0..8).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);
    assert_eq!(state.selected_index(), Some(0));

    // Mouse wheel down 1 — selection follows from 0 to 1.
    state.scroll_lines(1, &items);
    assert_eq!(state.scroll_offset(), 1);
    assert_eq!(state.selected_index(), Some(1));

    // prepare_layout should NOT undo the scroll (count_same guard).
    state.prepare_layout(&items, 80, 5);
    assert_eq!(state.scroll_offset(), 1);
    assert_eq!(state.selected_index(), Some(1));

    // Another scroll — to item 2.
    state.scroll_lines(1, &items);
    assert_eq!(state.scroll_offset(), 2);
    assert_eq!(state.selected_index(), Some(2));

    // Stable after prepare_layout.
    state.prepare_layout(&items, 80, 5);
    assert_eq!(state.scroll_offset(), 2);
    assert_eq!(state.selected_index(), Some(2));
}

#[test]
fn scroll_lines_noop_at_top_edge() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.selected_index(), Some(0));

    // Mouse wheel up at top — viewport can't scroll, selection stays.
    state.scroll_lines(-3, &items);
    assert_eq!(state.scroll_offset(), 0);
    assert_eq!(state.selected_index(), Some(0));
}

#[test]
fn scroll_lines_noop_at_bottom_edge() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to max offset.
    state.scroll_down(10);
    state.select_at(15, &items);

    // Mouse wheel down at bottom — viewport can't scroll, selection stays.
    state.scroll_lines(3, &items);
    assert_eq!(state.scroll_offset(), 10);
    assert_eq!(state.selected_index(), Some(15));
}

#[test]
fn scroll_does_not_panic_when_items_cleared_before_relayout() {
    // Layout still has rows from the previous frame; the model was emptied
    // before the next prepare_layout (viewer rebuild / rewind).
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);
    state.select_at(4, &items);
    assert_eq!(state.selected_index(), Some(4));

    let empty: &[TestItem] = &[];
    state.half_page_down(empty);
    state.scroll_lines(3, empty);
    state.select_next(empty);
    state.select_prev(empty);
    state.select_first(empty);
    state.select_last(empty);
    state.select_at(4, empty);
    assert!(!state.select_at_y(4, empty));

    let short: Vec<TestItem> = (0..2).map(TestItem::new).collect();
    state.half_page_down(&short);
    state.scroll_lines(1, &short);
}

#[test]
fn click_bottom_row_stable_after_append() {
    // Click the bottom visible row, then append items.
    // Selection and scroll should NOT be yanked by ensure_selected_visible.
    let mut items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Click the last visible item (item 9 at bottom of viewport [0..10)).
    state.select_at(9, &items);
    assert_eq!(state.selected_index(), Some(9));
    let scroll_before = state.scroll_offset();

    // Append items (simulates live tracing).
    items.extend((20..25).map(TestItem::new));
    state.prepare_layout(&items, 80, 10);

    // Selection and scroll should be unchanged (no margin yank).
    assert_eq!(state.selected_index(), Some(9));
    assert_eq!(state.scroll_offset(), scroll_before);
}

#[test]
fn ctrl_j_k_selection_follows_at_screen_y() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Select item 4 (screen-y = 4)
    state.select_at(4, &items);

    // Ctrl-j scrolls down 1 line
    assert!(state.handle_key_event(&key!('j', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 1);
    assert_eq!(state.selected_index(), Some(5)); // 4 + 1, screen-y still 4

    // Ctrl-k scrolls up 1 line
    assert!(state.handle_key_event(&key!('k', CONTROL).to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 0);
    assert_eq!(state.selected_index(), Some(4)); // back to 4
}

// -- Dirty flag / incremental append tests --------------------------------

#[test]
fn prepare_layout_skips_rebuild_when_clean() {
    // With Wrap mode + variable heights, calling prepare_layout with
    // unchanged inputs should reuse the cache (same total_height).
    let items = vec![
        TestItem::new(0).with_height(3),
        TestItem::new(1).with_height(2),
    ];
    let mut state = ListPaneState::new(WrapMode::Wrap, false);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.total_height(), 5);

    // Call again — same width, same count, same mode → should not rebuild.
    // (We can't directly observe "no rebuild" but we verify cache is valid.)
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.total_height(), 5);
}

#[test]
fn prepare_layout_incremental_append() {
    let mut items = vec![
        TestItem::new(0).with_height(3),
        TestItem::new(1).with_height(2),
    ];
    let mut state = ListPaneState::new(WrapMode::Wrap, false);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.total_height(), 5);

    // Append an item → incremental path
    items.push(TestItem::new(2).with_height(4));
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.total_height(), 9); // 3 + 2 + 4
    assert_eq!(state.visible_count(), 3);
}

#[test]
fn prepare_layout_full_rebuild_on_width_change() {
    let items = vec![
        TestItem::new(0).with_height(3),
        TestItem::new(1).with_height(2),
    ];
    let mut state = ListPaneState::new(WrapMode::Wrap, false);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.total_height(), 5);

    // Width change → full rebuild (heights depend on width)
    state.prepare_layout(&items, 40, 10);
    assert_eq!(state.total_height(), 5);
}

#[test]
fn prepare_layout_full_rebuild_on_filter_change() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alphabet"),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 3);

    state.set_filter(Some(FilterMatcher::substring("alph")));
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 2);
}

#[test]
fn prepare_layout_handles_item_eviction() {
    // Simulate eviction: items shrink between frames (front removed).
    let mut items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 20);

    // Select item 10 (stable id 10).
    state.select_at(10, &items);
    assert_eq!(state.selected_id(), Some(10));

    // Evict first 5 items. Item id=10 is now at index 5.
    items.drain(0..5);
    assert_eq!(items.len(), 15);
    state.prepare_layout(&items, 80, 10);

    // Selection should survive (stable id).
    assert_eq!(state.selected_id(), Some(10));
    assert_eq!(state.selected_index(), Some(5)); // shifted
    assert_eq!(state.visible_count(), 15);
    assert_eq!(state.total_height(), 15);
}

#[test]
fn prepare_layout_refilters_when_items_shrink_under_active_filter() {
    // Filter leaves a gap; shrink must drop stale high physical indices.
    let mut items = vec![
        TestItem::new(0).with_text("row-0"),
        TestItem::new(1).with_text("row-1"),
        TestItem::new(2).with_text("zzz"),
        TestItem::new(3).with_text("row-3"),
        TestItem::new(4).with_text("row-4"),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.set_filter(Some(FilterMatcher::substring("row")));
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 4);

    items.truncate(2);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.visible_count(), 2);
    assert_eq!(state.to_physical(0), 0);
    assert_eq!(state.to_physical(1), 1);
}

#[test]
fn prepare_layout_eviction_with_wrap_mode() {
    // Wrap mode: height cache must be fully recomputed after eviction.
    let mut items: Vec<TestItem> = (0..10)
        .map(|i| TestItem::new(i).with_height(if i % 2 == 0 { 2 } else { 1 }))
        .collect();
    let mut state = ListPaneState::new(WrapMode::Wrap, false);
    state.prepare_layout(&items, 80, 10);
    // Heights: 2,1,2,1,2,1,2,1,2,1 = 15
    assert_eq!(state.total_height(), 15);

    // Evict first 3 items (heights 2,1,2 = 5 removed).
    items.drain(0..3);
    state.prepare_layout(&items, 80, 10);
    // Remaining: 1,2,1,2,1,2,1 = 10
    assert_eq!(state.total_height(), 10);
    assert_eq!(state.visible_count(), 7);
}

// -- Center selected tests ------------------------------------------------

#[test]
fn center_selected_places_item_mid_viewport() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.select_at(15, &items);
    state.center_selected();
    // Item 15 should be approximately centered: scroll_offset ≈ 15 - 5 = 10
    assert_eq!(state.scroll_offset(), 10);
}

#[test]
fn center_selected_at_top_clamps() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.select_at(2, &items);
    state.center_selected();
    // Item 2 can't be centered (would need scroll_offset -3), clamps to 0
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn key_z_centers_selected() {
    let items: Vec<TestItem> = (0..30).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.select_at(15, &items);
    assert!(state.handle_key_event(&key!('z').to_key_event(), &items));
    assert_eq!(state.scroll_offset(), 10);
}

// -- Click-to-select tests ------------------------------------------------

#[test]
fn select_at_y_selectable() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    assert!(state.select_at_y(3, &items));
    assert_eq!(state.selected_index(), Some(3));
    assert_eq!(state.selected_id(), Some(3));
}

#[test]
fn select_at_y_non_selectable_returns_false() {
    let items = vec![
        TestItem::new(0),
        TestItem::new(1).not_selectable(),
        TestItem::new(2),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Auto-selected item 0.
    assert_eq!(state.selected_index(), Some(0));

    // Click on non-selectable item returns false, selection unchanged.
    assert!(!state.select_at_y(1, &items));
    assert_eq!(state.selected_index(), Some(0));
}

// -- Scroll past non-selectable (viewport-constrained) --------------------

#[test]
fn scroll_past_non_selectable_stays_in_viewport() {
    // Items: 0, 1, 2(non-sel), 3, 4, 5, 6, 7, 8, 9
    let mut items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    items[2] = TestItem::new(2).not_selectable();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    // Select item 1 (screen-y = 1, scroll_offset = 0)
    state.select_at(1, &items);

    // Mouse wheel down 1 — selection follows at screen-y.
    // Target virtual-y = 1+1 = 2 → item 2 (non-sel) → finds item 3.
    state.scroll_lines(1, &items);
    assert_eq!(state.scroll_offset(), 1);
    let sel = state.selected_index().unwrap();
    // Selection must be within viewport [1..6) and selectable.
    assert!(
        (1..6).contains(&sel),
        "selection should be in viewport, got {sel}"
    );
    // Should pick item 3 (forward from non-selectable 2).
    assert_eq!(sel, 3);
}

// -- Ctrl-d/u edge cases: cursor continues when viewport is clamped -----

#[test]
fn ctrl_d_at_bottom_moves_cursor_past_viewport_clamp() {
    // 20 items, viewport 10.  Max scroll = 10.
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to offset 8, select item 11 (screen-y = 3).
    state.scroll_down(8);
    state.select_at(11, &items);
    assert_eq!(state.selected_index(), Some(11));

    // Ctrl-d: half-page = 5.  Viewport wants to go to 13, clamped to 10.
    // Actual scroll = 2.  Leftover = 3.
    // Target virtual-y = (10 + 3) + 3 = 16 → item 16.
    state.half_page_down(&items);
    assert_eq!(state.scroll_offset(), 10); // clamped at max
    assert_eq!(state.selected_index(), Some(16)); // cursor kept going
}

#[test]
fn ctrl_u_at_top_moves_cursor_past_viewport_clamp() {
    // 20 items, viewport 10.
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to offset 3, select item 6 (screen-y = 3).
    state.scroll_down(3);
    state.select_at(6, &items);
    assert_eq!(state.selected_index(), Some(6));

    // Ctrl-u: half-page = 5.  Viewport wants to go to -2, clamped to 0.
    // Actual scroll = -3.  Leftover = -2.
    // Target virtual-y = (0 + 3) - 2 = 1 → item 1.
    state.half_page_up(&items);
    assert_eq!(state.scroll_offset(), 0); // clamped at min
    assert_eq!(state.selected_index(), Some(1)); // cursor kept going
}

#[test]
fn ctrl_d_at_very_bottom_clamps_cursor_to_last_selectable() {
    // Already at max scroll. Ctrl-d can't scroll at all.
    // Cursor should jump toward the last selectable item.
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to max (offset 10), select item 15 (screen-y = 5).
    state.scroll_down(10);
    state.select_at(15, &items);
    assert_eq!(state.scroll_offset(), 10);

    // Ctrl-d: half-page = 5.  No scroll possible.  Leftover = 5.
    // Target virtual-y = (10 + 5) + 5 = 20 → clamped to item 19.
    state.half_page_down(&items);
    assert_eq!(state.scroll_offset(), 10);
    assert_eq!(state.selected_index(), Some(19));
}

#[test]
fn ctrl_u_at_very_top_clamps_cursor_to_first_selectable() {
    // Already at offset 0.  Ctrl-u can't scroll at all.
    // Cursor should jump toward the first selectable item.
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // At offset 0, select item 5 (screen-y = 5).
    state.select_at(5, &items);
    assert_eq!(state.scroll_offset(), 0);

    // Ctrl-u: half-page = 5.  No scroll possible.  Leftover = -5.
    // Target virtual-y = (0 + 5) - 5 = 0 → item 0.
    state.half_page_up(&items);
    assert_eq!(state.scroll_offset(), 0);
    assert_eq!(state.selected_index(), Some(0));
}

#[test]
fn ctrl_d_skips_non_selectable_at_end() {
    // Last 3 items are non-selectable. Cursor should stop at the last
    // selectable item, not get stuck on a separator.
    let mut items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    items[17] = TestItem::new(17).not_selectable();
    items[18] = TestItem::new(18).not_selectable();
    items[19] = TestItem::new(19).not_selectable();

    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to max, select item 14.
    state.scroll_down(10);
    state.select_at(14, &items);

    // Ctrl-d: leftover pushes target past item 19.
    // Should land on item 16 (last selectable).
    state.half_page_down(&items);
    assert_eq!(state.selected_index(), Some(16));
}

#[test]
fn ctrl_u_skips_non_selectable_at_start() {
    // First 3 items are non-selectable.
    let mut items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    items[0] = TestItem::new(0).not_selectable();
    items[1] = TestItem::new(1).not_selectable();
    items[2] = TestItem::new(2).not_selectable();

    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Select item 5 at offset 0.
    state.select_at(5, &items);

    // Ctrl-u: target goes to y=0 → items 0,1,2 non-selectable.
    // Should find item 3 (first selectable).
    state.half_page_up(&items);
    assert_eq!(state.selected_index(), Some(3));
}

// -- ListMatcher tests ----------------------------------------------------

#[test]
fn matcher_substring_builds_match_indices() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alphabet"),
        TestItem::new(3).with_text("gamma"),
    ];
    let mut m = ListMatcher::new("alph", QueryKind::Substring, MatchMode::Filter);
    m.rebuild_matches(&items);
    assert_eq!(m.match_indices, vec![0, 2]);
    assert_eq!(m.match_count(), 2);
}

#[test]
fn matcher_regex_builds_match_indices() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("gamma"),
    ];
    let mut m = ListMatcher::new("^(alpha|gamma)$", QueryKind::Regex, MatchMode::Filter);
    m.rebuild_matches(&items);
    assert_eq!(m.match_indices, vec![0, 2]);
}

#[test]
fn matcher_bad_regex_matches_nothing() {
    let items = vec![TestItem::new(0).with_text("alpha")];
    let mut m = ListMatcher::new("[invalid", QueryKind::Regex, MatchMode::Filter);
    assert!(m.is_error());
    m.rebuild_matches(&items);
    assert!(m.match_indices.is_empty()); // bad regex matches nothing
}

#[test]
fn next_match_wraps_around() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alpha-2"),
        TestItem::new(3).with_text("gamma"),
        TestItem::new(4).with_text("alpha-3"),
    ];
    let mut m = ListMatcher::new("alpha", QueryKind::Substring, MatchMode::Filter);
    m.rebuild_matches(&items);
    assert_eq!(m.match_indices, vec![0, 2, 4]);

    // Starting at pi=0, next should go to pi=2.
    assert_eq!(m.next_match_after(0), Some(2));
    assert_eq!(m.current_match, Some(1));

    // From pi=2, next should go to pi=4.
    assert_eq!(m.next_match_after(2), Some(4));
    assert_eq!(m.current_match, Some(2));

    // From pi=4, next should wrap to pi=0.
    assert_eq!(m.next_match_after(4), Some(0));
    assert_eq!(m.current_match, Some(0));
}

#[test]
fn prev_match_wraps_around() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alpha-2"),
        TestItem::new(3).with_text("gamma"),
        TestItem::new(4).with_text("alpha-3"),
    ];
    let mut m = ListMatcher::new("alpha", QueryKind::Substring, MatchMode::Filter);
    m.rebuild_matches(&items);
    assert_eq!(m.match_indices, vec![0, 2, 4]);

    // Starting at pi=4, prev should go to pi=2.
    assert_eq!(m.prev_match_before(4), Some(2));
    assert_eq!(m.current_match, Some(1));

    // From pi=2, prev should go to pi=0.
    assert_eq!(m.prev_match_before(2), Some(0));
    assert_eq!(m.current_match, Some(0));

    // From pi=0, prev should wrap to pi=4.
    assert_eq!(m.prev_match_before(0), Some(4));
    assert_eq!(m.current_match, Some(2));
}

#[test]
fn search_mode_all_items_visible() {
    // Search mode: all items visible, matches highlighted but not filtered.
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alphabet"),
        TestItem::new(3).with_text("gamma"),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);

    // Set search matcher (not filter).
    state.set_matcher(Some(ListMatcher::new(
        "alph",
        QueryKind::Substring,
        MatchMode::Search,
    )));
    state.prepare_layout(&items, 80, 10);

    // All 4 items should be visible (search doesn't hide).
    assert_eq!(state.visible_count(), 4);
    assert_eq!(state.total_height(), 4);

    // But the matcher should have match_indices for items 0 and 2.
    let m = state.matcher().unwrap();
    assert_eq!(m.match_indices, vec![0, 2]);
}

#[test]
fn next_match_selects_and_scrolls() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("gamma"),
        TestItem::new(3).with_text("alpha-2"),
        TestItem::new(4).with_text("delta"),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.set_matcher(Some(ListMatcher::new(
        "alpha",
        QueryKind::Substring,
        MatchMode::Search,
    )));
    state.prepare_layout(&items, 80, 10);

    // Auto-selected item 0.
    assert_eq!(state.selected_index(), Some(0));

    // n → next match = item 3 ("alpha-2").
    state.next_match(&items);
    assert_eq!(state.selected_index(), Some(3));
    assert_eq!(state.selected_id(), Some(3));

    // n again → wraps to item 0 ("alpha").
    state.next_match(&items);
    assert_eq!(state.selected_index(), Some(0));

    // N (prev) → wraps to item 3.
    state.prev_match(&items);
    assert_eq!(state.selected_index(), Some(3));
}

// -- Follow mode: one-past and overscroll tests --------------------------

#[test]
fn j_one_past_engages_follow() {
    // j on last item twice → follow.
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    // Navigate to last item.
    for _ in 0..9 {
        state.select_next(&items);
    }
    assert_eq!(state.selected_index(), Some(9));
    assert!(!state.follow_mode); // at last item, but NOT follow yet

    // First j at end → at_content_edge = true, no mode change.
    state.select_next(&items);
    assert!(!state.follow_mode);
    assert_eq!(state.selected_index(), Some(9)); // still there

    // Second j at end → engage follow.
    state.select_next(&items);
    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None); // no cursor in follow
}

#[test]
fn j_one_past_resets_when_new_items_arrive() {
    // j at bottom, items arrive, j moves to new item, not follow.
    let mut items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Navigate to last item.
    for _ in 0..4 {
        state.select_next(&items);
    }
    assert_eq!(state.selected_index(), Some(4));

    // First j at end → at_content_edge.
    state.select_next(&items);
    assert!(!state.follow_mode);

    // New items arrive.
    items.push(TestItem::new(5));
    state.prepare_layout(&items, 80, 10);

    // j → moves to new item 5 (resets edge state).
    state.select_next(&items);
    assert_eq!(state.selected_index(), Some(5));
    assert!(!state.follow_mode);
}

#[test]
fn ctrl_d_one_past_engages_follow() {
    // ctrl-d at bottom twice → follow.
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // First ctrl-d → scroll to offset 5.
    state.half_page_down(&items);
    assert_eq!(state.scroll_offset(), 5);
    assert!(!state.follow_mode);

    // Second ctrl-d → scroll to offset 10 (max). at_content_edge = true.
    state.half_page_down(&items);
    assert_eq!(state.scroll_offset(), 10);
    assert!(!state.follow_mode); // one-past: not yet

    // Third ctrl-d → at bottom + at_content_edge → follow.
    state.half_page_down(&items);
    assert!(state.follow_mode);
}

#[test]
fn page_down_one_past_engages_follow() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // First page-down → offset 10 (max). at_content_edge = true.
    state.page_down(&items);
    assert_eq!(state.scroll_offset(), 10);
    assert!(!state.follow_mode);

    // Second page-down → follow.
    state.page_down(&items);
    assert!(state.follow_mode);
}

#[test]
fn mouse_wheel_overscroll_engages_follow() {
    // Mouse wheel at bottom with overscroll counter.
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to bottom.
    state.scroll_lines(10, &items);
    assert_eq!(state.scroll_offset(), 10);
    assert!(!state.follow_mode); // first hit: at_content_edge = true

    // Another scroll at bottom → overscroll counter fires.
    state.scroll_lines(3, &items);
    assert!(state.follow_mode);
}

#[test]
fn scroll_up_resets_edge_state() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to bottom → at_content_edge.
    state.scroll_lines(10, &items);
    assert!(!state.follow_mode);

    // Scroll up → resets edge state.
    state.scroll_lines(-1, &items);
    assert!(!state.follow_mode);

    // Scroll back to bottom → at_content_edge again (not follow).
    state.scroll_lines(1, &items);
    assert!(!state.follow_mode);

    // Once more → follow.
    state.scroll_lines(1, &items);
    assert!(state.follow_mode);
}

#[test]
fn scroll_down_not_at_bottom_stays_unfollowed() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    state.scroll_down(5);
    assert!(!state.follow_mode);
}

#[test]
fn scroll_up_from_bottom_disengages_follow() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 10);
    assert!(state.follow_mode);

    state.scroll_up(1);
    assert!(!state.follow_mode);
}

#[test]
fn scroll_and_center_to_bottom_engages_follow() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // Scroll to the bottom via scroll_and_center.
    state.scroll_and_center(10, &items);
    assert!(state.follow_mode);
}

#[test]
fn follow_mode_persists_through_prepare_layout() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 10);
    assert!(state.follow_mode);
    assert_eq!(state.scroll_offset(), 10);

    // Re-prepare layout — follow should persist.
    state.prepare_layout(&items, 80, 10);
    assert!(state.follow_mode);
    assert_eq!(state.scroll_offset(), 10);
}

#[test]
fn follow_mode_auto_scrolls_on_new_items() {
    let mut items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.scroll_offset(), 10);

    items.extend((20..25).map(TestItem::new));
    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.scroll_offset(), 15); // 25 - 10
    assert!(state.follow_mode);
}

// -- Follow mode: no cursor -----------------------------------------------

#[test]
fn follow_mode_has_no_selection() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 5);

    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None);
    assert_eq!(state.selected_id(), None);
}

#[test]
fn follow_mode_clears_selection_on_prepare_layout() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);
    assert_eq!(state.selected_index(), Some(0)); // auto-select in NAV

    // Engage follow.
    state.select_last(&items);
    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None);

    // prepare_layout should keep selection cleared.
    state.prepare_layout(&items, 80, 5);
    assert_eq!(state.selected_index(), None);
}

// -- Follow → NAV transitions ---------------------------------------------

#[test]
fn j_in_follow_is_noop() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 5);
    assert!(state.follow_mode);

    // j in follow → no-op (already at bottom).
    state.select_next(&items);
    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None);
}

#[test]
fn k_in_follow_exits_to_nav_and_moves_up() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 5);
    assert!(state.follow_mode);

    // k → exit follow, cursor at last visible, then move up 1.
    state.select_prev(&items);
    assert!(!state.follow_mode);
    // Should be one item above the last visible.
    let sel = state.selected_index().unwrap();
    assert!(sel < 9); // moved up from the last visible
}

#[test]
fn ctrl_u_in_follow_exits_and_scrolls_up() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 10);
    assert!(state.follow_mode);
    assert_eq!(state.scroll_offset(), 10);

    // ctrl-u → exit follow, scroll up half page.
    state.half_page_up(&items);
    assert!(!state.follow_mode);
    assert_eq!(state.scroll_offset(), 5);
    assert!(state.selected_index().is_some());
}

// -- NAV mode: new items don't reset edge state ---------------------------

#[test]
fn new_items_dont_reset_edge_state() {
    let mut items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 5);

    // Navigate to last item.
    for _ in 0..9 {
        state.select_next(&items);
    }
    // First j at end → at_content_edge.
    state.select_next(&items);
    assert!(!state.follow_mode);

    // New items arrive — at_content_edge should NOT be reset by
    // prepare_layout.
    items.extend((10..15).map(TestItem::new));
    state.prepare_layout(&items, 80, 5);

    // But j should move to new item (can_move_down), resetting edge.
    state.select_next(&items);
    assert_eq!(state.selected_index(), Some(10));
    assert!(!state.follow_mode);
}

#[test]
fn g_in_follow_exits_to_nav() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 10);
    assert!(state.follow_mode);

    state.select_first(&items);
    assert!(!state.follow_mode);
    assert_eq!(state.selected_index(), Some(0));
    assert_eq!(state.scroll_offset(), 0);
}

#[test]
fn g_in_follow_already_is_noop() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 10);

    // G in follow → no-op.
    state.select_last(&items);
    assert!(state.follow_mode);
    assert_eq!(state.scroll_offset(), 10);
}

#[test]
fn next_match_with_filter_mode() {
    // In filter mode, only matching items are visible.
    // n/N should still navigate between them.
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
        TestItem::new(2).with_text("alphabet"),
        TestItem::new(3).with_text("gamma"),
    ];
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.set_filter(Some(FilterMatcher::substring("alph")));
    state.prepare_layout(&items, 80, 10);

    assert_eq!(state.visible_count(), 2);
    // Auto-selected vis 0 (physical 0).
    assert_eq!(state.selected_index(), Some(0));

    // n → next match = physical 2 (vis 1).
    state.next_match(&items);
    assert_eq!(state.selected_index(), Some(1)); // vis index 1
    assert_eq!(state.selected_id(), Some(2)); // physical id 2

    // n → wraps to physical 0 (vis 0).
    state.next_match(&items);
    assert_eq!(state.selected_index(), Some(0));
}

// -- Config gating tests --------------------------------------------------

#[test]
fn follow_disabled_g_selects_last_item() {
    let config = ListPaneConfig {
        follow_enabled: false,
        ..ListPaneConfig::default()
    };
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
    state.prepare_layout(&items, 80, 5);

    // G selects last item normally (no follow).
    state.select_last(&items);
    assert!(!state.follow_mode);
    assert_eq!(state.selected_index(), Some(9));
}

#[test]
fn follow_disabled_constructor_ignores_follow_flag() {
    let config = ListPaneConfig {
        follow_enabled: false,
        ..ListPaneConfig::default()
    };
    let state = ListPaneState::new_with_config(WrapMode::NoWrap, true, config);
    assert!(!state.follow_mode); // forced off by config
}

#[test]
fn follow_disabled_j_at_bottom_no_one_past() {
    let config = ListPaneConfig {
        follow_enabled: false,
        ..ListPaneConfig::default()
    };
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
    state.prepare_layout(&items, 80, 10);

    // Navigate to last item.
    for _ in 0..4 {
        state.select_next(&items);
    }
    assert_eq!(state.selected_index(), Some(4));

    // j at end — should be a no-op, no follow.
    state.select_next(&items);
    assert!(!state.follow_mode);
    assert_eq!(state.selected_index(), Some(4));

    // Again — still no follow.
    state.select_next(&items);
    assert!(!state.follow_mode);
}

#[test]
fn follow_disabled_ctrl_d_at_bottom_no_follow() {
    let config = ListPaneConfig {
        follow_enabled: false,
        ..ListPaneConfig::default()
    };
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
    state.prepare_layout(&items, 80, 10);

    // Spam ctrl-d — should never engage follow.
    for _ in 0..10 {
        state.half_page_down(&items);
    }
    assert!(!state.follow_mode);
    assert_eq!(state.scroll_offset(), 10); // at bottom
}

#[test]
fn follow_disabled_mouse_wheel_at_bottom_no_follow() {
    let config = ListPaneConfig {
        follow_enabled: false,
        ..ListPaneConfig::default()
    };
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
    state.prepare_layout(&items, 80, 10);

    // Spam mouse wheel down — should never engage follow.
    for _ in 0..10 {
        state.scroll_lines(3, &items);
    }
    assert!(!state.follow_mode);
}

#[test]
fn follow_disabled_toggle_follow_is_noop() {
    let config = ListPaneConfig {
        follow_enabled: false,
        ..ListPaneConfig::default()
    };
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
    state.prepare_layout(&items, 80, 10);

    state.toggle_follow(&items);
    assert!(!state.follow_mode);
}

#[test]
fn wrap_toggle_disabled_w_not_consumed() {
    let config = ListPaneConfig {
        wrap_toggle_enabled: false,
        ..ListPaneConfig::default()
    };
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
    state.prepare_layout(&items, 80, 10);

    // 'w' should not be consumed.
    let consumed = state.handle_key_event(&key!('w').to_key_event(), &items);
    assert!(!consumed);
    assert_eq!(state.wrap_mode(), WrapMode::NoWrap);
}

// -- Toggle follow tests --------------------------------------------------

#[test]
fn toggle_follow_from_nav_engages() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);
    assert!(!state.follow_mode);

    state.toggle_follow(&items);
    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None);
    assert_eq!(state.scroll_offset(), 10);
}

#[test]
fn toggle_follow_from_follow_exits() {
    let items: Vec<TestItem> = (0..20).map(TestItem::new).collect();
    let mut state = new_streaming(WrapMode::NoWrap, true);
    state.prepare_layout(&items, 80, 10);
    assert!(state.follow_mode);

    state.toggle_follow(&items);
    assert!(!state.follow_mode);
    assert!(state.selected_index().is_some());
}

// -- Copy tests -----------------------------------------------------------

/// Helper: create a state with copy enabled.
fn new_with_copy() -> ListPaneState {
    ListPaneState::new_with_config(
        WrapMode::NoWrap,
        false,
        ListPaneConfig {
            copy_enabled: true,
            ..ListPaneConfig::default()
        },
    )
}

#[test]
fn copy_selected_copies_content_to_clipboard() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_with_copy();
    state.prepare_layout(&items, 80, 10);

    // Auto-selected item 0. Copy it.
    assert!(state.copy_selected(&items));

    // Read back from the internal clipboard.
    let copied = state.clipboard.get();
    assert_eq!(copied, Some("item-0".to_string()));
}

#[test]
fn copy_selected_after_navigation() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_with_copy();
    state.prepare_layout(&items, 80, 10);

    // Navigate to item 3.
    state.select_at(3, &items);
    assert!(state.copy_selected(&items));

    let copied = state.clipboard.get();
    assert_eq!(copied, Some("item-3".to_string()));
}

#[test]
fn copy_noop_when_no_selection() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_with_copy();
    state.prepare_layout(&items, 80, 10);

    // Clear selection.
    state.clear_selection();
    assert!(!state.copy_selected(&items));
}

#[test]
fn copy_noop_when_disabled() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    // Default config has copy_enabled: false.
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    assert!(!state.copy_selected(&items));
}

#[test]
fn copy_noop_in_follow_mode() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(
        WrapMode::NoWrap,
        true,
        ListPaneConfig {
            follow_enabled: true,
            copy_enabled: true,
            ..ListPaneConfig::default()
        },
    );
    state.prepare_layout(&items, 80, 10);
    assert!(state.follow_mode);
    assert_eq!(state.selected_index(), None);

    // No selection in follow mode → copy is a no-op.
    assert!(!state.copy_selected(&items));
}

#[test]
fn copy_with_custom_clipboard_provider() {
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct RecordingClip {
        last: Arc<Mutex<Option<String>>>,
    }
    impl ClipboardProvider for RecordingClip {
        fn get(&mut self) -> Option<String> {
            self.last.lock().unwrap().clone()
        }
        fn set(&mut self, text: &str) {
            *self.last.lock().unwrap() = Some(text.to_string());
        }
    }

    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_with_copy();
    let clip = Arc::new(Mutex::new(None));
    state.set_clipboard_provider(Box::new(RecordingClip { last: clip.clone() }));
    state.prepare_layout(&items, 80, 10);

    state.copy_selected(&items);
    assert_eq!(*clip.lock().unwrap(), Some("item-0".to_string()));
}

#[test]
fn y_key_copies_when_enabled() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_with_copy();
    state.prepare_layout(&items, 80, 10);

    // 'y' should be consumed and copy to clipboard.
    let consumed = state.handle_key_event(&key!('y').to_key_event(), &items);
    assert!(consumed);
    assert_eq!(state.clipboard.get(), Some("item-0".to_string()));
}

#[test]
fn y_key_not_consumed_when_disabled() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    // 'y' should not be consumed.
    let consumed = state.handle_key_event(&key!('y').to_key_event(), &items);
    assert!(!consumed);
}

#[test]
fn copy_noop_with_empty_list() {
    let items: Vec<TestItem> = vec![];
    let mut state = new_with_copy();
    state.prepare_layout(&items, 80, 10);

    assert!(!state.copy_selected(&items));
}

#[test]
fn copy_noop_after_filter_hides_all() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
    ];
    let mut state = ListPaneState::new_with_config(
        WrapMode::NoWrap,
        false,
        ListPaneConfig {
            copy_enabled: true,
            search_enabled: true,
            ..ListPaneConfig::default()
        },
    );
    // Filter with query that matches nothing.
    state.set_matcher(Some(ListMatcher::new(
        "zzzzz",
        QueryKind::Regex,
        MatchMode::Filter,
    )));
    state.prepare_layout(&items, 80, 10);

    // No visible items → no selection → copy is a no-op.
    assert_eq!(state.visible_count(), 0);
    assert!(!state.copy_selected(&items));
}

// -- Visual select tests --------------------------------------------------

/// Helper: create a state with visual select + copy enabled.
fn new_with_visual() -> ListPaneState {
    ListPaneState::new_with_config(
        WrapMode::NoWrap,
        false,
        ListPaneConfig {
            copy_enabled: true,
            visual_select_enabled: true,
            ..ListPaneConfig::default()
        },
    )
}

#[test]
fn v_enters_visual_mode() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_with_visual();
    state.prepare_layout(&items, 80, 10);
    assert!(!state.visual_mode);

    let consumed = state.handle_key_event(&key!('v').to_key_event(), &items);
    assert!(consumed);
    assert!(state.visual_mode);
    assert!(state.visual_anchor_id.is_some());
}

#[test]
fn v_toggles_visual_mode_off() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_with_visual();
    state.prepare_layout(&items, 80, 10);

    state.handle_key_event(&key!('v').to_key_event(), &items);
    assert!(state.visual_mode);

    state.handle_key_event(&key!('v').to_key_event(), &items);
    assert!(!state.visual_mode);
    assert!(state.visual_anchor_id.is_none());
}

#[test]
fn visual_mode_range_extends_with_j() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_with_visual();
    state.prepare_layout(&items, 80, 10);

    // Select item 3, enter visual, move down to 5.
    state.select_at(3, &items);
    state.enter_visual_mode(&items);
    state.select_next(&items); // → 4
    state.select_next(&items); // → 5
    state.prepare_layout(&items, 80, 10); // resolve range

    assert_eq!(state.multi_range(), Some(3..6)); // [3, 4, 5]
    assert_eq!(state.selected_index(), Some(5));
}

#[test]
fn visual_mode_range_extends_with_k() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_with_visual();
    state.prepare_layout(&items, 80, 10);

    // Select item 5, enter visual, move up to 3.
    state.select_at(5, &items);
    state.enter_visual_mode(&items);
    state.select_prev(&items); // → 4
    state.select_prev(&items); // → 3
    state.prepare_layout(&items, 80, 10);

    assert_eq!(state.multi_range(), Some(3..6)); // [3, 4, 5]
    assert_eq!(state.selected_index(), Some(3));
}

#[test]
fn shift_j_enters_visual_and_moves() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_with_visual();
    state.prepare_layout(&items, 80, 10);

    // Start at item 0. Shift-J enters visual and moves to 1.
    state.handle_key_event(&key!('J').to_key_event(), &items);
    assert!(state.visual_mode);
    assert_eq!(state.selected_index(), Some(1));

    // Another Shift-J extends to 2.
    state.handle_key_event(&key!('J').to_key_event(), &items);
    assert_eq!(state.selected_index(), Some(2));

    state.prepare_layout(&items, 80, 10);
    assert_eq!(state.multi_range(), Some(0..3)); // [0, 1, 2]
}

#[test]
fn esc_clears_visual_mode() {
    let items: Vec<TestItem> = (0..10).map(TestItem::new).collect();
    let mut state = new_with_visual();
    state.prepare_layout(&items, 80, 10);

    state.enter_visual_mode(&items);
    assert!(state.visual_mode);

    state.handle_key_event(&key!(Esc).to_key_event(), &items);
    assert!(!state.visual_mode);
    assert!(state.multi_range().is_none());
}

#[test]
fn y_copies_visual_range_then_clears() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = new_with_visual();
    state.prepare_layout(&items, 80, 10);

    // Select items 1..=3 visually.
    state.select_at(1, &items);
    state.enter_visual_mode(&items);
    state.select_next(&items); // → 2
    state.select_next(&items); // → 3
    state.prepare_layout(&items, 80, 10); // resolve range

    // y copies and clears visual mode.
    state.handle_key_event(&key!('y').to_key_event(), &items);
    assert_eq!(
        state.clipboard.get(),
        Some("item-1\nitem-2\nitem-3".to_string())
    );
    assert!(!state.visual_mode);
}

#[test]
fn visual_mode_disabled_v_not_consumed() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 10);

    let consumed = state.handle_key_event(&key!('v').to_key_event(), &items);
    assert!(!consumed);
    assert!(!state.visual_mode);
}

#[test]
fn search_clears_visual_mode() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new_with_config(
        WrapMode::NoWrap,
        false,
        ListPaneConfig {
            visual_select_enabled: true,
            search_enabled: true,
            ..ListPaneConfig::default()
        },
    );
    state.prepare_layout(&items, 80, 10);

    state.enter_visual_mode(&items);
    assert!(state.visual_mode);

    // '/' should clear visual mode.
    state.handle_key_event(&key!('/').to_key_event(), &items);
    assert!(!state.visual_mode);
}

/// Regression: select_next/select_prev must not panic when items is empty
/// but the layout cache has a stale non-zero item count (e.g. items were
/// rebuilt between prepare_layout and the next key event).
#[test]
fn select_next_prev_empty_items_no_panic() {
    let items: Vec<TestItem> = (0..5).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 10, 80);
    state.select_next(&items);
    assert!(state.selected_index.is_some());

    // Now call select_next/select_prev with an EMPTY slice while the
    // layout still thinks there are 5 items.  Must not panic.
    let empty: Vec<TestItem> = vec![];
    state.select_next(&empty);
    state.select_prev(&empty);
}

/// Regression: pressing 'j' through all items in a small viewport (4 lines,
/// 12 items) must keep the selection visible at every step.
#[test]
fn select_next_scrolls_in_small_viewport() {
    let items: Vec<TestItem> = (0..12).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 4); // width=80, viewport_height=4

    for step in 0..12 {
        state.select_next(&items);
        let idx = state.selected_index.unwrap();
        let item_y = state.layout.virtual_y(idx);
        let vp = state.viewport_height as usize;
        assert!(
            item_y >= state.scroll_offset && item_y < state.scroll_offset + vp,
            "step {step}: selected item {idx} at y={item_y} not visible \
             (scroll_offset={}, viewport={})",
            state.scroll_offset,
            vp,
        );
    }
}

/// Regression: 'j' must keep selection visible even when
/// prepare_layout is called between each select_next (simulates the
/// render → key → render cycle in the real app).
#[test]
fn select_next_with_prepare_layout_between_steps() {
    let items: Vec<TestItem> = (0..12).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);

    for step in 0..12 {
        // simulate render: prepare_layout first
        state.prepare_layout(&items, 80, 4); // width=80, viewport_height=4
        // simulate key: select_next
        state.select_next(&items);
        let idx = state.selected_index.unwrap();
        let item_y = state.layout.virtual_y(idx);
        let vp = state.viewport_height as usize;
        assert!(
            item_y >= state.scroll_offset && item_y < state.scroll_offset + vp,
            "step {step}: selected item {idx} at y={item_y} not visible \
             (scroll_offset={}, viewport={})",
            state.scroll_offset,
            vp,
        );
    }
}

/// Regression: scroll_lines must work in a small viewport with overflow.
#[test]
fn scroll_lines_works_in_small_viewport() {
    let items: Vec<TestItem> = (0..12).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    state.prepare_layout(&items, 80, 4); // width=80, viewport_height=4
    state.select_next(&items); // select first

    let total = state.layout.total_height();
    let vp = state.viewport_height;
    eprintln!(
        "total_height={total}, viewport_height={vp}, \
         item_count={}, scroll_offset={}",
        state.layout.item_count(),
        state.scroll_offset,
    );

    state.scroll_lines(1, &items);
    eprintln!("after scroll_lines(1): offset={}", state.scroll_offset);
    state.scroll_lines(1, &items);
    eprintln!("after scroll_lines(1): offset={}", state.scroll_offset);
    state.scroll_lines(1, &items);
    eprintln!("after scroll_lines(1): offset={}", state.scroll_offset);

    assert!(
        state.scroll_offset > 0,
        "after scrolling 3 lines down in 12-item list with 4-line viewport, \
         offset should be > 0 (got {}). total_height={total}, vp={vp}",
        state.scroll_offset,
    );
}

#[test]
fn paste_targets_only_active_list_input_and_preserves_comment_newlines() {
    let items = vec![
        TestItem::new(0).with_text("alpha"),
        TestItem::new(1).with_text("beta"),
    ];
    let mut state =
        ListPaneState::new_with_config(WrapMode::NoWrap, false, ListPaneConfig::streaming());
    state.prepare_layout(&items, 80, 4);
    assert!(state.handle_key_event(&key!('/').to_key_event(), &items));
    assert!(state.handle_key_event(&key!('a').to_key_event(), &items));
    assert!(state.handle_key_event(&key!('b').to_key_event(), &items));
    assert!(state.handle_key_event(&key!(Left).to_key_event(), &items));
    assert!(state.handle_paste("中\r\n", &items));
    assert_eq!(state.input_text(), "a中b");
    assert_eq!(state.matcher().map(ListMatcher::query), Some("a中b"));

    assert!(!state.handle_paste("\r\n", &items));
    assert_eq!(state.input_text(), "a中b");

    state.close_input_bar();
    assert!(!state.handle_paste("ignored", &items));

    state.open_comment_input("");
    assert!(state.handle_paste("a\nb", &items));
    assert_eq!(state.input_text(), "a\nb");
}

#[test]
fn content_down_clears_stale_scrollbar_drag_latch() {
    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    let items: Vec<TestItem> = (0..40).map(TestItem::new).collect();
    let mut state = ListPaneState::new(WrapMode::NoWrap, false);
    let pane = Rect::new(0, 0, 80, 10);
    let track = Rect::new(79, 0, 1, 10);
    state.prepare_layout(&items, pane.width, pane.height);
    state.set_scrollbar_area(Some(track));

    assert!(state.handle_mouse_event(
        MouseEventKind::Down(MouseButton::Left),
        track.x,
        5,
        pane,
        &items,
    ));
    assert!(
        state.is_scrollbar_dragging(),
        "press on the track must latch a thumb drag"
    );

    // Lost Up, then a content press — latch must not stick.
    assert!(
        state.handle_mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 4, pane, &items,)
    );
    assert!(
        !state.is_scrollbar_dragging(),
        "a later content Down must clear a stale scrollbar latch"
    );
}
