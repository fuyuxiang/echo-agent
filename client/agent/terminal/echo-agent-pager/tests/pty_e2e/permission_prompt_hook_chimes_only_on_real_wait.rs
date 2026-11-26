// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const READ_DONE: &str = "HOOK_READ_AUTO_ALLOWED";
const EDIT_DONE: &str = "HOOK_EDIT_AFTER_ALLOW";
const CHIME: &str = "permission_prompt_chime";

fn seed_permission_prompt_hook(content: &ContentController, log: &Path) {
    let hooks_dir = content.home().join(".echo-agent").join("hooks");
    std::fs::create_dir_all(&hooks_dir).expect("create ~/.echo-agent/hooks");
    let command = format!("printf '{CHIME}\\n' >> {}", log.display());
    let spec = json!({
        "hooks": {
            "Notification": [{
                "matcher": "permission_prompt",
                "hooks": [{ "type": "command", "command": command, "timeout": 5 }]
            }]
        }
    });
    std::fs::write(
        hooks_dir.join("permission_prompt.json"),
        serde_json::to_vec_pretty(&spec).expect("serialize hook spec"),
    )
    .expect("write hook spec");
}

fn hook_log_chimed(log: &Path) -> bool {
    std::fs::read_to_string(log).is_ok_and(|body| body.contains(CHIME))
}

/// Notification `permission_prompt` hook (Claude-style finish chime) must not
/// fire on auto-allowed tools, and must fire while a real permission UI waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn permission_prompt_hook_chimes_only_on_real_wait() {
    let content = ContentController::start().await.expect("start content");
    let hook_log = content.home().join("permission_prompt_hook.log");
    seed_permission_prompt_hook(&content, &hook_log);

    let read_target = content.home().join("hook_auto_read.txt");
    std::fs::write(&read_target, "safe to auto-allow\n").expect("write read fixture");
    let read_abs = dunce::canonicalize(&read_target).unwrap_or(read_target.clone());

    let edit_target = content.home().join("hook_needs_prompt.txt");
    std::fs::write(&edit_target, "old line\n").expect("write edit fixture");
    let edit_abs = dunce::canonicalize(&edit_target).unwrap_or(edit_target.clone());

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");

    let _read_turn = expect_tool_turn(
        &content,
        "call_hook_read",
        "read_file",
        json!({ "target_file": read_abs.to_string_lossy() }).to_string(),
    );
    content.set_response(READ_DONE);
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit auto-allow read");
    harness
        .wait_for_text(READ_DONE, Duration::from_secs(90))
        .unwrap_or_else(|_| {
            panic!(
                "auto-allowed read should settle without a permission card; got:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        !hook_log_chimed(&hook_log),
        "auto-allowed read must not run the permission_prompt hook; log: {:?}",
        std::fs::read_to_string(&hook_log).ok()
    );

    let _edit_turn = expect_tool_turn(
        &content,
        "call_hook_edit",
        "search_replace",
        json!({
            "file_path": edit_abs.to_string_lossy(),
            "old_string": "old line",
            "new_string": "new line",
        })
        .to_string(),
    );
    content.set_response(EDIT_DONE);
    harness
        .inject_keys(b"edit the fixture\r")
        .expect("submit edit that needs permission");
    harness
        .wait_for_text("No, reject", Duration::from_secs(30))
        .expect("permission modal opens");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !hook_log_chimed(&hook_log) {
        assert!(
            Instant::now() < deadline,
            "permission_prompt hook must fire before the user answers; log: {:?}\nscreen:\n{}",
            std::fs::read_to_string(&hook_log).ok(),
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(100));
    }

    harness.inject_keys(b"1").expect("allow once");
    harness
        .wait_for_text(EDIT_DONE, Duration::from_secs(90))
        .expect("turn settles after allow");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
