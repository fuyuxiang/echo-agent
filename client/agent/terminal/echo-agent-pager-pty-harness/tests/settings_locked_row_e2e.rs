//! E2E: the settings modal's locked coding-data row, driven off
//! the seeded auth entry through the full pipeline (auth.json → shell
//! `EchoAgentAuth` → auth meta → `AppView::coding_data_sharing_lock()` →
//! `PagerLocalSnapshot` → render):
//!
//! - ZDR team (`team_blocked_reasons` = `BLOCKED_REASON_NO_LOGS`): the value
//!   column shows exactly `ZDR` (no Opt in / Opt out), no `›` chevron;
//!   expanding the row shows only "Your team has Zero Data Retention."
//! - Team non-admin (`team_role` = `MEMBER`): the value shows
//!   `Opt out · Admin Managed`, no chevron; expanding shows only
//!   "Managed by your team admin."
//!
//! Both accounts also suppress the welcome privacy banner even with
//! `ECHO_AGENT_PRIVACY_NOTICE_ROLLOUT=1` — asserted on the authenticated welcome
//! screen before opening settings. Row/input details are unit-tested in
//! `echo-agent-pager` (`views/settings_modal/tests.rs`, `locked_coding_*`);
//! this suite covers the auth-to-render pipeline.
//!
//! ```bash
//! cargo test -p echo-agent-pager-pty-harness --test settings_locked_row_e2e \
//!   -- --ignored --nocapture
//! ```

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use echo_agent_pager_pty_harness::{
    ContentController, EnvOp, PtyHarness, keys, pager_binary, seed_fake_oauth_team_member,
    seed_fake_oauth_zdr_team,
};

