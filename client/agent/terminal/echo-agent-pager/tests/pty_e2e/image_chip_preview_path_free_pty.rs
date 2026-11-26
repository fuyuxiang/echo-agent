#[allow(unused_imports)]
use super::common::*;

/// Verifies path-free preview metadata appears and dismisses on a non-graphics PTY.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn image_chip_preview_path_free_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} image preview turn."));

    let png_name = "preview-fixture.png";
    let png_path = content.home().join(png_name);
    std::fs::write(&png_path, PNG_8X8_GRAY).expect("write png fixture");
    let path_str = png_path.display().to_string();

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Bracketed-paste the bare path alone so drop-classifier turns it into a chip.
    harness
        .inject_keys(format!("\x1b[200~{}\x1b[201~", path_str).as_bytes())
        .expect("paste png path");
    harness
        .wait_for_text("Image #1", Duration::from_secs(15))
        .expect("image chip attached");

    // Allow a frame for the preview overlay to paint (cursor is post-chip).
    harness.update(Duration::from_millis(400));

    let screen = harness.screen_contents();
    assert!(
        screen.contains("Image #1"),
        "chip label must render: {screen}"
    );
    assert!(
        !screen.contains("Image #1:") && !screen.contains(&format!("[Image #1: {path_str}]")),
        "chip must be path-free; screen still shows path-in-chip form:\n{screen}"
    );

    assert!(
        screen.contains("Format:"),
        "expected format metadata after path-paste; screen:\n{screen}"
    );
    assert!(
        screen.contains(png_name),
        "expected the pasted filename in preview; screen:\n{screen}"
    );

    let dismiss_sentinel = "PREVIEW_DISMISSED_64280529";
    harness
        .inject_keys(dismiss_sentinel.as_bytes())
        .expect("type after chip");
    harness
        .wait_for_text(dismiss_sentinel, Duration::from_secs(5))
        .expect("typed sentinel echoes");
    harness.update(Duration::from_millis(300));
    let after = harness.screen_contents();
    assert!(
        after.contains("Image #1"),
        "chip remains after typing: {after}"
    );
    assert!(
        after.contains(dismiss_sentinel),
        "dismissal sentinel must be visible: {after}"
    );
    assert!(
        !after.contains("Format:") && !after.contains("Path:") && !after.contains(png_name),
        "preview metadata/path must disappear after typing:\n{after}"
    );
    #[cfg(unix)]
    write_cast_if_requested(&harness, "image_chip_preview_path_free.cast");

    // Keep both states for video stills.
    if let Ok(dir) = std::env::var("PTY_E2E_ARTIFACT_DIR") {
        let path = std::path::Path::new(&dir).join("image_chip_preview_path_free_pty.txt");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            &path,
            format!("--- after paste ---\n{screen}\n--- after type ---\n{after}\n"),
        );
    }
}

/// Minimal valid 8×8 grayscale PNG (same fixture as edit-interject e2e).
const PNG_8X8_GRAY: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x00, 0x00, 0x00, 0x00, 0xe1, 0x64, 0xe1,
    0x57, 0x00, 0x00, 0x00, 0x0e, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x68, 0x80, 0x02, 0x06,
    0xca, 0x18, 0x00, 0x80, 0x84, 0x20, 0x01, 0x0d, 0x80, 0x24, 0x61, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
