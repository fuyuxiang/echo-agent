// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Distinctive tokens for the critical session banner (unlikely to collide
/// with welcome chrome or mock response text).
const CRIT_TITLE: &str = "ZZANNCRITTITLE";
const CRIT_MSG: &str = "ZZANNCRITMSG";
const CRIT_B_TITLE: &str = "ZZANNCRITBTITLE";
const CRIT_B_MSG: &str = "ZZANNCRITBMSG";
const INFO_TITLE: &str = "ZZANNINFOTITLE";
const INFO_MSG: &str = "ZZANNINFOMSG";
const HIDE_CTA: &str = "hide: /announcements hide";
/// Clickable hide button, right-aligned on the banner title row.
const HIDE_BUTTON: &str = "[hide]";
/// Slash-command description from `AnnouncementsCommand`.
const SLASH_DESC: &str = "Show or hide announcements";
/// Distinctive tokens for the 1-line promo row.
const PROMO_MSG: &str = "ZZANNPROMOMSG";
const PROMO_LABEL: &str = "ZZPROMOCTA";
/// The promo CTA renders as a bracketed button.
const PROMO_BUTTON: &str = "[ZZPROMOCTA]";
const PROMO_URL: &str = "https://echo-agent.invalid/zz-promo-cta";
/// Configured `cta.caption` for the pinned multi-surface fixture; the banner
/// paints it after the button, the in-session header never does.
const PROMO_CAPTION: &str = "or use Ctrl+O";
/// Second promo message (dismissible), distinct from `PROMO_MSG`.
const PROMO_B_MSG: &str = "ZZANNPROMOBMSG";

/// Promo announcement payload for `set_settings` pushes.
fn promo_settings_announcement() -> serde_json::Value {
    json!({
        "id": "pty-promo",
        "message": PROMO_MSG,
        "severity": "promo",
        "cta": { "label": PROMO_LABEL, "url": PROMO_URL },
    })
}

fn critical_override_json() -> String {
    format!(
        r#"[{{"id":"pty-crit","title":"{CRIT_TITLE}","message":"{CRIT_MSG}","severity":"critical"}}]"#
    )
}

fn info_override_json() -> String {
    format!(
        r#"[{{"id":"pty-info","title":"{INFO_TITLE}","message":"{INFO_MSG}","severity":"info"}}]"#
    )
}

fn spawn_with_announcements(content: &ContentController, override_json: &str) -> PtyHarness {
    let binary = pager_binary().expect("resolve pager binary");
    let overrides: Vec<(String, String)> = vec![(
        "ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE".into(),
        override_json.to_owned(),
    )];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        content,
        &[],
        &env_refs,
        Some(content.home()),
    )
    .expect("spawn pager with announcements override")
}

/// Welcome shows the announcement title (hero path still paints `title`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn critical_announcement_title_on_welcome() {
    let content = ContentController::start().await.expect("start content");
    let mut harness = spawn_with_announcements(&content, &critical_override_json());

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .wait_for_text(CRIT_TITLE, Duration::from_secs(10))
        .expect("critical title on welcome");
    harness
        .wait_for_text(CRIT_MSG, Duration::from_secs(5))
        .expect("critical message on welcome");

    let screen = harness.screen_contents();
    assert!(
        !screen.contains('‼') && !screen.contains('⚠') && !screen.contains('ℹ'),
        "welcome must not use severity emoji prefixes\nscreen:\n{screen}"
    );

    harness.quit().expect("clean quit");
}

