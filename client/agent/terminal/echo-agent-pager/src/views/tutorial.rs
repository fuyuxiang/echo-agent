//! Onboarding tutorial overlay (`/tutorial`).
//!
//! A top-level modal (works over both the welcome screen and an agent
//! session) with two screens:
//!
//! - **List** — the tutorial topics from [`crate::tutorial_docs`] with ✓
//!   marks for explored topics. Enter opens a topic; Esc closes.
//! - **Topic** — a scrollable markdown page (same chrome as the release-notes
//!   viewer); `→`/`←` flow through the topics in order, Esc returns to the
//!   list.
//!
//! Opened on demand via `/tutorial` (also listed in the command palette).
//! Never auto-shows.

use std::collections::HashSet;

use crossterm::event::{Event, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::theme::Theme;
use crate::tutorial_docs::TUTORIAL_TOPICS;
use crate::views::modal_window::{
    self as mw, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};
use crate::views::picker::{
    self, PickerConfig, PickerEntry, PickerHitAreas, PickerOutcome, PickerRow, PickerState,
    handle_picker_input,
};

/// Which tutorial screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialScreen {
    /// The topic list.
    List,
    /// A single topic page (index into [`TUTORIAL_TOPICS`]).
    Topic { index: usize },
    /// The full how-to guide a topic's "Go deeper" points at (`d` from the
    /// topic page); Esc returns to that topic.
    Guide { topic: usize },
}

/// State for the tutorial overlay.
pub struct TutorialState {
    pub screen: TutorialScreen,
    /// Topic indices the user has opened this launch (✓ marks).
    pub viewed: HashSet<usize>,
    /// List-screen navigation state.
    pub picker: PickerState,
    /// Shared modal chrome state (close button, shortcut hits).
    pub window: ModalWindowState,
    /// Topic-screen scroll offset.
    pub scroll: u16,
    /// Cached pre-rendered markdown lines for the topic screen, keyed by
    /// the width they were rendered at (invalidated on resize).
    pub cached_lines: Option<(u16, Vec<Line<'static>>)>,
}

impl TutorialState {
    pub fn new() -> Self {
        Self {
            screen: TutorialScreen::List,
            viewed: HashSet::new(),
            picker: PickerState::default(),
            window: ModalWindowState::new(),
            scroll: 0,
            cached_lines: None,
        }
    }

    /// Switch to the topic page at `index`, marking it viewed.
    fn open_topic(&mut self, index: usize) {
        if index >= TUTORIAL_TOPICS.len() {
            return;
        }
        self.viewed.insert(index);
        self.screen = TutorialScreen::Topic { index };
        self.scroll = 0;
        self.cached_lines = None;
        self.window = ModalWindowState::new();
    }

    /// Return from a topic page to the list.
    fn back_to_list(&mut self) {
        self.screen = TutorialScreen::List;
        self.scroll = 0;
        self.cached_lines = None;
        self.window = ModalWindowState::new();
    }

