//! Unit tests for [`super`] — minimal mode's commit pipeline.
//!
//! Split out of `commit.rs` to keep the production module scannable; the
//! `#[path]` attribute there keeps this a plain child module (`super::*` still
//! reaches the private items under test).

use super::*;
use echo_agent_pager::scrollback::block::RenderBlock;
use echo_agent_pager::scrollback::entry::ScrollbackEntry;
use echo_agent_pager::scrollback::state::ScrollbackState;
use echo_agent_pager_diff::DiffLine;
use ratatui::style::Color;

fn test_cwd() -> &'static std::path::Path {
    std::path::Path::new("/test/session")
}

fn finalized(text: &str) -> ScrollbackEntry {
    ScrollbackEntry::new(RenderBlock::stub(text, Color::Blue))
}

fn running(text: &str) -> ScrollbackEntry {
    ScrollbackEntry::running(RenderBlock::stub(text, Color::Blue))
}

fn default_appearance() -> AppearanceConfig {
    committed_appearance(&AppearanceConfig::default())
}

fn collapsed_appearance() -> AppearanceConfig {
    committed_appearance(&AppearanceConfig {
        minimal_collapse_thinking: true,
        ..Default::default()
    })
}

/// Run a commit pass for a RUNNING turn, returning the emitted indices.
fn commit_collect(state: &mut ScrollbackState) -> Vec<usize> {
    let mut seen = Vec::new();
    commit_leading_run(state, true, |_, i| {
        seen.push(i);
        true
    });
    seen
}

#[test]
fn commits_leading_finalized_run_and_stops_at_running() {
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(finalized("b"));
    s.push(running("c"));
    s.push(finalized("d")); // after the running block — must NOT commit yet

    assert_eq!(commit_collect(&mut s), vec![0, 1]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 2);
    assert!(minimal_api::is_committed(&s, s.get(0).unwrap()));
    assert!(minimal_api::is_committed(&s, s.get(1).unwrap()));
    assert!(!minimal_api::is_committed(&s, s.get(2).unwrap()));
    assert!(!minimal_api::is_committed(&s, s.get(3).unwrap()));

    // Finalize "c"; the next pass commits "c" then "d".
    s.get_mut(2).unwrap().mark_completed();
    assert_eq!(commit_collect(&mut s), vec![2, 3]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 4);
}

#[test]
fn pending_user_input_holds_the_frontier() {
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    let tool = s.push(finalized("tool")); // finalized but awaiting permission
    s.push(finalized("after"));
    assert!(s.set_pending_user_input(tool, true));

    // Stops before the pending tool, even though it (and "after") are finalized.
    assert_eq!(commit_collect(&mut s), vec![0]);

    // Resolving the prompt releases the rest of the run.
    assert!(s.set_pending_user_input(tool, false));
    assert_eq!(commit_collect(&mut s), vec![1, 2]);
}

#[test]
fn running_agent_message_commits_once_a_later_block_exists() {
    // The tracker leaves an agent message's `is_running` flag set until turn
    // end (handle_tool_call resets current_agent_msg without finishing the
    // entry when a tool follows). Minimal must still commit that message
    // mid-turn once a later *turn-progress* block proves it's complete —
    // otherwise the rest of the turn piles up in the live tail and scrolls
    // instead of accumulating into native scrollback.
    let mut s = ScrollbackState::new();
    s.push(ScrollbackEntry::running(RenderBlock::agent_message(
        "answer text",
    )));

    // While it's the last entry it may still be streaming → stays live.
    assert_eq!(commit_collect(&mut s), Vec::<usize>::new());
    assert_eq!(minimal_api::commit_scan_cursor(&s), 0);

    // A later tool (the tracker moved on) proves the message is done → it
    // commits even though its is_running flag still lingers. The new
    // last/running entry stays in the live tail.
    s.push(running("tool"));
    assert_eq!(commit_collect(&mut s), vec![0]);
    assert!(minimal_api::is_committed(&s, s.get(0).unwrap()));
    assert!(!minimal_api::is_committed(&s, s.get(1).unwrap()));
}

#[test]
fn interleaved_thinking_does_not_commit_a_still_streaming_agent_message() {
    // GBS-28: after a long subagent the model often emits a first response
    // token, then more thinking, then the rest of the sentence. Thinking is
    // pushed *after* the live agent entry without resetting
    // `current_agent_msg`, so later chunks still append. "Any later block"
    // used to trip print-once and freeze "The user" / "That" on the terminal
    // while the rest of the stream landed only in memory.
    let mut s = ScrollbackState::new();
    let msg = s.push(ScrollbackEntry::running(RenderBlock::agent_message(
        "The user",
    )));
    s.push(ScrollbackEntry::running(RenderBlock::thinking_streaming()));

    assert_eq!(
        commit_collect(&mut s),
        Vec::<usize>::new(),
        "interleaved thinking must not freeze the still-open agent stream"
    );
    assert!(!minimal_api::is_committed(&s, s.get(0).unwrap()));
    assert_eq!(minimal_api::commit_scan_cursor(&s), 0);

    // Later tokens still append to the same entry (`current_agent_msg` is
    // still set). Committing at the thinking boundary would have left this
    // suffix invisible on the print-once terminal.
    assert!(s.push_chunk_to_agent(msg, " asked me to wait."));
    assert_eq!(commit_collect(&mut s), Vec::<usize>::new());
    match &s.get(0).unwrap().block {
        RenderBlock::AgentMessage(m) => {
            assert_eq!(m.text(), "The user asked me to wait.");
        }
        other => panic!("expected agent message, got {other:?}"),
    }

    // A later tool is the real "tracker moved on" signal — now the prefix
    // (whatever has arrived) is complete and must leave the live tail.
    s.push(ScrollbackEntry::running(RenderBlock::execute("true")));
    assert_eq!(commit_collect(&mut s), vec![0]);
    assert!(minimal_api::is_committed(&s, s.get(0).unwrap()));
    assert!(!minimal_api::is_committed(&s, s.get(1).unwrap()));
    assert!(!minimal_api::is_committed(&s, s.get(2).unwrap()));
}

