//! Declarative end-to-end TUI scenario tests.
//!
//! These tests exercise the real `echo-agent-pager` binary through a PTY using
//! YAML scenarios under `tests/scenarios/`. They are ignored by default because
//! they build/spawn the pager and stream through a mock inference server.

use std::path::PathBuf;

use echo_agent_pager_pty_harness::{
    ScriptedRunConfig, ScriptedRunStatus, ScriptedScenario, ScriptedScenarioRunner, pager_binary,
};

fn scenario_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("scenarios")
        .join(name)
}

async fn run_scenario(name: &str) {
    let scenario = ScriptedScenario::from_file(&scenario_path(name)).expect("load scenario");
    let artifact_dir = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("scripted-scenarios");
    let runner = ScriptedScenarioRunner::new(ScriptedRunConfig::new(
        pager_binary().expect("resolve pager binary"),
        artifact_dir,
    ));
    let report = runner.run(&scenario).await.expect("run scenario");

    assert_eq!(
        report.status,
        ScriptedRunStatus::Passed,
        "scenario {} did not pass; report: {report:#?}",
        report.scenario,
    );
    assert!(
        report.bugs.is_empty(),
        "scenario {} identified bugs: {:#?}",
        report.scenario,
        report.bugs
    );
    assert!(
        report
            .steps
            .iter()
            .any(|step| step.action == "screenshot" && !step.artifacts.is_empty()),
        "scenario {} did not capture screenshots/artifacts",
        report.scenario
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_welcome_screen() {
    run_scenario("welcome.yaml").await;
}

/// Resize while slash dropdown is open must not kill the pager.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_slash_resize_storm() {
    run_scenario("slash_resize_storm.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_release_notes_scroll() {
    run_scenario("release_notes_scroll.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_mock_response() {
    run_scenario("mock_response.yaml").await;
}

/// GBT-6092: Esc on `/agents` opened from a dashboard conversation peek must
/// close the modal and stay in the overlay, not pop back to the dashboard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_dashboard_overlay_agents_esc() {
    run_scenario("dashboard_overlay_agents_esc.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_input_modalities() {
    run_scenario("input_modalities.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_ansi_execute_output() {
    run_scenario("ansi_execute_output.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_copy_selection() {
    run_scenario("copy_selection.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_table_cell_selection() {
    run_scenario("table_cell_selection.yaml").await;
}

/// Type-to-find pickers carry vim-mode. With vim-mode ON the command
/// palette opens in INPUT (a letter filters immediately); Esc clears the query
/// then a second Esc drops to NAV (letters no longer filter); `i` re-enters
/// INPUT. The footer's `i search` hint is absent on open (input) and present in
/// nav.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_vim_modal_command_palette() {
    run_scenario("vim_modal_command_palette.yaml").await;
}

/// Tool header selection copies only the operand (path/command), not the
/// `Read ` / `Run ` / `$ ` label — regression guard for Selectable::Spans on
/// tool-call headers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_tool_header_path_selection() {
    run_scenario("tool_header_path_selection.yaml").await;
}

/// Typed `[` must appear in the prompt without a follow-up keystroke.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_bracket_prompt_input() {
    run_scenario("bracket_prompt_input.yaml").await;
}

/// Double-clicking a `[Pasted: N lines]` chip expands it into plain
/// editable prompt text (user feedback: clicking the chip means "edit").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_paste_chip_double_click() {
    run_scenario("paste_chip_double_click.yaml").await;
}

/// Pasting identical content twice expands the chip instead of stacking a
/// duplicate one ("paste again" means "show the text").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_paste_chip_repaste() {
    run_scenario("paste_chip_repaste.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scripted_image_inputs() {
    run_scenario("image_inputs.yaml").await;
}

/// Path-free image-chip preview and dismissal on a non-graphics PTY.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_image_chip_preview_path_free() {
    run_scenario("image_chip_preview_path_free.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_image_normalize_corrupt_dropped() {
    run_scenario("image_normalize_corrupt_dropped.yaml").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_image_normalize_persist_atomic() {
    run_scenario("image_normalize_persist_atomic.yaml").await;
}

/// Mermaid fence (the engine is always compiled in) → inline Unicode-art diagram
/// + clickable affordance row ([Open Image] [Copy Image Path] [Copy Source]) and
/// NO inline image. The diagram renders as text art and the run never panics.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_mermaid_affordances() {
    run_scenario("mermaid-affordances.yaml").await;
}

/// User report: file path with a space (`Demo App.app`) must render
/// fully and the echo-agent-pty/PTY harness must carry OSC 8 for the *full* path
/// (`Demo%20App.app`), not a truncated link ending at `Demo`.
/// Screen assertions live in `path_space_hyperlink.yaml`; this wrapper also
/// checks `raw_output.bin` so CI has byte-level proof without only relying on
/// the ignored `pty_e2e` test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_path_space_hyperlink() {
    const FILE_URL_MARKER: &str = "Demo%20App.app";

    let scenario = ScriptedScenario::from_file(&scenario_path("path_space_hyperlink.yaml"))
        .expect("load scenario");
    let artifact_dir = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("scripted-scenarios");
    let runner = ScriptedScenarioRunner::new(ScriptedRunConfig::new(
        pager_binary().expect("resolve pager binary"),
        artifact_dir,
    ));
    let report = runner.run(&scenario).await.expect("run scenario");

    let raw_path = report.artifact_dir.join("raw_output.bin");
    let raw_bytes = std::fs::read(&raw_path).unwrap_or_else(|err| {
        panic!(
            "raw_output.bin missing at {} (report status: {:?}): {err}",
            raw_path.display(),
            report.status
        );
    });
    let raw = String::from_utf8_lossy(&raw_bytes);

    eprintln!(
        "\n=== PTY path-space hyperlink (scripted_path_space_hyperlink) ===\n\
         scenario status: {:?}\n\
         raw PTY bytes:   {}\n\
         OSC 8 present:   {}\n\
         full URL marker: {}\n\
         ===============================================================\n",
        report.status,
        raw_bytes.len(),
        raw.contains("\x1b]8;"),
        raw.contains(FILE_URL_MARKER),
    );

    assert_eq!(
        report.status,
        ScriptedRunStatus::Passed,
        "scenario failed: {:#?}",
        report.bugs
    );
    assert!(
        raw.contains("\x1b]8;"),
        "expected OSC 8 hyperlink sequences in echo-agent-pty PTY stream (path should be clickable)"
    );
    assert!(
        raw.contains(FILE_URL_MARKER),
        "OSC 8 file:// URL must include `{FILE_URL_MARKER}` so click/underline covers \
         `Demo App.app`, not just `Demo` (pre-fix truncated at the space).\n\
         truncated-only would omit this marker."
    );
    let has_truncated_only =
        raw.contains("mac-arm64/Demo\x07") || raw.contains("mac-arm64/Demo\x1b\\");
    assert!(
        !has_truncated_only || raw.contains(FILE_URL_MARKER),
        "must not emit a truncated OSC 8 ending at `Demo` without the space suffix"
    );
}

/// Count Kitty APC action tokens in raw PTY bytes.
fn count_kitty_actions(raw: &[u8]) -> (usize, usize, usize) {
    let text = String::from_utf8_lossy(raw);
    let mut transmit_display = 0usize;
    let mut transmit_only = 0usize;
    let mut place_only = 0usize;
    for part in text.split("\x1b_G") {
        if part.is_empty() {
            continue;
        }
        let params = part.split(';').next().unwrap_or("");
        if params.contains("a=T") {
            transmit_display += 1;
        } else if params.contains("a=t") {
            transmit_only += 1;
        }
        if params.contains("a=p") {
            place_only += 1;
        }
    }
    (transmit_display, transmit_only, place_only)
}

/// PTY harness: paste one PNG, hammer redraws, prove uploads stay bounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_inline_image_memory() {
    let scenario = ScriptedScenario::from_file(&scenario_path("inline_image_memory.yaml"))
        .expect("load scenario");
    let artifact_dir = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("scripted-scenarios");
    let runner = ScriptedScenarioRunner::new(ScriptedRunConfig::new(
        pager_binary().expect("resolve pager binary"),
        artifact_dir.clone(),
    ));
    let report = runner.run(&scenario).await.expect("run scenario");

    // Harness writes raw bytes under the per-run artifact dir (see scripted.rs).
    let raw_path = report.artifact_dir.join("raw_output.bin");
    let raw = std::fs::read(&raw_path).unwrap_or_else(|err| {
        panic!(
            "raw_output.bin missing at {} (report status: {:?}): {err}",
            raw_path.display(),
            report.status
        );
    });

    let (at, a_t, a_p) = count_kitty_actions(&raw);
    eprintln!(
        "\n=== PTY inline image memory (scripted_inline_image_memory) ===\n\
         scenario status: {:?}\n\
         raw PTY bytes:   {}\n\
         Kitty a=T (combined upload+display): {at}\n\
         Kitty a=t (transmit only):           {a_t}\n\
         Kitty a=p (place only):              {a_p}\n\
         ===============================================================\n",
        report.status,
        raw.len()
    );

    assert!(
        at + a_t + a_p > 0,
        "expected Kitty graphics in raw PTY output"
    );

    let uploads = at + a_t;
    assert!(
        uploads < 30,
        "too many image upload APCs (a=T + a=t = {uploads}); place={a_p}"
    );
    if a_p > 0 {
        assert!(
            a_p >= uploads,
            "expected place-only (a=p={a_p}) >= uploads (a=T+a=t={uploads})"
        );
    }

    assert_eq!(
        report.status,
        ScriptedRunStatus::Passed,
        "scenario failed: {:#?}",
        report.bugs
    );
}

