//! Empty-composer Enter sends the top mid-turn queued follow-up now.
//!
//! Regression for send-now discoverability: plain Enter with text still
//! *queues*; a second bare Enter on the empty prompt is cancel-and-send — the
//! running turn is cancelled (silently: no "Turn cancelled by user" marker)
//! and the queued row runs as the next turn, arriving on the wire as a
//! standard `<user_query>` prompt with the interjection preamble.

use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::wait_for_welcome;
use crate::{ContentController, PtyHarness, pager_binary};

const DEFAULT_ROWS: u16 = 50;
const DEFAULT_COLS: u16 = 120;
const INTERJECTION_WIRE_PREFIX: &str = "The user sent a message while you were working";

fn slow_turn_text(sentinel: &str) -> String {
    let mut s = String::from(sentinel);
    for i in 0..30 {
        s.push_str(&format!(" streaming{i}"));
    }
    s
}

fn all_user_message_blobs(content: &ContentController) -> Vec<String> {
    content
        .request_bodies()
        .iter()
        .flat_map(|b| {
            let items = b["messages"].as_array().or_else(|| b["input"].as_array());
            items
                .into_iter()
                .flatten()
                .filter(|m| m["role"] == "user")
                .map(|m| match m["content"].as_str() {
                    Some(s) => s.to_owned(),
                    None => m["content"].to_string(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Mid-turn queue via Enter, then empty Enter cancels the running turn and
/// runs that row as the next turn (cancel-and-send).
pub async fn assert_empty_enter_force_sends_top_queued() -> Result<()> {
    let content = ContentController::start()
        .await
        .context("start ContentController")?;
    // Gate turn 1's terminal event so the queue + empty-Enter provably land
    // mid-turn — a paced-chunk window races turn end on slow (remote) workers.
    let mut turn_one = content
        .expect_agent_turn_blocked("running turn before send-now", slow_turn_text("TURNONE"));
    let mut turn_two = content.expect_agent_turn(
        "promoted queued follow-up",
        "TURNTWO reply to the promoted follow-up.",
    );

    let binary = pager_binary().context("resolve pager binary")?;
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .context("spawn pager")?;

    wait_for_welcome(&mut harness).await?;

    harness.inject_keys(b"go\r").context("submit prompt")?;
    harness
        .wait_for_text("TURNONE", Duration::from_secs(30))
        .context("turn 1 streaming")?;
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .context("turn 1 completion-barrier timeout")?;

    harness
        .inject_keys(b"please also check the logs\r")
        .context("queue follow-up")?;
    harness
        .wait_for_text("please also check the logs", Duration::from_secs(10))
        .context("queued text visible")?;

    harness.inject_keys(b"\r").context("empty Enter send-now")?;
    // Cancel-and-send: the shell cancels turn 1 (its held completion is
    // irrelevant — the abort wins) and promotes the row to run as turn 2.
    turn_one.release();
    // The promoted row renders as a standard user prompt block ("❯ " prefix
    // distinguishes the committed block from the prefix-less queue row) with
    // the new turn's reply below it.
    harness
        .wait_for_text(
            "\u{276F} please also check the logs",
            Duration::from_secs(15),
        )
        .context("promoted prompt scrollback chrome")?;
    harness
        .wait_for_text("TURNTWO", Duration::from_secs(40))
        .context("promoted turn reply")?;
    tokio::time::timeout(Duration::from_secs(10), turn_two.wait_satisfied())
        .await
        .context("promoted turn expectation timeout")?;

    // A send-now cancel is silent: no "Turn cancelled by user" marker may
    // appear between the partial turn-1 output and the promoted prompt.
    if harness.contains_text("Turn cancelled by user") {
        bail!(
            "send-now cancel must not render a cancelled marker\n{}",
            harness.screen_contents()
        );
    }

    let users = all_user_message_blobs(&content);
    let Some(promoted) = users
        .iter()
        .find(|u| u.contains("please also check the logs"))
    else {
        bail!("queued follow-up never reached the wire: {users:#?}");
    };
    if !promoted.contains(INTERJECTION_WIRE_PREFIX) {
        bail!("send-now must use the interjection preamble: {promoted}");
    }
    if !promoted.contains("<user_query>") {
        bail!("send-now must wrap the steered text in user_query: {promoted}");
    }
    if harness.contains_text("panicked") {
        bail!("pager panicked\n{}", harness.screen_contents());
    }

    harness.quit().context("clean quit")?;
    Ok(())
}
