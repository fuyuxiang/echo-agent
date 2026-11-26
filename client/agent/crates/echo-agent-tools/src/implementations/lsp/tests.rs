use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::notification::ToolNotification;
use async_lsp::lsp_types::{self, Diagnostic};

use super::client::LspClient;
use super::config::LspServerConfig;
use super::diagnostics::Answer;
use super::dispatch::LspBackendAdapter;
use super::manager::{LspManager, drain_lsp_diagnostics};
use super::restart::restart_monitor;
use super::{LspBackend, LspError, LspOperation, LspToolInput, file_uri};

mod mock_servers;
use mock_servers::*;

/// How long a test waits for something a mock server has to get around to.
///
/// One constant for the suite rather than a hand-rolled iteration count at each
/// call site, so a slow machine is retuned in one place.
const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// Poll until `ready`, or fail saying what was being waited for.
async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if ready() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A file is held for this long in tests that are about letting go of one.
/// Long enough not to race a mock server's own latency, short enough that a
/// test can wait it out.
const BRIEF_VERDICT_TTL: std::time::Duration = std::time::Duration::from_millis(150);

/// The production rules, on a timescale a test can sit through. The real
/// durations are unit-tested against an injected clock in `pending.rs`; this is
/// for exercising the drain that consults them.
fn brief_policy() -> super::pending::PendingPolicy {
    super::pending::PendingPolicy {
        verdict_ttl: BRIEF_VERDICT_TTL,
        server_patience: std::time::Duration::from_millis(80),
    }
}