    /// Open the "Go deeper" guide for the topic at `index`, if it has one.
    fn open_guide(&mut self, index: usize) {
        let has_guide = TUTORIAL_TOPICS
            .get(index)
            .and_then(|t| t.go_deeper)
            .and_then(crate::docs::find_doc)
            .is_some();
        if has_guide {
            self.screen = TutorialScreen::Guide { topic: index };
            self.scroll = 0;
            self.cached_lines = None;
            self.window = ModalWindowState::new();
        }
    }
}

impl Default for TutorialState {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of routing an input event to the tutorial overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialOutcome {
    /// The overlay consumed the event (it owns all input while open).
    Consumed,
    /// The user closed the tutorial — the host should drop the state.
    Closed,
}

/// List-screen picker config. Search is disabled: six fixed topics don't
/// need filtering, and letter keys would otherwise start a query.
fn list_picker_config() -> PickerConfig<'static> {
    PickerConfig {
        title: None,
        show_search_hint: false,
        expandable: false,
        esc_clears_query: false,
        shortcuts: None,
        pending_hint: None,
        shortcuts_area: None,
        non_selectable: &[],
        non_selectable_clickable: &[],
        tabs: None,
        active_tab: 0,
        filter_label: None,
        filter_key_hint: None,
        filter_active: false,
        action_keys: &[],
        disable_search: true,
        compact_bottom_bar: false,
        search_only_on_slash: false,
        vim_normal_first: false,
        header_note: None,
    }
}

/// Route an input event to the tutorial overlay. The overlay consumes all
/// key and mouse input while open (top-level modal semantics).
pub fn handle_tutorial_input(ev: &Event, st: &mut TutorialState) -> TutorialOutcome {
    match st.screen {
        TutorialScreen::Topic { .. } => handle_topic_input(ev, st),
        TutorialScreen::Guide { .. } => handle_guide_input(ev, st),
        TutorialScreen::List => handle_list_input(ev, st),
    }
}

/// Guide screen: scroll like a topic page; Esc (or the close button)
/// returns to the topic it came from.
fn handle_guide_input(ev: &Event, st: &mut TutorialState) -> TutorialOutcome {
    let TutorialScreen::Guide { topic } = st.screen else {
        return TutorialOutcome::Consumed;
    };
    let chrome_cfg = ModalWindowConfig {
        title: "",
        tabs: None,
        shortcuts: &[],
        sizing: ModalSizing::default(),
        fold_info: None,
    };
    match ev {
        Event::Key(key) => {
            if key.kind == KeyEventKind::Release {
                return TutorialOutcome::Consumed;
            }
            match mw::handle_modal_key(&mut st.window, key, &chrome_cfg) {
                mw::ModalWindowOutcome::CloseRequested => {
                    st.open_topic(topic);
                    return TutorialOutcome::Consumed;
                }
                mw::ModalWindowOutcome::Handled => return TutorialOutcome::Consumed,
                _ => {}
            }
            crate::views::modal::apply_doc_scroll(key.code, &mut st.scroll);
            TutorialOutcome::Consumed
        }
        Event::Mouse(mouse) => {
            match mw::handle_modal_mouse(&mut st.window, mouse.kind, mouse.column, mouse.row) {
                mw::ModalWindowOutcome::CloseRequested => {
                    st.open_topic(topic);
                    return TutorialOutcome::Consumed;
                }
                mw::ModalWindowOutcome::Handled => return TutorialOutcome::Consumed,
                _ => {}
            }
            crate::views::modal::apply_doc_mouse_scroll(mouse.kind, &mut st.scroll);
            TutorialOutcome::Consumed
        }
        _ => TutorialOutcome::Consumed,
    }
}

fn handle_topic_input(ev: &Event, st: &mut TutorialState) -> TutorialOutcome {
    let chrome_cfg = ModalWindowConfig {
        title: "",
        tabs: None,
        shortcuts: &[],
        sizing: ModalSizing::default(),
        fold_info: None,
    };
    match ev {
        Event::Key(key) => {
            if key.kind == KeyEventKind::Release {
                return TutorialOutcome::Consumed;
            }
            match mw::handle_modal_key(&mut st.window, key, &chrome_cfg) {
                mw::ModalWindowOutcome::CloseRequested => {
                    st.back_to_list();
                    return TutorialOutcome::Consumed;
                }
                mw::ModalWindowOutcome::Handled => return TutorialOutcome::Consumed,
                _ => {}
            }
            // Linear flow: `→` reads on to the next topic (the list, once
            // the tour is done); `←` steps back; `d` opens the "Go deeper"
            // guide.
            if let TutorialScreen::Topic { index } = st.screen {
                match key.code {
                    crossterm::event::KeyCode::Right => {
                        if index + 1 < TUTORIAL_TOPICS.len() {
                            st.open_topic(index + 1);
                        } else {
                            st.back_to_list();
                        }
                        return TutorialOutcome::Consumed;
                    }
                    crossterm::event::KeyCode::Left => {
                        if let Some(prev) = index.checked_sub(1) {
                            st.open_topic(prev);
                        }
                        return TutorialOutcome::Consumed;
                    }
                    crossterm::event::KeyCode::Char('d') => {
                        st.open_guide(index);
                        return TutorialOutcome::Consumed;
                    }
                    _ => {}
                }
            }
            crate::views::modal::apply_doc_scroll(key.code, &mut st.scroll);
            TutorialOutcome::Consumed
        }
        Event::Mouse(mouse) => {
            match mw::handle_modal_mouse(&mut st.window, mouse.kind, mouse.column, mouse.row) {
                mw::ModalWindowOutcome::CloseRequested => {
                    st.back_to_list();
                    return TutorialOutcome::Consumed;
                }
                mw::ModalWindowOutcome::Handled => return TutorialOutcome::Consumed,
                _ => {}
            }
            crate::views::modal::apply_doc_mouse_scroll(mouse.kind, &mut st.scroll);
            TutorialOutcome::Consumed
        }
        _ => TutorialOutcome::Consumed,
    }
}

fn handle_list_input(ev: &Event, st: &mut TutorialState) -> TutorialOutcome {
    // Chrome first: close button clicks and Esc. `handle_picker_input` also
    // maps Esc to `Closed`, but the close button lives on the ModalWindow.
    if let Event::Mouse(mouse) = ev
        && matches!(
            mw::handle_modal_mouse(&mut st.window, mouse.kind, mouse.column, mouse.row),
            mw::ModalWindowOutcome::CloseRequested
        )
    {
        return TutorialOutcome::Closed;
    }
    if let Event::Key(key) = ev
        && key.kind == KeyEventKind::Release
    {
        return TutorialOutcome::Consumed;
    }
    // Search is disabled on the fixed topic list, but the picker's paste
    // path fills the query regardless of `disable_search` — swallow paste
    // here so it can't start an invisible filter.
    if matches!(ev, Event::Paste(_)) {
        return TutorialOutcome::Consumed;
    }

    let config = list_picker_config();
    match handle_picker_input(ev, &mut st.picker, TUTORIAL_TOPICS.len(), &config) {
        PickerOutcome::Selected(i) => {
            st.open_topic(i);
            TutorialOutcome::Consumed
        }
        PickerOutcome::Closed => TutorialOutcome::Closed,
        _ => TutorialOutcome::Consumed,
    }
}

/// Intro copy shown above the topic list. No time promises — just what it
/// is and how to leave.
const INTRO_LINES: [&str; 2] = [
    "Quick tips to get the most out of EchoAgent Build.",
    "Pick a topic. Esc when you're done.",
];

/// Topic page body: the embedded markdown minus its leading `# ` heading —
/// the modal window chrome already shows the title, so rendering the H1
/// would double it.
fn topic_body(content: &str) -> &str {
    match content.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => rest.trim_start_matches('\n'),
        _ => content,
    }
}

