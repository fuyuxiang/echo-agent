use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;

use super::runner::{AsyncTerminalRunner, TerminalError, TerminalRunRequest, TerminalRunResult};

pub struct AcpTerminalRunner {
    pub gateway: GatewaySender,
    pub session_id: acp::SessionId,
}

#[async_trait::async_trait]
// Terminal release on cancel is now handled by kill_and_release_all_for_session()
// in cancel_running_task() — see acp_session.rs.
impl AsyncTerminalRunner for AcpTerminalRunner {
    async fn run(&self, request: TerminalRunRequest) -> Result<TerminalRunResult, TerminalError> {
        let session_id = self.session_id.clone();
        // On Windows the ACP client spawns with its own shell; sending the
        // raw command avoids the /bin/bash dependency.
        #[cfg(unix)]
        let command = {
            let quoted =
                shlex::try_quote(&request.command).map_err(|_| TerminalError::CommandNotQuoted)?;
            format!("{} -lc {}", super::default_shell_path(), quoted)
        };
        #[cfg(not(unix))]
        let command = request.command.clone();
        let create_res = self
            .gateway
            .send(
                acp::CreateTerminalRequest::new(session_id.clone(), command)
                    .args(vec![])
                    .env(
                        request
                            .env
                            .into_iter()
                            .map(|(name, value)| acp::EnvVariable::new(name, value))
                            .collect::<Vec<_>>(),
                    )
                    .cwd(Some(request.cwd.to_path_buf()))
                    .output_byte_limit(Some(request.output_byte_limit as u64)),
            )
            .await
            .map_err(|e| TerminalError::Other(e.to_string()))?;

        // notify the client about the terminal
        let notification = acp::SessionNotification::new(
            session_id.clone(),
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                request.tool_call_id.clone(),
                acp::ToolCallUpdateFields::new()
                    .status(Some(acp::ToolCallStatus::InProgress))
                    .content(Some(vec![acp::ToolCallContent::Terminal(
                        acp::Terminal::new(create_res.terminal_id.clone()),
                    )])),
            )),
        );
        let _ = self.gateway.send(notification).await;

        let result = tokio::time::timeout(
            request.timeout,
            self.gateway.send(acp::WaitForTerminalExitRequest::new(
                session_id.clone(),
                create_res.terminal_id.clone(),
            )),
        )
        .await;

        let timed_out = match result {
            Ok(Ok(_)) => false,
            Ok(Err(e)) => return Err(TerminalError::Other(e.to_string())),
            Err(_) => {
                // timeout occurred, need to stop the command
                let _ = self
                    .gateway
                    .send(acp::KillTerminalRequest::new(
                        session_id.clone(),
                        create_res.terminal_id.clone(),
                    ))
                    .await;
                true
            }
        };

        let output = self
            .gateway
            .send(acp::TerminalOutputRequest::new(
                session_id.clone(),
                create_res.terminal_id.clone(),
            ))
            .await
            .map_err(|e| TerminalError::Other(e.to_string()))?;

        let _ = self
            .gateway
            .send(acp::ReleaseTerminalRequest::new(
                session_id,
                create_res.terminal_id,
            ))
            .await;

        let exit_status = output.exit_status.clone();
        let combined_output = output.output;
        let truncated = output.truncated;

        let exit_code = exit_status
            .as_ref()
            .and_then(|e| e.exit_code.map(|v| v as i32));
        let signal = exit_status.and_then(|e| e.signal);

        Ok(TerminalRunResult {
            combined_output,
            exit_code,
            truncated,
            signal,
            timed_out,
        })
    }
}
