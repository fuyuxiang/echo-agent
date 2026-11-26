//! Minimal-mode commit pipeline: which finalized blocks get printed into the
//! terminal's native scrollback, and in what display mode.
//!
//! This module is deliberately terminal-agnostic and unit-testable: the actual
//! `insert_before` call is injected as a closure by the caller (the per-frame
//! draw loop, added in a later PR). The "committed frontier" is the leading
//! contiguous run of finalized, non-pending entries; everything past it stays
//! in the live region until it finalizes.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use echo_agent_pager::app::PagerTerminal;
use echo_agent_pager::app::app_view::{ActiveView, AppView};
use echo_agent_pager::appearance::AppearanceConfig;
use echo_agent_pager::minimal_api;
use echo_agent_pager::render::Renderable;
use echo_agent_pager::scrollback::block::RenderBlock;
use echo_agent_pager::scrollback::blocks::ToolCallBlock;
use echo_agent_pager::scrollback::entry::{EntryId, ScrollbackEntry};
use echo_agent_pager::scrollback::state::ScrollbackState;
use echo_agent_pager::scrollback::types::DisplayMode;
use echo_agent_pager::scrollback::wrappers::EntryRenderer;
use echo_agent_pager::theme::Theme;

/// Blank rows emitted after each committed block — and held after each live-tail
/// entry (`super::live`) — in minimal mode.
///
/// Now `0`: adjacent blocks abut, relying on each block's own chrome (the accent
/// column + the `◆`/bullet that marks a block start) to read the boundary. A
/// full separator row per block made the transcript too airy for short
/// collapsed thinking / tool blocks (dogfood feedback), so the gap is dropped.
/// The constant is kept (rather than deleting the plumbing) so the spacing stays
/// tunable in one place.
///
/// Whatever the value, it must be applied identically on both sides of the
/// commit frontier (the committed footprint here and the live-tail footprint in
/// [`super::live::draw_tail`] / [`super::live::tail_height`]) so a block's total
/// height is unchanged when it moves from the live region into native
/// scrollback — otherwise the prompt would shift on every commit.
pub(crate) const MINIMAL_BLOCK_GAP: u16 = 0;