/// Render the tutorial overlay (list or topic screen) over `area`.
pub fn render_tutorial(buf: &mut Buffer, area: Rect, st: &mut TutorialState, compact: bool) {
    let theme = Theme::current();
    match st.screen {
        TutorialScreen::Topic { index } => {
            // `Topic` is only constructed via `open_topic`, which bounds-checks.
            let Some(topic) = TUTORIAL_TOPICS.get(index) else {
                return;
            };
            let next_hint = match TUTORIAL_TOPICS.get(index + 1) {
                Some(next) => format!("\u{2192} next: {}", next.title),
                None => "\u{2192} done".to_owned(),
            };
            let mut shortcuts = vec![
                Shortcut {
                    label: "\u{2191}/\u{2193} scroll",
                    clickable: false,
                    id: 0,
                },
                Shortcut {
                    label: &next_hint,
                    clickable: false,
                    id: 0,
                },
            ];
            if topic.go_deeper.is_some() {
                shortcuts.push(Shortcut {
                    label: "d go deeper",
                    clickable: false,
                    id: 0,
                });
            }
            shortcuts.push(Shortcut {
                label: "Esc list",
                clickable: false,
                id: 0,
            });
            crate::views::modal::render_doc_viewer_overlay_with_shortcuts(
                buf,
                area,
                &mut st.window,
                topic.title,
                topic_body(topic.content),
                &mut st.scroll,
                &mut st.cached_lines,
                compact,
                &theme,
                &shortcuts,
            );
        }
        TutorialScreen::Guide { topic } => {
            let Some(doc) = TUTORIAL_TOPICS
                .get(topic)
                .and_then(|t| t.go_deeper)
                .and_then(crate::docs::find_doc)
            else {
                return;
            };
            crate::views::modal::render_doc_viewer_overlay(
                buf,
                area,
                &mut st.window,
                doc.title,
                doc.content,
                &mut st.scroll,
                &mut st.cached_lines,
                compact,
                &theme,
            );
        }
        TutorialScreen::List => render_list(buf, area, st, compact, &theme),
    }
}