const ROWS: u16 = 50;
const COLS: u16 = 120;
const BANNER_TITLE: &str = "Help improve EchoAgent";
/// Head of the row's label (`Coding data, retention, and training`). The
/// modal truncates long labels, so match the stable prefix.
const ROW_LABEL: &str = "Coding data";
const CHEVRON: &str = "\u{203A}"; // ›
const ZDR_REASON: &str = "Your team has Zero Data Retention.";
const TEAM_REASON: &str = "Managed by your team admin.";
/// Head of the row's description in `settings/defs.rs`. Kept short so it
/// can't span one of the modal's word wraps — `contains_text` joins rows
/// with `\n`, so a match on wrapped copy would silently never fire.
const DESCRIPTION_PREFIX: &str = "Opt-in to provide EchoAgent";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn zdr_team_locks_row_and_suppresses_banner() {
    run_zdr().await.expect("zdr locked-row e2e");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in: spawns the real pager binary in a PTY (CI runs with --ignored)
async fn team_member_sees_admin_managed_row_and_no_banner() {
    run_team_member().await.expect("team-member locked-row e2e");
}

/// Rollout flag forced on (the banner would show for a plain opted-out user),
/// the sandbox's fake `ECHO_AGENT_API_KEY` removed so the seeded team OAuth entry is
/// the active auth, and ZDR product access enabled — without it a ZDR account
/// gets the blocked welcome screen ("not yet available") and can never reach
/// settings; the row lock and banner suppression key off `is_zdr` regardless.
fn locked_row_env_ops() -> [EnvOp<'static>; 3] {
    [
        EnvOp::set("ECHO_AGENT_PRIVACY_NOTICE_ROLLOUT", "1"),
        EnvOp::set("ECHO_AGENT_ZDR_ACCESS_ENABLED", "1"),
        EnvOp::remove("ECHO_AGENT_API_KEY"),
    ]
}

async fn run_zdr() -> Result<()> {
    let content = ContentController::start()
        .await
        .context("start mock server")?;
    seed_fake_oauth_zdr_team(&content, "pty-zdr-user");

    let mut pager = launch(&content).context("launch pager")?;
    assert_no_banner_on_welcome(&mut pager)?;

    let line = open_settings_and_grab_row_line(&mut pager)?;
    assert!(
        line.contains("ZDR"),
        "ZDR lock must show `ZDR` on the {ROW_LABEL:?} row: {line:?}\nscreen:\n{}",
        pager.screen_contents()
    );
    assert!(
        !line.contains("Opt"),
        "ZDR lock must replace the Opt in/Opt out value: {line:?}\nscreen:\n{}",
        pager.screen_contents()
    );
    assert!(
        !line.contains(CHEVRON),
        "locked row must not render the `{CHEVRON}` enter affordance: {line:?}\nscreen:\n{}",
        pager.screen_contents()
    );
    // Wrong-variant guard: the team-managed lock reason must not appear.
    assert!(
        !pager.contains_text("Managed by your team admin"),
        "ZDR account rendered the team-managed lock:\n{}",
        pager.screen_contents()
    );

    // Expanded view: the lock reason REPLACES the registry description.
    expand_focused_row(&mut pager, ZDR_REASON)?;
    assert!(
        !pager.contains_text(DESCRIPTION_PREFIX),
        "locked expansion must replace the description, not append to it:\n{}",
        pager.screen_contents()
    );
    assert!(
        !pager.contains_text("Managed by your team admin"),
        "ZDR expansion rendered the team-managed reason:\n{}",
        pager.screen_contents()
    );
    Ok(())
}

async fn run_team_member() -> Result<()> {
    let content = ContentController::start()
        .await
        .context("start mock server")?;
    seed_fake_oauth_team_member(&content, "pty-team-user");

    let mut pager = launch(&content).context("launch pager")?;
    assert_no_banner_on_welcome(&mut pager)?;

    let line = open_settings_and_grab_row_line(&mut pager)?;
    assert!(
        line.contains("Opt out \u{00B7} Admin Managed"),
        "team-managed lock must show `Opt out · Admin Managed`: {line:?}\nscreen:\n{}",
        pager.screen_contents()
    );
    assert!(
        !line.contains("ZDR"),
        "team-managed lock must not show the ZDR value: {line:?}\nscreen:\n{}",
        pager.screen_contents()
    );
    assert!(
        !line.contains(CHEVRON),
        "locked row must not render the `{CHEVRON}` enter affordance: {line:?}\nscreen:\n{}",
        pager.screen_contents()
    );

    // Expanded view: the lock reason REPLACES the registry description.
    expand_focused_row(&mut pager, TEAM_REASON)?;
    assert!(
        !pager.contains_text(DESCRIPTION_PREFIX),
        "locked expansion must replace the description, not append to it:\n{}",
        pager.screen_contents()
    );
    // Wrong-variant guard: the ZDR reason must not appear.
    assert!(
        !pager.contains_text("Zero Data Retention"),
        "team-managed expansion rendered the ZDR reason:\n{}",
        pager.screen_contents()
    );
    Ok(())
}

fn launch(content: &ContentController) -> Result<PtyHarness> {
    let project = tempfile::tempdir().context("project dir")?;
    std::fs::create_dir_all(project.path().join(".git")).context("create .git")?;
    let binary = pager_binary().context("resolve pager binary")?;
    let pager = spawn_pager(&binary, content, project.path()).context("spawn pager")?;
    // Keep the project dir alive for the pager's lifetime.
    std::mem::forget(project);
    Ok(pager)
}

fn spawn_pager(binary: &Path, content: &ContentController, project: &Path) -> Result<PtyHarness> {
    PtyHarness::spawn_with_content_env_ops_in_dir(
        binary,
        ROWS,
        COLS,
        content,
        &[],
        &locked_row_env_ops(),
        Some(project),
    )
}

/// Sync on "New worktree" — rendered only on the authenticated welcome menu
/// ("Quit" also renders while auth is still pending, where the banner is
/// gated off regardless of team state) — then assert the banner never shows.
fn assert_no_banner_on_welcome(pager: &mut PtyHarness) -> Result<()> {
    pager
        .wait_for_text("New worktree", Duration::from_secs(20))
        .context("authenticated welcome screen")?;
    pager.update(Duration::from_secs(2));
    assert!(
        !pager.contains_text(BANNER_TITLE),
        "team account must suppress the privacy banner:\n{}",
        pager.screen_contents()
    );
    Ok(())
}

/// Open settings via F2 and return the screen line holding the Coding data
/// sharing row. F2's `OpenSettings` binding is `When::AgentScreen` only —
/// the welcome screen never routes it — so Enter first starts a session
/// (`Action::NewSession`), then F2 in the agent view opens the modal
/// (`dispatch_open_settings`).
///
/// Navigation is always via the modal's `/` filter: typing the query clamps
/// the selection to the filtered set (`clamp_selected_to_visible`) and Enter
/// commits back to Browse PRESERVING query and selection, so afterwards the
/// row is both in the viewport and FOCUSED — the precondition for `→`
/// expansion in [`expand_focused_row`]. The lowercase query cannot collide
/// with the case-sensitive label.
fn open_settings_and_grab_row_line(pager: &mut PtyHarness) -> Result<String> {
    pager.inject_keys(keys::ENTER).context("start session")?;
    pager
        .wait_for_text_absent("New worktree", Duration::from_secs(20))
        .context("agent view opened")?;
    pager.update(Duration::from_millis(500));
    pager.inject_keys(keys::F2).context("press F2")?;
    pager
        .wait_for_text("Appearance", Duration::from_secs(20))
        .context("settings modal opened")?;
    pager.inject_keys(b"/").context("focus filter")?;
    pager.update(Duration::from_millis(300));
    pager
        .inject_keys(b"coding data sharing")
        .context("type filter query")?;
    pager.update(Duration::from_millis(300));
    pager.inject_keys(keys::ENTER).context("commit filter")?;
    pager
        .wait_for_text(ROW_LABEL, Duration::from_secs(20))
        .context("coding-data row visible")?;
    pager.update(Duration::from_millis(500));
    let screen = pager.screen_contents();
    screen
        .lines()
        .find(|l| l.contains(ROW_LABEL))
        .map(str::to_owned)
        .with_context(|| format!("{ROW_LABEL:?} line not found:\n{screen}"))
}

/// Expand the focused row with `→` (Browse-mode `KeyCode::Right` inserts the
/// focused key into `expanded_keys`) and wait for `reason` to render.
/// Callers reach here from [`open_settings_and_grab_row_line`], which leaves
/// the coding-data row focused.
fn expand_focused_row(pager: &mut PtyHarness, reason: &str) -> Result<()> {
    pager.inject_keys(keys::RIGHT).context("expand row")?;
    pager.update(Duration::from_millis(300));
    pager
        .wait_for_text(reason, Duration::from_secs(20))
        .with_context(|| format!("expanded lock reason {reason:?} on screen"))
}