/// Whether the entry at index `i` may be committed to native scrollback yet.
///
/// While a turn is **running**, a block is committable once it has finished
/// running AND is not awaiting user input. The pending-input gate is what makes
/// the mid-stream permission hold fall out for free: a tool blocked on a
/// permission / `ask_user_question` prompt is flagged `is_pending_user_input`,
/// so the leading-run scan stops *before* it and it stays in the live region
/// until the user answers.
///
/// **Lingering-`is_running` agent messages (the "accumulate, don't cap" fix):**
/// the tracker leaves an agent message's `is_running` flag set until turn end —
/// `handle_tool_call` resets `current_agent_msg` to `None` *without* finishing
/// the entry when a tool follows. That stale flag would wedge the frontier at
/// the message, piling the rest of the turn into the live tail. A later
/// *turn-progress* block (tool / next message / session event / …) proves the
/// tracker moved on and will never append again, so we commit it. Interleaved
/// **thinking** does not: `handle_thought_chunk` pushes a thinking entry
/// *after* the message without resetting `current_agent_msg`, so later tokens
/// still append. Treating that as "done" print-once-freezes the first word(s)
/// on the terminal (GBS-28). Only agent messages get this relaxation: a
/// running **tool** may still update its result, so it keeps the strict
/// `is_running` gate, and the **last** entry always stays live.
///
/// **Background-task lifecycle blocks commit even while "running".** A fresh
/// background task is pushed as a `BgTask` "started" block with the entry's
/// `is_running` flag set (`handle_task_backgrounded` → `set_last_running(true)`),
/// but that flag only drives bullet animation — the block's *content* never
/// changes (task completion pushes a **separate** `bg_task_completed`/`failed`
/// block, and live output goes to the task store, not this entry). An async task
/// can outlive its turn, so gating it on `is_running` would wedge the commit
/// frontier for the rest of the turn: the "started" block — and everything after
/// it — would stay stuck in the live tail (scrolled out of view), so the task is
/// invisible until it finishes. Committing it immediately matches design §6.11
/// ("status blocks committed") and keeps the frontier moving.
///
/// Once the turn is **idle** (`turn_running == false`) everything except a
/// pending-user-input block is stable and committable: the tracker can also
/// leave a thinking block's `is_running` flag set after the turn ends (finalize
/// missed at a transition), and that stale flag must not permanently wedge the
/// commit frontier. The caller finalizes such entries before rendering so they
/// print in their finished form. The pending-input gate deliberately applies in
/// every turn state (see the first check below).
///
/// ⚠️ **Print-once caveat for idle-pushed running blocks:** a spinner-style
/// entry pushed as `running` while the turn is idle is committed *immediately*
/// by this rule — any later in-place fill of that entry never reaches the
/// terminal. Handlers that fill placeholders (e.g. `SessionRecap`) must check
/// `ScrollbackState::is_committed` and append a fresh block instead.
pub fn is_committable(state: &ScrollbackState, i: usize, turn_running: bool) -> bool {
    let Some(entry) = state.get(i) else {
        return false;
    };
    // A block awaiting user input (permission / ask_user_question) holds the
    // frontier in EVERY turn state, not just mid-turn: its rendered form is
    // still going to change when the prompt resolves, so committing it
    // (print-once) would freeze the "waiting" form on the terminal. The idle
    // case is defensive — permissions normally resolve within the turn, but a
    // pending mark must never be committed out from under its modal.
    if entry.is_pending_user_input {
        return false;
    }
    if !turn_running {
        return true;
    }
    if !entry.is_running {
        return true;
    }
    // Running, mid-turn. Two block kinds are safe to commit despite a set
    // `is_running` flag: a BgTask lifecycle block (finalized event; flag is
    // animation-only — see above), and an agent message whose later sibling
    // proves the tracker moved on ([`agent_message_stream_closed`]). A running
    // **tool** may still update its result, and a still-open agent stream
    // (last, or only followed by interleaved thinking) must stay live.
    matches!(entry.block, RenderBlock::BgTask(_))
        || (matches!(entry.block, RenderBlock::AgentMessage(_))
            && agent_message_stream_closed(state, i))
}

/// Whether a later entry proves the tracker will not append to the agent
/// message at `i` again.
///
/// `handle_tool_call` / a new stream / turn-end push a tool, another message,
/// or a session event *and* drop `current_agent_msg`. Interleaved thinking
/// does not — it is inserted after the live message while chunks still
/// append — so it must not trip print-once (GBS-28).
fn agent_message_stream_closed(state: &ScrollbackState, i: usize) -> bool {
    ((i + 1)..state.len()).any(|j| {
        state
            .get(j)
            .is_some_and(|e| !matches!(e.block, RenderBlock::Thinking(_)))
    })
}

/// The display mode a block should be committed in (minimal mode, print-once).
///
/// Stamps BOTH the entry being committed and the still-uncommitted live-tail
/// entries ([`commit_active`]), so a block's height is identical either side of
/// the commit frontier and the prompt does not jerk when it crosses.
pub fn minimal_commit_display_mode(
    block: &RenderBlock,
    appearance: &AppearanceConfig,
) -> DisplayMode {
    let collapse_thinking = appearance.minimal_collapse_thinking;
    match block {
        RenderBlock::ToolCall(ToolCallBlock::Edit(_)) => DisplayMode::Expanded,
        RenderBlock::ToolCall(
            tc @ (ToolCallBlock::Search(_)
            | ToolCallBlock::Read(_)
            | ToolCallBlock::ListDir(_)
            | ToolCallBlock::MemorySearch(_)
            | ToolCallBlock::IntegrationSearch(_)),
        ) if tc.is_success() => DisplayMode::Collapsed,
        RenderBlock::ToolCall(_) => DisplayMode::Truncated,
        RenderBlock::Thinking(_) if collapse_thinking => DisplayMode::Collapsed,
        RenderBlock::Thinking(_) => DisplayMode::Expanded,
        _ => DisplayMode::Expanded,
    }
}

