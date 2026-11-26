//! Vendor-compatibility end-to-end tests.
//!
//! Each test builds a fake `$HOME` containing skills/rules/AGENTS.md under the
//! `.echo-agent`, `.cursor`, and `.claude` vendor dirs, spawns a real `echoagent agent
//! stdio` process against the mock inference server (toggling the
//! `ECHO_AGENT_<VENDOR>_<SURFACE>_ENABLED` env vars via `cmd.env`), sends one prompt,
//! and asserts on the full inference request bodies:
//!
//! - the EchoAgent-native skill is always present regardless of toggles
//! - each of the 6 (vendor x surface) cells toggles independently
//! - a vendor-shipped default skill (`shell`) under `~/.cursor` is always
//!   dropped by the denylist
//! - cross-vendor combos (all-cursor-off, all-claude-off, all-off) work
//!
//! These are `#[ignore]` (they spawn a built binary) like the agent-type
//! invariant suite. Run locally:
//! ```bash
//! cargo test -p echo-agent-runtime --test test_vendor_compat -- --ignored
//! ```

use std::future::Future;
use std::path::Path;

use echo_agent_test_support::*;

async fn with_local_set<F, Fut>(f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::task::LocalSet::new().run_until(f()).await;
}

/// Unique markers placed in skill descriptions / file contents so assertions
/// can't be fooled by incidental occurrences of a bare word like "shell".
const MARKER_ECHO_AGENT_SKILL: &str = "ZZ_ECHO_AGENT_SKILL_MARKER";
const MARKER_CURSOR_SKILL: &str = "ZZ_CURSOR_SKILL_MARKER";
const MARKER_CURSOR_SHELL: &str = "ZZ_CURSOR_SHELL_DENYLISTED_MARKER";
const MARKER_CLAUDE_SKILL: &str = "ZZ_CLAUDE_SKILL_MARKER";
const MARKER_CURSOR_RULE: &str = "ZZ_CURSOR_RULE_MARKER";
const MARKER_CLAUDE_RULE: &str = "ZZ_CLAUDE_RULE_MARKER";
const MARKER_CLAUDE_AGENTS: &str = "ZZ_CLAUDE_AGENTS_MARKER";
const MARKER_CURSOR_AGENTS: &str = "ZZ_CURSOR_AGENTS_MARKER";

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("path has parent")).expect("create dirs");
    std::fs::write(path, contents).expect("write file");
}

/// Write a `<vendor>/skills/<name>/SKILL.md` with the given description marker.
fn write_skill(home: &Path, vendor_dir: &str, name: &str, marker: &str) {
    let p = home
        .join(vendor_dir)
        .join("skills")
        .join(name)
        .join("SKILL.md");
    write_file(
        &p,
        &format!("---\nname: {name}\ndescription: {marker}\n---\n\nSkill body.\n"),
    );
}

/// Populate a fake `$HOME` + repo cwd with the full vendor-compat fixture set.
fn seed_fixtures(home: &Path, cwd: &Path) {
    // Skills (User scope, home-based).
    write_skill(
        home,
        ".echo-agent",
        "echo-agent-skill",
        MARKER_ECHO_AGENT_SKILL,
    );
    write_skill(home, ".cursor", "my-cursor-skill", MARKER_CURSOR_SKILL);
    // `shell` is a Cursor vendor-default → must be denylisted under ~/.cursor.
    write_skill(home, ".cursor", "shell", MARKER_CURSOR_SHELL);
    write_skill(home, ".claude", "my-claude-skill", MARKER_CLAUDE_SKILL);

    // Rules: repo-local `.cursor/rules/r.md` and `.claude/rules/c.md`
    // (discovered via the cwd→root walk, gated by their respective rules cell).
    write_file(
        &cwd.join(".cursor").join("rules").join("r.md"),
        &format!("# rule\n{MARKER_CURSOR_RULE}\n"),
    );
    write_file(
        &cwd.join(".claude").join("rules").join("c.md"),
        &format!("# rule\n{MARKER_CLAUDE_RULE}\n"),
    );
    // AGENTS.md: `~/.claude/CLAUDE.md` and `~/.cursor/AGENTS.md`
    // (discovered via the home compat scan, gated by their respective agents cell).
    write_file(
        &home.join(".claude").join("CLAUDE.md"),
        &format!("# claude instructions\n{MARKER_CLAUDE_AGENTS}\n"),
    );
    write_file(
        &home.join(".cursor").join("AGENTS.md"),
        &format!("# cursor instructions\n{MARKER_CURSOR_AGENTS}\n"),
    );
}