#[test]
fn running_tool_still_holds_the_frontier_even_with_a_later_block() {
    // The agent-message relaxation must NOT extend to tools: a running tool
    // can still update its result, so committing it (print-once) would lose
    // the update. It holds the frontier regardless of later blocks.
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(running("running tool")); // stub == not an AgentMessage
    s.push(finalized("after"));
    assert_eq!(commit_collect(&mut s), vec![0]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 1);
}

#[test]
fn plan_body_anchored_above_a_parked_tool_commits_while_it_is_still_running() {
    let mut s = ScrollbackState::new();
    s.push(finalized("user prompt"));
    let tool = s.push(running("exit_plan_mode")); // parked on the decision
    s.insert_block_before(tool, RenderBlock::agent_message("PLAN BODY"));

    // Prompt + plan commit; the running tool row still holds the frontier.
    assert_eq!(commit_collect(&mut s), vec![0, 1]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 2);
    assert!(matches!(
        s.get(1).map(|e| &e.block),
        Some(RenderBlock::AgentMessage(_))
    ));
    assert!(minimal_api::is_committed(&s, s.get(1).unwrap()));
    assert!(!minimal_api::is_committed(&s, s.get(2).unwrap()));

    // Answering the prompt finalizes the tool row, which then commits once,
    // in its finished form — and the plan is NOT re-emitted.
    s.get_mut(2).unwrap().mark_completed();
    assert_eq!(commit_collect(&mut s), vec![2]);
    assert!(!scan_frontier(&s, false).will_commit);
}

#[test]
fn anchored_plan_body_is_not_left_in_the_live_tail() {
    // Whatever the commit pass prints must leave the live tail, or the plan is
    // painted under the prompt AND printed above it.
    let mut s = ScrollbackState::new();
    s.push(finalized("user prompt"));
    let tool = s.push(running("exit_plan_mode"));
    s.insert_block_before(tool, RenderBlock::agent_message("PLAN BODY"));

    // Sizing pass: the tail is just the tool row (index 2), and a commit is
    // pending for the two entries above it.
    let before = scan_frontier(&s, true);
    assert_eq!(before.tail_start, 2, "plan is excluded from the live tail");
    assert!(before.will_commit);

    commit_leading_run(&mut s, true, |_, _| true);

    // Commit pass: same tail, nothing left to print.
    let after = scan_frontier(&s, true);
    assert_eq!(after.tail_start, before.tail_start, "tail must not move");
    assert!(!after.will_commit);
}

#[test]
fn revised_plan_anchors_to_its_own_tool_row_and_neither_plan_re_emits() {
    let mut s = ScrollbackState::new();
    let tool1 = s.push(running("exit_plan_mode #1"));
    s.insert_block_before(tool1, RenderBlock::agent_message("PLAN ONE"));
    assert_eq!(commit_collect(&mut s), vec![0]); // plan one

    s.get_mut(1).unwrap().mark_completed();
    let tool2 = s.push(running("exit_plan_mode #2"));
    s.insert_block_before(tool2, RenderBlock::agent_message("PLAN TWO"));

    // Tool #1 and plan two commit; tool #2 holds the frontier.
    assert_eq!(commit_collect(&mut s), vec![1, 2]);
    // A third pass re-emits nothing.
    assert!(commit_collect(&mut s).is_empty());
    assert_eq!(minimal_api::commit_scan_cursor(&s), 3);
}

#[test]
fn bg_task_started_commits_while_running_and_does_not_wedge_frontier() {
    // A fresh background task is pushed as a running "started" block
    // (`set_last_running(true)`). Its `is_running` flag is animation-only —
    // the block is a finalized lifecycle event whose content never changes —
    // so it must commit immediately even mid-turn. Otherwise it wedges the
    // frontier and the task (plus everything after it) stays hidden in the
    // live tail until the task finishes (the reported dogfood bug).
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(ScrollbackEntry::running(RenderBlock::bg_task(
        "sleep 60", "task-1",
    )));
    s.push(running("later tool")); // more turn output after the bg task

    // "a" + the running bg task commit; only the trailing running tool stays.
    assert_eq!(commit_collect(&mut s), vec![0, 1]);
    assert!(minimal_api::is_committed(&s, s.get(1).unwrap()));
    assert!(!minimal_api::is_committed(&s, s.get(2).unwrap()));
}

#[test]
fn bg_task_started_commits_as_last_running_entry() {
    // Even as the last entry of a still-running turn the bg "started" block
    // commits — a lifecycle block never streams more content (completion is
    // a separate block).
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(ScrollbackEntry::running(RenderBlock::bg_task(
        "sleep 60", "task-1",
    )));
    assert_eq!(commit_collect(&mut s), vec![0, 1]);
}