/// After entering a session, the critical banner is exactly the two-line
/// layout: `! Title` with a right-aligned `[hide]` button, then the message
/// (column-aligned with the title) followed by `hide: /announcements hide`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn critical_announcement_session_banner_two_lines() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} after critical banner."));

    let mut harness = spawn_with_announcements(&content, &critical_override_json());

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt to enter session");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session response");

    // Session top banner content (may already be visible mid-turn).
    harness
        .wait_for_text(&format!("! {CRIT_TITLE}"), Duration::from_secs(10))
        .expect("alert-prefixed critical title in session");
    harness
        .wait_for_text(CRIT_MSG, Duration::from_secs(5))
        .expect("critical message in session");
    harness
        .wait_for_text(HIDE_BUTTON, Duration::from_secs(5))
        .expect("[hide] button on title row");
    harness
        .wait_for_text(HIDE_CTA, Duration::from_secs(5))
        .expect("hide CTA on message row");

    let screen = harness.screen_contents();
    assert!(
        !screen.contains('‼') && !screen.contains('⚠') && !screen.contains('ℹ'),
        "session banner must not use severity emoji prefixes\nscreen:\n{screen}"
    );
    // Two-row layout: [hide] shares the title row; the CTA shares the message
    // row; the message column lines up with the title column (past the `! `).
    let (t_row, t_col) = locate_screen_text(&screen, CRIT_TITLE).expect("locate title on screen");
    let (m_row, m_col) = locate_screen_text(&screen, CRIT_MSG).expect("locate message on screen");
    assert_eq!(
        m_row,
        t_row + 1,
        "message must be the row under the title\nscreen:\n{screen}"
    );
    assert_eq!(
        m_col, t_col,
        "message column must align with the title column\nscreen:\n{screen}"
    );
    let title_line = screen.lines().nth(t_row as usize).unwrap_or_default();
    assert!(
        title_line.contains(HIDE_BUTTON),
        "[hide] must sit on the title row\nscreen:\n{screen}"
    );
    let msg_line = screen.lines().nth(m_row as usize).unwrap_or_default();
    assert!(
        msg_line.contains(HIDE_CTA),
        "CTA must sit on the message row\nscreen:\n{screen}"
    );

    harness.quit().expect("clean quit");
}

/// Clicking the banner's `[hide]` button collapses it exactly like
/// `/announcements hide`. Also pins the row-1 reservation: a long message
/// truncates with an ellipsis while the CTA keeps its full width.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn critical_announcement_hide_button_click_hides_banner() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} hide button click."));

    // 115-col message vs row-1 budget DEFAULT_COLS − 29: the truncation asserts below hold only while DEFAULT_COLS ≤ 143.
    let long_msg = format!(
        "{CRIT_MSG} elevated error rates persist across regions check status.echo-agent.invalid for updates and retry your request later"
    );
    let override_json = format!(
        r#"[{{"id":"pty-crit-click","title":"{CRIT_TITLE}","message":"{long_msg}","severity":"critical"}}]"#
    );
    let mut harness = spawn_with_announcements(&content, &override_json);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("enter session");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session response");
    harness
        .wait_for_text(HIDE_BUTTON, Duration::from_secs(10))
        .expect("[hide] button visible");
    harness
        .wait_for_text(CRIT_MSG, Duration::from_secs(5))
        .expect("message prefix visible");

    // Reservation: the truncated message row still ends with the intact CTA.
    let screen = harness.screen_contents();
    let (m_row, _) = locate_screen_text(&screen, CRIT_MSG).expect("locate message row");
    let msg_line = screen.lines().nth(m_row as usize).unwrap_or_default();
    assert!(
        msg_line.contains('…'),
        "long message must truncate with an ellipsis\nline:{msg_line:?}"
    );
    assert!(
        msg_line.trim_end().ends_with(HIDE_CTA),
        "CTA must keep its full reserved width\nline:{msg_line:?}"
    );

    // Click the [hide] button (SGR press + release at its first cell).
    let (h_row, h_col) = locate_screen_text(&screen, HIDE_BUTTON).expect("locate [hide]");
    let click = format!(
        "{}{}",
        sgr_mouse(0, h_row, h_col + 1, 'M'),
        sgr_mouse(0, h_row, h_col + 1, 'm')
    );
    harness.inject_keys(click.as_bytes()).expect("click [hide]");

    // The banner collapses exactly like `/announcements hide`.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        if !harness.contains_text(HIDE_BUTTON)
            && !harness.contains_text(CRIT_TITLE)
            && !harness.contains_text(CRIT_MSG)
        {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "[hide] click did not clear the session banner\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }

    harness.quit().expect("clean quit");
}