/// Spawn the agent with the given compat env overrides, send one prompt, and
/// return every inference request body concatenated into one string for
/// substring assertions (system prompt + skill listing + injected reminders).
async fn run_scenario(env: &[(&str, &str)]) -> String {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let mut sandbox = TestSandbox::new();
    seed_fixtures(sandbox.home(), workdir.workspace());
    sandbox.extend_env(env.iter().copied());

    let client =
        EchoAgentStdioClient::spawn_with_sandbox(&server, workdir.workspace(), sandbox).await;
    client.initialize_with_timeout().await;
    let session_id = client
        .create_session_with_timeout(workdir.workspace())
        .await;
    let _ = client.prompt_with_timeout(&session_id, "hello").await;

    let bodies: Vec<String> = server
        .requests()
        .iter()
        .filter_map(|e| e.body.as_ref().map(|b| b.to_string()))
        .collect();
    assert!(
        !bodies.is_empty(),
        "expected at least one inference request; stderr:\n{}",
        client.stderr()
    );
    bodies.join("\n---\n")
}

// ── Skills ──────────────────────────────────────────────────────────────────

/// Defaults (all vendors on): echoagent + cursor-vendor + claude-vendor skills present; the
/// denylisted vendor builtin `shell` is dropped.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_defaults_include_vendor_skills_but_drop_denylisted() {
    with_local_set(|| async {
        let body = run_scenario(&[]).await;
        assert!(
            body.contains(MARKER_ECHO_AGENT_SKILL),
            "echo-agent-skill must always be present"
        );
        assert!(
            body.contains(MARKER_CURSOR_SKILL),
            "cursor skill present when cursor.skills on (default)"
        );
        assert!(
            body.contains(MARKER_CLAUDE_SKILL),
            "claude skill present when claude.skills on (default)"
        );
        assert!(
            !body.contains(MARKER_CURSOR_SHELL),
            "denylisted Cursor builtin `shell` must be dropped"
        );
    })
    .await;
}

/// `ECHO_AGENT_CURSOR_SKILLS_ENABLED=false` drops the cursor-vendor skill; echoagent stays.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_cursor_skills_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[("ECHO_AGENT_CURSOR_SKILLS_ENABLED", "false")]).await;
        assert!(
            body.contains(MARKER_ECHO_AGENT_SKILL),
            "echo-agent-skill always present"
        );
        assert!(
            !body.contains(MARKER_CURSOR_SKILL),
            "cursor skill must be absent when cursor.skills disabled"
        );
        // Denylist still applies regardless of the toggle.
        assert!(!body.contains(MARKER_CURSOR_SHELL));
    })
    .await;
}

/// `ECHO_AGENT_CLAUDE_SKILLS_ENABLED=false` drops the claude-vendor skill; echoagent stays.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_claude_skills_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[("ECHO_AGENT_CLAUDE_SKILLS_ENABLED", "false")]).await;
        assert!(
            body.contains(MARKER_ECHO_AGENT_SKILL),
            "echo-agent-skill always present"
        );
        assert!(
            !body.contains(MARKER_CLAUDE_SKILL),
            "claude skill must be absent when claude.skills disabled"
        );
    })
    .await;
}

// ── Rules + AGENTS.md ────────────────────────────────────────────────────────

/// Defaults: all rules and AGENTS.md surfaces are present.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_rules_and_agents_present_by_default() {
    with_local_set(|| async {
        let body = run_scenario(&[]).await;
        assert!(
            body.contains(MARKER_CURSOR_RULE),
            "cursor rule present when cursor.rules on (default)"
        );
        assert!(
            body.contains(MARKER_CLAUDE_RULE),
            "claude rule present when claude.rules on (default)"
        );
        assert!(
            body.contains(MARKER_CLAUDE_AGENTS),
            "claude AGENTS.md present when claude.agents on (default)"
        );
        assert!(
            body.contains(MARKER_CURSOR_AGENTS),
            "cursor AGENTS.md present when cursor.agents on (default)"
        );
    })
    .await;
}

// ── Per-cell toggles (rules + agents) ────────────────────────────────────────

/// `ECHO_AGENT_CURSOR_RULES_ENABLED=false` drops cursor-vendor rules; claude-vendor rules stay.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_cursor_rules_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[("ECHO_AGENT_CURSOR_RULES_ENABLED", "false")]).await;
        assert!(
            !body.contains(MARKER_CURSOR_RULE),
            "cursor rule must be absent when cursor.rules disabled"
        );
        assert!(
            body.contains(MARKER_CLAUDE_RULE),
            "claude rule unaffected by cursor.rules toggle"
        );
    })
    .await;
}

/// `ECHO_AGENT_CLAUDE_RULES_ENABLED=false` drops claude-vendor rules; cursor-vendor rules stay.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_claude_rules_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[("ECHO_AGENT_CLAUDE_RULES_ENABLED", "false")]).await;
        assert!(
            !body.contains(MARKER_CLAUDE_RULE),
            "claude rule must be absent when claude.rules disabled"
        );
        assert!(
            body.contains(MARKER_CURSOR_RULE),
            "cursor rule unaffected by claude.rules toggle"
        );
    })
    .await;
}

