// Per-test-case module for the `pty_e2e` integration test crate.
//
// Regression pin for the in-place edit-HL upgrade: the target line's styling
// must CHANGE on screen after the first (hunk-only) paint, proving the
// file-scoped repaint landed. Doubles as the demo-artifact generator — the
// asciicast/HTML dumps under /tmp/edit_hl_video are kept for demo videos.
#[allow(unused_imports)]
use super::common::*;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use echo_agent_pager_pty_harness::StyledLine;

const DONE_SENTINEL: &str = "EDIT_HL_DONE";
const ARTIFACT_DIR: &str = "/tmp/edit_hl_video";

/// Marker unique to the edited (Insert) line; its styling flips on upgrade.
const TARGET_MARKER: &str = "min_length=2";
/// Tail of the same line; requiring both ends rejects partially-painted rows.
const TARGET_TAIL: &str = "upgrade target";

/// Style-run snapshot of every fully-painted screen row containing the target
/// line, row-position independent (scrolling must not read as a styling
/// change). `None` until the whole line is on screen.
fn target_line_style_snapshot(rows: &[StyledLine]) -> Option<String> {
    let mut snaps = Vec::new();
    for row in rows {
        let text: String = row.runs.iter().map(|r| r.text.as_str()).collect();
        if text.contains(TARGET_MARKER) && text.contains(TARGET_TAIL) {
            snaps.push(serde_json::to_string(&row.runs).unwrap_or_default());
        }
    }
    if snaps.is_empty() {
        None
    } else {
        Some(snaps.join("\n"))
    }
}

/// Python body: mid-file closing `"""` then fields.
/// Prefix padding slows full-file HL so the in-place upgrade is visible.
fn fixture_python(pad_lines: usize) -> String {
    let mut s = String::with_capacity(pad_lines * 48 + 512);
    s.push_str("# queue_item.py — edit HL demo fixture (cold-start mismatch shape)\n");
    for i in 0..pad_lines {
        s.push_str(&format!(
            "# pad line {i:04} keep full-file HL non-instant\n"
        ));
    }
    s.push_str(
        r#"
class ProcessQueueItem(BaseModel):
    """Request body for processing a single queue item.

    The item id is in the path; keep notes in the body.
    """

    notes: str = Field(..., min_length=1)
    category_id: CategoryId = Field(default=DEFAULT_CATEGORY_ID)
"#,
    );
    s
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
        // asciicast v2: [time, "o", data]
        let line = serde_json::json!([t, "o", out]);
        writeln!(f, "{line}").expect("event");
    }
}

