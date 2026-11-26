//! Mock language servers used by the LSP tests.
//!
//! Each is a small Python script speaking LSP over stdio, written to a temp dir
//! and spawned like a real server. They exist so the client can be tested
//! against the *shapes* real servers come in — full versus incremental sync,
//! push versus pull diagnostics, save with or without text — without needing
//! any of those servers installed.

use std::path::PathBuf;

const MOCK_LSP_SERVER: &str = r#"
import json, sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        if line.strip() == '':
            break
        if ':' in line:
            key, value = line.split(':', 1)
            headers[key.strip()] = value.strip()
    length = int(headers.get('Content-Length', 0))
    if length == 0:
        return None
    body = sys.stdin.read(length)
    return json.loads(body)

def send_message(msg):
    body = json.dumps(msg)
    header = f"Content-Length: {len(body)}\r\n\r\n"
    sys.stdout.write(header)
    sys.stdout.write(body)
    sys.stdout.flush()

def send_diagnostics(uri):
    send_message({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "diagnostics": [
                {
                    "range": {
                        "start": {"line": 0, "character": 5},
                        "end": {"line": 0, "character": 10}
                    },
                    "severity": 1,
                    "source": "mock",
                    "message": "mock error: undeclared variable"
                },
                {
                    "range": {
                        "start": {"line": 2, "character": 0},
                        "end": {"line": 2, "character": 15}
                    },
                    "severity": 2,
                    "source": "mock",
                    "message": "mock warning: unused import"
                }
            ]
        }
    })

while True:
    msg = read_message()
    if msg is None:
        break

    method = msg.get("method")
    msg_id = msg.get("id")

    if method == "initialize":
        send_message({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "capabilities": {
                    "textDocumentSync": 1,
                    "definitionProvider": True,
                    "referencesProvider": True
                }
            }
        })
    elif method == "initialized":
        pass
    elif method == "textDocument/didOpen":
        uri = msg["params"]["textDocument"]["uri"]
        send_diagnostics(uri)
    elif method == "textDocument/didChange":
        uri = msg["params"]["textDocument"]["uri"]
        send_diagnostics(uri)
    elif method == "textDocument/didSave":
        pass
    elif method == "textDocument/definition":
        uri = msg["params"]["textDocument"]["uri"]
        send_message({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{
                "uri": uri,
                "range": {
                    "start": {"line": 10, "character": 0},
                    "end": {"line": 10, "character": 20}
                }
            }]
        })
    elif method == "textDocument/references":
        uri = msg["params"]["textDocument"]["uri"]
        send_message({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [
                {
                    "uri": uri,
                    "range": {
                        "start": {"line": 5, "character": 0},
                        "end": {"line": 5, "character": 10}
                    }
                },
                {
                    "uri": uri,
                    "range": {
                        "start": {"line": 15, "character": 3},
                        "end": {"line": 15, "character": 13}
                    }
                }
            ]
        })
    elif method == "shutdown":
        send_message({"jsonrpc": "2.0", "id": msg_id, "result": None})
    elif method == "exit":
        break
    elif msg_id is not None:
        # Real servers answer requests they do not implement rather than
        # leaving the client hanging.
        send_message({"jsonrpc": "2.0", "id": msg_id,
                      "error": {"code": -32601, "message": "Method not found"}})
"#;

pub(super) fn write_mock_server() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("mock_lsp.py");
    std::fs::write(&script_path, MOCK_LSP_SERVER).unwrap();
    (dir, script_path)
}

pub(super) fn write_delayed_diagnostics_server() -> (tempfile::TempDir, PathBuf) {
    const DELAYED_SERVER: &str = r#"
import json, sys, time

def read_message():
    headers = {}
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        if line.strip() == '':
            break
        if ':' in line:
            key, value = line.split(':', 1)
            headers[key.strip()] = value.strip()
    length = int(headers.get('Content-Length', 0))
    if length == 0:
        return None
    return json.loads(sys.stdin.read(length))

def send_message(msg):
    body = json.dumps(msg)
    sys.stdout.write(f"Content-Length: {len(body)}\r\n\r\n{body}")
    sys.stdout.flush()

while True:
    msg = read_message()
    if msg is None:
        break
    method = msg.get("method")
    msg_id = msg.get("id")
    if method == "initialize":
        send_message({
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {"capabilities": {"textDocumentSync": 1}}
        })
    elif method == "initialized":
        pass
    elif method == "textDocument/didOpen":
        time.sleep(1.0)
        send_message({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": msg["params"]["textDocument"]["uri"],
                "diagnostics": [{
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 5}
                    },
                    "severity": 1,
                    "source": "delayed",
                    "message": "delayed diagnostic after restart"
                }]
            }
        })
    elif method == "shutdown":
        send_message({"jsonrpc": "2.0", "id": msg_id, "result": None})
    elif method == "exit":
        break
