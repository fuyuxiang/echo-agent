use crate::agent::mvp_agent::MvpAgent;
use crate::extensions::routing::RequestMeta;
use crate::session::ExtMethodResult;
use crate::terminal::{self, KillOutcome};
use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalRequest {
    pub session_id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    pub cwd: Option<String>,
    pub output_byte_limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TerminalIdRequest {
    pub session_id: String,
    pub terminal_id: String,
}

/// Response for any terminal creation — piped or PTY. Both return just a `terminalId`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalResponse {
    pub terminal_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PtyCreateRequest {
    pub shell: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    pub name: Option<String>,
    #[serde(default, rename = "_meta")]
    pub meta: Option<RequestMeta>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PtyLoadRequest {
    pub terminal_id: String,
    #[serde(default, rename = "_meta")]
    pub meta: Option<RequestMeta>,
}

/// Terminal kill request — `session_id` is required for piped terminals,
/// ignored for PTY terminals (looked up by `terminal_id` alone).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KillTerminalRequest {
    pub terminal_id: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PtyResizeRequest {
    pub terminal_id: String,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PtyInputNotification {
    pub terminal_id: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TerminalListResponse {
    pub terminals: Vec<terminal::TerminalInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitStatusResponse {
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputResponse {
    pub output: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<ExitStatusResponse>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KillOutcomeResponse {
    Killed,
    AlreadyExited,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KillTerminalResponse {
    pub outcome: KillOutcomeResponse,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseTerminalResponse {}

fn parse<T: serde::de::DeserializeOwned>(args: &acp::ExtRequest) -> Result<T, acp::Error> {
    serde_json::from_str(args.params.get())
        .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))
}

fn respond<T: Serialize>(result: Result<T, impl std::fmt::Display>) -> ExtResult {
    ExtMethodResult::from_result(result)
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

/// Like `respond`, but converts `TerminalExtError` into a structured
/// `{ code, message, data }` error instead of stringifying it.
fn respond_pty<T: Serialize>(result: Result<T, terminal::TerminalExtError>) -> ExtResult {
    let ext_result: ExtMethodResult<T> = match result {
        Ok(value) => ExtMethodResult::success(value),
        Err(err) => err.into(),
    };
    ext_result
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

const ERR_TERMINAL_NOT_FOUND: &str = "terminal not found";

impl From<terminal::ExitStatus> for ExitStatusResponse {
    fn from(s: terminal::ExitStatus) -> Self {
        Self {
            exit_code: s.exit_code,
            signal: s.signal,
        }
    }
}

impl From<terminal::OutputSnapshot> for TerminalOutputResponse {
    fn from(s: terminal::OutputSnapshot) -> Self {
        Self {
            output: s.output,
            truncated: s.truncated,
            exit_status: s.exit_status.map(Into::into),
        }
    }
}

impl From<KillOutcome> for KillOutcomeResponse {
    fn from(o: KillOutcome) -> Self {
        match o {
            KillOutcome::Killed => Self::Killed,
            KillOutcome::AlreadyExited => Self::AlreadyExited,
        }
    }
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/terminal/create" => {
            let req: CreateTerminalRequest = parse(args)?;
            let env: HashMap<String, String> = req
                .env
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect();

            let result = terminal::create_terminal(
                &req.session_id,
                &req.command,
                &req.args,
                env,
                req.cwd.as_deref(),
                req.output_byte_limit,
            )
            .await
            .map(|terminal_id| CreateTerminalResponse { terminal_id });

            respond(result)
        }

        "echo.agent/terminal/kill" => {
            // Try PTY registry first, then piped terminal registry.
            let req: KillTerminalRequest = parse(args)?;

            // PTY registry (agent-scoped, keyed by terminal_id alone)
            if terminal::pty_session::get_pty(&req.terminal_id)
                .await
                .is_some()
            {
                let already_exited = terminal::pty_session::is_exited(&req.terminal_id).await;
                terminal::pty_session::close_pty(&req.terminal_id)
                    .await
                    .ok();
                return respond(Ok::<_, String>(KillTerminalResponse {
                    outcome: if already_exited {
                        KillOutcomeResponse::AlreadyExited
                    } else {
                        KillOutcomeResponse::Killed
                    },
                }));
            }

            // Piped terminal registry (session-scoped)
            let session_id = match &req.session_id {
                Some(session_id) => Some(session_id.clone()),
                None => terminal::find_terminal_session_id(&req.terminal_id).await,
            };

            if let Some(session_id) = session_id {
                let result = terminal::kill_terminal(&session_id, &req.terminal_id)
                    .await
                    .and_then(|opt| {
                        opt.map(|outcome| KillTerminalResponse {
                            outcome: outcome.into(),
                        })
                        .ok_or_else(|| ERR_TERMINAL_NOT_FOUND.to_string())
                    });
                respond(result)
            } else {
                respond(Err::<KillTerminalResponse, _>(ERR_TERMINAL_NOT_FOUND))
            }
        }

        "echo.agent/terminal/output" => {
            let req: TerminalIdRequest = parse(args)?;
            let result = terminal::get_terminal_output(&req.session_id, &req.terminal_id)
                .await
                .map(TerminalOutputResponse::from)
                .ok_or(ERR_TERMINAL_NOT_FOUND);
            respond(result)
        }

        "echo.agent/terminal/wait_for_exit" => {
            let req: TerminalIdRequest = parse(args)?;
            let result = terminal::wait_for_terminal_exit(&req.session_id, &req.terminal_id)
                .await
                .map(ExitStatusResponse::from)
                .ok_or(ERR_TERMINAL_NOT_FOUND);
            respond(result)
        }

        "echo.agent/terminal/release" => {
            let req: TerminalIdRequest = parse(args)?;
            terminal::release_terminal(&req.session_id, &req.terminal_id).await;
            ExtMethodResult::success(ReleaseTerminalResponse {})
                .to_ext_response()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))
        }

        "echo.agent/terminal/background" => {
            // Mark a terminal as backgrounded - the process keeps running but
            // waiting callers are notified so the agent can continue.
            //
            // Route through the session's tool bridge so the LocalTerminalBackend
            // actor unblocks the foreground waiter (BashTool::run). Also try the
            // StreamingLocalTerminalRunner registry for AcpTerminalAdapter-based sessions.
            let req: TerminalIdRequest = parse(args)?;
            agent
                .background_foreground_command(&req.session_id, &req.terminal_id)
                .await;
            terminal::background_terminal(&req.session_id, &req.terminal_id).await;
            ExtMethodResult::success(ReleaseTerminalResponse {})
                .to_ext_response()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))
        }

        "echo.agent/terminal/pty/create" => {
            let req: PtyCreateRequest = parse(args)?;
            let env: HashMap<String, String> = req
                .env
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect();
            let target_client_id = req.meta.map(|m| m.client_id).unwrap_or_default();
            let cwd = req.cwd.or_else(|| {
                req.session_id
                    .as_ref()
                    .and_then(|sid| agent.get_session_cwd(sid))
                    .map(|p| p.to_string_lossy().into_owned())
            });

            let result = terminal::pty_session::create_pty(
                req.shell.as_deref(),
                cwd.as_deref(),
                env,
                req.rows.unwrap_or(24),
                req.cols.unwrap_or(80),
                req.name.as_deref(),
                agent.gateway.clone(),
                target_client_id,
            )
            .await
            .map(|id| CreateTerminalResponse { terminal_id: id });

            respond_pty(result)
        }

        "echo.agent/terminal/pty/load" => {
            let req: PtyLoadRequest = parse(args)?;
            let target_client_id = req.meta.map(|m| m.client_id).unwrap_or_default();
            let result =
                terminal::pty_session::load(&req.terminal_id, &agent.gateway, target_client_id)
                    .await;
            respond_pty(result)
        }

        "echo.agent/terminal/pty/resize" => {
            let req: PtyResizeRequest = parse(args)?;
            respond_pty(
                terminal::pty_session::resize_pty(&req.terminal_id, req.rows, req.cols).await,
            )
        }

        "echo.agent/terminal/list" => {
            let terminals = terminal::list_terminals().await;
            respond(Ok::<_, String>(TerminalListResponse { terminals }))
        }

        _ => Err(acp::Error::method_not_found()),
    }
}

pub(crate) async fn handle_pty_input(params: &serde_json::Value) {
    use base64::Engine as _;

    let Ok(input) = serde_json::from_value::<PtyInputNotification>(params.clone()) else {
        tracing::warn!("failed to parse pty input notification");
        return;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&input.data) else {
        tracing::warn!("failed to decode pty input base64");
        return;
    };

    if let Err(e) = terminal::pty_session::write_pty_input(&input.terminal_id, &bytes).await {
        tracing::warn!("pty input write failed: {e}");
    }
}