/// One step of the frontier walk — the single classification shared by every
/// consumer ([`commit_leading_run`], [`scan_frontier`]). Keeping this in one
/// place is load-bearing: the commit pass, the `will_commit` resize gate, the
/// viewport sizing (`tail_height`), and the tail renderer must all agree on
/// where the frontier stops, or a block's height flips between the live region
/// and native scrollback and the prompt jumps on commit.
enum Step {
    /// Uncommitted and committable: a commit pass consumes it.
    Commit,
    /// Already committed: skip over it (the id-set is authoritative; the scan
    /// cursor is only a lower-bound hint).
    Skip,
    /// End of entries, or the first uncommitted non-committable entry — the
    /// live tail starts here.
    Stop,
}

/// Classify the entry at `i` relative to the commit frontier.
fn classify(state: &ScrollbackState, i: usize, turn_running: bool) -> Step {
    match state.get(i) {
        None => Step::Stop,
        Some(e) if minimal_api::is_committed(state, e) => Step::Skip,
        Some(_) if !is_committable(state, i, turn_running) => Step::Stop,
        Some(_) => Step::Commit,
    }
}

/// Read-only projection of what a commit pass would do, for the consumers that
/// must agree with it without running it.
pub struct FrontierScan {
    /// Index of the first entry a commit pass would NOT consume — where the
    /// live tail starts after this frame's commit. Everything from here on
    /// stays in the pinned live region.
    pub tail_start: usize,
    /// Whether a commit pass would emit at least one block into native
    /// scrollback this frame.
    pub will_commit: bool,
}

/// Walk the frontier read-only (no cursor mutation, nothing marked committed).
///
/// Used by the overlay host's viewport sizing ([`super::overlay::sync_viewport`]
/// via [`super::live::tail_height`]) and its commit gate — both run *before*
/// [`commit_active`] in the frame and must mirror its stop condition exactly.
pub fn scan_frontier(state: &ScrollbackState, turn_running: bool) -> FrontierScan {
    let mut i = minimal_api::commit_scan_cursor(state);
    let mut will_commit = false;
    loop {
        match classify(state, i, turn_running) {
            Step::Stop => break,
            Step::Skip => i += 1,
            Step::Commit => {
                will_commit = true;
                i += 1;
            }
        }
    }
    FrontierScan {
        tail_start: i,
        will_commit,
    }
}

/// Commit the leading contiguous run of newly-committable entries (those past
/// the scan cursor that are finalized and not pending), in insertion order.
///
/// For each such entry, `on_commit(state, index)` runs first — the caller
/// finalizes/stamps the entry and renders it into native scrollback — and
/// **only if it returns `true`** is the entry marked committed. A `false`
/// return (the terminal write failed) stops the walk with the entry still
/// uncommitted and the cursor before it, so the next frame retries instead of
/// marking a block committed that never reached the terminal (a print-once
/// mode can never re-emit it — the block would silently vanish; bugbot).
///
/// The scan stops at the first still-running / pending entry (so a turn
/// streams smoothly and a sibling tool awaiting permission holds the
/// frontier). Returns the number of entries committed.
///
/// This is the ONE mutating frontier walk; [`commit_active`] drives it in
/// production and the unit tests below drive it directly, so the tested loop
/// and the production loop cannot drift.
pub fn commit_leading_run(
    state: &mut ScrollbackState,
    turn_running: bool,
    mut on_commit: impl FnMut(&mut ScrollbackState, usize) -> bool,
) -> usize {
    let mut i = minimal_api::commit_scan_cursor(state);
    let mut count = 0usize;
    loop {
        match classify(state, i, turn_running) {
            Step::Stop => break,
            Step::Skip => i += 1,
            Step::Commit => {
                if !on_commit(state, i) {
                    break; // emit failed — leave uncommitted, retry next frame
                }
                minimal_api::mark_committed(state, i);
                count += 1;
                i += 1;
            }
        }
    }
    minimal_api::set_commit_scan_cursor(state, i);
    count
}