/// Info-only announcements never open the session top banner (no hide CTA).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn info_announcement_no_session_banner() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} info-only path."));

    let mut harness = spawn_with_announcements(&content, &info_override_json());

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    // Welcome may still show the info announcement.
    harness
        .wait_for_text(INFO_TITLE, Duration::from_secs(10))
        .expect("info title on welcome");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session response");

    // MOCK_RESPONSE_SENTINEL (waited above) is the positive in-session sync point; short settle covers a late banner paint.
    harness.update(Duration::from_millis(500));
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(HIDE_CTA),
        "info-only must not open session critical banner (no hide CTA)\nscreen:\n{screen}"
    );

    harness.quit().expect("clean quit");
}

/// `/announcements hide` clears the session critical banner; `show` restores it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn critical_announcements_slash_hide_and_show() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} for hide/show."));

    let mut harness = spawn_with_announcements(&content, &critical_override_json());

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("enter session");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("response");
    harness
        .wait_for_text(HIDE_CTA, Duration::from_secs(10))
        .expect("banner visible before hide");

    harness
        .inject_keys(b"/announcements hide\r")
        .expect("hide command");

    // Wait until the hide CTA is gone (banner collapsed).
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        if !harness.contains_text(HIDE_CTA) && !harness.contains_text(CRIT_MSG) {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "/announcements hide did not clear session banner\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }

    harness
        .inject_keys(b"/announcements show\r")
        .expect("show command");
    harness
        .wait_for_text(HIDE_CTA, Duration::from_secs(10))
        .expect("banner restored after show");
    harness
        .wait_for_text(CRIT_MSG, Duration::from_secs(5))
        .expect("message restored after show");

    harness.quit().expect("clean quit");
}

/// Slash menu lists `/announcements` only when a critical announcement exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn announcements_slash_listed_only_for_critical() {
    // ── Critical: command appears in dropdown ──────────────────────────
    {
        let content = ContentController::start().await.expect("start content");
        content.set_response(format!("{MOCK_RESPONSE_SENTINEL} critical slash."));
        let mut harness = spawn_with_announcements(&content, &critical_override_json());

        harness
            .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
            .expect("welcome");
        harness
            .inject_keys(format!("{PROMPT}\r").as_bytes())
            .expect("enter session");
        harness
            .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
            .expect("response");
        harness
            .wait_for_text(HIDE_CTA, Duration::from_secs(10))
            .expect("critical banner up");

        // Narrow dropdown to announcements; description only renders in the menu.
        harness.inject_keys(b"/announ").expect("type slash prefix");
        harness
            .wait_for_text(SLASH_DESC, Duration::from_secs(10))
            .unwrap_or_else(|_| {
                panic!(
                    "expected /announcements in slash menu when critical exists\nscreen:\n{}",
                    harness.screen_contents()
                )
            });

        // Dismiss dropdown so quit is clean.
        harness.inject_keys(keys::ESC).expect("esc dropdown");
        harness.update(Duration::from_millis(200));
        harness.quit().expect("quit critical case");
    }

    // ── Info-only: command must not appear ─────────────────────────────
    {
        let content = ContentController::start().await.expect("start content");
        content.set_response(format!("{MOCK_RESPONSE_SENTINEL} info slash."));
        let mut harness = spawn_with_announcements(&content, &info_override_json());

        harness
            .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
            .expect("welcome");
        harness
            .inject_keys(format!("{PROMPT}\r").as_bytes())
            .expect("enter session");
        harness
            .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
            .expect("response");
        harness.update(Duration::from_millis(400));

        harness.inject_keys(b"/announ").expect("type slash prefix");
        // Positive anchor: echoed prompt input proves keystrokes processed (dropdown recomputed) before asserting absence.
        harness
            .wait_for_text("/announ", Duration::from_secs(5))
            .expect("slash prefix echoed in prompt");
        harness.update(Duration::from_millis(200));
        let screen = harness.screen_contents();
        assert!(
            !screen.contains(SLASH_DESC),
            "info-only must not list /announcements in slash menu\nscreen:\n{screen}"
        );

        harness.inject_keys(keys::ESC).expect("esc");
        harness.update(Duration::from_millis(200));
        harness.quit().expect("quit info case");
    }
}