"#;
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("delayed_lsp.py");
    std::fs::write(&script_path, DELAYED_SERVER).unwrap();
    (dir, script_path)
}

pub(super) fn write_init_failure_server() -> (tempfile::TempDir, PathBuf) {
    write_init_failure_server_n_times(3)
}

pub(super) fn write_slow_init_server(delay_ms: u64) -> (tempfile::TempDir, PathBuf) {
    let script = format!(
        r#"import json, sys, time

def read_message():
    headers = {{}}
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        if line.strip() == '':
            break
        if ':' in line:
            key, value = line.split(':', 1)
            headers[key.strip()] = value.strip()
    length = int(headers.get('Content-Length', 0))
    if length == 0:
        return None
    return json.loads(sys.stdin.read(length))

def send_message(msg):
    body = json.dumps(msg)
    sys.stdout.write(f"Content-Length: {{len(body)}}\r\n\r\n{{body}}")
    sys.stdout.flush()

while True:
    msg = read_message()
    if msg is None:
        break
    method = msg.get("method")
    msg_id = msg.get("id")
    if method == "initialize":
        time.sleep({delay_ms} / 1000.0)
        send_message({{
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {{"capabilities": {{"textDocumentSync": 1, "definitionProvider": True}}}}
        }})
    elif method == "initialized":
        pass
    elif method == "textDocument/definition":
        uri = msg["params"]["textDocument"]["uri"]
        send_message({{
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": [{{
                "uri": uri,
                "range": {{
                    "start": {{"line": 1, "character": 0}},
                    "end": {{"line": 1, "character": 5}}
                }}
            }}]
        }})
    elif method == "shutdown":
        send_message({{"jsonrpc": "2.0", "id": msg_id, "result": None}})
    elif method == "exit":
        break
"#
    );
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("slow_init_lsp.py");
    std::fs::write(&script_path, script).unwrap();
    (dir, script_path)
}

