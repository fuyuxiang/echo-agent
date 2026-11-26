//! Clipboard, paste, primary-selection, and inline-media PTY coverage.
//!
//! This family is isolated from ordinary PTY scheduling and serialized by
//! Bazel because its platform cases touch host-global clipboard state.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;

#[path = "pty_e2e/bracketed_ime_paste_skips_clipboard_image_linux.rs"]
mod bracketed_ime_paste_skips_clipboard_image_linux;
#[path = "pty_e2e/bracketed_ime_paste_skips_clipboard_image_macos.rs"]
mod bracketed_ime_paste_skips_clipboard_image_macos;
#[path = "pty_e2e/image_chip_preview_path_free_pty.rs"]
mod image_chip_preview_path_free_pty;
#[path = "pty_e2e/middle_click_pastes_primary_linux.rs"]
mod middle_click_pastes_primary_linux;
#[path = "pty_e2e/paste_bracketed_chip_text_sends_full_payload.rs"]
mod paste_bracketed_chip_text_sends_full_payload;
#[path = "pty_e2e/paste_bracketed_inline_text_echoes_and_sends_intact.rs"]
mod paste_bracketed_inline_text_echoes_and_sends_intact;
#[path = "pty_e2e/paste_bracketed_then_immediate_enter_sends_intact.rs"]
mod paste_bracketed_then_immediate_enter_sends_intact;
#[path = "pty_e2e/paste_ctrl_v_image_keeps_ui_responsive_macos.rs"]
mod paste_ctrl_v_image_keeps_ui_responsive_macos;
#[path = "pty_e2e/paste_ctrl_v_image_keeps_ui_responsive_windows.rs"]
mod paste_ctrl_v_image_keeps_ui_responsive_windows;
#[path = "pty_e2e/paste_ctrl_v_text_echoes_fast_macos.rs"]
mod paste_ctrl_v_text_echoes_fast_macos;
#[path = "pty_e2e/paste_ctrl_v_text_echoes_fast_windows.rs"]
mod paste_ctrl_v_text_echoes_fast_windows;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; exercises real copy output and /doctor"]
async fn unknown_ssh_clipboard_delivery_is_unverified() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} clipboard delivery sentinel"
    ));
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_ops_in_dir(
        &binary,
        60,
        80,
        &content,
        &[],
        &[
            EnvOp::set("SSH_CONNECTION", "scripted-test 1 127.0.0.1 2"),
            // Model the no-wrap-sink path even when the parent test process was
            // launched under `echoagent wrap`.
            EnvOp::remove("ECHO_AGENT_OSC52_SINK"),
            EnvOp::remove("LC_ECHO_AGENT_OSC52_SINK"),
        ],
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("response");
    harness
        .wait_for_text("Worked for", Duration::from_secs(20))
        .expect("turn completion marker before /copy");
    inject_keys_paced(&mut harness, b"/copy 1");
    harness
        .wait_for_text("/copy 1", Duration::from_secs(10))
        .expect("/copy command ready");
    let raw_before_copy = harness.raw_output().len();
    harness.inject_keys(b"\r").expect("run /copy");

    let copy_deadline = Instant::now() + Duration::from_secs(10);
    let payloads = loop {
        harness.update(Duration::from_millis(200));
        let payloads = decode_osc52_payloads(&harness.raw_output()[raw_before_copy..]);
        if !payloads.is_empty() || Instant::now() >= copy_deadline {
            break payloads;
        }
    };
    assert!(
        payloads
            .iter()
            .any(|payload| payload.contains("clipboard delivery sentinel")),
        "copy must still emit the response through OSC 52: {payloads:?}"
    );
    harness
        .wait_for_text("Copy sent", Duration::from_secs(10))
        .expect("unverified copy result visible at 80 columns");
    assert!(!harness.contains_text("Copy failed"));
    assert!(!harness.contains_text("Copied!"));

    harness.inject_keys(b"/doctor\r").expect("run /doctor");
    harness
        .wait_for_text("clipboard.delivery-unverified", Duration::from_secs(10))
        .expect("named clipboard finding");
    // The one-off SSH command wraps at 80 columns; this remediation stays contiguous.
    harness
        .wait_for_text("echoagent doctor fix ssh-wrap", Duration::from_secs(10))
        .expect("doctor-owned SSH-wrap remediation");
    assert!(!harness.contains_text("Copy failed"));
    assert!(!harness.contains_text("panicked"));

    harness.quit().expect("clean quit");
}
