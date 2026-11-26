// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Regression, Linux-hermetic: under Otty (`TERM_PROGRAM=otty` — the
/// only terminal known to deliver macOS IME commits as bracketed paste),
/// bracketed text that did not come from the system clipboard must not attach
/// the unrelated clipboard image. Any other terminal keeps the historical
/// probe behavior, asserted here with a second, TERM_PROGRAM-less spawn. A
/// fake `wl-paste`/`wl-copy` pair on `PATH` plays the clipboard; the sibling
/// `bracketed_ime_paste_skips_clipboard_image_macos` covers the reported
/// agent-prompt surface on a real pasteboard.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn bracketed_ime_paste_skips_clipboard_image_linux() {
    use std::os::unix::fs::PermissionsExt as _;

    const IME_PAYLOAD: &str = "中文";
    const CAPTION: &str = "CLIPCAPTION42";

    let tmp = tempfile::tempdir().expect("tempdir for fake clipboard tools");
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir fake bin");
    let text_file = tmp.path().join("clipboard_text");
    let png_file = echo_agent_pager_pty_harness::host_clipboard::write_fixture_png(tmp.path())
        .expect("write clipboard png fixture");

    std::fs::write(&text_file, b"").expect("empty clipboard text");

    let wl_paste = format!(
        "#!/bin/sh\ncase \"$*\" in\n  *image/png*) cat '{png}' ;;\n  *text*) cat '{txt}' ;;\n  *) exit 0 ;;\nesac\n",
        png = png_file.display(),
        txt = text_file.display(),
    );
    let wl_copy = "#!/bin/sh\nexit 0\n";
    for (name, body) in [("wl-paste", wl_paste.as_str()), ("wl-copy", wl_copy)] {
        let path = bin_dir.join(name);
        std::fs::write(&path, body).expect("write fake tool");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake tool");
    }

    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} ime paste turn."));

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let base_env = [
        ("PATH", path_env.as_str()),
        ("WAYLAND_DISPLAY", "wayland-fake"),
        ("DISPLAY", ""),
    ];

    /// Spawn the pager with `extra_env` and drive it to the dashboard, where
    /// bracketed paste routes to the dispatch input.
    fn spawn_on_dashboard(
        content: &ContentController,
        base_env: &[(&str, &str)],
        extra_env: &[EnvOp<'_>],
    ) -> PtyHarness {
        let mut operations: Vec<_> = base_env
            .iter()
            .map(|(key, value)| EnvOp::set(key, value))
            .collect();
        operations.extend_from_slice(extra_env);
        let binary = pager_binary().expect("resolve pager binary");
        let mut harness = PtyHarness::spawn_with_content_env_ops(
            &binary,
            DEFAULT_ROWS,
            DEFAULT_COLS,
            content,
            &[],
            &operations,
        )
        .expect("spawn pager");
        harness
            .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
            .expect("welcome text");
        harness
            .inject_keys(format!("{PROMPT}\r").as_bytes())
            .expect("submit prompt");
        harness
            .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
            .expect("turn rendered (idle session)");
        harness
            .inject_keys(b"\x1b[92;5u")
            .expect("ctrl+\\ open dashboard");
        harness
            .wait_for_text("+ New Agent", Duration::from_secs(10))
            .expect("dashboard opens");
        harness
    }

    // ── Otty: IME-style bracketed paste, image-only clipboard → no image ──
    let mut harness =
        spawn_on_dashboard(&content, &base_env, &[EnvOp::set("TERM_PROGRAM", "otty")]);
    harness
        .inject_keys(format!("\x1b[200~{IME_PAYLOAD}\x1b[201~").as_bytes())
        .expect("bracketed IME payload");
    harness
        .wait_for_text(IME_PAYLOAD, Duration::from_secs(10))
        .expect("IME text reaches the dispatch input");
    harness.update(Duration::from_millis(500));
    assert!(
        !harness.contains_text("[Image #"),
        "under Otty, IME-committed text must NOT attach the clipboard image\nscreen:\n{}",
        harness.screen_contents()
    );

    // ── Otty (positive control): payload == clipboard caption → image ──
    std::fs::write(&text_file, CAPTION.as_bytes()).expect("genuine-paste clipboard text");
    harness
        .inject_keys(format!("\x1b[200~{CAPTION}\x1b[201~").as_bytes())
        .expect("bracketed genuine clipboard payload");
    harness
        .wait_for_text("[Image #1", Duration::from_secs(10))
        .expect(
            "a genuine clipboard paste (payload matches clipboard text) must still \
             attach the clipboard image — the probe path must stay live",
        );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");

    // ── No TERM_PROGRAM (any other terminal): historical behavior intact —
    //    the same mismatched bracketed payload still attaches the image ──
    std::fs::write(&text_file, b"").expect("reset clipboard text");
    let mut harness = spawn_on_dashboard(&content, &base_env, &[]);
    harness
        .inject_keys(format!("\x1b[200~{IME_PAYLOAD}\x1b[201~").as_bytes())
        .expect("bracketed payload without otty");
    harness
        .wait_for_text("[Image #1", Duration::from_secs(10))
        .expect(
            "outside Otty the payload-origin gate must not run — the historical \
             bracketed-paste image probe attaches the clipboard image unchanged",
        );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
