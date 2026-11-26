// Demo artifact generator (and smoke assert) for basename-only collapsed
// Read headers. Writes asciicast + HTML under /tmp/basename_path_video for
// agg → gif/mp4. Not meant as a permanent product regression pin.
#[allow(unused_imports)]
use super::common::*;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DONE_SENTINEL: &str = "BASENAME_PATH_DEMO_DONE";
const ARTIFACT_DIR: &str = "/tmp/basename_path_video";
/// Unique basename so on-screen match is unambiguous.
const FILE_NAME: &str = "basename_demo_target.rs";
/// Nested dirs so full abs path is long and would dominate a collapsed header.
const NEST: &str = "very/deep/nested/project/src/module";

fn screen_shows_full_path(screen: &str, full_path: &str) -> bool {
    if screen.contains(full_path) {
        return true;
    }
    // Modal wraps long paths mid-segment-list (e.g. `…/src/` then `module/file`).
    // Require nest markers that never appear in HOME chrome alone.
    let joined = screen.replace('\n', "");
    if joined.contains(NEST) && joined.contains(FILE_NAME) {
        return true;
    }
    screen.contains("very/deep") && screen.contains("nested/project") && screen.contains(FILE_NAME)
}

fn write_asciicast(path: &Path, cols: u16, rows: u16, events: &[(f64, String)]) {
    let mut f = fs::File::create(path).expect("create cast");
    let header = serde_json::json!({
        "version": 2,
        "width": cols,
        "height": rows,
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "env": {"TERM": "xterm-256color", "SHELL": "/bin/zsh"},
    });
    writeln!(f, "{header}").expect("header");
    for (t, out) in events {
        let line = serde_json::json!([t, "o", out]);
        writeln!(f, "{line}").expect("event");
    }
}