/// A critical announcement added server-side AFTER a session is live reaches
/// the open TUI via the shell's periodic settings refresh — no `/new`, no
/// restart. Uses the shared 1s-poll oauth spawn (no announcements override).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn critical_announcement_reaches_live_session_via_periodic_refresh() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} periodic refresh."));
    let mut harness = spawn_polling_session(&content, "pty-announce-refresh");

    // Steady-state: several poll cycles with unchanged settings must show no banner.
    harness.update(Duration::from_secs(3));
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(CRIT_TITLE) && !screen.contains(HIDE_CTA),
        "no banner may exist before the server-side change\nscreen:\n{screen}"
    );

    // Server-side change mid-session (the remote settings flip): the next
    // `GET /v1/settings` returns a critical announcement.
    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [{
            "id": "pty-crit-live",
            "title": CRIT_TITLE,
            "message": CRIT_MSG,
            "severity": "critical",
        }],
    }));

    // Poll (1s) + push + redraw: the banner appears without /new or restart.
    harness
        .wait_for_text(CRIT_TITLE, Duration::from_secs(30))
        .expect("pushed critical title in live session");
    harness
        .wait_for_text(CRIT_MSG, Duration::from_secs(5))
        .expect("pushed critical message in live session");
    harness
        .wait_for_text(HIDE_CTA, Duration::from_secs(5))
        .expect("hide CTA on pushed session banner");

    // The push must also open the `/announcements` slash gate.
    harness.inject_keys(b"/announ").expect("type slash prefix");
    harness
        .wait_for_text(SLASH_DESC, Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expected /announcements in slash menu after the push\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    harness.inject_keys(keys::ESC).expect("esc dropdown");
    harness.update(Duration::from_millis(200));
    harness.quit().expect("clean quit");
}