#[test]
fn no_double_commit_after_mid_list_shift_remove() {
    let mut s = ScrollbackState::new();
    let a = s.push(finalized("a"));
    s.push(finalized("b"));
    s.push(finalized("c"));
    assert_eq!(commit_collect(&mut s), vec![0, 1, 2]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 3);

    // Remove an already-committed entry below the cursor (shift_remove shifts
    // the remaining indices down). The cursor is clamped; the per-entry
    // `committed` flags travel with "b"/"c", so neither is re-emitted.
    assert!(s.remove_entry(a));
    s.push(finalized("d")); // now at index 2

    assert_eq!(commit_collect(&mut s), vec![2]);
    assert!(minimal_api::is_committed(&s, s.get(0).unwrap())); // b
    assert!(minimal_api::is_committed(&s, s.get(1).unwrap())); // c
    assert!(minimal_api::is_committed(&s, s.get(2).unwrap())); // d
}

#[test]
fn mid_list_removal_below_cursor_does_not_strand_uncommitted_entries() {
    // Regression (review bug 2): a committed placeholder ("Loading
    // session...") is removed AFTER new uncommitted entries were appended
    // past the cursor — the `/resume` / reconnect `SessionLoaded` ordering.
    // Removing below the cursor shifts the uncommitted entries down one;
    // without the cursor decrement in `remove_entry` the first of them
    // slid below the cursor and was never committed NOR drawn in the live
    // tail (silently missing from minimal mode).
    let mut s = ScrollbackState::new();
    s.push(finalized("old-1"));
    let placeholder = s.push(finalized("Loading session..."));

    // A draw commits both; cursor = 2.
    let mut seen = Vec::new();
    commit_leading_run(&mut s, false, |_, i| {
        seen.push(i);
        true
    });
    assert_eq!(seen, vec![0, 1]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 2);

    // Replay appends entries, then the placeholder is removed in the same
    // event cycle (before the next commit pass).
    s.push(finalized("replayed-A"));
    s.push(finalized("replayed-B"));
    assert!(s.remove_entry(placeholder));
    // The cursor moved down with the shifted entries.
    assert_eq!(minimal_api::commit_scan_cursor(&s), 1);

    // The next pass commits BOTH replayed entries — none stranded.
    let mut seen = Vec::new();
    commit_leading_run(&mut s, false, |_, i| {
        seen.push(i);
        true
    });
    assert_eq!(seen, vec![1, 2]);
    assert!(minimal_api::is_committed(&s, s.get(1).unwrap())); // replayed-A
    assert!(minimal_api::is_committed(&s, s.get(2).unwrap())); // replayed-B
}

#[test]
fn pending_user_input_holds_the_frontier_even_when_idle() {
    // A block awaiting a permission / question answer must never commit,
    // even if the turn state reads idle (e.g. a prompt outliving its turn):
    // its rendered form still changes when the prompt resolves, and a
    // committed copy is frozen. The idle relaxation only applies to
    // *stale-running* flags, not pending-input marks.
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    let tool = s.push(finalized("tool"));
    assert!(s.set_pending_user_input(tool, true));

    let mut seen = Vec::new();
    commit_leading_run(&mut s, false, |_, i| {
        seen.push(i);
        true
    });
    assert_eq!(seen, vec![0], "pending entry must hold the frontier");
    assert!(!minimal_api::is_committed(&s, s.get(1).unwrap()));

    // Resolving the prompt releases it.
    assert!(s.set_pending_user_input(tool, false));
    let mut seen = Vec::new();
    commit_leading_run(&mut s, false, |_, i| {
        seen.push(i);
        true
    });
    assert_eq!(seen, vec![1]);
}

#[test]
fn failed_emit_leaves_entry_uncommitted_for_retry() {
    // Regression (bugbot "Committed flag set on IO failure"): a terminal
    // write failure must NOT mark the entry committed — print-once means a
    // marked-but-unprinted block can never be emitted again. The walk stops
    // with the cursor before the failed entry and retries next frame.
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(finalized("b"));

    // First pass: the emit fails on the first entry.
    let mut calls = 0usize;
    let n = commit_leading_run(&mut s, false, |_, _| {
        calls += 1;
        false
    });
    assert_eq!(n, 0, "nothing committed on failure");
    assert_eq!(calls, 1, "walk stops at the first failure");
    assert!(!minimal_api::is_committed(&s, s.get(0).unwrap()));
    assert_eq!(minimal_api::commit_scan_cursor(&s), 0, "cursor holds");

    // Retry pass succeeds and commits both.
    let n = commit_leading_run(&mut s, false, |_, _| true);
    assert_eq!(n, 2);
    assert!(minimal_api::is_committed(&s, s.get(0).unwrap()));
    assert!(minimal_api::is_committed(&s, s.get(1).unwrap()));
}

