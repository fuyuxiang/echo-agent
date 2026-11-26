//! Scrollback text/block selection: click counting, word/paragraph select,
//! drag latches, drag autoscroll ticks, and selection-highlight timers.

use super::{
    AgentPane, AgentView, DEFAULT_SELECTION_HIGHLIGHT_DURATION_MS, MULTI_CLICK_TIMEOUT_MS,
};
use crate::app::app_view::InputOutcome;
use crate::scrollback::table_geometry::{CellRef, TableGeometry};
use crate::scrollback::text_selection::{
    ActiveBlockDrag, ActiveTextDrag, AutoScrollDirection, DragAutoScrollState, PendingBlockDrag,
    PendingTextDrag, PersistentTextSelection, RangeHit, ResolvedSelectionModel, SelectionEndpoint,
    SelectionKind, SelectionOrigin, TableSelectionGeometry, apply_selection_boundary,
    block_drag_threshold_exceeded, compute_autoscroll, configured_word_separators,
    drag_threshold_exceeded, reconstruct_full_selection_text_with_boundaries,
    reconstruct_selection_text, reconstruct_selection_text_with_boundaries,
    reconstruct_table_selection_text, resolve_table_drag_kind, semantic_selection_at,
};
use crate::views::btw_overlay::BTW_OVERLAY_ENTRY_IDX;
use crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

/// Two fold/nav double-clicks on assistant text within this window count as a
/// repeated selection attempt and fire the word-select tip. Must be well over
/// [`MULTI_CLICK_TIMEOUT_MS`] so it measures separate gestures, and short
/// enough that the second gesture plausibly continues the first intent.
const WORD_SELECT_REPEAT_WINDOW: Duration = Duration::from_secs(10);

impl AgentView {
    /// Tick the selection highlight timer. Returns true if the selection
    /// was auto-dismissed (needs redraw). When `keep_text_selection` is on
    /// (cache), never timer-dismisses — Esc / click / nav still clear it.
    pub fn tick_selection_highlight(&mut self) -> bool {
        if crate::appearance::cache::load_keep_text_selection().holds() {
            return false;
        }
        if let Some(created) = self.selection_created_at
            && created.elapsed().as_millis() as u64 >= DEFAULT_SELECTION_HIGHLIGHT_DURATION_MS
        {
            self.persistent_text_selection = None;
            self.table_selection_geometry = None;
            self.selection_created_at = None;
            return true;
        }
        false
    }

    /// Side-car table geometry, if it was resolved for the given selection.
    pub(in crate::app) fn table_geometry_for_selection(
        &self,
        entry_idx: usize,
        range_id: u16,
    ) -> Option<&TableGeometry> {
        self.table_selection_geometry
            .as_ref()
            .and_then(|t| t.for_selection(entry_idx, range_id))
    }

    /// Run `f` with a text source over the anchor entry's full block output
    /// (so off-screen fragments are included). Lines outside the hit's
    /// selection range yield `None`, which detection treats as a boundary.
    ///
    /// `width_override` is the drag-start width snapshot, for callers whose
    /// entry may have scrolled fully out of `visible_blocks` by now; other
    /// callers pass `None` and use the current frame's geometry.
    fn with_entry_output_text_source<R>(
        &self,
        entry_idx: usize,
        range_id: u16,
        width_override: Option<u16>,
        f: impl FnOnce(&dyn Fn(usize) -> Option<String>) -> R,
    ) -> Option<R> {
        let scrollback = if let Some(ref child_id) = self.active_subagent
            && let Some(child) = self.subagent_views.get(child_id)
        {
            &child.scrollback
        } else {
            &self.scrollback
        };
        let visible_start = scrollback.visible_entry_range().start;
        let abs_idx = entry_idx + visible_start;
        // The per-block content width the geometry/hits were captured
        // against; see the width note in `reconstruct_drag_copy`.
        let content_width = width_override.or_else(|| {
            self.last_scrollback_selection_model
                .visible_block_content_width(entry_idx)
        })?;
        let entry = scrollback.get(abs_idx)?;
        let appearance = scrollback.appearance();
        let effective = entry.effective_output(content_width, appearance, false, scrollback.cwd());
        let lines = &effective.output().lines;
        let source = |i: usize| -> Option<String> {
            let line = lines.get(i)?;
            if line.selection_range != Some(range_id) {
                return None;
            }
            Some(crate::scrollback::types::derive_selection_text(line))
        };
        Some(f(&source))
    }

    /// Detect the table grid under a drag anchor (btw drags stay linear).
    /// Pre-gated on the anchor line having a box-drawing glyph: prose drags
    /// must not touch `effective_output`, which thrashes the render cache.
    fn compute_drag_table_geometry(&self, anchor: &RangeHit) -> Option<TableGeometry> {
        if anchor.entry_idx == BTW_OVERLAY_ENTRY_IDX {
            return None;
        }
        if let Some(line) = self.last_scrollback_selection_model.line_for_hit(anchor)
            && !line.text.contains(['│', '┌', '├', '└'])
        {
            return None;
        }
        self.with_entry_output_text_source(anchor.entry_idx, anchor.range_id, None, |src| {
            TableGeometry::detect(src, anchor.block_line_idx)
        })
        .flatten()
    }

    /// Re-resolve the drag's [`SelectionKind`] after an endpoint change;
    /// `prev` is the latch that keeps boundary touches from flipping it.
    fn resolve_drag_kind(
        &self,
        anchor: &RangeHit,
        head: &RangeHit,
        prev: SelectionKind,
    ) -> SelectionKind {
        let geom = self.table_geometry_for_selection(anchor.entry_idx, anchor.range_id);
        resolve_table_drag_kind(geom, anchor, head, prev)
    }

    /// Whether drag autoscroll is currently active and needs ticking.
    pub fn has_drag_autoscroll(&self) -> bool {
        self.drag_autoscroll.is_some()
    }

    /// Advance drag autoscroll by one tick.
    ///
    /// Scrolls the active scrollback (main or subagent) by `speed` rows in
    /// the autoscroll direction. After scrolling, recomputes the drag head
    /// from the stored mouse position using the current (soon-stale)
    /// selection model; [`Self::reclamp_drag_head_post_render`] re-snaps the
    /// head once the next render rebuilds the model.
    ///
    /// For block drag, implements long-block snap: if the current head block
    /// is no longer visible after scrolling, advances the head to the next
    /// visible block in the scroll direction.
    pub fn tick_drag_autoscroll(&mut self) -> bool {
        let Some(autoscroll) = self.drag_autoscroll else {
            return false;
        };

        // Scroll the active scrollback.
        let scrollback = if let Some(ref child_id) = self.active_subagent {
            if let Some(child) = self.subagent_views.get_mut(child_id) {
                &mut child.scrollback
            } else {
                &mut self.scrollback
            }
        } else {
            &mut self.scrollback
        };

        match autoscroll.direction {
            AutoScrollDirection::Up => scrollback.scroll_up(autoscroll.speed),
            AutoScrollDirection::Down => scrollback.scroll_down(autoscroll.speed),
        }

        // Recompute text drag head from stored mouse position, against the
        // pre-scroll (stale) model: a within-tick estimate that keeps the
        // overlay responsive; the post-render reclamp re-resolves it against
        // the fresh model every frame.
        // Skip for btw-anchored drags — they don't autoscroll the scrollback.
        if let Some((col, row)) = self.last_drag_mouse
            && let Some(drag) = self.drag_selection
            && drag.anchor.entry_idx != BTW_OVERLAY_ENTRY_IDX
        {
            let head = self
                .last_scrollback_selection_model
                .hit_test_nearest_in_range(drag.anchor, col, row)
                .unwrap_or(drag.head);
            let kind = self.resolve_drag_kind(&drag.anchor, &head, drag.kind);
            if let Some(ref mut drag) = self.drag_selection {
                drag.head = head;
                drag.kind = kind;
            }
        }

        // Long-block snap for block drag: if the current head block scrolled
        // out of the visible set, advance to the next/previous visible block.
        if let Some(ref mut drag) = self.block_drag_selection {
            let head_visible = self
                .last_scrollback_selection_model
                .visible_blocks
                .iter()
                .any(|b| b.entry_idx == drag.head_entry_idx);

            if !head_visible {
                match autoscroll.direction {
                    AutoScrollDirection::Down => {
                        if let Some(next) = self
                            .last_scrollback_selection_model
                            .visible_blocks
                            .iter()
                            .find(|b| b.entry_idx > drag.head_entry_idx)
                        {
                            drag.head_entry_idx = next.entry_idx;
                        }
                    }
                    AutoScrollDirection::Up => {
                        if let Some(prev) = self
                            .last_scrollback_selection_model
                            .visible_blocks
                            .iter()
                            .rev()
                            .find(|b| b.entry_idx < drag.head_entry_idx)
                        {
                            drag.head_entry_idx = prev.entry_idx;
                        }
                    }
                }
            }
        }

        true
    }

    /// Snap an active drag head to the freshly rebuilt selection model.
    ///
    /// Input handlers and autoscroll ticks hit-test against the previous
    /// frame's model, which scrolling/streaming/resizing make stale.
    /// `draw` calls this right after each model rebuild and before the
    /// corresponding overlay paints, so the head re-resolves from the held
    /// pointer position with at most one frame of lag. `btw_rebuilt` names
    /// the surface that was just rebuilt: a drag anchored on the other
    /// surface is left alone (its model is still last frame's). Keeps the
    /// previous head when the anchor's range has no lines in the fresh model.
    pub(in crate::app) fn reclamp_drag_head_post_render(&mut self, btw_rebuilt: bool) {
        let Some((col, row)) = self.last_drag_mouse else {
            return;
        };
        let Some(drag) = self.drag_selection else {
            return;
        };
        if (drag.anchor.entry_idx == BTW_OVERLAY_ENTRY_IDX) != btw_rebuilt {
            return;
        }
        let model = self.selection_model_for_hit(&drag.anchor);
        let Some(head) = model.hit_test_nearest_in_range(drag.anchor, col, row) else {
            return;
        };
        if head == drag.head {
            return;
        }
        let kind = self.resolve_drag_kind(&drag.anchor, &head, drag.kind);
        if let Some(ref mut drag) = self.drag_selection {
            drag.head = head;
            drag.kind = kind;
        }
    }

    /// True when any scrollback drag/press latch is set (incl. a bare left press or scrollbar drag).
    /// [`Self::clear_stuck_scrollback_drag`] also resets the derived autoscroll / last-drag-mouse state.
    pub(super) fn scrollback_drag_latched(&self) -> bool {
        self.left_mouse_down
            || self.scrollbar_dragging
            || self.pending_text_drag.is_some()
            || self.drag_selection.is_some()
            || self.pending_block_drag.is_some()
            || self.block_drag_selection.is_some()
            || self.deferred_text_press.is_some()
    }

    /// Discard any in-progress scrollback drag + mouse-down latch (recovery path).
    pub(super) fn clear_stuck_scrollback_drag(&mut self) {
        self.left_mouse_down = false;
        self.scrollbar_dragging = false;
        self.pending_text_drag = None;
        self.drag_selection = None;
        self.pending_block_drag = None;
        self.block_drag_selection = None;
        self.deferred_text_press = None;
        self.drag_autoscroll = None;
        self.last_drag_mouse = None;
    }

    /// Finish a latched gesture whose `Up(Left)` was lost, as that release
    /// would have: an active text/block drag delivers its copy (unlike
    /// [`Self::clear_stuck_scrollback_drag`], which discards the gesture).
    /// A sub-threshold press just drops its latches: a synthesized release
    /// must not fabricate a click or leave click/link arms dangling.
    pub(super) fn finish_stuck_drag_as_lost_up(&mut self) {
        self.left_mouse_down = false;
        self.scrollbar_dragging = false;
        self.deferred_text_press = None;
        self.pending_scrollback_click = None;
        self.pending_link_click = None;
        if self.drag_selection.is_some() {
            self.finish_text_drag();
        } else if self.block_drag_selection.is_some() {
            self.finish_block_drag();
        }
        self.pending_text_drag = None;
        self.pending_block_drag = None;
        self.drag_autoscroll = None;
        self.last_drag_mouse = None;
    }