pub(super) fn write_init_failure_server_n_times(
    failures_before_success: usize,
) -> (tempfile::TempDir, PathBuf) {
    let init_error_payload = format!(
        "{{\"code\": -32603, \"message\": \"init failed on purpose after {} failures\"}}",
        failures_before_success
    );
    let init_error_payload = init_error_payload.replace('"', r#"\""#);
    let script = format!(
        r#"import json, os, sys

FAILURES_BEFORE_SUCCESS = {failures_before_success}
COUNTER_FILE = os.environ["INIT_FAILURE_COUNTER_FILE"]
INIT_ERROR = json.loads("{init_error_payload}")

def read_message():
    headers = {{}}
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        if line.strip() == '':
            break
        if ':' in line:
            key, value = line.split(':', 1)
            headers[key.strip()] = value.strip()
    length = int(headers.get('Content-Length', 0))
    if length == 0:
        return None
    return json.loads(sys.stdin.read(length))

def send_message(msg):
    body = json.dumps(msg)
    sys.stdout.write(f"Content-Length: {{len(body)}}\r\n\r\n{{body}}")
    sys.stdout.flush()

def increment_attempts():
    attempts = 0
    if os.path.exists(COUNTER_FILE):
        with open(COUNTER_FILE, "r", encoding="utf-8") as f:
            content = f.read().strip()
            if content:
                attempts = int(content)
    attempts += 1
    with open(COUNTER_FILE, "w", encoding="utf-8") as f:
        f.write(str(attempts))
    return attempts

while True:
    msg = read_message()
    if msg is None:
        break
    method = msg.get("method")
    msg_id = msg.get("id")
    if method == "initialize":
        attempts = increment_attempts()
        if attempts <= FAILURES_BEFORE_SUCCESS:
            send_message({{"jsonrpc": "2.0", "id": msg_id, "error": INIT_ERROR}})
            break
        send_message({{
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {{"capabilities": {{"textDocumentSync": 1}}}}
        }})
    elif method == "initialized":
        pass
    elif method == "shutdown":
        send_message({{"jsonrpc": "2.0", "id": msg_id, "result": None}})
    elif method == "exit":
        break
"#
    );
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("init_fail_lsp.py");
    std::fs::write(&script_path, script).unwrap();
    (dir, script_path)
}

// ── Roslyn-shaped mock servers ──────────────────────────────────────────
//
// These differ only in how they answer `initialize` and what they do with the
// notifications that follow, so they share one framing preamble rather than
// each carrying its own copy of the JSON-RPC plumbing.

/// `read_message` / `send_message` / `publish` — the same for every mock.
const MOCK_PREAMBLE: &str = r#"
import json, sys

state = {"saves": 0, "pulls": 0}

def read_message():
    headers = {}
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        if line.strip() == '':
            break
        if ':' in line:
            key, value = line.split(':', 1)
            headers[key.strip()] = value.strip()
    length = int(headers.get('Content-Length', 0))
    if length == 0:
        return None
    return json.loads(sys.stdin.read(length))

def send_message(msg):
    body = json.dumps(msg)
    sys.stdout.write(f"Content-Length: {len(body)}\r\n\r\n")
    sys.stdout.write(body)
    sys.stdout.flush()

def one_diagnostic(message):
    return [{
        "range": {"start": {"line": 0, "character": 0},
                  "end": {"line": 0, "character": 1}},
        "severity": 1,
        "source": "mock",
        "message": message
    }]

def publish(uri, message):
    send_message({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": one_diagnostic(message)}
    })

def reply(msg, result):
    send_message({"jsonrpc": "2.0", "id": msg.get("id"), "result": result})

def publish_at(uri, message, version):
    send_message({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": one_diagnostic(message), "version": version}
    })

def notify(method, params=None):
    send_message({"jsonrpc": "2.0", "method": method, "params": params})

def ask(method, params, request_id):
    send_message({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})

def serve(capabilities, handle):
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        if method == "initialize":
            reply(msg, {"capabilities": capabilities})
        elif method == "shutdown":
            reply(msg, None)
        elif method == "exit":
            return
        else:
            handle(msg, method)
"#;

/// Write a mock server whose behaviour is `body`, on top of [`MOCK_PREAMBLE`].
pub(super) fn write_python_server(file_name: &str, body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join(file_name);
    std::fs::write(&script_path, format!("{MOCK_PREAMBLE}\n{body}")).unwrap();
    (dir, script_path)
}

/// A server that declares **incremental** sync (`textDocumentSync: 2`), like
/// Roslyn does. It reports back, as the diagnostic message, whether the
/// `didChange` it received carried a `range`. Roslyn dereferences that range
/// unconditionally and tears its request queue down when it is missing, so a
/// rangeless change against such a server is a client bug.
pub(super) fn write_incremental_sync_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "incremental_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/didOpen":
        publish(msg["params"]["textDocument"]["uri"], "opened")
    elif method == "textDocument/didChange":
        change = msg["params"]["contentChanges"][0]
        uri = msg["params"]["textDocument"]["uri"]
        if change.get("range") is None:
            publish(uri, "changed without range")
        else:
            r = change["range"]
            publish(uri, "changed with range %d:%d-%d:%d" % (
                r["start"]["line"], r["start"]["character"],
                r["end"]["line"], r["end"]["character"]))

serve({"textDocumentSync": 2}, handle)
"#,
    )
}

/// A Roslyn-shaped server: incremental sync, **no** save support, and
/// diagnostics served by pull only — it never publishes. Its diagnostic message
/// reports what the client actually did, so tests can assert on client
/// behaviour rather than on internal state.
pub(super) fn write_pull_diagnostics_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "pull_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/didSave":
        state["saves"] += 1
    elif method == "textDocument/diagnostic":
        state["pulls"] += 1
        previous = msg["params"].get("previousResultId")
        reply(msg, {
            "kind": "full",
            "resultId": "result-%d" % state["pulls"],
            "items": one_diagnostic("pull #%d saves=%d prev=%s" % (
                state["pulls"], state["saves"], previous))
        })

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// A pull server that answers honestly: a document is clean unless its name
/// says "broken". Used to check that "no problems" counts as an answer rather
/// than as silence, and that a real problem after a run of clean files is still
/// reported promptly.
pub(super) fn write_selective_pull_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "selective_pull_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/diagnostic":
        state["pulls"] += 1
        uri = msg["params"]["textDocument"]["uri"]
        items = one_diagnostic("pulled problem") if "broken" in uri else []
        reply(msg, {"kind": "full", "resultId": "r-%d" % state["pulls"], "items": items})

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": False, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// A pull server that answers the second pull with an empty report before
/// going back to reporting the problem — the shape Roslyn has when it is asked
/// again before it has finished re-analyzing an edit.
pub(super) fn write_mid_analysis_pull_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "mid_analysis_pull_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/diagnostic":
        state["pulls"] += 1
        items = [] if state["pulls"] == 2 else one_diagnostic("real problem %d" % state["pulls"])
        reply(msg, {"kind": "full", "resultId": "r-%d" % state["pulls"], "items": items})

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": False, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// A pull server that takes its time, and every answer names the revision it
/// was asked about — so an answer to superseded text is recognisable on sight.
///
/// When the first pull arrives it touches [`FIRST_PULL_MARKER`] beside the
/// document, which is the moment a test has to edit the file again if it wants
/// an answer to land for a revision the server has since been sent a
/// replacement for. The signal deliberately goes through the filesystem rather
/// than a `publishDiagnostics`: a push is itself an answer, and would be the
/// newest one, which is exactly the thing under test.
pub(super) fn write_slow_pull_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "slow_pull_lsp.py",
        r#"
