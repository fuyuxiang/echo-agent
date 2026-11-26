//! Link affordances in scrollback: highlight cycling, hover tracking, the
//! macOS modifier poll, and painting link highlight styles.
#[cfg(test)]
use super::app_should_open_link_on_click_with;
#[cfg(target_os = "macos")]
use super::is_link_modifier_held;
#[cfg(test)]
use super::paste::paste_key_tests;
#[cfg(test)]
use super::{AgentPane, PromptMode, test_fixtures};
use super::{AgentView, app_should_open_link_on_click, has_native_link_hover};
#[cfg(test)]
use crate::actions::{ActionId, ActionRegistry, When};
#[cfg(test)]
use crate::app::actions::Action;
#[cfg(test)]
use crate::scrollback::render::ScratchBuffer;
#[cfg(test)]
use crate::scrollback::text_selection::{PendingTextDrag, RangeHit, ResolvedSelectionModel};
#[cfg(test)]
use crate::views::plan_approval_view::PlanApprovalFocus;
#[cfg(any(target_os = "macos", test))]
use crossterm::event::KeyModifiers;
#[cfg(test)]
use crossterm::event::{Event, KeyCode, KeyEvent};
#[cfg(test)]
use ratatui::buffer::Buffer;
use ratatui::style::Style;
#[cfg(test)]
use std::time::Instant;
impl AgentView {
    /// Cycle the highlighted link index forward or backward through visible links.
    ///
    /// When `forward` is true, advances to the next link (wrapping around).
    /// When no link is highlighted, selects the first (forward) or last (backward).
    pub fn cycle_highlighted_link(&mut self, forward: bool) {
        let count = self.visible_link_map.links().len();
        if count == 0 {
            self.highlighted_link_idx = None;
            return;
        }
        self.highlighted_link_idx = Some(match self.highlighted_link_idx {
            None => {
                if forward {
                    0
                } else {
                    count - 1
                }
            }
            Some(cur) => {
                if forward {
                    (cur + 1) % count
                } else {
                    (cur + count - 1) % count
                }
            }
        });
    }
    /// Return the semantic target of the currently highlighted link, if any.
    pub fn highlighted_link_target(&self) -> Option<&crate::render::osc8::LinkTarget> {
        self.highlighted_link_idx
            .and_then(|idx| self.visible_link_map.links().get(idx))
            .map(|link| &link.target)
    }
    /// Return the current OSC 8 URL for the highlighted link preview.
    pub fn highlighted_link_url(&self) -> Option<std::sync::Arc<str>> {
        self.highlighted_link_target()
            .and_then(crate::render::osc8::resolve_link_target)
            .and_then(|resolved| resolved.osc8_url)
    }
    /// True when `(x, y)` lies inside an overlay drawn over the scrollback this
    /// frame (dropdown, goal detail). Such positions belong to the overlay, not
    /// to any scrollback link beneath it, so link hover/click must ignore them.
    pub(in crate::app) fn pos_occluded(&self, x: u16, y: u16) -> bool {
        let pos = ratatui::layout::Position { x, y };
        self.frame_occluder_rects.iter().any(|r| r.contains(pos))
    }
    /// Rect form of [`Self::pos_occluded`]: any overlay intersecting `rect`
    /// counts as covering it — the conservative drop-whole rule shared by the
    /// CTA OSC 8 spans and the impression funnel (overlap ⇒ not counted).
    pub(in crate::app) fn rect_occluded(&self, rect: ratatui::layout::Rect) -> bool {
        self.frame_occluder_rects
            .iter()
            .any(|r| rect.intersects(*r))
    }
    /// Append the promo banner [label] button's OSC 8 span when one should be
    /// emitted this frame: an armed CTA rect (draw already suppressed it under
    /// prompt dropdowns), no frame occluder covering it (same drop-whole rule
    /// as the scrollback spans — e.g. the goal-detail overlay can reach the
    /// banner row on short terminals), and a usable CTA target. Split from
    /// `draw`'s emit-gated block so the guard set is unit-testable; the caller
    /// owns the `hyperlink_route().emit_osc8` check.
    pub(super) fn push_promo_cta_link_span(
        &self,
        link_spans_out: &mut Vec<echo_agent_ratatui_inline::LinkSpan>,
        banner_announcements: &[echo_agent_announcements::RemoteAnnouncement],
        hidden_announcement_ids: &std::collections::BTreeSet<String>,
    ) {
        if let Some((_, url)) = crate::views::announcements::promo_cta_target(
            banner_announcements,
            hidden_announcement_ids,
        ) {
            self.push_cta_link_span(link_spans_out, self.hit_announcement_cta.rect, url);
        }
    }
    /// OSC 8 twin for the in-session header upgrade CTA (`hit_upgrade_cta`),
    /// sharing the same slot-gated url + occluder drop-whole rule as the banner
    /// CTA so hyperlink-capable terminals can open the promo from the header.
    pub(super) fn push_upgrade_cta_link_span(
        &self,
        link_spans_out: &mut Vec<echo_agent_ratatui_inline::LinkSpan>,
        banner_announcements: &[echo_agent_announcements::RemoteAnnouncement],
        hidden_announcement_ids: &std::collections::BTreeSet<String>,
    ) {
        if let Some((_, url)) = crate::views::announcements::promo_cta_target(
            banner_announcements,
            hidden_announcement_ids,
        ) {
            self.push_cta_link_span(link_spans_out, self.hit_upgrade_cta.rect, url);
        }
    }
    /// Append the OSC 8 spans a `command` status row opened, in the screen
    /// columns the row was painted at, under the same occluder drop-whole rule
    /// as the CTA spans above.
    pub(super) fn push_status_line_link_spans(
        &self,
        link_spans_out: &mut Vec<echo_agent_ratatui_inline::LinkSpan>,
        status_line_spans: Vec<echo_agent_ratatui_inline::LinkSpan>,
    ) {
        for span in status_line_spans {
            let width = span.col_end.saturating_sub(span.col_start);
            let row = ratatui::layout::Rect::new(span.col_start, span.row, width, 1);
            if !self.rect_occluded(row) {
                link_spans_out.push(span);
            }
        }
    }
    /// Emit an OSC 8 hyperlink span over a CTA button `rect` when armed (draw
    /// already suppressed it under prompt dropdowns) and no frame occluder
    /// covers it (same drop-whole rule as the scrollback spans — e.g. the
    /// goal-detail overlay can reach the top/bottom rows on short terminals).
    fn push_cta_link_span(
        &self,
        link_spans_out: &mut Vec<echo_agent_ratatui_inline::LinkSpan>,
        rect: Option<ratatui::layout::Rect>,
        url: &str,
    ) {
        if let Some(rect) = rect
            && !self.rect_occluded(rect)
        {
            link_spans_out.push(echo_agent_ratatui_inline::LinkSpan {
                row: rect.y,
                col_start: rect.x,
                col_end: rect.x.saturating_add(rect.width),
                url: url.into(),
                id: None,
            });
        }
    }
    /// Hit-test `(col, row)` and arm [`Self::pending_link_click`] when the app
    /// owns the open. Returns `true` when a link was hit so the caller can skip
    /// text-drag even if the terminal owns the open (pending stays unset).
    pub(in crate::app) fn try_arm_link_click(&mut self, col: u16, row: u16) -> bool {
        if has_native_link_hover() || self.pos_occluded(col, row) {
            return false;
        }
        let Some(link) = self.visible_link_map.link_at(col, row) else {
            return false;
        };
        self.pending_link_click =
            app_should_open_link_on_click(link).then(|| (col, row, link.target.clone()));
        true
    }
    /// Re-evaluate which link (if any) is under the cursor for the given
    /// modifier state.  Returns `true` when `hovered_link_idx` changed.
    pub(in crate::app) fn update_hovered_link(&mut self, modifier_held: bool) -> bool {
        if !modifier_held && self.hovered_link_idx.is_none() {
            return false;
        }
        let new = if modifier_held
            && !has_native_link_hover()
            && !self.pos_occluded(self.last_mouse_pos.0, self.last_mouse_pos.1)
        {
            self.visible_link_map
                .link_at(self.last_mouse_pos.0, self.last_mouse_pos.1)
                .filter(|hit| app_should_open_link_on_click(hit))
                .and_then(|hit| {
                    self.visible_link_map
                        .links()
                        .iter()
                        .position(|l| std::ptr::eq(l, hit))
                })
        } else {
            None
        };
        if new != self.hovered_link_idx {
            self.hovered_link_idx = new;
            true
        } else {
            false
        }
    }
    /// How long after the last pointer movement the Cmd link-hover poll stays
    /// armed. Within this window a Cmd press underlines the hovered link at
    /// tick latency; after it, the poll (and with it the animation tick loop)
    /// parks so a pointer merely resting over the window costs zero CPU.
    /// Any pointer movement — including the reflexive nudge people make
    /// before Cmd+clicking — re-arms it instantly via the mouse event.
    #[cfg(target_os = "macos")]
    const LINK_MODIFIER_POLL_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
    /// Whether macOS should keep ticking / polling Cmd for link hover.
    ///
    /// Covers scrollback (`hovered_entry`) **and** `/btw` panel links (map
    /// entries under `last_btw_area` / existing hover index) — poll must not
    /// require scrollback-only `hovered_entry` or Cmd release over the panel
    /// leaves a stuck highlight.
    ///
    /// The poll is bounded by recent pointer activity: `hovered_entry` is set
    /// whenever the pointer rests anywhere over content, so an unbounded gate
    /// would hold the ~30fps animation tick (each tick running a CoreGraphics
    /// modifier query) alive for as long as the mouse happens to sit over the
    /// window — i.e. approximately always. An active link highlight
    /// (`hovered_link_idx`) keeps polling regardless of the window so a held
    /// Cmd never strands a stuck underline.
    pub fn needs_link_modifier_poll(&self) -> bool {
        if has_native_link_hover() || self.visible_link_map.is_empty() {
            return false;
        }
        if self.hovered_link_idx.is_some() {
            return true;
        }
        #[cfg(target_os = "macos")]
        {
            let recently_moved = self
                .last_mouse_moved_at
                .is_some_and(|t| t.elapsed() < Self::LINK_MODIFIER_POLL_WINDOW);
            if !recently_moved {
                return false;
            }
        }
        if self.hovered_entry.is_some() {
            return true;
        }
        self.last_btw_area.area() > 0
            && self
                .last_btw_area
                .contains((self.last_mouse_pos.0, self.last_mouse_pos.1).into())
    }
    /// Poll macOS modifier state for link hover. Returns `true` when
    /// `hovered_link_idx` changed and a redraw is needed.
    ///
    /// On macOS, crossterm's Kitty protocol does not report modifier-only
    /// key events, so bare Cmd press/release is invisible to the input
    /// handler. This method polls CoreGraphics directly and is meant to
    /// be called from the animation tick loop.
    ///
    /// No-op on non-macOS (Ctrl key events are reported normally).
    pub fn poll_link_modifier(&mut self) -> bool {
        #[cfg(target_os = "macos")]
        if self.needs_link_modifier_poll() {
            return self.update_hovered_link(is_link_modifier_held(KeyModifiers::empty()));
        }
        false
    }
    /// Paint keyboard/hover active style for links whose index is in `index_range`.
    /// Shared by the scrollback and `/btw` highlight passes (z-order split).
    ///
    /// When `highlighted_link_idx == hovered_link_idx` the rects are painted
    /// once (the style is idempotent, but de-duping reads more clearly and
    /// avoids redundant buffer writes).
    pub(super) fn paint_link_highlights(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        style: Style,
        index_range: std::ops::Range<usize>,
    ) {
        let mut paint = |idx: usize| {
            if !index_range.contains(&idx) {
                return;
            }
            if let Some(link) = self.visible_link_map.links().get(idx) {
                for r in &link.rects {
                    buf.set_style(*r, style);
                }
            }
        };
        if let Some(idx) = self.highlighted_link_idx {
            paint(idx);
        }
        if let Some(idx) = self.hovered_link_idx
            && Some(idx) != self.highlighted_link_idx
        {
            paint(idx);
        }
    }
}
#[cfg(test)]
mod link_click_tests {
    use super::test_fixtures::make_agent;
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::agent::{AgentId, AgentState};
    use crate::app::app_view::InputOutcome;
    use crate::render::osc8::{LinkOverlay, OverlayLink};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use std::sync::Arc;
    /// Set up the agent so clicks at (col, row) land in the scrollback pane.
    fn setup_scrollback_area(agent: &mut AgentView, area: Rect) {
        agent.pane_areas.scrollback = area;
        agent.active_pane = AgentPane::Scrollback;
    }
    /// Add a link to the visible_link_map covering (col_start..col_end, row).
    fn add_visible_target(
        agent: &mut AgentView,
        row: u16,
        col_start: u16,
        col_end: u16,
        target: crate::render::osc8::LinkTarget,
    ) {
        let mut overlay = LinkOverlay::new();
        overlay.push(OverlayLink {
            screen_row: row,
            col_start,
            col_end,
            target,
            presentation: crate::render::osc8::LinkPresentation::Opaque,
            id: Some(1),
        });
        agent.visible_link_map.rebuild(1, &overlay, vec![]);
    }
    fn add_visible_link(agent: &mut AgentView, row: u16, col_start: u16, col_end: u16, url: &str) {
        add_visible_target(
            agent,
            row,
            col_start,
            col_end,
            crate::render::osc8::LinkTarget::Url(Arc::from(url)),
        );
    }
    fn mouse_down(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }
    fn mouse_up(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }
    fn mouse_drag(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }
    fn mouse_moved(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }
    /// One selectable line on screen row 5, cols 0..40, in an 80x24
    /// scrollback pane — the shared surface for the drag-latch tests.
    fn install_selectable_line(agent: &mut AgentView) {
        setup_scrollback_area(agent, Rect::new(0, 0, 80, 24));
        let mut model = ResolvedSelectionModel::default();
        model.push_line(crate::scrollback::text_selection::ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 5,
            screen_x: 0,
            selectable_cols: 0..40,
            text: "selectable scrollback line for drag selection".into(),
            painted_region: None,
            joiner_to_previous: None,
        });
        agent.update_scrollback_selection_state(model, Default::default());
    }
    /// Drive a real `Down`→`Drag` through `handle_input` on a selectable
    /// scrollback line so `drag_selection` is genuinely promoted, then leave the
    /// button held with no `Up` — the latched state the recovery guard targets.
    fn latch_real_scrollback_drag(agent: &mut AgentView, reg: &ActionRegistry) {
        install_selectable_line(agent);
        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 5)), reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 5)), reg);
        assert!(
            agent.drag_selection.is_some(),
            "setup: Down→Drag on a selectable line must promote drag_selection"
        );
        assert!(agent.left_mouse_down, "setup: button must still be held");
    }
    fn mouse_button_event(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }
    #[test]
    fn esc_unsticks_latched_drag() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_real_scrollback_drag(&mut agent, &reg);
        let _ = agent.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &reg,
        );
        assert!(!agent.left_mouse_down);
        assert!(agent.drag_selection.is_none());
        assert!(agent.pending_text_drag.is_none());
        assert!(agent.pending_block_drag.is_none());
        assert!(agent.block_drag_selection.is_none());
        assert!(!agent.scrollbar_dragging);
        let _ = agent.handle_input(&Event::Mouse(mouse_moved(30, 5)), &reg);
        assert!(agent.drag_selection.is_none());
    }
    #[test]
    fn live_drag_events_not_interrupted() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_real_scrollback_drag(&mut agent, &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(20, 5)), &reg);
        assert!(agent.drag_selection.is_some());
    }
    /// A bare `Moved` while the latch is held means the release was lost:
    /// the drag finishes as the Up would have (copy delivered, highlight
    /// persisted) instead of extending on every hover forever.
    #[test]
    fn bare_moved_finishes_lost_up_drag() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_real_scrollback_drag(&mut agent, &reg);
        let outcome = agent.handle_input(&Event::Mouse(mouse_moved(30, 5)), &reg);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.left_mouse_down);
        assert!(agent.drag_selection.is_none(), "finished, not extended");
        assert!(
            agent.persistent_text_selection.is_some(),
            "the finished drag delivered its copy and persisted the highlight"
        );
        let _ = agent.handle_input(&Event::Mouse(mouse_moved(40, 5)), &reg);
        assert!(agent.drag_selection.is_none());
    }
    /// A press whose release is lost before any drag motion: the first bare
    /// `Moved` drops the latch without promoting a selection or copying.
    #[test]
    fn bare_moved_after_plain_press_drops_latch_without_promoting() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        install_selectable_line(&mut agent);
        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 5)), &reg);
        assert!(agent.pending_text_drag.is_some(), "setup: press armed");
        let _ = agent.handle_input(&Event::Mouse(mouse_moved(10, 5)), &reg);
        assert!(!agent.left_mouse_down);
        assert!(agent.pending_text_drag.is_none(), "latch dropped");
        assert!(agent.drag_selection.is_none(), "hover must not select");
        assert!(agent.persistent_text_selection.is_none(), "nothing copied");
        let _ = agent.handle_input(&Event::Mouse(mouse_moved(20, 5)), &reg);
        assert!(agent.drag_selection.is_none());
    }
    /// A press following a lost release finishes the interrupted gesture
    /// (delivering its copy) before starting the next one.
    #[test]
    fn fresh_mouse_down_finishes_prior_latched_drag() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_real_scrollback_drag(&mut agent, &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_down(5, 10)), &reg);
        assert!(
            agent.drag_selection.is_none(),
            "the stale promoted selection must not survive into the fresh press"
        );
        assert!(agent.left_mouse_down, "the new press owns the button latch");
    }
    /// Replay of a VS Code context-menu gesture captured live: the press
    /// landed on the menu (never reported), so the pager sees `Down(Right)`
    /// → `Drag(Left)`×N → `Up(Left)` with no `Down(Left)` anywhere. With no
    /// armed left gesture it must stay inert.
    #[test]
    fn vscode_menu_gesture_right_press_left_drags_left_release_is_inert() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        install_selectable_line(&mut agent);
        let _ = agent.handle_input(
            &Event::Mouse(mouse_button_event(
                MouseEventKind::Down(MouseButton::Right),
                2,
                5,
            )),
            &reg,
        );
        for col in [4u16, 8, 12, 16] {
            let _ = agent.handle_input(&Event::Mouse(mouse_drag(col, 5)), &reg);
        }
        let _ = agent.handle_input(&Event::Mouse(mouse_up(16, 5)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_moved(25, 5)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_moved(30, 5)), &reg);
        assert!(!agent.left_mouse_down);
        assert!(agent.drag_selection.is_none());
        assert!(agent.pending_text_drag.is_none());
        assert!(agent.persistent_text_selection.is_none());
        assert!(!agent.scrollback_drag_latched());
    }
    /// An unpaired right release while the left latch is held still means
    /// the gesture ended: the drag finishes with its copy.
    #[test]
    fn unpaired_right_release_finishes_left_drag() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_real_scrollback_drag(&mut agent, &reg);
        let outcome = agent.handle_input(
            &Event::Mouse(mouse_button_event(
                MouseEventKind::Up(MouseButton::Right),
                10,
                5,
            )),
            &reg,
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.left_mouse_down);
        assert!(agent.drag_selection.is_none(), "finished, not stuck");
        assert!(
            agent.persistent_text_selection.is_some(),
            "the finished drag delivered its copy and persisted the highlight"
        );
    }
    /// Replay of the wedged VS Code state captured live: `Down(Left)` per
    /// click, never `Up(Left)` or `Drag(Left)`, only bare `Moved`. Clicks
    /// must stay inert instead of growing a runaway selection each.
    #[test]
    fn wedged_terminal_clicks_without_releases_stay_inert() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        install_selectable_line(&mut agent);
        for (i, col) in [4u16, 12, 20, 28].iter().enumerate() {
            let _ = agent.handle_input(&Event::Mouse(mouse_down(*col, 5)), &reg);
            let _ = agent.handle_input(&Event::Mouse(mouse_moved(col + 3, 5)), &reg);
            let _ = agent.handle_input(&Event::Mouse(mouse_moved(col + 6, 5)), &reg);
            assert!(
                agent.drag_selection.is_none(),
                "click {i}: bare Moved must not grow a selection"
            );
            assert!(
                agent.persistent_text_selection.is_none(),
                "click {i}: nothing was selected, so nothing may persist"
            );
            assert!(!agent.left_mouse_down, "click {i}: latch dropped");
        }
    }
    /// An unpaired right drag while the left latch is held is held motion:
    /// the latch survives and the left release finishes normally.
    #[test]
    fn unpaired_right_drag_keeps_left_latch() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_real_scrollback_drag(&mut agent, &reg);
        let _ = agent.handle_input(
            &Event::Mouse(mouse_button_event(
                MouseEventKind::Drag(MouseButton::Right),
                12,
                5,
            )),
            &reg,
        );
        assert!(
            agent.drag_selection.is_some(),
            "mis-encoded drag must not drop the live gesture"
        );
        let _ = agent.handle_input(&Event::Mouse(mouse_up(14, 5)), &reg);
        assert!(agent.drag_selection.is_none());
        assert!(agent.persistent_text_selection.is_some(), "copy delivered");
    }
    #[test]
    fn non_esc_key_clears_latch() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_real_scrollback_drag(&mut agent, &reg);
        let _ = agent.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            &reg,
        );
        assert!(!agent.left_mouse_down);
        assert!(agent.drag_selection.is_none());
        assert!(agent.pending_text_drag.is_none());
        assert!(agent.pending_block_drag.is_none());
        assert!(agent.block_drag_selection.is_none());
        assert!(!agent.scrollbar_dragging);
    }
    /// Clicking the banner's [hide] button dispatches the same action as
    /// `/announcements hide`; clicks outside the cached rect do not.
    #[test]
    fn click_on_announcement_hide_button_dispatches_hide_action() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        agent
            .hit_announcement_hide
            .set(Some(Rect::new(70, 1, 6, 1)));
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(72, 1)), &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::AnnouncementsHide)),
            "[hide] click must dispatch AnnouncementsHide"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(72, 2)), &reg);
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::AnnouncementsHide)
        ));
        agent.hit_announcement_hide.set(None);
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(72, 1)), &reg);
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::AnnouncementsHide)
        ));
    }
    /// Clicking the promo banner's [label] CTA button dispatches the open
    /// action (URL resolved at dispatch time); clicks outside the cached
    /// rect (or on a collapsed banner) do not.
    #[test]
    fn click_on_announcement_cta_button_dispatches_open_action() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        agent.hit_announcement_cta.set(Some(Rect::new(0, 1, 15, 1)));
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(3, 1)), &reg);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(_))
            ),
            "[label] click must dispatch AnnouncementsOpenCta"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(3, 2)), &reg);
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::AnnouncementsOpenCta(_))
        ));
        agent.hit_announcement_cta.set(None);
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(3, 1)), &reg);
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::AnnouncementsOpenCta(_))
        ));
    }
    /// Draw one 80x30 frame with `announcements` in the banner slot — shared
    /// fixture for the banner dropdown-suppression tests so `draw`'s long
    /// positional signature is spelled once.
    fn draw_banner_frame(
        agent: &mut AgentView,
        reg: &ActionRegistry,
        announcements: &[echo_agent_announcements::RemoteAnnouncement],
        banner_height: u16,
    ) {
        draw_frame_sized(agent, reg, announcements, banner_height, 80);
    }
    fn draw_frame_sized(
        agent: &mut AgentView,
        reg: &ActionRegistry,
        announcements: &[echo_agent_announcements::RemoteAnnouncement],
        banner_height: u16,
        cols: u16,
    ) -> Buffer {
        draw_frame_privacy(agent, reg, announcements, banner_height, cols, false)
    }
    fn draw_frame_privacy(
        agent: &mut AgentView,
        reg: &ActionRegistry,
        announcements: &[echo_agent_announcements::RemoteAnnouncement],
        banner_height: u16,
        cols: u16,
        privacy_banner: bool,
    ) -> Buffer {
        let area = Rect::new(0, 0, cols, 30);
        let bundle = crate::app::bundle::BundleState::default();
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        agent.draw(
            area,
            &mut buf,
            reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams {
                height: banner_height,
                announcements,
                hidden_ids: &std::collections::BTreeSet::new(),
                privacy_banner,
                mouse_pos: None,
                tip: None,
            },
            &bundle,
            false,
            false,
            &mut Vec::new(),
            crate::app::agent_view::AppRenderParams::default(),
        );
        buf
    }
    /// Draw-path: prompt dropdowns Clear-and-paint over the banner rows after
    /// `render_banner` runs, so the same frame's rect refresh must suppress the
    /// [hide] click target — otherwise a click on a dropdown row would silently
    /// hide + persist a critical from a button that is no longer on screen.
    #[test]
    fn open_prompt_dropdown_suppresses_announcement_hide_click_target() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let critical = [echo_agent_announcements::RemoteAnnouncement {
            severity: Some("critical".into()),
            title: Some("ZZCRIT".into()),
            message: Some("outage".into()),
            ..Default::default()
        }];
        draw_banner_frame(&mut agent, &reg, &critical, 2);
        let rect = agent
            .hit_announcement_hide
            .rect
            .expect("critical banner must arm the [hide] rect");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::AnnouncementsHide)),
            "sanity: visible [hide] must dispatch"
        );
        let _ = agent.prompt.handle_paste("/");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(
            agent.prompt.any_dropdown_open(),
            "setup: slash dropdown must be open"
        );
        draw_banner_frame(&mut agent, &reg, &critical, 2);
        assert!(
            agent.hit_announcement_hide.rect.is_none(),
            "open dropdown must suppress the [hide] rect"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::AnnouncementsHide)),
            "click where [hide] used to be must not hide-and-persist under a dropdown"
        );
    }
    /// Privacy upsell banner: when the caller passes `privacy_banner: true`,
    /// the render layer gives it the slot (even over an announcement — the
    /// critical-outranks-privacy ranking lives in `AppView::draw`, which
    /// never passes `true` while a critical announcement is live), arms its
    /// three rects, and clicks dispatch the banner actions. Turning it off
    /// clears the rects.
    #[test]
    fn privacy_banner_owns_slot_and_clicks_dispatch() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let critical = [echo_agent_announcements::RemoteAnnouncement {
            severity: Some("critical".into()),
            title: Some("ZZCRIT".into()),
            message: Some("outage".into()),
            ..Default::default()
        }];
        let buf = draw_frame_privacy(&mut agent, &reg, &critical, 2, 80, true);
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.contains("Help improve EchoAgent"),
            "banner copy painted"
        );
        assert!(
            !text.contains("ZZCRIT"),
            "critical announcement yields the slot to the privacy banner"
        );
        assert!(
            agent.hit_announcement_hide.rect.is_none(),
            "announcement [hide] must not be clickable under the privacy banner"
        );
        let rect = agent
            .privacy_banner
            .hit_opt_in
            .rect
            .expect("accept rect armed");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::PrivacyBannerOptIn)
        ));
        let rect = agent
            .privacy_banner
            .hit_opt_out
            .rect
            .expect("customize rect armed");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::PrivacyBannerOptOut)
        ));
        let rect = agent
            .privacy_banner
            .hit_terms
            .rect
            .expect("terms rect armed");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::OpenUrl(ref url))
                if url == crate::views::privacy_banner::PRIVACY_BANNER_TERMS_URL
        ));
        let rect = agent
            .privacy_banner
            .hit_policy
            .rect
            .expect("privacy policy rect armed");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::OpenUrl(ref url))
                if url == crate::views::privacy_banner::PRIVACY_BANNER_POLICY_URL
        ));
        draw_frame_privacy(&mut agent, &reg, &critical, 2, 80, false);
        assert!(agent.privacy_banner.hit_opt_in.rect.is_none());
        assert!(agent.privacy_banner.hit_opt_out.rect.is_none());
        assert!(agent.privacy_banner.hit_terms.rect.is_none());
        assert!(agent.privacy_banner.hit_policy.rect.is_none());
        assert!(agent.hit_announcement_hide.rect.is_some());
    }
    /// Promo twin of the [hide] suppression test: the [label] CTA rect must
    /// also drop under an open dropdown so a dropdown click cannot open a URL
    /// from a button that is no longer on screen.
    #[test]
    fn open_prompt_dropdown_suppresses_announcement_cta_click_target() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let promo = [echo_agent_announcements::RemoteAnnouncement {
            id: Some("promo-1".into()),
            severity: Some("promo".into()),
            message: Some("ZZPROMO".into()),
            cta: Some(echo_agent_announcements::AnnouncementCta {
                label: Some("Go".into()),
                url: Some("https://echo-agent.invalid/promo".into()),
                caption: None,
            }),
            ..Default::default()
        }];
        draw_banner_frame(&mut agent, &reg, &promo, 1);
        let rect = agent
            .hit_announcement_cta
            .rect
            .expect("promo row must arm the [label] rect");
        assert!(
            agent.hit_announcement_hide.rect.is_some(),
            "promo row must arm the [hide] rect too"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(_))
            ),
            "sanity: visible [label] must dispatch"
        );
        let _ = agent.prompt.handle_paste("/");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(
            agent.prompt.any_dropdown_open(),
            "setup: slash dropdown must be open"
        );
        draw_banner_frame(&mut agent, &reg, &promo, 1);
        assert!(
            agent.hit_announcement_cta.rect.is_none(),
            "open dropdown must suppress the [label] rect"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(
            !matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(_))
            ),
            "click where [label] used to be must not open a URL under a dropdown"
        );
    }
    /// Turn-status twin of the banner suppression tests: dropdowns paint over
    /// the stop button's row, so its rect must drop while one is open — a
    /// click on dropdown chrome must never cancel the running turn.
    #[test]
    fn open_prompt_dropdown_suppresses_stop_button_click_target() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        agent.session.state = AgentState::TurnRunning;
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let rect = agent
            .hit_cancel_button
            .rect
            .expect("running turn must arm the stop rect");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x, rect.y)), &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
            "sanity: visible stop must dispatch CancelTurn"
        );
        let _ = agent.prompt.handle_paste("/");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(
            agent.prompt.any_dropdown_open(),
            "setup: slash dropdown must be open"
        );
        draw_banner_frame(&mut agent, &reg, &[], 0);
        assert!(
            agent.hit_cancel_button.rect.is_none(),
            "open dropdown must suppress the stop rect"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x, rect.y)), &reg);
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
            "click where stop used to be must not cancel the turn under a dropdown"
        );
    }
    /// Clicking the still-running watcher cue toggles the tasks pane like
    /// Ctrl+G; only the first click that reveals the pane shows the one-time
    /// shortcut toast.
    #[test]
    fn watching_cue_click_opens_tasks_pane_with_one_time_shortcut_toast() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        super::test_fixtures::add_running_bg_task(&mut agent);
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let rect = agent.hit_watching_cue.rect.expect("cue rect must be armed");
        let click = Event::Mouse(mouse_down(rect.x + 1, rect.y));
        let _ = agent.handle_input(&click, &reg);
        assert!(agent.tasks.overlay.focused);
        assert!(agent.toast.is_none(), "focus-only click must not toast");
        agent.tasks.overlay.hide();
        agent.tasks.on_state_change();
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let _ = agent.handle_input(&click, &reg);
        assert!(agent.tasks.overlay.visible && agent.tasks.overlay.focused);
        assert_eq!(agent.active_pane, AgentPane::Tasks);
        let toast = agent.toast.clone().map(|(msg, _)| msg);
        assert_eq!(toast.as_deref(), Some("Tip: Ctrl+G toggles the tasks pane"));
        agent.toast = None;
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let _ = agent.handle_input(&click, &reg);
        assert!(!agent.tasks.overlay.visible);
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let _ = agent.handle_input(&click, &reg);
        assert!(agent.tasks.overlay.visible);
        assert!(agent.toast.is_none(), "toast fires only once per session");
    }
    /// Bg twin: the `[↓]` demote button rides the same turn-status row, so its
    /// rect must drop under an open dropdown too — a dropdown click must never
    /// background the running execute tool.
    #[test]
    fn open_prompt_dropdown_suppresses_bg_button_click_target() {
        use crate::acp::meta::NotificationMeta;
        use agent_client_protocol as acp;
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        agent.session.state = AgentState::TurnRunning;
        agent.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(Arc::from("exec-1")), "sleep 5")
                    .kind(acp::ToolKind::Execute)
                    .status(acp::ToolCallStatus::InProgress)
                    .content(vec![])
                    .locations(vec![]),
            ),
            &NotificationMeta::default(),
            &mut agent.scrollback,
        );
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let rect = agent
            .hit_bg_button
            .rect
            .expect("running execute must arm the bg rect");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x, rect.y)), &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::DemoteToBackground)),
            "sanity: visible bg button must dispatch DemoteToBackground"
        );
        let _ = agent.prompt.handle_paste("/");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(
            agent.prompt.any_dropdown_open(),
            "setup: slash dropdown must be open"
        );
        draw_banner_frame(&mut agent, &reg, &[], 0);
        assert!(
            agent.hit_bg_button.rect.is_none(),
            "open dropdown must suppress the bg rect"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x, rect.y)), &reg);
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::DemoteToBackground)),
            "click where the bg button used to be must not demote under a dropdown"
        );
    }
    #[test]
    fn subagent_view_suppresses_background_button() {
        let reg = ActionRegistry::defaults();
        let mut parent = make_agent();
        let mut child = make_agent();
        super::test_fixtures::add_running_execute(&mut child);
        parent
            .subagent_views
            .insert("child-sid".into(), Box::new(child));
        assert!(!parent.subagent_views["child-sid"].is_subagent_view);
        parent.open_subagent_fullscreen("child-sid".into());
        let child = parent.subagent_views.get_mut("child-sid").unwrap();
        draw_banner_frame(child, &reg, &[], 0);
        assert!(
            child.hit_bg_button.rect.is_none(),
            "read-only child view must not advertise a background button"
        );
    }
    /// Header twin: the top-header upgrade CTA rect must drop under an open
    /// dropdown too — the only suppression consumer previously without a
    /// dropdown pin (its occluder-class twin lives below).
    #[test]
    fn open_prompt_dropdown_suppresses_header_upgrade_cta_click_target() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (120, 30);
        let promo = [echo_agent_announcements::RemoteAnnouncement {
            id: Some("promo-pin".into()),
            severity: Some("promo".into()),
            message: Some("ZZPROMO".into()),
            dismissible: Some(false),
            cta: Some(echo_agent_announcements::AnnouncementCta {
                label: Some("Upgrade Account".into()),
                url: Some("https://echo-agent.invalid/promo".into()),
                caption: None,
            }),
            ..Default::default()
        }];
        let _ = draw_frame_sized(&mut agent, &reg, &promo, 1, 120);
        let rect = agent
            .hit_upgrade_cta
            .rect
            .expect("promo must arm the header CTA rect");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(_))
            ),
            "sanity: visible header CTA must dispatch"
        );
        let _ = agent.prompt.handle_paste("/");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(
            agent.prompt.any_dropdown_open(),
            "setup: slash dropdown must be open"
        );
        let _ = draw_frame_sized(&mut agent, &reg, &promo, 1, 120);
        assert!(
            agent.hit_upgrade_cta.rect.is_none(),
            "open dropdown must suppress the header CTA rect"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(
            !matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(_))
            ),
            "click where the header CTA used to be must not open under a dropdown"
        );
    }
    /// Second suppression layer for the turn-status row: a frame occluder
    /// (the goal-detail class — NOT a dropdown, so the rects stay armed)
    /// covering the [stop] + bg buttons must swallow both clicks at dispatch
    /// time — a click on overlay text must never cancel or demote the turn.
    #[test]
    fn frame_occluder_over_stop_and_bg_buttons_swallows_clicks() {
        use crate::acp::meta::NotificationMeta;
        use agent_client_protocol as acp;
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        agent.session.state = AgentState::TurnRunning;
        agent.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(Arc::from("exec-1")), "sleep 5")
                    .kind(acp::ToolKind::Execute)
                    .status(acp::ToolCallStatus::InProgress)
                    .content(vec![])
                    .locations(vec![]),
            ),
            &NotificationMeta::default(),
            &mut agent.scrollback,
        );
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let stop = agent
            .hit_cancel_button
            .rect
            .expect("running turn must arm the stop rect");
        let bg = agent
            .hit_bg_button
            .rect
            .expect("running execute must arm the bg rect");
        assert!(
            agent.frame_occluder_rects.is_empty(),
            "setup: overlay-free frame must accumulate no occluders"
        );
        agent.frame_occluder_rects.push(Rect::new(0, stop.y, 80, 1));
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(stop.x, stop.y)), &reg);
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
            "occluded [stop] click must not cancel the turn"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(bg.x, bg.y)), &reg);
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::DemoteToBackground)),
            "occluded bg click must not demote the turn"
        );
        assert!(
            agent.hit_cancel_button.rect.is_some() && agent.hit_bg_button.rect.is_some(),
            "occluder guard is click-time: the rects stay armed"
        );
        draw_banner_frame(&mut agent, &reg, &[], 0);
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(bg.x, bg.y)), &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::DemoteToBackground)),
            "overlay-free bg click must dispatch again"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(stop.x, stop.y)), &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
            "overlay-free [stop] click must dispatch again"
        );
    }
    /// Subagent fullscreen takeover: the parent's banner/header chrome is not
    /// painted, so the takeover draw must drop the armed [hide]/[label]/header
    /// CTA rects — a stale rect would fake post-draw impressions and clicks.
    #[test]
    fn subagent_fullscreen_clears_announcement_and_header_cta_rects() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (120, 30);
        let promo = [echo_agent_announcements::RemoteAnnouncement {
            id: Some("promo-1".into()),
            severity: Some("promo".into()),
            message: Some("ZZPROMO".into()),
            cta: Some(echo_agent_announcements::AnnouncementCta {
                label: Some("Go".into()),
                url: Some("https://echo-agent.invalid/promo".into()),
                caption: None,
            }),
            ..Default::default()
        }];
        let _ = draw_frame_sized(&mut agent, &reg, &promo, 1, 120);
        assert!(
            agent.hit_announcement_cta.rect.is_some(),
            "banner CTA armed"
        );
        assert!(agent.hit_announcement_hide.rect.is_some(), "[hide] armed");
        assert!(agent.hit_upgrade_cta.rect.is_some(), "header CTA armed");
        agent.active_subagent = Some("child-sid".into());
        let _ = draw_frame_sized(&mut agent, &reg, &promo, 1, 120);
        assert!(agent.hit_announcement_cta.rect.is_none());
        assert!(agent.hit_announcement_hide.rect.is_none());
        assert!(agent.hit_upgrade_cta.rect.is_none());
    }
    /// In-session header upgrade CTA: a promo owning the slot arms
    /// `hit_upgrade_cta` (clickable → `AnnouncementsOpenCta(Header)`), and the
    /// draw caches `pinned_upgrade_cta_live` so the `Ctrl+O` arm can override
    /// YOLO — but ONLY for a pinned (non-dismissible) promo.
    #[test]
    fn header_upgrade_cta_rect_and_ctrl_o_override() {
        use crate::actions::ActionId;
        use echo_agent_telemetry::events::AnnouncementCtaSurface;
        let reg = ActionRegistry::defaults();
        let cta = || {
            Some(echo_agent_announcements::AnnouncementCta {
                label: Some("Upgrade Account".into()),
                url: Some("https://echo-agent.invalid/promo".into()),
                caption: None,
            })
        };
        let mut agent = make_agent();
        agent.last_terminal_size = (120, 30);
        let pinned = [echo_agent_announcements::RemoteAnnouncement {
            id: Some("promo-pin".into()),
            severity: Some("promo".into()),
            message: Some("ZZPROMO".into()),
            dismissible: Some(false),
            cta: cta(),
            ..Default::default()
        }];
        let buf = draw_frame_sized(&mut agent, &reg, &pinned, 1, 120);
        assert!(
            agent.pinned_upgrade_cta_live,
            "pinned promo lights the Ctrl+O override"
        );
        let rect = agent
            .hit_upgrade_cta
            .rect
            .expect("pinned promo must arm the header CTA rect");
        let header_row: String = (0..120)
            .filter_map(|x| buf.cell((x, rect.y)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(
            header_row.contains("[Upgrade Account]"),
            "row={header_row:?}"
        );
        assert!(
            !header_row.contains("Ctrl+O"),
            "top-header button must stay bare; row={header_row:?}"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(rect.x + 1, rect.y)), &reg);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(AnnouncementCtaSurface::Header))
            ),
            "header CTA click opens with the Header surface"
        );
        assert!(
            matches!(
                agent.handle_agent_action(ActionId::ToggleYolo),
                InputOutcome::Action(Action::AnnouncementsOpenCta(
                    AnnouncementCtaSurface::Keyboard
                ))
            ),
            "Ctrl+O opens the pinned CTA (Keyboard surface) instead of YOLO"
        );
        let mut agent = make_agent();
        agent.last_terminal_size = (120, 30);
        let dismissible = [echo_agent_announcements::RemoteAnnouncement {
            id: Some("promo-dis".into()),
            severity: Some("promo".into()),
            message: Some("ZZPROMO".into()),
            cta: cta(),
            ..Default::default()
        }];
        draw_frame_sized(&mut agent, &reg, &dismissible, 1, 120);
        assert!(
            !agent.pinned_upgrade_cta_live,
            "dismissible promo must not steal Ctrl+O"
        );
        assert!(
            agent.hit_upgrade_cta.rect.is_some(),
            "dismissible promo still shows the clickable header CTA"
        );
        assert!(
            matches!(
                agent.handle_agent_action(ActionId::ToggleYolo),
                InputOutcome::Action(Action::SetYoloMode(_))
            ),
            "Ctrl+O keeps toggling YOLO for a dismissible promo"
        );
        let mut agent = make_agent();
        agent.last_terminal_size = (120, 30);
        draw_frame_sized(&mut agent, &reg, &[], 0, 120);
        assert!(
            agent.hit_upgrade_cta.rect.is_none(),
            "no promo → no header CTA"
        );
        assert!(!agent.pinned_upgrade_cta_live);
        assert!(matches!(
            agent.handle_agent_action(ActionId::ToggleYolo),
            InputOutcome::Action(Action::SetYoloMode(_))
        ));
    }
    /// A non-dismissible promo draws with the CTA armed but NO [hide] click
    /// target (`BannerHits.hide` is None, so the mouse hide path is dead).
    #[test]
    fn non_dismissible_promo_arms_cta_but_no_hide_rect() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let promo = [echo_agent_announcements::RemoteAnnouncement {
            id: Some("promo-pin".into()),
            severity: Some("promo".into()),
            message: Some("ZZPROMO".into()),
            dismissible: Some(false),
            cta: Some(echo_agent_announcements::AnnouncementCta {
                label: Some("Go".into()),
                url: Some("https://echo-agent.invalid/promo".into()),
                caption: None,
            }),
            ..Default::default()
        }];
        draw_banner_frame(&mut agent, &reg, &promo, 1);
        assert!(
            agent.hit_announcement_cta.rect.is_some(),
            "pinned promo keeps its CTA clickable"
        );
        assert!(
            agent.hit_announcement_hide.rect.is_none(),
            "pinned promo must arm no [hide] target"
        );
    }
    /// Second suppression layer: a frame occluder (the goal-detail overlay
    /// class — registered in `frame_occluder_rects`, NOT a dropdown, so the
    /// banner rects stay armed) covering the banner row must swallow both
    /// button clicks (`pos_occluded` guard) AND drop the promo OSC 8 span
    /// whole; the next overlay-free frame re-enables all three. The span half
    /// pins `push_promo_cta_link_span` directly — `draw` only calls it behind
    /// the process-global `hyperlink_route().emit_osc8` gate, which is
    /// brand-dependent and unforceable per-test.
    #[test]
    fn frame_occluder_over_banner_swallows_clicks_and_drops_cta_link_span() {
        let reg = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let promo = [echo_agent_announcements::RemoteAnnouncement {
            id: Some("promo-1".into()),
            severity: Some("promo".into()),
            message: Some("ZZPROMO".into()),
            cta: Some(echo_agent_announcements::AnnouncementCta {
                label: Some("Go".into()),
                url: Some("https://echo-agent.invalid/promo".into()),
                caption: None,
            }),
            ..Default::default()
        }];
        let no_hidden = std::collections::BTreeSet::new();
        draw_banner_frame(&mut agent, &reg, &promo, 1);
        let cta = agent.hit_announcement_cta.rect.expect("cta rect armed");
        let hide = agent.hit_announcement_hide.rect.expect("hide rect armed");
        assert!(
            agent.frame_occluder_rects.is_empty(),
            "setup: overlay-free frame must accumulate no occluders"
        );
        agent.frame_occluder_rects.push(Rect::new(0, cta.y, 80, 1));
        let mut spans = Vec::new();
        agent.push_promo_cta_link_span(&mut spans, &promo, &no_hidden);
        assert!(spans.is_empty(), "occluded [label] must emit no OSC 8 span");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(cta.x + 1, cta.y)), &reg);
        assert!(
            !matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(_))
            ),
            "occluded [label] click must not open a URL"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(hide.x + 1, hide.y)), &reg);
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::AnnouncementsHide)),
            "occluded [hide] click must not hide-and-persist"
        );
        draw_banner_frame(&mut agent, &reg, &promo, 1);
        agent.push_promo_cta_link_span(&mut spans, &promo, &no_hidden);
        assert_eq!(spans.len(), 1, "overlay-free frame must emit the span");
        assert_eq!(
            (spans[0].row, spans[0].col_start, spans[0].col_end),
            (cta.y, cta.x, cta.x + cta.width),
            "span must cover exactly the [label] button cells"
        );
        assert_eq!(&*spans[0].url, "https://echo-agent.invalid/promo");
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(cta.x + 1, cta.y)), &reg);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::AnnouncementsOpenCta(_))
            ),
            "overlay-free [label] click must dispatch"
        );
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(hide.x + 1, hide.y)), &reg);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::AnnouncementsHide)),
            "overlay-free [hide] click must dispatch"
        );
    }
    #[test]
    fn drag_clears_pending_link_click() {
        let mut agent = make_agent();
        let area = Rect::new(0, 0, 80, 24);
        setup_scrollback_area(&mut agent, area);
        agent.pending_link_click = Some((
            15,
            5,
            crate::render::osc8::LinkTarget::Url("https://example.com".into()),
        ));
        agent.left_mouse_down = true;
        let outcome = agent.handle_mouse(&mouse_drag(16, 5));
        assert!(matches!(
            outcome,
            InputOutcome::Changed | InputOutcome::Unchanged
        ));
        assert!(agent.pending_link_click.is_none());
    }
    #[test]
    fn up_at_same_position_returns_open_link_action() {
        let mut agent = make_agent();
        let area = Rect::new(0, 0, 80, 24);
        setup_scrollback_area(&mut agent, area);
        agent.pending_link_click = Some((
            15,
            5,
            crate::render::osc8::LinkTarget::Url("https://example.com".into()),
        ));
        agent.left_mouse_down = true;
        let outcome = agent.handle_mouse(&mouse_up(15, 5));
        match outcome {
            InputOutcome::Action(Action::OpenLink(target)) => {
                assert_eq!(
                    target,
                    crate::render::osc8::LinkTarget::Url("https://example.com".into())
                );
            }
            other => panic!("expected Action::OpenLink, got {other:?}"),
        }
    }
    /// A modifier+click preserves a filesystem target through app activation
    /// (Ctrl on Linux/Windows; macOS polls CoreGraphics so the Down step isn't
    /// reproducible in a unit test).
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn modifier_click_on_file_link_opens_via_our_handler() {
        let mut agent = make_agent();
        let area = Rect::new(0, 0, 80, 24);
        setup_scrollback_area(&mut agent, area);
        add_visible_target(
            &mut agent,
            5,
            10,
            30,
            crate::render::osc8::LinkTarget::File(Arc::from(std::path::Path::new(
                "/tmp/session/images/1.png",
            ))),
        );
        let mut down = mouse_down(15, 5);
        down.modifiers = crossterm::event::KeyModifiers::CONTROL;
        assert!(matches!(agent.handle_mouse(&down), InputOutcome::Changed));
        match agent.handle_mouse(&mouse_up(15, 5)) {
            InputOutcome::Action(Action::OpenLink(target)) => {
                assert_eq!(
                    target,
                    crate::render::osc8::LinkTarget::File(Arc::from(std::path::Path::new(
                        "/tmp/session/images/1.png",
                    )))
                );
            }
            other => panic!("expected Action::OpenLink(file), got {other:?}"),
        }
    }
    fn test_link(url: &str, painted_w: u16) -> crate::scrollback::VisibleLink {
        crate::scrollback::VisibleLink {
            rects: vec![Rect::new(0, 0, painted_w, 1)],
            target: crate::render::osc8::LinkTarget::Url(std::sync::Arc::from(url)),
            id: None,
        }
    }
    fn test_file_link(path: &std::path::Path, painted_w: u16) -> crate::scrollback::VisibleLink {
        crate::scrollback::VisibleLink {
            rects: vec![Rect::new(0, 0, painted_w, 1)],
            target: crate::render::osc8::LinkTarget::File(Arc::from(path)),
            id: None,
        }
    }
    #[test]
    fn app_should_open_link_visibility_matrix() {
        let bare = "https://example.com";
        let bare_w = unicode_width::UnicodeWidthStr::width(bare) as u16;
        let mailto = "mailto:a@b.com";
        let mailto_w = unicode_width::UnicodeWidthStr::width(mailto) as u16;
        let file = "file:///tmp/session/images/1.png";
        let file_w = unicode_width::UnicodeWidthStr::width(file) as u16;
        assert!(app_should_open_link_on_click_with(
            false,
            &test_link(bare, bare_w)
        ));
        assert!(app_should_open_link_on_click_with(
            false,
            &test_link(bare, 4)
        ));
        assert!(!app_should_open_link_on_click_with(
            true,
            &test_link(bare, bare_w)
        ));
        assert!(!app_should_open_link_on_click_with(
            true,
            &test_link(mailto, mailto_w)
        ));
        assert!(app_should_open_link_on_click_with(
            true,
            &test_link(bare, 4)
        ));
        assert!(app_should_open_link_on_click_with(
            true,
            &test_link(bare, bare_w.saturating_add(40))
        ));
        let file_path = std::path::Path::new("/tmp/session/images/1.png");
        assert!(app_should_open_link_on_click_with(
            true,
            &test_file_link(file_path, file_w)
        ));
        assert!(app_should_open_link_on_click_with(
            true,
            &test_file_link(file_path, 8)
        ));
    }
    /// Regression: while the plan preview (line viewer) is open and the
    /// feedback prompt is focused, a left-drag that starts in the prompt
    /// must keep routing Drag/Up to the prompt so text selection works in
    /// the input — even when the pointer wanders up into the plan area. It
    /// must NOT be swallowed by the line viewer's plan-line gutter drag.
    #[test]
    fn plan_feedback_drag_routes_to_prompt_not_line_viewer() {
        let mut agent = make_agent();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "test-session".into(),
            tool_call_id: "call-1".into(),
            plan_content: Some("# Plan\n\nStep one\nStep two".into()),
        };
        agent.plan_approval_view = Some(
            crate::views::plan_approval_view::PlanApprovalViewState::new(
                request,
                crate::views::prompt_widget::StashedPrompt {
                    text: String::new(),
                    cursor: 0,
                    images: Vec::new(),
                    chip_elements: Vec::new(),
                    image_counter: 0,
                    image_undo_stash: Vec::new(),
                },
                tx,
            ),
        );
        agent.show_plan_preview();
        assert!(
            agent.line_viewer.is_some(),
            "plan preview should open the line viewer"
        );
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.focus = PlanApprovalFocus::Prompt;
        }
        agent.pane_areas.prompt = Rect::new(0, 20, 80, 3);
        let registry = ActionRegistry::defaults();
        let _ = agent.handle_input(&Event::Mouse(mouse_down(5, 21)), &registry);
        assert!(
            agent.plan_prompt_mouse_drag,
            "down in prompt should arm the prompt drag"
        );
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(40, 5)), &registry);
        assert!(
            agent.plan_prompt_mouse_drag,
            "drag should keep the prompt drag armed"
        );
        assert!(
            agent
                .line_viewer
                .as_ref()
                .and_then(|v| v.plan_ref())
                .and_then(|p| p.gutter_drag_start)
                .is_none(),
            "forwarded drag must not start a plan-line gutter selection"
        );
        let _ = agent.handle_input(&Event::Mouse(mouse_up(40, 5)), &registry);
        assert!(
            !agent.plan_prompt_mouse_drag,
            "up should disarm the prompt drag"
        );
    }
    #[test]
    fn up_at_different_position_does_not_open_url() {
        let mut agent = make_agent();
        let area = Rect::new(0, 0, 80, 24);
        setup_scrollback_area(&mut agent, area);
        agent.pending_link_click = Some((
            15,
            5,
            crate::render::osc8::LinkTarget::Url("https://example.com".into()),
        ));
        agent.left_mouse_down = true;
        let outcome = agent.handle_mouse(&mouse_up(16, 5));
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::OpenLink(_))
        ));
        assert!(agent.pending_link_click.is_none());
    }
    #[test]
    fn down_on_non_link_clears_pending_link_click() {
        let mut agent = make_agent();
        let area = Rect::new(0, 0, 80, 24);
        setup_scrollback_area(&mut agent, area);
        add_visible_link(&mut agent, 5, 10, 30, "https://example.com");
        agent.pending_link_click = Some((
            15,
            5,
            crate::render::osc8::LinkTarget::Url("https://example.com".into()),
        ));
        let outcome = agent.handle_mouse(&mouse_down(5, 3));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.pending_link_click.is_none());
    }
    /// The lost-release finish drops the press's link arm so a later
    /// unrelated Up can't fire it.
    #[test]
    fn moved_with_left_mouse_down_finishes_press_and_drops_link_arm() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        let area = Rect::new(0, 0, 80, 24);
        setup_scrollback_area(&mut agent, area);
        agent.pending_link_click = Some((
            15,
            5,
            crate::render::osc8::LinkTarget::Url("https://example.com".into()),
        ));
        agent.left_mouse_down = true;
        agent.pending_text_drag = Some(PendingTextDrag {
            start_col: 15,
            start_row: 5,
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                col_within_range: 15,
            },
            anchor_content_width: None,
        });
        let outcome = agent.handle_input(&Event::Mouse(mouse_moved(16, 5)), &reg);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.left_mouse_down, "lost release finished the press");
        assert!(agent.pending_text_drag.is_none());
        assert!(
            agent.pending_link_click.is_none(),
            "the finished press must not leave its link arm dangling"
        );
    }
    #[test]
    fn pending_link_click_survives_up_on_non_scrollback_pane() {
        let mut agent = make_agent();
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 20));
        agent.active_pane = AgentPane::Prompt;
        agent.pending_link_click = Some((
            15,
            5,
            crate::render::osc8::LinkTarget::Url("https://example.com".into()),
        ));
        agent.left_mouse_down = true;
        let outcome = agent.handle_mouse(&mouse_up(15, 5));
        match outcome {
            InputOutcome::Action(Action::OpenLink(target)) => {
                assert_eq!(
                    target,
                    crate::render::osc8::LinkTarget::Url("https://example.com".into())
                );
            }
            other => panic!("expected Action::OpenLink, got {other:?}"),
        }
    }
    /// `/btw` panel links share the pane-agnostic Up path: once Down records
    /// `pending_link_click` (mouse-fallback terminals, Cmd/Ctrl held), Up over
    /// the panel opens the URL even though active_pane is Prompt.
    #[test]
    fn btw_panel_pending_link_click_opens_on_up() {
        let mut agent = make_agent();
        agent.last_btw_area = Rect::new(2, 18, 70, 6);
        agent.active_pane = AgentPane::Prompt;
        agent.btw_focused = true;
        add_visible_link(&mut agent, 20, 4, 40, "https://example.com/btw");
        agent.pending_link_click = Some((
            10,
            20,
            crate::render::osc8::LinkTarget::Url("https://example.com/btw".into()),
        ));
        agent.left_mouse_down = true;
        let outcome = agent.handle_mouse(&mouse_up(10, 20));
        match outcome {
            InputOutcome::Action(Action::OpenLink(target)) => {
                assert_eq!(
                    target,
                    crate::render::osc8::LinkTarget::Url("https://example.com/btw".into())
                );
            }
            other => panic!("expected Action::OpenLink for btw link, got {other:?}"),
        }
    }
    /// On mouse-fallback terminals, Down on a `/btw` link with the link
    /// modifier records `pending_link_click` instead of starting text drag.
    #[test]
    fn btw_panel_down_on_link_sets_pending_when_mouse_fallback() {
        if has_native_link_hover() {
            return;
        }
        let mut agent = make_agent();
        agent.last_btw_area = Rect::new(2, 18, 70, 6);
        agent.active_pane = AgentPane::Prompt;
        add_visible_link(&mut agent, 20, 4, 40, "https://example.com/btw");
        let outcome = agent.handle_mouse(&mouse_down(10, 20));
        assert!(matches!(
            outcome,
            InputOutcome::Changed | InputOutcome::Unchanged
        ));
        assert!(agent.pending_link_click.is_none());
        #[cfg(not(target_os = "macos"))]
        {
            let mut down = mouse_down(10, 20);
            down.modifiers = crossterm::event::KeyModifiers::CONTROL;
            let outcome = agent.handle_mouse(&down);
            assert!(matches!(outcome, InputOutcome::Changed));
            assert_eq!(
                agent.pending_link_click.as_ref(),
                Some(&(
                    10,
                    20,
                    crate::render::osc8::LinkTarget::Url("https://example.com/btw".into())
                ))
            );
        }
    }
    #[test]
    fn needs_link_modifier_poll_true_over_btw_area() {
        let mut agent = make_agent();
        agent.visible_link_map.rebuild(
            1,
            &{
                let mut o = LinkOverlay::new();
                o.push(OverlayLink {
                    screen_row: 20,
                    col_start: 4,
                    col_end: 40,
                    target: crate::render::osc8::LinkTarget::Url(Arc::from(
                        "https://example.com/btw",
                    )),
                    presentation: crate::render::osc8::LinkPresentation::Opaque,
                    id: Some(1),
                });
                o
            },
            vec![],
        );
        agent.last_btw_area = Rect::new(2, 18, 70, 6);
        agent.last_mouse_pos = (10, 20);
        agent.last_mouse_moved_at = Some(Instant::now());
        agent.hovered_entry = None;
        if has_native_link_hover() {
            assert!(!agent.needs_link_modifier_poll());
        } else {
            assert!(agent.needs_link_modifier_poll());
        }
    }
    /// The Cmd link-hover poll is bounded by pointer activity: a pointer
    /// merely resting over content (hovered_entry set, no recent movement)
    /// must not demand ticks forever — that held a permanent ~30fps loop
    /// with a CoreGraphics query per tick on macOS. An active link
    /// highlight keeps polling regardless (so Cmd release is observed).
    #[test]
    #[cfg(target_os = "macos")]
    fn needs_link_modifier_poll_expires_without_recent_mouse_movement() {
        let mut agent = make_agent();
        add_multiple_links(&mut agent);
        agent.hovered_entry = Some(0);
        agent.last_mouse_moved_at = None;
        assert!(!agent.needs_link_modifier_poll());
        agent.last_mouse_moved_at = Some(Instant::now());
        assert_eq!(agent.needs_link_modifier_poll(), !has_native_link_hover());
        agent.last_mouse_moved_at = Some(
            Instant::now()
                - AgentView::LINK_MODIFIER_POLL_WINDOW
                - std::time::Duration::from_millis(1),
        );
        assert!(!agent.needs_link_modifier_poll());
        agent.hovered_link_idx = Some(0);
        assert_eq!(agent.needs_link_modifier_poll(), !has_native_link_hover());
    }
    fn add_multiple_links(agent: &mut AgentView) {
        let mut overlay = LinkOverlay::new();
        for (i, url) in ["https://a.com", "https://b.com", "https://c.com"]
            .iter()
            .enumerate()
        {
            overlay.push(OverlayLink {
                screen_row: i as u16,
                col_start: 0,
                col_end: 10,
                target: crate::render::osc8::LinkTarget::Url(Arc::from(*url)),
                presentation: crate::render::osc8::LinkPresentation::Opaque,
                id: Some(i as u32),
            });
        }
        agent.visible_link_map.rebuild(1, &overlay, vec![]);
    }
    fn add_colliding_id_links(agent: &mut AgentView) {
        let mut overlay = LinkOverlay::new();
        for (i, url) in [
            "https://first.com",
            "https://second.com",
            "https://third.com",
        ]
        .iter()
        .enumerate()
        {
            overlay.push(OverlayLink {
                screen_row: (i as u16) + 3,
                col_start: 0,
                col_end: 10,
                target: crate::render::osc8::LinkTarget::Url(Arc::from(*url)),
                presentation: crate::render::osc8::LinkPresentation::Opaque,
                id: Some(0),
            });
        }
        agent.visible_link_map.rebuild(1, &overlay, vec![]);
    }
    #[test]
    fn cycle_forward_from_none_selects_first() {
        let mut agent = make_agent();
        add_multiple_links(&mut agent);
        agent.cycle_highlighted_link(true);
        assert_eq!(agent.highlighted_link_idx, Some(0));
        assert_eq!(
            agent.highlighted_link_url().as_deref(),
            Some("https://a.com")
        );
    }
    #[test]
    fn cycle_backward_from_none_selects_last() {
        let mut agent = make_agent();
        add_multiple_links(&mut agent);
        agent.cycle_highlighted_link(false);
        assert_eq!(agent.highlighted_link_idx, Some(2));
        assert_eq!(
            agent.highlighted_link_url().as_deref(),
            Some("https://c.com")
        );
    }
    #[test]
    fn cycle_forward_wraps_around() {
        let mut agent = make_agent();
        add_multiple_links(&mut agent);
        agent.highlighted_link_idx = Some(2);
        agent.cycle_highlighted_link(true);
        assert_eq!(agent.highlighted_link_idx, Some(0));
    }
    #[test]
    fn cycle_backward_wraps_around() {
        let mut agent = make_agent();
        add_multiple_links(&mut agent);
        agent.highlighted_link_idx = Some(0);
        agent.cycle_highlighted_link(false);
        assert_eq!(agent.highlighted_link_idx, Some(2));
    }
    #[test]
    fn cycle_with_no_links_clears_index() {
        let mut agent = make_agent();
        agent.highlighted_link_idx = Some(5);
        agent.cycle_highlighted_link(true);
        assert_eq!(agent.highlighted_link_idx, None);
    }
    #[test]
    fn colliding_ids_hover_paints_only_the_hit_link() {
        let mut agent = make_agent();
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        add_colliding_id_links(&mut agent);
        assert_eq!(agent.visible_link_map.len(), 3);
        assert_eq!(
            agent.visible_link_map.link_at(5, 4).map(|l| &l.target),
            Some(&crate::render::osc8::LinkTarget::Url(Arc::from(
                "https://second.com"
            )))
        );
        agent.last_mouse_pos = (5, 4);
        if !has_native_link_hover() {
            assert!(agent.update_hovered_link(true));
            assert_eq!(agent.hovered_link_idx, Some(1));
        } else {
            agent.hovered_link_idx = Some(1);
        }
        let style = Style::default().add_modifier(ratatui::style::Modifier::UNDERLINED);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 8));
        agent.paint_link_highlights(&mut buf, style, 0..agent.visible_link_map.len());
        assert!(
            buf[(5, 4)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
        assert!(
            !buf[(5, 3)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
        assert!(
            !buf[(5, 5)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }
    #[test]
    fn colliding_ids_modifier_click_opens_hit_url() {
        if has_native_link_hover() {
            return;
        }
        let mut agent = make_agent();
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        add_colliding_id_links(&mut agent);
        #[cfg(not(target_os = "macos"))]
        {
            let mut down = mouse_down(5, 5);
            down.modifiers = crossterm::event::KeyModifiers::CONTROL;
            assert!(matches!(agent.handle_mouse(&down), InputOutcome::Changed));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(agent.try_arm_link_click(5, 5));
            agent.left_mouse_down = true;
        }
        match agent.handle_mouse(&mouse_up(5, 5)) {
            InputOutcome::Action(Action::OpenLink(target)) => {
                assert_eq!(
                    target,
                    crate::render::osc8::LinkTarget::Url(Arc::from("https://third.com"))
                );
            }
            other => panic!("expected Action::OpenLink(third), got {other:?}"),
        }
    }
    #[test]
    fn enter_opens_highlighted_link() {
        let mut agent = make_agent();
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        add_multiple_links(&mut agent);
        agent.highlighted_link_idx = Some(1);
        let registry = ActionRegistry::defaults();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&enter, &registry);
        match outcome {
            InputOutcome::Action(Action::OpenLink(target)) => {
                assert_eq!(
                    target,
                    crate::render::osc8::LinkTarget::Url("https://b.com".into())
                );
            }
            other => panic!("expected Action::OpenLink, got {other:?}"),
        }
        assert_eq!(agent.highlighted_link_idx, None);
    }
    #[test]
    fn enter_without_highlight_does_not_open_link() {
        let mut agent = make_agent();
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        add_multiple_links(&mut agent);
        assert_eq!(agent.highlighted_link_idx, None);
        let registry = ActionRegistry::defaults();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&enter, &registry);
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::OpenLink(_))
        ));
    }
    /// Enter with a previous user prompt selected enters inline edit mode
    /// (edit-and-resubmit) instead of falling through to OpenBlockViewer.
    #[test]
    fn enter_on_selected_user_prompt_enters_inline_edit() {
        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::user_prompt(
                "fix the bug",
            ));
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::agent_message("done"));
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
        let registry = ActionRegistry::defaults();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&enter, &registry);
        if crate::app::inline_edit::INLINE_EDIT_ENABLED {
            assert!(matches!(outcome, InputOutcome::Changed), "got {outcome:?}");
            assert!(agent.inline_edit.is_some(), "Enter must start inline edit");
        } else {
            assert!(agent.inline_edit.is_none(), "feature gated off: no edit");
            assert!(
                matches!(outcome, InputOutcome::Action(Action::OpenBlockViewer)),
                "gated off: Enter must fall through to OpenBlockViewer, got {outcome:?}"
            );
        }
    }
    /// Bash prompts are not inline-editable: Enter falls through to the
    /// registry (OpenBlockViewer) exactly as before.
    #[test]
    fn enter_on_selected_bash_prompt_falls_through() {
        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::bash_prompt("ls"));
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
        let registry = ActionRegistry::defaults();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&enter, &registry);
        assert!(agent.inline_edit.is_none());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::OpenBlockViewer)),
            "expected fall-through to OpenBlockViewer, got {outcome:?}"
        );
    }
    /// Double-click on a user prompt: enters inline edit when the feature is
    /// enabled; while gated off it does NOT edit (falls through to the fold
    /// arm), leaving the prompt free for text selection. Written for both flag
    /// states so it stays valid when INLINE_EDIT_ENABLED is flipped back on.
    #[test]
    fn double_click_on_user_prompt_enters_inline_edit() {
        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::user_prompt(
                "fix the bug",
            ));
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::agent_message("done"));
        agent.scrollback.prepare_layout(80, 40);
        let now = std::time::Instant::now();
        (agent.last_click, _) = agent.handle_scrollback_click(now, 0, false);
        let _ = agent.handle_scrollback_click(now + std::time::Duration::from_millis(10), 0, false);
        if crate::app::inline_edit::INLINE_EDIT_ENABLED {
            assert!(
                agent.inline_edit.is_some(),
                "double-click must start inline edit"
            );
        } else {
            assert!(
                agent.inline_edit.is_none(),
                "feature gated off: double-click must not edit"
            );
        }
    }
    #[test]
    fn enter_on_subagent_group_header_falls_through_to_group_toggle() {
        let mut agent = make_agent();
        let mut appearance = crate::appearance::AppearanceConfig::default();
        appearance.scrollback.display.group_max_visible = 3;
        agent.scrollback.set_appearance(appearance);
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::Subagent(
                crate::scrollback::blocks::SubagentBlock::started(
                    "child task",
                    "child-sid",
                    "general-purpose",
                    None,
                    None,
                    None,
                    false,
                ),
            ));
        for i in 0..5 {
            agent
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::tool_call(
                    format!("Tool{i}"),
                    "info",
                    true,
                ));
        }
        for i in 0..6 {
            if let Some(e) = agent.scrollback.entry_mut(i) {
                e.display_mode = crate::scrollback::types::DisplayMode::Collapsed;
            }
        }
        agent
            .subagent_views
            .insert("child-sid".into(), Box::new(make_agent()));
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
        assert!(agent.scrollback.is_selected_group_header());
        let registry = ActionRegistry::defaults();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&enter, &registry);
        assert!(
            agent.active_subagent.is_none(),
            "Enter on a group header must not open the hidden entry's subagent fullscreen"
        );
        assert!(
            matches!(outcome, InputOutcome::Action(Action::OpenBlockViewer)),
            "Enter must fall through to OpenBlockViewer (group toggle), got {outcome:?}"
        );
    }
    #[test]
    fn single_click_on_plan_tool_group_header_does_not_open_plan_preview() {
        use crate::scrollback::blocks::tool::{OtherToolCallBlock, ToolCallBlock};
        let mut agent = make_agent();
        let mut appearance = crate::appearance::AppearanceConfig::default();
        appearance.scrollback.display.group_max_visible = 3;
        agent.scrollback.set_appearance(appearance);
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::ToolCall(
                ToolCallBlock::Other(OtherToolCallBlock::new(
                    "enter_plan_mode",
                    "enter plan mode",
                )),
            ));
        for i in 0..5 {
            agent
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::tool_call(
                    format!("Tool{i}"),
                    "info",
                    true,
                ));
        }
        for i in 0..6 {
            if let Some(e) = agent.scrollback.entry_mut(i) {
                e.display_mode = crate::scrollback::types::DisplayMode::Collapsed;
            }
        }
        agent.scrollback.prepare_layout(80, 40);
        let _ = agent.handle_scrollback_click(std::time::Instant::now(), 0, false);
        assert!(agent.scrollback.is_selected_group_header());
        assert!(
            agent.toast.is_none(),
            "single click on a group header must not trigger the plan preview, got toast {:?}",
            agent.toast
        );
    }
    #[test]
    fn esc_clears_highlighted_link() {
        let mut agent = make_agent();
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        add_multiple_links(&mut agent);
        agent.highlighted_link_idx = Some(0);
        let registry = ActionRegistry::defaults();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&esc, &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.highlighted_link_idx, None);
    }
    #[test]
    fn highlighted_link_url_returns_none_for_out_of_bounds() {
        let mut agent = make_agent();
        add_multiple_links(&mut agent);
        agent.highlighted_link_idx = Some(99);
        assert!(agent.highlighted_link_url().is_none());
    }
    fn make_search_agent() -> (AgentView, ActionRegistry) {
        use crate::scrollback::block::RenderBlock;
        let mut agent = make_agent();
        agent.vim_mode = true;
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("foo one"));
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("foo two"));
        agent.scrollback.prepare_layout(80, 24);
        (agent, ActionRegistry::defaults())
    }
    /// Empty-scrollback (new-session) counterpart to `make_search_agent`.
    fn make_empty_vim_agent() -> (AgentView, ActionRegistry) {
        let mut agent = make_agent();
        agent.vim_mode = true;
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        assert!(agent.scrollback.is_empty());
        (agent, ActionRegistry::defaults())
    }
    fn press(agent: &mut AgentView, reg: &ActionRegistry, code: KeyCode) -> InputOutcome {
        agent.handle_scrollback_key(&KeyEvent::new(code, KeyModifiers::NONE), reg)
    }
    fn type_query(agent: &mut AgentView, reg: &ActionRegistry, query: &str) {
        for c in query.chars() {
            press(agent, reg, KeyCode::Char(c));
        }
    }
    /// Wait for the daemon to publish the result of the most recent keystroke.
    ///
    /// One keystroke is one atomic `Update`, so it yields exactly one snapshot
    /// bump; break on the first `poll` that observes it. Panics if the daemon
    /// never responds so a wedged daemon surfaces here, not as a confusing
    /// downstream assertion.
    fn settle_search(agent: &mut AgentView) {
        for _ in 0..1000 {
            if agent.poll_scrollback_search() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("scrollback search daemon did not publish a result");
    }
    /// Type a query one keystroke at a time, settling after each so the daemon
    /// processes one query per bump. Settling between keystrokes keeps each
    /// `poll` aligned to a single keystroke (no cross-keystroke coalescing), so
    /// the final assertions see the result for the complete query.
    fn type_query_and_settle(agent: &mut AgentView, reg: &ActionRegistry, query: &str) {
        for c in query.chars() {
            press(agent, reg, KeyCode::Char(c));
            settle_search(agent);
        }
    }
    #[test]
    fn vim_slash_opens_scrollback_search() {
        let (mut agent, reg) = make_search_agent();
        assert!(agent.scrollback_search.is_none());
        let out = press(&mut agent, &reg, KeyCode::Char('/'));
        assert!(matches!(out, InputOutcome::Changed));
        let search = agent.scrollback_search.as_ref().expect("search opened");
        assert!(search.is_composing());
        assert_eq!(search.query(), "");
    }
    /// Vim `/` on an empty scrollback (new session) must focus the prompt and
    /// forward `/` like non-vim mode rather than open an empty search.
    #[test]
    fn vim_slash_empty_scrollback_focuses_prompt() {
        let (mut agent, reg) = make_empty_vim_agent();
        let out = press(&mut agent, &reg, KeyCode::Char('/'));
        assert!(
            matches!(out, InputOutcome::ActionThenForward(Action::FocusPrompt)),
            "vim `/` on empty scrollback must focus the prompt, got {out:?}"
        );
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn non_vim_slash_does_not_open_search() {
        let (mut agent, reg) = make_search_agent();
        agent.vim_mode = false;
        press(&mut agent, &reg, KeyCode::Char('/'));
        assert!(agent.scrollback_search.is_none());
    }
    /// Regression (user report): non-vim '/' from scrollback must
    /// focus-forward into the prompt like letters do.
    #[test]
    fn non_vim_slash_focuses_prompt_and_forwards() {
        let (mut agent, reg) = make_search_agent();
        agent.vim_mode = false;
        let out = press(&mut agent, &reg, KeyCode::Char('/'));
        assert!(
            matches!(out, InputOutcome::ActionThenForward(Action::FocusPrompt)),
            "'/' from scrollback (non-vim) must focus the prompt and forward, got {out:?}"
        );
        assert!(agent.scrollback_search.is_none());
    }
    /// '?' must bubble past the pane handler (agent-level palette alt key).
    #[test]
    fn non_vim_question_mark_still_bubbles_to_palette() {
        let (mut agent, reg) = make_search_agent();
        agent.vim_mode = false;
        let out = press(&mut agent, &reg, KeyCode::Char('?'));
        assert!(
            matches!(out, InputOutcome::Unchanged),
            "'?' must bubble (palette binding lives at agent level), got {out:?}"
        );
    }
    /// Drive the FULL input router with an unmodified `/` so the overlay
    /// intercepts ahead of `handle_scrollback_key` are exercised.
    fn route_slash(agent: &mut AgentView, reg: &ActionRegistry) -> InputOutcome {
        agent.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            reg,
        )
    }
    #[test]
    fn router_slash_opens_search_when_no_overlay_pending() {
        let (mut agent, reg) = make_search_agent();
        route_slash(&mut agent, &reg);
        assert!(agent.scrollback_search.is_some());
    }
    /// Router-level symmetry with the non-empty case: empty scrollback focuses the prompt, not search.
    #[test]
    fn router_slash_focuses_prompt_when_scrollback_empty() {
        let (mut agent, reg) = make_empty_vim_agent();
        let out = route_slash(&mut agent, &reg);
        assert!(
            matches!(out, InputOutcome::ActionThenForward(Action::FocusPrompt)),
            "router `/` on empty scrollback must focus the prompt, got {out:?}"
        );
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn router_slash_blocked_while_permission_pending() {
        let (mut agent, reg) = make_search_agent();
        agent
            .permission_queue
            .push_back(super::test_fixtures::make_followup_permission_state());
        route_slash(&mut agent, &reg);
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn router_slash_blocked_while_plan_approval_pending() {
        let (mut agent, reg) = make_search_agent();
        agent.plan_approval_view = Some(super::test_fixtures::make_plan_approval_view_state());
        route_slash(&mut agent, &reg);
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn router_slash_blocked_while_cancel_turn_pending() {
        let (mut agent, reg) = make_search_agent();
        agent.cancel_turn_view = Some(crate::views::modal::CancelTurnViewState {
            active_idx: 0,
            running_count: 1,
        });
        route_slash(&mut agent, &reg);
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn router_slash_blocked_while_question_pending() {
        let (mut agent, reg) = make_search_agent();
        agent.question_view =
            Some(super::paste_key_tests::make_question_view_state_in_input_mode());
        route_slash(&mut agent, &reg);
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn router_slash_blocked_while_btw_panel_open() {
        let (mut agent, reg) = make_search_agent();
        agent.btw_state = Some(crate::views::btw_overlay::BtwOverlayState::Loading {
            question: "q".into(),
        });
        route_slash(&mut agent, &reg);
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn router_slash_restarts_search_while_browsing() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "foo");
        press(&mut agent, &reg, KeyCode::Enter);
        assert!(!agent.scrollback_search.as_ref().unwrap().is_composing());
        route_slash(&mut agent, &reg);
        assert!(agent.scrollback_search.as_ref().unwrap().is_composing());
    }
    #[test]
    fn typing_builds_query_and_finds_matches() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "foo");
        let search = agent.scrollback_search.as_ref().unwrap();
        assert_eq!(search.query(), "foo");
        assert_eq!(search.match_count(), 2);
        assert_eq!(search.current_index(), Some(0));
    }
    #[test]
    fn backspace_edits_the_query() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "fox");
        assert_eq!(agent.scrollback_search.as_ref().unwrap().match_count(), 0);
        press(&mut agent, &reg, KeyCode::Backspace);
        settle_search(&mut agent);
        let search = agent.scrollback_search.as_ref().unwrap();
        assert_eq!(search.query(), "fo");
        assert_eq!(search.match_count(), 2);
    }
    #[test]
    fn enter_accepts_then_n_and_shift_n_navigate() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "foo");
        press(&mut agent, &reg, KeyCode::Enter);
        assert!(!agent.scrollback_search.as_ref().unwrap().is_composing());
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(0)
        );
        press(&mut agent, &reg, KeyCode::Char('n'));
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(1)
        );
        press(&mut agent, &reg, KeyCode::Char('n'));
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(0),
            "n wraps"
        );
        agent.handle_scrollback_key(
            &KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
            &reg,
        );
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(1)
        );
    }
    #[test]
    fn esc_cancels_search() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query(&mut agent, &reg, "foo");
        let out = press(&mut agent, &reg, KeyCode::Esc);
        assert!(matches!(out, InputOutcome::Changed));
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn arrow_keys_navigate_matches_while_browsing() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "foo");
        press(&mut agent, &reg, KeyCode::Enter);
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(0)
        );
        let out = press(&mut agent, &reg, KeyCode::Down);
        assert!(matches!(out, InputOutcome::Changed));
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(1),
            "Down advances to the next match"
        );
        press(&mut agent, &reg, KeyCode::Up);
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(0),
            "Up returns to the previous match"
        );
        assert!(!agent.scrollback_search.as_ref().unwrap().is_composing());
    }
    #[test]
    fn arrow_keys_navigate_matches_while_composing() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "foo");
        assert!(agent.scrollback_search.as_ref().unwrap().is_composing());
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(0)
        );
        press(&mut agent, &reg, KeyCode::Down);
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(1),
            "Down advances a match without leaving the composing phase"
        );
        assert!(
            agent.scrollback_search.as_ref().unwrap().is_composing(),
            "arrows must not accept the query"
        );
        press(&mut agent, &reg, KeyCode::Up);
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().current_index(),
            Some(0)
        );
    }
    #[test]
    fn open_scrollback_search_focuses_pane_and_starts_composing() {
        let (mut agent, _reg) = make_search_agent();
        agent.set_active_pane(AgentPane::Prompt, false);
        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert!(agent.scrollback_search.is_none());
        agent.open_scrollback_search(None);
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
        let search = agent.scrollback_search.as_ref().expect("search opened");
        assert!(search.is_composing());
        assert_eq!(search.query(), "");
    }
    #[test]
    fn open_scrollback_search_prefills_query_and_stays_composing() {
        use crate::scrollback::block::RenderBlock;
        let (mut agent, _reg) = make_search_agent();
        agent.scrollback.push_block(RenderBlock::user_prompt("abc"));
        agent.scrollback.prepare_layout(80, 24);
        agent.set_active_pane(AgentPane::Prompt, false);
        agent.open_scrollback_search(Some("a.c"));
        settle_search(&mut agent);
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
        let search = agent.scrollback_search.as_ref().expect("search opened");
        assert_eq!(search.query(), "a.c");
        assert_eq!(
            search.match_count(),
            1,
            "regex `a.c` must match `abc`; a literal search would find nothing"
        );
        assert_eq!(search.current_index(), Some(0));
        assert!(
            search.is_composing(),
            "a pre-filled query must mirror typing and not auto-accept"
        );
    }
    #[test]
    fn open_scrollback_search_blocked_by_dirty_edit_does_not_orphan_session() {
        let (mut agent, _reg) = make_search_agent();
        agent.set_active_pane(AgentPane::Prompt, false);
        agent.prompt_mode = PromptMode::EditingQueued {
            id: 0,
            original: "queued text".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        assert!(
            agent.prompt.text() != "queued text",
            "empty prompt must read dirty against the snapshot"
        );
        agent.open_scrollback_search(Some("foo"));
        assert!(
            agent.scrollback_search.is_none(),
            "blocked pane switch must not open a search session"
        );
        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert!(
            agent.active_modal.is_none(),
            "the blocked switch must not arm an invisible EditConfirm modal"
        );
    }
    /// Render `agent` into a fresh buffer of `area` and return it. Centralizes
    /// the `draw` boilerplate (including the dev-only tracing arg) so render
    /// tests stay readable.
    fn render_agent(agent: &mut AgentView, area: Rect, reg: &ActionRegistry) -> Buffer {
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        let bundle = crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };
        agent.draw(
            area,
            &mut buf,
            reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams::none(),
            &bundle,
            false,
            false,
            &mut Vec::new(),
            crate::app::agent_view::AppRenderParams::default(),
        );
        buf
    }
    /// Concatenated symbols of buffer row `y` across `width` columns.
    fn buffer_row(buf: &Buffer, width: u16, y: u16) -> String {
        (0..width)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect()
    }
    /// Draw-path integration: an active ephemeral tip paints into the banner
    /// row on a tall, unoccluded terminal; the draw-path height gate suppresses
    /// it when the area is too short; and a co-occurring mode-switch banner
    /// wins the slot while the tip merely yields (its TTL keeps ticking).
    #[test]
    fn ephemeral_tip_draw_path_reserve_paint_and_precedence() {
        use std::collections::HashMap;
        let reg = ActionRegistry::defaults();
        let tall = Rect::new(0, 0, 80, 30);
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let _ = agent.show_ephemeral_tip(
            crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("ZZTIPZZ")),
            &mut HashMap::new(),
        );
        assert!(
            agent.ephemeral_tip.is_active(),
            "tall, unoccluded show must put the tip on screen"
        );
        let buf = render_agent(&mut agent, tall, &reg);
        assert!(
            (0..tall.height).any(|y| buffer_row(&buf, tall.width, y).contains("ZZTIPZZ")),
            "tip text must paint into the banner row"
        );
        let short = Rect::new(0, 0, 80, 16);
        let buf = render_agent(&mut agent, short, &reg);
        assert!(
            !(0..short.height).any(|y| buffer_row(&buf, short.width, y).contains("ZZTIPZZ")),
            "short terminal must not reserve or paint the tip row"
        );
        agent.show_mode_switch_banner("PlanMode");
        let buf = render_agent(&mut agent, tall, &reg);
        let frame: String = (0..tall.height)
            .map(|y| buffer_row(&buf, tall.width, y))
            .collect();
        assert!(
            frame.contains("Switched to mode: PlanMode"),
            "mode-switch banner must own the slot"
        );
        assert!(
            !frame.contains("ZZTIPZZ"),
            "tip yields the slot while the mode-switch banner is active"
        );
        assert!(
            agent.ephemeral_tip.is_active(),
            "tip is only hidden by precedence, not cleared"
        );
    }
    /// Regression (#send-now bold leak): an ephemeral tip that reserved the
    /// banner row must render with its own styling even when a session tip is
    /// (wrongly or historically) handed to `draw` at the same time. The
    /// session tip's bold `Tip: ` prefix used to underpaint the row and —
    /// because `Cell::set_style` merges modifiers — leak BOLD into the first
    /// five cells of the ephemeral tip ("**Queue**d · Enter to send now").
    #[test]
    fn ephemeral_tip_not_bolded_by_session_tip_underpaint() {
        use ratatui::style::Modifier;
        use std::collections::HashMap;
        let reg = ActionRegistry::defaults();
        let tall = Rect::new(0, 0, 80, 30);
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let _ =
            agent.show_ephemeral_tip(crate::tips::send_now::send_now_tip(), &mut HashMap::new());
        assert!(agent.ephemeral_tip.is_active());
        let mut buf = Buffer::empty(tall);
        let mut scratch = ScratchBuffer::new();
        let bundle = crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };
        agent.draw(
            tall,
            &mut buf,
            &reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams {
                tip: Some("ZZSESSIONTIPZZ never shown in agent view"),
                ..crate::app::agent_view::BannerSlotParams::none()
            },
            &bundle,
            false,
            false,
            &mut Vec::new(),
            crate::app::agent_view::AppRenderParams::default(),
        );
        let tip_y = (0..tall.height)
            .find(|&y| buffer_row(&buf, tall.width, y).contains("Queued"))
            .expect("ephemeral tip must paint into the banner row");
        let row = buffer_row(&buf, tall.width, tip_y);
        assert!(
            !(0..tall.height).any(|y| buffer_row(&buf, tall.width, y).contains("ZZSESSIONTIPZZ")),
            "session tip must not remain visible in the agent view"
        );
        let start = row[..row.find("Queued").expect("tip text")].chars().count() as u16;
        let bold_cols: Vec<u16> = (0..tall.width)
            .filter(|&x| {
                buf.cell((x, tip_y))
                    .expect("cell in row")
                    .modifier
                    .contains(Modifier::BOLD)
            })
            .collect();
        assert_eq!(
            bold_cols,
            (start + 9..start + 14).collect::<Vec<u16>>(),
            "only the Enter chord may be bold, got row {row:?}"
        );
    }
    /// Critical banner yields over an active ephemeral tip and occludes new shows.
    #[test]
    fn critical_banner_draw_path_yields_and_occludes_ephemeral_tip() {
        use std::collections::HashMap;
        let reg = ActionRegistry::defaults();
        let tall = Rect::new(0, 0, 80, 30);
        let mut agent = make_agent();
        agent.last_terminal_size = (80, 30);
        let _ = agent.ephemeral_tip.show(
            crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("ZZTIPZZ")),
            &mut HashMap::new(),
        );
        assert!(agent.ephemeral_tip.is_active());
        let critical = [echo_agent_announcements::RemoteAnnouncement {
            severity: Some("critical".into()),
            message: Some("ZZCRITZZ outage".into()),
            ..Default::default()
        }];
        let long_tip = format!("LONGTIPWRAP {}", "word ".repeat(40).trim_end());
        assert!(
            crate::tips::render::tip_height(80, &long_tip) > 2,
            "fixture tip must wrap taller than critical banner height"
        );
        let mut buf = Buffer::empty(tall);
        let mut scratch = ScratchBuffer::new();
        let bundle = crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };
        agent.draw(
            tall,
            &mut buf,
            &reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams {
                height: 2,
                announcements: &critical,
                hidden_ids: &std::collections::BTreeSet::new(),
                privacy_banner: false,
                mouse_pos: None,
                tip: Some(long_tip.as_str()),
            },
            &bundle,
            false,
            false,
            &mut Vec::new(),
            crate::app::agent_view::AppRenderParams::default(),
        );
        let frame: String = (0..tall.height)
            .map(|y| buffer_row(&buf, tall.width, y))
            .collect();
        assert!(
            frame.contains("ZZCRITZZ"),
            "critical announcement must paint into the banner row"
        );
        assert!(
            !frame.contains("ZZTIPZZ"),
            "ephemeral tip must yield while critical owns the slot"
        );
        assert!(
            !frame.contains("LONGTIPWRAP"),
            "session tip must not paint under critical"
        );
        assert!(
            agent.ephemeral_tip.is_active(),
            "pre-existing tip stays active (yield, not clear)"
        );
        assert!(
            agent.session_banner_active,
            "draw must set the session-banner occluder flag"
        );
        assert!(
            !agent.show_ephemeral_tip(
                crate::tips::EphemeralTip::new("t2", ratatui::text::Line::from("NEWTIP")),
                &mut HashMap::new(),
            ),
            "show_ephemeral_tip must refuse while critical banner occludes"
        );
    }
    #[test]
    fn search_active_reserves_two_bottom_rows() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query(&mut agent, &reg, "foo");
        let area = Rect::new(0, 0, 80, 24);
        let buf = render_agent(&mut agent, area, &reg);
        let bar_y = (0..area.height)
            .find(|&y| buffer_row(&buf, area.width, y).contains("search:"))
            .expect("the search bar should render in a reserved row");
        assert!(bar_y >= 1, "the search bar can't sit on the top row");
        assert!(
            buffer_row(&buf, area.width, bar_y - 1).contains('\u{2500}'),
            "a divider rule should occupy the row directly above the search bar"
        );
    }
    #[test]
    fn search_reserved_rows_clamps_to_available_height() {
        assert_eq!(AgentView::search_reserved_rows(24, false), 0);
        assert_eq!(AgentView::search_reserved_rows(24, true), 2);
        assert_eq!(AgentView::search_reserved_rows(2, true), 2);
        assert_eq!(AgentView::search_reserved_rows(1, true), 1);
        assert_eq!(AgentView::search_reserved_rows(0, true), 0);
    }
    #[test]
    fn browsing_leaves_non_search_keys_to_normal_handling() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query(&mut agent, &reg, "foo");
        press(&mut agent, &reg, KeyCode::Enter);
        press(&mut agent, &reg, KeyCode::Char('j'));
        assert!(agent.scrollback_search.is_some());
        assert!(!agent.scrollback_search.as_ref().unwrap().is_composing());
    }
    #[test]
    fn search_is_smart_case() {
        use crate::scrollback::block::RenderBlock;
        let mut agent = make_agent();
        agent.vim_mode = true;
        setup_scrollback_area(&mut agent, Rect::new(0, 0, 80, 24));
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("Error and error"));
        agent.scrollback.prepare_layout(80, 24);
        let reg = ActionRegistry::defaults();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "error");
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().match_count(),
            2,
            "a lowercase query is case-insensitive"
        );
        press(&mut agent, &reg, KeyCode::Esc);
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "Error");
        assert_eq!(
            agent.scrollback_search.as_ref().unwrap().match_count(),
            1,
            "an uppercase query is case-sensitive (smart-case)"
        );
    }
    #[test]
    fn enter_on_empty_query_closes_search() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        let out = press(&mut agent, &reg, KeyCode::Enter);
        assert!(matches!(out, InputOutcome::Changed));
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn esc_dismisses_search_while_browsing() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query(&mut agent, &reg, "foo");
        press(&mut agent, &reg, KeyCode::Enter);
        let out = press(&mut agent, &reg, KeyCode::Esc);
        assert!(matches!(out, InputOutcome::Changed));
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn navigate_with_no_matches_is_noop() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query_and_settle(&mut agent, &reg, "zzz");
        press(&mut agent, &reg, KeyCode::Enter);
        assert_eq!(agent.scrollback_search.as_ref().unwrap().match_count(), 0);
        press(&mut agent, &reg, KeyCode::Char('n'));
        let search = agent.scrollback_search.as_ref().unwrap();
        assert_eq!(search.current_index(), None);
        assert!(agent.scrollback_search.is_some());
    }
    #[test]
    fn composing_swallows_tab_without_changing_pane() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        let out = press(&mut agent, &reg, KeyCode::Tab);
        assert!(matches!(out, InputOutcome::Unchanged));
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
        assert!(agent.scrollback_search.as_ref().unwrap().is_composing());
    }
    #[test]
    fn leaving_scrollback_pane_clears_search() {
        let (mut agent, reg) = make_search_agent();
        press(&mut agent, &reg, KeyCode::Char('/'));
        assert!(agent.scrollback_search.is_some());
        agent.set_active_pane(AgentPane::Prompt, false);
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn esc_with_running_turn_dismisses_search_not_cancel_turn() {
        let (mut agent, reg) = make_search_agent();
        agent.session.state = AgentState::TurnRunning;
        press(&mut agent, &reg, KeyCode::Char('/'));
        type_query(&mut agent, &reg, "foo");
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let out = agent.handle_input(&esc, &reg);
        assert!(!matches!(out, InputOutcome::Action(Action::CancelTurn)));
        assert!(agent.scrollback_search.is_none());
    }
    #[test]
    fn is_bare_scrollback_excludes_open_search() {
        let (mut agent, reg) = make_search_agent();
        assert!(
            agent.is_bare_scrollback(),
            "scrollback focused with nothing layered on top"
        );
        press(&mut agent, &reg, KeyCode::Char('/'));
        assert!(
            !agent.is_bare_scrollback(),
            "an open scrollback search is a layered sub-state"
        );
    }
    #[test]
    fn subagent_esc_cancels_open_search_before_closing_view() {
        let reg = ActionRegistry::defaults();
        let mut parent = make_agent();
        let (mut child, _) = make_search_agent();
        press(&mut child, &reg, KeyCode::Char('/'));
        type_query(&mut child, &reg, "foo");
        assert!(child.scrollback_search.is_some());
        let child_sid = "child-sid".to_string();
        parent
            .subagent_views
            .insert(child_sid.clone(), Box::new(child));
        parent.active_subagent = Some(child_sid.clone());
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        parent.handle_input(&esc, &reg);
        assert!(
            parent.active_subagent.is_some(),
            "view stays open while the child's search is cancelled"
        );
        assert!(
            parent.subagent_views[&child_sid]
                .scrollback_search
                .is_none(),
            "the forwarded Esc cancels the child's search"
        );
        parent.handle_input(&esc, &reg);
        assert!(
            parent.active_subagent.is_none(),
            "Esc closes the subagent view once no search is open"
        );
    }
    #[test]
    fn subagent_q_while_searching_types_into_query_not_close() {
        let reg = ActionRegistry::defaults();
        let mut parent = make_agent();
        let (mut child, _) = make_search_agent();
        press(&mut child, &reg, KeyCode::Char('/'));
        type_query(&mut child, &reg, "fo");
        assert!(child.scrollback_search.as_ref().unwrap().is_composing());
        let child_sid = "child-sid".to_string();
        parent
            .subagent_views
            .insert(child_sid.clone(), Box::new(child));
        parent.active_subagent = Some(child_sid.clone());
        let q = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        parent.handle_input(&q, &reg);
        assert!(
            parent.active_subagent.is_some(),
            "view stays open while a search is composing"
        );
        assert_eq!(
            parent.subagent_views[&child_sid]
                .scrollback_search
                .as_ref()
                .unwrap()
                .query(),
            "foq",
            "q is typed into the query, not treated as a close key"
        );
    }
    #[test]
    fn subagent_scrollback_search_delivers_results_via_child_poll() {
        let reg = ActionRegistry::defaults();
        let mut parent = make_agent();
        let (mut child, _) = make_search_agent();
        press(&mut child, &reg, KeyCode::Char('/'));
        assert!(child.scrollback_search.is_some());
        let child_sid = "child-sid".to_string();
        parent
            .subagent_views
            .insert(child_sid.clone(), Box::new(child));
        parent.active_subagent = Some(child_sid.clone());
        for c in "foo".chars() {
            parent.handle_input(
                &Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                &reg,
            );
            let mut delivered = false;
            for _ in 0..1000 {
                if parent
                    .subagent_views
                    .get_mut(&child_sid)
                    .unwrap()
                    .poll_scrollback_search()
                {
                    delivered = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(delivered, "child search daemon did not publish a result");
        }
        let search = parent.subagent_views[&child_sid]
            .scrollback_search
            .as_ref()
            .unwrap();
        assert_eq!(
            search.match_count(),
            2,
            "child search finds both 'foo' entries"
        );
        assert_eq!(
            search.current_index(),
            Some(0),
            "child search parks the cursor on the first match"
        );
    }
    #[test]
    fn o_key_cycles_links_via_registry() {
        let registry = ActionRegistry::defaults();
        let o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(
            registry.lookup(&o, When::ScrollbackFocused),
            Some(ActionId::OpenNextLink)
        );
    }
    #[test]
    fn shift_o_key_cycles_links_backward_via_registry() {
        let registry = ActionRegistry::defaults();
        let shift_o = KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT);
        assert_eq!(
            registry.lookup(&shift_o, When::ScrollbackFocused),
            Some(ActionId::OpenPrevLink)
        );
    }
    #[test]
    fn navigation_clears_highlighted_link_via_dispatch() {
        use crate::app::actions::Action;
        use crate::app::app_view::AppView;
        use crate::app::dispatch::{SwitchCause, dispatch, switch_to_agent};
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = AppView::new(tx.clone(), ModelState::default(), Vec::new());
        let id = AgentId(0);
        let mut agent = make_agent();
        add_multiple_links(&mut agent);
        agent.highlighted_link_idx = Some(1);
        app.agents.insert(id, agent);
        switch_to_agent(&mut app, id, SwitchCause::New);
        dispatch(Action::SelectNext, &mut app);
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(
            agent.highlighted_link_idx, None,
            "navigation should clear highlighted link"
        );
    }
}
