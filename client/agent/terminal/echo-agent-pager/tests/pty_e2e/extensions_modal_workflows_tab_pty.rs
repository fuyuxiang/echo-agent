// Per-test-case module for the `pty_e2e` integration test crate.
//
// The extensions modal's Workflows tab lists a seeded user workflow as a
// flat browse-only row. Run with `--nocapture` to dump screen contents
// when debugging failures.
#[allow(unused_imports)]
use super::common::*;

const SEEDED_WORKFLOW: &str = "demo-workflow";

fn seed_user_workflow(content: &ContentController) {
    let workflows_dir = content.home().join(".echo-agent").join("workflows");
    std::fs::create_dir_all(&workflows_dir).expect("create workflows dir");
    std::fs::write(
        workflows_dir.join(format!("{SEEDED_WORKFLOW}.rhai")),
        format!(
            "let meta = #{{ name: \"{SEEDED_WORKFLOW}\", description: \"extensions modal workflows fixture\" }};\ncomplete(\"ok\");\n"
        ),
    )
    .expect("write workflow script");
}

fn dump_screen(label: &str, harness: &PtyHarness) {
    let screen = harness.screen_contents();
    eprintln!(
        "\n========== PTY CAPTURE: {label} ==========\n{screen}\n========== END: {label} ==========\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn extensions_modal_workflows_tab_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_user_workflow(&content);

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness.inject_keys(b"/plugins\r").expect("submit /plugins");
    harness
        .wait_for_text("Workflows", Duration::from_secs(15))
        .expect("extensions modal tab bar with Workflows");

    // Navigate by content, not tab position: cycle until the seeded catalog
    // row shows (it renders only on the Workflows tab), so tab insertions or
    // reorders can't silently land the assertions on the wrong tab.
    let mut reached_workflows_tab = false;
    for _ in 0..6 {
        harness.inject_keys(b"\t").expect("cycle tab");
        if harness
            .wait_for_text(SEEDED_WORKFLOW, Duration::from_secs(3))
            .is_ok()
        {
            reached_workflows_tab = true;
            break;
        }
    }
    assert!(
        reached_workflows_tab,
        "no tab ever showed the seeded workflow row '{SEEDED_WORKFLOW}'\nscreen:\n{}",
        harness.screen_contents()
    );
    dump_screen("workflows tab with seeded row", &harness);
    // No spawn cwd: portable-pty falls back to the sandbox $HOME, and with no
    // git repo there project_root() == $HOME, so the seeded file is project scope.
    assert!(
        harness.contains_text("(project)"),
        "workflow row must show its source as the right label\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_text("(builtin)"),
        "built-in workflows must list with their scope-independent label\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_text("r reload"),
        "Workflows tab must advertise the reload action key\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