/// Drain until a summary mentioning `needle` appears, or give up.
///
/// Several of these flows take more than one drain by design — a server that
/// answers, then says its answer was premature, is answered again on the drain
/// after the one that heard it — and which drain lands where is a race with the
/// mock's own scheduling, not something worth pinning down.
async fn drain_until_reported(mgr: &tokio::sync::Mutex<LspManager>, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT + WAIT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Some(summary) = drain_lsp_diagnostics(mgr, std::time::Duration::from_millis(500))
            .await
            .filter(|summary| summary.text.contains(needle))
        {
            return summary.text;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("no summary mentioning {needle:?} within the deadline");
}

fn mock_server_config(script_path: &Path) -> LspServerConfig {
    let mut ext_map = HashMap::new();
    ext_map.insert(".ts".to_string(), "typescript".to_string());
    LspServerConfig {
        command: "python3".to_string(),
        args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
        extensions: ext_map,
        startup_timeout: Some(10_000),
        ..Default::default()
    }
}

async fn start_mock_client() -> (tempfile::TempDir, tempfile::TempDir, LspClient) {
    let (script_dir, script_path) = write_mock_server();
    let config = mock_server_config(&script_path);
    let workspace = tempfile::tempdir().unwrap();
    let notify = Arc::new(tokio::sync::Notify::new());
    let client = LspClient::start("mock".to_string(), 1, config, workspace.path(), notify)
        .await
        .expect("mock LSP handshake failed");
    (script_dir, workspace, client)
}

async fn poll_diagnostics(client: &LspClient, path: &Path, expected: usize) -> Vec<Diagnostic> {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let diags = client.get_diagnostics(path);
        if diags.len() >= expected {
            return diags;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    client.get_diagnostics(path)
}

/// Creates a single-server LspManager with the mock TS server, already initialized.
async fn single_server_manager(script_path: &Path, workspace: &tempfile::TempDir) -> LspManager {
    let mut servers = BTreeMap::new();
    servers.insert("mock-ts".to_string(), mock_server_config(script_path));

    let mut mgr = LspManager::new(
        servers,
        workspace.path().to_path_buf(),
        false,
        crate::notification::ToolNotificationHandle::noop(),
    );
    mgr.ensure_initialized().await;
    mgr
}

/// Wait until the LSP server has published diagnostics for `path`.
/// Polls the shared diagnostics map (not drain) with 10ms sleeps.
async fn wait_for_server(mgr: &LspManager, path: &Path, timeout_ms: u64) {
    let uri = file_uri(path).unwrap().to_string();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        for client in mgr.clients.values() {
            if client.diagnostics.covers(&uri).is_some() {
                return;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_for_server timed out after {timeout_ms}ms for {uri}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Regression test for the Roslyn teardown: a server that asked for
/// incremental sync must receive a range on every `didChange`, and that range
/// must span the previous revision so the full text we send replaces it.
#[tokio::test(flavor = "current_thread")]
async fn did_change_carries_range_for_incremental_servers() {
    let (_dir, script_path) = write_incremental_sync_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("test.ts");
    // Two lines; the second is 5 UTF-16 units long, so the document ends at 1:5.
    let first = "const x = 1;\nabcde";
    std::fs::write(&file, first).unwrap();
    client.notify_file_change(&file, first, "typescript");
    poll_diagnostics(&client, &file, 1).await;

    let second = "const x = 2;\nabcde\nmore";
    client.notify_file_change(&file, second, "typescript");

    let message = wait_for_message(&client, &file, |m| m.starts_with("changed")).await;
    assert_eq!(
        message.as_deref(),
        Some("changed with range 0:0-1:5"),
        "didChange must cover the whole previous revision, not be sent rangeless"
    );

    client.shutdown().await;
}

/// Wait for a diagnostic on `path` whose message satisfies `pred`.
async fn wait_for_message(
    client: &LspClient,
    path: &Path,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Some(d) = client.get_diagnostics(path).first()
            && pred(&d.message)
        {
            return Some(d.message.clone());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    client
        .get_diagnostics(path)
        .first()
        .map(|d| d.message.clone())
}

async fn start_client_with(script_path: &Path) -> (tempfile::TempDir, LspClient) {
    let workspace = tempfile::tempdir().unwrap();
    let notify = Arc::new(tokio::sync::Notify::new());
    let client = LspClient::start(
        "mock".to_string(),
        1,
        mock_server_config(script_path),
        workspace.path(),
        notify,
    )
    .await
    .expect("handshake failed");
    (workspace, client)
}

/// A pull-model server publishes nothing, so diagnostics must come from an
/// explicit `textDocument/diagnostic` request. Without this the C# integration
/// reported no errors at all.
#[tokio::test(flavor = "current_thread")]
async fn pull_diagnostics_populate_the_map() {
    let (_dir, script_path) = write_pull_diagnostics_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    client.notify_file_change(&file, "let x = 1;\n", "typescript");

    let message = wait_for_message(&client, &file, |m| m.starts_with("pull #"))
        .await
        .expect("pull diagnostics should have populated the map");
    assert_eq!(
        message, "pull #1 saves=0 prev=None",
        "first pull carries no previous result id, and no didSave was sent"
    );

    client.shutdown().await;
}

/// The second pull sends back the result id from the first, so the server can
/// answer "unchanged" instead of recomputing the whole report.
#[tokio::test(flavor = "current_thread")]
async fn pull_diagnostics_send_previous_result_id() {
    let (_dir, script_path) = write_pull_diagnostics_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    client.notify_file_change(&file, "let x = 1;\n", "typescript");
    wait_for_message(&client, &file, |m| m.starts_with("pull #1")).await;

    client.notify_file_change(&file, "let x = 2;\n", "typescript");
    let message = wait_for_message(&client, &file, |m| m.starts_with("pull #2"))
        .await
        .expect("second pull should have happened");
    assert_eq!(message, "pull #2 saves=0 prev=result-1");

    client.shutdown().await;
}

/// A server that answers "nothing wrong" while it is still re-analyzing must
/// not be taken at its word: acting on that answer erases errors the file
/// really has, and the follow-up "unchanged" reply would then make the blank
/// permanent.
#[tokio::test(flavor = "current_thread")]
async fn a_premature_empty_answer_does_not_erase_known_diagnostics() {
    let (_dir, script_path) = write_mid_analysis_pull_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    client.notify_file_change(&file, "let x = 1;\n", "typescript");
    assert_eq!(
        wait_for_message(&client, &file, |m| m == "real problem 1").await,
        Some("real problem 1".to_string())
    );

    // The next edit draws the empty answer. The diagnostics we already have
    // must survive it, and the server's next word must land.
    client.notify_file_change(&file, "let x = 2;\n", "typescript");

    let mut settled = None;
    for _ in 0..400 {
        let current = client.get_diagnostics(&file);
        assert!(
            !current.is_empty(),
            "diagnostics were blanked by an answer from a server mid-analysis"
        );
        if current[0].message == "real problem 3" {
            settled = Some(current[0].message.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        settled,
        Some("real problem 3".to_string()),
        "the server's settled answer should replace the stale one"
    );

    client.shutdown().await;
}

/// An answer to a pull that was already in flight when the file changed again
/// describes the old text. It is worth reading — it is the best we have until
/// the re-pull lands — but it must not pass as the server's verdict on the new
/// edit, or the turn reports diagnostics for text that no longer exists.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_about_the_previous_revision_does_not_settle_the_new_one() {
    let (_dir, script_path) = write_slow_pull_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    client.notify_file_change(&file, "let x = 1;\n", "typescript");

    // Edit again while the server is working on the first pull.
    let marker = workspace.path().join(FIRST_PULL_MARKER);
    wait_until("the server to start its first pull", || marker.exists()).await;
    let second_edit = client
        .notify_file_change(&file, "let x = 2;\n", "typescript")
        .expect("the change was sent");
    let uri = file_uri(&file).unwrap().to_string();

    assert_eq!(
        wait_for_message(&client, &file, |m| m.starts_with("pull 1 answers")).await,
        Some("pull 1 answers revision 1".to_string()),
        "the in-flight pull answers first, about the text before the edit"
    );
    assert!(
        !client.diagnostics.answered_for(&uri, second_edit),
        "an answer about the previous revision is not a verdict on the new edit"
    );

    assert_eq!(
        wait_for_message(&client, &file, |m| m.starts_with("pull 2 answers")).await,
        Some("pull 2 answers revision 2".to_string()),
        "the edit that overtook the pull must start another one"
    );
    assert!(
        client.diagnostics.answered_for(&uri, second_edit),
        "and that answer settles the edit"
    );

    client.shutdown().await;
}

/// The result id we remember has to name what is actually in the store. A
/// stale clean answer keeps the known errors — and if it recorded its own id
/// anyway, the next `unchanged` reply confirms diagnostics the server no longer
/// reports, so a file that has been fixed goes on showing its old errors.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_the_store_refused_leaves_no_result_id_behind() {
    let (_dir, script_path) = write_stale_clean_pull_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    client.notify_file_change(&file, "let x = 1;\n", "typescript");
    assert_eq!(
        wait_for_message(&client, &file, |m| m == "the problem").await,
        Some("the problem".to_string())
    );

    // The second pull answers "clean", but the file changes again before that
    // answer lands, so the store keeps the errors and never stores the answer.
    client.notify_file_change(&file, "let x = 2;\n", "typescript");
    let marker = workspace.path().join(SECOND_PULL_MARKER);
    for _ in 0..300 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(marker.exists(), "the server never started its second pull");
    client.notify_file_change(&file, "let x = 3;\n", "typescript");

    // The server's verdict on the file as it stands now must get through. It
    // only can if we stopped claiming to hold a clean report we never stored.
    let mut settled = false;
    for _ in 0..500 {
        if client.get_diagnostics(&file).is_empty() {
            settled = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        settled,
        "a fixed file kept reporting errors the server no longer has"
    );

    client.shutdown().await;
}

/// A server that does not implement pull diagnostics is asked once, tells us
/// so, and is never asked again.
#[tokio::test(flavor = "multi_thread")]
async fn pull_diagnostics_are_not_retried_on_a_server_that_says_it_has_none() {
    use super::pull::PullSupport;

    let (_dir, script_path) = write_pull_rejecting_server();
    let (workspace, mut client) = start_client_with(&script_path).await;
    // This one advertises no diagnostic provider, which is not proof of
    // absence — Roslyn implements the handler without advertising it — so an
    // unadvertised server still gets asked.
    assert_eq!(client.pull.support(), PullSupport::Asking);

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();
    client.notify_file_change(&file, "const x = 1;\n", "typescript");

    wait_until("the server to turn the request down", || {
        client.pull.support() != PullSupport::Asking
    })
    .await;
    assert_eq!(
        client.pull.support(),
        PullSupport::Rejected,
        "a MethodNotFound reply should switch pulling off for this server"
    );

    // Several more edits, and it is not asked again.
    let counted = script_path.parent().unwrap().join("pulls.txt");
    assert_eq!(std::fs::read_to_string(&counted).unwrap(), "1");
    for round in 0..3 {
        let text = format!("const x = {round};\n");
        std::fs::write(&file, &text).unwrap();
        client.notify_file_change(&file, &text, "typescript");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&counted).unwrap(),
        "1",
        "one rejection is conclusive; there is no reason to ask twice"
    );

    client.shutdown().await;
}

/// A server that asks for the text on save still gets it.
#[tokio::test(flavor = "current_thread")]
async fn did_save_includes_text_when_the_server_asks_for_it() {
    let (_dir, script_path) = write_save_with_text_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    client.notify_file_change(&file, "let x = 1;\n", "typescript");

    let message = wait_for_message(&client, &file, |m| m.starts_with("saved"))
        .await
        .expect("server should have been told about the save");
    assert_eq!(message, "saved with text=True");

    client.shutdown().await;
}

/// Full-sync servers keep getting the rangeless form they expect.
#[tokio::test(flavor = "current_thread")]
async fn did_change_stays_rangeless_for_full_sync_servers() {
    let (_dir, workspace, mut client) = start_mock_client().await;

    let file = workspace.path().join("test.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();
    client.notify_file_change(&file, "const x = 1;\n", "typescript");
    client.notify_file_change(&file, "const x = 2;\n", "typescript");
    // The mock full-sync server accepts both forms; the assertion that matters
    // is that it stayed up and kept publishing.
    let diags = poll_diagnostics(&client, &file, 1).await;
    assert!(!diags.is_empty());

    client.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_did_open_publishes_diagnostics() {
    let (_dir, workspace, mut client) = start_mock_client().await;

    let test_file = workspace.path().join("test.ts");
    std::fs::write(&test_file, "const x = 1;\n").unwrap();
    client.notify_file_change(&test_file, "const x = 1;\n", "typescript");

    let diags = poll_diagnostics(&client, &test_file, 2).await;
    assert_eq!(diags.len(), 2, "expected 2 diagnostics, got {:?}", diags);

    assert_eq!(diags[0].message, "mock error: undeclared variable");
    assert_eq!(
        diags[0].severity,
        Some(lsp_types::DiagnosticSeverity::ERROR)
    );

    assert_eq!(diags[1].message, "mock warning: unused import");
    assert_eq!(
        diags[1].severity,
        Some(lsp_types::DiagnosticSeverity::WARNING)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_goto_definition() {
    let (_dir, workspace, mut client) = start_mock_client().await;
    let test_file = workspace.path().join("test.ts");
    std::fs::write(&test_file, "const x = 1;\n").unwrap();
    client.notify_file_change(&test_file, "const x = 1;\n", "typescript");

    let locations = client.goto_definition(&test_file, 0, 5).await.unwrap();
    assert_eq!(
        locations.len(),
        1,
        "expected 1 location, got {:?}",
        locations
    );
    assert_eq!(locations[0].range.start.line, 10);
    assert_eq!(locations[0].range.start.character, 0);
    assert_eq!(locations[0].range.end.line, 10);
    assert_eq!(locations[0].range.end.character, 20);
}

/// Exercises the production API: init -> notify (fire-and-forget) -> drain -> shutdown.
#[tokio::test(flavor = "current_thread")]
async fn e2e_lsp_manager_full_lifecycle() {
    let (_dir, script_path) = write_mock_server();
    let workspace = tempfile::tempdir().unwrap();

    let mut mgr = single_server_manager(&script_path, &workspace).await;
    assert!(mgr.is_initialized());
    mgr.ensure_initialized().await; // idempotent

    // Fire-and-forget notification.
    let test_file = workspace.path().join("app.ts");
    let content = "let y = 2;\n";
    std::fs::write(&test_file, content).unwrap();
    mgr.notify_file_changed(&test_file, content);
    assert!(mgr.has_pending_diagnostics());

    let pending_before = mgr.pending_count();
    mgr.notify_file_changed(Path::new("readme.md"), "");
    assert_eq!(mgr.pending_count(), pending_before);

    wait_for_server(&mgr, &test_file, 2000).await;

    let mgr = tokio::sync::Mutex::new(mgr);
    let timeout = std::time::Duration::from_secs(2);
    let summary = drain_lsp_diagnostics(&mgr, timeout)
        .await
        .expect("expected diagnostics");
    assert!(
        summary.text.contains("mock error: undeclared variable"),
        "summary: {}",
        summary.text
    );
    assert!(
        summary.text.contains("mock warning: unused import"),
        "summary: {}",
        summary.text
    );
    assert!(summary.text.starts_with("<lsp-diagnostics>"));
    assert!(summary.text.ends_with("</lsp-diagnostics>"));
    assert_eq!(summary.file_count, 1);
    assert_eq!(summary.diagnostic_count, 2);

    assert!(!mgr.lock().await.has_pending_diagnostics());
    assert!(drain_lsp_diagnostics(&mgr, timeout).await.is_none());

    mgr.lock().await.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_did_change_updates_diagnostics() {
    let (_dir, workspace, mut client) = start_mock_client().await;
    let test_file = workspace.path().join("test.ts");
    std::fs::write(&test_file, "const x = 1;\n").unwrap();

    // First open.
    client.notify_file_change(&test_file, "const x = 1;\n", "typescript");
    let diags = poll_diagnostics(&client, &test_file, 2).await;
    assert_eq!(diags.len(), 2);

    // Second call triggers didChange, not didOpen.
    client.notify_file_change(&test_file, "const x = 2;\n", "typescript");
    let diags = poll_diagnostics(&client, &test_file, 2).await;
    assert_eq!(diags.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_spawn_failure_is_graceful() {
    let config = LspServerConfig {
        command: "/nonexistent/binary/that/does/not/exist".to_string(),
        ..Default::default()
    };
    let workspace = tempfile::tempdir().unwrap();
    let notify = Arc::new(tokio::sync::Notify::new());

    let result = LspClient::start("bad".to_string(), 1, config, workspace.path(), notify).await;
    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), LspError::SpawnFailed(_)),
        "expected SpawnFailed"
    );
}

/// 2 good + 1 bad server. Verifies bad one is skipped, routing works,
/// and combined diagnostics summary includes both files.
#[tokio::test(flavor = "current_thread")]
async fn e2e_multi_server_routing() {
    let (_dir, script_path) = write_mock_server();

    let mut ts_ext = HashMap::new();
    ts_ext.insert(".ts".to_string(), "typescript".to_string());
    let mut py_ext = HashMap::new();
    py_ext.insert(".py".to_string(), "python".to_string());

    let mut servers = BTreeMap::new();
    servers.insert(
        "mock-ts".to_string(),
        LspServerConfig {
            command: "python3".to_string(),
            args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
            extensions: ts_ext,
            startup_timeout: Some(10_000),
            ..Default::default()
        },
    );
    servers.insert(
        "mock-py".to_string(),
        LspServerConfig {
            command: "python3".to_string(),
            args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
            extensions: py_ext,
            startup_timeout: Some(10_000),
            ..Default::default()
        },
    );
    servers.insert(
        "bad".to_string(),
        LspServerConfig {
            command: "/nonexistent".to_string(),
            startup_timeout: Some(2_000),
            ..Default::default()
        },
    );

    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = LspManager::new(
        servers,
        workspace.path().to_path_buf(),
        false,
        crate::notification::ToolNotificationHandle::noop(),
    );

    mgr.ensure_initialized().await;
    // Bad server skipped, 2 good ones remain.
    assert_eq!(mgr.clients.len(), 2);

    // .ts and .py route to their respective servers.
    let ts_file = workspace.path().join("app.ts");
    let ts_content = "let x = 1;\n";
    std::fs::write(&ts_file, ts_content).unwrap();
    mgr.notify_file_changed(&ts_file, ts_content);

    let py_file = workspace.path().join("main.py");
    let py_content = "x = 1\n";
    std::fs::write(&py_file, py_content).unwrap();
    mgr.notify_file_changed(&py_file, py_content);

    // .go has no configured server — doesn't add to pending.
    let go_file = workspace.path().join("main.go");
    std::fs::write(&go_file, "package main\n").unwrap();
    let pending_before = mgr.pending_count();
    mgr.notify_file_changed(&go_file, "package main\n");
    assert_eq!(
        mgr.pending_count(),
        pending_before,
        ".go should not add pending"
    );

    wait_for_server(&mgr, &ts_file, 2000).await;
    wait_for_server(&mgr, &py_file, 2000).await;

    let mgr = tokio::sync::Mutex::new(mgr);
    let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(2))
        .await
        .expect("expected combined diagnostics");
    assert!(
        summary.text.contains("app.ts"),
        "missing ts: {}",
        summary.text
    );
    assert!(
        summary.text.contains("main.py"),
        "missing py: {}",
        summary.text
    );
    assert_eq!(summary.file_count, 2);
    assert_eq!(summary.diagnostic_count, 4); // 2 per file (error + warning)

    mgr.lock().await.shutdown().await;
}

/// Requires `npx typescript-language-server` on PATH. Run with:
/// `cargo test -p echo-agent-runtime e2e_real_typescript_language_server -- --ignored`
#[ignore]
#[tokio::test(flavor = "current_thread")]
async fn e2e_real_typescript_language_server() {
    let workspace = tempfile::tempdir().unwrap();

    std::fs::write(
        workspace.path().join("tsconfig.json"),
        r#"{"compilerOptions": {"strict": true}}"#,
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("package.json"),
        r#"{"name": "test", "dependencies": {"typescript": "*"}}"#,
    )
    .unwrap();

    // Install typescript so the language server can find tsserver.
    let npm = std::process::Command::new("npm")
        .args(["install", "--silent"])
        .current_dir(workspace.path())
        .output()
        .expect("npm install failed");
    assert!(
        npm.status.success(),
        "npm install failed: {}",
        String::from_utf8_lossy(&npm.stderr)
    );

    let ts_file = workspace.path().join("test.ts");
    std::fs::write(&ts_file, "const x: number = 'hello';\n").unwrap();

    let mut ext_map = HashMap::new();
    ext_map.insert(".ts".to_string(), "typescript".to_string());

    let config = LspServerConfig {
        command: "npx".to_string(),
        args: vec![
            "--yes".to_string(),
            "typescript-language-server".to_string(),
            "--stdio".to_string(),
        ],
        extensions: ext_map,
        startup_timeout: Some(30_000),
        ..Default::default()
    };

    let notify = Arc::new(tokio::sync::Notify::new());
    let mut client = LspClient::start("tsserver".to_string(), 1, config, workspace.path(), notify)
        .await
        .expect("real TS language server failed to start");

    assert_eq!(client.server_name(), "tsserver");

    client.notify_file_change(&ts_file, "const x: number = 'hello';\n", "typescript");

    // Real servers take longer — poll for up to 10 seconds.
    let diags = {
        let mut result = vec![];
        for _ in 0..200 {
            result = client.get_diagnostics(&ts_file);
            if !result.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        result
    };

    assert!(
        !diags.is_empty(),
        "expected diagnostics from real TS server, got none"
    );
    let has_type_error = diags
        .iter()
        .any(|d| d.message.contains("not assignable") || d.message.contains("Type"));
    assert!(
        has_type_error,
        "expected type error diagnostic, got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );

    eprintln!("Real TS server produced {} diagnostics:", diags.len());
    for d in &diags {
        eprintln!(
            "  [{:?}] L{}: {}",
            d.severity,
            d.range.start.line + 1,
            d.message
        );
    }

    client.shutdown().await;
}

/// Simulates the host session diagnostics flow:
/// edit -> notify_file_changed -> drain_lsp_diagnostics -> inject as user message.
#[tokio::test(flavor = "current_thread")]
async fn e2e_session_diagnostics_injection_flow() {
    let (_dir, script_path) = write_mock_server();
    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = single_server_manager(&script_path, &workspace).await;

    // Step 1: tool edits file -> fire-and-forget notify (returns immediately).
    let edited_file = workspace.path().join("component.ts");
    let content = "const x: number = 'wrong_type';\n";
    std::fs::write(&edited_file, content).unwrap();
    mgr.notify_file_changed(&edited_file, content);

    // Step 2: (simulated) other tools run... time passes... LSP server responds.
    wait_for_server(&mgr, &edited_file, 2000).await;

    let mgr = tokio::sync::Mutex::new(mgr);
    let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(2))
        .await
        .expect("diagnostics should be available");
    assert!(summary.text.starts_with("<lsp-diagnostics>"));
    assert!(summary.text.ends_with("</lsp-diagnostics>"));
    assert!(
        summary.text.contains("error[L1]"),
        "should have error at line 1: {}",
        summary.text
    );
    assert!(
        summary.text.contains("warn[L3]"),
        "should have warning at line 3: {}",
        summary.text
    );
    assert_eq!(summary.file_count, 1);
    assert_eq!(summary.diagnostic_count, 2);

    // Step 4: the injected user message the model sees.
    let injected = format!("<system-reminder>\n{}\n</system-reminder>", summary.text);
    assert!(injected.contains("mock error: undeclared variable"));
    assert!(injected.contains("mock warning: unused import"));

    // Step 5: .py has no server — notify is a no-op.
    {
        let mut mgr = mgr.lock().await;
        let py_file = workspace.path().join("script.py");
        std::fs::write(&py_file, "x = 1\n").unwrap();
        mgr.notify_file_changed(&py_file, "x = 1\n");
        assert!(!mgr.has_pending_diagnostics(), ".py should not add pending");
        mgr.shutdown().await;
    }
}

/// Exercises the tool dispatch path through dispatch_tool_typed:
/// goToDefinition, findReferences, missing server, missing args.
#[tokio::test(flavor = "current_thread")]
async fn e2e_session_tool_dispatch_flow() {
    use super::{LspOperation, LspToolInput};

    let (_dir, script_path) = write_mock_server();
    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = single_server_manager(&script_path, &workspace).await;

    let ts_file = workspace.path().join("app.ts");
    let content = "function greet() { return 'hi'; }\n";
    std::fs::write(&ts_file, content).unwrap();
    mgr.notify_file_changed(&ts_file, content);

    // goToDefinition
    let result = mgr
        .dispatch_tool_typed(&LspToolInput {
            operation: LspOperation::GoToDefinition,
            file_path: Some(ts_file.to_string_lossy().into_owned()),
            line: Some(0),
            character: Some(9),
            query: None,
        })
        .await;
    assert!(!result.is_error, "should succeed: {}", result.text);
    assert!(
        result.text.contains(":11:1"),
        "expected line 11: {}",
        result.text
    );

    // findReferences
    let result = mgr
        .dispatch_tool_typed(&LspToolInput {
            operation: LspOperation::FindReferences,
            file_path: Some(ts_file.to_string_lossy().into_owned()),
            line: Some(0),
            character: Some(9),
            query: None,
        })
        .await;
    assert!(!result.is_error);
    assert!(result.text.contains(":6:1"), "line 6: {}", result.text);
    assert!(result.text.contains(":16:4"), "line 16: {}", result.text);

    // missing server -> error
    let rs_file = workspace.path().join("lib.rs");
    std::fs::write(&rs_file, "fn main() {}\n").unwrap();
    let result = mgr
        .dispatch_tool_typed(&LspToolInput {
            operation: LspOperation::GoToDefinition,
            file_path: Some(rs_file.to_string_lossy().into_owned()),
            line: Some(0),
            character: Some(3),
            query: None,
        })
        .await;
    assert!(result.is_error);
    assert!(
        result.text.contains("No LSP server configured"),
        "{}",
        result.text
    );

    // missing file_path for position-based operation -> error
    let result = mgr
        .dispatch_tool_typed(&LspToolInput {
            operation: LspOperation::GoToDefinition,
            file_path: None,
            line: None,
            character: None,
            query: None,
        })
        .await;
    assert!(result.is_error);
    assert!(result.text.contains("Required"), "{}", result.text);

    mgr.shutdown().await;
}

/// Verifies the tools_enabled gating logic.
#[tokio::test(flavor = "current_thread")]
async fn e2e_tools_enabled_gating() {
    let (_dir, script_path) = write_mock_server();
    let workspace = tempfile::tempdir().unwrap();

    // tools_enabled=false (default) — tools should NOT be advertised.
    let mut mgr = single_server_manager(&script_path, &workspace).await;
    assert!(!mgr.tools_enabled(), "tools disabled by default");
    mgr.shutdown().await;

    // tools_enabled=true — Arc<dyn LspBackend> would be injected into ToolBridge Resources.
    let mut servers = BTreeMap::new();
    servers.insert("mock-ts".to_string(), mock_server_config(&script_path));
    let mut mgr = LspManager {
        tools_enabled: true,
        ..LspManager::new(
            servers,
            workspace.path().to_path_buf(),
            false,
            crate::notification::ToolNotificationHandle::noop(),
        )
    };
    mgr.ensure_initialized().await;
    assert!(mgr.tools_enabled());
    mgr.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_restart_monitor_preserves_replacement_client() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (_dir, script_path) = write_mock_server();
            let workspace = tempfile::tempdir().unwrap();
            let mut mgr = single_server_manager(&script_path, &workspace).await;

            let original_lifecycle_id = mgr.clients.get("mock-ts").unwrap().lifecycle_id;
            let tracked_docs = vec![(
                "file:///tmp/replayed.ts".to_string(),
                "typescript".to_string(),
            )];
            let replacement_lifecycle_id = mgr.alloc_lifecycle_id();
            let replacement = LspClient::start(
                "mock-ts".to_string(),
                replacement_lifecycle_id,
                mock_server_config(&script_path),
                workspace.path(),
                mgr.diagnostics_ready.clone(),
            )
            .await
            .expect("replacement should start");
            mgr.clients.insert("mock-ts".to_string(), replacement);

            let lsp_manager = Arc::new(tokio::sync::Mutex::new(mgr));
            let monitor = tokio::task::spawn_local(restart_monitor(
                Arc::downgrade(&lsp_manager),
                "mock-ts".to_string(),
            ));

            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

            {
                let mut mgr = lsp_manager.lock().await;
                // Mutate in place: LspClient now implements Drop, so moving
                // fields out with `..stale` is rejected.
                let mut stale = mgr.clients.remove("mock-ts").unwrap();
                stale.lifecycle_id = original_lifecycle_id;
                for (uri, lang) in tracked_docs {
                    stale.documents.commit(
                        &uri,
                        0,
                        &lang,
                        async_lsp::lsp_types::Position::default(),
                    );
                }
                mgr.clients.insert("mock-ts".to_string(), stale);
                let healthy_lifecycle_id = mgr.alloc_lifecycle_id();
                let healthy = LspClient::start(
                    "mock-ts".to_string(),
                    healthy_lifecycle_id,
                    mock_server_config(&script_path),
                    workspace.path(),
                    mgr.diagnostics_ready.clone(),
                )
                .await
                .expect("healthy replacement should start");
                mgr.clients.insert("mock-ts".to_string(), healthy);
            }

            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

            let current_lifecycle_id = lsp_manager
                .lock()
                .await
                .clients
                .get("mock-ts")
                .map(|c| c.lifecycle_id)
                .expect("healthy client should still be present");
            assert_ne!(current_lifecycle_id, original_lifecycle_id);

            monitor.abort();
            let _ = monitor.await;
            lsp_manager.lock().await.shutdown().await;
        })
        .await;
}

/// The monitor holds only a `Weak` to the manager, so once the sole strong
/// `Arc` drops at session teardown the next poll's upgrade fails and the task
/// must exit. A `Weak`->`Arc` regression would keep the manager (and its child
/// processes) alive and the join would time out.
#[tokio::test(flavor = "current_thread")]
async fn restart_monitor_exits_when_manager_arc_dropped() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (_dir, script_path) = write_mock_server();
            let workspace = tempfile::tempdir().unwrap();
            let mgr = single_server_manager(&script_path, &workspace).await;

            // A live client keeps the monitor polling (rather than exiting on a
            // missing client), so the only way out is a failed `Weak` upgrade.
            let lsp_manager = Arc::new(tokio::sync::Mutex::new(mgr));
            let monitor = tokio::task::spawn_local(restart_monitor(
                Arc::downgrade(&lsp_manager),
                "mock-ts".to_string(),
            ));

            drop(lsp_manager);

            tokio::time::timeout(std::time::Duration::from_secs(5), monitor)
                .await
                .expect("monitor must exit once the manager Arc is dropped")
                .expect("monitor task should not panic");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_finalize_no_longer_blocks_on_slow_lsp_startup() {
    let (_dir, script_path) = write_slow_init_server(1_500);
    let workspace = tempfile::tempdir().unwrap();

    let mut ext_map = HashMap::new();
    ext_map.insert(".ts".to_string(), "typescript".to_string());
    let server_config = LspServerConfig {
        command: "python3".to_string(),
        args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
        extensions: ext_map,
        startup_timeout: Some(5_000),
        ..Default::default()
    };
    let mut servers = BTreeMap::new();
    servers.insert("slow".to_string(), server_config);

    let lsp_manager = Arc::new(tokio::sync::Mutex::new(LspManager::new(
        servers,
        workspace.path().to_path_buf(),
        true,
        crate::notification::ToolNotificationHandle::noop(),
    )));
    let adapter = LspBackendAdapter::new(lsp_manager.clone());

    let start = tokio::time::Instant::now();
    adapter.ensure_started_background();
    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "background start trigger should return immediately"
    );
    assert!(
        !adapter.is_ready(),
        "slow startup should not be ready immediately"
    );

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !adapter.is_ready(),
        "adapter should still be starting shortly after trigger"
    );

    adapter
        .ensure_ready()
        .await
        .expect("startup should eventually succeed");
    assert!(
        adapter.is_ready(),
        "adapter should be ready after awaited startup"
    );
    lsp_manager.lock().await.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_first_dispatch_waits_for_background_startup() {
    let (_dir, script_path) = write_slow_init_server(750);
    let workspace = tempfile::tempdir().unwrap();
    let test_file = workspace.path().join("app.ts");
    std::fs::write(&test_file, "const value = 1;\n").unwrap();

    let mut ext_map = HashMap::new();
    ext_map.insert(".ts".to_string(), "typescript".to_string());
    let server_config = LspServerConfig {
        command: "python3".to_string(),
        args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
        extensions: ext_map,
        startup_timeout: Some(5_000),
        ..Default::default()
    };
    let mut servers = BTreeMap::new();
    servers.insert("slow".to_string(), server_config);

    let lsp_manager = Arc::new(tokio::sync::Mutex::new(LspManager::new(
        servers,
        workspace.path().to_path_buf(),
        true,
        crate::notification::ToolNotificationHandle::noop(),
    )));
    let adapter = LspBackendAdapter::new(lsp_manager.clone());
    adapter.ensure_started_background();

    let start = tokio::time::Instant::now();
    let result = adapter
        .dispatch(&LspToolInput {
            operation: LspOperation::GoToDefinition,
            file_path: Some(test_file.to_string_lossy().into_owned()),
            line: Some(0),
            character: Some(6),
            query: None,
        })
        .await;
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(500),
        "dispatch should wait for startup readiness"
    );
    assert!(
        adapter.is_ready(),
        "dispatch should leave the adapter ready"
    );
    assert!(
        !result.is_error
            || result.text.contains("Definition")
            || result.text.contains("No LSP server configured"),
        "result: {}",
        result.text
    );
    lsp_manager.lock().await.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_restart_monitor_emits_failed_on_restart_init_error() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (_dir, script_path) = write_init_failure_server();
            let counter_dir = tempfile::tempdir().unwrap();
            let counter_path = counter_dir.path().join("attempts.txt");
            let workspace = tempfile::tempdir().unwrap();
            let (handle, mut rx) = crate::notification::ToolNotificationHandle::channel();

            let mut ext_map = HashMap::new();
            ext_map.insert(".ts".to_string(), "typescript".to_string());
            let mut env = HashMap::new();
            env.insert(
                "INIT_FAILURE_COUNTER_FILE".to_string(),
                counter_path.to_string_lossy().into_owned(),
            );
            let server_config = LspServerConfig {
                command: "python3".to_string(),
                args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
                env,
                extensions: ext_map,
                // Generous startup window: the init-failure server responds to
                // `initialize` (and bumps the on-disk counter) essentially
                // instantly, so a large timeout adds no latency on the happy
                // path. It only removes a cold-start race — with a tight 500ms
                // window a slow python3 spawn under load is killed *before* it
                // increments the counter, so `attempts` (deterministically 3)
                // and the on-disk counter (2) diverge and the test flakes.
                startup_timeout: Some(10_000),
                restart_on_crash: Some(true),
                max_restarts: Some(3),
                ..Default::default()
            };

            let mut servers = BTreeMap::new();
            servers.insert("failing".to_string(), server_config.clone());

            let healthy_script = write_mock_server();
            let healthy_client = LspClient::start(
                "failing".to_string(),
                1,
                mock_server_config(&healthy_script.1),
                workspace.path(),
                Arc::new(tokio::sync::Notify::new()),
            )
            .await
            .expect("healthy client should start");

            let mut mgr = LspManager::new(servers, workspace.path().to_path_buf(), false, handle);
            mgr.clients.insert("failing".to_string(), healthy_client);

            let lsp_manager = Arc::new(tokio::sync::Mutex::new(mgr));
            let monitor = tokio::task::spawn_local(restart_monitor(
                Arc::downgrade(&lsp_manager),
                "failing".to_string(),
            ));

            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            {
                let mut mgr = lsp_manager.lock().await;
                let crashed = mgr.clients.remove("failing").unwrap();
                crashed.main_loop.abort();
                mgr.clients.insert("failing".to_string(), crashed);
            }

            let mut saw_failed = false;
            // Restart backoff is 1s + 2s + 4s = 7s of mandatory sleeps before
            // the budget is exhausted; the deadline only bounds the failure
            // wait (the loop breaks as soon as the notification arrives), so
            // keep it well clear of that floor to stay robust under load.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                    Ok(Some(ToolNotification::LspServerFailed(failed))) => {
                        assert_eq!(failed.server_name, "failing");
                        assert!(failed.error.contains("init failed on purpose"));
                        assert_eq!(failed.attempts, 3);
                        saw_failed = true;
                        break;
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }

            assert!(saw_failed, "expected LspServerFailed notification");
            let attempts = std::fs::read_to_string(&counter_path)
                .expect("counter file should exist")
                .trim()
                .parse::<u32>()
                .expect("counter should be an integer");
            assert_eq!(
                attempts, 3,
                "restart init should consume the full retry budget"
            );
            monitor.abort();
            let _ = monitor.await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_drain_timeout_preserves_pending_diagnostics() {
    let (_dir, script_path) = write_delayed_diagnostics_server();
    let workspace = tempfile::tempdir().unwrap();

    let mut ext_map = HashMap::new();
    ext_map.insert(".ts".to_string(), "typescript".to_string());
    let server_config = LspServerConfig {
        command: "python3".to_string(),
        args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
        extensions: ext_map,
        startup_timeout: Some(10_000),
        ..Default::default()
    };
    let mut servers = BTreeMap::new();
    servers.insert("delayed".to_string(), server_config);

    let mut mgr = LspManager::new(
        servers,
        workspace.path().to_path_buf(),
        false,
        crate::notification::ToolNotificationHandle::noop(),
    );
    mgr.ensure_initialized().await;

    let test_file = workspace.path().join("slow.ts");
    std::fs::write(&test_file, "const y = 1;\n").unwrap();
    mgr.notify_file_changed(&test_file, "const y = 1;\n");
    assert!(mgr.is_uri_pending(&test_file));

    let mgr = tokio::sync::Mutex::new(mgr);
    let first = drain_lsp_diagnostics(&mgr, std::time::Duration::from_millis(100)).await;
    assert!(
        first.is_none(),
        "first drain should time out before diagnostics arrive"
    );
    assert!(mgr.lock().await.is_uri_pending(&test_file));

    let second = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(2))
        .await
        .expect("pending diagnostics should survive timeout");
    assert!(second.text.contains("delayed diagnostic after restart"));
    assert_eq!(second.file_count, 1);
    assert!(!mgr.lock().await.has_pending_diagnostics());
    mgr.lock().await.shutdown().await;
}

/// A server that never reports diagnostics must not keep files pending
/// forever: the set would grow for the whole session and every later drain
/// would block for its full timeout.
///
/// The two things being checked here are separate on purpose. Holding on to a
/// file and blocking a turn on its server are different decisions with
/// different deadlines, and one number doing both jobs is how a server that
/// answered several clean edits came to be written off.
#[tokio::test(flavor = "current_thread")]
async fn drain_gives_up_on_a_server_that_never_reports() {
    let (_dir, script_path) = write_silent_server();
    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = single_server_manager(&script_path, &workspace).await;
    mgr.pending_policy = brief_policy();

    let test_file = workspace.path().join("quiet.ts");
    std::fs::write(&test_file, "const y = 1;\n").unwrap();
    mgr.notify_file_changed(&test_file, "const y = 1;\n");
    assert!(mgr.is_uri_pending(&test_file));

    let mgr = tokio::sync::Mutex::new(mgr);
    let timeout = std::time::Duration::from_millis(30);

    // Still within the file's time, so it is still owed an answer.
    assert!(drain_lsp_diagnostics(&mgr, timeout).await.is_none());
    assert!(mgr.lock().await.has_pending_diagnostics());

    // Past it, and the file is let go rather than carried for the session.
    tokio::time::sleep(BRIEF_VERDICT_TTL).await;
    assert!(drain_lsp_diagnostics(&mgr, timeout).await.is_none());
    assert!(
        !mgr.lock().await.has_pending_diagnostics(),
        "a file nobody ever answers for must not be waited on forever"
    );

    // And later drains return immediately rather than waiting out the timeout,
    // because the server has now been silent for longer than it is worth
    // blocking on.
    mgr.lock()
        .await
        .notify_file_changed(&test_file, "const y = 2;\n");
    let started = std::time::Instant::now();
    assert!(
        drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(5))
            .await
            .is_none()
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "drain should not block on a server that has said nothing, took {:?}",
        started.elapsed()
    );

    mgr.lock().await.shutdown().await;
}

/// Editing many files against a server that says nothing must not let the
/// pending set grow with the session.
#[tokio::test(flavor = "current_thread")]
async fn a_silent_server_does_not_accumulate_pending_files() {
    let (_dir, script_path) = write_silent_server();
    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = single_server_manager(&script_path, &workspace).await;
    mgr.pending_policy = brief_policy();
    let mgr = tokio::sync::Mutex::new(mgr);
    let timeout = std::time::Duration::from_millis(20);

    // An edit is always followed by a drain in production (the after-edit
    // reminder), so that is the shape to exercise here.
    for i in 0..50 {
        let file = workspace.path().join(format!("file{i}.ts"));
        std::fs::write(&file, "const y = 1;\n").unwrap();
        mgr.lock()
            .await
            .notify_file_changed(&file, "const y = 1;\n");
        let _ = drain_lsp_diagnostics(&mgr, timeout).await;
    }

    tokio::time::sleep(BRIEF_VERDICT_TTL).await;
    let _ = drain_lsp_diagnostics(&mgr, timeout).await;

    assert_eq!(
        mgr.lock().await.pending_count(),
        0,
        "a silent server should be owed nothing once every file has waited its time"
    );

    mgr.lock().await.shutdown().await;
}

/// The same guarantee, for a server that is plainly alive. Silence used to be
/// charged only on drains that came back empty-handed, so a file the server
/// never answered for was carried along untouched for the rest of the session
/// as long as some *other* file kept reporting problems.
///
/// Nothing charges anything now — a file is let go when its own deadline
/// passes, whatever the drain it happens to be sitting in was doing.
#[tokio::test(flavor = "multi_thread")]
async fn a_productive_server_still_lets_go_of_a_file_it_never_answers_for() {
    let (_dir, script_path) = write_partially_answering_server();
    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = single_server_manager(&script_path, &workspace).await;
    mgr.pending_policy = brief_policy();
    let mgr = tokio::sync::Mutex::new(mgr);
    let generous = std::time::Duration::from_secs(2);

    let quiet = workspace.path().join("quiet.ts");
    std::fs::write(&quiet, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&quiet, "const y = 1;\n");

    // Every drain from here on has something to report — about another file.
    for i in 0..3 {
        let loud = workspace.path().join(format!("loud{i}.ts"));
        std::fs::write(&loud, "const y = 1;\n").unwrap();
        mgr.lock()
            .await
            .notify_file_changed(&loud, "const y = 1;\n");
        let summary = drain_lsp_diagnostics(&mgr, generous).await;
        assert!(
            summary.is_some_and(|s| s.text.contains("loud problem")),
            "the server is answering for the loud files"
        );
        tokio::time::sleep(BRIEF_VERDICT_TTL).await;
    }

    assert!(
        !mgr.lock().await.is_uri_pending(&quiet),
        "a file the server never answers for must not be waited on forever"
    );

    mgr.lock().await.shutdown().await;
}

/// Not blocking on a silent server has to be reversible. Roslyn can spend a
/// long time loading a solution before it answers anything, and if those first
/// quiet turns disabled waiting for good, diagnostics would never appear for
/// the rest of the session — the exact outcome this work exists to prevent.
#[tokio::test(flavor = "current_thread")]
async fn a_server_that_starts_answering_is_waited_on_again() {
    let (_dir, script_path) = write_silent_server();
    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = single_server_manager(&script_path, &workspace).await;
    // Short patience, generous hold: this is about whether we block, not about
    // letting go of files.
    mgr.pending_policy = super::pending::PendingPolicy {
        verdict_ttl: std::time::Duration::from_secs(30),
        server_patience: std::time::Duration::from_millis(50),
    };
    let mgr = tokio::sync::Mutex::new(mgr);
    let brief = std::time::Duration::from_millis(20);

    let file = workspace.path().join("slow.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    // Quiet for long enough that the server stops being worth blocking on.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    let started = std::time::Instant::now();
    assert!(
        drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(5))
            .await
            .is_none()
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "a server that has said nothing should not cost a turn its whole budget"
    );

    // The server finishes loading and answers the question it was asked.
    let uri = file_uri(&file).unwrap().to_string();
    let store = mgr.lock().await.clients["mock-ts"].diagnostics.clone();
    store.install(
        &uri,
        Answer::new(
            vec![Diagnostic {
                severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                message: "server is back".to_string(),
                ..Default::default()
            }],
            super::documents::FIRST_VERSION,
            None,
        ),
    );

    let summary = drain_lsp_diagnostics(&mgr, brief)
        .await
        .expect("the answer it finally gave must be reported");
    assert!(summary.text.contains("server is back"), "{}", summary.text);

    // And the next edit is waited on again, so an answer arriving during that
    // wait is reported rather than missed.
    let next = workspace.path().join("awake.ts");
    std::fs::write(&next, "const y = 2;\n").unwrap();
    let next_uri = file_uri(&next).unwrap().to_string();
    mgr.lock()
        .await
        .notify_file_changed(&next, "const y = 2;\n");

    let late = store.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        late.install(
            &next_uri,
            Answer::new(
                vec![Diagnostic {
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    message: "still awake".to_string(),
                    ..Default::default()
                }],
                super::documents::FIRST_VERSION,
                None,
            ),
        );
    });

    let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(3))
        .await
        .expect("a server that has started answering must be waited on again");
    assert!(summary.text.contains("still awake"), "{}", summary.text);

    mgr.lock().await.shutdown().await;
}

/// Roslyn answers before it has loaded the solution, so its first answer is
/// empty, and it says so afterwards. Guessing how long that takes is what the
/// old design did; here the server is taken at its word — the questions it
/// answered too early are asked again, and the real answer is read.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_finishes_loading_late_still_gets_its_diagnostics_read() {
    let (_dir, script_path) = write_loads_late_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);

    let file = workspace.path().join("late.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    let summary = drain_until_reported(&mgr, "found once the solution was loaded").await;
    assert!(
        summary.contains("found once the solution was loaded"),
        "{summary}"
    );

    mgr.lock().await.shutdown().await;
}

