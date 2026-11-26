use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction};
use tokio::sync::{Notify, oneshot};

#[derive(Debug)]
pub struct ElicitationJob {
    pub server_name: String,
    /// Pre-validated by [`bridge_elicit`] via [`wire_mode_and_fields`], so
    /// consumers never see an unsupported mode.
    pub fields: WireElicitFields,
    pub response_tx: oneshot::Sender<ElicitResult>,
}

struct ElicitationInboxInner {
    slot: parking_lot::Mutex<Option<ElicitationJob>>,
    notify: Notify,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct ElicitationInbox {
    inner: Arc<ElicitationInboxInner>,
}

impl std::fmt::Debug for ElicitationInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElicitationInbox")
            .field("closed", &self.inner.closed.load(Ordering::SeqCst))
            .field("occupied", &self.inner.slot.lock().is_some())
            .finish()
    }
}

impl Default for ElicitationInbox {
    fn default() -> Self {
        Self::new()
    }
}

impl ElicitationInbox {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ElicitationInboxInner {
                slot: parking_lot::Mutex::new(None),
                notify: Notify::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub fn close(&self) {
        {
            let mut slot = self.inner.slot.lock();
            self.inner.closed.store(true, Ordering::SeqCst);
            if let Some(prev) = slot.take() {
                let _ = prev.response_tx.send(cancel_result());
            }
        }
        self.inner.notify.notify_waiters();
    }

    pub fn push(&self, job: ElicitationJob) -> Result<(), ElicitationJob> {
        {
            let mut slot = self.inner.slot.lock();
            if self.inner.closed.load(Ordering::SeqCst) {
                return Err(job);
            }
            if let Some(prev) = slot.replace(job) {
                let _ = prev.response_tx.send(cancel_result());
            }
        }
        self.inner.notify.notify_one();
        Ok(())
    }

    pub async fn recv(&self) -> Option<ElicitationJob> {
        loop {
            if let Some(job) = self.inner.slot.lock().take() {
                return Some(job);
            }
            if self.inner.closed.load(Ordering::SeqCst) {
                return None;
            }
            self.inner.notify.notified().await;
        }
    }
}

pub type SharedElicitationTx = Arc<parking_lot::Mutex<Option<ElicitationInbox>>>;

pub fn decline_result() -> ElicitResult {
    ElicitResult::new(ElicitationAction::Decline)
}

pub fn cancel_result() -> ElicitResult {
    ElicitResult::new(ElicitationAction::Cancel)
}

pub fn accept_result(content: Option<serde_json::Value>) -> ElicitResult {
    let mut result = ElicitResult::new(ElicitationAction::Accept);
    if let Some(c) = content {
        result = result.with_content(c);
    }
    result
}

pub async fn bridge_elicit(
    bridge: &SharedElicitationTx,
    server_name: &str,
    params: ElicitRequestParams,
) -> ElicitResult {
    let Some(fields) = wire_mode_and_fields(&params) else {
        tracing::warn!(
            server = %server_name,
            "unsupported elicitation mode; declining"
        );
        return decline_result();
    };

    let sender = bridge.lock().clone();
    let Some(tx) = sender else {
        tracing::debug!(
            server = %server_name,
            "elicitation request with no bridge installed; declining"
        );
        return decline_result();
    };

    let (response_tx, response_rx) = oneshot::channel();
    let job = ElicitationJob {
        server_name: server_name.to_string(),
        fields,
        response_tx,
    };
    if tx.push(job).is_err() {
        tracing::warn!(
            server = %server_name,
            "elicitation bridge channel closed; cancelling"
        );
        return cancel_result();
    }

    match response_rx.await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                server = %server_name,
                "elicitation response oneshot dropped; cancelling"
            );
            cancel_result()
        }
    }
}

pub fn elicit_result_from_wire(
    response: &echo_agent_tools::mcp_elicitation::McpElicitExtResponse,
) -> ElicitResult {
    use echo_agent_tools::mcp_elicitation::McpElicitExtResponse;
    match response {
        McpElicitExtResponse::Accept { content } => accept_result(content.clone()),
        McpElicitExtResponse::Decline => decline_result(),
        McpElicitExtResponse::Cancel => cancel_result(),
    }
}

/// Message + mode-tagged fields of a supported, size-validated elicitation
/// request — exactly what [`McpElicitExtRequest`] still needs on top of the
/// session/tool-call identifiers the shell adds.
///
/// [`McpElicitExtRequest`]: echo_agent_tools::mcp_elicitation::McpElicitExtRequest
#[derive(Debug, Clone)]
pub struct WireElicitFields {
    pub message: String,
    pub mode: echo_agent_tools::mcp_elicitation::McpElicitModeFields,
}