import os, time

state["revisions"] = 0

def handle(msg, method):
    if method in ("textDocument/didOpen", "textDocument/didChange"):
        state["revisions"] += 1
    elif method == "textDocument/diagnostic":
        state["pulls"] += 1
        asked_about = state["revisions"]
        if state["pulls"] == 1:
            path = msg["params"]["textDocument"]["uri"][len("file://"):]
            open(os.path.join(os.path.dirname(path), "first-pull-started"), "w").close()
        time.sleep(0.3)
        reply(msg, {
            "kind": "full",
            "resultId": "r-%d" % state["pulls"],
            "items": one_diagnostic("pull %d answers revision %d" % (
                state["pulls"], asked_about))
        })

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": False, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// The file [`write_slow_pull_server`] touches once its first pull is in flight.
pub(super) const FIRST_PULL_MARKER: &str = "first-pull-started";

/// The file [`write_stale_clean_pull_server`] touches once its second pull is
/// in flight.
pub(super) const SECOND_PULL_MARKER: &str = "second-pull-started";

/// A pull server whose "the file is clean now" answer arrives late, and which
/// then stands by it when asked again with its own result id.
///
/// The first pull reports a problem. The second answers clean, slowly enough
/// that a test can edit the file again first. From the third on, a client that
/// sends back the clean report's id is told "unchanged" — so a client that
/// remembers an id for an answer it never stored will have the server confirm
/// errors the server does not have.
pub(super) fn write_stale_clean_pull_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "stale_clean_pull_lsp.py",
        r#"
import os, time

def handle(msg, method):
    if method == "textDocument/diagnostic":
        state["pulls"] += 1
        previous = msg["params"].get("previousResultId")
        if state["pulls"] == 1:
            reply(msg, {"kind": "full", "resultId": "r1",
                        "items": one_diagnostic("the problem")})
        elif state["pulls"] == 2:
            path = msg["params"]["textDocument"]["uri"][len("file://"):]
            open(os.path.join(os.path.dirname(path), "second-pull-started"), "w").close()
            time.sleep(0.3)
            reply(msg, {"kind": "full", "resultId": "clean", "items": []})
        elif previous == "clean":
            reply(msg, {"kind": "unchanged", "resultId": "clean"})
        else:
            reply(msg, {"kind": "full", "resultId": "clean", "items": []})

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": False, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// A pull server that answers for some documents and simply never replies for
/// others — the shape of a server that is working, and productive, but has
/// nothing to say about one particular file, ever. Documents whose name
/// contains "loud" get an error; the rest get silence.
pub(super) fn write_partially_answering_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "partial_pull_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/diagnostic":
        uri = msg["params"]["textDocument"]["uri"]
        if "loud" not in uri:
            return
        state["pulls"] += 1
        reply(msg, {
            "kind": "full",
            "resultId": "r-%d" % state["pulls"],
            "items": one_diagnostic("loud problem")
        })

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": False, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// A server that accepts everything and never reports a diagnostic.
pub(super) fn write_silent_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "silent_lsp.py",
        r#"
def handle(msg, method):
    pass

serve({"textDocumentSync": 1}, handle)
"#,
    )
}

/// A server that asks for `didSave` **with** the document text, and reports
/// back whether it actually got it.
pub(super) fn write_save_with_text_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "save_text_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/didSave":
        has_text = msg["params"].get("text") is not None
        publish(msg["params"]["textDocument"]["uri"], "saved with text=%s" % has_text)