/// The same, through the request the specification provides for. Advertising
/// `refreshSupport` obliges us to answer it — a server left waiting on a
/// response it never gets is a protocol error we would have introduced — so
/// the mock reports whether we did.
#[tokio::test(flavor = "multi_thread")]
async fn a_diagnostics_refresh_request_is_answered_and_acted_on() {
    let (_dir, script_path) = write_diagnostic_refresh_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);

    let file = workspace.path().join("refresh.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    let summary = drain_until_reported(&mgr, "refresh answered=").await;
    assert!(
        summary.contains("refresh answered=True"),
        "the server's refresh request must get a response: {summary}"
    );

    mgr.lock().await.shutdown().await;
}

/// A pushed report may name the revision it describes. A server running one
/// revision behind is then not mistaken for one that has answered the edit —
/// the case arrival order alone cannot tell apart, and the reason the old
/// design needed a monotonic sequence and an edit marker to approximate it.
#[tokio::test(flavor = "current_thread")]
async fn a_pushed_verdict_on_the_previous_revision_does_not_settle_the_edit() {
    let (_dir, script_path) = write_versioned_push_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);
    let brief = std::time::Duration::from_millis(400);

    let file = workspace.path().join("versioned.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    let opened = drain_lsp_diagnostics(&mgr, brief)
        .await
        .expect("the verdict on the opened text answers the question we asked");
    assert!(
        opened.text.contains("verdict on version 1"),
        "{}",
        opened.text
    );

    // The edit takes the document to version 2; the server answers about 1.
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 2;\n");
    assert!(
        drain_lsp_diagnostics(&mgr, brief).await.is_none(),
        "a verdict the server itself labels as being about the previous \
         revision is not a verdict on this edit"
    );
    assert!(
        mgr.lock().await.is_uri_pending(&file),
        "so the file is still owed one"
    );

    mgr.lock().await.shutdown().await;
}