#[test]
fn scan_frontier_mirrors_commit_leading_run() {
    // `scan_frontier` (read-only: viewport sizing + the will-commit gate)
    // must agree exactly with the mutating walk, in every phase.
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(finalized("b"));
    s.push(running("c"));
    s.push(finalized("d"));

    // Pre-commit: the pass would commit a+b and stop at the running entry.
    let scan = scan_frontier(&s, true);
    assert!(scan.will_commit);
    assert_eq!(scan.tail_start, 2);

    let n = commit_leading_run(&mut s, true, |_, _| true);
    assert_eq!(n, 2);
    assert_eq!(minimal_api::commit_scan_cursor(&s), scan.tail_start);

    // Post-commit: nothing left to commit; the tail starts at the cursor.
    let scan = scan_frontier(&s, true);
    assert!(!scan.will_commit);
    assert_eq!(scan.tail_start, 2);

    // Idle with no entries pending: everything committable.
    let scan = scan_frontier(&s, false);
    assert!(scan.will_commit);
    assert_eq!(scan.tail_start, 4);
}

#[test]
fn remove_from_below_frontier_then_push_still_commits() {
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(finalized("b"));
    s.push(finalized("c"));
    assert_eq!(commit_collect(&mut s), vec![0, 1, 2]);

    // Rewind: drop everything from index 1 (keep only "a"). Without the
    // cursor clamp this would strand the cursor at 3 and silently skip the
    // next pushes.
    let removed = s.remove_from(1);
    assert_eq!(removed.len(), 2);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 1);

    s.push(finalized("d")); // index 1
    assert_eq!(commit_collect(&mut s), vec![1]);
}

#[test]
fn btw_block_emits_once_across_repeated_frontier_passes() {
    let mut s = ScrollbackState::new();
    s.push(ScrollbackEntry::new(RenderBlock::Btw(
        echo_agent_pager::scrollback::blocks::BtwBlock::new("original question", "original answer"),
    )));

    let mut emitted = Vec::new();
    assert_eq!(
        commit_leading_run(&mut s, false, |state, i| {
            let RenderBlock::Btw(block) = &state.get(i).unwrap().block else {
                panic!("expected Btw block")
            };
            assert_eq!(block.question, "original question");
            assert_eq!(block.content().text(), "original answer");
            emitted.push(i);
            true
        }),
        1
    );
    assert!(minimal_api::is_committed(&s, s.get(0).unwrap()));

    assert_eq!(
        commit_leading_run(&mut s, false, |_, i| {
            emitted.push(i);
            true
        }),
        0
    );
    assert_eq!(emitted, vec![0]);
    assert!(!scan_frontier(&s, false).will_commit);
}

#[test]
fn commit_leading_run_advances_frontier_and_marks_committed_once() {
    let mut s = ScrollbackState::new();
    s.push(finalized("h1"));
    s.push(finalized("h2"));
    s.push(finalized("h3"));

    // Advances the frontier, marking the leading finalized run committed.
    let mut emitted = Vec::new();
    let n = commit_leading_run(&mut s, false, |_, i| {
        emitted.push(i);
        true
    });
    assert_eq!(n, 3);
    assert_eq!(emitted, vec![0, 1, 2]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 3);
    assert!((0..3).all(|i| minimal_api::is_committed(&s, s.get(i).unwrap())));

    // A second pass commits nothing (already-committed entries are skipped).
    let mut again = Vec::new();
    commit_leading_run(&mut s, false, |_, i| {
        again.push(i);
        true
    });
    assert!(again.is_empty());
}

#[test]
fn idle_turn_commits_past_stale_running_entry() {
    // Regression for the missing-edit/stuck-spinner bug: the agent tracker
    // can leave an entry's `is_running` flag set after the turn ends (e.g. a
    // thinking block whose finalize was missed at the thinking→tool
    // transition). While the turn runs, that entry correctly holds the
    // frontier; once the turn is idle the frontier must advance past it.
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    s.push(running("stale")); // stale is_running flag
    s.push(finalized("c"));

    // Running turn: blocked at the running entry.
    let mut seen = Vec::new();
    commit_leading_run(&mut s, true, |_, i| {
        seen.push(i);
        true
    });
    assert_eq!(seen, vec![0]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 1);

    // Idle turn: commit everything past the stale flag.
    let mut seen = Vec::new();
    commit_leading_run(&mut s, false, |_, i| {
        seen.push(i);
        true
    });
    assert_eq!(seen, vec![1, 2]);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 3);
}

#[test]
fn clear_resets_the_frontier() {
    let mut s = ScrollbackState::new();
    s.push(finalized("a"));
    commit_collect(&mut s);
    assert_eq!(minimal_api::commit_scan_cursor(&s), 1);

    s.clear();
    assert_eq!(minimal_api::commit_scan_cursor(&s), 0);
}

/// Height-exactness guard (design K5 / risk #1). `commit_active` reserves
/// exactly `desired_height(width)` rows via `insert_before`; if `render`
/// paints real content beyond that, those rows are silently clipped (lost)
/// from native scrollback. Render each block type into an over-tall buffer
/// and assert no non-space glyph lands past `desired_height`. (Background
/// fill of blank spaces past `h` is fine — only real content matters.)
fn assert_committed_fits(label: &str, block: RenderBlock, width: u16) {
    let mut entry = ScrollbackEntry::new(block);
    entry.set_display_mode(minimal_commit_display_mode(
        &entry.block,
        &default_appearance(),
    ));
    assert_committed_fits_entry(label, &entry, width);
}