/// Appearance used when committing blocks to native scrollback.
///
/// Timestamps are forced off: `EntryRenderer::desired_height` subtracts the
/// timestamp column but `render` treats it as an overlay, so leaving timestamps
/// on would make the reserved `insert_before` height disagree with the painted
/// rows (design decision K5). Block horizontal padding is zeroed so committed
/// content is flush-left with the welcome card (which paints edge-to-edge);
/// paired with [`minimal_renderer`] reclaiming the accent column, glyphs start
/// at column 0. The live region's prompt / status / info rows mirror that via
/// [`super::live::live_left_inset`].
///
/// The two reasoning-legibility toggles are set here rather than in
/// `pager.toml` so the full TUI stays provably untouched — design doc §6.16.
pub(crate) fn committed_appearance(base: &AppearanceConfig) -> AppearanceConfig {
    let mut a = base.clone();
    a.show_timestamps = false;
    a.scrollback.layout.block_pad_left = 0;
    a.scrollback.layout.block_pad_right = 0;
    a.scrollback.blocks.thinking.body_dim_italic = true;
    a.scrollback.blocks.thinking.collapsed_expand_hint = true;
    a
}

pub(crate) const COMMITTED_TICK: u64 = 0;

/// The renderer for one minimal-mode entry, on **either** side of the commit
/// frontier — `tick` is the only difference. Chrome here decides a block's
/// wrapped height, so both sides must agree or the prompt jumps on commit (K5);
/// keeping it one constructor is what makes that unbreakable.
///
/// Reasoning alone keeps the accent column, as the marker that separates it
/// from the answer. Design doc §6.16.
pub(crate) fn minimal_renderer<'a>(
    entry: &'a ScrollbackEntry,
    theme: &'a Theme,
    appearance: AppearanceConfig,
    cwd: &'a std::path::Path,
    tick: u64,
) -> EntryRenderer<'a> {
    // Reserved only where it is actually painted: `ThinkingBlock::accent`
    // returns `None` when collapsed, and reserving a column nothing paints
    // would indent the header over a blank gutter. Collapsed reasoning has no
    // body to delimit anyway — the folded `Thought for Xs` header cannot be
    // mistaken for the answer. `only_thinking_spends_the_accent_column` pins
    // reserved == painted so the two rules cannot drift apart.
    let hide_accent = !matches!(entry.block, RenderBlock::Thinking(_))
        || entry.display_mode() == DisplayMode::Collapsed;
    EntryRenderer::new(entry, theme)
        .with_appearance(appearance)
        .with_cwd(Some(cwd))
        .with_tick(tick)
        .with_flat_background(true)
        .with_hide_accent(hide_accent)
        // The accent resolves to `Color::Reset` under the terminal-native
        // palette — full-brightness default fg, which would shout.
        .with_dim_accent(true)
}

/// Emit one committed block into native scrollback via `insert_before`, capping
/// its height at `max_rows` (0 = unbounded).
///
/// "Diffs always full" (K9) means a multi-thousand-line `Edit` would otherwise
/// allocate one `Buffer` of `desired_height` rows and emit a huge writer-thread
/// send burst (§6.15). When the block is taller than `max_rows`, only the top
/// `max_rows - 1` content rows are committed and the final row becomes a
/// `… N more lines · /transcript to view` footer. The block is laid out at its
/// **full** `desired_height` so wrapping is byte-identical to an uncapped commit
/// (K5); the `insert_before` buffer is only `commit_h` rows tall, so any content
/// past it is clipped — bounding the allocation to the cap.
fn insert_committed(
    terminal: &mut PagerTerminal,
    renderer: EntryRenderer<'_>,
    width: u16,
    max_rows: u16,
    footer_style: Style,
) -> std::io::Result<()> {
    let full_h = renderer.desired_height(width);
    if full_h == 0 {
        return Ok(());
    }
    let commit_h = if max_rows > 0 && full_h > max_rows {
        max_rows
    } else {
        full_h
    };
    // Propagated (not swallowed): the caller must NOT mark the entry committed
    // when the terminal write failed — print-once means a marked-but-unprinted
    // block can never be emitted again (bugbot).
    terminal.insert_before(commit_h, move |buf| {
        paint_committed(buf, renderer, width, full_h, footer_style);
    })?;
    insert_gap(terminal);
    Ok(())
}