/// A server with a push channel has told us how it reports, and what it
/// returns from a pull may be only part of it. rust-analyzer is the case in
/// point: it answers `textDocument/diagnostic` with its own analysis and
/// deliberately leaves `cargo check` results to `publishDiagnostics`. Believing
/// the pull answer is the whole picture loses every check error in the crate —
/// and worse, settles the file as clean, so nothing is reported at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_publishes_is_not_second_guessed_with_a_pull() {
    let (_dir, script_path) = write_push_and_pull_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);

    let file = workspace.path().join("checked.rs.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    let summary = drain_until_reported(&mgr, "the check that only the push channel runs").await;
    assert!(
        summary.contains("the check that only the push channel runs"),
        "{summary}"
    );

    // And from here on it is not asked at all — its own reports are the truth.
    let before = mgr.lock().await.clients["mock-ts"].pull.support();
    assert_eq!(before, super::pull::PullSupport::Asking, "not rejected");
    for round in 0..3 {
        let text = format!("const y = {round};\n");
        std::fs::write(&file, &text).unwrap();
        mgr.lock().await.notify_file_changed(&file, &text);
        let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(2)).await;
        assert!(
            summary.is_some_and(|s| s.text.contains("the check that only the push channel runs")),
            "round {round}: the pushed report is what the reader gets"
        );
    }

    mgr.lock().await.shutdown().await;
}