/// [`assert_committed_fits`] for a caller-stamped display mode.
fn assert_committed_fits_entry(label: &str, entry: &ScrollbackEntry, width: u16) {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());

    let renderer = minimal_renderer(entry, &theme, appearance, test_cwd(), COMMITTED_TICK);
    let h = renderer.desired_height(width);
    assert!(h > 0, "{label}@{width}: desired_height was 0");
    // The accent bar and background fill intentionally stretch to the given
    // area height (chrome, not content). Only the content columns
    // (x >= chrome_width) carry real text that `insert_before` would clip.
    let chrome = renderer.chrome_width();

    let extra = 8u16;
    let area = Rect::new(0, 0, width, h + extra);
    let mut buf = Buffer::empty(area);
    renderer.render(area, &mut buf);

    for y in h..(h + extra) {
        for x in chrome..width {
            let sym = buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ");
            assert!(
                sym.trim().is_empty(),
                "{label}@{width}: content {sym:?} painted at row {y} col {x}, past \
                 desired_height {h} — insert_before would clip it from scrollback"
            );
        }
    }
}

#[test]
fn committed_block_uses_owning_session_cwd_for_tool_paths() {
    use ratatui::buffer::Buffer;

    let cwd = std::path::Path::new("/alternate/worktree");
    let mut entry =
        ScrollbackEntry::new(RenderBlock::edit("/alternate/worktree/src/main.rs", None));
    entry.set_display_mode(DisplayMode::Expanded);
    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());
    let renderer = minimal_renderer(&entry, &theme, appearance, cwd, COMMITTED_TICK);
    let width = 80;
    let height = renderer.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    renderer.render(area, &mut buf);

    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            text.push_str(buf[(x, y)].symbol());
        }
    }
    assert!(text.contains("src/main.rs"), "rendered text: {text:?}");
    assert!(
        !text.contains("/alternate/worktree"),
        "session prefix should be elided: {text:?}"
    );
}

#[test]
fn committed_blocks_fit_desired_height() {
    // Thinking blocks render zero rows unless `show_thinking_blocks` is on.
    // The toggle is a thread-local, so pin it on here so the thinking
    // block's committed height is actually exercised.
    minimal_api::set_show_thinking_blocks(true);

    let long = "Hello there — this is a longer message that should wrap across \
                several lines at narrow widths to exercise the wrapping math, with \
                enough words to overflow eighty columns comfortably.";
    for width in [40u16, 80, 120] {
        assert_committed_fits("user_prompt", RenderBlock::user_prompt(long), width);
        assert_committed_fits(
            "agent_message",
            RenderBlock::agent_message(format!(
                "{long}\n\n- bullet one\n- bullet two\n\n```rust\nfn main() {{}}\n```"
            )),
            width,
        );
        assert_committed_fits("thinking", RenderBlock::thinking(long), width);
        assert_committed_fits(
            "execute",
            RenderBlock::execute_with_output(
                "cargo build --release",
                "line 1\nline 2\nline 3\nline 4\nline 5\nline 6",
                None::<String>,
            ),
            width,
        );
        assert_committed_fits("edit", RenderBlock::edit("src/main.rs", None), width);
        assert_committed_fits("read", RenderBlock::read("src/main.rs", None), width);
        assert_committed_fits(
            "list_dir",
            RenderBlock::list_dir_with_output("src", "a.rs\nb.rs\nc.rs"),
            width,
        );
        assert_committed_fits("search", RenderBlock::search("TODO", 0, vec![]), width);
        assert_committed_fits("system", RenderBlock::system("Session restored"), width);
        assert_committed_fits("bg_task", RenderBlock::bg_task("sleep 30", "task-1"), width);
    }
}

/// Regression pinned: `md_style::to_anstyle` used to map `Color::Reset`
/// to a concrete ANSI-7 silver, washing out assistant/thinking markdown
/// body text on light terminals; `highlight_bash_command` leaked raw
/// syntect RGB.
#[test]
fn terminal_native_lock_paints_only_native_colors() {
    use echo_agent_pager::theme::cache as theme_cache;
    use ratatui::buffer::Buffer;

    let _guard = theme_cache::test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    struct LockReset;
    impl Drop for LockReset {
        fn drop(&mut self) {
            echo_agent_pager::theme::cache::set_terminal_native_lock(false);
        }
    }
    let _reset = LockReset;
    theme_cache::set_terminal_native_lock(true);
    minimal_api::set_show_thinking_blocks(true);

    let md = "Intro paragraph with **bold**, _italic_, `inline code`, and a \
              [link](https://example.com).\n\n# Heading one\n\n## Heading two\n\n\
              - item one\n- item two\n\n> a quote\n\n```rust\nfn main() { println!(\"hi\"); }\n```";
    use similar::ChangeTag;
    let hunk = vec![
        DiffLine {
            text: "let x = 1;\n".into(),
            lo: 1,
            ln: 1,
            tag: ChangeTag::Equal,
        },
        DiffLine {
            text: "let y = 2;\n".into(),
            lo: 2,
            ln: 0,
            tag: ChangeTag::Delete,
        },
        DiffLine {
            text: "let y = 3;\n".into(),
            lo: 0,
            ln: 2,
            tag: ChangeTag::Insert,
        },
    ];
    let blocks = vec![
        ("agent_message", RenderBlock::agent_message(md)),
        (
            "thinking",
            RenderBlock::thinking("Weighing the *options* with `care`."),
        ),
        ("user_prompt", RenderBlock::user_prompt("run the tests")),
        (
            "edit",
            RenderBlock::edit_with_hunks("src/main.rs", vec![hunk]),
        ),
        (
            "execute",
            RenderBlock::execute_with_output("cargo build", "ok\n", None::<String>),
        ),
        ("system", RenderBlock::system("Session restored")),
    ];

    for (label, block) in blocks {
        let mut entry = ScrollbackEntry::new(block);
        let theme = Theme::current();
        let appearance = committed_appearance(&AppearanceConfig::default());
        entry.set_display_mode(minimal_commit_display_mode(&entry.block, &appearance));
        let renderer = minimal_renderer(&entry, &theme, appearance, test_cwd(), COMMITTED_TICK);

        let width = 100u16;
        let h = renderer.desired_height(width).max(1);
        let area = Rect::new(0, 0, width, h);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        for y in 0..h {
            for x in 0..width {
                let Some(cell) = buf.cell((x, y)) else {
                    continue;
                };
                for (which, c) in [("fg", cell.fg), ("bg", cell.bg)] {
                    assert!(
                        !matches!(c, Color::Rgb(..) | Color::Indexed(_)),
                        "{label}: non-native {which} {c:?} at ({x},{y}) under \
                         symbol {:?} — minimal must only use Reset / named \
                         ANSI-16 so the terminal palette controls rendering",
                        cell.symbol()
                    );
                }
            }
        }
    }
}