/// Emit [`MINIMAL_BLOCK_GAP`] blank rows into native scrollback as the trailing
/// gap after a committed block. The rows are left unpainted, so they inherit the
/// terminal's own background (matching the flat, transparent committed look).
pub(super) fn insert_gap(terminal: &mut PagerTerminal) {
    if MINIMAL_BLOCK_GAP == 0 {
        return;
    }
    let _ = terminal.insert_before(MINIMAL_BLOCK_GAP, |_buf| {});
}

/// Paint a committed block into `buf` (a `commit_h`-row buffer), laying it out at
/// its full `full_h` so wrapping matches an uncapped commit exactly (K5). When
/// `buf` is shorter than `full_h` the block is capped: rows past it are clipped
/// and the final row becomes a `… N more lines · /transcript to view` footer
/// (§6.15). Extracted from [`insert_committed`] so the cap is unit-testable
/// without a live terminal.
fn paint_committed(
    buf: &mut ratatui::buffer::Buffer,
    renderer: EntryRenderer<'_>,
    width: u16,
    full_h: u16,
    footer_style: Style,
) {
    let commit_h = buf.area.height;
    let area = Rect {
        x: buf.area.x,
        y: buf.area.y,
        width,
        height: full_h,
    };
    renderer.render(area, buf);
    if commit_h > 0 && commit_h < full_h {
        // Top `commit_h - 1` rows are content; the last row is the footer.
        let hidden = full_h.saturating_sub(commit_h.saturating_sub(1));
        let y = buf.area.y + commit_h - 1;
        let row = Rect {
            x: buf.area.x,
            y,
            width,
            height: 1,
        };
        // Dim default-fg chrome (not hard-coded DarkGray) under terminal-native.
        let style = footer_style.bg(Color::Reset);
        // Clear any clipped content that landed on the footer row first.
        buf.set_style(row, style);
        let text = format!("\u{2026} {hidden} more lines \u{00b7} /transcript to view");
        buf.set_span(buf.area.x, y, &Span::styled(text, style), width);
    }
}