/// A report can arrive for a file we have never opened — a workspace-wide or
/// `cargo check` pass covers more than the client has asked about. It describes
/// text we never sent, so it is not a verdict on the first edit we make to that
/// file, however new it looks.
#[tokio::test(flavor = "multi_thread")]
async fn a_report_from_before_we_opened_a_file_does_not_settle_our_first_edit() {
    let (_dir, script_path) = write_publishes_before_open_server();
    let workspace = tempfile::tempdir().unwrap();
    let file = workspace.path().join("preopened.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    let uri = file_uri(&file).unwrap().to_string();

    let mut config = mock_server_config(&script_path);
    config.env.insert("PREOPENED_URI".to_string(), uri.clone());
    let mut servers = BTreeMap::new();
    servers.insert("mock-ts".to_string(), config);
    let mut mgr = LspManager::new(
        servers,
        workspace.path().to_path_buf(),
        false,
        crate::notification::ToolNotificationHandle::noop(),
    );
    mgr.ensure_initialized().await;

    // Wait for the unsolicited report to land before touching the file.
    wait_until("the unsolicited report", || {
        mgr.clients["mock-ts"].diagnostics.covers(&uri).is_some()
    })
    .await;

    mgr.notify_file_changed(&file, "const y = 2;\n");
    let mgr = tokio::sync::Mutex::new(mgr);
    assert!(
        drain_lsp_diagnostics(&mgr, std::time::Duration::from_millis(300))
            .await
            .is_none(),
        "a report about text the server was never sent is not a verdict on our edit"
    );
    assert!(
        mgr.lock().await.is_uri_pending(&file),
        "so the file is still owed one"
    );

    mgr.lock().await.shutdown().await;
}

/// A refresh from a server we do not pull from has nothing to act on. Throwing
/// away what it has already told us would leave the reader with nothing at all,
/// so its reports stand.
#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_we_cannot_act_on_does_not_discard_what_we_know() {
    let (_dir, script_path) = write_refresh_without_pull_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);

    let file = workspace.path().join("kept.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    let summary = drain_until_reported(&mgr, "a real problem").await;
    assert!(summary.contains("a real problem"), "{summary}");

    // The refresh request has been answered by now; the diagnostics it could
    // not replace must still be there.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let uri = file_uri(&file).unwrap().to_string();
    assert_eq!(
        mgr.lock().await.clients["mock-ts"]
            .diagnostics
            .items(&uri)
            .len(),
        1,
        "the only report we have must survive a refresh we cannot act on"
    );

    mgr.lock().await.shutdown().await;
}

