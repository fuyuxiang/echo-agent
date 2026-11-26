//! Probe that verifies a running workspace-server actually serves tools
//! over the server connection (not just that it reached READY).
//!
//! Connects to the server as a client harness, binds the workspace-server's
//! session (its `server_id` equals the sandbox `session_id`), then
//! invokes real tools and asserts the results:
//!   - `run_terminal_command` echoes a nonce that must come back,
//!   - `read_file` reads a file the first command wrote.
//!
//! Exits 0 and prints `PROBE_OK` on success; non-zero with a diagnostic
//! on failure. Intended for sandbox end-to-end tests.
//!
//! The client connects to the *local* server (the same one the
//! workspace-server reaches back to, e.g. `ws://localhost:10030/v1/tools`)
//! using a bearer token. `servers.list` is scoped per-user on the server, so
//! the bearer must resolve to the same user that owns the session — the
//! access token from `~/.echo-agent/auth.json` does (same identity).

use base64::Engine;
use clap::Parser;
use echo_agent_computer_hub_sdk::pool::HubConnectionPool;
use echo_agent_computer_hub_sdk::{AuthCredential, ToolHarnessBuilder};
use echo_agent_tool_protocol::{SessionId, ToolId};
use echo_agent_tool_runtime::{ToolCallContext, ToolStreamItem, TypedToolOutput};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "workspace-server-probe")]
#[command(about = "Invoke tools on a running workspace-server via the server connection")]
struct Args {
    /// Server WebSocket URL the client connects to (the local server).
    #[arg(long, default_value = "ws://localhost:10030/v1/tools")]
    hub_url: String,

    /// The workspace-server's server_id (equals the sandbox session_id).
    #[arg(long)]
    session_id: String,

    /// Hub user to authenticate as. Must match the user the
    /// workspace-server registered under (`local-dev` in local-auth-dev).
    #[arg(long, default_value = "local-dev")]
    user_id: String,

    /// Explicit bearer token (overrides --user-id). For real tokens.
    #[arg(long)]
    bearer: Option<String>,

    /// Workspace directory to bind on the server.
    #[arg(long, default_value = "/workspace")]
    cwd: String,
}

/// Mint an unsigned JWT carrying `{"sub": user_id}`. The local-auth-dev
/// hub derives the principal's user from the bearer's JWT `sub` and does
/// NOT verify the signature, so this is sufficient to authenticate as a
/// specific local-dev user. Not usable against a real (verifying) hub.
fn dev_bearer(user_id: &str) -> String {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = b64.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = b64.encode(json!({ "sub": user_id }).to_string());
    format!("{header}.{payload}.sig")
}

fn bearer(args: &Args) -> String {
    args.bearer
        .clone()
        .unwrap_or_else(|| dev_bearer(&args.user_id))
}

/// Drive a tool call to its terminal item, discarding progress.
async fn call_tool(
    harness: &echo_agent_computer_hub_sdk::ToolHarness,
    name: &str,
    args: Value,
) -> anyhow::Result<Value> {
    let tool_id = ToolId::new(name).map_err(|e| anyhow::anyhow!("invalid tool id {name}: {e}"))?;
    let mut stream = harness
        .call(tool_id, args, ToolCallContext::default())
        .await;
    loop {
        let item = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
        match item {
            Some(ToolStreamItem::Progress(_)) => {}
            Some(ToolStreamItem::Terminal(Ok(typed))) => {
                let typed: TypedToolOutput = typed;
                return Ok(typed.value);
            }
            Some(ToolStreamItem::Terminal(Err(e))) => {
                anyhow::bail!("tool `{name}` returned error: {e}")
            }
            None => anyhow::bail!("tool `{name}` stream ended without a terminal item"),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    echo_agent_extra_ca::ensure_default_crypto_provider();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let args = Args::parse();

    let credential = AuthCredential::bearer(bearer(&args));

    // The server connection can drop once right after connect (the SDK
    // reconnects with backoff). build()'s eager session_open doesn't
    // survive that first blip, so retry the connect + bind a few times.
    let mut last_err = None;
    for attempt in 1..=8u32 {
        match connect_and_bind(&args, &credential).await {
            Ok(harness) => return run_checks(&harness, &args).await,
            Err(e) => {
                eprintln!("[probe] attempt {attempt} failed: {e}");
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("probe failed")))
}

/// Connect to the server, open a session, and bind the workspace-server.
async fn connect_and_bind(
    args: &Args,
    credential: &AuthCredential,
) -> anyhow::Result<echo_agent_computer_hub_sdk::ToolHarness> {
    let harness_session = SessionId::new(format!("probe-{}", Uuid::new_v4()))
        .map_err(|e| anyhow::anyhow!("invalid harness session id: {e}"))?;
    let url = Url::parse(&format!("{}?role=harness", args.hub_url))
        .map_err(|e| anyhow::anyhow!("invalid --hub-url: {e}"))?;

    let harness = ToolHarnessBuilder::default()
        .pool(HubConnectionPool::new())
        .url(url)
        .auth(credential.clone())
        .session(harness_session)
        .build()
        .await
        .map_err(|e| anyhow::anyhow!("failed to build server harness: {e}"))?;

    let servers = harness
        .list_servers()
        .await
        .map_err(|e| anyhow::anyhow!("servers.list failed: {e}"))?;
    let server_ids: Vec<String> = servers
        .iter()
        .map(|s| s.server_id.as_str().to_owned())
        .collect();
    eprintln!("[probe] servers visible to this user: {server_ids:?}");

    let server_id = args.session_id.as_str();
    // Strict servers (`--require-explicit-toolset`) fail metadata-less binds
    // closed: bind with exactly the tools the checks below invoke.
    let metadata = json!({
        "tools": [
            {"id": "EchoAgentBuild:run_terminal_cmd", "name_override": "run_terminal_command"},
            {"id": "EchoAgentBuild:read_file"},
        ],
    });
    let tools = harness
        .session_bind(server_id, Some(&args.cwd), Some(metadata))
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "session_bind({server_id}) failed: {e} (is the workspace-server registered \
                 for this user? visible servers: {server_ids:?})"
            )
        })?;
    eprintln!(
        "[probe] bound workspace-server {server_id}: {} tools available",
        tools.len()
    );
    Ok(harness)
}

/// Invoke real tools on the bound workspace-server and assert results.
async fn run_checks(
    harness: &echo_agent_computer_hub_sdk::ToolHarness,
    _args: &Args,
) -> anyhow::Result<()> {
    // 1) run_terminal_command must echo our nonce back.
    let nonce = format!("probe-nonce-{}", Uuid::new_v4());
    let marker = format!("/tmp/{nonce}.txt");
    let out = call_tool(
        harness,
        "run_terminal_command",
        json!({ "command": format!("echo {nonce} | tee {marker}") }),
    )
    .await?;
    let out_text = serde_json::to_string(&out).unwrap_or_default();
    anyhow::ensure!(
        out_text.contains(&nonce),
        "run_terminal_command output did not contain the nonce; got: {out_text}"
    );
    eprintln!("[probe] run_terminal_command OK (nonce echoed)");

    // 2) read_file must read the file the command wrote.
    let read_out = call_tool(harness, "read_file", json!({ "target_file": marker })).await?;
    let read_text = serde_json::to_string(&read_out).unwrap_or_default();
    anyhow::ensure!(
        read_text.contains(&nonce),
        "read_file did not return the written nonce; got: {read_text}"
    );
    eprintln!("[probe] read_file OK (read back written nonce)");

    println!("PROBE_OK");
    Ok(())
}