/// enterprise deploy report: with `ECHO_AGENT_GOAL=1`, `/goal` must
/// show in the slash menu on the welcome screen *before* the first user turn.
/// The scenario types `/goal` pre-session and asserts the dropdown carries the
/// builtin's description (which only renders when the command is advertised),
/// then captures screenshot artifacts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_goal_slash_presession() {
    run_scenario("goal_slash_presession.yaml").await;
}

/// Counterpart to `scripted_goal_slash_presession`: with the goal flag
/// explicitly off (`ECHO_AGENT_GOAL=0`; goal mode defaults on), `/goal` must stay
/// hidden pre-session (gate fail-closed) while an
/// AlwaysOn builtin (`/compact`) still shows — proving it's the gate, not a
/// broken dropdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_goal_slash_presession_disabled() {
    run_scenario("goal_slash_presession_disabled.yaml").await;
}

/// Full folder-trust session: with `ECHO_AGENT_FOLDER_TRUST=1` and a git repo that
/// ships a repo-local `.mcp.json` (declared via the scenario `workspace`), the
/// trust question renders before any session, accepting it (`y`) lets the
/// session proceed, and a submitted prompt streams the mock response. The
/// declarative counterpart to the programmatic `folder_trust_*` PTY tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_folder_trust_prompt() {
    run_scenario("folder_trust_prompt.yaml").await;
}