/// Per-ID hide: hiding critical A must not suppress a DIFFERENT critical B
/// pushed later in the same session — the banner re-arms for new ids. Uses
/// the shared 1s-poll oauth spawn (no announcements override).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn hidden_critical_does_not_suppress_new_critical_id() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} per-id hide."));
    let mut harness = spawn_polling_session(&content, "pty-announce-perid");

    // Critical A arrives via the poll push.
    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [{
            "id": "pty-crit-a",
            "title": CRIT_TITLE,
            "message": CRIT_MSG,
            "severity": "critical",
        }],
    }));
    harness
        .wait_for_text(CRIT_TITLE, Duration::from_secs(30))
        .expect("critical A banner");
    harness
        .wait_for_text(HIDE_CTA, Duration::from_secs(5))
        .expect("hide CTA on A banner");

    harness
        .inject_keys(b"/announcements hide\r")
        .expect("hide command");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        if !harness.contains_text(HIDE_CTA) && !harness.contains_text(CRIT_MSG) {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "/announcements hide did not clear banner A\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }

    // Several poll cycles with the UNCHANGED list must not resurrect hidden A.
    harness.update(Duration::from_secs(3));
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(CRIT_TITLE) && !screen.contains(HIDE_CTA),
        "hidden critical A must stay hidden across identical polls\nscreen:\n{screen}"
    );

    // Server-side flip to critical B (new id): the banner must re-arm.
    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [{
            "id": "pty-crit-b",
            "title": CRIT_B_TITLE,
            "message": CRIT_B_MSG,
            "severity": "critical",
        }],
    }));
    harness
        .wait_for_text(CRIT_B_TITLE, Duration::from_secs(30))
        .expect("critical B banner after hiding A");
    harness
        .wait_for_text(CRIT_B_MSG, Duration::from_secs(5))
        .expect("critical B message");
    harness
        .wait_for_text(HIDE_CTA, Duration::from_secs(5))
        .expect("hide CTA re-armed for B");
    assert!(
        !harness.contains_text(CRIT_TITLE),
        "A's banner content must not linger after the B push\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}

/// A promo pushed mid-session (poll flip) paints the 1-line promo row —
/// `[label]` button and both hide affordances on ONE row (the message is NOT
/// painted on the banner; it lives on the welcome hero) — and opens the
/// `/announcements` slash gate; a critical published mid-promo takes over the
/// single banner slot (promo row gone, red critical up).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn promo_announcement_banner_slash_gate_and_critical_preemption() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} promo banner."));
    let mut harness = spawn_polling_session(&content, "pty-announce-promo");

    // Steady-state: several poll cycles with unchanged settings, no banner.
    harness.update(Duration::from_secs(2));
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(PROMO_MSG) && !screen.contains(HIDE_CTA),
        "no banner may exist before the promo push\nscreen:\n{screen}"
    );

    // Server-side flip: the next `GET /v1/settings` returns a promo with CTA.
    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [promo_settings_announcement()],
    }));

    harness
        .wait_for_text(PROMO_BUTTON, Duration::from_secs(30))
        .expect("promo [label] button in live session");
    harness
        .wait_for_text(HIDE_BUTTON, Duration::from_secs(5))
        .expect("[hide] button on promo row");
    harness
        .wait_for_text(HIDE_CTA, Duration::from_secs(5))
        .expect("hide CTA on promo row");

    // One-line layout: the row starts with the [label] button and carries both
    // right-hand hide affordances (the promo message is not painted here).
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(PROMO_MSG),
        "the promo message must NOT paint on the session banner\nscreen:\n{screen}"
    );
    // The `[label]` button now also renders on the in-session top header (after
    // the cwd), so target the BANNER row specifically: the only row carrying
    // both the button and the hide affordances (the header has neither hide).
    let row_line = screen
        .lines()
        .find(|l| l.contains(PROMO_BUTTON) && l.contains(HIDE_CTA))
        .unwrap_or_else(|| panic!("locate promo banner row\nscreen:\n{screen}"));
    // Left-aligned: only the frame's left inset may precede the button.
    let (lead, _) = row_line
        .split_once(PROMO_BUTTON)
        .expect("button on the banner row");
    assert!(
        lead.trim().is_empty(),
        "[label] must lead the banner row (inset only)\nline:{row_line:?}"
    );
    assert!(
        row_line.contains(HIDE_CTA) && row_line.trim_end().ends_with(HIDE_BUTTON),
        "hide affordances must sit right-aligned on the promo banner row\nline:{row_line:?}"
    );
    assert!(
        !screen.contains('‼') && !screen.contains('⚠') && !screen.contains('ℹ'),
        "promo row must not use severity emoji prefixes\nscreen:\n{screen}"
    );

    // The push must open the `/announcements` slash gate for promo too.
    harness.inject_keys(b"/announ").expect("type slash prefix");
    harness
        .wait_for_text(SLASH_DESC, Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expected /announcements in slash menu for a promo\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    // Crossterm may hold a lone ESC briefly to disambiguate CSI sequences; a
    // fixed 200ms pump races the dismiss paint under remote CI (same class as
    // minimal_slash_dropdown_dismisses_with_esc). Wait for the sentinel to
    // leave, clear the residual `/announ` draft, then re-sync the promo row so
    // the preemption wait starts from a known banner state.
    harness.inject_keys(keys::ESC).expect("esc dropdown");
    harness
        .wait_for_text_absent(SLASH_DESC, Duration::from_secs(10))
        .expect("slash dropdown dismissed after Esc");
    harness
        .inject_keys(b"\x15")
        .expect("Ctrl+U clear residual slash draft");
    harness
        .wait_for_text(PROMO_BUTTON, Duration::from_secs(10))
        .expect("promo banner still up after slash dismiss");

    // Critical published mid-promo, with the promo STILL in the list: the
    // single slot flips to the critical banner (precedence, not replacement).
    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [
            promo_settings_announcement(),
            {
                "id": "pty-crit-preempt",
                "title": CRIT_TITLE,
                "message": CRIT_MSG,
                "severity": "critical",
            },
        ],
    }));
    harness
        .wait_for_text(&format!("! {CRIT_TITLE}"), Duration::from_secs(30))
        .expect("critical banner preempts the promo");
    harness
        .wait_for_text(CRIT_MSG, Duration::from_secs(5))
        .expect("critical message after preemption");
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(PROMO_BUTTON) && !screen.contains(PROMO_MSG),
        "one banner slot: the promo row must yield to the critical\nscreen:\n{screen}"
    );

    harness.quit().expect("clean quit");
}