fn render_list(buf: &mut Buffer, area: Rect, st: &mut TutorialState, compact: bool, theme: &Theme) {
    let progress = format!("{}/{} explored", st.viewed.len(), TUTORIAL_TOPICS.len());
    let shortcuts = [
        Shortcut {
            label: &progress,
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "\u{2191}/\u{2193} navigate",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Enter open",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Esc done",
            clickable: false,
            id: 0,
        },
    ];
    let modal_config = ModalWindowConfig {
        title: "Welcome to EchoAgent Build",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing {
            width_pct: 0.60,
            max_width: 100,
            min_width: 44,
            v_margin: 4,
            h_pad: 2,
            v_pad: 1,
            footer_lines: 2,
        }
        .with_compact(compact),
        fold_info: None,
    };
    let Some(mca) = mw::render_modal_window(buf, area, &mut st.window, &modal_config, theme) else {
        return;
    };

    // Intro copy, then a blank row, then the topic rows.
    let intro_style = Style::default().fg(theme.gray_bright);
    let mut y = mca.content.y;
    for line in INTRO_LINES {
        if y >= mca.content.y + mca.content.height {
            break;
        }
        Paragraph::new(Line::styled(line, intro_style)).render(
            Rect {
                x: mca.content.x,
                y,
                width: mca.content.width,
                height: 1,
            },
            buf,
        );
        y += 1;
    }
    y = y.saturating_add(1); // gap

    let entries_area = Rect {
        x: mca.content.x,
        y,
        width: mca.content.width,
        height: (mca.content.y + mca.content.height).saturating_sub(y),
    };
    if entries_area.height == 0 {
        return;
    }

    // Narrow modals can't fit title + blurb on one row; stack the blurb below.
    const NARROW_THRESHOLD: u16 = 64;
    let narrow = entries_area.width < NARROW_THRESHOLD;
    let blurb_slices: Vec<[&str; 1]> = TUTORIAL_TOPICS.iter().map(|t| [t.blurb]).collect();

    let picker_entries: Vec<PickerEntry<'_>> = TUTORIAL_TOPICS
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let viewed = st.viewed.contains(&i);
            PickerEntry::Row(PickerRow {
                label: t.title,
                right_label: if narrow { "" } else { t.blurb },
                selected: i == st.picker.selected,
                expanded: narrow,
                fields: &[],
                description_lines: if narrow { &blurb_slices[i][..] } else { &[] },
                summary_lines: &[],
                dimmed: false,
                indent: 0,
                badge: if viewed { "\u{2713}" } else { "" },
                badge_color: Some(theme.accent_success),
                collapsible: false,
                underline_last_desc: false,
            })
        })
        .collect();

    let non_sel = vec![false; picker_entries.len()];
    let content_hit = picker::render_picker_content_with_scrollbar_x(
        buf,
        entries_area,
        theme,
        &mut st.picker,
        &picker_entries,
        &non_sel,
        &[],
        Some(theme.bg_base),
        false,
        0,
        mca.inner_x + mca.inner_width.saturating_sub(1),
    );
    st.picker.hit_areas = Some(PickerHitAreas {
        close_button: Rect::default(),
        search_bar: Rect::default(),
        item_rects: content_hit.item_rects,
        entry_indices: content_hit.entry_indices,
        tab_rects: vec![],
        filter_rect: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn enter_opens_selected_topic_and_marks_viewed() {
        let mut st = TutorialState::new();
        assert_eq!(st.screen, TutorialScreen::List);
        assert!(st.viewed.is_empty());

        let outcome = handle_tutorial_input(&key(KeyCode::Down), &mut st);
        assert_eq!(outcome, TutorialOutcome::Consumed);
        assert_eq!(st.picker.selected, 1);

        let outcome = handle_tutorial_input(&key(KeyCode::Enter), &mut st);
        assert_eq!(outcome, TutorialOutcome::Consumed);
        assert_eq!(st.screen, TutorialScreen::Topic { index: 1 });
        assert!(st.viewed.contains(&1));
    }

    #[test]
    fn esc_pops_topic_to_list_then_closes() {
        let mut st = TutorialState::new();
        handle_tutorial_input(&key(KeyCode::Enter), &mut st);
        assert!(matches!(st.screen, TutorialScreen::Topic { .. }));

        let outcome = handle_tutorial_input(&key(KeyCode::Esc), &mut st);
        assert_eq!(outcome, TutorialOutcome::Consumed);
        assert_eq!(st.screen, TutorialScreen::List);
        assert!(st.viewed.contains(&0), "opened topic stays ✓-marked");

        let outcome = handle_tutorial_input(&key(KeyCode::Esc), &mut st);
        assert_eq!(outcome, TutorialOutcome::Closed);
    }

    #[test]
    fn d_opens_the_go_deeper_guide_and_esc_returns_to_the_topic() {
        let mut st = TutorialState::new();
        st.open_topic(0);
        assert!(
            TUTORIAL_TOPICS[0].go_deeper.is_some(),
            "topic 0 has a guide"
        );

        handle_tutorial_input(&key(KeyCode::Char('d')), &mut st);
        assert_eq!(st.screen, TutorialScreen::Guide { topic: 0 });

        // Esc returns to the topic the guide came from, not the list.
        handle_tutorial_input(&key(KeyCode::Esc), &mut st);
        assert_eq!(st.screen, TutorialScreen::Topic { index: 0 });
    }

    #[test]
    fn d_is_a_noop_on_a_topic_without_a_guide() {
        let last = TUTORIAL_TOPICS.len() - 1;
        assert!(
            TUTORIAL_TOPICS[last].go_deeper.is_none(),
            "the closing topic intentionally has no single guide"
        );
        let mut st = TutorialState::new();
        st.open_topic(last);
        handle_tutorial_input(&key(KeyCode::Char('d')), &mut st);
        assert_eq!(st.screen, TutorialScreen::Topic { index: last });
    }

    #[test]
    fn topic_body_strips_the_duplicated_h1() {
        // The window title already names the topic; the H1 must not render
        // a second time inside the page.
        assert_eq!(topic_body("# Title\n\nBody text.\n"), "Body text.\n");
        // Every real topic starts with an H1, so every body drops it.
        for t in TUTORIAL_TOPICS {
            assert!(!topic_body(t.content).starts_with("# "), "{}", t.title);
        }
        // Content without a leading H1 passes through untouched.
        assert_eq!(topic_body("plain text"), "plain text");
    }

    #[test]
    fn right_flows_through_topics_and_back_to_list() {
        let mut st = TutorialState::new();
        handle_tutorial_input(&key(KeyCode::Enter), &mut st);
        assert_eq!(st.screen, TutorialScreen::Topic { index: 0 });

        // → walks the whole tour, marking each topic viewed…
        for expected in 1..TUTORIAL_TOPICS.len() {
            handle_tutorial_input(&key(KeyCode::Right), &mut st);
            assert_eq!(st.screen, TutorialScreen::Topic { index: expected });
            assert!(st.viewed.contains(&expected));
        }
        // …and lands back on the list after the last page.
        let outcome = handle_tutorial_input(&key(KeyCode::Right), &mut st);
        assert_eq!(outcome, TutorialOutcome::Consumed);
        assert_eq!(st.screen, TutorialScreen::List);
        assert_eq!(st.viewed.len(), TUTORIAL_TOPICS.len(), "full tour ✓-marked");
    }

    #[test]
    fn left_steps_back_and_stops_at_first_topic() {
        let mut st = TutorialState::new();
        st.open_topic(1);
        handle_tutorial_input(&key(KeyCode::Left), &mut st);
        assert_eq!(st.screen, TutorialScreen::Topic { index: 0 });
        // At the first topic, ← is a no-op (Esc returns to the list).
        handle_tutorial_input(&key(KeyCode::Left), &mut st);
        assert_eq!(st.screen, TutorialScreen::Topic { index: 0 });
    }

    #[test]
    fn topic_page_scrolls_and_ignores_typing() {
        let mut st = TutorialState::new();
        handle_tutorial_input(&key(KeyCode::Enter), &mut st);
        handle_tutorial_input(&key(KeyCode::Down), &mut st);
        assert!(st.scroll > 0, "Down scrolls the topic page");
        handle_tutorial_input(&key(KeyCode::Up), &mut st);
        assert_eq!(st.scroll, 0);
        // Printable chars are consumed without effect (no search on topics).
        let outcome = handle_tutorial_input(&key(KeyCode::Char('x')), &mut st);
        assert_eq!(outcome, TutorialOutcome::Consumed);
        assert!(matches!(st.screen, TutorialScreen::Topic { .. }));
    }

    #[test]
    fn list_typing_does_not_start_a_query() {
        // Search is disabled: letters must not filter the fixed topic list.
        let mut st = TutorialState::new();
        handle_tutorial_input(&key(KeyCode::Char('w')), &mut st);
        assert!(st.picker.query().is_empty());
        assert_eq!(st.screen, TutorialScreen::List);
    }

    #[test]
    fn list_paste_does_not_start_a_query() {
        // The picker's paste path ignores `disable_search`; the list screen
        // must swallow paste so it can't start an invisible filter.
        let mut st = TutorialState::new();
        let outcome = handle_tutorial_input(&Event::Paste("worktrees".to_owned()), &mut st);
        assert_eq!(outcome, TutorialOutcome::Consumed);
        assert!(st.picker.query().is_empty());
        assert_eq!(st.screen, TutorialScreen::List);
    }

    #[test]
    fn render_list_populates_hit_areas() {
        let mut st = TutorialState::new();
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        render_tutorial(&mut buf, area, &mut st, false);
        let hit = st.picker.hit_areas.as_ref().expect("hit areas populated");
        assert_eq!(
            hit.item_rects.len(),
            TUTORIAL_TOPICS.len(),
            "one click rect per topic"
        );
    }

    #[test]
    fn render_topic_screen_smoke() {
        let mut st = TutorialState::new();
        st.open_topic(0);
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        render_tutorial(&mut buf, area, &mut st, false);
    }
}