    /// On xterm.js embeds a lost release can also mean the terminal's own
    /// button tracker is wedged and will eat every release from now on
    /// (VS Code after a context-menu gesture). Toggling reporting off and on
    /// resets the tracker so the next gesture gets clean reports. Callers
    /// must know the button is UP (bare `Moved`, an unpaired release): the
    /// toggle clears xterm.js's tracking of a press in flight, so firing it
    /// mid-press would break that gesture. Gated to xterm.js embeds: other
    /// terminals don't have the wedge, and some (VTE) emit spurious events
    /// on mouse-mode churn.
    pub(super) fn reset_wedged_mouse_reporting(&self) {
        if crate::terminal::terminal_context().brand.is_xtermjs_embed()
            && crate::app::MOUSE_CAPTURE_ENABLED.load(std::sync::atomic::Ordering::Acquire)
        {
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                let _ = crossterm::execute!(
                    stderr,
                    crossterm::event::DisableMouseCapture,
                    crossterm::event::EnableMouseCapture
                );
            });
        }
    }

    /// Update [`Self::plan_prompt_mouse_drag`] for a left-button mouse event
    /// during plan feedback and report whether the event should be forwarded
    /// to the feedback prompt (for cursor placement / text selection).
    ///
    /// `in_prompt` is whether the pointer currently sits in the prompt rect.
    /// A left press arms the drag when it lands in the prompt; subsequent
    /// `Drag`/`Up` events keep being routed to the prompt even after the
    /// pointer leaves the rect (TextArea tracks drag state internally and
    /// handles drag-beyond-edge), and the release disarms it. Shared by both
    /// plan-feedback mouse paths (line-viewer-open and empty-plan) so the
    /// drag semantics stay in sync.
    pub(super) fn route_plan_prompt_mouse_drag(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
        in_prompt: bool,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        let was_dragging = self.plan_prompt_mouse_drag;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.plan_prompt_mouse_drag = in_prompt;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.plan_prompt_mouse_drag = false;
            }
            _ => {}
        }
        let continue_drag = was_dragging
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            );
        in_prompt || continue_drag
    }

    pub(in crate::app) fn begin_pending_text_drag(&mut self, mouse: &MouseEvent) -> bool {
        self.begin_pending_text_drag_on(mouse, false)
    }

    pub(in crate::app) fn begin_pending_btw_text_drag(&mut self, mouse: &MouseEvent) -> bool {
        self.begin_pending_text_drag_on(mouse, true)
    }

    fn begin_pending_text_drag_on(&mut self, mouse: &MouseEvent, btw: bool) -> bool {
        let model = if btw {
            &self.last_btw_selection_model
        } else {
            &self.last_scrollback_selection_model
        };
        let hit = model.hit_test_selectable_range(mouse.column, mouse.row);
        tracing::debug!(
            event = "begin_pending_text_drag",
            col = mouse.column,
            row = mouse.row,
            btw,
            hit = ?hit,
            "drag start hit-test"
        );
        let Some(hit) = hit else {
            return false;
        };
        // Snapshot the anchor block's render width now (the btw model carries
        // its own geometry): copy must survive the block scrolling fully out
        // of `visible_blocks` before mouse-up.
        let anchor_content_width = model.visible_block_content_width(hit.entry_idx);
        self.pending_text_drag = Some(PendingTextDrag {
            anchor: hit,
            start_col: mouse.column,
            start_row: mouse.row,
            anchor_content_width,
        });
        self.drag_selection = None;
        true
    }

    fn drag_model(&self) -> &ResolvedSelectionModel {
        let is_btw = self
            .pending_text_drag
            .is_some_and(|p| p.anchor.entry_idx == BTW_OVERLAY_ENTRY_IDX)
            || self
                .drag_selection
                .as_ref()
                .is_some_and(|d| d.anchor.entry_idx == BTW_OVERLAY_ENTRY_IDX);
        if is_btw {
            &self.last_btw_selection_model
        } else {
            &self.last_scrollback_selection_model
        }
    }

    fn update_text_drag(&mut self, mouse: &MouseEvent) -> bool {
        let Some(pending) = self.pending_text_drag else {
            return false;
        };
        let threshold = drag_threshold_exceeded(&pending, mouse.column, mouse.row);
        tracing::debug!(
            event = "update_text_drag",
            col = mouse.column,
            row = mouse.row,
            anchor = ?pending.anchor,
            threshold,
            "pending drag update"
        );
        if !threshold {
            return false;
        }
        let model = self.drag_model();
        let head = model
            .hit_test_nearest_in_range(pending.anchor, mouse.column, mouse.row)
            .unwrap_or(pending.anchor);
        tracing::debug!(
            event = "promote_text_drag",
            head = ?head,
            "pending drag promoted"
        );
        self.arm_text_drag(mouse, pending.anchor, head, pending.anchor_content_width);
        true
    }

    /// Shared arming tail for every text drag — threshold promotion
    /// ([`Self::update_text_drag`]) and deferred conversion
    /// ([`Self::convert_deferred_text_press`]) both end here so their
    /// bookkeeping cannot drift. Geometry resolves once per drag, at arming;
    /// btw mouse-downs don't clear a held persistent selection, so a btw
    /// drag must not wipe its side-car.
    fn arm_text_drag(
        &mut self,
        mouse: &MouseEvent,
        anchor: RangeHit,
        head: RangeHit,
        anchor_content_width: Option<u16>,
    ) {
        if anchor.entry_idx != BTW_OVERLAY_ENTRY_IDX {
            self.table_selection_geometry =
                self.compute_drag_table_geometry(&anchor)
                    .map(|geometry| TableSelectionGeometry {
                        entry_idx: anchor.entry_idx,
                        range_id: anchor.range_id,
                        geometry,
                    });
        }
        let kind = self.resolve_drag_kind(&anchor, &head, SelectionKind::Linear);
        self.drag_selection = Some(ActiveTextDrag {
            anchor,
            head,
            kind,
            anchor_content_width,
        });
        // Store the pointer at arming too, or an arm-then-hold-still drag
        // would stay invisible to the post-render reclamp while content
        // scrolls or streams underneath it.
        self.last_drag_mouse = Some((mouse.column, mouse.row));
    }

    /// One-way deferred-anchor conversion. A scrollback press with no
    /// selectable text under it arms [`AgentView::deferred_text_press`]
    /// alongside the normal block-drag latch; the FIRST drag position that
    /// hits selectable text (the same hit test the press ran) becomes both
    /// anchor and head of an [`ActiveTextDrag`], and any block-drag state in
    /// flight is cancelled — once text, the gesture stays text. A gesture
    /// that never enters text leaves the block drag to run unchanged.
    fn convert_deferred_text_press(&mut self, mouse: &MouseEvent) -> bool {
        let Some((press_col, press_row)) = self.deferred_text_press else {
            return false;
        };
        let Some(hit) = self
            .last_scrollback_selection_model
            .hit_test_selectable_range(mouse.column, mouse.row)
        else {
            return false;
        };
        tracing::debug!(
            event = "convert_deferred_text_press",
            press_col,
            press_row,
            col = mouse.column,
            row = mouse.row,
            hit = ?hit,
            "deferred press entered text"
        );
        self.deferred_text_press = None;
        self.pending_block_drag = None;
        self.block_drag_selection = None;
        // The width snapshot is taken at anchor placement (here, not at the
        // press): copy must survive the block scrolling out before mouse-up.
        let anchor_content_width = self
            .last_scrollback_selection_model
            .visible_block_content_width(hit.entry_idx);
        self.arm_text_drag(mouse, hit, hit, anchor_content_width);
        true
    }

    fn update_active_text_drag(&mut self, mouse: &MouseEvent) -> bool {
        let Some(drag) = self.drag_selection else {
            return false;
        };
        let is_btw = drag.anchor.entry_idx == BTW_OVERLAY_ENTRY_IDX;
        let head = self
            .selection_model_for_hit(&drag.anchor)
            .hit_test_nearest_in_range(drag.anchor, mouse.column, mouse.row)
            .unwrap_or(drag.head);
        let kind = self.resolve_drag_kind(&drag.anchor, &head, drag.kind);
        let changed = head != drag.head || kind != drag.kind;
        if let Some(ref mut drag) = self.drag_selection {
            drag.head = head;
            drag.kind = kind;
        }
        self.last_drag_mouse = Some((mouse.column, mouse.row));
        // Btw drags don't scroll the scrollback pane.
        self.drag_autoscroll = if is_btw {
            None
        } else {
            self.drag_autoscroll_for(mouse.row)
        };
        changed
    }

    pub(in crate::app) fn handle_scrollback_drag_motion(
        &mut self,
        mouse: &MouseEvent,
    ) -> InputOutcome {
        if self.scrollbar_dragging {
            self.apply_scrollbar_click(mouse.row);
            return InputOutcome::Changed;
        }
        if self.drag_selection.is_some() {
            return if self.update_active_text_drag(mouse) {
                InputOutcome::Changed
            } else {
                InputOutcome::Unchanged
            };
        }
        // Anchor-less press: convert to a text drag the moment the pointer
        // enters selectable text; a miss falls through to the block-drag
        // branches unchanged.
        if self.convert_deferred_text_press(mouse) {
            return InputOutcome::Changed;
        }
        if self.block_drag_selection.is_some() {
            return if self.update_active_block_drag(mouse) {
                InputOutcome::Changed
            } else {
                InputOutcome::Unchanged
            };
        }
        if self.pending_text_drag.is_some() {
            return if self.update_text_drag(mouse) {
                InputOutcome::Changed
            } else {
                InputOutcome::Unchanged
            };
        }
        if self.pending_block_drag.is_some() {
            return if self.promote_pending_block_drag(mouse) {
                InputOutcome::Changed
            } else {
                InputOutcome::Unchanged
            };
        }
        // An armed anchor-less press owns the rest of the gesture: strip
        // presses deliberately keep focus where it was, so a conversion miss
        // must not leak motion into the todo/viewer/prompt handlers below
        // (a prompt-focused strip drag would otherwise edit the prompt).
        if self.deferred_text_press.is_some() {
            return InputOutcome::Unchanged;
        }
        if self.active_pane == AgentPane::Todo
            && self
                .todo
                .handle_mouse(mouse.kind, mouse.column, mouse.row, self.pane_areas.todo)
        {
            return InputOutcome::Changed;
        }
        if self.block_viewer.is_some() && self.active_pane == AgentPane::Scrollback {
            self.handle_block_viewer_mouse(mouse);
            return InputOutcome::Changed;
        }
        if self.active_pane == AgentPane::Prompt {
            self.prompt.handle_mouse(mouse);
            InputOutcome::Changed
        } else {
            InputOutcome::Unchanged
        }
    }

    /// Clipboard text for a finished drag, tagged with the kind that
    /// produced it — `Linear` when a table copy fell through — so the
    /// persisted highlight mirrors what reached the clipboard.
    fn reconstruct_drag_copy(&self, drag: &ActiveTextDrag) -> Option<(String, SelectionKind)> {
        if drag.anchor.entry_idx == BTW_OVERLAY_ENTRY_IDX {
            // Same width rule as scrollback anchors: the drag-start snapshot
            // matches the wrap the drag's block_line_idx values came from,
            // with the current panel width as fallback.
            let content_width = drag
                .anchor_content_width
                .map(usize::from)
                .unwrap_or_else(|| self.last_btw_area.width.saturating_sub(4) as usize);
            if let Some(ref btw) = self.btw_state {
                let full_model = btw.full_selection_model(content_width);
                if let Some(text) = reconstruct_selection_text(&full_model, drag) {
                    return Some((text, SelectionKind::Linear));
                }
            }
            return reconstruct_selection_text(&self.last_btw_selection_model, drag)
                .map(|text| (text, SelectionKind::Linear));
        }
        if drag.kind != SelectionKind::Linear
            && let Some(geom) =
                self.table_geometry_for_selection(drag.anchor.entry_idx, drag.anchor.range_id)
            && let Some(text) = self
                .with_entry_output_text_source(
                    drag.anchor.entry_idx,
                    drag.anchor.range_id,
                    // Snapshot keeps the table copy alive after the block
                    // autoscrolls fully out of the viewport.
                    drag.anchor_content_width,
                    |src| {
                        // Geometry was frozen at promote; a streaming re-wrap
                        // since then shifts every block_line_idx, so re-detect
                        // and require an exact match before slicing.
                        if TableGeometry::detect(src, drag.anchor.block_line_idx).as_ref()
                            != Some(geom)
                        {
                            return None;
                        }
                        reconstruct_table_selection_text(geom, drag, src)
                    },
                )
                .flatten()
        {
            return Some((text, drag.kind));
        }
        let scrollback = if let Some(ref child_id) = self.active_subagent
            && let Some(child) = self.subagent_views.get(child_id)
        {
            &child.scrollback
        } else {
            &self.scrollback
        };
        let visible_start = scrollback.visible_entry_range().start;
        let abs_idx = drag.anchor.entry_idx + visible_start;
        // Width must come from the same VisibleBlockGeometry the drag's
        // block_line_idx values were captured against; the pane-wide width
        // re-wraps timestamp-reserving blocks and shifts every index.
        // Prefer the drag-start snapshot: by mouse-up the anchor block may
        // have autoscrolled fully out of `visible_blocks`.
        let entry_content_width = drag.anchor_content_width.or_else(|| {
            self.last_scrollback_selection_model
                .visible_block_content_width(drag.anchor.entry_idx)
        });
        if let Some(entry) = scrollback.get(abs_idx)
            && let Some(content_width) = entry_content_width
        {
            let appearance = scrollback.appearance();
            entry.ensure_cached(content_width, appearance, false, scrollback.cwd());
            let rendered = entry.cached_rendered_output_ref();
            if let Some(text) = reconstruct_full_selection_text_with_boundaries(
                &rendered.output.lines,
                &rendered.boundaries,
                drag,
            ) {
                return Some((text, SelectionKind::Linear));
            }
        }
        reconstruct_selection_text_with_boundaries(
            &self.last_scrollback_selection_model,
            &self.last_scrollback_selection_boundaries,
            drag,
        )
        .map(|text| (text, SelectionKind::Linear))
    }

    /// Persist a finished drag as the highlight. `kind` is the copied
    /// shape, not `drag.kind`: a degraded table drag persists `Linear` and
    /// drops its orphaned side-car.
    fn persist_drag_selection(&mut self, drag: &ActiveTextDrag, kind: SelectionKind) {
        if kind == SelectionKind::Linear && drag.kind != SelectionKind::Linear {
            self.table_selection_geometry = None;
        }
        self.persistent_text_selection = Some(PersistentTextSelection {
            entry_idx: drag.anchor.entry_idx,
            range_id: drag.anchor.range_id,
            anchor: SelectionEndpoint {
                block_line_idx: drag.anchor.block_line_idx,
                col_within_range: drag.anchor.col_within_range,
            },
            head: SelectionEndpoint {
                block_line_idx: drag.head.block_line_idx,
                col_within_range: drag.head.col_within_range,
            },
            origin: SelectionOrigin::Drag,
            kind,
        });
        self.selection_created_at = Some(Instant::now());
    }

    pub(in crate::app) fn finish_text_drag(&mut self) -> bool {
        let drag = self.drag_selection;
        let copied = drag.and_then(|d| self.reconstruct_drag_copy(&d));
        self.pending_text_drag = None;
        self.drag_selection = None;
        self.drag_autoscroll = None;
        self.last_drag_mouse = None;
        if let Some((text, kind)) = copied
            && !text.is_empty()
        {
            // Capture drag geometry as a persistent selection only when the
            // clipboard copy succeeds. Setting it unconditionally would leave
            // a highlight with nothing in the clipboard if reconstruction fails.
            if let Some(d) = drag {
                self.persist_drag_selection(&d, kind);
            }
            self.copy_to_clipboard(&text);
            return true;
        }
        false
    }

    pub(in crate::app) fn begin_pending_block_drag(&mut self, mouse: &MouseEvent) -> bool {
        let block = self
            .last_scrollback_selection_model
            .hit_test_visible_block(mouse.column, mouse.row);
        let Some(block) = block else {
            return false;
        };
        if !block.drag_startable {
            return false;
        }
        tracing::debug!(
            event = "begin_pending_block_drag",
            entry_idx = block.entry_idx,
            col = mouse.column,
            row = mouse.row,
            "scrollback block drag start"
        );
        self.pending_block_drag = Some(PendingBlockDrag {
            anchor_entry_idx: block.entry_idx,
            start_col: mouse.column,
            start_row: mouse.row,
        });
        self.drag_selection = None;
        self.block_drag_selection = None;
        true
    }

    fn promote_pending_block_drag(&mut self, mouse: &MouseEvent) -> bool {
        let Some(pending) = self.pending_block_drag else {
            return false;
        };
        if !block_drag_threshold_exceeded(&pending, mouse.column, mouse.row) {
            return false;
        }
        let head_entry_idx = self
            .last_scrollback_selection_model
            .hit_test_visible_block(mouse.column, mouse.row)
            .map(|b| b.entry_idx)
            .unwrap_or(pending.anchor_entry_idx);
        tracing::debug!(
            event = "promote_block_drag",
            anchor = pending.anchor_entry_idx,
            head = head_entry_idx,
            "scrollback block drag promoted"
        );
        self.pending_block_drag = None;
        self.block_drag_selection = Some(ActiveBlockDrag {
            anchor_entry_idx: pending.anchor_entry_idx,
            head_entry_idx,
        });
        true
    }

    fn update_active_block_drag(&mut self, mouse: &MouseEvent) -> bool {
        let Some(ref mut drag) = self.block_drag_selection else {
            return false;
        };
        let new_head = self
            .last_scrollback_selection_model
            .hit_test_visible_block(mouse.column, mouse.row)
            .map(|b| b.entry_idx)
            .unwrap_or(drag.head_entry_idx);
        let changed = new_head != drag.head_entry_idx;
        drag.head_entry_idx = new_head;
        self.last_drag_mouse = Some((mouse.column, mouse.row));
        self.drag_autoscroll = self.drag_autoscroll_for(mouse.row);
        changed
    }

    /// Autoscroll for a drag pointer at `row`, measured from the first content row: a pinned sticky header is fixed chrome and never a scroll gutter.
    fn drag_autoscroll_for(&self, row: u16) -> Option<DragAutoScrollState> {
        let pane = self.pane_areas.scrollback;
        let content = self.last_scrollback_selection_model.content_area;
        let pane_bottom = pane.y.saturating_add(pane.height);

        // Header-only viewport: the pane publishes a zero-height content rect at the header's end. All chrome,
        // nothing to scroll toward. An unset rect (no frame yet) falls back to the pane-wide zone instead.
        if content.height == 0 {
            let header_only = content.y > pane.y && content.y < pane_bottom;
            return if header_only {
                None
            } else {
                compute_autoscroll(row, pane)
            };
        }

        // A content rect outside this pane belongs to another layout; use the pane-wide zone.
        if content.y < pane.y || content.y >= pane_bottom {
            return compute_autoscroll(row, pane);
        }

        // Header rows above the content are inert chrome.
        if (pane.y..content.y).contains(&row) {
            return None;
        }

        compute_autoscroll(
            row,
            Rect {
                y: content.y,
                height: pane_bottom - content.y,
                ..pane
            },
        )
    }

    pub(in crate::app) fn finish_block_drag(&mut self) -> bool {
        let drag = self.block_drag_selection.take();
        self.pending_block_drag = None;
        self.drag_autoscroll = None;
        self.last_drag_mouse = None;
        let Some(drag) = drag else {
            return false;
        };
        let start = std::cmp::min(drag.anchor_entry_idx, drag.head_entry_idx);
        let end = std::cmp::max(drag.anchor_entry_idx, drag.head_entry_idx);

        let appearance = self.scrollback.appearance().clone();
        let mut parts: Vec<String> = Vec::new();

        for idx in start..=end {
            let Some(entry) = self.scrollback.entry(idx) else {
                continue;
            };
            if !entry.block.is_drag_block_selectable() {
                continue;
            }
            if self.scrollback.entry_content_hidden_by_group(idx) {
                continue;
            }

            // BgTask: copy stdout from session state when available.
            if let crate::scrollback::block::RenderBlock::BgTask(ref block) = entry.block {
                let stdout = self
                    .session
                    .bg_tasks
                    .get(&block.task_id)
                    .map(|t| &t.stdout)
                    .filter(|s| !s.is_empty());
                if let Some(stdout) = stdout {
                    parts.push(stdout.clone());
                    continue;
                }
            }

            // Find the content width from the resolved model's visible block geometry.
            let content_width = self
                .last_scrollback_selection_model
                .visible_block_content_width(idx)
                .unwrap_or(80);

            let ctx = crate::scrollback::types::BlockContext {
                mode: entry.display_mode,
                is_running: entry.is_running,
                width: content_width,
                raw: entry.raw,
                max_lines: None,
                appearance: appearance.clone(),
                is_selected: self.scrollback.selected() == Some(idx),
                cwd: Some(self.session.cwd.clone()),
            };

            if let Some(text) = entry.block.copy_visible_text_in_state(&ctx)
                && !text.is_empty()
            {
                parts.push(text);
            }
        }

        let text = parts.join("\n\n");
        tracing::debug!(
            event = "finish_block_drag",
            anchor = drag.anchor_entry_idx,
            head = drag.head_entry_idx,
            blocks_copied = parts.len(),
            "scrollback block drag finished"
        );
        if !text.is_empty() {
            self.copy_to_clipboard(&text);
            return true;
        }
        false
    }

    /// Handle a click on a scrollback entry with multi-click detection.
    ///
    /// - Single click: select entry
    /// - Double-click non-prompt: toggle fold in place
    /// - Double-click prompt: toggle fold + scroll to top
    /// - Triple-click non-prompt: toggle fold + scroll to top
    ///
    /// Returns `(last_click_state, show_word_select_tip)`. The tip flag is set
    /// on a REPEATED double-click on assistant text (second gesture within
    /// [`WORD_SELECT_REPEAT_WINDOW`]) while Text selection is still fold/nav
    /// (not `word_select`), so dispatch can teach `/settings → Text
    /// selection`. A lone double-click, or one on fold-affordance surfaces
    /// (headers, prompts, tool rows), never tips.
    pub(in crate::app) fn handle_scrollback_click(
        &mut self,
        now: Instant,
        idx: usize,
        header_row_click: bool,
    ) -> (Option<(Instant, usize, u8)>, bool) {
        let click_count = if let Some((last_time, last_idx, prev_count)) = self.last_click
            && last_idx == idx
            && now.duration_since(last_time).as_millis() < MULTI_CLICK_TIMEOUT_MS
        {
            prev_count + 1
        } else {
            1
        };

        let entry_block = self.scrollback.entry(idx).map(|e| &e.block);
        let is_bg_task = entry_block
            .is_some_and(|b| matches!(b, crate::scrollback::block::RenderBlock::BgTask(_)));
        let is_subagent = entry_block
            .is_some_and(|b| matches!(b, crate::scrollback::block::RenderBlock::Subagent(_)));
        let is_workflow = entry_block
            .is_some_and(|b| matches!(b, crate::scrollback::block::RenderBlock::Workflow(_)));

        // Word-select tip probe (see WORD_SELECT_REPEAT_WINDOW): assistant
        // messages only — headers / prompts / tool rows are fold-nav surfaces
        // where double-click is the designed gesture, and bg-task / subagent
        // double-clicks open a viewer that owns input.
        let word_select_probe = click_count == 2
            && entry_block.is_some_and(|b| b.is_agent_message())
            && !super::is_text_selection_on_double_click();
        let show_word_select_tip = word_select_probe
            && self
                .last_word_select_probe
                .is_some_and(|t| now.duration_since(t) <= WORD_SELECT_REPEAT_WINDOW);
        if word_select_probe {
            self.last_word_select_probe = Some(now);
        }

        self.scrollback.set_selected(Some(idx));

        // Expanded verb-group slot, header row (`header_row_click`): the slot
        // acts as member 0 everywhere else, so the group affordance lives
        // here — double-click on the header row collapses the group; a
        // single click just selects. Member rows fall through to the normal
        // foldable path below.
        if header_row_click {
            if click_count == 2 {
                self.scrollback.collapse_group_if_expanded();
                return (None, show_word_select_tip);
            }
            if click_count >= 3 {
                return (None, false);
            }
        }

        // Double-click on a group header → expand/collapse the group.
        // Both expand ("N more") and collapse ("▾ N tool calls") headers
        // are standalone entries with their own index.
        let is_group_header = self.scrollback.is_selected_group_header();
        if is_group_header {
            if click_count == 2 {
                self.scrollback.toggle_group_expansion();
                return (None, show_word_select_tip);
            }
            if click_count >= 3 {
                return (None, false);
            }
        }

        let foldable = self.scrollback.get(idx).is_some_and(|e| e.is_foldable());
        let is_prompt = self
            .scrollback
            .entry(idx)
            .is_some_and(|e| e.block.is_user_prompt());

        // Single-click on plan mode tool call → show plan preview/toast.
        let is_plan_tool = !is_group_header
            && self
                .scrollback
                .entry(idx)
                .is_some_and(|e| e.block.is_plan_mode_tool());

        // Credit-limit URL click is handled upstream (before this method)
        // so only the URL line is clickable, not the whole block.

        // Double-click on bg-task / subagent blocks (matched above) opens a
        // viewer instead of folding.
        match click_count {
            1 if is_plan_tool => {
                self.show_plan_preview();
            }
            2 if is_bg_task => {
                // Double-click bg task: open block viewer (same as Enter).
                if let Some(entry) = self.scrollback.entry(idx)
                    && let crate::scrollback::block::RenderBlock::BgTask(ref bt) = entry.block
                    && let Some(task) = self.session.bg_tasks.get(&bt.task_id)
                {
                    let eid = task
                        .scrollback_entry_id
                        .unwrap_or_else(|| crate::scrollback::entry::EntryId::new(0));
                    let is_running = task.status == crate::app::agent::BgTaskStatus::Running;
                    self.block_viewer =
                        Some(crate::views::block_viewer::BlockViewerPane::for_bg_task(
                            eid,
                            &bt.task_id,
                            &task.stdout,
                            is_running,
                        ));
                }
            }
            2 if is_subagent => {
                // Double-click subagent: open subagent view (same as Enter)
                if let Some(entry) = self.scrollback.entry(idx)
                    && let crate::scrollback::block::RenderBlock::Subagent(ref sb) = entry.block
                {
                    let child_sid = sb.child_session_id.clone();
                    if self.subagent_views.contains_key(&child_sid) {
                        self.open_subagent_fullscreen(child_sid);
                    }
                }
            }
            2 if is_workflow => {
                if let Some(entry) = self.scrollback.entry(idx)
                    && let crate::scrollback::block::RenderBlock::Workflow(ref wf) = entry.block
                {
                    let run_id = wf.run_id.clone();
                    self.open_workflow_detail_by_run_id(&run_id);
                }
            }
            2 if is_prompt => {
                // Edit in place; bash/cron keep the old fold behavior.
                //
                // Gated OFF for now (unsolved scroll jump on enter — see
                // inline_edit::INLINE_EDIT_ENABLED). When disabled this is a
                // no-op, so the block below runs and restores the EXACT
                // pre-feature double-click behavior for a prompt: fold (if
                // foldable) + scroll the entry to the top.
                if !(crate::app::inline_edit::INLINE_EDIT_ENABLED && self.enter_inline_edit(idx)) {
                    if foldable {
                        self.scrollback.toggle_fold_selected();
                    }
                    self.scrollback.scroll_to_entry_top(idx);
                }
            }
            2 => {
                if foldable {
                    self.scrollback.toggle_fold_selected();
                }
            }
            3.. if !is_prompt => {
                if foldable {
                    self.scrollback.toggle_fold_selected();
                }
                self.scrollback.scroll_to_entry_top(idx);
            }
            _ => {}
        }

        let last_click = if click_count >= 3 {
            None
        } else {
            Some((now, idx, click_count))
        };
        (last_click, show_word_select_tip)
    }

    /// Return the correct selection model for a hit, accounting for the
    /// /btw overlay panel which has its own model.
    fn selection_model_for_hit(&self, hit: &RangeHit) -> &ResolvedSelectionModel {
        if hit.entry_idx == BTW_OVERLAY_ENTRY_IDX {
            &self.last_btw_selection_model
        } else {
            &self.last_scrollback_selection_model
        }
    }

    /// Count text-level multi-clicks, incrementing when the new click lands
    /// on the same (entry, range, line) within [`MULTI_CLICK_TIMEOUT_MS`].
    pub(in crate::app) fn count_text_click(&self, now: Instant, hit: &RangeHit) -> u8 {
        if let Some(ref prev) = self.last_text_click
            && prev.entry_idx == hit.entry_idx
            && prev.range_id == hit.range_id
            && prev.block_line_idx == hit.block_line_idx
            && now.duration_since(prev.time).as_millis() < MULTI_CLICK_TIMEOUT_MS
        {
            prev.click_count.saturating_add(1)
        } else {
            1
        }
    }

    /// Select the word (or URL) under the cursor described by `hit`.
    pub(in crate::app) fn select_word_at(&mut self, hit: &RangeHit) {
        let model = self.selection_model_for_hit(hit);
        let separators = configured_word_separators();
        let Some(selection) = semantic_selection_at(model, hit, separators) else {
            return;
        };
        self.persistent_text_selection = Some(PersistentTextSelection {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            anchor: selection.anchor,
            head: selection.head,
            origin: SelectionOrigin::DoubleClick,
            kind: SelectionKind::Linear,
        });
        self.selection_created_at = Some(Instant::now());
        self.copy_to_clipboard_debounced(&selection.text);

        if hit.entry_idx != BTW_OVERLAY_ENTRY_IDX {
            self.scrollback.set_selected(Some(hit.entry_idx));
        }
    }

    fn selection_text_for_full_line(&self, hit: &RangeHit) -> Option<String> {
        let model = self.selection_model_for_hit(hit);
        let line = model.line_for_hit(hit)?;
        let boundary = if hit.entry_idx == BTW_OVERLAY_ENTRY_IDX {
            None
        } else {
            self.last_scrollback_selection_boundaries
                .boundary_for_hit(hit)
        };
        Some(apply_selection_boundary(
            line.text.clone(),
            boundary,
            true,
            true,
        ))
    }

    /// Select the full line at the cursor position described by `hit`.
    pub(in crate::app) fn select_line_at(&mut self, hit: &RangeHit) {
        let model = self.selection_model_for_hit(hit);
        let Some(line) = model.line_for_hit(hit) else {
            return;
        };
        let width = line
            .selectable_cols
            .end
            .saturating_sub(line.selectable_cols.start);

        if width == 0 {
            return;
        }

        let Some(clipboard_text) = self.selection_text_for_full_line(hit) else {
            return;
        };

        self.persistent_text_selection = Some(PersistentTextSelection {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            anchor: SelectionEndpoint {
                block_line_idx: hit.block_line_idx,
                col_within_range: 0,
            },
            head: SelectionEndpoint {
                block_line_idx: hit.block_line_idx,
                col_within_range: width.saturating_sub(1),
            },
            origin: SelectionOrigin::TripleClick,
            kind: SelectionKind::Linear,
        });
        self.selection_created_at = Some(Instant::now());

        if !clipboard_text.is_empty() {
            self.copy_to_clipboard_debounced(&clipboard_text);
        }

        if hit.entry_idx != BTW_OVERLAY_ENTRY_IDX {
            self.scrollback.set_selected(Some(hit.entry_idx));
        }
    }

    /// Select the logical line (paragraph or list item) at `hit`.
    ///
    /// The run is bounded to a single pre-wrap logical line: prose paragraphs
    /// collapse soft breaks into one logical line, so this takes the whole
    /// wrapped paragraph, while each list item (and each hard-broken source
    /// line) is its own logical line, so triple-click on a bullet takes just
    /// that item, not the whole list. Soft-wrap continuation rows carry a
    /// `joiner_to_previous`; a `None` joiner marks a new logical line and bounds
    /// the run. A single-line run delegates to [`Self::select_line_at`].
    pub(in crate::app) fn select_paragraph_at(&mut self, hit: &RangeHit) {
        // Resolve the run's first/last rendered lines under an immutable
        // borrow, then release it before mutating selection state.
        let resolved = {
            let model = self.selection_model_for_hit(hit);
            let Some(range) = model.range(hit.entry_idx, hit.range_id) else {
                return;
            };
            let Some(click_pos) = range
                .lines
                .iter()
                .position(|line| line.block_line_idx == hit.block_line_idx)
            else {
                return;
            };

            // `b` is a soft-wrap continuation of `a` (same logical line): the two
            // are adjacent and `b` carries a joiner. A `None` joiner starts a new
            // logical line (new paragraph, list item, or source line) and bounds
            // the run. Reads go through `.get()`, so an out-of-range neighbor just
            // ends the walk instead of panicking; `click_pos` is valid and
            // start/end only move inward-to-outward from it.
            let continues = |a: usize, b: usize| match (range.lines.get(a), range.lines.get(b)) {
                (Some(a), Some(b)) => {
                    a.block_line_idx + 1 == b.block_line_idx && b.joiner_to_previous.is_some()
                }
                _ => false,
            };
            let mut start = click_pos;
            while start > 0 && continues(start - 1, start) {
                start -= 1;
            }
            let mut end = click_pos;
            while continues(end, end + 1) {
                end += 1;
            }

            // Bounds-checked reads; `None` only if the model shifted under us,
            // in which case the caller falls back to the single clicked line.
            match (range.lines.get(start), range.lines.get(end)) {
                (Some(first), Some(last)) => Some((
                    first.block_line_idx,
                    last.block_line_idx,
                    last.selectable_cols
                        .end
                        .saturating_sub(last.selectable_cols.start),
                )),
                _ => None,
            }
        };

        // Degrade to the single clicked line if the run could not be resolved,
        // which never panics and is still a valid selection.
        let Some((first_block_line_idx, last_block_line_idx, last_width)) = resolved else {
            self.select_line_at(hit);
            return;
        };

        // A single-line paragraph is exactly the clicked line, so reuse the
        // tested full-line copy path (boundary-aware).
        if first_block_line_idx == last_block_line_idx {
            self.select_line_at(hit);
            return;
        }
        if last_width == 0 {
            return;
        }

        let model = self.selection_model_for_hit(hit);
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: hit.entry_idx,
                range_id: hit.range_id,
                block_line_idx: first_block_line_idx,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: hit.entry_idx,
                range_id: hit.range_id,
                block_line_idx: last_block_line_idx,
                col_within_range: last_width.saturating_sub(1),
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };
        let clipboard_text = if hit.entry_idx == BTW_OVERLAY_ENTRY_IDX {
            reconstruct_selection_text(model, &drag)
        } else {
            reconstruct_selection_text_with_boundaries(
                model,
                &self.last_scrollback_selection_boundaries,
                &drag,
            )
        };

        self.persistent_text_selection = Some(PersistentTextSelection {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            anchor: SelectionEndpoint {
                block_line_idx: first_block_line_idx,
                col_within_range: 0,
            },
            head: SelectionEndpoint {
                block_line_idx: last_block_line_idx,
                col_within_range: last_width.saturating_sub(1),
            },
            origin: SelectionOrigin::TripleClick,
            kind: SelectionKind::Linear,
        });
        self.selection_created_at = Some(Instant::now());

        if let Some(text) = clipboard_text.filter(|text| !text.is_empty()) {
            self.copy_to_clipboard_debounced(&text);
        }

        if hit.entry_idx != BTW_OVERLAY_ENTRY_IDX {
            self.scrollback.set_selected(Some(hit.entry_idx));
        }
    }

    /// Select and copy the whole table cell at `hit`, wrapped fragments
    /// included. `false` when there is no cell there (no grid, or a column
    /// outside it), so the caller falls back to paragraph selection.
    pub(in crate::app) fn select_cell_at(&mut self, hit: &RangeHit) -> bool {
        let Some(geometry) = self.compute_drag_table_geometry(hit) else {
            return false;
        };
        // Triple-click on a border/divider row is the whole-table shortcut
        // (drags from grid lines stay linear).
        let Some(cell) = geometry.cell_at(hit.block_line_idx, hit.col_within_range) else {
            return self.select_whole_table_at(hit, geometry);
        };
        let Some(clipboard_text) =
            self.with_entry_output_text_source(hit.entry_idx, hit.range_id, None, |src| {
                geometry.cell_text(cell, src)
            })
        else {
            return false;
        };

        let lines = geometry.row_lines(cell.row);
        let band = geometry.band(cell.col);
        self.persistent_text_selection = Some(PersistentTextSelection {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            anchor: SelectionEndpoint {
                block_line_idx: lines.start,
                col_within_range: band.start,
            },
            head: SelectionEndpoint {
                block_line_idx: lines.end.saturating_sub(1),
                col_within_range: band.end.saturating_sub(1),
            },
            origin: SelectionOrigin::TripleClick,
            kind: SelectionKind::TableCell,
        });
        self.table_selection_geometry = Some(TableSelectionGeometry {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            geometry,
        });
        self.selection_created_at = Some(Instant::now());

        if !clipboard_text.is_empty() {
            self.copy_to_clipboard_debounced(&clipboard_text);
        }
        self.scrollback.set_selected(Some(hit.entry_idx));
        true
    }

    /// Select and copy the entire table containing `hit` — the grid-line
    /// counterpart of [`Self::select_cell_at`].
    fn select_whole_table_at(&mut self, hit: &RangeHit, geometry: TableGeometry) -> bool {
        let anchor_cell = CellRef { row: 0, col: 0 };
        let head_cell = CellRef {
            row: geometry.n_rows() - 1,
            col: geometry.n_cols() - 1,
        };
        let Some(clipboard_text) =
            self.with_entry_output_text_source(hit.entry_idx, hit.range_id, None, |src| {
                geometry.grid_tsv(anchor_cell, head_cell, src)
            })
        else {
            return false;
        };

        let first = geometry.row_lines(0);
        let last = geometry.row_lines(head_cell.row);
        self.persistent_text_selection = Some(PersistentTextSelection {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            anchor: SelectionEndpoint {
                block_line_idx: first.start,
                col_within_range: geometry.band(0).start,
            },
            head: SelectionEndpoint {
                block_line_idx: last.end.saturating_sub(1),
                col_within_range: geometry.band(head_cell.col).end.saturating_sub(1),
            },
            origin: SelectionOrigin::TripleClick,
            kind: SelectionKind::TableGrid {
                anchor: anchor_cell,
                head: head_cell,
            },
        });
        self.table_selection_geometry = Some(TableSelectionGeometry {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            geometry,
        });
        self.selection_created_at = Some(Instant::now());

        if !clipboard_text.is_empty() {
            self.copy_to_clipboard_debounced(&clipboard_text);
        }
        self.scrollback.set_selected(Some(hit.entry_idx));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionRegistry;
    use crate::app::agent_view::test_fixtures::make_agent;
    use crate::scrollback::table_geometry::CellRef;
    use crate::scrollback::text_selection::{ResolvedSelectableLine, VisibleBlockGeometry};
    use crossterm::event::{Event, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    const TABLE: &[&str] = &[
        "┌─────────┬────────┐",
        "│ Name    │ Role   │",
        "├─────────┼────────┤",
        "│ Alice   │ Eng    │",
        "└─────────┴────────┘",
    ];

    fn table_geometry() -> TableGeometry {
        TableGeometry::detect(|i| TABLE.get(i).map(|s| s.to_string()), 1).expect("grid detected")
    }

    /// Agent whose TABLE lines are in the visible model but with no
    /// `visible_blocks`/scrollback entry, forcing the table copy to fall
    /// through to the visible-model linear reconstruction.
    fn agent_with_visible_table_lines_only() -> AgentView {
        let mut agent = make_agent();
        let mut model = ResolvedSelectionModel::default();
        for (i, text) in TABLE.iter().enumerate() {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i,
                screen_y: i as u16,
                screen_x: 0,
                selectable_cols: 0..20,
                text: (*text).to_string(),
                painted_region: None,
                joiner_to_previous: None,
            });
        }
        agent.update_scrollback_selection_state(model, Default::default());
        agent.table_selection_geometry = Some(TableSelectionGeometry {
            entry_idx: 0,
            range_id: 0,
            geometry: table_geometry(),
        });
        agent
    }

    fn table_drag() -> ActiveTextDrag {
        ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 1,
                col_within_range: 2,
            },
            head: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 3,
                col_within_range: 12,
            },
            kind: SelectionKind::TableGrid {
                anchor: CellRef { row: 0, col: 0 },
                head: CellRef { row: 1, col: 1 },
            },
            anchor_content_width: None,
        }
    }

    /// Build an agent whose scrollback selection model holds a single range
    /// with the given `(block_line_idx, text, joiner_to_previous)` lines. A
    /// `None` joiner starts a new logical line; `Some` marks a soft-wrap
    /// continuation of the previous line.
    fn agent_with_range_lines(lines: &[(usize, &str, Option<&str>)]) -> AgentView {
        let mut agent = make_agent();
        let mut model = ResolvedSelectionModel::default();
        for (bl, text, joiner) in lines {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: *bl,
                screen_y: *bl as u16,
                screen_x: 0,
                selectable_cols: 0..text.len() as u16,
                text: (*text).to_string(),
                painted_region: None,
                joiner_to_previous: joiner.map(str::to_string),
            });
        }
        agent.update_scrollback_selection_state(model, Default::default());
        agent
    }

    /// Triple-click on a soft-wrapped prose paragraph selects the whole wrapped
    /// logical line (continuation rows carry a joiner) and stops at the next
    /// logical line.
    #[test]
    fn select_paragraph_at_selects_soft_wrapped_logical_line() {
        // One paragraph wrapped across rows 0,1,2 (row 0 starts it; 1 and 2 are
        // soft-wrap continuations). Row 3 starts the next paragraph.
        let mut agent = agent_with_range_lines(&[
            (0, "alpha one", None),
            (1, "alpha two", Some(" ")),
            (2, "alpha three", Some(" ")),
            (3, "bravo one", None),
        ]);

        agent.select_paragraph_at(&RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 1, // a continuation row
            col_within_range: 0,
        });

        let sel = agent
            .persistent_text_selection
            .expect("paragraph selection persists");
        assert_eq!(sel.origin, SelectionOrigin::TripleClick);
        assert_eq!(sel.kind, SelectionKind::Linear);
        assert_eq!(
            sel.anchor.block_line_idx, 0,
            "extends to the paragraph start"
        );
        assert_eq!(
            sel.head.block_line_idx, 2,
            "stops at the next logical line, not into the following paragraph"
        );
    }

    /// Triple-click on a tight bullet list selects only the clicked item, not
    /// the whole list: each item is its own logical line (joiner `None`).
    #[test]
    fn select_paragraph_at_on_bullet_selects_only_that_item() {
        let mut agent = agent_with_range_lines(&[
            (0, "\u{2022} item one", None),
            (1, "\u{2022} item two", None),
            (2, "\u{2022} item three", None),
        ]);

        agent.select_paragraph_at(&RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 1,
            col_within_range: 0,
        });

        let sel = agent.persistent_text_selection.expect("selection persists");
        assert_eq!(sel.anchor.block_line_idx, 1, "only the clicked bullet");
        assert_eq!(
            sel.head.block_line_idx, 1,
            "does not extend to sibling bullets"
        );
    }

    /// A wrapped bullet (its continuation carries a joiner) selects the whole
    /// item, but not the sibling bullets on either side.
    #[test]
    fn select_paragraph_at_on_wrapped_bullet_selects_the_item_only() {
        let mut agent = agent_with_range_lines(&[
            (0, "\u{2022} first", None),
            (1, "\u{2022} second is long", None),
            (2, "and wraps", Some(" ")),
            (3, "\u{2022} third", None),
        ]);

        agent.select_paragraph_at(&RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 1, // start of the wrapped bullet
            col_within_range: 0,
        });

        let sel = agent
            .persistent_text_selection
            .expect("paragraph selection persists");
        assert_eq!(sel.anchor.block_line_idx, 1, "starts at the clicked bullet");
        assert_eq!(
            sel.head.block_line_idx, 2,
            "includes the wrap but stops before the next bullet"
        );
    }

    /// A single-line logical run selects exactly that line (delegates to the
    /// existing line-select path).
    #[test]
    fn select_paragraph_at_single_line_selects_only_that_line() {
        let mut agent = agent_with_range_lines(&[(1, "solo", None)]);

        agent.select_paragraph_at(&RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 1,
            col_within_range: 0,
        });

        let sel = agent.persistent_text_selection.expect("selection persists");
        assert_eq!(sel.origin, SelectionOrigin::TripleClick);
        assert_eq!(sel.anchor.block_line_idx, 1);
        assert_eq!(sel.head.block_line_idx, 1);
    }

    #[test]
    fn semantic_word_and_url_copy_ignore_hidden_edit_boundaries() {
        let mut model = ResolvedSelectionModel::default();
        let mut boundaries =
            crate::scrollback::text_selection::ResolvedSelectionBoundaries::default();
        for (entry_idx, text, hit_col, prefix, suffix, expected) in [
            (0, "foo rest", 0, "   ", "", "foo"),
            (
                1,
                "rest https://echo-agent.invalid",
                5,
                "",
                "   ",
                "https://echo-agent.invalid",
            ),
        ] {
            let line = ResolvedSelectableLine {
                entry_idx,
                range_id: 0,
                block_line_idx: 0,
                screen_y: entry_idx as u16,
                screen_x: 0,
                selectable_cols: 0..text.len() as u16,
                text: text.to_string(),
                painted_region: None,
                joiner_to_previous: None,
            };
            boundaries.push(
                &line,
                std::sync::Arc::new(crate::scrollback::types::SelectionBoundary::new(
                    prefix.to_string(),
                    suffix.to_string(),
                )),
            );
            model.push_line(line);
            let hit = RangeHit {
                entry_idx,
                range_id: 0,
                block_line_idx: 0,
                col_within_range: hit_col,
            };
            let copied = semantic_selection_at(
                &model,
                &hit,
                crate::scrollback::text_selection::DEFAULT_WORD_SEPARATORS,
            )
            .expect("semantic range")
            .text;
            assert_eq!(copied, expected);
        }
        assert!(!boundaries.is_empty());
    }

    #[test]
    fn select_word_at_stores_wrap_aware_endpoints() {
        let mut agent = make_agent();
        let mut model = ResolvedSelectionModel::default();
        for (i, (text, joiner)) in [("hello_world_", None), ("identifier", Some(""))]
            .into_iter()
            .enumerate()
        {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i,
                screen_y: i as u16,
                screen_x: 0,
                selectable_cols: 0..text.len() as u16,
                text: text.to_string(),
                painted_region: None,
                joiner_to_previous: joiner.map(str::to_string),
            });
        }
        agent.update_scrollback_selection_state(model, Default::default());
        agent.select_word_at(&RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 1,
            col_within_range: 0,
        });
        assert_eq!(
            agent.persistent_text_selection,
            Some(PersistentTextSelection {
                entry_idx: 0,
                range_id: 0,
                anchor: SelectionEndpoint {
                    block_line_idx: 0,
                    col_within_range: 0,
                },
                head: SelectionEndpoint {
                    block_line_idx: 1,
                    col_within_range: 9,
                },
                origin: SelectionOrigin::DoubleClick,
                kind: SelectionKind::Linear,
            })
        );
    }

    #[test]
    fn installed_scrollback_companion_reaches_linear_and_full_line_copy() {
        use crate::scrollback::block::RenderBlock;
        use crate::scrollback::render::ScratchBuffer;
        use crate::scrollback::scrollback_pane::ScrollbackPane;
        use crate::scrollback::state::ScrollbackState;
        use crate::scrollback::types::DisplayMode;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let mut state = ScrollbackState::new();
        let edit = state.push_block(RenderBlock::edit("   foo.rs   ", None));
        state
            .get_by_id_mut(edit)
            .expect("Edit entry")
            .set_display_mode(DisplayMode::Expanded);
        let area = Rect::new(0, 0, 8, 20);
        state.prepare_layout(area.width, area.height);
        let mut buffer = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        let rendered = ScrollbackPane::new().render_with_scratch_and_selection_boundaries(
            area,
            &mut buffer,
            &state,
            &mut scratch,
        );
        assert!(!rendered.selection_boundaries.is_empty());

        let model = rendered.output.selection_model;
        let (entry_idx, range_id, first_line_idx, last_line_idx, last_width) = {
            let range = model
                .ranges
                .iter()
                .find(|range| !range.lines.is_empty())
                .expect("visible Edit path range");
            let first = range.lines.first().expect("first path row");
            let last = range.lines.last().expect("last path row");
            (
                range.entry_idx,
                range.range_id,
                first.block_line_idx,
                last.block_line_idx,
                last.selectable_cols
                    .end
                    .saturating_sub(last.selectable_cols.start),
            )
        };
        let mut agent = make_agent();
        agent.update_scrollback_selection_state(model, rendered.selection_boundaries);
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx,
                range_id,
                block_line_idx: first_line_idx,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx,
                range_id,
                block_line_idx: last_line_idx,
                col_within_range: last_width.saturating_sub(1),
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };
        assert_eq!(
            agent.reconstruct_drag_copy(&drag),
            Some(("   foo.rs   ".to_string(), SelectionKind::Linear))
        );
        let full_line_copies: Vec<String> = agent
            .last_scrollback_selection_model
            .range(entry_idx, range_id)
            .expect("installed Edit range")
            .lines
            .iter()
            .filter_map(|line| {
                agent.selection_text_for_full_line(&RangeHit {
                    entry_idx: line.entry_idx,
                    range_id: line.range_id,
                    block_line_idx: line.block_line_idx,
                    col_within_range: 0,
                })
            })
            .collect();
        assert!(
            full_line_copies.iter().any(|text| text.starts_with("   ")),
            "prefix boundary did not reach full-line copy: {full_line_copies:?}"
        );
        assert!(
            full_line_copies.iter().any(|text| text.ends_with("   ")),
            "suffix boundary did not reach full-line copy: {full_line_copies:?}"
        );
    }

    #[test]
    fn active_child_copy_uses_child_scrollback_cwd() {
        use crate::scrollback::block::RenderBlock;
        use crate::scrollback::render::ScratchBuffer;
        use crate::scrollback::scrollback_pane::ScrollbackPane;
        use crate::scrollback::types::{DisplayMode, derive_selection_text};
        use ratatui::buffer::Buffer;

        let parent_cwd = std::path::PathBuf::from("/parent/worktree");
        let child_cwd = std::path::PathBuf::from("/child/worktree");
        let mut child = make_agent();
        child.session.cwd = child_cwd.clone();
        child.scrollback.set_cwd(Some(child_cwd.clone()));
        let read = child.scrollback.push_block(RenderBlock::read(
            child_cwd.join("src/lib.rs").to_string_lossy(),
            None,
        ));
        child
            .scrollback
            .get_by_id_mut(read)
            .expect("Read entry")
            .set_display_mode(DisplayMode::Expanded);

        let area = Rect::new(0, 0, 40, 8);
        child.scrollback.prepare_layout(area.width, area.height);
        let mut buffer = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        let rendered = ScrollbackPane::new().render_with_scratch_and_selection_boundaries(
            area,
            &mut buffer,
            &child.scrollback,
            &mut scratch,
        );
        let line = rendered
            .output
            .selection_model
            .ranges
            .iter()
            .flat_map(|range| &range.lines)
            .find(|line| line.text == "src/lib.rs")
            .expect("child-relative Read header")
            .clone();
        let content_width = rendered
            .output
            .selection_model
            .visible_block_content_width(line.entry_idx)
            .expect("visible child block width");

        let mut parent = make_agent();
        parent.session.cwd = parent_cwd;
        parent.update_scrollback_selection_state(
            rendered.output.selection_model,
            rendered.selection_boundaries,
        );
        let child_id = "child".to_string();
        parent
            .subagent_views
            .insert(child_id.clone(), Box::new(child));
        parent.active_subagent = Some(child_id.clone());

        let source_text = parent
            .with_entry_output_text_source(
                line.entry_idx,
                line.range_id,
                Some(content_width),
                |source| source(line.block_line_idx),
            )
            .flatten();
        assert_eq!(source_text.as_deref(), Some("src/lib.rs"));
        {
            let child = parent.subagent_views.get(&child_id).expect("active child");
            let entry = child.scrollback.get(0).expect("child Read entry");
            let cached = entry.cached_output_ref();
            assert_eq!(
                derive_selection_text(&cached.lines[line.block_line_idx]),
                "src/lib.rs",
                "copy helper must not rebuild the child cache against parent cwd"
            );
        }

        let path_width = line
            .selectable_cols
            .end
            .saturating_sub(line.selectable_cols.start);
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: line.entry_idx,
                range_id: line.range_id,
                block_line_idx: line.block_line_idx,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: line.entry_idx,
                range_id: line.range_id,
                block_line_idx: line.block_line_idx,
                col_within_range: path_width.saturating_sub(1),
            },
            kind: SelectionKind::Linear,
            anchor_content_width: Some(content_width),
        };
        assert_eq!(
            parent.reconstruct_drag_copy(&drag),
            Some(("src/lib.rs".to_string(), SelectionKind::Linear))
        );
    }

    /// Regression: a table drag whose table copy can't run must return and
    /// persist `Linear` (and drop the side-car) to match the linear text
    /// that was copied.
    #[test]
    fn table_drag_falling_back_to_linear_copy_persists_linear_kind() {
        let mut agent = agent_with_visible_table_lines_only();
        let drag = table_drag();

        let (text, kind) = agent
            .reconstruct_drag_copy(&drag)
            .expect("linear fallback must still produce text");
        assert!(
            text.contains('│'),
            "fallback is the linear row sweep (border glyphs included): {text:?}"
        );
        assert_eq!(kind, SelectionKind::Linear);

        agent.persist_drag_selection(&drag, kind);
        let sel = agent.persistent_text_selection.expect("selection persists");
        assert_eq!(
            sel.kind,
            SelectionKind::Linear,
            "highlight must mirror the linear copy, not the drag's table shape"
        );
        assert!(
            agent.table_selection_geometry.is_none(),
            "side-car geometry of the abandoned cell selection must be dropped"
        );
    }

    /// When the table-aware copy did produce the text, the drag's table kind
    /// persists and the side-car geometry stays for the overlay to consume.
    #[test]
    fn table_copy_success_keeps_table_kind_and_side_car() {
        let mut agent = agent_with_visible_table_lines_only();
        let drag = table_drag();

        agent.persist_drag_selection(&drag, drag.kind);
        let sel = agent.persistent_text_selection.expect("selection persists");
        assert_eq!(sel.kind, drag.kind);
        assert!(agent.table_selection_geometry.is_some());
    }

    /// Run one double-click gesture (click + click, 100ms apart) at `t` on
    /// `idx`, threading `last_click` the way the mouse caller does. Returns
    /// the tip flag of the second click.
    fn double_click_gesture(agent: &mut AgentView, t: Instant, idx: usize) -> bool {
        let (last, tip1) = agent.handle_scrollback_click(t, idx, false);
        assert!(!tip1, "a single click must never tip");
        agent.last_click = last;
        let (last, tip2) =
            agent.handle_scrollback_click(t + Duration::from_millis(100), idx, false);
        agent.last_click = last;
        tip2
    }

    #[test]
    fn workflow_double_click_opens_matching_run_id_and_closes_goal_detail() {
        use crate::scrollback::blocks::WorkflowBlock;
        use crate::views::workflows::WorkflowRunSnapshot;

        let run = |run_id: &str| WorkflowRunSnapshot {
            run_id: run_id.to_owned(),
            name: "same-display-name".to_owned(),
            objective: "objective".to_owned(),
            status: "active".to_owned(),
            management_available: true,
            builtin: false,
            phases: Vec::new(),
            current_phase: None,
            agents: Vec::new(),
            agent_budget: None,
            agents_used: 0,
            agents_reserved: 0,
            agents_remaining: None,
            agent_usage_incomplete: false,
            active_agents: 0,
            elapsed_ms: 0,
            received_at: Instant::now(),
            pause_message: None,
            result_summary: None,
        };
        let mut agent = make_agent();
        agent.workflow_runs = vec![run("wf_other"), run("wf_target")];
        agent.show_goal_detail = true;
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::Workflow(
                WorkflowBlock::started("wf_target", "same-display-name", "objective"),
            ));

        assert!(!double_click_gesture(&mut agent, Instant::now(), 0));

        assert!(agent.show_workflows);
        assert!(!agent.show_goal_detail);
        assert_eq!(
            agent.workflows_view.selected_run_id.as_deref(),
            Some("wf_target")
        );
        assert_eq!(
            agent.workflows_view.detail_run_id.as_deref(),
            Some("wf_target")
        );
    }

    /// The word-select tip needs a REPEATED double-click on assistant text:
    /// the first gesture is treated as intentional folding; only a second
    /// gesture inside the repeat window tips. Fold-affordance surfaces
    /// (tool rows etc.) never tip and never arm the probe.
    #[test]
    fn word_select_tip_requires_repeated_double_click_on_assistant_text() {
        use crate::appearance::TextSelection;
        crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);

        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::agent_message(
                "assistant words to select",
            ));
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::tool_call(
                "Tool", "info", true,
            ));
        agent.scrollback.prepare_layout(80, 40);

        let t0 = Instant::now();
        assert!(
            !double_click_gesture(&mut agent, t0, 0),
            "first double-click gesture is intentional folding — no tip"
        );
        // Second, separate gesture (past the 300ms multi-click timeout,
        // inside the 10s repeat window) → the repeated-attempt signal.
        let t1 = t0 + Duration::from_secs(1);
        assert!(
            double_click_gesture(&mut agent, t1, 0),
            "repeated double-click on assistant text must tip"
        );

        // Tool-call rows are fold affordances: repeated gestures never tip
        // and must not arm the probe for a later assistant-text click.
        agent.last_word_select_probe = None;
        let t2 = t1 + Duration::from_secs(2);
        assert!(!double_click_gesture(&mut agent, t2, 1));
        let t3 = t2 + Duration::from_secs(1);
        assert!(!double_click_gesture(&mut agent, t3, 1));
        assert!(
            agent.last_word_select_probe.is_none(),
            "fold-affordance double-clicks must not arm the probe"
        );

        // A stale probe outside the window does not tip, but re-arms.
        let t4 = t3 + Duration::from_secs(1);
        agent.last_word_select_probe = Some(t4 - Duration::from_secs(11));
        assert!(
            !double_click_gesture(&mut agent, t4, 0),
            "probe outside the repeat window must not tip"
        );
        let t5 = t4 + Duration::from_secs(1);
        assert!(
            double_click_gesture(&mut agent, t5, 0),
            "the expired attempt still re-arms for the next gesture"
        );
    }

    // -----------------------------------------------------------------------
    // reclamp_drag_head_post_render tests
    // -----------------------------------------------------------------------

    fn mouse_down(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
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

    /// A one-range model whose lines `first_bl..first_bl+count` sit on
    /// consecutive rows starting at `first_y` — model B of a reclamp test is
    /// the same range with shifted indices/rows (a scrolled frame).
    fn stacked_lines_model(first_bl: usize, first_y: u16, count: usize) -> ResolvedSelectionModel {
        let mut model = ResolvedSelectionModel::default();
        for i in 0..count {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: first_bl + i,
                screen_y: first_y + i as u16,
                screen_x: 0,
                selectable_cols: 0..40,
                text: format!("stacked line {}", first_bl + i),
                painted_region: None,
                joiner_to_previous: (first_bl + i > 0).then(|| "\n".to_string()),
            });
        }
        model
    }

    /// Down→Drag→Drag through `handle_input` on `stacked_lines_model(0, 5, 3)`
    /// so the drag is genuinely promoted and `last_drag_mouse` is (10, 7).
    fn latch_drag_with_mouse_held(agent: &mut AgentView, reg: &ActionRegistry) {
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 24);
        agent.active_pane = AgentPane::Scrollback;
        agent.last_scrollback_selection_model = stacked_lines_model(0, 5, 3);
        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 5)), reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 6)), reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 7)), reg);
        let drag = agent.drag_selection.expect("drag promoted");
        assert_eq!(drag.anchor.block_line_idx, 0, "setup: anchor on line 0");
        assert_eq!(drag.head.block_line_idx, 2, "setup: head on line 2");
        assert_eq!(agent.last_drag_mouse, Some((10, 7)), "setup: pointer held");
    }

    /// After a scroll the rebuilt model puts different lines under the held
    /// pointer; the post-render reclamp must move the head there.
    #[test]
    fn reclamp_follows_held_pointer_onto_rebuilt_model() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        latch_drag_with_mouse_held(&mut agent, &reg);

        // Scrolled down by two rows: lines 2..=6 now occupy rows 5..=9.
        agent.last_scrollback_selection_model = stacked_lines_model(2, 5, 5);
        agent.reclamp_drag_head_post_render(false);

        let drag = agent.drag_selection.expect("drag still active");
        assert_eq!(
            drag.head.block_line_idx, 4,
            "head must re-resolve to the line now under the pointer row"
        );
        assert_eq!(drag.head.col_within_range, 10);
        assert_eq!(drag.anchor.block_line_idx, 0, "anchor never reclamps");
    }

    /// Promotion stores the pointer, so a promote-then-hold-still drag is
    /// reclamped as soon as the next frame rebuilds the model (e.g. a wheel
    /// scroll between mouse-down and the promoting move).
    #[test]
    fn promotion_stores_pointer_for_reclamp() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 24);
        agent.active_pane = AgentPane::Scrollback;
        agent.last_scrollback_selection_model = stacked_lines_model(0, 5, 3);
        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 5)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 6)), &reg);
        assert!(agent.drag_selection.is_some(), "setup: drag promoted");
        assert_eq!(
            agent.last_drag_mouse,
            Some((10, 6)),
            "promotion must store the pointer"
        );

        // No further motion: the rebuilt (scrolled) model alone must move
        // the head to the line now under the held pointer.
        agent.last_scrollback_selection_model = stacked_lines_model(2, 5, 5);
        agent.reclamp_drag_head_post_render(false);

        let drag = agent.drag_selection.expect("drag still active");
        assert_eq!(drag.head.block_line_idx, 3);
    }

    /// Guard: without an active drag the reclamp must not fabricate one.
    #[test]
    fn reclamp_noop_without_active_drag() {
        let mut agent = make_agent();
        agent.last_scrollback_selection_model = stacked_lines_model(0, 5, 3);
        agent.last_drag_mouse = Some((10, 6));

        agent.reclamp_drag_head_post_render(false);

        assert!(agent.drag_selection.is_none());
    }

    /// Guard: without a held-pointer position (cleared by finish/recovery
    /// paths) there is nothing to re-resolve against, so the head stays put.
    #[test]
    fn reclamp_noop_without_last_drag_mouse() {
        let mut agent = make_agent();
        let anchor = RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            col_within_range: 2,
        };
        agent.drag_selection = Some(ActiveTextDrag {
            anchor,
            head: RangeHit {
                block_line_idx: 1,
                ..anchor
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        });
        agent.last_drag_mouse = None;
        agent.last_scrollback_selection_model = stacked_lines_model(0, 5, 3);

        agent.reclamp_drag_head_post_render(false);

        let drag = agent.drag_selection.expect("drag still active");
        assert_eq!(
            drag.head.block_line_idx, 1,
            "head untouched without pointer"
        );
    }

    /// A btw-anchored drag reclamps only on the btw rebuild and only against
    /// the btw model: the scrollback rebuild is gated off, scrollback lines
    /// under the pointer never capture its head, and a btw model miss keeps
    /// the previous head.
    #[test]
    fn reclamp_btw_drag_gated_to_btw_rebuild_and_model() {
        let mut agent = make_agent();
        let btw_anchor = RangeHit {
            entry_idx: crate::views::btw_overlay::BTW_OVERLAY_ENTRY_IDX,
            range_id: 0,
            block_line_idx: 0,
            col_within_range: 0,
        };
        agent.drag_selection = Some(ActiveTextDrag {
            anchor: btw_anchor,
            head: btw_anchor,
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        });
        agent.last_drag_mouse = Some((10, 6));
        // Scrollback model has a hit under the pointer; btw model is empty.
        agent.last_scrollback_selection_model = stacked_lines_model(0, 5, 3);
        agent.last_btw_selection_model = ResolvedSelectionModel::default();

        agent.reclamp_drag_head_post_render(false);
        agent.reclamp_drag_head_post_render(true);

        let drag = agent.drag_selection.expect("drag still active");
        assert_eq!(drag.head, btw_anchor, "btw head must not follow scrollback");

        // Once the btw model has lines, only the btw rebuild moves the head.
        let mut btw_model = ResolvedSelectionModel::default();
        btw_model.push_line(ResolvedSelectableLine {
            entry_idx: BTW_OVERLAY_ENTRY_IDX,
            range_id: 0,
            block_line_idx: 3,
            screen_y: 6,
            screen_x: 0,
            selectable_cols: 0..40,
            text: "btw line".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });
        agent.last_btw_selection_model = btw_model;

        agent.reclamp_drag_head_post_render(false);
        let drag = agent.drag_selection.expect("drag still active");
        assert_eq!(drag.head, btw_anchor, "scrollback rebuild is gated off");

        agent.reclamp_drag_head_post_render(true);
        let drag = agent.drag_selection.expect("drag still active");
        assert_eq!(drag.head.block_line_idx, 3, "btw rebuild moves the head");
    }

    // -----------------------------------------------------------------------
    // anchor_content_width snapshot tests
    // -----------------------------------------------------------------------

    /// The linear copy resolves the anchor entry's lines with the drag-start
    /// width snapshot when the block is gone from `visible_blocks` (scrolled
    /// fully out before mouse-up); without the snapshot that copy fails.
    #[test]
    fn reconstruct_drag_copy_uses_width_snapshot_when_anchor_block_scrolled_out() {
        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::agent_message(
                "SNAPWIDTH alpha beta",
            ));
        // The rebuilt frame no longer contains the anchor block at all.
        agent.last_scrollback_selection_model = ResolvedSelectionModel::default();

        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                col_within_range: 8,
            },
            kind: SelectionKind::Linear,
            anchor_content_width: Some(60),
        };

        let (text, kind) = agent
            .reconstruct_drag_copy(&drag)
            .expect("snapshot width must reach the full-output reconstruction");
        assert_eq!(text, "SNAPWIDTH");
        assert_eq!(kind, SelectionKind::Linear);

        // Same drag without a snapshot: the mouse-up-time lookup misses and
        // the visible-model fallback has nothing.
        let no_snapshot = ActiveTextDrag {
            anchor_content_width: None,
            ..drag
        };
        assert!(agent.reconstruct_drag_copy(&no_snapshot).is_none());
    }

    /// Mouse-down on a selectable line snapshots the anchor block's render
    /// width and promotion carries it onto the active drag.
    #[test]
    fn drag_promotion_carries_anchor_width_snapshot() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 24);
        agent.active_pane = AgentPane::Scrollback;
        let mut model = stacked_lines_model(0, 5, 3);
        model.visible_blocks.push(VisibleBlockGeometry {
            entry_idx: 0,
            area: Rect::new(0, 5, 46, 3),
            content_area: Rect::new(0, 5, 46, 3),
            selection_area: Rect::new(0, 5, 46, 3),
            content_width: 46,
            top_clipped: false,
            bottom_clipped: false,
            drag_startable: true,
        });
        agent.last_scrollback_selection_model = model;

        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 5)), &reg);
        assert_eq!(
            agent.pending_text_drag.and_then(|p| p.anchor_content_width),
            Some(46)
        );
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 6)), &reg);
        assert_eq!(
            agent.drag_selection.and_then(|d| d.anchor_content_width),
            Some(46)
        );
    }

    /// Resolver miss at promotion (the anchor's range vanished from the
    /// frame between press and threshold): the head collapses to the
    /// anchor — the live successor of the deleted clamp-to-anchor helper.
    #[test]
    fn promotion_miss_collapses_head_to_anchor() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 24);
        agent.active_pane = AgentPane::Scrollback;
        agent.last_scrollback_selection_model = stacked_lines_model(0, 5, 3);
        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 5)), &reg);
        assert!(agent.pending_text_drag.is_some(), "setup: pending armed");

        // The rebuilt frame lost the range entirely.
        agent.last_scrollback_selection_model = ResolvedSelectionModel::default();
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 7)), &reg);

        let drag = agent.drag_selection.expect("promoted despite the miss");
        assert_eq!(drag.head, drag.anchor, "head collapsed to the anchor");
    }

    /// Resolver miss mid-drag (the range scrolled fully out): the head
    /// keeps its previous position instead of jumping — the live successor
    /// of the deleted keep-previous-head helper.
    #[test]
    fn active_drag_motion_miss_keeps_previous_head() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 24);
        agent.active_pane = AgentPane::Scrollback;
        agent.last_scrollback_selection_model = stacked_lines_model(0, 5, 3);
        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 5)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 7)), &reg);
        let head_before = agent.drag_selection.expect("setup: active").head;
        assert_eq!(head_before.block_line_idx, 2, "setup: head extended");

        agent.last_scrollback_selection_model = ResolvedSelectionModel::default();
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(30, 9)), &reg);

        let drag = agent.drag_selection.expect("still active");
        assert_eq!(drag.head, head_before, "head kept across the miss");
    }

    /// The TABLE-shaped copy also survives full scroll-out via the width
    /// snapshot: the side-car geometry frozen while the block was visible
    /// re-detects against the snapshot-width output and the cell text is
    /// copied, with no `visible_blocks` entry at mouse-up. Without the
    /// snapshot the whole copy fails.
    #[test]
    fn table_copy_uses_width_snapshot_when_anchor_block_scrolled_out() {
        const WIDTH: u16 = 40;
        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::agent_message(
                "| Name | Role |\n|------|------|\n| Alice | Eng |",
            ));

        // Freeze the side-car the way promotion does, from the entry's own
        // rendering at the snapshot width (probe scan finds a grid line).
        let probe_geometry = (0..6)
            .find_map(|probe| {
                agent
                    .with_entry_output_text_source(0, 0, Some(WIDTH), |src| {
                        TableGeometry::detect(src, probe)
                    })
                    .flatten()
            })
            .expect("markdown table renders a detectable grid");
        let anchor_line = probe_geometry.row_lines(0).start;
        let anchor_col = probe_geometry.band(0).start;
        // Head sweeps to the cell's far corner so the copied span covers the
        // whole cell, not a single padding column.
        let head_line = probe_geometry.row_lines(0).end.saturating_sub(1);
        let head_col = probe_geometry.band(0).end.saturating_sub(1);
        // Re-detect at the anchor line: the copy path requires the side-car
        // to exactly match its own re-detect there.
        let geometry = agent
            .with_entry_output_text_source(0, 0, Some(WIDTH), |src| {
                TableGeometry::detect(src, anchor_line)
            })
            .flatten()
            .expect("grid detected at the anchor line");
        agent.table_selection_geometry = Some(TableSelectionGeometry {
            entry_idx: 0,
            range_id: 0,
            geometry,
        });
        // The rebuilt frame no longer contains the anchor block at all.
        agent.last_scrollback_selection_model = ResolvedSelectionModel::default();

        let anchor = RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: anchor_line,
            col_within_range: anchor_col,
        };
        let drag = ActiveTextDrag {
            anchor,
            head: RangeHit {
                block_line_idx: head_line,
                col_within_range: head_col,
                ..anchor
            },
            kind: SelectionKind::TableCell,
            anchor_content_width: Some(WIDTH),
        };

        let (text, kind) = agent
            .reconstruct_drag_copy(&drag)
            .expect("snapshot width must keep the table copy alive");
        assert_eq!(kind, SelectionKind::TableCell);
        assert!(text.contains("Name"), "cell content copied: {text:?}");
        assert!(!text.contains('│'), "no border glyphs: {text:?}");
        assert!(!text.contains("Role"), "band-clamped to one cell: {text:?}");

        // Without the snapshot neither the table nor the linear path has a
        // width, so the copy fails outright.
        let no_snapshot = ActiveTextDrag {
            anchor_content_width: None,
            ..drag
        };
        assert!(agent.reconstruct_drag_copy(&no_snapshot).is_none());
    }

    /// The btw copy prefers the drag-start width snapshot; the current
    /// panel-area fallback only applies without one (here the default area
    /// makes the fallback width 0, so only the snapshot path can produce
    /// the text).
    #[test]
    fn btw_copy_prefers_width_snapshot_over_panel_fallback() {
        let mut agent = make_agent();
        agent.btw_state = Some(crate::views::btw_overlay::BtwOverlayState::done(
            "q".to_string(),
            "BTWSNAP alpha beta".to_string(),
        ));

        let anchor = RangeHit {
            entry_idx: BTW_OVERLAY_ENTRY_IDX,
            range_id: 0,
            block_line_idx: 0,
            col_within_range: 0,
        };
        let drag = ActiveTextDrag {
            anchor,
            head: RangeHit {
                col_within_range: 6,
                ..anchor
            },
            kind: SelectionKind::Linear,
            anchor_content_width: Some(30),
        };

        let (text, kind) = agent
            .reconstruct_drag_copy(&drag)
            .expect("snapshot width must reach the btw full model");
        assert_eq!(text, "BTWSNAP");
        assert_eq!(kind, SelectionKind::Linear);

        let no_snapshot = ActiveTextDrag {
            anchor_content_width: None,
            ..drag
        };
        assert!(agent.reconstruct_drag_copy(&no_snapshot).is_none());
    }

    // -----------------------------------------------------------------------
    // deferred text-press (anchor on entry into text) tests
    // -----------------------------------------------------------------------

    fn mouse_up(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }

    /// Two message entries with chrome rows and a dead gap between them:
    /// entry 0 area rows 4-6 (row 4 chrome, text on rows 5-6, width 46),
    /// rows 7-8 dead gap, entry 1 area rows 9-10 (row 9 chrome, text on
    /// row 10, width 52).
    fn agent_with_chrome_and_gap() -> AgentView {
        let mut agent = make_agent();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 24);
        agent.active_pane = AgentPane::Scrollback;
        let mut model = ResolvedSelectionModel::default();
        for (bl, y) in [(0usize, 5u16), (1, 6)] {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: bl,
                screen_y: y,
                screen_x: 0,
                selectable_cols: 0..20,
                text: format!("entry zero line {bl}"),
                painted_region: None,
                joiner_to_previous: (bl > 0).then(|| "\n".to_string()),
            });
        }
        model.push_line(ResolvedSelectableLine {
            entry_idx: 1,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 10,
            screen_x: 0,
            selectable_cols: 0..20,
            text: "entry one line".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });
        let block = |entry_idx: usize, area: Rect, content_width: u16| VisibleBlockGeometry {
            entry_idx,
            area,
            content_area: area,
            selection_area: area,
            content_width,
            top_clipped: false,
            bottom_clipped: false,
            drag_startable: true,
        };
        model
            .visible_blocks
            .push(block(0, Rect::new(0, 4, 46, 3), 46));
        model
            .visible_blocks
            .push(block(1, Rect::new(0, 9, 52, 2), 52));
        agent.last_scrollback_selection_model = model;
        agent
    }

    /// A chrome press arms the block drag AND the anchor-less latch; motion
    /// that stays on chrome/gap rows promotes and extends the block drag in
    /// both directions exactly as before, with the latch armed but idle.
    #[test]
    fn chrome_press_drag_within_chrome_is_block_drag_unchanged() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 4)), &reg);
        assert_eq!(agent.deferred_text_press, Some((6, 4)));
        let pending = agent.pending_block_drag.expect("block drag pends");
        assert_eq!(pending.anchor_entry_idx, 0);
        assert!(agent.pending_text_drag.is_none());

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(6, 9)), &reg);
        let drag = agent.block_drag_selection.expect("block drag promoted");
        assert_eq!((drag.anchor_entry_idx, drag.head_entry_idx), (0, 1));

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(6, 4)), &reg);
        let drag = agent.block_drag_selection.expect("still a block drag");
        assert_eq!((drag.anchor_entry_idx, drag.head_entry_idx), (0, 0));
        assert!(agent.drag_selection.is_none());
        assert_eq!(agent.deferred_text_press, Some((6, 4)), "latch stays armed");
    }

    /// A chrome press whose drag enters text converts to a text drag
    /// anchored at the ENTRY position (not the press, not nearest-to-press)
    /// with the entry block's width snapshot, and cancels the block drag
    /// already in flight.
    #[test]
    fn chrome_press_drag_into_text_converts_at_entry_point() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 4)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 9)), &reg);
        assert!(agent.block_drag_selection.is_some(), "setup: block active");

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(10, 10)), &reg);

        let drag = agent.drag_selection.expect("converted to text drag");
        let entry_hit = RangeHit {
            entry_idx: 1,
            range_id: 0,
            block_line_idx: 0,
            col_within_range: 10,
        };
        assert_eq!(drag.anchor, entry_hit, "anchor at the entry position");
        assert_eq!(drag.head, entry_hit);
        assert_eq!(drag.anchor_content_width, Some(52), "entry block's width");
        assert_eq!(drag.kind, SelectionKind::Linear);
        assert!(agent.block_drag_selection.is_none(), "block drag cancelled");
        assert!(agent.pending_block_drag.is_none());
        assert!(agent.deferred_text_press.is_none(), "latch consumed");
    }

    /// A press on a dead gap row (no block area, so nothing block-armed)
    /// stays anchor-less through further dead rows and anchors at the exact
    /// row/col where the pointer first enters text.
    #[test]
    fn gap_press_drag_through_gap_anchors_at_entry_row_and_col() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 7)), &reg);
        assert_eq!(agent.deferred_text_press, Some((6, 7)));
        assert!(agent.pending_block_drag.is_none(), "gap is outside areas");

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(6, 8)), &reg);
        assert!(agent.drag_selection.is_none(), "still dead: no conversion");
        assert!(agent.block_drag_selection.is_none());

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(14, 6)), &reg);
        let drag = agent.drag_selection.expect("converted on text entry");
        assert_eq!(
            drag.anchor,
            RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 1,
                col_within_range: 14,
            },
            "anchored at the entry row/col"
        );
        assert_eq!(drag.anchor_content_width, Some(46));
    }

    /// Press + release with no motion is a plain click: no selection of any
    /// kind, and the click cascade still consumes the press.
    #[test]
    fn deferred_press_release_without_motion_is_plain_click() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 7)), &reg);
        assert!(
            agent.pending_scrollback_click.is_some(),
            "setup: click pends"
        );

        let _ = agent.handle_input(&Event::Mouse(mouse_up(6, 7)), &reg);

        assert!(agent.deferred_text_press.is_none());
        assert!(agent.drag_selection.is_none());
        assert!(agent.block_drag_selection.is_none());
        assert!(agent.persistent_text_selection.is_none());
        assert!(
            agent.pending_scrollback_click.is_none(),
            "the click path consumed the press"
        );
    }

    /// A gesture that never enters text finishes as a whole-block copy,
    /// exactly as without the latch (payload content pinned by e2e).
    #[test]
    fn deferred_press_never_entering_text_finishes_block_copy() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::agent_message(
                "BLOCKCOPY zero",
            ));
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::agent_message(
                "BLOCKCOPY one",
            ));

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 4)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(6, 9)), &reg);
        assert!(agent.block_drag_selection.is_some(), "setup: block active");

        let outcome = agent.handle_input(&Event::Mouse(mouse_up(6, 9)), &reg);

        assert!(
            matches!(outcome, InputOutcome::Changed),
            "block copy finished"
        );
        assert!(agent.block_drag_selection.is_none());
        assert!(agent.drag_selection.is_none());
        assert!(agent.persistent_text_selection.is_none());
        assert!(agent.deferred_text_press.is_none());
    }

    /// Conversion is one-way: once the gesture is a text drag, motion back
    /// over chrome/gap rows keeps extending it (the pre-existing head rule:
    /// nearest line within the anchor's range) and never re-arms block drag.
    #[test]
    fn converted_drag_stays_text_over_chrome_and_gap() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 7)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(14, 6)), &reg);
        assert!(agent.drag_selection.is_some(), "setup: converted");

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(6, 4)), &reg);
        let drag = agent.drag_selection.expect("still a text drag");
        assert_eq!(drag.head.block_line_idx, 0, "head snapped within range");
        assert_eq!(drag.head.col_within_range, 6);
        assert!(agent.block_drag_selection.is_none(), "no block re-arm");
        assert!(agent.pending_block_drag.is_none());

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(6, 9)), &reg);
        let drag = agent.drag_selection.expect("still a text drag");
        assert_eq!(drag.head.block_line_idx, 1, "head follows nearest line");
        assert_eq!(drag.anchor.entry_idx, 0, "anchor pinned to entry 0");
    }

    /// The latch clears on the recovery path and on the stale-latch guard
    /// (any non-drag event, e.g. Esc, while latched).
    #[test]
    fn deferred_latch_cleared_on_recovery_and_stale_event() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 7)), &reg);
        assert!(agent.scrollback_drag_latched(), "latch counts as a drag");
        agent.clear_stuck_scrollback_drag();
        assert!(agent.deferred_text_press.is_none());

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 7)), &reg);
        assert!(agent.deferred_text_press.is_some());
        let esc = Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ));
        let _ = agent.handle_input(&esc, &reg);
        assert!(
            agent.deferred_text_press.is_none(),
            "stale-latch guard cleared the deferred press"
        );
    }

    /// The chrome-and-gap agent with the panes shrunk so rows 16-19 form the
    /// passive strip band between the scrollback pane (rows 0-15) and the
    /// prompt box (rows 20-22) — turn status / banner / gap-row territory.
    fn agent_with_above_prompt_strip() -> AgentView {
        let mut agent = agent_with_chrome_and_gap();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 16);
        agent.pane_areas.prompt = Rect::new(0, 20, 80, 3);
        agent
    }

    /// A press on the strip band arms ONLY the anchor-less latch: no block
    /// or text drag pends, and no click latch is set (release keeps doing
    /// nothing, as the band did before).
    #[test]
    fn strip_press_arms_deferred_latch_only() {
        let mut agent = agent_with_above_prompt_strip();
        let reg = ActionRegistry::defaults();

        let outcome = agent.handle_input(&Event::Mouse(mouse_down(10, 17)), &reg);

        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.deferred_text_press, Some((10, 17)));
        assert!(agent.pending_block_drag.is_none());
        assert!(agent.pending_text_drag.is_none());
        assert!(agent.pending_scrollback_click.is_none(), "no click latch");
    }

    /// Dragging from the strip band up into message text converts at the
    /// entry point, exactly like the in-pane deferred press.
    #[test]
    fn strip_press_drag_into_text_converts_at_entry_point() {
        let mut agent = agent_with_above_prompt_strip();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(10, 17)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(14, 6)), &reg);

        let drag = agent.drag_selection.expect("converted on text entry");
        assert_eq!(
            drag.anchor,
            RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 1,
                col_within_range: 14,
            },
            "anchored at the entry row/col"
        );
        assert_eq!(drag.anchor_content_width, Some(46));
        assert!(agent.deferred_text_press.is_none(), "latch consumed");
    }

    /// Press + release on the strip without motion does what the band did
    /// before the latch existed: nothing.
    #[test]
    fn strip_press_release_without_motion_does_nothing() {
        let mut agent = agent_with_above_prompt_strip();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(10, 17)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_up(10, 17)), &reg);

        assert!(agent.deferred_text_press.is_none());
        assert!(agent.drag_selection.is_none());
        assert!(agent.block_drag_selection.is_none());
        assert!(agent.persistent_text_selection.is_none());
        assert_eq!(agent.scrollback.selected(), None, "no entry selected");
    }

    /// A strip drag that never enters text selects nothing and leaves no
    /// state behind on release.
    #[test]
    fn strip_press_drag_never_entering_text_selects_nothing() {
        let mut agent = agent_with_above_prompt_strip();
        let reg = ActionRegistry::defaults();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(10, 17)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(30, 18)), &reg);
        assert!(agent.drag_selection.is_none(), "no text under the pointer");

        let _ = agent.handle_input(&Event::Mouse(mouse_up(30, 18)), &reg);

        assert!(agent.deferred_text_press.is_none());
        assert!(agent.drag_selection.is_none());
        assert!(agent.persistent_text_selection.is_none());
    }

    /// Interactive controls in and around the band never arm the latch:
    /// the scrollbar and the turn-status cancel button consume their press
    /// first, and the prompt box routes to the prompt pane.
    #[test]
    fn interactive_rows_do_not_arm_deferred_latch() {
        let reg = ActionRegistry::defaults();

        let mut agent = agent_with_above_prompt_strip();
        agent.hit_scrollbar.set(Some(Rect::new(79, 0, 1, 20)));
        let _ = agent.handle_input(&Event::Mouse(mouse_down(79, 17)), &reg);
        assert!(agent.scrollbar_dragging, "scrollbar owns the press");
        assert!(agent.deferred_text_press.is_none());

        let mut agent = agent_with_above_prompt_strip();
        agent.hit_cancel_button.set(Some(Rect::new(30, 17, 6, 1)));
        let outcome = agent.handle_input(&Event::Mouse(mouse_down(32, 17)), &reg);
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(crate::app::actions::Action::CancelTurn)
            ),
            "cancel button owns the press"
        );
        assert!(agent.deferred_text_press.is_none());

        let mut agent = agent_with_above_prompt_strip();
        let _ = agent.handle_input(&Event::Mouse(mouse_down(5, 21)), &reg);
        assert!(agent.deferred_text_press.is_none(), "prompt is a pane");
    }

    /// With the block viewer open the band must not arm: the modal owns the
    /// screen while the scrollback model beneath keeps rebuilding, so an
    /// armed latch would convert on (and copy) text hidden under it.
    #[test]
    fn strip_press_with_block_viewer_open_arms_nothing() {
        let mut agent = agent_with_above_prompt_strip();
        let reg = ActionRegistry::defaults();
        agent.block_viewer = Some(crate::views::block_viewer::BlockViewerPane::for_plain_text(
            "t", "content",
        ));

        let _ = agent.handle_input(&Event::Mouse(mouse_down(10, 17)), &reg);
        assert!(agent.deferred_text_press.is_none(), "viewer owns the press");

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(14, 6)), &reg);
        assert!(agent.drag_selection.is_none(), "nothing converts");
        assert!(agent.persistent_text_selection.is_none());
    }

    /// A strip press keeps focus where it was (deliberate), so with the
    /// prompt focused the un-converted motion crosses the prompt box — the
    /// armed gesture must own that motion: nothing may leak into the prompt
    /// handlers, and the gesture must still convert once it reaches text.
    #[test]
    fn strip_gesture_does_not_leak_motion_into_prompt() {
        let mut agent = agent_with_above_prompt_strip();
        let reg = ActionRegistry::defaults();
        agent.active_pane = AgentPane::Prompt;
        agent.prompt.set_text("draft text");
        let cursor_before = agent.prompt.cursor();

        let _ = agent.handle_input(&Event::Mouse(mouse_down(10, 17)), &reg);
        assert!(agent.deferred_text_press.is_some(), "setup: latch armed");

        let outcome = agent.handle_input(&Event::Mouse(mouse_drag(5, 21)), &reg);
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(agent.prompt.text(), "draft text", "prompt text untouched");
        assert_eq!(agent.prompt.cursor(), cursor_before, "cursor untouched");

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(14, 6)), &reg);
        assert!(
            agent.drag_selection.is_some(),
            "conversion survives crossing the prompt region"
        );
    }

    /// A strip press replaces any held highlight immediately, exactly like
    /// the in-pane press's eager clear.
    #[test]
    fn strip_press_clears_previous_persistent_selection() {
        let mut agent = agent_with_above_prompt_strip();
        let reg = ActionRegistry::defaults();
        let anchor = RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            col_within_range: 0,
        };
        agent.persist_drag_selection(
            &ActiveTextDrag {
                anchor,
                head: RangeHit {
                    col_within_range: 5,
                    ..anchor
                },
                kind: SelectionKind::Linear,
                anchor_content_width: None,
            },
            SelectionKind::Linear,
        );
        agent.table_selection_geometry = Some(TableSelectionGeometry {
            entry_idx: 0,
            range_id: 0,
            geometry: table_geometry(),
        });
        agent.selection_created_at = Some(Instant::now());
        assert!(agent.persistent_text_selection.is_some(), "setup: held");

        let _ = agent.handle_input(&Event::Mouse(mouse_down(10, 17)), &reg);

        assert!(agent.persistent_text_selection.is_none(), "highlight gone");
        assert!(agent.table_selection_geometry.is_none());
        assert!(agent.selection_created_at.is_none());
        assert!(agent.deferred_text_press.is_some(), "latch still arms");
    }

    /// A press on a recap block's non-selectable rows rides the in-pane
    /// deferred path (recap is a scrollback entry, not a strip): dragging
    /// up into message text converts at the entry point, and a motionless
    /// press still feeds the normal scrollback click cascade.
    #[test]
    fn recap_block_press_converts_on_text_entry_and_clicks_as_before() {
        let mut agent = agent_with_chrome_and_gap();
        let reg = ActionRegistry::defaults();
        // Real entries so the click cascade has something to resolve; the
        // recap's rows 12-14 register no selectable lines here, modeling a
        // press on its non-selectable header chrome (the summary body IS
        // selection-registered in production).
        for _ in 0..2 {
            agent
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::agent_message("m"));
        }
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::session_event(
                crate::scrollback::blocks::SessionEvent::Recap {
                    summary: "did things".to_string(),
                    auto: true,
                },
            ));
        agent
            .last_scrollback_selection_model
            .visible_blocks
            .push(VisibleBlockGeometry {
                entry_idx: 2,
                area: Rect::new(0, 12, 46, 3),
                content_area: Rect::new(0, 12, 46, 3),
                selection_area: Rect::new(0, 12, 46, 3),
                content_width: 46,
                top_clipped: false,
                bottom_clipped: false,
                drag_startable: true,
            });

        let _ = agent.handle_input(&Event::Mouse(mouse_down(6, 13)), &reg);
        assert_eq!(agent.deferred_text_press, Some((6, 13)));
        assert!(
            agent.pending_scrollback_click.is_some(),
            "click still pends"
        );

        let _ = agent.handle_input(&Event::Mouse(mouse_drag(14, 6)), &reg);
        let drag = agent.drag_selection.expect("converted on text entry");
        assert_eq!(drag.anchor.entry_idx, 0);
        assert_eq!(drag.anchor.block_line_idx, 1);
        assert_eq!(drag.anchor.col_within_range, 14);
        assert!(agent.pending_block_drag.is_none(), "block drag cancelled");
        assert!(agent.block_drag_selection.is_none());

        // Motionless press + release on the recap, against the REAL layout
        // (prepare_layout populates entry_index_at_screen_row): the click
        // cascade still selects the recap entry — the latch must not eat it.
        let mut agent2 = make_agent();
        agent2.pane_areas.scrollback = Rect::new(0, 0, 80, 24);
        agent2.active_pane = AgentPane::Scrollback;
        for _ in 0..2 {
            agent2
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::agent_message("m"));
        }
        agent2
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::session_event(
                crate::scrollback::blocks::SessionEvent::Recap {
                    summary: "did things".to_string(),
                    auto: true,
                },
            ));
        agent2.scrollback.prepare_layout(80, 24);
        let (recap_area, _, _) = agent2
            .scrollback
            .entry_screen_area(2, agent2.pane_areas.scrollback)
            .expect("recap entry laid out");

        let _ = agent2.handle_input(&Event::Mouse(mouse_down(6, recap_area.y)), &reg);
        assert!(agent2.deferred_text_press.is_some(), "latch armed on recap");
        let _ = agent2.handle_input(&Event::Mouse(mouse_up(6, recap_area.y)), &reg);

        assert_eq!(
            agent2.scrollback.selected(),
            Some(2),
            "the recap click still selects the recap entry"
        );
        assert!(agent2.pending_scrollback_click.is_none(), "click consumed");
        assert!(agent2.drag_selection.is_none());
        assert!(agent2.deferred_text_press.is_none());
    }

    // -----------------------------------------------------------------------
    // drag-autoscroll bounce tests (tick + reclamp interplay)
    // -----------------------------------------------------------------------

    /// Agent over real scrollback content taller than its viewport (30
    /// one-line messages through the real layout; pane rows 0-9, prompt at
    /// rows 14-16 so rows 10-13 are the strip band), so
    /// `tick_drag_autoscroll` moves real offsets against real clamps.
    fn agent_with_tall_scrollback() -> AgentView {
        let mut agent = make_agent();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 10);
        agent.pane_areas.prompt = Rect::new(0, 14, 80, 3);
        agent.active_pane = AgentPane::Scrollback;
        for i in 0..30 {
            agent
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::agent_message(
                    format!("line {i}"),
                ));
        }
        agent.scrollback.prepare_layout(80, 10);
        agent
    }

    /// A held pointer (no motion events) in the bottom edge zone — and
    /// equally on a strip row below the pane — must scroll monotonically
    /// down and stop dead at the clamp: the direction is only ever written
    /// by motion handlers, so ticks alone can never flip it or oscillate
    /// at the boundary. The per-tick reclamp must stay a pure head snap
    /// (it never scrolls).
    #[test]
    fn held_pointer_autoscroll_is_monotonic_and_clamp_stable() {
        let mut agent = agent_with_tall_scrollback();
        let (_, viewport, total) = agent.scrollback.scroll_info();
        let max_offset = total - viewport as usize;

        // Row 9 = pane bottom edge zone; row 12 = strip band below it.
        for row in [9u16, 12] {
            agent.scrollback.scroll_up(10_000);
            agent.drag_autoscroll = compute_autoscroll(row, agent.pane_areas.scrollback);
            let armed = agent.drag_autoscroll.expect("edge zone arms autoscroll");
            assert_eq!(armed.direction, AutoScrollDirection::Down);

            let mut prev = agent.scrollback.scroll_info().0;
            let mut clamped_ticks = 0;
            for _ in 0..200 {
                agent.tick_drag_autoscroll();
                agent.reclamp_drag_head_post_render(false);
                let now = agent.scrollback.scroll_info().0;
                assert!(now >= prev, "offset regressed: {now} < {prev} (row {row})");
                clamped_ticks = if now == prev { clamped_ticks + 1 } else { 0 };
                prev = now;
                assert_eq!(
                    agent.drag_autoscroll,
                    Some(armed),
                    "ticks must never rewrite the autoscroll state"
                );
            }
            assert_eq!(prev, max_offset, "reached the bottom clamp (row {row})");
            assert!(clamped_ticks >= 100, "held flat at the clamp, no wobble");
        }

        // Above the top edge: the mirror run scrolls monotonically up and
        // holds at offset 0.
        agent.scrollback.scroll_down(10_000);
        agent.drag_autoscroll = compute_autoscroll(0, agent.pane_areas.scrollback);
        assert_eq!(
            agent.drag_autoscroll.map(|a| a.direction),
            Some(AutoScrollDirection::Up)
        );
        let mut prev = agent.scrollback.scroll_info().0;
        for _ in 0..200 {
            agent.tick_drag_autoscroll();
            agent.reclamp_drag_head_post_render(false);
            let now = agent.scrollback.scroll_info().0;
            assert!(now <= prev, "offset regressed upward: {now} > {prev}");
            prev = now;
        }
        assert_eq!(prev, 0, "held at the top clamp");
    }

    /// A drag selecting text in the pinned header must not arm scroll-up.
    #[test]
    fn autoscroll_zone_starts_below_the_sticky_header() {
        let mut agent = agent_with_tall_scrollback();
        // A 4-row sticky header: content starts at pane row 4.
        agent.last_scrollback_selection_model.content_area = Rect::new(0, 4, 80, 6);

        // Header rows are inert.
        for row in [0u16, 1, 2, 3] {
            assert!(
                agent.drag_autoscroll_for(row).is_none(),
                "row {row} is sticky-header chrome and must not autoscroll"
            );
        }
        // The content's own top rows still do.
        assert_eq!(
            agent.drag_autoscroll_for(4).map(|a| a.direction),
            Some(AutoScrollDirection::Up)
        );
        // The bottom edge is still the pane's.
        assert_eq!(
            agent.drag_autoscroll_for(9).map(|a| a.direction),
            Some(AutoScrollDirection::Down)
        );
    }

    /// Without a header (compact mode, scrolled to the top) the zone is the pane.
    #[test]
    fn autoscroll_zone_falls_back_to_the_pane_without_a_header() {
        let mut agent = agent_with_tall_scrollback();
        agent.last_scrollback_selection_model.content_area = Rect::new(0, 0, 80, 10);
        assert_eq!(
            agent.drag_autoscroll_for(0).map(|a| a.direction),
            Some(AutoScrollDirection::Up)
        );

        // An unpopulated model falls back rather than disabling autoscroll.
        agent.last_scrollback_selection_model.content_area = Rect::default();
        assert_eq!(
            agent.drag_autoscroll_for(0).map(|a| a.direction),
            Some(AutoScrollDirection::Up)
        );
        assert_eq!(
            agent.drag_autoscroll_for(9).map(|a| a.direction),
            Some(AutoScrollDirection::Down)
        );
    }

    /// A frame whose rows are all header chrome (a sticky header filling a short pane) must not arm autoscroll
    /// anywhere. The pane publishes the zero-height content rect at the header's end.
    #[test]
    fn autoscroll_stays_off_in_a_header_only_viewport() {
        let mut agent = agent_with_tall_scrollback();
        agent.last_scrollback_selection_model.content_area = Rect::new(0, 9, 80, 0);

        for row in [0u16, 5, 9] {
            assert!(
                agent.drag_autoscroll_for(row).is_none(),
                "row {row} is header chrome in a header-only viewport and must not autoscroll"
            );
        }
    }

    /// The strip conversion landing on the bottommost text row (inside the
    /// edge zone): with content already at-bottom the clamped offset must
    /// not move and the reclamped head must not wobble; with room to
    /// scroll, tick + per-frame reclamp advance offset and head
    /// monotonically — the reclamp never amplifies the scroll into
    /// oscillation.
    #[test]
    fn conversion_at_bottom_edge_ticks_without_oscillation() {
        let mut agent = agent_with_tall_scrollback();
        let reg = ActionRegistry::defaults();

        // Content at-bottom; bottom two viewport rows carry text.
        agent.scrollback.scroll_down(10_000);
        let clamped = agent.scrollback.scroll_info().0;
        agent.last_scrollback_selection_model = stacked_lines_model(0, 8, 2);

        // Real gesture: strip press, drag up onto the bottom text row
        // (conversion), then a held motion there arms Down autoscroll.
        let _ = agent.handle_input(&Event::Mouse(mouse_down(2, 12)), &reg);
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(2, 9)), &reg);
        let drag = agent.drag_selection.expect("converted on the bottom row");
        assert_eq!(drag.anchor.block_line_idx, 1, "anchored on the edge row");
        let _ = agent.handle_input(&Event::Mouse(mouse_drag(2, 9)), &reg);
        assert_eq!(
            agent.drag_autoscroll.map(|a| a.direction),
            Some(AutoScrollDirection::Down),
            "bottom-row anchor arms Down, same as main's bottom-row drags"
        );

        let head_before = agent.drag_selection.unwrap().head;
        for _ in 0..50 {
            agent.tick_drag_autoscroll();
            agent.reclamp_drag_head_post_render(false);
            assert_eq!(
                agent.scrollback.scroll_info().0,
                clamped,
                "clamped content must not move under a held pointer"
            );
            assert_eq!(
                agent.drag_selection.unwrap().head,
                head_before,
                "head must not wobble while nothing moved"
            );
        }

        // With room to scroll: rebuild the model each tick the way render
        // does (content shifted up under the held pointer) and require both
        // offset and head to advance monotonically.
        agent.scrollback.scroll_up(10_000);
        let mut prev_offset = agent.scrollback.scroll_info().0;
        let mut prev_head = 0usize;
        if let Some(ref mut d) = agent.drag_selection {
            d.anchor.block_line_idx = 0;
            d.head.block_line_idx = 0;
        }
        for _ in 0..50 {
            agent.tick_drag_autoscroll();
            let offset = agent.scrollback.scroll_info().0;
            assert!(offset >= prev_offset, "offset regressed mid-autoscroll");
            prev_offset = offset;
            // The frame under a held pointer: rows 0-9 now show block lines
            // offset..offset+10.
            agent.last_scrollback_selection_model = stacked_lines_model(offset, 0, 10);
            agent.reclamp_drag_head_post_render(false);
            let head = agent.drag_selection.unwrap().head.block_line_idx;
            assert!(head >= prev_head, "head bounced back: {head} < {prev_head}");
            prev_head = head;
        }
        assert!(prev_head > 0, "head advanced with the scrolled content");
    }

    /// A wheel-up mid-drag while Down autoscroll is armed: each writer
    /// moves the offset once per its own event (the wheel never rewrites
    /// the autoscroll state, ticks never re-apply the wheel), so there is
    /// no feedback loop — after the wheel stops, ticks settle the offset
    /// back at the clamp.
    #[test]
    fn wheel_up_during_autoscroll_down_settles_without_feedback() {
        let mut agent = agent_with_tall_scrollback();
        agent.scrollback.scroll_up(10_000);
        agent.drag_autoscroll = compute_autoscroll(9, agent.pane_areas.scrollback);
        let armed = agent.drag_autoscroll.expect("Down armed");

        for _ in 0..10 {
            agent.tick_drag_autoscroll();
        }
        let mid = agent.scrollback.scroll_info().0;
        assert!(mid > 0, "setup: autoscroll moved off the top");

        agent.handle_scroll(-3, 5, 5);
        let after_wheel = agent.scrollback.scroll_info().0;
        assert_eq!(after_wheel, mid - 3, "the wheel wrote exactly once");
        assert_eq!(
            agent.drag_autoscroll,
            Some(armed),
            "the wheel must not rewrite the autoscroll state"
        );

        let (_, viewport, total) = agent.scrollback.scroll_info();
        let max_offset = total - viewport as usize;
        let mut prev = after_wheel;
        for _ in 0..200 {
            agent.tick_drag_autoscroll();
            agent.reclamp_drag_head_post_render(false);
            let now = agent.scrollback.scroll_info().0;
            assert!(
                now >= prev,
                "offset regressed after the wheel: {now} < {prev}"
            );
            prev = now;
        }
        assert_eq!(prev, max_offset, "ticks settle back at the clamp");
    }

    /// Btw presses never arm the deferred latch: the panel keeps its exact
    /// hitbox (a press on its non-text area arms nothing).
    #[test]
    fn btw_press_does_not_arm_deferred_latch() {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        agent.last_btw_area = Rect::new(10, 10, 30, 6);

        let _ = agent.handle_input(&Event::Mouse(mouse_down(12, 12)), &reg);

        assert!(agent.deferred_text_press.is_none(), "btw stays exact-only");
        assert!(agent.pending_block_drag.is_none());
        assert!(agent.pending_text_drag.is_none());
    }
}