/// Commit the active agent's newly-finalized blocks into native scrollback.
///
/// For each entry in the leading committable run: stamp its print-once display
/// mode (K9), then `insert_before` it using the shared `EntryRenderer` at
/// exactly `desired_height(width)` rows (K5).
///
/// On resume/attach (`loading_replay`) the replayed transcript is printed into
/// native scrollback like any other finalized block — minimal has no separate
/// history pane, so the terminal's scrollback *is* the history; without this a
/// resumed session looks empty (nothing redrawn). The commit frontier
/// (`committed` flags + `commit_scan_cursor`) still guarantees each block prints
/// exactly once.
pub fn commit_active(app: &mut AppView, terminal: &mut PagerTerminal) {
    let id = match &app.active_view {
        ActiveView::Agent(id) => *id,
        _ => return, // welcome / dashboard: nothing to commit
    };
    // Snapshot the commit appearance before borrowing `agents` mutably.
    let appearance = committed_appearance(&app.appearance);
    let Some(agent) = app.agents.get_mut(&id) else {
        return;
    };
    // Hold commits while a centered fullscreen app-modal (settings) is open: it
    // takes the whole live region, so an `insert_before` underneath it would
    // scroll the popup. Deferred commits flush on the next frame after it closes.
    if super::overlay::app_modal_active(agent) {
        return;
    }
    // NB: `sync_pending_user_input_marks` already ran at the top of the frame
    // ([`sync_pending_marks`], called from `crate::draw` BEFORE the viewport
    // sizing) so the sizing pass and this commit pass judge committability
    // against the same marks — syncing here made a tool look committable to
    // `sync_viewport`/`tail_height` on the very frame its permission arrived.
    //
    // Whether a turn is actively running. When idle, every remaining entry is
    // stable and committable (see `is_committable`); a stale `is_running` flag
    // left by the tracker must not wedge the frontier.
    let turn_running = agent.session.state.is_turn_running();
    let cwd = agent.session.cwd.as_path();
    let sb = &mut agent.scrollback;

    // NB: resume/attach replay (`agent.session.loading_replay`) intentionally
    // falls through to the normal commit pass below, so the loaded transcript is
    // printed into native scrollback (a resumed session must be visible).

    let theme = Theme::current();
    let footer_style = theme.dim();
    let max_rows = appearance.minimal_max_commit_rows;
    let width = terminal.viewport_area().width;
    if width == 0 {
        return;
    }

    // Drive the ONE frontier walk (`commit_leading_run` — also what the unit
    // tests exercise) with the production per-entry work: finalize, stamp the
    // print-once display mode, print, then remember folded blocks for Ctrl+E.
    commit_leading_run(sb, turn_running, |sb, i| {
        // If the turn is idle but this entry still carries a stale
        // `is_running` flag, finalize it first so it renders in its
        // finished form (e.g. "Thought for Xs", not an animated "Thinking…").
        if let Some(id) = sb.get(i).filter(|e| e.is_running).map(|e| e.id) {
            sb.finish_running(id);
        }
        // Stamp the print-once display mode before measuring/rendering.
        if let Some(e) = sb.get_mut(i) {
            let mode = minimal_commit_display_mode(&e.block, &appearance);
            e.set_display_mode(mode);
        }
        if let Some(e) = sb.get(i) {
            // `insert_committed` pushes these rows above the pinned viewport,
            // into the terminal's own scrollback (capped — §6.15). A failed
            // write returns `false` so the walk leaves the entry uncommitted
            // (retried next frame) instead of marking a never-printed block.
            //
            // NOTE (print-once contract): from a successful insert on, the
            // entry's content is frozen on the user's terminal. Mutating it in
            // place later (`get_by_id_mut` + edit, the `/recap` fill pattern)
            // will NOT reach the screen — append a fresh block instead (see
            // the `SessionRecap` handler in `acp_handler.rs`).
            let renderer = minimal_renderer(e, &theme, appearance.clone(), cwd, COMMITTED_TICK);
            if insert_committed(terminal, renderer, width, max_rows, footer_style).is_err() {
                return false;
            }
        }
        // Remember folded blocks (collapsed reasoning / truncated output) so
        // `Ctrl+E` / `/expand` can re-print them in full later (K10) — only
        // after the print actually succeeded.
        if let Some((id, mode)) = sb.get(i).map(|e| (e.id, e.display_mode()))
            && matches!(mode, DisplayMode::Collapsed | DisplayMode::Truncated)
        {
            minimal_api::record_committed_for_expand(sb, id);
        }
        true
    });

    // Stamp the still-uncommitted "live tail" entries with the same print-once
    // display policy they will commit with (collapsed reasoning, truncated tool
    // output, full diffs/messages). The tail renders each entry at its current
    // `display_mode`, but blocks stream Expanded and commit folded — so without
    // this the live region is tall while a block streams and snaps short the
    // instant it finalizes, jerking the prompt upward (dogfood nit). Matching
    // the tail height to the committed height keeps the prompt put across a
    // commit. Idempotent: `set_display_mode` no-ops when unchanged.
    let mut j = minimal_api::commit_scan_cursor(sb);
    while let Some(e) = sb.get_mut(j) {
        let mode = minimal_commit_display_mode(&e.block, &appearance);
        e.set_display_mode(mode);
        j += 1;
    }
}

