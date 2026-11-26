//! Exit detection and completion for ACP background terminals: awaits
//! `wait_for_exit` while polling `terminal/output` into the [`OutputRecorder`],
//! then completes and releases the terminal.

use std::time::Duration;

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use echo_agent_tools::notification::types::ToolNotificationHandle;

use super::adapter::{SnapshotOutput, TaskMap, parse_exit};
use super::output_recorder::OutputRecorder;

const RECORDER_POLL: Duration = Duration::from_millis(250);

const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(500);

const GATEWAY_LOST_AFTER: Duration = Duration::from_secs(30);

fn max_poll_errors(cadence: Duration) -> u32 {
    (GATEWAY_LOST_AFTER.as_millis() / cadence.as_millis().max(1)).max(1) as u32
}

enum PollStep {
    Output(Box<acp::TerminalOutputResponse>),
    Retry,
    GaveUp,
}

async fn poll_terminal_output(
    gateway: &GatewaySender,
    session_id: &acp::SessionId,
    terminal_id: &acp::TerminalId,
    consecutive_errors: &mut u32,
    max_errors: u32,
) -> PollStep {
    match gateway
        .send(acp::TerminalOutputRequest::new(
            session_id.clone(),
            terminal_id.clone(),
        ))
        .await
    {
        Ok(output) => {
            *consecutive_errors = 0;
            PollStep::Output(Box::new(output))
        }
        Err(e) => {
            *consecutive_errors += 1;
            if *consecutive_errors >= max_errors {
                tracing::error!(
                    terminal_id = %terminal_id.0,
                    error = %e,
                    "gateway unreachable after consecutive poll failures"
                );
                PollStep::GaveUp
            } else {
                PollStep::Retry
            }
        }
    }
}

enum Exit {
    WithOutput(Box<acp::TerminalOutputResponse>),
    NeedFetch,
    Lost,
}

pub(super) async fn watch_for_exit(
    gateway: GatewaySender,
    session_id: acp::SessionId,
    task_id: String,
    tasks: TaskMap,
    notification_handle: ToolNotificationHandle,
    mut recorder: OutputRecorder,
) {
    let terminal_id = acp::TerminalId::new(task_id.clone());

    let wait = gateway.send(acp::WaitForTerminalExitRequest::new(
        session_id.clone(),
        terminal_id.clone(),
    ));
    tokio::pin!(wait);
    let mut wait_pending = true;
    let mut consecutive_errors = 0u32;
    let poll_error_budget = max_poll_errors(RECORDER_POLL);
    let exit = loop {
        tokio::select! {
            res = &mut wait, if wait_pending => match res {
                Ok(_) => break Exit::NeedFetch,
                Err(e) => {
                    tracing::warn!(
                        task_id,
                        error = %e,
                        "watch_for_exit: gateway error waiting for terminal exit, polling until exit"
                    );
                    wait_pending = false;
                }
            },
            _ = tokio::time::sleep(RECORDER_POLL) => {
                match poll_terminal_output(
                    &gateway,
                    &session_id,
                    &terminal_id,
                    &mut consecutive_errors,
                    poll_error_budget,
                )
                .await
                {
                    PollStep::Output(output) => {
                        if let Err(e) = recorder.append(&output.output).await {
                            tracing::debug!(task_id, error = %e, "output recorder append failed; retrying next poll");
                        }
                        if output.exit_status.is_some() {
                            break Exit::WithOutput(output);
                        }
                    }
                    PollStep::Retry => {}
                    PollStep::GaveUp => break Exit::Lost,
                }
            }
        }
    };

    let output = match exit {
        Exit::Lost => {
            complete_and_release(
                &gateway,
                &session_id,
                &terminal_id,
                &tasks,
                &notification_handle,
                &task_id,
                SnapshotOutput {
                    output: recorder.mirrored().to_string(),
                    truncated: false,
                    exit_code: None,
                    signal: Some("gateway-lost".into()),
                },
            )
            .await;
            return;
        }
        Exit::WithOutput(output) => *output,
        Exit::NeedFetch => match gateway
            .send(acp::TerminalOutputRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
        {
            Ok(output) => output,
            // The fetch failed; fall back to what we already mirrored to disk so
            // the completion snapshot is not empty while the log file has data.
            Err(_) => acp::TerminalOutputResponse::new(recorder.mirrored().to_string(), false),
        },
    };

    let (exit_code, signal) = parse_exit(&output.exit_status);
    if let Err(e) = recorder.append(&output.output).await {
        tracing::warn!(task_id, error = %e, "output recorder failed to write final output");
    }
    complete_and_release(
        &gateway,
        &session_id,
        &terminal_id,
        &tasks,
        &notification_handle,
        &task_id,
        SnapshotOutput {
            output: output.output,
            truncated: output.truncated,
            exit_code,
            signal,
        },
    )
    .await;
}

/// Releases even when the task is gone, so the client terminal is not leaked.
async fn complete_and_release(
    gateway: &GatewaySender,
    session_id: &acp::SessionId,
    terminal_id: &acp::TerminalId,
    tasks: &TaskMap,
    notification_handle: &ToolNotificationHandle,
    task_id: &str,
    out: SnapshotOutput,
) {
    let snapshot = {
        let mut guard = tasks.lock().unwrap();
        guard.get_mut(task_id).map(|task| {
            task.mark_completed(out.clone());
            task.to_snapshot(task_id, out)
        })
    };
    if let Some(snapshot) = snapshot {
        notification_handle.send_task_complete(snapshot);
    }
    release_terminal(gateway, session_id, terminal_id).await;
}

pub(super) async fn release_terminal(
    gateway: &GatewaySender,
    session_id: &acp::SessionId,
    terminal_id: &acp::TerminalId,
) {
    if let Err(e) = gateway
        .send(acp::ReleaseTerminalRequest::new(
            session_id.clone(),
            terminal_id.clone(),
        ))
        .await
    {
        tracing::debug!(terminal_id = %terminal_id.0, error = %e, "release_terminal failed");
    }
}

/// Fallback exit detector for the blocking `wait_for_completion` path. Unlike
/// [`watch_for_exit`] it only detects exit and does not mirror output. Returns
/// `true` on exit, `false` on deadline or [`GATEWAY_LOST_AFTER`] of failures.
pub(super) async fn poll_for_terminal_exit(
    gateway: &GatewaySender,
    session_id: &acp::SessionId,
    terminal_id: &acp::TerminalId,
    deadline: Option<tokio::time::Instant>,
) -> bool {
    let mut consecutive_errors = 0u32;
    loop {
        if let Some(dl) = deadline
            && tokio::time::Instant::now() >= dl
        {
            return false;
        }
        tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        match poll_terminal_output(
            gateway,
            session_id,
            terminal_id,
            &mut consecutive_errors,
            max_poll_errors(EXIT_POLL_INTERVAL),
        )
        .await
        {
            PollStep::Output(output) => {
                if output.exit_status.is_some() {
                    return true;
                }
            }
            PollStep::Retry => {}
            PollStep::GaveUp => return false,
        }
    }
}