serve({
    "textDocumentSync": {"openClose": True, "change": 1, "save": {"includeText": True}}
}, handle)
"#,
    )
}

/// A pull server in the shape Roslyn has at session start: it answers before it
/// has loaded the solution, so its first answer is empty, and it says so
/// afterwards with `workspace/projectInitializationComplete`.
pub(super) fn write_loads_late_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "loads_late_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/diagnostic":
        state["pulls"] += 1
        if state["pulls"] == 1:
            # Still loading. Nothing to report — yet.
            reply(msg, {"kind": "full", "resultId": "r-1", "items": []})
            notify("workspace/projectInitializationComplete", None)
        else:
            reply(msg, {
                "kind": "full",
                "resultId": "r-%d" % state["pulls"],
                "items": one_diagnostic("found once the solution was loaded")
            })

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// The same, but announced the way the specification provides for: a
/// `workspace/diagnostic/refresh` request, which the client has to answer.
/// Whether the client answered is reported as the diagnostic message, so a
/// client that advertises `refreshSupport` and then ignores the request fails
/// the test rather than merely logging.
pub(super) fn write_diagnostic_refresh_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "diagnostic_refresh_lsp.py",
        r#"
state["answered_refresh"] = False

def handle(msg, method):
    if method is None and msg.get("id") == 9001:
        state["answered_refresh"] = True
    elif method == "textDocument/diagnostic":
        state["pulls"] += 1
        if state["pulls"] == 1:
            reply(msg, {"kind": "full", "resultId": "r-1", "items": []})
            ask("workspace/diagnostic/refresh", None, 9001)
        else:
            reply(msg, {
                "kind": "full",
                "resultId": "r-%d" % state["pulls"],
                "items": one_diagnostic(
                    "refresh answered=%s" % state["answered_refresh"])
            })

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// A push server that names the revision it analyzed, and runs one behind: the
/// report for an edit describes the text before it, and the real verdict
/// follows. Servers that fill in `version` let us tell those apart exactly
/// instead of crediting whatever arrives.
pub(super) fn write_versioned_push_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "versioned_push_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/didOpen":
        uri = msg["params"]["textDocument"]["uri"]
        version = msg["params"]["textDocument"]["version"]
        publish_at(uri, "verdict on version %d" % version, version)
    elif method == "textDocument/didChange":
        uri = msg["params"]["textDocument"]["uri"]
        version = msg["params"]["textDocument"]["version"]
        # One revision behind: this describes the text before the edit.
        publish_at(uri, "stale verdict on version %d" % (version - 1), version - 1)

serve({"textDocumentSync": {"openClose": True, "change": 1}}, handle)
"#,
    )
}

/// rust-analyzer's shape: it publishes, *and* it answers
/// `textDocument/diagnostic` — but deliberately with a different, smaller set.
/// Its `cargo check` results only ever arrive by push, so a client that takes
/// the pull answer as the whole picture loses every one of them.
pub(super) fn write_push_and_pull_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "push_and_pull_lsp.py",
        r#"
def handle(msg, method):
    if method in ("textDocument/didOpen", "textDocument/didChange"):
        uri = msg["params"]["textDocument"]["uri"]
        version = msg["params"]["textDocument"]["version"]
        publish_at(uri, "the check that only the push channel runs, pulls=%d" % state["pulls"], version)
    elif method == "textDocument/diagnostic":
        state["pulls"] += 1
        # Answers, and has nothing of its own to say about this file.
        reply(msg, {"kind": "full", "resultId": "r-%d" % state["pulls"], "items": []})

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// A server that publishes for a file before it has ever been told about it —
/// the shape of a workspace-wide or `cargo check` report arriving for a file
/// the client has not opened.
pub(super) fn write_publishes_before_open_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "publishes_before_open_lsp.py",
        r#"
import os

def handle(msg, method):
    if method == "initialized":
        # Report on a file the client has not opened, the way a workspace-wide
        # or check-on-save pass does.
        publish(os.environ["PREOPENED_URI"], "reported before the file was opened")

serve({"textDocumentSync": {"openClose": True, "change": 1}}, handle)
"#,
    )
}

/// A push-only server that asks for a diagnostics refresh anyway. There is
/// nothing to re-pull from it, so the right response is to leave what it has
/// already told us alone rather than throw it away.
pub(super) fn write_refresh_without_pull_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "refresh_without_pull_lsp.py",
        r#"