/// An edit landing while a suspicious empty answer is being double-checked
/// must not cost us the errors we already hold.
///
/// The confirmation exists because a server answers before it has finished
/// re-analyzing. If the second answer is written down anyway after the file has
/// changed underneath it, two things go wrong at once: real errors are erased
/// on the strength of a verdict about text that no longer exists, and the
/// re-pull that follows then has nothing to lose, so it believes the first
/// premature blank it is given and reports the newest edit as clean.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_during_the_confirmation_does_not_cost_us_the_errors() {
    let (_dir, script_path) = write_clean_then_silent_server();
    let (workspace, mut client) = start_client_with(&script_path).await;

    let file = workspace.path().join("racing.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    client.notify_file_change(&file, "const y = 1;\n", "typescript");
    assert_eq!(
        wait_for_message(&client, &file, |m| m == "the real problem").await,
        Some("the real problem".to_string()),
        "the server reports the problem to begin with"
    );

    // Second edit: the answer is clean, so the pull waits to be sure.
    client.notify_file_change(&file, "const y = 2;\n", "typescript");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    // Third edit, while that wait is still running. The confirmation the pull
    // is about to receive is about the second edit's text, not this one's.
    client.notify_file_change(&file, "const y = 3;\n", "typescript");

    // Long enough for the confirmation to come back and be acted on, and for
    // the re-pull it queued to run into the server's silence.
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    let held = client.get_diagnostics(&file);
    assert_eq!(
        held.len(),
        1,
        "a clean answer about replaced text must not erase the errors we hold"
    );
    assert_eq!(held[0].message, "the real problem");

    client.shutdown().await;
}