/// PTY demo: long abs path → collapsed header shows basename only; open block
/// viewer and show full path in modal preamble. Dumps asciicast for video.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "demo video generator; run with cargo test -p echo-agent-pager --test pty_e2e_smoke basename_path_demo_pty -- --ignored --nocapture"]
async fn basename_path_demo_pty() {
    fs::create_dir_all(ARTIFACT_DIR).expect("artifact dir");
    let content = ContentController::start().await.expect("start content");

    let nest_dir = content.home().join(NEST);
    fs::create_dir_all(&nest_dir).expect("nest dirs");
    let target = nest_dir.join(FILE_NAME);
    fs::write(&target, "// basename path demo fixture\npub fn demo() {}\n").expect("write fixture");
    let abs = dunce::canonicalize(&target).unwrap_or(target.clone());
    let full_path = abs.to_string_lossy().into_owned();

    let _tool_turn = expect_tool_turn(
        &content,
        "call_basename_read",
        "read_file",
        json!({ "target_file": full_path }).to_string(),
    );
    content.set_response(DONE_SENTINEL);

    let binary = pager_binary().expect("resolve pager binary");
    let rows = DEFAULT_ROWS;
    let cols = DEFAULT_COLS;
    // The demo shows the RAW basename-only `Read {path}` header; with
    // verb-group folding on (default), a lone read folds into "Read 1 file"
    // and the basename row never renders.
    seed_ui_config(&content, "group_tool_verbs = false");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        rows,
        cols,
        &content,
        &["--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");

    let t0 = Instant::now();
    let mut events: Vec<(f64, String)> = Vec::new();
    let mut raw_cursor = 0usize;
    let mut sample = |harness: &mut PtyHarness| {
        harness.update(Duration::from_millis(50));
        let raw = harness.raw_output();
        if raw.len() > raw_cursor {
            let chunk = &raw[raw_cursor..];
            raw_cursor = raw.len();
            let s = String::from_utf8_lossy(chunk).into_owned();
            if !s.is_empty() {
                events.push((t0.elapsed().as_secs_f64(), s));
            }
        }
    };

    let welcome_deadline = Instant::now() + WELCOME_TIMEOUT;
    loop {
        sample(&mut harness);
        if harness.contains_text(WELCOME_SCREEN_SENTINEL) {
            break;
        }
        assert!(
            Instant::now() < welcome_deadline,
            "welcome timeout; screen:\n{}",
            harness.screen_contents()
        );
    }
    // Hold welcome for the video.
    for _ in 0..15 {
        sample(&mut harness);
    }

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Wait for collapsed Read header with basename.
    let read_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        sample(&mut harness);
        let screen = harness.screen_contents();
        if screen.contains(FILE_NAME) && screen.contains("Read ") {
            break;
        }
        if Instant::now() > read_deadline {
            panic!("timeout waiting for Read {FILE_NAME}; screen:\n{screen}");
        }
    }

    // Settle turn.
    let settle_deadline = Instant::now() + Duration::from_secs(45);
    loop {
        sample(&mut harness);
        if harness.contains_text(DONE_SENTINEL) {
            break;
        }
        if Instant::now() > settle_deadline {
            panic!(
                "timeout waiting for {DONE_SENTINEL}; screen:\n{}",
                harness.screen_contents()
            );
        }
    }

    // Hold collapsed view so the video can show basename-only header.
    for _ in 0..40 {
        sample(&mut harness);
    }

    let collapsed = harness.screen_contents();
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("collapsed.txt"),
        &collapsed,
    )
    .ok();
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("collapsed.html"),
        harness.screen_html(),
    )
    .ok();

    // Collapsed header must show basename, not the nested parent segments.
    assert!(
        collapsed.contains(FILE_NAME),
        "collapsed screen must show basename; screen:\n{collapsed}"
    );
    assert!(
        collapsed.contains("Read "),
        "collapsed screen must show Read label; screen:\n{collapsed}"
    );
    // Parent nest segment should not appear next to the tool label on the
    // collapsed one-liner (full path may still appear elsewhere rarely; the
    // header itself is basename-only — check the Read line specifically).
    let mut saw_basename_header = false;
    for line in collapsed.lines() {
        if line.contains("Read ") && line.contains(FILE_NAME) {
            saw_basename_header = true;
            assert!(
                !line.contains("very/deep")
                    && !line.contains(&*full_path)
                    && !line.contains("/module/"),
                "collapsed Read header must not show full/nested path; line={line:?} full={full_path}"
            );
        }
    }
    assert!(
        saw_basename_header,
        "expected a Read {FILE_NAME} header line; screen:\n{collapsed}"
    );

    // Open block viewer: Tab → select Read line (click) → Ctrl+f (OpenBlockViewer
    // alt binding; Enter alone may toggle a verb-group header instead).
    harness.inject_keys(b"\t").expect("focus scrollback");
    for _ in 0..10 {
        sample(&mut harness);
    }
    let _ = harness.wait_for_text("Space:prompt", Duration::from_secs(10));
    for _ in 0..10 {
        sample(&mut harness);
    }

    let screen_for_click = harness.screen_contents();
    if let Some((row, col)) = locate_screen_text(&screen_for_click, FILE_NAME) {
        let click = format!(
            "{}{}",
            sgr_mouse(0, row, col, 'M'),
            sgr_mouse(0, row, col, 'm'),
        );
        harness
            .inject_keys(click.as_bytes())
            .expect("click Read header");
        for _ in 0..8 {
            sample(&mut harness);
        }
    }

    // Ctrl+f = OpenBlockViewer (alt key).
    harness.inject_keys(b"\x06").expect("Ctrl+f open viewer");

    let modal_deadline = Instant::now() + Duration::from_secs(15);
    let mut expanded_screen = loop {
        sample(&mut harness);
        let screen = harness.screen_contents();
        if screen_shows_full_path(&screen, &full_path) || Instant::now() > modal_deadline {
            break screen;
        }
    };

    // If viewer didn't open, fold-expand the Read block (Truncated header uses
    // full path via width: None) — still demos the dual display.
    if !screen_shows_full_path(&expanded_screen, &full_path) {
        // Esc out of any partial overlay, then click + Enter (toggle fold / group).
        harness.inject_keys(b"\x1b").expect("esc");
        for _ in 0..6 {
            sample(&mut harness);
        }
        if let Some((row, col)) = locate_screen_text(&harness.screen_contents(), FILE_NAME) {
            let click = format!(
                "{}{}",
                sgr_mouse(0, row, col, 'M'),
                sgr_mouse(0, row, col, 'm'),
            );
            harness
                .inject_keys(click.as_bytes())
                .expect("re-click Read");
            for _ in 0..6 {
                sample(&mut harness);
            }
        }
        harness.inject_keys(b"\r").expect("Enter expand/view");
        let expand_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            sample(&mut harness);
            expanded_screen = harness.screen_contents();
            if screen_shows_full_path(&expanded_screen, &full_path) {
                break;
            }
            if Instant::now() > expand_deadline {
                break;
            }
        }
    }

    // Hold expanded/modal view for the video.
    for _ in 0..50 {
        sample(&mut harness);
    }
    expanded_screen = harness.screen_contents();
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("expanded.txt"),
        &expanded_screen,
    )
    .ok();
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("expanded.html"),
        harness.screen_html(),
    )
    .ok();

    let expanded_has_full = screen_shows_full_path(&expanded_screen, &full_path);
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("assertions.txt"),
        format!(
            "full_path={full_path}\n\
             basename={FILE_NAME}\n\
             collapsed_has_basename={}\n\
             collapsed_has_full_on_header={}\n\
             expanded_has_full_path={expanded_has_full}\n",
            collapsed.contains(FILE_NAME),
            collapsed
                .lines()
                .any(|l| l.contains("Read ") && l.contains(&*full_path)),
        ),
    )
    .ok();

    assert!(
        expanded_has_full,
        "expanded / modal view should show full path ({full_path}); screen:\n{expanded_screen}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    let cast_path = PathBuf::from(ARTIFACT_DIR).join("basename-path-demo.cast");
    write_asciicast(&cast_path, cols, rows, &events);
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("raw.ansi"),
        harness.raw_output(),
    )
    .ok();
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("final.txt"),
        harness.screen_contents(),
    )
    .ok();
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("final.html"),
        harness.screen_html(),
    )
    .ok();

    eprintln!(
        "basename path demo artifacts → {ARTIFACT_DIR} (cast events={}, duration≈{:.1}s, full_path={full_path})",
        events.len(),
        t0.elapsed().as_secs_f64()
    );

    harness.quit().expect("clean quit");
}
