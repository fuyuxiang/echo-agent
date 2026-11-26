//! PTY: a background shell command spawned by a tool call must be reaped when
//! the pager exits, so it can't outlive the TUI. This is the `/loop`
//! orphaned-watcher quota-drain bug: background `run_terminal_command` / `monitor`
//! commands are `setsid`-detached, so they escape the terminal's process group
//! and survive a quit/kill unless the exit path reaps them.
//!
//! Drives the real pager against the mock (spawn args are only `--yolo --trust`;
//! the prompt is typed in via `inject_keys` after the welcome screen), scripts a
//! background command that records its PID then sleeps, quits via a real SIGINT,
//! and asserts the PID is gone. Without the fix the detached process reparents to
//! init and keeps running, so the final poll times out.
#[allow(unused_imports)]
use super::common::*;

/// 10 min: comfortably outlasts the test's worst-case runtime yet self-exits, so
/// a reap regression can't leak a ~68-year `/bin/sleep` on CI (tracked by PID
/// file, so the exact bound only needs to outlast the test).
#[cfg(unix)]
const SLEEP_SECS: &str = "600";

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn background_task_reaped_on_quit() {
    let content = ContentController::start().await.expect("start content");
    let pidfile = content.home().join("orphan_bg.pid");
    let donefile = content.home().join("orphan_bg.done");
    let errfile = content.home().join("orphan_bg.err");

    // Turn 1: the model runs a background command via run_terminal_command. It writes
    // its PID, then sleeps; the done/err files capture an unexpected early exit
    // for diagnostics. Absolute /bin/sleep avoids PATH surprises in the spawn env.
    let command = format!(
        "echo $$ > {pid}; /bin/sleep {SLEEP_SECS} 2> {err}; echo rc=$? > {done}",
        pid = pidfile.display(),
        err = errfile.display(),
        done = donefile.display(),
    );
    let args = json!({
        "command": command,
        "description": "background orphan reap test",
        "is_background": true
    })
    .to_string();
    let _background_turn = expect_tool_turn(&content, "call_bg", "run_terminal_command", args);
    // Follow-up turns settle to plain text so the session goes idle.
    content.set_response("BG_TASK_STARTED");

    let binary = pager_binary().expect("resolve pager binary");
    // --yolo skips the bash permission prompt; --trust skips the folder-trust gate.
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // The settle text confirms the tool call ran and the follow-up turn finished.
    harness
        .wait_for_text("BG_TASK_STARTED", Duration::from_secs(45))
        .unwrap_or_else(|_| {
            panic!(
                "background tool call never settled; screen:\n{}",
                harness.screen_contents()
            )
        });

    // The background command writes its PID once it runs (proof the tool executed).
    if !wait_until(Duration::from_secs(10), || pidfile.exists()) {
        panic!(
            "background command never ran (no pidfile)\n--- non-system messages ---\n{}\n--- files under home ---\n{}\n--- screen ---\n{}",
            dump_non_system_messages(&content.request_bodies()),
            dump_files(content.home()),
            harness.screen_contents()
        );
    }
    let pid = read_pid(&pidfile);
    assert!(pid > 1, "pidfile did not contain a valid pid: {pid}");

    // Sanity: the detached sleep is alive before we quit.
    if !wait_until(Duration::from_secs(5), || pid_alive(pid)) {
        panic!(
            "background sleep (pid {pid}) exited immediately; nothing to test\n\
             done={:?} err={:?}\n--- files under home ---\n{}\n--- screen ---\n{}",
            std::fs::read_to_string(&donefile).ok(),
            std::fs::read_to_string(&errfile).ok(),
            dump_files(content.home()),
            harness.screen_contents()
        );
    }

    // A real SIGINT (not an injected Ctrl+C key byte) drives the OS-signal exit.
    // Both the graceful-quit teardown and the hard-exit tail reap spawned
    // children via the process-global ProcessScope, so the orphan dies either way.
    harness.send_signal(libc::SIGINT).expect("send SIGINT");
    let exit = harness
        .wait_exit_code(Duration::from_secs(15))
        .expect("wait after SIGINT");
    assert!(
        matches!(exit, PtyExitPoll::Exited(_) | PtyExitPoll::PendingStatus),
        "pager did not exit after SIGINT: {exit:?}"
    );

    // The fix: no orphaned background process survives the quit. Without it the
    // setsid-detached sleep reparents to init and keeps running -> this times out.
    assert!(
        wait_until(Duration::from_secs(15), || !pid_alive(pid)),
        "background sleep (pid {pid}) survived pager exit (orphaned)"
    );
}

/// Read a PID written by `echo $$`.
#[cfg(unix)]
fn read_pid(p: &Path) -> i32 {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(-1)
}

/// Whether `pid` exists (running or not-yet-reaped zombie) via `kill(pid, 0)`.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    // SAFETY: kill with signal 0 performs only an existence/permission check.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Recursive listing of files under `home` (with sizes), to find where tool
/// output / our marker files landed.
#[cfg(unix)]
fn dump_files(home: &Path) -> String {
    let mut out = String::new();
    let mut stack = vec![home.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push_str(&format!("{} ({len}B)\n", p.display()));
            }
        }
    }
    out
}