/// `ECHO_AGENT_CURSOR_AGENTS_ENABLED=false` drops cursor-vendor AGENTS.md; claude-vendor stays.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_cursor_agents_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[("ECHO_AGENT_CURSOR_AGENTS_ENABLED", "false")]).await;
        assert!(
            !body.contains(MARKER_CURSOR_AGENTS),
            "cursor AGENTS.md must be absent when cursor.agents disabled"
        );
        assert!(
            body.contains(MARKER_CLAUDE_AGENTS),
            "claude AGENTS.md unaffected by cursor.agents toggle"
        );
    })
    .await;
}

/// `ECHO_AGENT_CLAUDE_AGENTS_ENABLED=false` drops claude-vendor AGENTS.md; cursor-vendor stays.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_claude_agents_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[("ECHO_AGENT_CLAUDE_AGENTS_ENABLED", "false")]).await;
        assert!(
            !body.contains(MARKER_CLAUDE_AGENTS),
            "claude AGENTS.md must be absent when claude.agents disabled"
        );
        assert!(
            body.contains(MARKER_CURSOR_AGENTS),
            "cursor AGENTS.md unaffected by claude.agents toggle"
        );
    })
    .await;
}

// ── Cross-vendor combinations ────────────────────────────────────────────────

/// All cursor-vendor compat OFF: cursor skills, rules, and AGENTS.md all absent;
/// all claude-vendor surfaces unaffected.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_all_cursor_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[
            ("ECHO_AGENT_CURSOR_SKILLS_ENABLED", "false"),
            ("ECHO_AGENT_CURSOR_RULES_ENABLED", "false"),
            ("ECHO_AGENT_CURSOR_AGENTS_ENABLED", "false"),
        ])
        .await;
        assert!(!body.contains(MARKER_CURSOR_SKILL));
        assert!(!body.contains(MARKER_CURSOR_SHELL));
        assert!(!body.contains(MARKER_CURSOR_RULE));
        assert!(!body.contains(MARKER_CURSOR_AGENTS));
        assert!(
            body.contains(MARKER_ECHO_AGENT_SKILL),
            "echoagent always present"
        );
        assert!(body.contains(MARKER_CLAUDE_SKILL), "claude unaffected");
        assert!(body.contains(MARKER_CLAUDE_RULE), "claude unaffected");
        assert!(body.contains(MARKER_CLAUDE_AGENTS), "claude unaffected");
    })
    .await;
}

/// All claude-vendor compat OFF: claude skills, rules, and AGENTS.md all absent;
/// all cursor-vendor surfaces unaffected.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_all_claude_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[
            ("ECHO_AGENT_CLAUDE_SKILLS_ENABLED", "false"),
            ("ECHO_AGENT_CLAUDE_RULES_ENABLED", "false"),
            ("ECHO_AGENT_CLAUDE_AGENTS_ENABLED", "false"),
        ])
        .await;
        assert!(!body.contains(MARKER_CLAUDE_SKILL));
        assert!(!body.contains(MARKER_CLAUDE_RULE));
        assert!(!body.contains(MARKER_CLAUDE_AGENTS));
        assert!(
            body.contains(MARKER_ECHO_AGENT_SKILL),
            "echoagent always present"
        );
        assert!(body.contains(MARKER_CURSOR_SKILL), "cursor unaffected");
        assert!(body.contains(MARKER_CURSOR_RULE), "cursor unaffected");
        assert!(body.contains(MARKER_CURSOR_AGENTS), "cursor unaffected");
        assert!(!body.contains(MARKER_CURSOR_SHELL), "denylist still active");
    })
    .await;
}

/// All vendor compat OFF: only echo-agent-native skill survives.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn vendor_compat_all_vendors_disabled() {
    with_local_set(|| async {
        let body = run_scenario(&[
            ("ECHO_AGENT_CURSOR_SKILLS_ENABLED", "false"),
            ("ECHO_AGENT_CURSOR_RULES_ENABLED", "false"),
            ("ECHO_AGENT_CURSOR_AGENTS_ENABLED", "false"),
            ("ECHO_AGENT_CLAUDE_SKILLS_ENABLED", "false"),
            ("ECHO_AGENT_CLAUDE_RULES_ENABLED", "false"),
            ("ECHO_AGENT_CLAUDE_AGENTS_ENABLED", "false"),
        ])
        .await;
        assert!(
            body.contains(MARKER_ECHO_AGENT_SKILL),
            "echoagent always present"
        );
        assert!(!body.contains(MARKER_CURSOR_SKILL));
        assert!(!body.contains(MARKER_CURSOR_SHELL));
        assert!(!body.contains(MARKER_CURSOR_RULE));
        assert!(!body.contains(MARKER_CURSOR_AGENTS));
        assert!(!body.contains(MARKER_CLAUDE_SKILL));
        assert!(!body.contains(MARKER_CLAUDE_RULE));
        assert!(!body.contains(MARKER_CLAUDE_AGENTS));
    })
    .await;
}
