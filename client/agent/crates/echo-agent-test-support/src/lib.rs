#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Shared test utilities for echo-agent-build crates: mock inference server, SSE
//! generators, ACP stdio client, headless runner, env/process sandbox.
//!
//! Provides:
//! - [`MockInferenceServer`] — Mock /v1/chat/completions + /v1/responses with request logging
//! - [`EchoAgentStdioClient`] — ACP client that drives `echoagent agent stdio` as a subprocess
//! - [`RawStdioClient`] — raw-wire ACP driver for bytes the typed client can't
//!   produce (Foundation `\/` methods, string UUID ids)
//! - [`leader::LeaderStdioClient`] — ACP client that drives `echoagent agent --leader stdio` (unix)
//! - [`TestSandbox`] — Own isolated paths, hermetic child env, optional git setup, diagnostics
//! - [`TestProcess`] — Own detached child lifecycle, process-tree teardown, bounded output tails
//! - [`run_headless`] — Run `echoagent -p` against the mock server and capture output
//! - [`git_workdir`] — Create a git-initialized [`TestSandbox`]
//! - [`echo_agent_binary`] — Resolve the echoagent binary path (ECHO_AGENT_BINARY env or cargo_bin)
//! - [`spawn_counting_server`] — Connection-counting HTTP/1.1 server for wire/pooling tests
//! - [`uds_proxy::UdsProxy`] — Frame-aware fault-injection proxy for leader IPC sockets (unix)
//! - [`ResourceSnapshot`] — RSS/threads/fds sampling for soak tests
/// Multiply a harness timeout by `ECHO_AGENT_TEST_TIMEOUT_SCALE` (positive integer,
/// default 1). CI lanes on shared runner pools raise it so pool load slows
/// tests instead of failing them (see the EchoAgent Build merge CI workflow).
pub fn scaled(base: std::time::Duration) -> std::time::Duration {
    let scale = std::env::var("ECHO_AGENT_TEST_TIMEOUT_SCALE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(1);
    base * scale
}
pub mod acp_client;
pub mod counting_server;
pub mod env;
pub mod headless;
mod inference_override;
#[cfg(unix)]
pub mod leader;
pub mod mock_server;
pub mod process;
pub mod resources;
pub mod sandbox;
pub mod scripted;
pub mod sse;
#[cfg(unix)]
pub mod uds_proxy;
pub use acp_client::{EchoAgentStdioClient, RawStdioClient};
pub use counting_server::spawn_counting_server;
pub use env::{EnvGuard, echo_agent_binary, git_workdir};
pub use headless::{
    HeadlessResult, assert_headless_success, assert_no_crashes, run_headless,
    run_headless_in_sandbox, run_headless_in_sandbox_borrowed,
    run_headless_in_sandbox_borrowed_with_env, run_headless_in_sandbox_with_env,
    run_headless_with_env, stderr_tail,
};
pub use inference_override::{InferenceEndpoint, InferenceExpectation, InferenceRequestMatcher};
#[cfg(unix)]
pub use leader::LeaderFixture;
pub use mock_server::{
    MockInferenceServer, MockModelEntry, ScriptedResponse, SseEvent, StorageUpload,
};
#[cfg(unix)]
pub use process::process_has_exited_without_reap;
pub use process::{
    TestOutput, TestOutputSnapshot, TestProcess, TestProcessConfig, TestProcessState,
    TestProcessStderr, TestProcessStdout, TestProcessTermination, TestProcessTree, TestStdin,
};
pub use resources::{ResourceGrowth, ResourceSnapshot, RssMeasurement, RssOutcome, RssSampler};
pub use sandbox::{TestSandbox, TestSandboxBuilder};