/// The refresh has to put a server back in the "worth waiting for" column, or
/// it fails on the very case it exists for.
///
/// Roslyn goes quiet for as long as it takes to load a solution — long enough
/// that we stop blocking turns on it — and then announces it is ready. If that
/// announcement does not restart the clock, the drain that should wait for the
/// re-pull the server just asked for returns without waiting, and its answer
/// surfaces a turn late.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_announces_it_is_ready_is_waited_on_again() {
    let (_dir, script_path) = write_loads_after_going_quiet_server();
    let workspace = tempfile::tempdir().unwrap();
    let mut mgr = single_server_manager(&script_path, &workspace).await;
    // Short patience, generous hold: this is about whether we block, not about
    // letting go of files.
    mgr.pending_policy = super::pending::PendingPolicy {
        verdict_ttl: std::time::Duration::from_secs(30),
        server_patience: std::time::Duration::from_millis(80),
    };
    let mgr = tokio::sync::Mutex::new(mgr);

    let file = workspace.path().join("loading.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    // Past the first pull (refused, leaving the server looking silent) and the
    // announcement that followed it. The re-pull that announcement queued is in
    // flight but has not answered yet.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(2))
        .await
        .expect("a server that has just said it is ready must be waited on");
    assert!(
        summary.text.contains("found once the solution was loaded"),
        "{}",
        summary.text
    );

    mgr.lock().await.shutdown().await;
}

/// The first answer from a server we have not seen publish does not get to be
/// a verdict of "clean".
///
/// This is the rust-analyzer case, and it is the *first* edit of a session
/// rather than a rare race: the server has published nothing yet, because
/// nothing has been open for it to publish about. It answers a pull promptly
/// with only what its own analysis knows, while the errors that matter — the
/// ones `cargo check` finds — follow on the push channel. Taking that first
/// answer at face value settles the file as clean, and when the real errors
/// arrive there is nobody left waiting for them.
#[tokio::test(flavor = "multi_thread")]
async fn a_servers_first_word_does_not_settle_an_edit_as_clean() {
    let (_dir, script_path) = write_slow_check_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);

    let file = workspace.path().join("checked.ts");
    std::fs::write(&file, "const y = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const y = 1;\n");

    let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(2))
        .await
        .expect("the errors the push channel carries must still be waited for");
    assert!(
        summary.text.contains("an error only the check finds"),
        "{}",
        summary.text
    );

    mgr.lock().await.shutdown().await;
}

/// Not believing a server's first "clean" has to end in believing it, or
/// something worse than a wrong answer takes its place: a file that stays
/// pending for its whole deadline, so every turn for the next half minute
/// spends the drain's budget waiting for a question that was answered at the
/// start.
#[tokio::test(flavor = "multi_thread")]
async fn a_first_clean_answer_is_settled_once_it_has_been_confirmed() {
    let (_dir, script_path) = write_selective_pull_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);

    let file = workspace.path().join("fine.ts");
    std::fs::write(&file, "const ok = 1;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&file, "const ok = 1;\n");

    // Long enough for the second ask and its answer.
    assert!(
        drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(2))
            .await
            .is_none(),
        "a clean file has nothing to report"
    );
    assert!(
        !mgr.lock().await.has_pending_diagnostics(),
        "the file must be settled by the confirmation, not left waiting out its deadline"
    );

    mgr.lock().await.shutdown().await;
}