/// PTY: search_replace on a mid-file `"""` fixture; assert the Edit block's
/// target line restyles in place (hunk-only → file-scoped upgrade) and dump
/// asciicast + HTML demo artifacts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn edit_hl_inplace_refresh_pty() {
    fs::create_dir_all(ARTIFACT_DIR).expect("artifact dir");
    let content = ContentController::start().await.expect("start content");
    // No config seeds: the collapsed_edit_blocks flag ships OFF, so Edit
    // diffs arrive expanded — this test asserts the on-screen restyle of the
    // diff BODY and doubles as the flag-off/legacy-default e2e.

    // ~2.5k pad lines: full-file HL takes hundreds of ms (visible upgrade).
    let pad = 2500usize;
    let body = fixture_python(pad);
    let target = content.home().join("queue_item.py");
    fs::write(&target, &body).expect("write fixture");
    let abs = dunce::canonicalize(&target).unwrap_or(target.clone());

    // Small unique edit on the field line after the closing """ (the spill zone).
    let old = "    notes: str = Field(..., min_length=1)";
    let new = "    notes: str = Field(..., min_length=2)  # HL upgrade target";
    let _tool_turn = expect_tool_turn(
        &content,
        "call_edit_hl",
        "search_replace",
        json!({
            "file_path": abs.to_string_lossy(),
            "old_string": old,
            "new_string": new,
        })
        .to_string(),
    );
    content.set_response(DONE_SENTINEL);

    let binary = pager_binary().expect("resolve pager binary");
    let rows = DEFAULT_ROWS;
    let cols = DEFAULT_COLS;
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        rows,
        cols,
        &content,
        &["--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");

    // Timed raw-output samples for asciicast.
    let t0 = Instant::now();
    let mut events: Vec<(f64, String)> = Vec::new();
    let mut raw_cursor = 0usize;
    let mut sample = |harness: &mut PtyHarness| {
        harness.update(Duration::from_millis(50));
        let raw = harness.raw_output();
        if raw.len() > raw_cursor {
            let chunk = &raw[raw_cursor..];
            raw_cursor = raw.len();
            if let Ok(s) = std::str::from_utf8(chunk) {
                if !s.is_empty() {
                    events.push((t0.elapsed().as_secs_f64(), s.to_owned()));
                }
            } else {
                // Lossy for binary OSC/etc. still useful for video.
                let s = String::from_utf8_lossy(chunk).into_owned();
                if !s.is_empty() {
                    events.push((t0.elapsed().as_secs_f64(), s));
                }
            }
        }
    };

    // Welcome
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
    // Hold welcome briefly for the video.
    for _ in 0..20 {
        sample(&mut harness);
    }

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Phase 1: hunk-only first paint — capture the target line's styling at
    // first sighting. The 2.5k pad keeps the full-file HL slow enough that
    // this frame reliably precedes the upgrade.
    // Phase 2: in-place upgrade — poll until the SAME line's styling changes.
    let edit_deadline = Instant::now() + Duration::from_secs(90);
    let mut saw_edit = false;
    let mut first_styles: Option<String> = None;
    let mut upgraded_styles: Option<String> = None;
    let mut post_upgrade_html: Option<String> = None;
    loop {
        sample(&mut harness);
        let screen = harness.screen_contents();
        if !saw_edit && (screen.contains("Edit ") || screen.contains("queue_item.py")) {
            saw_edit = true;
        }
        if let Some(snap) = target_line_style_snapshot(&harness.screen_styled()) {
            match &first_styles {
                None => {
                    first_styles = Some(snap);
                    let _ = fs::write(
                        PathBuf::from(ARTIFACT_DIR).join("frame_edit_first.html"),
                        harness.screen_html(),
                    );
                    let _ = fs::write(
                        PathBuf::from(ARTIFACT_DIR).join("frame_edit_first.txt"),
                        &screen,
                    );
                }
                Some(first) if upgraded_styles.is_none() && *first != snap => {
                    upgraded_styles = Some(snap);
                    let html = harness.screen_html();
                    post_upgrade_html = Some(html.clone());
                    let _ = fs::write(
                        PathBuf::from(ARTIFACT_DIR).join("frame_edit_upgraded.html"),
                        &html,
                    );
                    let _ = fs::write(
                        PathBuf::from(ARTIFACT_DIR).join("frame_edit_upgraded.txt"),
                        &screen,
                    );
                }
                _ => {}
            }
        }
        if saw_edit && upgraded_styles.is_some() && screen.contains(DONE_SENTINEL) {
            break;
        }
        if Instant::now() > edit_deadline {
            panic!(
                "timeout waiting for edit HL flow (saw_edit={saw_edit} \
                 first_styles={} upgraded={}); screen:\n{screen}",
                first_styles.is_some(),
                upgraded_styles.is_some(),
            );
        }
    }

    // Hold the upgraded view for the video (~3s).
    for _ in 0..60 {
        sample(&mut harness);
    }

    // The in-place upgrade proof: the marker line's style runs changed after
    // the hunk-only first paint while its text stayed put.
    let first = first_styles.expect("target line styled snapshot at first paint");
    let upgraded = upgraded_styles.expect("target line styling must change (file-scoped upgrade)");
    assert_ne!(
        first, upgraded,
        "upgrade must restyle the target line in place"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    // Final full dumps.
    let final_screen = harness.screen_contents();
    let final_html = harness.screen_html();
    fs::write(PathBuf::from(ARTIFACT_DIR).join("final.txt"), &final_screen).ok();
    fs::write(PathBuf::from(ARTIFACT_DIR).join("final.html"), &final_html).ok();
    fs::write(
        PathBuf::from(ARTIFACT_DIR).join("raw.ansi"),
        harness.raw_output(),
    )
    .ok();

    let cast_path = PathBuf::from(ARTIFACT_DIR).join("edit-hl-demo.cast");
    write_asciicast(&cast_path, cols, rows, &events);
    eprintln!(
        "edit HL PTY artifacts → {ARTIFACT_DIR} (cast events={}, duration≈{:.1}s)",
        events.len(),
        events.last().map(|(t, _)| *t).unwrap_or(0.0)
    );

    // Soft color proof: HTML after upgrade should include span styling (not plain text only).
    let html = post_upgrade_html.unwrap_or(final_html);
    assert!(
        html.contains("style=") || html.contains("<span"),
        "expected styled HTML for syntax colors; html len={}",
        html.len()
    );
}