/// Dashboard `/model` list: mouse-clicking a model must accept the completion
/// into the dispatch prompt and must not attach the session row under the
/// dropdown (click-through regression).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_dashboard_model_list_click() {
    run_scenario("dashboard_model_list_click.yaml").await;
}

/// Shortcuts cheatsheet end-to-end: open the modal, inline-expand a hint (→) and
/// collapse it (←), open the man-style detail screen (Enter) showing long_help,
/// return to browse (Esc), and close with the global Ctrl+X chord.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_shortcuts_help_detail() {
    run_scenario("shortcuts_help_detail.yaml").await;
}

/// Undo tip happy path: a substantial Ctrl+U wipe shows the "… to undo"
/// banner (which then clears on its own TTL — no early retire on undo).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_undo_tip_clear_shows() {
    run_scenario("undo_tip_clear_shows.yaml").await;
}

/// Undo tip Ctrl+C variant: clearing a substantial draft with Ctrl+C shows
/// the banner too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_undo_tip_ctrl_c_clear_shows() {
    run_scenario("undo_tip_ctrl_c_clear_shows.yaml").await;
}

/// Undo tip no-show edge: wiping a short (<20 char) draft shows nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_undo_tip_short_draft_no_show() {
    run_scenario("undo_tip_short_draft_no_show.yaml").await;
}

/// Undo tip no-show edge: on a short terminal the renderability gate refuses
/// the banner even after a substantial wipe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_undo_tip_short_terminal_no_show() {
    run_scenario("undo_tip_short_terminal_no_show.yaml").await;
}

/// Undo tip no-show edge: accepting an @-file completion is not a wipe and
/// must not hint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_undo_tip_completion_accept_no_show() {
    run_scenario("undo_tip_completion_accept_no_show.yaml").await;
}

/// Plan-nudge happy path: with contextual hints enabled, typing a planning
/// keyword shows the "Planning? Check out plan mode via shift+tab" banner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_plan_nudge_shows() {
    run_scenario("plan_nudge_shows.yaml").await;
}

/// Plan-nudge opt-out edge: contextual hints ship default-OFF, so this scenario
/// also pins `ECHO_AGENT_CONTEXTUAL_HINTS=0` (belt-and-suspenders); the same planning
/// keyword shows nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_plan_nudge_opt_out_no_show() {
    run_scenario("plan_nudge_opt_out_no_show.yaml").await;
}

/// Follow-up: a mid-message `/test-skill` token stays teal in the
/// scrollback echo after submit (screenshots capture composer + echo styling).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_mid_text_skill_token_echo() {
    run_scenario("mid_text_skill_token_echo.yaml").await;
}

