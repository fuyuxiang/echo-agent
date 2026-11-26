// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Tutor report: plain-text file paths with spaces only partially linkify —
/// OSC 8 / click / underline stopped at the first space in a synthetic
/// macOS app-bundle path (`Demo` vs `Demo App.app`). Prove the full path is
/// on screen AND the PTY stream carries an OSC 8 hyperlink whose `file://`
/// URL encodes the space (`%20`), so the click target spans the whole
/// filename — not a truncated prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn file_path_with_space_emits_full_osc8_hyperlink() {
    // Synthetic macOS app-bundle path with a space in the final segment.
    const PATH_PREFIX: &str = "/Users/alice/src/app/release/mac-arm64/Demo";
    const FULL_PATH: &str = "/Users/alice/src/app/release/mac-arm64/Demo App.app";
    // file:// URL percent-encodes the space; this is what OSC 8 must carry.
    const FILE_URL_MARKER: &str = "Demo%20App.app";

    let content = ContentController::start().await.expect("start content");
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} open \"{FULL_PATH}\" (or release/mac/ on Intel)."
    ));

    let binary = pager_binary().expect("resolve pager binary");
    // OSC 8 emission is gated on a Native-capable brand (`hyperlink_route`).
    // The default harness PTY only sets `TERM=xterm-256color`, so brand is
    // `Unknown` and the pager deliberately skips OSC 8. Pin WezTerm so the
    // byte-level proof below is meaningful (same override as `pty_xtversion`).
    let overrides: Vec<(String, String)> = vec![("TERM_PROGRAM".into(), "WezTerm".into())];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    // Wide enough that the path does not wrap mid-segment (wrap would still
    // linkify, but we want a single-row assertion on the screen text).
    let mut harness =
        PtyHarness::spawn_with_content_env(&binary, DEFAULT_ROWS, 160, &content, &[], &env_refs)
            .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Wait until the distinctive path segment is painted (streaming may lag).
    harness
        .wait_for_text(PATH_PREFIX, Duration::from_secs(30))
        .expect("path prefix on screen");
    // Give the frame a beat to flush OSC 8 for the completed line.
    harness.update(Duration::from_millis(400));

    let screen = harness.screen_contents();
    assert!(
        screen.contains(FULL_PATH) || (screen.contains(PATH_PREFIX) && screen.contains("App.app")),
        "full path (incl. space + `App.app`) must render on screen;\n\
         missing either the prefix or `App.app` means the link still truncates.\n\
         screen excerpt:\n{}",
        screen.chars().take(2000).collect::<String>()
    );
    assert!(
        screen.contains("App.app"),
        "filename suffix after the space must be visible"
    );

    // Strongest proof: OSC 8 in the raw PTY stream targets the *full* path.
    // Pre-fix the scanner stopped at the space, so the hyperlink URL would
    // end at `…/Demo` with no `%20App.app`.
    let raw = String::from_utf8_lossy(harness.raw_output());
    assert!(
        raw.contains("\x1b]8;"),
        "expected OSC 8 hyperlink sequences in PTY output (path should be clickable)"
    );
    assert!(
        raw.contains(FILE_URL_MARKER),
        "OSC 8 file:// URL must include the space-encoded suffix `{FILE_URL_MARKER}` so the \
         click/underline region covers `Demo App.app`, not just `Demo`.\n\
         (truncated link would omit this marker.)\n\
         OSC 8 snippets: {}",
        osc8_snippets(&raw)
    );
    // Guard against a partial link *and* a full one: the truncated form must
    // not be the only match. A naive prefix link would use `…/Demo` with
    // no following `%20`.
    let has_truncated_only =
        raw.contains("mac-arm64/Demo\x07") || raw.contains("mac-arm64/Demo\x1b\\");
    assert!(
        !has_truncated_only || raw.contains(FILE_URL_MARKER),
        "must not emit a truncated OSC 8 ending at `Demo` without the space suffix"
    );

    harness.quit().expect("clean quit");
}
