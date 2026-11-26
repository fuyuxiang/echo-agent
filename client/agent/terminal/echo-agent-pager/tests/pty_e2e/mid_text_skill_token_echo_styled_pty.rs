// Per-test-case module for the `pty_e2e` integration test crate.
//
// A plain prompt with a mid-text `/skill` token must keep
// the composer's teal token highlight in the scrollback echo — live, with the
// real shell advertising the skill from an on-disk SKILL.md.
#[allow(unused_imports)]
use super::common::*;

use echo_agent_pager_pty_harness::StyledLine;

/// Typed prompt: plain text with the skill referenced mid-message.
const TYPED: &str = "great /test-skill do it";
/// The token the composer (and the echo) must render in the skill accent.
const TOKEN: &str = "/test-skill";
/// Leading word of the prompt — must stay in the plain body color.
const BODY_WORD: &str = "great";

const DONE_SENTINEL: &str = "MIDTEXT_SKILL_ECHO_DONE";

/// On the first screen row whose text contains `row_marker`, return the fg of
/// the styled run containing `needle`. `None` until the row (or run) exists.
fn run_fg_on_row(rows: &[StyledLine], row_marker: &str, needle: &str) -> Option<Option<String>> {
    for row in rows {
        let text: String = row.runs.iter().map(|r| r.text.as_str()).collect();
        if !text.contains(row_marker) {
            continue;
        }
        for run in &row.runs {
            if run.text.contains(needle) {
                return Some(run.fg.clone());
            }
        }
    }
    None
}

/// Seed a user-invocable skill under `dir/.echo-agent/skills`. The workspace must
/// be a git repo: the live session advertises workspace-local skills only for
/// git workspaces (offline `echoagent inspect` scans plain dirs too) — same shape
/// as the mid_text_skill_token_echo.yaml scenario's `git_init: true`.
fn seed_test_skill(dir: &Path) {
    let skill_dir = dir.join(".echo-agent").join("skills").join("test-skill");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill for echo styling.\n---\n\nDo the thing.\n",
    )
    .expect("write SKILL.md");
}

/// Type `great /test-skill do it`, wait for the composer to highlight the
/// advertised skill token, submit, and assert the scrollback echo renders the
/// SAME token in the same accent fg while the body word stays a different fg.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn mid_text_skill_token_echo_styled_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{DONE_SENTINEL} echo styling verified."));
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    git2::Repository::init(workspace.path()).expect("git init workspace");
    seed_test_skill(workspace.path());

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--trust"],
        Some(workspace.path()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // The first char promotes the welcome prompt to a session.
    inject_keys_paced(&mut harness, TYPED.as_bytes());
    harness
        .wait_for_text(TYPED, Duration::from_secs(10))
        .expect("typed prompt echoed in the composer");

    // Wait for the shell to advertise the skill: once the registry syncs, the
    // composer restyles the token — its fg diverges from the body word's fg.
    let composer_deadline = Instant::now() + Duration::from_secs(30);
    let composer_token_fg = loop {
        harness.update(Duration::from_millis(100));
        let rows = harness.screen_styled();
        let token_fg = run_fg_on_row(&rows, TYPED, TOKEN);
        let body_fg = run_fg_on_row(&rows, TYPED, BODY_WORD);
        if let (Some(token_fg), Some(body_fg)) = (token_fg, body_fg)
            && token_fg != body_fg
        {
            break token_fg;
        }
        assert!(
            Instant::now() < composer_deadline,
            "composer never highlighted {TOKEN} (skill not advertised?)\nscreen:\n{}",
            harness.screen_contents()
        );
    };

    harness.inject_keys(b"\r").expect("submit prompt");
    harness
        .wait_for_text(DONE_SENTINEL, Duration::from_secs(30))
        .expect("mock response rendered (turn finished)");

    // The composer cleared on submit, so the only row with the full typed
    // text is the scrollback echo. It may take a paint to settle.
    let echo_deadline = Instant::now() + Duration::from_secs(10);
    let (echo_token_fg, echo_body_fg) = loop {
        harness.update(Duration::from_millis(100));
        let rows = harness.screen_styled();
        if let (Some(token_fg), Some(body_fg)) = (
            run_fg_on_row(&rows, TYPED, TOKEN),
            run_fg_on_row(&rows, TYPED, BODY_WORD),
        ) {
            break (token_fg, body_fg);
        }
        assert!(
            Instant::now() < echo_deadline,
            "echo row never painted\nscreen:\n{}",
            harness.screen_contents()
        );
    };

    assert_ne!(
        echo_token_fg,
        echo_body_fg,
        "echo must style {TOKEN} differently from the body\nscreen:\n{}",
        harness.screen_contents()
    );
    assert_eq!(
        echo_token_fg,
        composer_token_fg,
        "echo token fg must match the composer's highlight fg\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
