//! Instrumented ACP client and dedicated-thread agent topology shared by the
//! perf tests. The agent runs on its OWN thread so client-side timestamping
//! never competes with the parent session's `LocalSet`. Included per binary
//! via `#[path]`; unused items in one binary are dead code there.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use agent_client_protocol::{self as acp};

use crate::acp_harness;
use echo_agent_acp::{
    AcpAgentGatewayReceiver as GatewayReceiver, AcpAgentGatewaySender as GatewaySender,
    LineBufferedRead,
};
use echo_agent_runtime::agent::config::Config as AgentConfig;
use echo_agent_runtime::agent::mvp_agent::MvpAgent;
use serde_json::Value;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Debug)]
pub struct DispatchEvent {
    pub at: Instant,
    pub tool_call_id: String,
    pub task_id: Option<String>,
    pub last_status: Option<String>,
    pub last_output: Option<String>,
}

#[derive(Debug)]
pub struct SpawnedEvent {
    pub at: Instant,
    pub subagent_id: String,
}

#[derive(Debug)]
pub struct FinishedEvent {
    pub at: Instant,
    pub subagent_id: String,
    pub status: String,
    pub agent_duration_ms: u64,
}

/// Raw callback log; each perf test derives its own analysis from it.
#[derive(Default)]
pub struct Recorded {
    /// Every `session_notification`, as `(arrival, sessionUpdate kind)`.
    pub session_updates: Vec<(Instant, String)>,
    pub dispatch: Vec<DispatchEvent>,
    pub spawned: Vec<SpawnedEvent>,
    pub finished: Vec<FinishedEvent>,
}

pub type SharedRecorded = Rc<RefCell<Recorded>>;

/// Auto-approves permissions; timestamps every client callback into a
/// [`Recorded`] log.
pub struct PerfRecorder {
    pub rec: SharedRecorded,
}

impl PerfRecorder {
    pub fn new() -> (Self, SharedRecorded) {
        let rec: SharedRecorded = Rc::new(RefCell::new(Recorded::default()));
        (Self { rec: rec.clone() }, rec)
    }
}

pub fn json_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|s| s.as_str()).map(str::to_string)
}

impl PerfRecorder {
    fn record_tool_call(&self, update: &Value) {
        let Some(kind) = update.get("sessionUpdate").and_then(|s| s.as_str()) else {
            return;
        };
        if kind != "tool_call" && kind != "tool_call_update" {
            return;
        }
        let Some(tool_call_id) = json_str(update, "toolCallId") else {
            return;
        };
        let task_id = update
            .get("rawInput")
            .and_then(|raw| json_str(raw, "task_id"));
        let status = json_str(update, "status");
        let output = update
            .get("rawOutput")
            .map(|o| o.to_string().chars().take(300).collect::<String>());
        let mut rec = self.rec.borrow_mut();
        if let Some(existing) = rec
            .dispatch
            .iter_mut()
            .find(|d| d.tool_call_id == tool_call_id)
        {
            // First-seen instant wins; later updates backfill fields.
            if existing.task_id.is_none() {
                existing.task_id = task_id;
            }
            if status.is_some() {
                existing.last_status = status;
            }
            if output.is_some() {
                existing.last_output = output;
            }
            return;
        }
        // A fresh id with no rawInput yet still records; task_id backfills.
        if kind == "tool_call" || task_id.is_some() {
            rec.dispatch.push(DispatchEvent {
                at: Instant::now(),
                tool_call_id,
                task_id,
                last_status: status,
                last_output: output,
            });
        }
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for PerfRecorder {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Ok(acp::RequestPermissionResponse::new(
            acp_harness::allow_once(&args),
        ))
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        if let Ok(v) = serde_json::to_value(&args.update) {
            let kind = json_str(&v, "sessionUpdate").unwrap_or_default();
            self.rec
                .borrow_mut()
                .session_updates
                .push((Instant::now(), kind));
            self.record_tool_call(&v);
        }
        Ok(())
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
        if args.method.as_ref() != "echo.agent/session_notification" {
            return Ok(());
        }
        let Ok(params) = serde_json::from_str::<Value>(args.params.get()) else {
            return Ok(());
        };
        let Some(update) = params.get("update") else {
            return Ok(());
        };
        match update.get("sessionUpdate").and_then(|s| s.as_str()) {
            Some("subagent_spawned") => {
                if let Some(subagent_id) = json_str(update, "subagent_id") {
                    self.rec.borrow_mut().spawned.push(SpawnedEvent {
                        at: Instant::now(),
                        subagent_id,
                    });
                }
            }
            Some("subagent_finished") => {
                if let Some(subagent_id) = json_str(update, "subagent_id") {
                    self.rec.borrow_mut().finished.push(FinishedEvent {
                        at: Instant::now(),
                        subagent_id,
                        status: json_str(update, "status").unwrap_or_default(),
                        agent_duration_ms: update
                            .get("duration_ms")
                            .and_then(|d| d.as_u64())
                            .unwrap_or(0),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Joins the dedicated agent thread; `finish()` (or drop) shuts it down.
pub struct AgentThread {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl AgentThread {
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            join.join().expect("agent thread panicked");
        }
    }
}

impl Drop for AgentThread {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stand up the agent on its own thread (own current-thread runtime and
/// `LocalSet`), mirroring production's session topology; returns the client
/// pipe ends for [`acp_harness::connect_client`].
pub fn spawn_agent_thread(name: &str) -> (acp_harness::AgentPipes, AgentThread) {
    let (c2a_client, c2a_agent) = tokio::io::duplex(acp_harness::DUPLEX_BUFFER_BYTES);
    let (a2c_agent, a2c_client) = tokio::io::duplex(acp_harness::DUPLEX_BUFFER_BYTES);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let join = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("agent runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let agent_config = AgentConfig::default();
                let auth_manager = Arc::new(agent_config.create_auth_manager());
                let (gw_tx, gw_rx) = tokio::sync::mpsc::unbounded_channel();
                let agent =
                    MvpAgent::new(GatewaySender::new(gw_tx), &agent_config, auth_manager, None)
                        .expect("valid config");

                let agent_incoming = LineBufferedRead::spawn_local(c2a_agent.compat());
                let (agent_conn, agent_io) = acp::AgentSideConnection::new(
                    agent,
                    a2c_agent.compat_write(),
                    agent_incoming,
                    |fut| {
                        tokio::task::spawn_local(fut);
                    },
                );
                tokio::task::spawn_local(
                    GatewayReceiver::new(gw_rx, agent_conn)
                        .with_on_meta(
                            echo_agent_file_utils::trace_context::span_from_meta_traceparent,
                        )
                        .run(),
                );
                tokio::task::spawn_local(agent_io);
                let _ = shutdown_rx.await;
            });
        })
        .expect("spawn agent thread");

    (
        acp_harness::AgentPipes {
            to_agent: c2a_client,
            from_agent: a2c_client,
        },
        AgentThread {
            shutdown: Some(shutdown_tx),
            join: Some(join),
        },
    )
}