/// Clicking the promo `[label]` button dispatches the open action through the
/// safe-open path — observed via the `ECHO_AGENT_TEST_OPEN_URL_FILE` seam, never a
/// real browser — and does NOT hide the row. The raw PTY stream also carries
/// the CTA URL as OSC 8 (WezTerm pin), the mouse-off/tmux fallback. Then the
/// promo hide roundtrip: `/announcements hide` clears the row, `show` restores.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn promo_cta_click_opens_link_and_hide_roundtrip() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} promo click."));
    let url_file = content.home().join("opened-urls.txt");
    let url_file_str = url_file.to_str().expect("utf8 url file path").to_owned();
    // OSC 8 emission is gated on a Native-capable brand (`hyperlink_route`);
    // pin WezTerm like the file-path hyperlink test so the byte-level OSC 8
    // assert below is meaningful.
    let extra_env = [
        ("ECHO_AGENT_TEST_OPEN_URL_FILE", url_file_str.as_str()),
        ("TERM_PROGRAM", "WezTerm"),
    ];
    let mut harness = spawn_polling_session_with_env(&content, "pty-announce-cta", &extra_env);

    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [promo_settings_announcement()],
    }));
    harness
        .wait_for_text(PROMO_BUTTON, Duration::from_secs(30))
        .expect("promo [label] button in live session");

    // Give the frame a beat to flush, then prove the button cells carry the
    // CTA URL as OSC 8 (the URL never renders as text, so raw-stream presence
    // means the hyperlink wrap).
    harness.update(Duration::from_millis(400));
    let raw = String::from_utf8_lossy(harness.raw_output()).into_owned();
    assert!(
        raw.contains("\x1b]8;"),
        "expected OSC 8 sequences in PTY output for the promo CTA"
    );
    assert!(
        raw.contains(PROMO_URL),
        "OSC 8 must carry the promo CTA URL; snippets: {}",
        osc8_snippets(&raw)
    );

    // SGR click on the BANNER [label] button (the row with the hide affordances),
    // not the in-session top-header copy of the button.
    let screen = harness.screen_contents();
    let (b_row, banner_line) = screen
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(PROMO_BUTTON) && l.contains(HIDE_CTA))
        .map(|(i, l)| (i as u16, l))
        .unwrap_or_else(|| panic!("locate promo banner row\nscreen:\n{screen}"));
    let b_col = {
        let byte = banner_line
            .find(PROMO_BUTTON)
            .expect("button on the banner row");
        banner_line[..byte].chars().count() as u16
    };
    let click = format!(
        "{}{}{}",
        sgr_mouse(35, b_row, b_col + 1, 'M'),
        sgr_mouse(0, b_row, b_col + 1, 'M'),
        sgr_mouse(0, b_row, b_col + 1, 'm')
    );
    harness
        .inject_keys(click.as_bytes())
        .expect("click [label]");

    // Dispatch resolves the promo URL from current state and routes it via
    // open_url_if_safe; the seam records it instead of launching a browser.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        let opened = std::fs::read_to_string(&url_file).unwrap_or_default();
        if opened.lines().any(|l| l == PROMO_URL) {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "[label] click did not open the CTA URL via the seam\nrecorded:{opened:?}\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }

    // Drop hover (pointer away) and pin: an open click must not hide the row.
    harness
        .inject_keys(sgr_mouse(35, 2, 2, 'M').as_bytes())
        .expect("move pointer away");
    harness.update(Duration::from_millis(300));
    assert!(
        harness.contains_text(PROMO_BUTTON),
        "CTA click must not hide the promo row\nscreen:\n{}",
        harness.screen_contents()
    );

    // Per-ID hide roundtrip on the promo: hide clears the row...
    harness
        .inject_keys(b"/announcements hide\r")
        .expect("hide command");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        if !harness.contains_text(PROMO_BUTTON) {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "/announcements hide did not clear the promo row\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }

    // ...and show restores the button (round trip through the persisted hidden
    // ids). The message stays hero-only, so the banner restores just the button.
    harness
        .inject_keys(b"/announcements show\r")
        .expect("show command");
    harness
        .wait_for_text(PROMO_BUTTON, Duration::from_secs(10))
        .expect("promo row restored after show");

    harness.quit().expect("clean quit");
}

/// `dismissible: false` pins the promo: no hide affordances paint and
/// `/announcements hide` leaves the banner on screen; a later dismissible promo
/// re-arms [hide] and hides normally (back-compat pinned in-scenario). The
/// message is hero-only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn non_dismissible_promo_ignores_hide_then_dismissible_hides() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} pinned promo."));
    let mut harness = spawn_polling_session(&content, "pty-announce-pinned");

    // Pinned promo arrives via the poll flip.
    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [{
            "id": "pty-promo-pinned",
            "message": PROMO_MSG,
            "severity": "promo",
            "dismissible": false,
            "cta": { "label": PROMO_LABEL, "url": PROMO_URL },
        }],
    }));
    harness
        .wait_for_text(PROMO_BUTTON, Duration::from_secs(30))
        .expect("pinned promo [label] button");

    // Pinned promo: the message is never painted on the banner (hero-only).
    harness.update(Duration::from_millis(300));
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(PROMO_MSG),
        "the promo message must NOT paint on the session banner\nscreen:\n{screen}"
    );

    // No hide affordances anywhere on the pinned row.
    assert!(
        !screen.contains(HIDE_BUTTON) && !screen.contains(HIDE_CTA),
        "pinned promo must paint no hide affordances\nscreen:\n{screen}"
    );

    // This fixture configures no `cta.caption`: the banner row stays the bare
    // button (no dim helper text follows it).
    assert!(
        screen.lines().any(|l| l.trim() == PROMO_BUTTON),
        "a caption-less pinned promo paints a bare [label] banner row\nscreen:\n{screen}"
    );

    // The slash hide no-ops: several poll cycles later the row is still up.
    harness
        .inject_keys(b"/announcements hide\r")
        .expect("hide command");
    harness.update(Duration::from_secs(3));
    assert!(
        harness.contains_text(PROMO_BUTTON),
        "/announcements hide must not clear a pinned promo\nscreen:\n{}",
        harness.screen_contents()
    );

    // Back-compat: a dismissible promo re-arms [hide] and hides normally. Its
    // message is hero-only too, so [hide] appearing signals the flip.
    content.server().set_settings(json!({
        "allow_access": true,
        "announcements": [{
            "id": "pty-promo-hideable",
            "message": PROMO_B_MSG,
            "severity": "promo",
            "cta": { "label": PROMO_LABEL, "url": PROMO_URL },
        }],
    }));
    harness
        .wait_for_text(HIDE_BUTTON, Duration::from_secs(30))
        .expect("[hide] re-armed for the dismissible promo");
    harness
        .inject_keys(b"/announcements hide\r")
        .expect("hide dismissible promo");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        if !harness.contains_text(HIDE_BUTTON) && !harness.contains_text(PROMO_BUTTON) {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "/announcements hide did not clear the dismissible promo\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }

    harness.quit().expect("clean quit");
}

