use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use agent_client_protocol as acp;
use agent_client_protocol::Client as _;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use echo_agent_mcp::elicitation::{
    ElicitationInbox, ElicitationJob, cancel_result, elicit_result_from_wire,
};
use echo_agent_mcp::wire::MCP_ELICIT;
use echo_agent_tools::mcp_elicitation::{McpElicitExtRequest, McpElicitExtResponse};

use crate::session::pending_interaction::{
    PendingInteractionGuard, PendingInteractions, PendingKind,
};

pub(crate) struct ElicitationCoordinatorGuard {
    inbox: ElicitationInbox,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for ElicitationCoordinatorGuard {
    fn drop(&mut self) {
        self.inbox.close();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[must_use]
pub(crate) fn spawn_elicitation_coordinator(
    job_rx: ElicitationInbox,
    gateway: GatewaySender,
    session_id: acp::SessionId,
    pending_interactions: PendingInteractions,
    non_interactive: Rc<Cell<bool>>,
) -> ElicitationCoordinatorGuard {
    let inbox = job_rx.clone();
    let task = tokio::task::spawn_local(async move {
        while let Some(job) = job_rx.recv().await {
            handle_one_job(
                job,
                &gateway,
                &session_id,
                &pending_interactions,
                non_interactive.get(),
            )
            .await;
        }
    });
    ElicitationCoordinatorGuard {
        inbox,
        task: Some(task),
    }
}

async fn handle_one_job(
    job: ElicitationJob,
    gateway: &GatewaySender,
    session_id: &acp::SessionId,
    pending_interactions: &PendingInteractions,
    non_interactive: bool,
) {
    // `fields` was validated by `bridge_elicit` before the job was queued.
    let ElicitationJob {
        server_name,
        fields,
        mut response_tx,
    } = job;

    if non_interactive {
        tracing::info!(
            server = %server_name,
            "MCP elicitation in non-interactive session; cancelling"
        );
        let _ = response_tx.send(cancel_result());
        return;
    }

    let tool_call_id = format!("mcp-elicit-{}", uuid::Uuid::new_v4());

    let ext_req = McpElicitExtRequest {
        session_id: session_id.0.to_string(),
        tool_call_id: tool_call_id.clone(),
        server_name: server_name.clone(),
        message: fields.message,
        mode: fields.mode,
    };

    debug_assert!(
        !ext_req.session_id.is_empty(),
        "mcp elicit reverse-request must carry a non-empty sessionId"
    );

    let ext_request = match serde_json::value::to_raw_value(&ext_req) {
        Ok(raw) => acp::ExtRequest::new(MCP_ELICIT, raw.into()),
        Err(e) => {
            tracing::error!(
                server = %server_name,
                error = %e,
                "failed to serialize mcp elicit request; cancelling"
            );
            let _ = response_tx.send(cancel_result());
            return;
        }
    };

    let _pending_guard = PendingInteractionGuard::new(
        Arc::clone(pending_interactions),
        gateway.clone(),
        session_id.clone(),
        tool_call_id,
        PendingKind::McpElicitation,
    );

    // Race the user's answer against the MCP side abandoning the job:
    // when the server cancels `elicitation/create` (or the client is torn
    // down), the bridge drops its receiver and `response_tx.closed()`
    // fires. Returning drops `_pending_guard`, whose `InteractionResolved`
    // broadcast dismisses the now-orphaned pager card.
    let result = tokio::select! {
        response = gateway.ext_method(ext_request) => match response {
            Ok(raw) => match serde_json::from_str::<McpElicitExtResponse>(raw.0.get()) {
                Ok(typed) => elicit_result_from_wire(&typed),
                Err(e) => {
                    tracing::error!(
                        server = %server_name,
                        error = %e,
                        "malformed mcp elicit response; cancelling"
                    );
                    cancel_result()
                }
            },
            Err(e) => {
                tracing::warn!(
                    server = %server_name,
                    error = %e,
                    "mcp elicit ACP transport error; cancelling"
                );
                cancel_result()
            }
        },
        _ = response_tx.closed() => {
            tracing::info!(
                server = %server_name,
                "mcp elicit abandoned by server; dismissing HITL card"
            );
            return;
        }
    };

    let _ = response_tx.send(result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_mcp::elicitation::wire_mode_and_fields;
    use echo_agent_mcp::rmcp::model::{
        ElicitRequestParams, ElicitationAction, ElicitationSchema, PrimitiveSchemaDefinition,
        StringSchema,
    };
    use echo_agent_tools::mcp_elicitation::McpElicitModeFields;

    #[test]
    fn wire_fields_form() {
        let schema = ElicitationSchema::builder()
            .required_property(
                "email",
                PrimitiveSchemaDefinition::String(StringSchema::email()),
            )
            .build()
            .unwrap();
        let params = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "Need email".into(),
            requested_schema: schema,
        };
        let fields = wire_mode_and_fields(&params).expect("form mode is supported");
        assert_eq!(fields.message, "Need email");
        assert!(matches!(
            fields.mode,
            McpElicitModeFields::Form {
                requested_schema: Some(_)
            }
        ));
    }

    #[test]
    fn wire_response_maps() {
        let accept = elicit_result_from_wire(&McpElicitExtResponse::Accept {
            content: Some(serde_json::json!({"a": 1})),
        });
        assert_eq!(accept.action, ElicitationAction::Accept);
        assert!(accept.content.is_some());

        assert_eq!(
            elicit_result_from_wire(&McpElicitExtResponse::Decline).action,
            ElicitationAction::Decline
        );
        assert_eq!(
            elicit_result_from_wire(&McpElicitExtResponse::Cancel).action,
            ElicitationAction::Cancel
        );
    }

    #[test]
    fn cancel_helper() {
        assert_eq!(cancel_result().action, ElicitationAction::Cancel);
    }
}