/// Auto-compact: shrinking to 14 rows drops the sticky prev-question header
/// (compact chrome engages); growing back to 32 rows restores it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_auto_compact_resize() {
    run_scenario("auto_compact_resize.yaml").await;
}

/// Small-screen tip happy path: a 24-row terminal (in the 21..=28 band) shows
/// the one-shot "Tight on space? Try /compact-mode" banner, which expires on
/// its TTL and never re-shows after resizes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_small_screen_tip_band() {
    run_scenario("small_screen_tip_band.yaml").await;
}

/// Small-screen tip no-show edge: a 40-row terminal is above the band.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_small_screen_tip_no_show_tall() {
    run_scenario("small_screen_tip_no_show_tall.yaml").await;
}

/// Small-screen tip no-show edge: at 14 rows auto-compact engages instead and
/// the banner row cannot render.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_small_screen_tip_no_show_tiny() {
    run_scenario("small_screen_tip_no_show_tiny.yaml").await;
}

/// Inline edit-and-resubmit happy path: Enter on a selected previous prompt
/// opens the in-place editor; Enter with changed text raises the confirm
/// dialog; `y` rewinds the conversation and resubmits the edited text as a
/// fresh turn (distinct per-turn mock sentinels prove the tail was
/// truncated), with no "Reverted conversation" note.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_inline_edit_resubmit() {
    run_scenario("inline_edit_resubmit.yaml").await;
}

/// Enter on an unchanged inline edit just closes the editor: no rewind
/// popup, no resubmit, transcript untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_inline_edit_unchanged_exit() {
    run_scenario("inline_edit_unchanged_exit.yaml").await;
}

/// Esc from the inline resubmit popup returns to the still-open editor with
/// the edit intact; a second Esc discards the edit and restores the original
/// prompt text in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "scripted scenario; run with cargo test -- --ignored"]
async fn scripted_inline_edit_dismiss_returns_to_editor() {
    run_scenario("inline_edit_dismiss_returns_to_editor.yaml").await;
}

/// Sanity-check: every scenario YAML in the list below must parse. Runs at
/// `cargo test` time without `--ignored` so a malformed YAML breaks CI
/// immediately rather than only when the scripted runner is opted in.
#[test]
fn scenarios_parse() {
    for name in [
        "image_normalize_corrupt_dropped.yaml",
        "image_normalize_persist_atomic.yaml",
        "mermaid-affordances.yaml",
        "path_space_hyperlink.yaml",
        "goal_slash_presession.yaml",
        "goal_slash_presession_disabled.yaml",
        "folder_trust_prompt.yaml",
        "dashboard_model_list_click.yaml",
        "dashboard_overlay_agents_esc.yaml",
        "paste_chip_double_click.yaml",
        "paste_chip_repaste.yaml",
        "slash_resize_storm.yaml",
        "shortcuts_help_detail.yaml",
        "undo_tip_clear_shows.yaml",
        "undo_tip_ctrl_c_clear_shows.yaml",
        "undo_tip_short_draft_no_show.yaml",
        "undo_tip_short_terminal_no_show.yaml",
        "undo_tip_completion_accept_no_show.yaml",
        "plan_nudge_shows.yaml",
        "plan_nudge_opt_out_no_show.yaml",
        "mid_text_skill_token_echo.yaml",
        "auto_compact_resize.yaml",
        "small_screen_tip_band.yaml",
        "small_screen_tip_no_show_tall.yaml",
        "small_screen_tip_no_show_tiny.yaml",
        "inline_edit_resubmit.yaml",
        "inline_edit_unchanged_exit.yaml",
        "inline_edit_dismiss_returns_to_editor.yaml",
    ] {
        let path = scenario_path(name);
        let scenario =
            ScriptedScenario::from_file(&path).unwrap_or_else(|err| panic!("parse {name}: {err}"));
        assert!(!scenario.steps.is_empty(), "{name} has no steps");
    }
}

#[test]
fn ansi_execute_output_scenario_parses() {
    let path = scenario_path("ansi_execute_output.yaml");
    let scenario = ScriptedScenario::from_file(&path).unwrap_or_else(|err| panic!("parse: {err}"));
    assert!(!scenario.steps.is_empty(), "scenario has no steps");
}

#[test]
fn vim_modal_command_palette_scenario_parses() {
    let path = scenario_path("vim_modal_command_palette.yaml");
    let scenario = ScriptedScenario::from_file(&path).unwrap_or_else(|err| panic!("parse: {err}"));
    assert!(!scenario.steps.is_empty(), "scenario has no steps");
}