#[test]
fn large_commit_is_capped_with_footer() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());
    // A tall block: a fenced code block keeps each line on its own row
    // (markdown would otherwise join soft-wrapped prose into one paragraph),
    // so the block is comfortably taller than the cap.
    let lines: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
    let body = format!("```\n{}\n```", lines.join("\n"));
    let mut entry = ScrollbackEntry::new(RenderBlock::agent_message(body));
    entry.set_display_mode(minimal_commit_display_mode(&entry.block, &appearance));

    let width = 80u16;
    let renderer = minimal_renderer(&entry, &theme, appearance, test_cwd(), COMMITTED_TICK);
    let full_h = renderer.desired_height(width);
    assert!(full_h > 12, "expected a tall block, got {full_h}");

    // Paint into a cap-height buffer (what `insert_committed` allocates).
    let cap = 12u16;
    let area = Rect::new(0, 0, width, cap);
    let mut buf = Buffer::empty(area);
    paint_committed(&mut buf, renderer, width, full_h, theme.dim());

    // The final row is the overflow footer naming the hidden line count and
    // pointing at /transcript; the buffer is exactly `cap` rows (bounded).
    let last: String = (0..width)
        .filter_map(|x| buf.cell((x, cap - 1)).map(|c| c.symbol().to_string()))
        .collect();
    assert!(last.contains("more lines"), "footer row: {last:?}");
    assert!(last.contains("/transcript"), "footer row: {last:?}");
    // A hidden-line count is present (full_h minus the kept content rows).
    let hidden = full_h - (cap - 1);
    assert!(
        last.contains(&hidden.to_string()),
        "footer should name {hidden} hidden lines: {last:?}"
    );
}

#[test]
fn small_commit_is_not_capped() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());
    let mut entry = ScrollbackEntry::new(RenderBlock::agent_message("one short line"));
    entry.set_display_mode(minimal_commit_display_mode(&entry.block, &appearance));

    let width = 80u16;
    let renderer = minimal_renderer(&entry, &theme, appearance, test_cwd(), COMMITTED_TICK);
    let full_h = renderer.desired_height(width);

    // Buffer is exactly the block's height → no footer (uncapped path).
    let area = Rect::new(0, 0, width, full_h);
    let mut buf = Buffer::empty(area);
    paint_committed(&mut buf, renderer, width, full_h, theme.dim());

    let mut all = String::new();
    for y in 0..full_h {
        for x in 0..width {
            all.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
        }
    }
    assert!(
        !all.contains("more lines"),
        "no footer when uncapped: {all:?}"
    );
}

#[test]
fn committed_edit_keeps_diff_line_backgrounds() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use similar::ChangeTag;

    let hunk = vec![
        DiffLine {
            text: "let x = 1;\n".into(),
            lo: 10,
            ln: 10,
            tag: ChangeTag::Equal,
        },
        DiffLine {
            text: "let y = 2;\n".into(),
            lo: 11,
            ln: 0,
            tag: ChangeTag::Delete,
        },
        DiffLine {
            text: "let y = 3;\n".into(),
            lo: 0,
            ln: 11,
            tag: ChangeTag::Insert,
        },
        DiffLine {
            text: "let z = 4;\n".into(),
            lo: 12,
            ln: 12,
            tag: ChangeTag::Equal,
        },
    ];
    let block = RenderBlock::edit_with_hunks("src/main.rs", vec![hunk]);
    let mut entry = ScrollbackEntry::new(block);
    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());
    entry.set_display_mode(minimal_commit_display_mode(&entry.block, &appearance));
    let renderer = minimal_renderer(&entry, &theme, appearance, test_cwd(), COMMITTED_TICK);

    let width = 80u16;
    let h = renderer.desired_height(width);
    let area = Rect::new(0, 0, width, h);
    let mut buf = Buffer::empty(area);
    renderer.render(area, &mut buf);

    // The committed edit uses a flat background (terminal transparency), but
    // must still paint the per-line diff backgrounds — otherwise an added /
    // removed line is indistinguishable from context.
    let mut saw_insert = false;
    let mut saw_delete = false;
    for y in 0..h {
        for x in 0..width {
            if let Some(cell) = buf.cell((x, y)) {
                saw_insert |= cell.bg == theme.diff_insert_bg;
                saw_delete |= cell.bg == theme.diff_delete_bg;
            }
        }
    }
    assert!(
        saw_insert,
        "committed edit lost the insert (green) diff background"
    );
    assert!(
        saw_delete,
        "committed edit lost the delete (red) diff background"
    );
}