pub fn wire_mode_and_fields(params: &ElicitRequestParams) -> Option<WireElicitFields> {
    use echo_agent_tools::mcp_elicitation::{
        MAX_ELICIT_ID_CHARS, MAX_ELICIT_MESSAGE_CHARS, MAX_ELICIT_SCHEMA_BYTES,
        MAX_ELICIT_URL_CHARS, McpElicitModeFields, chars_within,
    };
    match params {
        ElicitRequestParams::FormElicitationParams {
            message,
            requested_schema,
            ..
        } => {
            if !chars_within(message, MAX_ELICIT_MESSAGE_CHARS) {
                return None;
            }
            let schema = serde_json::to_value(requested_schema).unwrap_or(serde_json::Value::Null);
            let schema_len = serde_json::to_vec(&schema)
                .map(|b| b.len())
                .unwrap_or(usize::MAX);
            if schema_len > MAX_ELICIT_SCHEMA_BYTES {
                return None;
            }
            Some(WireElicitFields {
                message: message.clone(),
                mode: McpElicitModeFields::Form {
                    requested_schema: Some(schema),
                },
            })
        }
        ElicitRequestParams::UrlElicitationParams {
            message,
            url,
            elicitation_id,
            ..
        } => {
            if !chars_within(message, MAX_ELICIT_MESSAGE_CHARS)
                || !chars_within(url, MAX_ELICIT_URL_CHARS)
                || !chars_within(elicitation_id, MAX_ELICIT_ID_CHARS)
            {
                return None;
            }
            Some(WireElicitFields {
                message: message.clone(),
                mode: McpElicitModeFields::Url {
                    url: url.clone(),
                    elicitation_id: elicitation_id.clone(),
                },
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{ElicitationSchema, PrimitiveSchemaDefinition, StringSchema};

    fn url_fields(message: &str, url: &str, elicitation_id: &str) -> WireElicitFields {
        wire_mode_and_fields(&ElicitRequestParams::UrlElicitationParams {
            meta: None,
            message: message.into(),
            url: url.into(),
            elicitation_id: elicitation_id.into(),
        })
        .expect("url mode is supported")
    }

    #[tokio::test]
    async fn no_bridge_declines() {
        let bridge: SharedElicitationTx = Arc::new(parking_lot::Mutex::new(None));
        let schema = ElicitationSchema::builder()
            .required_property(
                "email",
                PrimitiveSchemaDefinition::String(StringSchema::email()),
            )
            .build()
            .unwrap();
        let params = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "hi".into(),
            requested_schema: schema,
        };
        let result = bridge_elicit(&bridge, "srv", params).await;
        assert_eq!(result.action, ElicitationAction::Decline);
    }

    #[tokio::test]
    async fn bridge_accept_with_content() {
        let inbox = ElicitationInbox::new();
        let bridge: SharedElicitationTx = Arc::new(parking_lot::Mutex::new(Some(inbox.clone())));

        let schema = ElicitationSchema::builder()
            .required_property(
                "email",
                PrimitiveSchemaDefinition::String(StringSchema::email()),
            )
            .build()
            .unwrap();
        let params = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "hi".into(),
            requested_schema: schema,
        };

        let handle = tokio::spawn(async move {
            let job = inbox.recv().await.expect("job");
            assert_eq!(job.server_name, "srv");
            let _ = job.response_tx.send(accept_result(Some(serde_json::json!({
                "email": "a@b.com"
            }))));
        });

        let result = bridge_elicit(&bridge, "srv", params).await;
        handle.await.unwrap();
        assert_eq!(result.action, ElicitationAction::Accept);
        assert_eq!(result.content.unwrap()["email"], "a@b.com");
    }

    /// Dropping the bridge future (server cancelled `elicitation/create`)
    /// must close the queued job's response channel, so the coordinator's
    /// `response_tx.closed()` race can dismiss the orphaned HITL card.
    #[tokio::test]
    async fn abandoned_bridge_closes_job_channel() {
        let inbox = ElicitationInbox::new();
        let bridge: SharedElicitationTx = Arc::new(parking_lot::Mutex::new(Some(inbox.clone())));
        let params = ElicitRequestParams::UrlElicitationParams {
            meta: None,
            message: "open".into(),
            url: "https://example.com".into(),
            elicitation_id: "e1".into(),
        };
        let task = tokio::spawn(async move { bridge_elicit(&bridge, "srv", params).await });
        let mut job = inbox.recv().await.expect("job");
        task.abort();
        let _ = task.await;
        tokio::time::timeout(std::time::Duration::from_secs(1), job.response_tx.closed())
            .await
            .expect("sender must observe the receiver drop");
    }

    #[tokio::test]
    async fn closed_channel_cancels() {
        let inbox = ElicitationInbox::new();
        inbox.close();
        let bridge: SharedElicitationTx = Arc::new(parking_lot::Mutex::new(Some(inbox)));
        let params = ElicitRequestParams::UrlElicitationParams {
            meta: None,
            message: "open".into(),
            url: "https://example.com".into(),
            elicitation_id: "e1".into(),
        };
        let result = bridge_elicit(&bridge, "srv", params).await;
        assert_eq!(result.action, ElicitationAction::Cancel);
    }

    #[tokio::test]
    async fn push_after_close_does_not_occupy_slot() {
        let inbox = ElicitationInbox::new();
        inbox.close();
        let (response_tx, _response_rx) = oneshot::channel();
        assert!(
            inbox
                .push(ElicitationJob {
                    server_name: "srv".into(),
                    fields: url_fields("late", "https://example.com", "late"),
                    response_tx,
                })
                .is_err()
        );
        let leftover = tokio::time::timeout(std::time::Duration::from_millis(50), inbox.recv())
            .await
            .expect("recv must not hang");
        assert!(leftover.is_none());
    }

    #[tokio::test]
    async fn concurrent_close_does_not_strand_a_push() {
        for _ in 0..200 {
            let inbox = ElicitationInbox::new();
            let pusher = inbox.clone();
            let thread = std::thread::spawn(move || {
                let (response_tx, response_rx) = oneshot::channel();
                let rejected = pusher
                    .push(ElicitationJob {
                        server_name: "srv".into(),
                        fields: url_fields("race", "https://example.com", "race"),
                        response_tx,
                    })
                    .is_err();
                (rejected, response_rx)
            });
            inbox.close();
            let (rejected, response_rx) = thread.join().expect("pusher");
            if !rejected {
                let action =
                    tokio::time::timeout(std::time::Duration::from_millis(50), response_rx)
                        .await
                        .expect("oneshot must complete")
                        .expect("oneshot must not drop")
                        .action;
                assert_eq!(action, ElicitationAction::Cancel);
            }
            let leftover = tokio::time::timeout(std::time::Duration::from_millis(50), inbox.recv())
                .await
                .expect("recv must not hang");
            assert!(leftover.is_none());
        }
    }

    #[tokio::test]
    async fn later_job_cancels_queued_job() {
        let inbox = ElicitationInbox::new();
        let first = {
            let (response_tx, response_rx) = oneshot::channel();
            inbox
                .push(ElicitationJob {
                    server_name: "a".into(),
                    fields: url_fields("first", "https://example.com/1", "1"),
                    response_tx,
                })
                .expect("push first");
            response_rx
        };
        inbox
            .push(ElicitationJob {
                server_name: "b".into(),
                fields: url_fields("second", "https://example.com/2", "2"),
                response_tx: oneshot::channel().0,
            })
            .expect("push second");
        assert_eq!(first.await.unwrap().action, ElicitationAction::Cancel);
        let kept = inbox.recv().await.expect("kept");
        assert_eq!(kept.server_name, "b");
    }

    #[test]
    fn wire_mapping_form_and_url() {
        use echo_agent_tools::mcp_elicitation::McpElicitModeFields;
        let schema = ElicitationSchema::builder()
            .required_property("x", PrimitiveSchemaDefinition::String(StringSchema::new()))
            .build()
            .unwrap();
        let form = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "m".into(),
            requested_schema: schema,
        };
        let fields = wire_mode_and_fields(&form).expect("form mode is supported");
        assert_eq!(fields.message, "m");
        assert!(matches!(
            fields.mode,
            McpElicitModeFields::Form {
                requested_schema: Some(_)
            }
        ));

        let url_p = ElicitRequestParams::UrlElicitationParams {
            meta: None,
            message: "u".into(),
            url: "https://echo-agent.invalid".into(),
            elicitation_id: "id1".into(),
        };
        let fields = wire_mode_and_fields(&url_p).expect("url mode is supported");
        assert_eq!(fields.message, "u");
        let McpElicitModeFields::Url {
            url,
            elicitation_id,
        } = fields.mode
        else {
            panic!("expected url mode");
        };
        assert_eq!(url, "https://echo-agent.invalid");
        assert_eq!(elicitation_id, "id1");
    }

    #[test]
    fn unknown_mode_is_declined_not_empty_form() {
        fn mapped_or_declined(params: &ElicitRequestParams) -> Result<(), ElicitResult> {
            match wire_mode_and_fields(params) {
                Some(_) => Ok(()),
                None => Err(decline_result()),
            }
        }

        let schema = ElicitationSchema::builder()
            .required_property("x", PrimitiveSchemaDefinition::String(StringSchema::new()))
            .build()
            .unwrap();
        assert!(
            mapped_or_declined(&ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "m".into(),
                requested_schema: schema,
            })
            .is_ok()
        );
        assert!(
            mapped_or_declined(&ElicitRequestParams::UrlElicitationParams {
                meta: None,
                message: "u".into(),
                url: "https://echo-agent.invalid".into(),
                elicitation_id: "id1".into(),
            })
            .is_ok()
        );

        let declined = decline_result();
        assert_eq!(declined.action, ElicitationAction::Decline);
        assert!(
            declined.content.is_none(),
            "unknown mode must not become Accept with {{}}"
        );
    }

    #[test]
    fn oversized_message_is_declined() {
        use echo_agent_tools::mcp_elicitation::MAX_ELICIT_MESSAGE_CHARS;
        let schema = ElicitationSchema::builder()
            .required_property("x", PrimitiveSchemaDefinition::String(StringSchema::new()))
            .build()
            .unwrap();
        let params = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "m".repeat(MAX_ELICIT_MESSAGE_CHARS + 1),
            requested_schema: schema,
        };
        assert!(wire_mode_and_fields(&params).is_none());
    }
}