/// Pinned (`dismissible:false`) promo seeded via `ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE`.
fn pinned_promo_override_json() -> String {
    format!(
        r#"[{{"id":"pty-promo-pinned","message":"{PROMO_MSG}","severity":"promo","dismissible":false,"cta":{{"label":"{PROMO_LABEL}","url":"{PROMO_URL}","caption":"{PROMO_CAPTION}"}}}}]"#
    )
}

/// [`spawn_with_announcements`] with extra env pairs appended (e.g. the
/// `ECHO_AGENT_TEST_OPEN_URL_FILE` seam + a `TERM_PROGRAM` pin for OSC 8).
fn spawn_with_announcements_and_env(
    content: &ContentController,
    override_json: &str,
    extra_env: &[(&str, &str)],
) -> PtyHarness {
    let binary = pager_binary().expect("resolve pager binary");
    let announcement = ("ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE", override_json);
    let mut env_refs = vec![announcement];
    env_refs.extend_from_slice(extra_env);
    PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        content,
        &[],
        &env_refs,
        Some(content.home()),
    )
    .expect("spawn pager with announcements override + env")
}

/// Free-tier multi-surface upgrade CTA. A pinned promo surfaces the `[label]`
/// button with NO `[hide]` affordance on the welcome hero, the in-session top
/// header (after the cwd path), and the above-prompt banner; `Ctrl+O` opens the
/// url via the seam. The dashboard surface + the "Ctrl+O toggles YOLO when no
/// pinned CTA" fallback are covered by the colocated unit tests (dashboard
/// render/input, agent input arm).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn pinned_promo_multi_surface_and_ctrl_o_open() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} multi-surface promo."));
    let url_file = content.home().join("opened-urls.txt");
    let url_file_str = url_file.to_str().expect("utf8 url file path").to_owned();
    let extra_env = [
        ("ECHO_AGENT_TEST_OPEN_URL_FILE", url_file_str.as_str()),
        ("TERM_PROGRAM", "WezTerm"),
    ];
    let mut harness =
        spawn_with_announcements_and_env(&content, &pinned_promo_override_json(), &extra_env);

    // (1) Welcome hero: [label], no [hide].
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .wait_for_text(PROMO_BUTTON, Duration::from_secs(10))
        .expect("upgrade CTA on the welcome hero");
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(HIDE_BUTTON),
        "a pinned promo shows no [hide] on welcome\nscreen:\n{screen}"
    );

    // (2) Enter a session.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt to enter session");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session response");

    // (3) + (4) The [label] paints on BOTH the top header row (after the cwd)
    //     and the above-prompt banner, none with [hide].
    harness
        .wait_for_text(PROMO_BUTTON, Duration::from_secs(10))
        .expect("upgrade CTA in session");
    harness.update(Duration::from_millis(300));
    let screen = harness.screen_contents();
    assert!(
        !screen.contains(HIDE_BUTTON),
        "a pinned promo shows no [hide] in session\nscreen:\n{screen}"
    );
    let button_rows = screen.lines().filter(|l| l.contains(PROMO_BUTTON)).count();
    assert!(
        button_rows >= 2,
        "the [label] must paint on BOTH the header and the banner rows (got {button_rows})\nscreen:\n{screen}"
    );
    // The configured caption follows the banner button ONLY: the in-session
    // header always passes a bare button, so exactly one row pairs them.
    let caption_pair = format!("{PROMO_BUTTON} {PROMO_CAPTION}");
    let caption_rows = screen.lines().filter(|l| l.contains(&caption_pair)).count();
    assert_eq!(
        caption_rows, 1,
        "the cta.caption must paint after the banner button and never after the header button\nscreen:\n{screen}"
    );

    // (5) Ctrl+O opens the CTA url via the seam (the pinned promo steals the
    //     chord from YOLO). The banner cells also carry the URL as OSC 8.
    let raw = String::from_utf8_lossy(harness.raw_output()).into_owned();
    assert!(
        raw.contains(PROMO_URL),
        "OSC 8 must carry the promo CTA URL; snippets: {}",
        osc8_snippets(&raw)
    );
    harness
        .inject_keys(CTRL_O)
        .expect("Ctrl+O opens the pinned CTA");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        let opened = std::fs::read_to_string(&url_file).unwrap_or_default();
        if opened.lines().any(|l| l == PROMO_URL) {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "Ctrl+O did not open the pinned CTA url via the seam\nrecorded:{opened:?}\nscreen:\n{}",
                harness.screen_contents()
            );
        }
    }

    harness.quit().expect("clean quit");
}