/// Asserted through `chrome_width` because that is what both `desired_height`
/// and `render` subtract from the wrap width — one column is the whole cost of
/// the rail, which is why restoring it is height-safe (K5).
#[test]
fn only_thinking_spends_the_accent_column() {
    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());
    let chrome = |entry: &ScrollbackEntry| {
        minimal_renderer(
            entry,
            &theme,
            appearance.clone(),
            test_cwd(),
            COMMITTED_TICK,
        )
        .chrome_width()
    };

    assert_eq!(
        chrome(&ScrollbackEntry::new(RenderBlock::thinking("reasoning"))),
        1,
        "reasoning reserves the 1-col accent gutter"
    );

    for block in [
        RenderBlock::agent_message("answer"),
        RenderBlock::user_prompt("ask"),
        RenderBlock::execute("ls"),
        RenderBlock::read("src/lib.rs", None),
        RenderBlock::edit("src/main.rs", None),
        RenderBlock::system("Session restored"),
    ] {
        let entry = ScrollbackEntry::new(block);
        assert_eq!(
            chrome(&entry),
            0,
            "only reasoning may spend the accent column: {:?}",
            entry.block
        );
    }

    // A column is reserved only where the block actually paints one. Reserving
    // without painting leaves the content indented over a blank gutter — which
    // is what a collapsed reasoning header did while `hide_accent` keyed off
    // the block type alone.
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let rail = echo_agent_pager::glyphs::accent_bar();
    minimal_api::set_show_thinking_blocks(true);
    for mode in [
        DisplayMode::Collapsed,
        DisplayMode::Truncated,
        DisplayMode::Expanded,
    ] {
        let mut entry = ScrollbackEntry::new(RenderBlock::thinking(
            "reasoning long enough to wrap over a couple of rows at sixty columns",
        ));
        entry.set_display_mode(mode);

        let renderer = minimal_renderer(
            &entry,
            &theme,
            appearance.clone(),
            test_cwd(),
            COMMITTED_TICK,
        );
        let reserved = renderer.chrome_width();
        let h = renderer.desired_height(60);
        let area = Rect::new(0, 0, 60, h);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);
        let painted = buf.cell((0, 0)).expect("first cell").symbol() == rail;

        assert_eq!(
            reserved == 1,
            painted,
            "{mode:?}: reserved a column={} but painted the rail={painted} — a \
             reserved-but-unpainted column is a blank indent",
            reserved == 1,
        );
    }
}

#[test]
fn committed_thinking_paints_a_dim_rail_in_column_zero() {
    use echo_agent_pager::theme::cache as theme_cache;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    let _guard = theme_cache::test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    struct LockReset;
    impl Drop for LockReset {
        fn drop(&mut self) {
            echo_agent_pager::theme::cache::set_terminal_native_lock(false);
        }
    }
    let _reset = LockReset;
    theme_cache::set_terminal_native_lock(true);
    minimal_api::set_show_thinking_blocks(true);

    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());
    let mut entry = ScrollbackEntry::new(RenderBlock::thinking(
        "a reasoning body long enough to wrap over several rows at this width",
    ));
    entry.set_display_mode(minimal_commit_display_mode(&entry.block, &appearance));

    let width = 40u16;
    let renderer = minimal_renderer(
        &entry,
        &theme,
        appearance.clone(),
        test_cwd(),
        COMMITTED_TICK,
    );
    let h = renderer.desired_height(width);
    assert!(h > 1, "expected a multi-row reasoning block, got {h}");
    let area = Rect::new(0, 0, width, h);
    let mut buf = Buffer::empty(area);
    renderer.render(area, &mut buf);

    let rail = echo_agent_pager::glyphs::accent_bar();
    for y in 0..h {
        let cell = buf.cell((0, y)).expect("accent cell");
        assert_eq!(cell.symbol(), rail, "row {y} lost the rail");
        assert!(
            cell.modifier.contains(Modifier::DIM),
            "row {y}: rail must be dim, got {:?}",
            cell.modifier
        );
    }

    let answer = ScrollbackEntry::new(RenderBlock::agent_message("the answer"));
    let renderer = minimal_renderer(&answer, &theme, appearance, test_cwd(), COMMITTED_TICK);
    let area = Rect::new(0, 0, width, renderer.desired_height(width).max(1));
    let mut buf = Buffer::empty(area);
    renderer.render(area, &mut buf);
    assert_ne!(
        buf.cell((0, 0)).expect("first cell").symbol(),
        rail,
        "assistant output must not wear a rail"
    );
}

