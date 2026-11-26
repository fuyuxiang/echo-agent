// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Linux X11 PTY e2e; run in CI with the built pager"]
async fn middle_click_pastes_primary_linux() {
    use std::os::unix::fs::PermissionsExt as _;

    const PRIMARY: &str = "X11PRIMARYQQQ exact selection";
    const CLIPBOARD: &str = "X11CLIPBOARDQQQ must stay unused";

    let tmp = tempfile::tempdir().expect("tempdir for fake X11 clipboard tools");
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir fake bin");
    let argv_log = tmp.path().join("clipboard-argv.log");

    let recorder = format!(
        "tool=${{0##*/}}\nprintf '%s' \"$tool\" >> '{log}'\n\
         for arg in \"$@\"; do printf '\\t%s' \"$arg\" >> '{log}'; done\n\
         printf '\\n' >> '{log}'\n",
        log = argv_log.display(),
    );
    let xclip = format!(
        "#!/bin/sh\n{recorder}case \"$*\" in\n\
         \"--version\") exit 0 ;;\n\
         \"-o -selection primary\") printf '%s' '{PRIMARY}' ;;\n\
         \"-o -selection clipboard\") printf '%s' '{CLIPBOARD}' ;;\n\
         *) exit 1 ;;\nesac\n"
    );
    let xsel = format!(
        "#!/bin/sh\n{recorder}case \"$*\" in\n\
         \"--version\") exit 0 ;;\n\
         \"--primary --output\") printf '%s' '{PRIMARY}' ;;\n\
         \"--clipboard --output\") printf '%s' '{CLIPBOARD}' ;;\n\
         *) exit 1 ;;\nesac\n"
    );
    for (name, body) in [("xclip", xclip), ("xsel", xsel)] {
        let path = bin_dir.join(name);
        std::fs::write(&path, body).expect("write fake X11 clipboard tool");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake X11 clipboard tool");
    }

    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} primary paste turn."));
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let overrides = [
        ("PATH", path_env.as_str()),
        ("TERM", "xterm"),
        ("DISPLAY", ":99"),
        ("WAYLAND_DISPLAY", ""),
    ];
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &overrides,
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness
        .inject_keys(b"\x1b[<1;10;10M\x1b[<1;10;10m")
        .expect("inject SGR middle down and up");
    harness
        .wait_for_text(PRIMARY, Duration::from_secs(10))
        .expect("PRIMARY reaches the promoted session prompt");
    let prompt_screen = harness.screen_contents();
    assert_eq!(
        prompt_screen.matches(PRIMARY).count(),
        1,
        "middle down must insert PRIMARY exactly once\nscreen:\n{prompt_screen}"
    );
    assert!(
        !prompt_screen.contains(CLIPBOARD),
        "middle click must never insert CLIPBOARD\nscreen:\n{}",
        prompt_screen
    );

    harness.inject_keys(b"\r").expect("submit PRIMARY");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("PRIMARY prompt reaches the mock backend");

    // Recorded requests repeat cumulative history, so wire presence is the stable assertion here.
    let user_messages = all_user_message_blobs(&content);
    assert!(
        user_messages
            .iter()
            .any(|message| message.contains(PRIMARY)),
        "PRIMARY must reach the model; messages: {user_messages:#?}"
    );
    assert!(
        user_messages
            .iter()
            .all(|message| !message.contains(CLIPBOARD)),
        "CLIPBOARD must remain untouched; messages: {user_messages:#?}"
    );

    let argv = std::fs::read_to_string(&argv_log).expect("read fake-tool argv log");
    let primary_argv = "xclip\t-o\t-selection\tprimary";
    assert_eq!(
        argv.lines().filter(|line| *line == primary_argv).count(),
        1,
        "middle release and welcome forwarding must not re-read PRIMARY:\n{argv}"
    );
    assert!(
        !argv.lines().any(|line| {
            line.contains("\t-selection\tclipboard") || line.contains("\t--clipboard")
        }),
        "middle click must not invoke CLIPBOARD argv:\n{argv}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