def handle(msg, method):
    if method in ("textDocument/didOpen", "textDocument/didChange"):
        uri = msg["params"]["textDocument"]["uri"]
        publish(uri, "a real problem")
        ask("workspace/diagnostic/refresh", None, 9002)

serve({"textDocumentSync": {"openClose": True, "change": 1}}, handle)
"#,
    )
}

/// A server that says nothing of its own accord and does not implement pull
/// diagnostics either. It is asked once, says so, and must not be asked again.
pub(super) fn write_pull_rejecting_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "pull_rejecting_lsp.py",
        r#"
import os

# The count goes to a file, not a diagnostic: a server that publishes is not
# one we pull from, so publishing here would remove the thing being counted.
counted = os.path.join(os.path.dirname(sys.argv[0]), "pulls.txt")

def handle(msg, method):
    if method == "textDocument/diagnostic":
        state["pulls"] += 1
        with open(counted, "w") as f:
            f.write(str(state["pulls"]))
        send_message({
            "jsonrpc": "2.0",
            "id": msg.get("id"),
            "error": {"code": -32601, "message": "method not found"}
        })

serve({"textDocumentSync": {"openClose": True, "change": 1}}, handle)
"#,
    )
}

/// Reports a real problem once, then answers "clean" twice, then stops
/// answering at all. Enough rope to hang a client that lets a clean answer
/// about replaced text erase what it holds: the two clean answers belong to a
/// revision that has been superseded by the time the second arrives, and the
/// silence afterwards means nothing can quietly put the error back.
pub(super) fn write_clean_then_silent_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "clean_then_silent_lsp.py",
        r#"
def handle(msg, method):
    if method == "textDocument/diagnostic":
        state["pulls"] += 1
        if state["pulls"] == 1:
            items = one_diagnostic("the real problem")
        elif state["pulls"] <= 3:
            items = []
        else:
            return  # no reply at all
        reply(msg, {"kind": "full", "resultId": "r-%d" % state["pulls"], "items": items})

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// Roslyn's worst-case shape: asked for diagnostics before it has loaded the
/// solution, it does not answer at all. Some time later it announces it is
/// ready, and only then does it start answering — and even then not instantly.
///
/// By the time it speaks, a client that judges silence by the clock has already
/// stopped waiting for it, which is exactly when it must start again.
pub(super) fn write_loads_after_going_quiet_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "loads_after_quiet_lsp.py",
        r#"
import time

def handle(msg, method):
    if method == "textDocument/diagnostic":
        state["pulls"] += 1
        if state["pulls"] == 1:
            # Still loading, and it will not answer questions about code it has
            # not read. Not MethodNotFound — it implements this, it just cannot
            # answer yet.
            send_message({
                "jsonrpc": "2.0",
                "id": msg.get("id"),
                "error": {"code": -32603, "message": "still loading"}
            })
            # Some time later — long enough that a client watching the clock
            # has given up on it — the solution is open.
            time.sleep(0.1)
            notify("workspace/projectInitializationComplete", None)
            return
        time.sleep(0.25)
        reply(msg, {
            "kind": "full",
            "resultId": "r-%d" % state["pulls"],
            "items": one_diagnostic("found once the solution was loaded")
        })

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}

/// rust-analyzer at its most dangerous: it answers a pull promptly and has
/// nothing of its own to say, while the errors that matter — the ones only
/// `cargo check` finds — arrive on the push channel a moment later.
///
/// A client that takes the pull answer as the verdict settles the file as
/// clean, and by the time the real errors land nobody is waiting for them.
pub(super) fn write_slow_check_server() -> (tempfile::TempDir, PathBuf) {
    write_python_server(
        "slow_check_lsp.py",
        r#"
import threading

def publish_later(uri, version):
    def run():
        import time
        time.sleep(0.3)
        publish_at(uri, "an error only the check finds", version)
    threading.Thread(target=run, daemon=True).start()

def handle(msg, method):
    if method in ("textDocument/didOpen", "textDocument/didChange"):
        publish_later(msg["params"]["textDocument"]["uri"],
                      msg["params"]["textDocument"]["version"])
    elif method == "textDocument/diagnostic":
        state["pulls"] += 1
        # Answers at once, with only what its own analysis knows: nothing.
        reply(msg, {"kind": "full", "resultId": "r-%d" % state["pulls"], "items": []})

serve({
    "textDocumentSync": {"openClose": True, "change": 2},
    "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False}
}, handle)
"#,
    )
}
