//! End-to-end test for subagent orphan reconciliation on session resume.
//!
//! When a process dies mid-subagent, the subagent's `meta.json` is left
//! `status: "running"` with no `SubagentFinished` — so on resume the client
//! shows it Running forever. `MvpAgent::load_session` heals this: it scans the
//! session's `subagents/` dir and flips any stale `running` meta (not tracked by
//! the live coordinator) to `cancelled` (mechanism A, the meta pass).
//!
//! This test spawns a real `echoagent agent stdio` process, seeds an orphaned
//! `running` meta on disk, resumes the session, and asserts the meta was
//! reconciled to `cancelled`.
//!
//! Run locally (needs a pre-built binary):
//! ```bash
//! cargo test -p echo-agent-runtime --test test_subagent_orphan_reconcile -- --ignored
//! ```

use std::future::Future;
use std::path::{Path, PathBuf};

use echo_agent_test_support::*;

async fn with_local_set<F, Fut>(f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::task::LocalSet::new().run_until(f()).await;
}

/// Find `<home>/sessions/<enc-cwd>/<id>` without depending on the internal cwd
/// encoder: scan the one level of cwd dirs for a child named `<id>`.
fn locate_session_dir(home: &Path, id: &str) -> PathBuf {
    let sessions = home.join("sessions");
    for entry in std::fs::read_dir(&sessions)
        .expect("read sessions dir")
        .flatten()
    {
        let candidate = entry.path().join(id);
        if candidate.is_dir() {
            return candidate;
        }
    }
    panic!(
        "session dir for {id} not found under {}",
        sessions.display()
    );
}

#[tokio::test]
#[ignore] // requires pre-built binary
async fn resume_reconciles_orphaned_running_subagent() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");
        let workdir = git_workdir();

        // Phase 1: create a real session, then take its home so we can seed it.
        let mut writer = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;
        writer.initialize_with_timeout().await;
        let session_id = writer
            .create_session_with_timeout(workdir.workspace())
            .await;
        let shared_sandbox = writer.take_sandbox();
        drop(writer);

        // Simulate a crash: inject a subagent meta left `running` on disk (no
        // terminal write, no SubagentFinished) — exactly what a dead process
        // leaves behind.
        let echo_agent_home = shared_sandbox.echo_agent_home().to_path_buf();
        let session_dir = locate_session_dir(&echo_agent_home, session_id.0.as_ref());
        let sub_id = "sa-orphan";
        let meta_path = session_dir.join("subagents").join(sub_id).join("meta.json");
        std::fs::create_dir_all(meta_path.parent().unwrap()).unwrap();
        std::fs::write(
            &meta_path,
            serde_json::json!({
                "subagent_id": sub_id,
                "parent_session_id": session_id.0.as_ref(),
                "child_session_id": "child-orphan",
                "subagent_type": "general-purpose",
                "description": "stuck task",
                "prompt": "do work",
                "status": "running",
                "started_at": chrono::Utc::now().to_rfc3339(),
            })
            .to_string(),
        )
        .unwrap();

        // Phase 2: resume in a fresh process. `load_session` runs the reconcile.
        let reader =
            EchoAgentStdioClient::spawn_with_sandbox(&server, workdir.workspace(), shared_sandbox)
                .await;
        reader.initialize_with_timeout().await;
        let _ = reader
            .load_session_with_timeout(&session_id, workdir.workspace())
            .await;

        // The orphan's on-disk meta must now be terminal (cancelled), not running.
        let reread: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&meta_path).expect("read orphan meta"))
                .expect("parse orphan meta");
        assert_eq!(
            reread.get("status").and_then(|s| s.as_str()),
            Some("cancelled"),
            "resume must reconcile the orphaned running subagent to cancelled\nstderr:\n{}",
            stderr_tail(&reader.stderr(), 2000)
        );
    })
    .await;
}