/// Re-print the entries queued by `Ctrl+E` / `/expand` into native scrollback,
/// fully expanded, below the committed conversation (design decision K10).
///
/// Committed terminal text cannot be mutated in place, so "expanding" a folded
/// block is an honest re-print of the same entry in `Expanded` mode. The entry
/// itself is already committed and past the scan cursor, so flipping its display
/// mode has no effect on the live tail.
///
/// The re-print is **uncapped** (`max_rows = 0`): the initial commit truncated
/// the block under `minimal_max_commit_rows`, and this is the explicit "show me
/// the whole thing" action — capping it again just reprinted the same footer
/// (bugbot). A one-shot user-initiated tall insert is an acceptable burst.
pub fn expand_pending(app: &mut AppView, terminal: &mut PagerTerminal) {
    if minimal_api::minimal_pending_expand(app).is_empty() {
        return;
    }
    let id = match &app.active_view {
        ActiveView::Agent(id) => *id,
        _ => return,
    };
    let width = terminal.viewport_area().width;
    if width == 0 {
        return;
    }
    let appearance = committed_appearance(&app.appearance);
    // Guards: a missing active agent must leave the IDs queued, so confirm it
    // exists before consuming the queue below (the queue take needs `&mut app`,
    // which can't overlap the agent borrow — hence the check-then-reborrow).
    // Likewise hold the whole queue while a centered app-modal owns the live
    // region — an `insert_before` would scroll the popup and the user wouldn't
    // see the re-print (same hold as `commit_active`; bugbot).
    match app.agents.get(&id) {
        Some(agent) if !super::overlay::app_modal_active(agent) => {}
        _ => return,
    }
    let theme = Theme::current();
    let footer_style = theme.dim();
    // Consume the expand queue only after every guard above has passed: a
    // non-agent active view, a 0-width (probe) frame, or an open app-modal must
    // leave the IDs queued for a later frame, not silently drop a Ctrl+E /
    // /expand request.
    let ids = minimal_api::take_minimal_pending_expand(app);
    let mut requeue: Vec<EntryId> = Vec::new();
    {
        let Some(agent) = app.agents.get_mut(&id) else {
            // Can't happen (existence checked just above, nothing in between
            // can remove the agent) — but if it ever does, the drained queue
            // must go back rather than silently vanish.
            minimal_api::requeue_minimal_pending_expand(app, ids);
            return;
        };
        let cwd = agent.session.cwd.as_path();
        let sb = &mut agent.scrollback;
        let mut iter = ids.into_iter();
        while let Some(eid) = iter.next() {
            let Some(idx) = sb.index_of_id(eid) else {
                continue; // entry removed (rewind / clear) since the keypress
            };
            if let Some(e) = sb.get_mut(idx) {
                e.set_display_mode(DisplayMode::Expanded);
            }
            if let Some(e) = sb.get(idx) {
                let renderer = minimal_renderer(e, &theme, appearance.clone(), cwd, COMMITTED_TICK);
                if insert_committed(terminal, renderer, width, 0, footer_style).is_err() {
                    // Terminal write failed: keep this id and the rest queued
                    // so the request retries next frame instead of vanishing.
                    requeue.push(eid);
                    requeue.extend(iter);
                    break;
                }
            }
        }
    }
    if !requeue.is_empty() {
        minimal_api::requeue_minimal_pending_expand(app, requeue);
    }
}

/// Re-mark tool entries that are blocked on a pending permission/question so
/// the frontier holds them in the live region (the full TUI does this each
/// frame in `AgentView::draw`, which minimal bypasses); `is_committable` reads
/// `is_pending_user_input`.
///
/// Called at the TOP of the frame (from [`crate::draw`]), before
/// [`super::overlay::sync_viewport`]: the viewport sizing, the `will_commit`
/// gate, and the commit pass must all judge committability against the same
/// marks — syncing inside the commit pass let a just-arrived permission's tool
/// look committable to the sizing walk for one frame (bugbot).
pub fn sync_pending_marks(app: &mut AppView) {
    if let ActiveView::Agent(id) = &app.active_view
        && let Some(agent) = app.agents.get_mut(id)
    {
        minimal_api::sync_pending_user_input_marks(agent);
    }
}

#[cfg(test)]
#[path = "commit_tests.rs"]
mod tests;