#[test]
fn commit_display_mode_policy() {
    assert_eq!(
        minimal_commit_display_mode(&RenderBlock::thinking("reasoning"), &default_appearance()),
        DisplayMode::Expanded
    );
    assert_eq!(
        minimal_commit_display_mode(&RenderBlock::edit("file.rs", None), &default_appearance()),
        DisplayMode::Expanded
    );
    assert_eq!(
        minimal_commit_display_mode(&RenderBlock::execute("ls"), &default_appearance()),
        DisplayMode::Truncated
    );
    assert_eq!(
        minimal_commit_display_mode(&RenderBlock::agent_message("hi"), &default_appearance()),
        DisplayMode::Expanded
    );
}

#[test]
fn commit_display_mode_lookups_collapse_on_success_only() {
    use echo_agent_pager::scrollback::blocks::{
        ListDirToolCallBlock, ReadToolCallBlock, SearchToolCallBlock,
    };

    assert_eq!(
        minimal_commit_display_mode(
            &RenderBlock::search("pat", 3, vec![]),
            &default_appearance()
        ),
        DisplayMode::Collapsed
    );
    assert_eq!(
        minimal_commit_display_mode(
            &RenderBlock::read("src/lib.rs", None),
            &default_appearance()
        ),
        DisplayMode::Collapsed
    );
    assert_eq!(
        minimal_commit_display_mode(
            &RenderBlock::list_dir_with_output("src", "a.rs\nb.rs"),
            &default_appearance()
        ),
        DisplayMode::Collapsed
    );

    for failed in [
        RenderBlock::ToolCall(ToolCallBlock::Search(
            SearchToolCallBlock::new("pat").with_error("regex parse error"),
        )),
        RenderBlock::ToolCall(ToolCallBlock::Read(
            ReadToolCallBlock::new("gone.rs").with_error("file not found"),
        )),
        RenderBlock::ToolCall(ToolCallBlock::ListDir(
            ListDirToolCallBlock::new("gone/").with_error("no such directory"),
        )),
    ] {
        assert_eq!(
            minimal_commit_display_mode(&failed, &default_appearance()),
            DisplayMode::Truncated,
            "failed lookup must stay truncated: {failed:?}"
        );
    }
}

#[test]
fn collapse_thinking_toggle_flips_only_reasoning() {
    assert_eq!(
        minimal_commit_display_mode(&RenderBlock::thinking("reasoning"), &default_appearance()),
        DisplayMode::Expanded,
        "default (K9): reasoning commits in full"
    );
    assert_eq!(
        minimal_commit_display_mode(&RenderBlock::thinking("reasoning"), &collapsed_appearance()),
        DisplayMode::Collapsed
    );

    // Everything else is unaffected by the toggle.
    for block in [
        RenderBlock::agent_message("hi"),
        RenderBlock::edit("file.rs", None),
        RenderBlock::execute("ls"),
        RenderBlock::read("src/lib.rs", None),
    ] {
        assert_eq!(
            minimal_commit_display_mode(&block, &collapsed_appearance()),
            minimal_commit_display_mode(&block, &default_appearance()),
            "the toggle must only touch reasoning: {block:?}"
        );
    }
}

#[test]
fn collapsed_thinking_commit_is_one_advertised_row() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    minimal_api::set_show_thinking_blocks(true);
    let theme = Theme::current();
    let appearance = committed_appearance(&AppearanceConfig::default());

    let mut entry = ScrollbackEntry::new(RenderBlock::thinking(
        "a long reasoning body that would otherwise occupy many rows in the transcript",
    ));
    entry.set_display_mode(minimal_commit_display_mode(
        &entry.block,
        &collapsed_appearance(),
    ));
    assert_eq!(entry.display_mode(), DisplayMode::Collapsed);
    // The mode `commit_active` records for the Ctrl+E ring.
    assert!(matches!(
        entry.display_mode(),
        DisplayMode::Collapsed | DisplayMode::Truncated
    ));

    for width in [40u16, 80, 120] {
        let renderer = minimal_renderer(
            &entry,
            &theme,
            appearance.clone(),
            test_cwd(),
            COMMITTED_TICK,
        );
        let h = renderer.desired_height(width);
        assert_eq!(h, 1, "collapsed reasoning is one row @{width}");
        let area = Rect::new(0, 0, width, h);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);
        let row: String = (0..width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(row.contains("Thought"), "@{width}: {row:?}");
        assert!(
            row.contains("ctrl+e to expand"),
            "@{width}: the only way into a print-once folded block must be \
             advertised: {row:?}"
        );
    }

    // Too narrow for the hint: the header still wins, still one row.
    let renderer = minimal_renderer(&entry, &theme, appearance, test_cwd(), COMMITTED_TICK);
    assert_eq!(renderer.desired_height(16), 1);
}

/// K5 guard: reasoning committed collapsed must still fit its reserved
/// `insert_before` height.
#[test]
fn collapsed_thinking_commit_fits_desired_height() {
    minimal_api::set_show_thinking_blocks(true);
    let long = "Reasoning that is long enough to wrap several times at forty \
                columns, with a `code span` and **emphasis** to exercise the \
                markdown spans under the dim+italic patch.";
    for width in [40u16, 80, 120] {
        let mut entry = ScrollbackEntry::new(RenderBlock::thinking(long));
        entry.set_display_mode(minimal_commit_display_mode(
            &entry.block,
            &collapsed_appearance(),
        ));
        assert_committed_fits_entry("thinking_collapsed", &entry, width);
    }
}