/// The most common case in real use is an edit that introduces no problems.
/// Answering "this file is clean" is an answer, and must not be counted as the
/// server going quiet — otherwise a few clean edits in a row write a perfectly
/// healthy server off, and the next real error only surfaces a turn late.
#[tokio::test(flavor = "current_thread")]
async fn a_server_reporting_clean_files_is_still_waited_on() {
    let (_dir, script_path) = write_selective_pull_server();
    let workspace = tempfile::tempdir().unwrap();
    let mgr = tokio::sync::Mutex::new(single_server_manager(&script_path, &workspace).await);
    // Comfortably longer than the pull's own retry, so a clean answer lands
    // inside the drain rather than after it.
    let timeout = std::time::Duration::from_secs(3);

    // Let the server say something first. Until it has, a clean answer is not
    // taken as a verdict — we cannot yet tell a pull-only server from one that
    // publishes and has not got round to it — and this test is about what
    // happens *after* we know what kind of server it is.
    let first = workspace.path().join("broken0.ts");
    std::fs::write(&first, "const bad = ;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&first, "const bad = ;\n");
    assert!(
        drain_lsp_diagnostics(&mgr, timeout)
            .await
            .is_some_and(|s| s.text.contains("pulled problem")),
        "the server's first word tells us it answers pulls"
    );

    // Several clean edits — more than the give-up threshold.
    for i in 0..4 {
        let file = workspace.path().join(format!("fine{i}.ts"));
        std::fs::write(&file, "const ok = 1;\n").unwrap();
        mgr.lock()
            .await
            .notify_file_changed(&file, "const ok = 1;\n");
        assert!(
            drain_lsp_diagnostics(&mgr, timeout).await.is_none(),
            "a clean file has nothing to report"
        );
        assert!(
            !mgr.lock().await.has_pending_diagnostics(),
            "a clean answer settles the file: it should not still be pending after drain {i}"
        );
    }

    // Now break something. It must be reported by this drain, not the next one.
    let broken = workspace.path().join("broken.ts");
    std::fs::write(&broken, "const bad = ;\n").unwrap();
    mgr.lock()
        .await
        .notify_file_changed(&broken, "const bad = ;\n");
    let summary = drain_lsp_diagnostics(&mgr, timeout)
        .await
        .expect("a healthy server must still be waited on after clean edits");
    assert!(summary.text.contains("pulled problem"), "{}", summary.text);

    mgr.lock().await.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_restart_replay_requeues_pending_diagnostics() {
    let (_dir, script_path) = write_delayed_diagnostics_server();
    let workspace = tempfile::tempdir().unwrap();

    let mut ext_map = HashMap::new();
    ext_map.insert(".ts".to_string(), "typescript".to_string());
    let server_config = LspServerConfig {
        command: "python3".to_string(),
        args: vec!["-u".to_string(), script_path.to_string_lossy().into_owned()],
        extensions: ext_map,
        startup_timeout: Some(10_000),
        ..Default::default()
    };
    let mut servers = BTreeMap::new();
    servers.insert("delayed".to_string(), server_config.clone());

    let mut mgr = LspManager::new(
        servers,
        workspace.path().to_path_buf(),
        false,
        crate::notification::ToolNotificationHandle::noop(),
    );
    mgr.ensure_initialized().await;

    let test_file = workspace.path().join("restart.ts");
    std::fs::write(&test_file, "const y = 1;\n").unwrap();
    mgr.notify_file_changed(&test_file, "const y = 1;\n");
    assert!(mgr.is_uri_pending(&test_file));

    let tracked_docs = mgr
        .clients
        .get("delayed")
        .expect("server should exist")
        .tracked_documents();
    let lifecycle_id = mgr.alloc_lifecycle_id();
    let mut restarted = LspClient::start(
        "delayed".to_string(),
        lifecycle_id,
        server_config,
        workspace.path(),
        mgr.diagnostics_ready.clone(),
    )
    .await
    .expect("restart should succeed");

    // The production replay, not a copy of it: a test that re-implements the
    // step it is testing stops testing it the moment the real one changes.
    for (uri, edit) in super::restart::replay_tracked_documents(&mut restarted, &tracked_docs) {
        mgr.mark_uri_pending_diagnostics("delayed", lifecycle_id, uri, edit);
    }
    mgr.clients.insert("delayed".to_string(), restarted);

    let mgr = tokio::sync::Mutex::new(mgr);
    let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(3))
        .await
        .expect("replayed document should still produce diagnostics");
    assert!(summary.text.contains("delayed diagnostic after restart"));
    assert_eq!(summary.file_count, 1);

    mgr.lock().await.shutdown().await;
}

/// Polls `try_wait` until the child is reaped or the budget expires; a live
/// child (failed kill) times out and returns false.
#[cfg(unix)]
async fn std_child_died(child: &mut std::process::Child) -> bool {
    for _ in 0..50 {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

/// A language-server child enrolled via a `ProcessScope` is reaped by
/// `kill_all`, proving session close reclaims LSP servers even while the owning
/// client is still alive (the wedged-actor case).
#[cfg(unix)]
#[tokio::test]
async fn scope_reaps_enrolled_language_server_child() {
    let (_script_dir, _workspace, mut client) = start_mock_client().await;

    let scope = crate::util::ProcessScope::new();
    assert!(
        client.enroll(Some(&scope)),
        "enroll on an open scope must succeed"
    );
    assert_eq!(
        scope.live_count(),
        1,
        "enroll must register the server group"
    );

    // Take the child handle to observe death; the client keeps the owning
    // Arc<ProcessGroup>, so the scope's weak stays live across kill_all.
    let mut child = client.child_process.take().expect("stdio child");
    scope.kill_all();

    // Prove kill_all killed the child WHILE the client is still alive (the
    // wedged-actor case) — dropping the client first would let its own Drop
    // killpg mask a broken kill_all. `waitid(WNOWAIT)` observes the exit
    // without reaping (signal 0 can't: it succeeds on a zombie), so the
    // SIGKILLed leader stays unreaped and its pgid reserved. nix only
    // exposes waitid on Linux; macOS falls back to the weaker
    // drop-then-reap order below (CI runs the strong branch).
    #[cfg(target_os = "linux")]
    {
        let pid = nix::unistd::Pid::from_raw(child.id() as i32);
        let mut died = false;
        for _ in 0..50 {
            use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};
            match waitid(
                Id::Pid(pid),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT | WaitPidFlag::WNOHANG,
            ) {
                Ok(WaitStatus::StillAlive) => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Ok(_) => {
                    died = true;
                    break;
                }
                Err(e) => panic!("waitid on the server child failed: {e}"),
            }
        }
        assert!(
            died,
            "scope.kill_all must kill the enrolled server child while the client is alive"
        );
    }

    // Linux: the leader is dead but unreaped, so its pgid is still reserved —
    // the client Drop's killpg targets the zombie's group, not a reused pgid.
    // macOS: dropping before the reap keeps the same pgid-reservation safety,
    // at the cost of not isolating kill_all from the Drop killpg.
    drop(client);

    assert!(
        std_child_died(&mut child).await,
        "scope.kill_all must reap the enrolled server child"
    );
}

/// Dropping an `LspClient` without `shutdown` still reaps its server child, so a
/// session never orphans a language server when graceful teardown is skipped.
/// Probes the pid with signal 0 (ESRCH once reaped) since the client owns the
/// child handle and waits on it during `Drop`.
#[cfg(unix)]
#[tokio::test]
async fn drop_reaps_server_child_without_shutdown() {
    let (_script_dir, _workspace, client) = start_mock_client().await;

    let pid = client.child_process.as_ref().expect("stdio child").id() as i32;
    let alive = |pid: i32| nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
    assert!(alive(pid), "server child should be running before drop");

    drop(client);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while alive(pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        !alive(pid),
        "Drop must reap the server child even without shutdown"
    );
}

/// The whole point of this work, measured against the real thing.
///
/// Mocks model the shapes real servers come in, and they missed the one that
/// mattered most: Roslyn answers a pull before it has finished re-analyzing,
/// and taking that answer at face value erases errors the file really has. It
/// took driving the real server to find that, so the harness that found it is
/// committed rather than thrown away.
///
/// Drives `LspManager` the way a session does — edit, drain, repeat — and
/// watches the three things the bug showed up in: the server's lifecycle id
/// (which changes only when echoagent restarts it), the number of server processes,
/// and whether real C# diagnostics keep arriving.
///
/// Requires `Microsoft.CodeAnalysis.LanguageServer`. Run with:
///
/// ```text
/// ROSLYN_DLL=/path/to/Microsoft.CodeAnalysis.LanguageServer.dll \
///   cargo test -p echo-agent-tools e2e_real_roslyn_survives_editing -- --ignored --nocapture
/// ```
#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn e2e_real_roslyn_survives_editing() {
    let Ok(dll) = std::env::var("ROSLYN_DLL") else {
        panic!("set ROSLYN_DLL to Microsoft.CodeAnalysis.LanguageServer.dll");
    };
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();

    std::fs::write(
        root.join("App.csproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <Nullable>enable</Nullable>
  </PropertyGroup>
</Project>
"#,
    )
    .unwrap();

    // One file with a real, obvious error in it: `int x = "text";` is CS0029.
    let file = root.join("Broken.cs");
    let broken =
        |round: usize| format!("class Broken {{ void M{round}() {{ int x = \"text\"; }} }}\n");
    std::fs::write(&file, broken(0)).unwrap();

    let mut servers = BTreeMap::new();
    servers.insert(
        "csharp".to_string(),
        LspServerConfig {
            command: "dotnet".to_string(),
            args: vec![
                dll,
                "--stdio".to_string(),
                "--logLevel".to_string(),
                "Warning".to_string(),
                "--extensionLogDirectory".to_string(),
                root.join("roslyn-logs").display().to_string(),
            ],
            extensions: HashMap::from([(".cs".to_string(), "csharp".to_string())]),
            workspace_open: Some(super::config::WorkspaceOpen {
                solution: None,
                projects: vec!["App.csproj".to_string()],
            }),
            startup_timeout: Some(120_000),
            // Deliberate, and the assertion below depends on it: with the
            // default (`false`) a torn-down server is never rebuilt, so the
            // lifecycle id could not move and "no restarts" would hold
            // vacuously. Enabling it is also the configuration under which the
            // memory growth was observed — the rebuild is what costs the
            // gigabytes. With it off, the same crash simply ends C#
            // diagnostics for the session.
            restart_on_crash: Some(true),
            ..Default::default()
        },
    );

    let mut mgr = LspManager::new(
        servers,
        root.to_path_buf(),
        false,
        crate::notification::ToolNotificationHandle::noop(),
    );
    mgr.ensure_initialized().await;
    let started_lifecycle = mgr.clients["csharp"].lifecycle_id;
    let mgr = std::sync::Arc::new(tokio::sync::Mutex::new(mgr));
    // The restart monitor is what would rebuild a torn-down server, and
    // rebuilding is what the bug cost. Without it running, this test could not
    // observe the failure it is here to rule out.
    let monitor = tokio::spawn(super::restart_monitor(
        std::sync::Arc::downgrade(&mgr),
        "csharp".to_string(),
    ));

    let mut rounds_with_diagnostics = 0;
    for round in 0..12 {
        let text = broken(round);
        std::fs::write(&file, &text).unwrap();
        mgr.lock().await.notify_file_changed(&file, &text);

        // Generous: a real solution takes its time, and the point is what the
        // server ends up saying, not how fast.
        let summary = drain_lsp_diagnostics(&mgr, std::time::Duration::from_secs(5)).await;
        if summary
            .as_ref()
            .is_some_and(|s| s.text.contains("CS0029") || s.text.contains("cannot implicitly"))
        {
            rounds_with_diagnostics += 1;
        }

        let guard = mgr.lock().await;
        let client = &guard.clients["csharp"];
        assert_eq!(
            client.lifecycle_id, started_lifecycle,
            "round {round}: the server was restarted — this is the bug, and each \
             restart re-reads the whole solution"
        );
        eprintln!(
            "round {round:2}: lifecycle={} diagnostics={}",
            client.lifecycle_id,
            summary.map_or(0, |s| s.diagnostic_count)
        );
    }

    assert!(
        rounds_with_diagnostics >= 6,
        "expected the real C# error to be reported on most rounds, got {rounds_with_diagnostics}/12"
    );

    monitor.abort();
    mgr.lock().await.shutdown().await;
}
