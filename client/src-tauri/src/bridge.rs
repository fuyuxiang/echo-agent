//! ACP → Tauri bridge.
//!
//! A long-lived task drains `AgentHandle.rx` (the agent→client channel) and:
//!  - `SessionNotification` → serialize the SessionUpdate, emit `agent://update`,
//!    then ack the oneshot (agent future hangs otherwise);
//!  - `RequestPermission` → register a pending permission in `Permissions`,
//!    emit `agent://permission` (the frontend resolves via a command);
//!  - `ExtNotification("echo.agent/session/prompt_complete")` → emit `agent://complete`;
//!  - fs/terminal requests → never arrive (we advertised no capability); if
//!    they do, we deny so the agent future still completes.

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

use echo_agent_acp::AcpClientMessage;

const MAX_PENDING_INTERACTIONS: usize = 64;
const MAX_EXT_METHOD_PARAMS_BYTES: usize = 256 * 1024;
const MAX_INTERACTION_ID_CHARS: usize = 256;
const MAX_INTERACTION_TEXT_CHARS: usize = 4_096;
const MAX_INTERACTION_TITLE_CHARS: usize = 512;
const MAX_PERMISSION_OPTIONS: usize = 16;
const MAX_PERMISSION_RAW_INPUT_BYTES: usize = 64 * 1024;
const MAX_QUESTION_ITEMS: usize = 12;
const MAX_QUESTION_OPTIONS: usize = 16;
const MAX_QUESTION_OPTION_TEXT_CHARS: usize = 1_024;
const MAX_QUESTION_TIMEOUT_SECS: u64 = 24 * 60 * 60;
const MAX_FOLDER_TRUST_CONFIG_KINDS: usize = 32;
const MAX_FOLDER_TRUST_PATH_CHARS: usize = 4_096;

/// Registry of permissions awaiting a user decision. The frontend calls the
/// `agent_resolve_permission` command, which looks up the entry by id and
/// fulfills the oneshot the agent is waiting on.
#[derive(Default, Clone)]
pub struct Permissions {
    inner: Arc<Mutex<Vec<PendingPermission>>>,
}

/// Registry of questions awaiting a user answer. The frontend calls the
/// `agent_resolve_question` command, which looks up the entry by id and
/// fulfills the oneshot the agent is waiting on.
#[derive(Default, Clone)]
pub struct Questions {
    inner: Arc<Mutex<Vec<PendingQuestion>>>,
}

/// Typed registry for `echo.agent/exit_plan_mode` reverse requests.
#[derive(Default, Clone)]
pub struct PlanApprovals {
    inner: Arc<Mutex<Vec<PendingPlanApproval>>>,
}

/// Typed registry for `echo.agent/folder_trust/request` reverse requests.
#[derive(Default, Clone)]
pub struct FolderTrusts {
    inner: Arc<Mutex<Vec<PendingFolderTrust>>>,
}

struct PendingPermission {
    request: PermissionFrontend,
    response_tx: oneshot::Sender<PermissionOutcome>,
}

struct PendingQuestion {
    request: QuestionFrontend,
    response_tx: oneshot::Sender<QuestionOutcome>,
}

struct PendingPlanApproval {
    request: PlanApprovalFrontend,
    response_tx: oneshot::Sender<PlanApprovalOutcome>,
}

struct PendingFolderTrust {
    request: FolderTrustFrontend,
    response_tx: oneshot::Sender<FolderTrustOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionFrontend {
    pub request_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub title: String,
    pub questions: Vec<QuestionItem>,
    pub mode: QuestionMode,
    pub timeout: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuestionClosedFrontend {
    request_id: String,
    session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionItem {
    pub id: String,
    pub question: String,
    pub options: Vec<QuestionOptionFrontend>,
    pub multi_select: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOptionFrontend {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionMode {
    #[default]
    Default,
    Plan,
}

/// Frontend → bridge outcome for an `ask_user_question` reverse-request.
///
/// The wire format sent back to EchoAgent must match
/// `AskUserQuestionExtResponse` (internally tagged on `"outcome"`,
/// snake_case variants): e.g. `{"outcome":"accepted","answers":{...}}`.
pub enum QuestionOutcome {
    /// User accepted. `answers` is keyed by **question text** (not id);
    /// values are selected option labels (or `"Other"` for freeform).
    /// `annotations` carries freeform notes / previews keyed the same way.
    Accepted {
        answers: HashMap<String, Vec<String>>,
        annotations: Option<HashMap<String, QuestionAnnotation>>,
    },
    ChatAboutThis {
        partial_answers: HashMap<String, String>,
    },
    SkipInterview {
        partial_answers: HashMap<String, String>,
    },
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestionAnnotation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionFrontend {
    pub request_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_kind: String,
    pub title: String,
    pub raw_input: Option<Value>,
    pub options: Vec<PermissionOptionFrontend>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionClosedFrontend {
    pub request_id: String,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOptionFrontend {
    pub option_id: String,
    pub kind: String,
    pub title: String,
}

pub enum PermissionOutcome {
    Selected(String), // optionId
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanApprovalFrontend {
    pub request_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_content: Option<String>,
}

pub enum PlanApprovalOutcome {
    Approved,
    Cancelled { feedback: Option<String> },
    Abandoned,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderTrustFrontend {
    pub request_id: String,
    pub session_id: String,
    pub cwd: String,
    pub workspace: String,
    pub config_kinds: Vec<String>,
}

pub enum FolderTrustOutcome {
    Trust,
    Reject,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanApprovalWireRequest {
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    plan_content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FolderTrustWireRequest {
    session_id: String,
    cwd: String,
    workspace: String,
    #[serde(default)]
    config_kinds: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionWireRequest {
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    title: Option<String>,
    questions: Vec<QuestionWireItem>,
    #[serde(default)]
    mode: QuestionMode,
    // The Runtime propagates the tool's wait budget as `timeoutSecs`. Accept
    // the legacy `timeout` spelling too so the renderer always shows the same
    // authoritative countdown as the pending backend request.
    #[serde(default, alias = "timeoutSecs")]
    timeout: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionWireItem {
    #[serde(default)]
    id: Option<String>,
    question: String,
    #[serde(default, alias = "answers")]
    options: Vec<QuestionWireOption>,
    #[serde(default, alias = "multi_select")]
    multi_select: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum QuestionWireOption {
    Label(String),
    Detailed {
        #[serde(default)]
        id: Option<String>,
        #[serde(alias = "option")]
        label: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        preview: Option<String>,
    },
}

/// Replayable snapshot used when the renderer subscribes after an interaction
/// was emitted. Vectors retain arrival order, so each per-session UI can queue
/// deterministically instead of overwriting an older request.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInteractionsFrontend {
    pub permissions: Vec<PermissionFrontend>,
    pub questions: Vec<QuestionFrontend>,
    pub plan_approvals: Vec<PlanApprovalFrontend>,
    pub folder_trust_requests: Vec<FolderTrustFrontend>,
}

impl Permissions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cheap clone (inner is `Arc<Mutex<..>>`). Used to hand the registry
    /// to the dispatcher from a `State<Permissions>` without moving it out.
    pub fn share(&self) -> Permissions {
        Permissions {
            inner: self.inner.clone(),
        }
    }

    /// Register a pending permission; returns the id and the receiver the
    /// dispatcher awaits (then forwards back to the agent).
    pub async fn register(
        &self,
        request: PermissionFrontend,
    ) -> Result<oneshot::Receiver<PermissionOutcome>, &'static str> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.inner.lock().await;
        if pending.len() >= MAX_PENDING_INTERACTIONS {
            return Err("too many pending permission requests");
        }
        pending.push(PendingPermission {
            request,
            response_tx: tx,
        });
        Ok(rx)
    }

    /// Called by the `agent_resolve_permission` command.
    pub async fn resolve(
        &self,
        id: &str,
        outcome: PermissionOutcome,
    ) -> Option<PermissionClosedFrontend> {
        let mut pending = self.inner.lock().await;
        if let Some(index) = pending
            .iter()
            .position(|entry| entry.request.request_id == id)
        {
            let entry = pending.remove(index);
            let notice = PermissionClosedFrontend {
                request_id: entry.request.request_id.clone(),
                session_id: entry.request.session_id.clone(),
            };
            let _ = entry.response_tx.send(outcome);
            Some(notice)
        } else {
            None
        }
    }

    pub async fn discard(&self, id: &str) -> Option<PermissionClosedFrontend> {
        let mut pending = self.inner.lock().await;
        let index = pending
            .iter()
            .position(|entry| entry.request.request_id == id)?;
        let entry = pending.remove(index);
        Some(PermissionClosedFrontend {
            request_id: entry.request.request_id,
            session_id: entry.request.session_id,
        })
    }

    pub async fn list(&self, session_id: Option<&str>) -> Vec<PermissionFrontend> {
        self.inner
            .lock()
            .await
            .iter()
            .filter(|entry| session_id.is_none_or(|sid| entry.request.session_id == sid))
            .map(|entry| entry.request.clone())
            .collect()
    }

    pub async fn cancel_all(&self) -> Vec<PermissionClosedFrontend> {
        let entries = std::mem::take(&mut *self.inner.lock().await);
        let mut closed = Vec::with_capacity(entries.len());
        for entry in entries {
            closed.push(PermissionClosedFrontend {
                request_id: entry.request.request_id.clone(),
                session_id: entry.request.session_id.clone(),
            });
            let _ = entry.response_tx.send(PermissionOutcome::Cancelled);
        }
        closed
    }
}

impl Questions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn share(&self) -> Questions {
        Questions {
            inner: self.inner.clone(),
        }
    }

    pub async fn register(
        &self,
        request: QuestionFrontend,
    ) -> Result<oneshot::Receiver<QuestionOutcome>, &'static str> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.inner.lock().await;
        if pending.len() >= MAX_PENDING_INTERACTIONS {
            return Err("too many pending question requests");
        }
        pending.push(PendingQuestion {
            request,
            response_tx: tx,
        });
        Ok(rx)
    }

    pub async fn resolve(&self, id: &str, outcome: QuestionOutcome) -> bool {
        let mut pending = self.inner.lock().await;
        if let Some(index) = pending
            .iter()
            .position(|entry| entry.request.request_id == id)
        {
            let entry = pending.remove(index);
            let _ = entry.response_tx.send(outcome);
            true
        } else {
            false
        }
    }

    pub async fn discard(&self, id: &str) {
        self.inner
            .lock()
            .await
            .retain(|entry| entry.request.request_id != id);
    }

    pub async fn list(&self, session_id: Option<&str>) -> Vec<QuestionFrontend> {
        self.inner
            .lock()
            .await
            .iter()
            .filter(|entry| session_id.is_none_or(|sid| entry.request.session_id == sid))
            .map(|entry| entry.request.clone())
            .collect()
    }

    pub async fn cancel_all(&self) {
        let entries = std::mem::take(&mut *self.inner.lock().await);
        for entry in entries {
            let _ = entry.response_tx.send(QuestionOutcome::Cancelled);
        }
    }
}

impl PlanApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn share(&self) -> Self {
        self.clone()
    }

    pub async fn register(
        &self,
        request: PlanApprovalFrontend,
    ) -> Result<oneshot::Receiver<PlanApprovalOutcome>, &'static str> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.inner.lock().await;
        if pending.len() >= MAX_PENDING_INTERACTIONS {
            return Err("too many pending plan approval requests");
        }
        pending.push(PendingPlanApproval {
            request,
            response_tx: tx,
        });
        Ok(rx)
    }

    pub async fn resolve(&self, id: &str, outcome: PlanApprovalOutcome) -> bool {
        let mut pending = self.inner.lock().await;
        if let Some(index) = pending
            .iter()
            .position(|entry| entry.request.request_id == id)
        {
            let entry = pending.remove(index);
            let _ = entry.response_tx.send(outcome);
            true
        } else {
            false
        }
    }

    pub async fn discard(&self, id: &str) {
        self.inner
            .lock()
            .await
            .retain(|entry| entry.request.request_id != id);
    }

    pub async fn list(&self, session_id: Option<&str>) -> Vec<PlanApprovalFrontend> {
        self.inner
            .lock()
            .await
            .iter()
            .filter(|entry| session_id.is_none_or(|sid| entry.request.session_id == sid))
            .map(|entry| entry.request.clone())
            .collect()
    }

    pub async fn cancel_all(&self) {
        let entries = std::mem::take(&mut *self.inner.lock().await);
        for entry in entries {
            let _ = entry
                .response_tx
                .send(PlanApprovalOutcome::Cancelled { feedback: None });
        }
    }
}

impl FolderTrusts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn share(&self) -> Self {
        self.clone()
    }

    pub async fn register(
        &self,
        request: FolderTrustFrontend,
    ) -> Result<oneshot::Receiver<FolderTrustOutcome>, &'static str> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.inner.lock().await;
        if pending.len() >= MAX_PENDING_INTERACTIONS {
            return Err("too many pending folder trust requests");
        }
        pending.push(PendingFolderTrust {
            request,
            response_tx: tx,
        });
        Ok(rx)
    }

    pub async fn resolve(&self, id: &str, outcome: FolderTrustOutcome) -> bool {
        let mut pending = self.inner.lock().await;
        if let Some(index) = pending
            .iter()
            .position(|entry| entry.request.request_id == id)
        {
            let entry = pending.remove(index);
            let _ = entry.response_tx.send(outcome);
            true
        } else {
            false
        }
    }

    pub async fn discard(&self, id: &str) {
        self.inner
            .lock()
            .await
            .retain(|entry| entry.request.request_id != id);
    }

    pub async fn list(&self, session_id: Option<&str>) -> Vec<FolderTrustFrontend> {
        self.inner
            .lock()
            .await
            .iter()
            .filter(|entry| session_id.is_none_or(|sid| entry.request.session_id == sid))
            .map(|entry| entry.request.clone())
            .collect()
    }

    pub async fn cancel_all(&self) {
        let entries = std::mem::take(&mut *self.inner.lock().await);
        for entry in entries {
            let _ = entry.response_tx.send(FolderTrustOutcome::Reject);
        }
    }
}

pub fn new_interaction_id() -> String {
    Uuid::now_v7().to_string()
}

fn validate_required_text(value: &str, max_chars: usize, field: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} is required"));
    }
    if value.chars().count() > max_chars || value.chars().any(|character| character == '\0') {
        return Err(format!("{field} is invalid or too long"));
    }
    Ok(())
}

fn optional_display_text(value: Option<String>, max_chars: usize) -> Option<String> {
    value
        .map(|value| bounded_display_text(&value, max_chars))
        .filter(|value| !value.trim().is_empty())
}

fn bounded_display_text(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut truncated = false;
    let mut chars = 0usize;
    for character in value.chars() {
        if character == '\0' {
            truncated = true;
            continue;
        }
        if chars >= max_chars {
            truncated = true;
            break;
        }
        output.push(character);
        chars = chars.saturating_add(1);
    }
    if truncated {
        output.push_str("\n[truncated]");
    }
    output
}

fn capped_json_for_frontend(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let mut budget = MAX_PERMISSION_RAW_INPUT_BYTES;
    let mut truncated = false;
    let candidate = bounded_json_value(value, &mut budget, 0, &mut truncated);
    if !truncated {
        if let Ok(encoded) = serde_json::to_vec(&candidate) {
            if encoded.len() <= MAX_PERMISSION_RAW_INPUT_BYTES {
                return Some(candidate);
            }
        }
    }
    Some(serde_json::json!({
        "_truncated": true,
        "message": "Permission input omitted because it exceeded the UI safety limit"
    }))
}

fn bounded_json_value(
    value: &Value,
    budget: &mut usize,
    depth: usize,
    truncated: &mut bool,
) -> Value {
    if *budget == 0 || depth >= 8 {
        *truncated = true;
        return Value::String("[truncated]".into());
    }
    match value {
        Value::Null => Value::Null,
        Value::Bool(value) => {
            *budget = budget.saturating_sub(5);
            Value::Bool(*value)
        }
        Value::Number(value) => {
            *budget = budget.saturating_sub(value.to_string().len());
            Value::Number(value.clone())
        }
        Value::String(value) => {
            let max_chars = MAX_INTERACTION_TEXT_CHARS.min(*budget);
            let bounded = bounded_display_text(value, max_chars);
            if bounded.len() < value.len() {
                *truncated = true;
            }
            *budget = budget.saturating_sub(bounded.len());
            Value::String(bounded)
        }
        Value::Array(items) => {
            if items.len() > 64 {
                *truncated = true;
            }
            Value::Array(
                items
                    .iter()
                    .take(64)
                    .map(|item| bounded_json_value(item, budget, depth + 1, truncated))
                    .collect(),
            )
        }
        Value::Object(items) => {
            if items.len() > 64 {
                *truncated = true;
            }
            let mut out = serde_json::Map::new();
            for (key, item) in items.iter().take(64) {
                let key = bounded_display_text(key, MAX_INTERACTION_TITLE_CHARS);
                *budget = budget.saturating_sub(key.len());
                out.insert(key, bounded_json_value(item, budget, depth + 1, truncated));
            }
            Value::Object(out)
        }
    }
}

fn question_frontend_from_wire(request: QuestionWireRequest) -> Result<QuestionFrontend, String> {
    validate_required_text(
        &request.session_id,
        MAX_INTERACTION_ID_CHARS,
        "question sessionId",
    )?;
    validate_required_text(
        &request.tool_call_id,
        MAX_INTERACTION_ID_CHARS,
        "question toolCallId",
    )?;
    if request.questions.is_empty() || request.questions.len() > MAX_QUESTION_ITEMS {
        return Err(format!(
            "question request must contain 1..={MAX_QUESTION_ITEMS} questions"
        ));
    }

    let mut items = Vec::with_capacity(request.questions.len());
    for (index, question) in request.questions.into_iter().enumerate() {
        validate_required_text(
            &question.question,
            MAX_INTERACTION_TEXT_CHARS,
            "question text",
        )?;
        if question.options.len() > MAX_QUESTION_OPTIONS {
            return Err(format!(
                "question options cannot exceed {MAX_QUESTION_OPTIONS}"
            ));
        }
        let mut options = Vec::with_capacity(question.options.len());
        for option in question.options {
            let option = match option {
                QuestionWireOption::Label(label) => {
                    validate_required_text(
                        &label,
                        MAX_QUESTION_OPTION_TEXT_CHARS,
                        "question option label",
                    )?;
                    QuestionOptionFrontend {
                        id: None,
                        label,
                        description: String::new(),
                        preview: None,
                    }
                }
                QuestionWireOption::Detailed {
                    id,
                    label,
                    description,
                    preview,
                } => {
                    validate_required_text(
                        &label,
                        MAX_QUESTION_OPTION_TEXT_CHARS,
                        "question option label",
                    )?;
                    let id = id.filter(|id| {
                        !id.trim().is_empty()
                            && id.chars().count() <= MAX_INTERACTION_ID_CHARS
                            && !id.chars().any(|character| character == '\0')
                    });
                    QuestionOptionFrontend {
                        id,
                        label,
                        description: bounded_display_text(
                            &description,
                            MAX_INTERACTION_TITLE_CHARS,
                        ),
                        preview: optional_display_text(preview, MAX_INTERACTION_TITLE_CHARS),
                    }
                }
            };
            options.push(option);
        }
        let id = question
            .id
            .filter(|id| {
                !id.trim().is_empty()
                    && id.chars().count() <= MAX_INTERACTION_ID_CHARS
                    && !id.chars().any(|character| character == '\0')
            })
            .unwrap_or_else(|| format!("q-{index}"));
        items.push(QuestionItem {
            id,
            question: question.question,
            options,
            multi_select: question.multi_select.unwrap_or(false),
        });
    }

    let title = optional_display_text(request.title, MAX_INTERACTION_TITLE_CHARS)
        .or_else(|| items.first().map(|item| item.question.clone()))
        .unwrap_or_else(|| "Agent 提问".into());

    Ok(QuestionFrontend {
        request_id: new_interaction_id(),
        session_id: request.session_id,
        tool_call_id: request.tool_call_id,
        title,
        questions: items,
        mode: request.mode,
        timeout: request
            .timeout
            .map(|timeout| timeout.min(MAX_QUESTION_TIMEOUT_SECS)),
    })
}

fn plan_frontend_from_wire(
    request: PlanApprovalWireRequest,
) -> Result<PlanApprovalFrontend, String> {
    validate_required_text(
        &request.session_id,
        MAX_INTERACTION_ID_CHARS,
        "plan sessionId",
    )?;
    validate_required_text(
        &request.tool_call_id,
        MAX_INTERACTION_ID_CHARS,
        "plan toolCallId",
    )?;
    Ok(PlanApprovalFrontend {
        request_id: new_interaction_id(),
        session_id: request.session_id,
        tool_call_id: request.tool_call_id,
        plan_content: optional_display_text(request.plan_content, MAX_PERMISSION_RAW_INPUT_BYTES),
    })
}

fn folder_trust_frontend_from_wire(
    request: FolderTrustWireRequest,
) -> Result<FolderTrustFrontend, String> {
    validate_required_text(
        &request.session_id,
        MAX_INTERACTION_ID_CHARS,
        "folder trust sessionId",
    )?;
    validate_required_text(
        &request.cwd,
        MAX_FOLDER_TRUST_PATH_CHARS,
        "folder trust cwd",
    )?;
    validate_required_text(
        &request.workspace,
        MAX_FOLDER_TRUST_PATH_CHARS,
        "folder trust workspace",
    )?;
    if request.config_kinds.len() > MAX_FOLDER_TRUST_CONFIG_KINDS {
        return Err(format!(
            "folder trust configKinds cannot exceed {MAX_FOLDER_TRUST_CONFIG_KINDS}"
        ));
    }
    for kind in &request.config_kinds {
        validate_required_text(
            kind,
            MAX_INTERACTION_TITLE_CHARS,
            "folder trust config kind",
        )?;
    }
    Ok(FolderTrustFrontend {
        request_id: new_interaction_id(),
        session_id: request.session_id,
        cwd: request.cwd,
        workspace: request.workspace,
        config_kinds: request.config_kinds,
    })
}

pub async fn pending_interactions(
    permissions: &Permissions,
    questions: &Questions,
    plan_approvals: &PlanApprovals,
    folder_trusts: &FolderTrusts,
    session_id: Option<&str>,
) -> PendingInteractionsFrontend {
    let (permissions, questions, plan_approvals, folder_trust_requests) = tokio::join!(
        permissions.list(session_id),
        questions.list(session_id),
        plan_approvals.list(session_id),
        folder_trusts.list(session_id),
    );
    PendingInteractionsFrontend {
        permissions,
        questions,
        plan_approvals,
        folder_trust_requests,
    }
}

/// Payload emitted on the `agent://update` event — the raw SessionUpdate JSON,
/// plus the session id it belongs to (so the frontend can route updates for
/// side-channel sessions like inspiration generation away from the main
/// transcript store).
#[derive(Clone, Debug)]
pub struct UpdateEvent {
    /// Session id this update belongs to. `None` only if EchoAgent omitted it
    /// (shouldn't happen for SessionNotification). When present, the frontend
    /// checks it against the current session before applying.
    pub session_id: Option<String>,
    /// Preserve ACP metadata needed for replay-aware timing and terminal copy.
    pub meta: Option<Value>,
    pub update: Value,
}

impl Serialize for UpdateEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut payload = self
            .update
            .as_object()
            .cloned()
            .ok_or_else(|| serde::ser::Error::custom("Session update must be a JSON object"))?;
        let nested_meta = payload.remove("_meta");
        let meta = match (&self.meta, nested_meta) {
            (Some(Value::Object(outer)), Some(Value::Object(mut inner))) => {
                // Notification metadata owns replay/timing identity. The update
                // also carries prompt/model metadata; emit one combined key so
                // JSON consumers cannot silently discard either envelope.
                inner.extend(outer.clone());
                Some(Value::Object(inner))
            }
            (Some(outer), _) => Some(outer.clone()),
            (None, inner) => inner,
        };
        if let Some(meta) = meta {
            payload.insert("_meta".into(), meta);
        }
        if let Some(session_id) = &self.session_id {
            payload.insert("sessionId".into(), Value::String(session_id.clone()));
        }
        payload.serialize(serializer)
    }
}

/// Payload emitted on `agent://complete`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteEvent {
    pub session_id: String,
    pub prompt_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<u64>,
    pub stop_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancellation_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_result: Option<String>,
}

/// Exact per-prompt usage carried by EchoAgent's durable `TurnCompleted`
/// session update. Unlike `session/usage`, this is not a process-local
/// cumulative counter, so the frontend can persist and replay it idempotently
/// without assigning an old session's history to the current day.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnUsageEvent {
    pub session_id: String,
    pub prompt_id: String,
    pub usage: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

/// Payload emitted on `agent://turn-error` — a turn that ended abnormally
/// (`stopReason: "rate_limit" | "error"`). EchoAgent reports mid-stream failures
/// (e.g. a 429 hit while a tool was running) via `prompt_complete` with these
/// stop reasons rather than as a thrown error, so without this event the
/// frontend would silently mark the turn "complete" and the user would see no
/// explanation for why the agent stopped mid-task.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnErrorEvent {
    pub session_id: String,
    pub prompt_id: String,
    /// "rate_limit" | "error" (mirrors EchoAgent's `stop_reason_for_turn_error`).
    pub kind: String,
    /// Server-provided detail string (null for rate_limit — EchoAgent deliberately
    /// omits it so the client shows its own message). We forward it verbatim
    /// when present; the frontend decides how to render.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Payload emitted on `agent://summary` — a freshly generated (or manually
/// renamed) session title. The frontend updates the sidebar entry in place.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryEvent {
    pub session_id: String,
    pub title: String,
}

/// Payload emitted on `agent://subagent` — a live or replayed subagent lifecycle event
/// (spawned / progress / finished). EchoAgent sends these as
/// `echo.agent/session_notification` (live) or `echo.agent/session/update`
/// (durable replay) extension notifications addressed to the parent session.
/// We forward both identity/turn provenance and progress/result fields so the
/// frontend can reconstruct the complete history after a session reload.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentEvent {
    /// Parent session that owns the subagent.
    pub session_id: String,
    /// Lifecycle phase: "spawned" | "progress" | "finished".
    pub phase: String,
    /// Subagent unique id (= child session id).
    pub subagent_id: String,
    /// Child session's ACP session id (same as subagent_id for spawned/progress).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    /// Parent prompt/turn that created this subagent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_prompt_id: Option<String>,
    /// Human-readable description / task title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Agent type ("general-purpose", "explore", "plan", etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    /// Effective model used by the child session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Named persona and role, when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Context/capability provenance needed to audit how the child was run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_context_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_normalized: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
    /// Status: "running" (spawned/progress) or the finished status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Elapsed wall-clock time in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Number of completed turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_count: Option<u32>,
    /// Total tool calls so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_count: Option<u32>,
    /// Current tokens used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    /// Context window capacity in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u64>,
    /// Context window usage percentage (0-100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_usage_pct: Option<u8>,
    /// Distinct tool names called so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_used: Option<Vec<String>>,
    /// Number of errors observed by progress reporting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<u32>,
    /// Error message (finished only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Final output text (finished only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Durable event timestamp and replay marker from EchoAgent `_meta`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<i64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_replay: bool,
}

/// EchoAgent deliberately uses a distinct method for durable session replay.
/// Keep this parity check centralized so adding a second rail cannot silently
/// regress one event family (subagents, usage, titles, etc.).
fn is_echo_agent_session_update_method(method: &str) -> bool {
    matches!(
        method,
        "echo.agent/session_notification" | "echo.agent/session/update"
    )
}

/// Spawn the dispatcher that forwards agent→client messages to the frontend.
pub fn spawn_dispatcher(
    app: AppHandle,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AcpClientMessage>,
    permissions: Permissions,
    questions: Questions,
    plan_approvals: PlanApprovals,
    folder_trusts: FolderTrusts,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(msg) = rx.recv().await {
            handle_client_message(
                &app,
                msg,
                &permissions,
                &questions,
                &plan_approvals,
                &folder_trusts,
            )
            .await;
        }
        tracing::info!("EchoAgent agent channel closed");
    });
}

async fn handle_client_message(
    app: &AppHandle,
    msg: AcpClientMessage,
    perms: &Permissions,
    questions: &Questions,
    plan_approvals: &PlanApprovals,
    folder_trusts: &FolderTrusts,
) {
    match msg {
        AcpClientMessage::SessionNotification(b) => {
            let update = serialize_session_update(&b.request.update);
            let sid = b.request.session_id.0.as_ref().to_string();
            crate::coding_review_runtime::observe(
                &sid,
                &update,
                b.request
                    .meta
                    .as_ref()
                    .and_then(|meta| serde_json::to_value(meta).ok())
                    .as_ref(),
            );
            crate::groups::observe(
                &sid,
                &update,
                b.request
                    .meta
                    .as_ref()
                    .and_then(|m| serde_json::to_value(m).ok())
                    .as_ref(),
            );
            // RED evidence comes from the native Runtime channel, not rendered
            // tool-card text or a client-supplied exit code. Resolve the session
            // through its native workspace capability before touching task data.
            if matches!(
                update.get("sessionUpdate").and_then(Value::as_str),
                Some("tool_call" | "tool_call_update")
            ) {
                let root = app
                    .try_state::<crate::commands::AppState>()
                    .and_then(|state| state.session_workspace(&sid).ok())
                    .and_then(|root| {
                        app.try_state::<crate::shell_fs::FilesystemAccess>()
                            .and_then(|access| {
                                access.require_workspace(&root.to_string_lossy()).ok()
                            })
                    });
                if let Some(root) = root {
                    let meta = b
                        .request
                        .meta
                        .as_ref()
                        .and_then(|meta| serde_json::to_value(meta).ok());
                    if let Err(error) = crate::coding::tdd::observe_runtime_update(
                        &root,
                        &sid,
                        &update,
                        meta.as_ref(),
                    )
                    .await
                    {
                        tracing::warn!(%error, session_id = %sid, "native Runtime RED capture rejected");
                    }
                }
            }
            tracing::debug!(session_id = %sid, update = %update, "agent://update");
            let _ = app.emit(
                "agent://update",
                UpdateEvent {
                    session_id: Some(sid),
                    meta: b
                        .request
                        .meta
                        .as_ref()
                        .and_then(|meta| serde_json::to_value(meta).ok()),
                    update,
                },
            );
            // ACK so the agent's notification future completes.
            let _ = b.response_tx.send(Ok(()));
        }
        AcpClientMessage::RequestPermission(b) => {
            let req = &b.request;
            let session_id_str = req.session_id.0.as_ref().to_string();
            if let Err(error) = validate_required_text(
                &session_id_str,
                MAX_INTERACTION_ID_CHARS,
                "permission sessionId",
            ) {
                tracing::warn!(%error, "malformed permission request; cancelling fail-closed");
                send_permission_cancelled(b.response_tx);
                return;
            }
            if req.options.is_empty() || req.options.len() > MAX_PERMISSION_OPTIONS {
                tracing::warn!(
                    options = req.options.len(),
                    "invalid permission options; cancelling fail-closed"
                );
                send_permission_cancelled(b.response_tx);
                return;
            }

            let mut options = Vec::with_capacity(req.options.len());
            for o in &req.options {
                let option_id = o.option_id.0.as_ref().to_string();
                if let Err(error) = validate_required_text(
                    &option_id,
                    MAX_INTERACTION_ID_CHARS,
                    "permission optionId",
                ) {
                    tracing::warn!(%error, "malformed permission request; cancelling fail-closed");
                    send_permission_cancelled(b.response_tx);
                    return;
                }
                options.push(PermissionOptionFrontend {
                    option_id,
                    kind: permission_kind_str(&o.kind).to_string(),
                    title: bounded_display_text(&o.name, MAX_INTERACTION_TITLE_CHARS),
                });
            }

            // Auto-approve: if the permission mode is "always-approve", pick the
            // first allow/allow_always option and respond immediately.
            let task_always = crate::permission_config::is_session_permission_mode_active(
                &session_id_str,
                "always-approve",
            );
            let automation_full_access =
                crate::automations::is_full_access_session(&session_id_str)
                    && crate::permission_config::ensure_always_approve_available().is_ok();
            if task_always || automation_full_access {
                let auto_option = preferred_allow_option(&options);
                if let Some(opt) = auto_option {
                    let response = acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Selected(
                            acp::SelectedPermissionOutcome::new(acp::PermissionOptionId::new(
                                Arc::from(opt.option_id.as_str()),
                            )),
                        ),
                    );
                    tracing::info!(session_id = %session_id_str, "auto-approved permission (always-approve mode)");
                    let _ = b.response_tx.send(Ok(response));
                    return;
                }
                // 没有 allow 选项时不静默选择 first（那可能是 deny，会让工具莫名失败）。
                // 改为回退到下方正常的人工审批流程，把决定权交给用户。
                tracing::info!(
                    session_id = %session_id_str,
                    "always-approve mode but no allow option present — falling back to manual approval"
                );
            }

            // Computer tools already have a session-bound desktop consent gate.
            // Do not ask once in Runtime and again inside that same service.
            if is_builtin_computer_permission(&req.tool_call)
                && crate::automation::manages_computer_permissions(&session_id_str).await
            {
                if let Some(opt) = options.iter().find(|option| option.kind == "allow") {
                    let response = acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Selected(
                            acp::SelectedPermissionOutcome::new(acp::PermissionOptionId::new(
                                Arc::from(opt.option_id.as_str()),
                            )),
                        ),
                    );
                    tracing::info!(session_id = %session_id_str, "delegated computer permission to desktop consent gate");
                    let _ = b.response_tx.send(Ok(response));
                    return;
                }
            }

            // These two first-party MCP tools only query fixed public endpoints.
            // The runtime still applies explicit deny rules before requesting
            // permission; ordinary conversations should not require a click for
            // each weather or search lookup.
            if is_builtin_live_info_permission(&req.tool_call) {
                if let Some(opt) = options.iter().find(|option| option.kind == "allow") {
                    let response = acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Selected(
                            acp::SelectedPermissionOutcome::new(acp::PermissionOptionId::new(
                                Arc::from(opt.option_id.as_str()),
                            )),
                        ),
                    );
                    tracing::info!(session_id = %session_id_str, "auto-approved built-in live information lookup");
                    let _ = b.response_tx.send(Ok(response));
                    return;
                }
            }

            // Extract tool metadata from the ACP ToolCallUpdate so the frontend
            // can display the tool kind, title, and raw input parameters.
            let tool_call_id = req.tool_call.tool_call_id.0.as_ref().to_string();
            if let Err(error) = validate_required_text(
                &tool_call_id,
                MAX_INTERACTION_ID_CHARS,
                "permission toolCallId",
            ) {
                tracing::warn!(%error, "malformed permission request; cancelling fail-closed");
                send_permission_cancelled(b.response_tx);
                return;
            }
            let tool_kind = req
                .tool_call
                .fields
                .kind
                .as_ref()
                .map(|k| format!("{k:?}").to_lowercase())
                .unwrap_or_default();
            let title = req
                .tool_call
                .fields
                .title
                .clone()
                .or_else(|| options.first().map(|o| o.title.clone()))
                .unwrap_or_else(|| "permission".into());
            let raw_input = capped_json_for_frontend(req.tool_call.fields.raw_input.as_ref());

            let frontend = PermissionFrontend {
                request_id: new_interaction_id(),
                session_id: session_id_str.clone(),
                tool_call_id,
                tool_kind: bounded_display_text(&tool_kind, MAX_INTERACTION_TITLE_CHARS),
                title: bounded_display_text(&title, MAX_INTERACTION_TITLE_CHARS),
                raw_input,
                options,
            };
            let request_id = frontend.request_id.clone();
            let rx = match perms.register(frontend.clone()).await {
                Ok(rx) => rx,
                Err(error) => {
                    tracing::warn!(%error, "permission queue full; cancelling fail-closed");
                    send_permission_cancelled(b.response_tx);
                    return;
                }
            };
            // Close the race where this handler observed Ask immediately
            // before this exact session acknowledged Always. Once registered,
            // re-check the task-owned mode; requests created before the switch
            // stay parked because the switch itself never drains the queue.
            let late_auto_option =
                if crate::permission_config::is_session_permission_mode_active(
                    &session_id_str,
                    "always-approve",
                ) || (crate::automations::is_full_access_session(&session_id_str)
                    && crate::permission_config::ensure_always_approve_available().is_ok())
                {
                    preferred_allow_option(&frontend.options).map(|option| option.option_id.clone())
                } else {
                    None
                };
            if let Some(option_id) = late_auto_option {
                if let Some(notice) = perms
                    .resolve(&request_id, PermissionOutcome::Selected(option_id))
                    .await
                {
                    emit_permission_closed(app, notice);
                }
                tracing::info!(session_id = %session_id_str, "auto-approved permission after mode-switch race");
            } else {
                let emitted = app.emit("agent://permission", frontend.clone()).is_ok();
                if !emitted {
                    // If the native event channel itself is unavailable, never
                    // leave the agent parked on an interaction nobody can see.
                    if let Some(notice) = perms
                        .resolve(&request_id, PermissionOutcome::Cancelled)
                        .await
                    {
                        emit_permission_closed(app, notice);
                    }
                }
                if emitted {
                    let notify_app = app.clone();
                    let notify_session = session_id_str.clone();
                    let notify_request = request_id.clone();
                    let notify_permissions = perms.clone();
                    tokio::spawn(async move {
                        let lookup_session = notify_session.clone();
                        let lookup_request = notify_request.clone();
                        let Ok((title, notification_id)) = tokio::task::spawn_blocking(move || {
                            let title =
                                crate::notifications::task_title_for_notification(&lookup_session)
                                    .as_deref()
                                    .map(|name| format!("任务《{name}》需要授权"))
                                    .unwrap_or_else(|| "有任务需要授权".into());
                            let notification_id = crate::notifications::append_with_target(
                                crate::notifications::NotificationKind::Permission,
                                &title,
                                Some("打开任务查看请求并决定是否允许"),
                                Some(&lookup_session),
                                Some(&lookup_request),
                                None,
                                "warn",
                            )
                            .ok();
                            (title, notification_id)
                        })
                        .await
                        else {
                            tracing::error!("permission notification preparation task failed");
                            return;
                        };
                        if !notify_permissions
                            .list(Some(&notify_session))
                            .await
                            .iter()
                            .any(|pending| pending.request_id == notify_request)
                        {
                            return;
                        }
                        let _ = crate::notifications::dispatch_with_desktop_fallback(
                            &notify_app,
                            crate::notifications::NotifyMessage {
                                title,
                                body: Some("打开任务查看请求并决定是否允许".into()),
                                level: "warn".into(),
                                session_id: Some(notify_session),
                                request_id: Some(notify_request),
                                automation_id: None,
                                notification_id,
                                hidden_title: Some("任务需要授权".into()),
                                background_only: true,
                            },
                        )
                        .await;
                    });
                }
            }

            // Do not block the dispatcher: another session's updates and
            // interactions must continue flowing while this decision is open.
            let registry = perms.clone();
            let close_app = app.clone();
            let mut response_tx = b.response_tx;
            tokio::spawn(async move {
                let outcome = tokio::select! {
                    result = rx => result.unwrap_or(PermissionOutcome::Cancelled),
                    () = response_tx.closed() => {
                        if let Some(notice) = registry.discard(&request_id).await {
                            emit_permission_closed(&close_app, notice);
                        }
                        return;
                    }
                };
                let response = match outcome {
                    PermissionOutcome::Selected(option_id) => acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Selected(
                            acp::SelectedPermissionOutcome::new(acp::PermissionOptionId::new(
                                Arc::from(option_id.as_str()),
                            )),
                        ),
                    ),
                    PermissionOutcome::Cancelled => acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Cancelled,
                    ),
                };
                let _ = response_tx.send(Ok(response));
            });
        }
        AcpClientMessage::ExtNotification(b) => {
            let method = b.request.method.as_ref().to_string();
            // params is a RawValue on the wire; deserialize to extract fields.
            let raw_str = b.request.params.get();
            let params: Value = serde_json::from_str(raw_str).unwrap_or(Value::Null);
            if method == "echo.agent/session/prompt_complete" {
                let complete = parse_complete_event(&params);
                // The legacy broadcast has no prompt identity. The durable
                // turn_completed rail below carries the Runtime's admitted
                // identity even for Chat/WeiXin callers that omitted one.
                // A session-only ending cannot settle a replacement turn.
                if !complete.prompt_id.is_empty() {
                    handle_prompt_complete(app, complete).await;
                }
            } else if is_echo_agent_session_update_method(&method) {
                // Session-scoped notification: EchoAgent uses this to push
                // `SessionSummaryGenerated` after the first user prompt (the
                // LLM-generated title). The update is a tagged enum with the
                // wire field name `sessionUpdate` (see notification.rs:359).
                tracing::info!(
                    method,
                    "received ext notification, dispatching to handle_session_notification"
                );
                handle_session_notification(app, &params).await;
            } else if method == "echo.agent/mcp/server_status"
                || method == "echo.agent/mcp/init_progress"
            {
                // MCP connector status / startup progress — surface to the
                // connectors panel for live state updates.
                let _ = app.emit("agent://mcp-status", &params);
            } else if method == "echo.agent/toggle_plan_mode" {
                // Plan mode toggled (either by us or by EchoAgent). Mirror to frontend.
                let _ = app.emit("agent://plan-mode", &params);
            } else if method == "echo.agent/yolo_mode_changed" {
                // Runtime policy/capability changes may be client-wide. Publish
                // only the safe new-task capability envelope here; task pickers
                // continue to read their own persisted and acknowledged state.
                let _ = app.emit(
                    "agent://permission-mode",
                    crate::permission_config::permission_mode_status(true, None),
                );
            } else if method == "echo.agent/models/update" {
                // Model list updated (e.g. after config reload).
                let _ = app.emit("agent://models-update", &params);
            } else if method == "echo.agent/task_backgrounded"
                || method == "echo.agent/task_completed"
            {
                // Background task lifecycle — refresh the tasks panel.
                let _ = app.emit("agent://task-update", &params);
            } else if method == "echo.agent/git_head_changed"
                || method == "echo.agent/gitHeadChanged"
            {
                // git HEAD moved — useful for status bar / worktree UI.
                let _ = app.emit("agent://git-head", &params);
            }
            let _ = b.response_tx.send(Ok(()));
        }
        AcpClientMessage::ReadTextFile(b) => deny_fs_terminal(b.response_tx),
        AcpClientMessage::WriteTextFile(b) => deny_fs_terminal(b.response_tx),
        AcpClientMessage::CreateTerminal(b) => deny_fs_terminal(b.response_tx),
        AcpClientMessage::TerminalOutput(b) => deny_fs_terminal(b.response_tx),
        AcpClientMessage::ReleaseTerminal(b) => deny_fs_terminal(b.response_tx),
        AcpClientMessage::WaitForTerminalExit(b) => deny_fs_terminal(b.response_tx),
        AcpClientMessage::KillTerminalCommand(b) => deny_fs_terminal(b.response_tx),
        AcpClientMessage::ExtMethod(b) => {
            let method = b.request.method.as_ref().to_string();
            let raw_str = b.request.params.get();
            match method.as_str() {
                "echo.agent/desktop_document_tool" => {
                    let params = serde_json::from_str::<Value>(raw_str);
                    let handle = app.clone();
                    tokio::spawn(async move {
                        let result = match params {
                            Ok(request) => {
                                crate::coding_document_bridge::dispatch(handle, request).await
                            }
                            Err(_) => {
                                serde_json::json!({"handled":true,"error":"Malformed document request"})
                            }
                        };
                        send_ext_json(b.response_tx, result);
                    });
                }
                "echo.agent/ask_user_question" => {
                    if raw_str.len() > MAX_EXT_METHOD_PARAMS_BYTES {
                        tracing::warn!(
                            "oversized ask_user_question request; cancelling fail-closed"
                        );
                        send_ext_json(b.response_tx, serde_json::json!({ "outcome": "cancelled" }));
                        return;
                    };
                    let request = match serde_json::from_str::<QuestionWireRequest>(raw_str) {
                        Ok(request) => request,
                        Err(error) => {
                            tracing::warn!(%error, "malformed ask_user_question request; cancelling fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "cancelled" }),
                            );
                            return;
                        }
                    };
                    let frontend = match question_frontend_from_wire(request) {
                        Ok(frontend) => frontend,
                        Err(error) => {
                            tracing::warn!(%error, "invalid ask_user_question request; cancelling fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "cancelled" }),
                            );
                            return;
                        }
                    };
                    let request_id = frontend.request_id.clone();
                    let session_id = frontend.session_id.clone();
                    let rx = match questions.register(frontend.clone()).await {
                        Ok(rx) => rx,
                        Err(error) => {
                            tracing::warn!(%error, "question queue full; cancelling fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "cancelled" }),
                            );
                            return;
                        }
                    };
                    tracing::info!(request_id, "emitting agent://question");
                    if app.emit("agent://question", frontend).is_err() {
                        let _ = questions
                            .resolve(&request_id, QuestionOutcome::Cancelled)
                            .await;
                    }

                    let registry = questions.clone();
                    let response_app = app.clone();
                    let mut response_tx = b.response_tx;
                    tokio::spawn(async move {
                        let outcome = tokio::select! {
                            result = rx => result.unwrap_or(QuestionOutcome::Cancelled),
                            () = response_tx.closed() => {
                                registry.discard(&request_id).await;
                                emit_question_closed(&response_app, &session_id, &request_id);
                                return;
                            }
                        };
                        let value = question_response_value(outcome);
                        send_ext_json(response_tx, value);
                        emit_question_closed(&response_app, &session_id, &request_id);
                    });
                }
                "echo.agent/exit_plan_mode" => {
                    if raw_str.len() > MAX_EXT_METHOD_PARAMS_BYTES {
                        tracing::warn!("oversized exit_plan_mode request; cancelling fail-closed");
                        send_ext_json(b.response_tx, serde_json::json!({ "outcome": "cancelled" }));
                        return;
                    };
                    let request = match serde_json::from_str::<PlanApprovalWireRequest>(raw_str) {
                        Ok(request) => request,
                        Err(error) => {
                            tracing::warn!(%error, "malformed exit_plan_mode request; cancelling fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "cancelled" }),
                            );
                            return;
                        }
                    };
                    let frontend = match plan_frontend_from_wire(request) {
                        Ok(frontend) => frontend,
                        Err(error) => {
                            tracing::warn!(%error, "invalid exit_plan_mode request; cancelling fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "cancelled" }),
                            );
                            return;
                        }
                    };
                    let request_id = frontend.request_id.clone();
                    let session_id = frontend.session_id.clone();
                    let rx = match plan_approvals.register(frontend.clone()).await {
                        Ok(rx) => rx,
                        Err(error) => {
                            tracing::warn!(%error, "plan approval queue full; cancelling fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "cancelled" }),
                            );
                            return;
                        }
                    };
                    emit_plan_approval(app, "plan_approval_request", &frontend);
                    if app.emit("agent://plan-approval", frontend).is_err() {
                        let _ = plan_approvals
                            .resolve(
                                &request_id,
                                PlanApprovalOutcome::Cancelled { feedback: None },
                            )
                            .await;
                    }

                    let registry = plan_approvals.clone();
                    let response_app = app.clone();
                    let mut response_tx = b.response_tx;
                    tokio::spawn(async move {
                        let outcome = tokio::select! {
                            result = rx => result.unwrap_or(PlanApprovalOutcome::Cancelled { feedback: None }),
                            () = response_tx.closed() => {
                                registry.discard(&request_id).await;
                                emit_plan_approval_closed(&response_app, &session_id, &request_id);
                                return;
                            }
                        };
                        let value = match outcome {
                            PlanApprovalOutcome::Approved => {
                                serde_json::json!({ "outcome": "approved" })
                            }
                            PlanApprovalOutcome::Cancelled { feedback } => {
                                serde_json::json!({ "outcome": "cancelled", "feedback": feedback })
                            }
                            PlanApprovalOutcome::Abandoned => {
                                serde_json::json!({ "outcome": "abandoned" })
                            }
                        };
                        send_ext_json(response_tx, value);
                        emit_plan_approval_closed(&response_app, &session_id, &request_id);
                    });
                }
                "echo.agent/folder_trust/request" => {
                    if raw_str.len() > MAX_EXT_METHOD_PARAMS_BYTES {
                        tracing::warn!("oversized folder trust request; rejecting fail-closed");
                        send_ext_json(b.response_tx, serde_json::json!({ "outcome": "reject" }));
                        return;
                    };
                    let request = match serde_json::from_str::<FolderTrustWireRequest>(raw_str) {
                        Ok(request) => request,
                        Err(error) => {
                            tracing::warn!(%error, "malformed folder trust request; rejecting fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "reject" }),
                            );
                            return;
                        }
                    };
                    let frontend = match folder_trust_frontend_from_wire(request) {
                        Ok(frontend) => frontend,
                        Err(error) => {
                            tracing::warn!(%error, "invalid folder trust request; rejecting fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "reject" }),
                            );
                            return;
                        }
                    };
                    let request_id = frontend.request_id.clone();
                    let rx = match folder_trusts.register(frontend.clone()).await {
                        Ok(rx) => rx,
                        Err(error) => {
                            tracing::warn!(%error, "folder trust queue full; rejecting fail-closed");
                            send_ext_json(
                                b.response_tx,
                                serde_json::json!({ "outcome": "reject" }),
                            );
                            return;
                        }
                    };
                    tracing::info!(request_id, "emitting agent://folder-trust");
                    if app.emit("agent://folder-trust", frontend).is_err() {
                        let _ = folder_trusts
                            .resolve(&request_id, FolderTrustOutcome::Reject)
                            .await;
                    }

                    let registry = folder_trusts.clone();
                    let mut response_tx = b.response_tx;
                    tokio::spawn(async move {
                        let outcome = tokio::select! {
                            result = rx => result.unwrap_or(FolderTrustOutcome::Reject),
                            () = response_tx.closed() => {
                                registry.discard(&request_id).await;
                                return;
                            }
                        };
                        let value = match outcome {
                            FolderTrustOutcome::Trust => serde_json::json!({ "outcome": "trust" }),
                            FolderTrustOutcome::Reject => {
                                serde_json::json!({ "outcome": "reject" })
                            }
                        };
                        send_ext_json(response_tx, value);
                    });
                }
                _ => {
                    let err = acp::Error::new(
                        acp::ErrorCode::MethodNotFound.into(),
                        format!("ext method unsupported: {method}"),
                    );
                    let _ = b.response_tx.send(Err(err));
                }
            }
        }
    }
}

fn send_ext_json(response_tx: oneshot::Sender<acp::Result<acp::ExtResponse>>, value: Value) {
    let raw = serde_json::value::to_raw_value(&value)
        .unwrap_or_else(|_| serde_json::value::to_raw_value(&Value::Null).expect("null JSON"));
    let _ = response_tx.send(Ok(acp::ExtResponse::new(raw.into())));
}

fn send_permission_cancelled(
    response_tx: tokio::sync::oneshot::Sender<acp::Result<acp::RequestPermissionResponse>>,
) {
    let response = acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled);
    let _ = response_tx.send(Ok(response));
}

fn question_response_value(outcome: QuestionOutcome) -> Value {
    match outcome {
        QuestionOutcome::Accepted {
            answers,
            annotations,
        } => {
            let mut value = serde_json::json!({
                "outcome": "accepted",
                "answers": answers,
            });
            if let Some(annotations) = annotations.filter(|items| !items.is_empty()) {
                value
                    .as_object_mut()
                    .expect("question response is an object")
                    .insert(
                        "annotations".into(),
                        serde_json::to_value(annotations).unwrap_or(Value::Null),
                    );
            }
            value
        }
        QuestionOutcome::ChatAboutThis { partial_answers } => serde_json::json!({
            "outcome": "chat_about_this",
            "partial_answers": partial_answers,
        }),
        QuestionOutcome::SkipInterview { partial_answers } => serde_json::json!({
            "outcome": "skip_interview",
            "partial_answers": partial_answers,
        }),
        QuestionOutcome::Cancelled => serde_json::json!({ "outcome": "cancelled" }),
    }
}

fn emit_question_closed(app: &AppHandle, session_id: &str, request_id: &str) {
    let _ = app.emit(
        "agent://question-closed",
        QuestionClosedFrontend {
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
        },
    );
}

fn emit_plan_approval(app: &AppHandle, update_kind: &str, request: &PlanApprovalFrontend) {
    let mut update = serde_json::to_value(request).unwrap_or(Value::Null);
    if let Some(object) = update.as_object_mut() {
        object.insert(
            "sessionUpdate".into(),
            Value::String(update_kind.to_string()),
        );
    }
    let _ = app.emit(
        "agent://update",
        UpdateEvent {
            session_id: Some(request.session_id.clone()),
            meta: None,
            update,
        },
    );
}

fn emit_plan_approval_closed(app: &AppHandle, session_id: &str, request_id: &str) {
    let _ = app.emit(
        "agent://update",
        UpdateEvent {
            session_id: Some(session_id.to_string()),
            meta: None,
            update: serde_json::json!({
                "sessionUpdate": "plan_approval_resolved",
                "requestId": request_id,
            }),
        },
    );
}

pub(crate) fn parse_complete_event(params: &Value) -> CompleteEvent {
    let meta = params.get("_meta").and_then(Value::as_object);
    let string_field = |camel: &str, snake: &str| {
        params
            .get(camel)
            .or_else(|| params.get(snake))
            .or_else(|| meta.and_then(|fields| fields.get(camel)))
            .or_else(|| meta.and_then(|fields| fields.get(snake)))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    CompleteEvent {
        session_id: string_field("sessionId", "session_id").unwrap_or_default(),
        prompt_id: string_field("promptId", "prompt_id").unwrap_or_default(),
        turn_id: params
            .get("turnId")
            .or_else(|| params.get("turn_id"))
            .and_then(Value::as_u64),
        stop_reason: string_field("stopReason", "stop_reason")
            .unwrap_or_else(|| "end_turn".to_string()),
        cancel_trigger: string_field("cancelTrigger", "cancel_trigger"),
        cancellation_category: string_field("cancellationCategory", "cancellation_category"),
        agent_result: string_field("agentResult", "agent_result"),
    }
}

fn parse_live_durable_completion(
    session_id: &str,
    params: &Value,
    update: &Value,
) -> Option<CompleteEvent> {
    if params
        .get("_meta")
        .and_then(|meta| meta.get("isReplay"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return None;
    }
    let mut payload = update.as_object()?.clone();
    payload.insert("sessionId".into(), Value::String(session_id.into()));
    if let Some(meta) = params.get("_meta") {
        payload.insert("_meta".into(), meta.clone());
    }
    if !payload.contains_key("stop_reason") && !payload.contains_key("stopReason") {
        return None;
    }
    let complete = parse_complete_event(&Value::Object(payload));
    (!complete.session_id.is_empty() && !complete.prompt_id.is_empty()).then_some(complete)
}

fn claim_public_completion(complete: &CompleteEvent) -> bool {
    if complete.prompt_id.is_empty() {
        return true;
    }
    static SEEN: std::sync::OnceLock<
        std::sync::Mutex<std::collections::VecDeque<(String, String)>>,
    > = std::sync::OnceLock::new();
    let mut seen = SEEN
        .get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
        .lock()
        .unwrap();
    let key = (complete.session_id.clone(), complete.prompt_id.clone());
    if seen.contains(&key) {
        return false;
    }
    seen.push_back(key);
    if seen.len() > 4096 {
        seen.pop_front();
    }
    true
}
/// Both ACP response and notification enter here. The durable automation record
/// claims finalization, so channel order cannot change status or duplicate delivery.
pub(crate) async fn handle_prompt_complete(app: &AppHandle, complete: CompleteEvent) {
    if complete.session_id.is_empty() || complete.prompt_id.is_empty() {
        tracing::warn!("ignored completion without an admitted prompt identity");
        return;
    }
    // Prompt finished: preserve its full terminal semantics. In
    // particular, several distinct outcomes share stopReason
    // "cancelled" and can only be explained by their category.
    let session_id = complete.session_id.clone();
    let stop_reason = complete.stop_reason.clone();
    let disposition = completion_disposition(&complete);
    if disposition == CompletionDisposition::Stopped {
        crate::automations::mark_run_stopping(&session_id, &complete.prompt_id);
    }
    crate::coding_document_bridge::complete_run(&session_id, &complete.prompt_id).await;
    if let Err(error) = crate::coding_review_runtime::complete(app, &complete) {
        tracing::warn!(%error,"runtime review persistence failed");
    }
    let automation = if disposition != CompletionDisposition::Superseded {
        crate::automations::complete_run_for_session(
            &session_id,
            &complete.prompt_id,
            disposition == CompletionDisposition::Success,
            (disposition != CompletionDisposition::Success).then_some(
                complete
                    .cancellation_category
                    .as_deref()
                    .unwrap_or(&stop_reason),
            ),
        )
    } else {
        crate::automations::interrupt_run_for_session(&session_id, &complete.prompt_id)
    };
    let automation_handled = !matches!(
        &automation,
        crate::automations::AutomationCompletionOutcome::NotAutomation
    );
    match automation {
        crate::automations::AutomationCompletionOutcome::Persisted(completion) => {
            crate::automations::publish_automation_completion(app, *completion)
        }
        crate::automations::AutomationCompletionOutcome::PersistencePending(error) => {
            tracing::error!(%error, %session_id, "automation finished but history persistence is pending retry");
            let _ = app.emit(
                "automation://persistence-error",
                serde_json::json!({"sessionId": session_id, "error": error, "retrying": true}),
            );
        }
        _ => {}
    }
    // Public turn settlement is independent of automation history I/O and
    // applies to ordinary follow-ups in a historical automation session too.
    if !claim_public_completion(&complete) {
        return;
    }
    let automation_notification: Option<crate::automations::AutomationCompletion> = None;

    let failed = disposition == CompletionDisposition::Failed;
    let stopped = disposition == CompletionDisposition::Stopped;
    // EchoAgent reports mid-turn failures (429 hit while a tool was
    // running, connection reset, etc.) via prompt_complete with
    // stopReason "rate_limit" or "error" — NOT as a thrown error.
    // The `agent_result` field carries the server detail for
    // generic errors (null/absent for rate_limit). Forward both as
    // a dedicated `agent://turn-error` so the UI can surface a
    // friendly message instead of silently marking the turn done.
    if failed {
        let detail = complete
            .agent_result
            .clone()
            .or_else(|| complete.cancellation_category.clone());
        tracing::info!(
            session_id = %session_id,
            stop_reason = %stop_reason,
            detail,
            "turn ended abnormally — emitting agent://turn-error"
        );
        let _ = app.emit(
            "agent://turn-error",
            TurnErrorEvent {
                session_id: session_id.clone(),
                prompt_id: complete.prompt_id.clone(),
                kind: stop_reason.clone(),
                detail,
            },
        );
    }
    // `send_now` ends the old turn only so a replacement prompt can
    // take over. Its automation occurrence is closed as manually interrupted,
    // while public settlement must not stop the replacement conversation.
    if disposition != CompletionDisposition::Superseded && !automation_handled {
        if let Some(completion) = automation_notification.as_ref() {
            crate::automations::emit_automation_update(app, completion.event.clone());
        }
        let body = if failed {
            "打开任务查看错误并重试"
        } else if stopped {
            "打开任务查看当前进度"
        } else {
            "打开任务查看结果"
        };
        let notify_app = app.clone();
        let notify_session = session_id.clone();
        let push = automation_notification
            .as_ref()
            .is_none_or(|completion| completion.push);
        let automation_name = automation_notification.map(|completion| completion.automation_name);
        tokio::spawn(async move {
            let is_automation = automation_name.is_some();
            let lookup_session = notify_session.clone();
            let Ok((title, notification_id)) = tokio::task::spawn_blocking(move || {
                let task_title = if automation_name.is_some() {
                    None
                } else {
                    crate::notifications::task_title_for_notification(&lookup_session)
                };
                let title = if let Some(name) = automation_name.as_deref() {
                    if failed {
                        format!("自动化《{name}》运行失败")
                    } else if stopped {
                        format!("自动化《{name}》已停止")
                    } else {
                        format!("自动化《{name}》已完成")
                    }
                } else if let Some(name) = task_title.as_deref() {
                    if failed {
                        format!("任务《{name}》的回复失败")
                    } else if stopped {
                        format!("任务《{name}》的回复已停止")
                    } else {
                        format!("任务《{name}》的回复已完成")
                    }
                } else if failed {
                    "任务回复失败".into()
                } else if stopped {
                    "任务回复已停止".into()
                } else {
                    "任务回复已完成".into()
                };
                let notification_id = crate::notifications::append_with_target(
                    if failed {
                        crate::notifications::NotificationKind::Error
                    } else {
                        crate::notifications::NotificationKind::SessionComplete
                    },
                    &title,
                    Some(body),
                    Some(&lookup_session),
                    None,
                    None,
                    if failed {
                        "error"
                    } else if stopped {
                        "warn"
                    } else {
                        "info"
                    },
                )
                .ok();
                (title, notification_id)
            })
            .await
            else {
                tracing::error!("completion notification preparation task failed");
                return;
            };
            if push {
                let message = crate::notifications::NotifyMessage {
                    title,
                    body: Some(body.into()),
                    level: if failed {
                        "error".into()
                    } else if stopped {
                        "warn".into()
                    } else {
                        "info".into()
                    },
                    session_id: Some(notify_session),
                    request_id: None,
                    automation_id: None,
                    notification_id,
                    hidden_title: Some(
                        if is_automation {
                            if failed {
                                "自动化运行失败"
                            } else if stopped {
                                "自动化已停止"
                            } else {
                                "自动化已完成"
                            }
                        } else if failed {
                            "任务回复失败"
                        } else if stopped {
                            "任务回复已停止"
                        } else {
                            "任务回复已完成"
                        }
                        .into(),
                    ),
                    background_only: !is_automation,
                };
                let _ = crate::notifications::dispatch_with_desktop_fallback(&notify_app, message)
                    .await;
            }
        });
    }
    let _ = app.emit("agent://complete", complete);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionDisposition {
    Success,
    Stopped,
    Failed,
    Superseded,
}

/// Keep desktop lifecycle, notifications, and automation records aligned with
/// EchoAgent's richer terminal metadata. Several failure modes intentionally
/// use `stopReason: "cancelled"`, so stop reason alone is insufficient.
pub(crate) fn completion_disposition(complete: &CompleteEvent) -> CompletionDisposition {
    if complete.stop_reason == "cancelled" && complete.cancel_trigger.as_deref() == Some("send_now")
    {
        return CompletionDisposition::Superseded;
    }
    if matches!(
        complete.cancellation_category.as_deref(),
        Some("HookDenied" | "max_turns_reached" | "action_stationarity")
    ) {
        return CompletionDisposition::Failed;
    }
    match complete.stop_reason.as_str() {
        "error" | "rate_limit" | "rate_limited" | "refusal" | "content_filter" | "max_tokens"
        | "max_turns" => CompletionDisposition::Failed,
        "cancelled" => CompletionDisposition::Stopped,
        _ => CompletionDisposition::Success,
    }
}

/// Parse an `echo.agent/session_notification` payload and, if it carries a freshly
/// generated session title (`SessionSummaryGenerated`), emit `agent://summary`
/// so the frontend can update the sidebar entry. Unknown update variants are
/// ignored (we ACK regardless, in `handle_client_message`).
///
/// The wire shape (snake_case tag AND fields — EchoAgent's `SessionUpdate` uses
/// `rename_all = "snake_case"` on the enum, which renames only the tag; the
/// struct-variant fields keep their Rust snake_case names):
/// ```json
/// { "sessionId": "...", "update": { "sessionUpdate": "session_summary_generated",
///                                   "session_summary": "..." }, "meta": {...} }
/// ```
async fn handle_session_notification(app: &AppHandle, params: &Value) {
    let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
        tracing::debug!("session_notification: missing sessionId, ignoring");
        return;
    };
    let Some(update) = params.get("update") else {
        tracing::debug!(session_id, "session_notification: missing update field");
        return;
    };
    let kind = update
        .get("sessionUpdate")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    tracing::info!(session_id, kind, "received echo.agent/session_notification");
    crate::groups::observe(session_id, update, params.get("_meta"));
    match kind {
        "turn_completed" => {
            if let Some(complete) = parse_live_durable_completion(session_id, params, update) {
                handle_prompt_complete(app, complete).await;
            }
            // Durable turn endings arrive through this extension notification,
            // not ACP SessionNotification. Forward them to the transcript so
            // replayed replies close their streaming state on session load.
            let _ = app.emit(
                "agent://update",
                durable_completion_update_event(session_id, params, update),
            );
            if let Some(event) = parse_turn_usage_event(session_id, params, update) {
                let _ = app.emit("agent://turn-usage", event);
            }
        }
        "session_summary_generated" => {
            // Accept the camelCase variant too, defensively — reading only
            // `sessionSummary` silently drops every generated title (the event
            // never fires and the sidebar/topbar keeps the placeholder).
            let raw_title = update
                .get("session_summary")
                .or_else(|| update.get("sessionSummary"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let is_manual = params
                .get("_meta")
                .and_then(|meta| meta.get("echo.agent/titleIsManual"))
                .and_then(Value::as_bool)
                == Some(true);
            let title = if is_manual {
                (!raw_title.trim().is_empty()).then(|| raw_title.trim().to_string())
            } else {
                crate::session_title::clean_auto_title(raw_title)
            };
            if let Some(title) = title {
                tracing::info!(session_id, title, "emitting agent://summary");
                let _ = app.emit(
                    "agent://summary",
                    SummaryEvent {
                        session_id: session_id.to_string(),
                        title,
                    },
                );
            } else {
                tracing::warn!(
                    session_id,
                    "session_summary_generated contained no safe display title"
                );
            }
        }
        "subagent_spawned" | "subagent_progress" | "subagent_finished" => {
            emit_subagent_event(app, session_id, kind, update, params);
        }
        _ => {
            tracing::debug!(
                session_id,
                kind,
                "session_notification: unhandled kind, ignoring"
            );
        }
    }
}

fn durable_completion_update_event(
    session_id: &str,
    params: &Value,
    update: &Value,
) -> UpdateEvent {
    UpdateEvent {
        session_id: Some(session_id.to_string()),
        meta: params.get("_meta").cloned(),
        update: update.clone(),
    }
}

fn parse_turn_usage_event(
    session_id: &str,
    params: &Value,
    update: &Value,
) -> Option<TurnUsageEvent> {
    let prompt_id = update
        .get("prompt_id")
        .or_else(|| update.get("promptId"))
        .and_then(Value::as_str)?;
    if prompt_id.is_empty() {
        return None;
    }
    let usage = update.get("usage").filter(|value| value.is_object())?;
    let meta = params.get("_meta");
    Some(TurnUsageEvent {
        session_id: session_id.to_string(),
        prompt_id: prompt_id.to_string(),
        usage: usage.clone(),
        occurred_at: meta
            .and_then(|value| value.get("agentTimestampMs"))
            .and_then(Value::as_i64),
        event_id: meta
            .and_then(|value| value.get("eventId"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Forward subagent lifecycle events to the frontend as `agent://subagent`.
/// EchoAgent emits `subagent_spawned` (before child starts), `subagent_progress`
/// (every ~2s while running), and `subagent_finished` (on completion).
fn parse_subagent_event(
    parent_session_id: &str,
    kind: &str,
    update: &Value,
    params: &Value,
) -> Option<SubagentEvent> {
    let subagent_id = update
        .get("subagent_id")
        .or_else(|| update.get("subagentId"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if subagent_id.is_empty() {
        tracing::warn!(kind, "subagent notification missing subagent_id, skipping");
        return None;
    }

    let str_field = |key: &str| {
        update
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let u64_field = |key: &str| {
        update
            .get(key)
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
    };
    let u32_field = |key: &str| u64_field(key).map(|v| v as u32);
    let u8_field = |key: &str| u64_field(key).map(|v| v as u8);

    let (phase, status) = match kind {
        "subagent_spawned" => ("spawned".to_string(), Some("running".to_string())),
        "subagent_progress" => ("progress".to_string(), Some("running".to_string())),
        "subagent_finished" => {
            let st = str_field("status").unwrap_or_else(|| "completed".to_string());
            ("finished".to_string(), Some(st))
        }
        _ => return None,
    };

    let tools_used = update
        .get("tools_used")
        .or_else(|| update.get("toolsUsed"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        });

    let meta = params.get("_meta");
    Some(SubagentEvent {
        session_id: parent_session_id.to_string(),
        phase,
        subagent_id: subagent_id.clone(),
        child_session_id: str_field("child_session_id")
            .or_else(|| str_field("childSessionId"))
            .or_else(|| Some(subagent_id.clone())),
        parent_prompt_id: str_field("parent_prompt_id").or_else(|| str_field("parentPromptId")),
        description: str_field("description"),
        subagent_type: str_field("subagent_type").or_else(|| str_field("subagentType")),
        model: str_field("model"),
        persona: str_field("persona"),
        role: str_field("role"),
        effective_context_source: str_field("effective_context_source")
            .or_else(|| str_field("effectiveContextSource")),
        context_normalized: update
            .get("context_normalized")
            .or_else(|| update.get("contextNormalized"))
            .and_then(Value::as_bool),
        capability_mode: str_field("capability_mode").or_else(|| str_field("capabilityMode")),
        resumed_from: str_field("resumed_from").or_else(|| str_field("resumedFrom")),
        workflow_run_id: str_field("workflow_run_id").or_else(|| str_field("workflowRunId")),
        status,
        duration_ms: u64_field("duration_ms").or_else(|| u64_field("durationMs")),
        turn_count: u32_field("turn_count")
            .or_else(|| u32_field("turnCount"))
            .or_else(|| u32_field("turns")),
        tool_call_count: u32_field("tool_call_count")
            .or_else(|| u32_field("toolCallCount"))
            .or_else(|| u32_field("tool_calls")),
        tokens_used: u64_field("tokens_used").or_else(|| u64_field("tokensUsed")),
        context_window_tokens: u64_field("context_window_tokens")
            .or_else(|| u64_field("contextWindowTokens")),
        context_usage_pct: u8_field("context_usage_pct").or_else(|| u8_field("contextUsagePct")),
        tools_used,
        error_count: u32_field("error_count").or_else(|| u32_field("errorCount")),
        error: str_field("error"),
        output: str_field("output"),
        occurred_at: meta
            .and_then(|value| value.get("agentTimestampMs"))
            .and_then(Value::as_i64),
        is_replay: meta
            .and_then(|value| value.get("isReplay"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn emit_subagent_event(
    app: &AppHandle,
    parent_session_id: &str,
    kind: &str,
    update: &Value,
    params: &Value,
) {
    let Some(evt) = parse_subagent_event(parent_session_id, kind, update, params) else {
        return;
    };

    tracing::info!(
        session_id = %evt.session_id,
        phase = %evt.phase,
        subagent_id = %evt.subagent_id,
        "emitting agent://subagent"
    );
    let _ = app.emit("agent://subagent", evt);
}

/// Send a MethodNotFound error on a fs/terminal response channel. We advertised
/// no fs/terminal capability so these shouldn't arrive — deny to keep the
/// agent's future from hanging.
fn deny_fs_terminal<T>(response_tx: tokio::sync::oneshot::Sender<acp::Result<T>>) {
    let err = acp::Error::new(
        acp::ErrorCode::MethodNotFound.into(),
        "EchoAgent does not handle fs/terminal requests".to_string(),
    );
    let _ = response_tx.send(Err(err));
}

/// `acp::SessionUpdate` isn't `Serialize` in a form we can emit directly, so
/// round-trip through JSON: the ACP crate does serialize for the wire format.
fn serialize_session_update(update: &acp::SessionUpdate) -> Value {
    serde_json::to_value(update).unwrap_or_else(
        |_| serde_json::json!({ "type": "unknown", "error": "failed to serialize session update" }),
    )
}

fn permission_kind_str(k: &acp::PermissionOptionKind) -> &'static str {
    match k {
        acp::PermissionOptionKind::AllowOnce => "allow",
        acp::PermissionOptionKind::AllowAlways => "allow_always",
        acp::PermissionOptionKind::RejectOnce => "deny",
        acp::PermissionOptionKind::RejectAlways => "deny_always",
        _ => "other",
    }
}

/// Choose the least-persistent successful outcome. Global always-approve
/// already supplies the policy; it must not create an extra remembered grant.
fn preferred_allow_option(
    options: &[PermissionOptionFrontend],
) -> Option<&PermissionOptionFrontend> {
    options
        .iter()
        .find(|option| option.kind == "allow")
        .or_else(|| options.iter().find(|option| option.kind == "allow_always"))
}

fn is_builtin_live_info_permission(call: &acp::ToolCallUpdate) -> bool {
    let Some(meta) = call
        .meta
        .as_ref()
        .and_then(|meta| meta.get("echo.agent/tool"))
    else {
        return false;
    };
    let name = meta.get("name").and_then(serde_json::Value::as_str);
    let target = call.fields.title.as_deref();
    let is_live_info = target.is_some_and(|target| {
        target
            == format!(
                "{}__weather_forecast",
                crate::live_info_mcp::MCP_SERVER_NAME
            )
            || target == format!("{}__search_web", crate::live_info_mcp::MCP_SERVER_NAME)
    });
    if !is_live_info {
        return false;
    }
    let namespace = meta.get("namespace").and_then(serde_json::Value::as_str);
    let direct = namespace == Some("mcp") && name == target;
    let wrapper = namespace == Some("echo_agent_build")
        && name == Some("use_tool")
        && call.fields.raw_input.as_ref().is_some_and(|raw| {
            raw.get("variant").and_then(serde_json::Value::as_str) == Some("UseTool")
                && raw.get("tool_name").and_then(serde_json::Value::as_str) == target
        });
    direct || wrapper
}

fn is_builtin_computer_permission(call: &acp::ToolCallUpdate) -> bool {
    let Some(target) = call.fields.title.as_deref() else {
        return false;
    };
    let Some(name) = target.strip_prefix("echoagent-automation__") else {
        return false;
    };
    if !crate::automation::is_managed_computer_tool(name) {
        return false;
    }
    let Some(meta) = call.meta.as_ref().and_then(|m| m.get("echo.agent/tool")) else {
        return false;
    };
    match (meta["namespace"].as_str(), meta["name"].as_str()) {
        (Some("mcp"), Some(name)) => name == target,
        (Some("echo_agent_build"), Some("use_tool")) => call
            .fields
            .raw_input
            .as_ref()
            .is_some_and(|raw| raw["variant"] == "UseTool" && raw["tool_name"] == target),
        _ => false,
    }
}

pub(crate) fn emit_permission_closed(app: &AppHandle, notice: PermissionClosedFrontend) {
    let _ = app.emit("agent://permission-closed", notice);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission_option(id: &str, kind: &str) -> PermissionOptionFrontend {
        PermissionOptionFrontend {
            option_id: id.into(),
            kind: kind.into(),
            title: id.into(),
        }
    }

    fn permission_request(
        request_id: &str,
        options: Vec<PermissionOptionFrontend>,
    ) -> PermissionFrontend {
        PermissionFrontend {
            request_id: request_id.into(),
            session_id: "session-1".into(),
            tool_call_id: format!("tool-{request_id}"),
            tool_kind: "edit".into(),
            title: "Edit file".into(),
            raw_input: None,
            options,
        }
    }

    #[test]
    fn always_approve_prefers_allow_once_over_remembered_grant() {
        let options = vec![
            permission_option("remember", "allow_always"),
            permission_option("once", "allow"),
        ];
        assert_eq!(
            preferred_allow_option(&options).map(|option| option.option_id.as_str()),
            Some("once")
        );
    }

    #[test]
    fn only_registered_live_information_tools_are_auto_approved() {
        let make_call = |name: &str, namespace: &str| {
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from("call-1")),
                acp::ToolCallUpdateFields::new().title(Some(name.to_string())),
            )
            .meta(
                serde_json::json!({
                    "echo.agent/tool": { "name": name, "namespace": namespace }
                })
                .as_object()
                .cloned(),
            )
        };
        assert!(is_builtin_live_info_permission(&make_call(
            "echoagent-live-info__weather_forecast",
            "mcp"
        )));
        assert!(is_builtin_live_info_permission(&make_call(
            "echoagent-live-info__search_web",
            "mcp"
        )));
        assert!(!is_builtin_live_info_permission(&make_call(
            "other-server__search_web",
            "mcp"
        )));
        assert!(!is_builtin_live_info_permission(&make_call(
            "echoagent-live-info__search_web",
            "echo_agent_build"
        )));

        let wrapped = acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("call-2")),
            acp::ToolCallUpdateFields::new()
                .title(Some("echoagent-live-info__search_web".to_string()))
                .raw_input(Some(serde_json::json!({
                    "variant": "UseTool",
                    "tool_name": "echoagent-live-info__search_web",
                    "tool_input": { "query": "天气" }
                }))),
        )
        .meta(
            serde_json::json!({
                "echo.agent/tool": { "name": "use_tool", "namespace": "echo_agent_build" }
            })
            .as_object()
            .cloned(),
        );
        assert!(is_builtin_live_info_permission(&wrapped));
    }

    #[test]
    fn computer_permission_delegation_requires_registered_mcp_identity() {
        let make = |target: &str, name: &str, namespace: &str, raw| {
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from("call-1")),
                acp::ToolCallUpdateFields::new()
                    .title(Some(target.to_owned()))
                    .raw_input(raw),
            )
            .meta(
                serde_json::json!({"echo.agent/tool":{"name":name,"namespace":namespace}})
                    .as_object()
                    .cloned(),
            )
        };
        let target = "echoagent-automation__computer_set_value";
        assert!(is_builtin_computer_permission(&make(
            target, target, "mcp", None
        )));
        let wrapped = make(
            target,
            "use_tool",
            "echo_agent_build",
            Some(serde_json::json!({"variant":"UseTool","tool_name":target,"tool_input":{}})),
        );
        assert!(is_builtin_computer_permission(&wrapped));
        for target in [
            "other-server__computer_set_value",
            "echoagent-automation__browser_click",
            "echoagent-automation___proof",
            "echoagent-automation__computer_unknown",
        ] {
            assert!(!is_builtin_computer_permission(&make(
                target, target, "mcp", None
            )));
        }
        assert!(!is_builtin_computer_permission(&make(
            target,
            "Bash",
            "echo_agent_build",
            None
        )));
        assert!(!is_builtin_computer_permission(&make(
            target,
            "use_tool",
            "echo_agent_build",
            Some(
                serde_json::json!({"variant":"UseTool","tool_name":"other-server__computer_set_value"})
            )
        )));
    }

    #[tokio::test]
    async fn permission_terminal_paths_return_authoritative_closure_notices() {
        let registry = Permissions::new();
        let resolved_rx = registry
            .register(permission_request(
                "resolved",
                vec![permission_option("once", "allow")],
            ))
            .await
            .expect("register resolved request");
        let resolved = registry
            .resolve("resolved", PermissionOutcome::Selected("once".into()))
            .await
            .expect("resolved closure notice");
        assert_eq!(resolved.request_id, "resolved");
        assert!(matches!(
            resolved_rx.await.expect("resolved outcome"),
            PermissionOutcome::Selected(option_id) if option_id == "once"
        ));

        let discarded_rx = registry
            .register(permission_request(
                "discarded",
                vec![permission_option("once", "allow")],
            ))
            .await
            .expect("register discarded request");
        let discarded = registry
            .discard("discarded")
            .await
            .expect("discard closure notice");
        assert_eq!(discarded.request_id, "discarded");
        assert!(discarded_rx.await.is_err());

        let cancelled_rx = registry
            .register(permission_request(
                "cancelled",
                vec![permission_option("once", "allow")],
            ))
            .await
            .expect("register cancelled request");
        let closed = registry.cancel_all().await;
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].request_id, "cancelled");
        assert!(matches!(
            cancelled_rx.await.expect("cancelled outcome"),
            PermissionOutcome::Cancelled
        ));
    }

    #[test]
    fn question_wire_preserves_mode_multiselect_description_and_preview() {
        let request: QuestionWireRequest = serde_json::from_value(serde_json::json!({
            "sessionId": "s-1",
            "toolCallId": "tc-1",
            "mode": "plan",
            "timeoutSecs": 42,
            "questions": [{
                "id": "database",
                "question": "Which database?",
                "multiSelect": true,
                "options": [{
                    "id": "redis",
                    "label": "Redis",
                    "description": "In-memory store",
                    "preview": "redis.conf"
                }]
            }]
        }))
        .expect("typed question request");
        assert_eq!(request.mode, QuestionMode::Plan);
        assert_eq!(request.timeout, Some(42));
        assert_eq!(request.questions[0].multi_select, Some(true));
        let QuestionWireOption::Detailed {
            description,
            preview,
            ..
        } = &request.questions[0].options[0]
        else {
            panic!("expected detailed option")
        };
        assert_eq!(description, "In-memory store");
        assert_eq!(preview.as_deref(), Some("redis.conf"));
    }

    #[test]
    fn question_frontend_limits_runtime_wire_fields() {
        let request: QuestionWireRequest = serde_json::from_value(serde_json::json!({
            "sessionId": "s-1",
            "toolCallId": "tc-1",
            "title": "Pick a database",
            "timeoutSecs": u64::MAX,
            "questions": [{
                "question": "Which database?",
                "options": [{
                    "id": "redis",
                    "label": "Redis",
                    "description": "x".repeat(MAX_INTERACTION_TITLE_CHARS + 10),
                    "preview": "redis.conf"
                }]
            }]
        }))
        .expect("typed question request");
        let frontend = question_frontend_from_wire(request).expect("bounded frontend request");
        assert_eq!(frontend.timeout, Some(MAX_QUESTION_TIMEOUT_SECS));
        assert!(frontend.questions[0].options[0]
            .description
            .ends_with("[truncated]"));

        let oversized_questions: Vec<Value> = (0..=MAX_QUESTION_ITEMS)
            .map(|index| {
                serde_json::json!({
                    "question": format!("Question {index}?"),
                    "options": []
                })
            })
            .collect();
        let request: QuestionWireRequest = serde_json::from_value(serde_json::json!({
            "sessionId": "s-1",
            "toolCallId": "tc-1",
            "questions": oversized_questions
        }))
        .expect("typed oversized question request");
        assert!(question_frontend_from_wire(request).is_err());
    }

    #[test]
    fn permission_raw_input_is_bounded_for_frontend() {
        let raw = serde_json::json!({
            "payload": "x".repeat(MAX_PERMISSION_RAW_INPUT_BYTES + 1)
        });
        let bounded = capped_json_for_frontend(Some(&raw)).expect("bounded value");
        assert_eq!(bounded["_truncated"], true);
    }

    #[test]
    fn plan_question_actions_use_runtime_wire_outcomes() {
        let chat = question_response_value(QuestionOutcome::ChatAboutThis {
            partial_answers: HashMap::from([("Database?".into(), "Redis".into())]),
        });
        assert_eq!(chat["outcome"], "chat_about_this");
        assert_eq!(chat["partial_answers"]["Database?"], "Redis");

        let skip = question_response_value(QuestionOutcome::SkipInterview {
            partial_answers: HashMap::new(),
        });
        assert_eq!(skip["outcome"], "skip_interview");
    }

    #[tokio::test]
    async fn plan_registry_is_ordered_replayable_and_acked_once() {
        let registry = PlanApprovals::new();
        let first = PlanApprovalFrontend {
            request_id: "r-1".into(),
            session_id: "s-1".into(),
            tool_call_id: "tc-1".into(),
            plan_content: Some("first".into()),
        };
        let second = PlanApprovalFrontend {
            request_id: "r-2".into(),
            session_id: "s-1".into(),
            tool_call_id: "tc-2".into(),
            plan_content: Some("second".into()),
        };
        let first_rx = registry.register(first).await.expect("first request");
        let _second_rx = registry.register(second).await.expect("second request");
        let ids: Vec<String> = registry
            .list(Some("s-1"))
            .await
            .into_iter()
            .map(|request| request.request_id)
            .collect();
        assert_eq!(ids, ["r-1", "r-2"]);
        assert!(registry.resolve("r-1", PlanApprovalOutcome::Approved).await);
        assert!(
            !registry
                .resolve("r-1", PlanApprovalOutcome::Abandoned)
                .await
        );
        assert!(matches!(
            first_rx.await.expect("approval result"),
            PlanApprovalOutcome::Approved
        ));
    }

    #[tokio::test]
    async fn plan_registry_rejects_when_pending_queue_is_full() {
        let registry = PlanApprovals::new();
        for index in 0..MAX_PENDING_INTERACTIONS {
            registry
                .register(PlanApprovalFrontend {
                    request_id: format!("r-{index}"),
                    session_id: "s-1".into(),
                    tool_call_id: format!("tc-{index}"),
                    plan_content: None,
                })
                .await
                .expect("queue slot");
        }
        assert!(registry
            .register(PlanApprovalFrontend {
                request_id: "overflow".into(),
                session_id: "s-1".into(),
                tool_call_id: "tc-overflow".into(),
                plan_content: None,
            })
            .await
            .is_err());
    }

    #[test]
    fn parses_durable_turn_usage_with_replay_metadata() {
        let params = serde_json::json!({
            "_meta": { "eventId": "evt-1", "agentTimestampMs": 1_788_000_000_000_i64 }
        });
        let update = serde_json::json!({
            "sessionUpdate": "turn_completed",
            "prompt_id": "p-1",
            "usage": { "inputTokens": 120, "outputTokens": 30, "modelCalls": 2 }
        });
        let event = parse_turn_usage_event("s-1", &params, &update).expect("valid usage");
        assert_eq!(event.session_id, "s-1");
        assert_eq!(event.prompt_id, "p-1");
        assert_eq!(event.occurred_at, Some(1_788_000_000_000));
        assert_eq!(event.event_id.as_deref(), Some("evt-1"));
        assert_eq!(event.usage["modelCalls"], 2);
    }

    #[test]
    fn forwards_durable_turn_completion_with_session_and_replay_metadata() {
        let params = serde_json::json!({
            "_meta": { "isReplay": true, "agentTimestampMs": 1_788_000_000_000_i64 }
        });
        let update = serde_json::json!({
            "sessionUpdate": "turn_completed",
            "prompt_id": "p-1",
            "stop_reason": "end_turn"
        });
        let value = serde_json::to_value(durable_completion_update_event("s-1", &params, &update))
            .expect("serializable update");
        assert_eq!(value["sessionId"], "s-1");
        assert_eq!(value["_meta"]["isReplay"], true);
        assert_eq!(value["sessionUpdate"], "turn_completed");
        assert_eq!(value["stop_reason"], "end_turn");
    }

    #[test]
    fn live_durable_completion_uses_exact_prompt_and_cancellation_identity() {
        let params = serde_json::json!({"_meta": {
            "cancelTrigger": "send_now", "cancellationCategory": "Superseded"
        }});
        let update = serde_json::json!({
            "sessionUpdate": "turn_completed", "prompt_id": "old-prompt",
            "stop_reason": "cancelled", "agent_result": "partial response"
        });
        let complete = parse_live_durable_completion("session", &params, &update).unwrap();
        assert_eq!(complete.session_id, "session");
        assert_eq!(complete.prompt_id, "old-prompt");
        assert_eq!(complete.stop_reason, "cancelled");
        assert_eq!(complete.cancel_trigger.as_deref(), Some("send_now"));
        assert_eq!(
            complete.cancellation_category.as_deref(),
            Some("Superseded")
        );
        assert_eq!(complete.agent_result.as_deref(), Some("partial response"));
        assert_eq!(
            completion_disposition(&complete),
            CompletionDisposition::Superseded
        );
    }

    #[test]
    fn ordinary_chat_completion_uses_the_runtime_generated_prompt_identity() {
        // Ordinary Chat and WeiXin may omit a client promptId. Runtime still
        // allocates an internal identity and includes it on its durable rail.
        let update = serde_json::json!({
            "sessionUpdate": "turn_completed", "prompt_id": "runtime-generated",
            "stop_reason": "end_turn"
        });
        let complete =
            parse_live_durable_completion("ordinary-chat", &Value::Null, &update).unwrap();
        assert_eq!(complete.prompt_id, "runtime-generated");
        assert_eq!(complete.stop_reason, "end_turn");
        assert_eq!(
            completion_disposition(&complete),
            CompletionDisposition::Success
        );
        assert!(
            parse_live_durable_completion(
                "ordinary-chat",
                &serde_json::json!({
                    "_meta": {"isReplay": true}
                }),
                &update
            )
            .is_none(),
            "replayed history must not finalize or notify an active run"
        );
    }

    #[test]
    fn durable_completion_rejects_missing_prompt_or_terminal_fields() {
        for update in [
            serde_json::json!({"sessionUpdate":"turn_completed","stop_reason":"end_turn"}),
            serde_json::json!({"sessionUpdate":"turn_completed","prompt_id":"" ,"stop_reason":"end_turn"}),
            serde_json::json!({"sessionUpdate":"turn_completed","prompt_id":"p"}),
        ] {
            assert!(parse_live_durable_completion("session", &Value::Null, &update).is_none());
        }
    }

    #[test]
    fn accepts_both_live_and_durable_echo_agent_update_methods() {
        assert!(is_echo_agent_session_update_method(
            "echo.agent/session_notification"
        ));
        assert!(is_echo_agent_session_update_method(
            "echo.agent/session/update"
        ));
        assert!(!is_echo_agent_session_update_method("session/update"));
    }

    #[test]
    fn parses_replayed_subagent_provenance_and_evidence() {
        let params = serde_json::json!({
            "_meta": {
                "isReplay": true,
                "agentTimestampMs": 1_788_000_000_123_i64
            }
        });
        let spawned = serde_json::json!({
            "sessionUpdate": "subagent_spawned",
            "subagent_id": "child-1",
            "child_session_id": "child-1",
            "parent_prompt_id": "prompt-7",
            "description": "审查持久化链路",
            "subagent_type": "explore",
            "model": "model-a",
            "persona": "reviewer",
            "role": "researcher",
            "effective_context_source": "new",
            "context_normalized": true,
            "capability_mode": "read-only",
            "workflow_run_id": "workflow-1"
        });
        let event = parse_subagent_event("parent-1", "subagent_spawned", &spawned, &params)
            .expect("valid subagent event");
        assert_eq!(event.session_id, "parent-1");
        assert_eq!(event.subagent_id, "child-1");
        assert_eq!(event.parent_prompt_id.as_deref(), Some("prompt-7"));
        assert_eq!(event.description.as_deref(), Some("审查持久化链路"));
        assert_eq!(event.model.as_deref(), Some("model-a"));
        assert_eq!(event.capability_mode.as_deref(), Some("read-only"));
        assert_eq!(event.context_normalized, Some(true));
        assert_eq!(event.occurred_at, Some(1_788_000_000_123));
        assert!(event.is_replay);

        let finished = serde_json::json!({
            "subagent_id": "child-1",
            "child_session_id": "child-1",
            "status": "completed",
            "tool_calls": 12,
            "turns": 3,
            "duration_ms": 4200,
            "tokens_used": 9000,
            "output": "审查完成"
        });
        let event = parse_subagent_event("parent-1", "subagent_finished", &finished, &params)
            .expect("valid finished event");
        assert_eq!(event.status.as_deref(), Some("completed"));
        assert_eq!(event.tool_call_count, Some(12));
        assert_eq!(event.turn_count, Some(3));
        assert_eq!(event.output.as_deref(), Some("审查完成"));
    }

    #[test]
    fn complete_event_preserves_terminal_metadata_and_camel_agent_result() {
        let event = parse_complete_event(&serde_json::json!({
            "sessionId": "s-1",
            "promptId": "p-1",
            "turnId": 7,
            "stopReason": "cancelled",
            "cancelTrigger": "permission",
            "cancellationCategory": "PermissionRejected",
            "agentResult": "permission denied"
        }));
        assert_eq!(event.session_id, "s-1");
        assert_eq!(event.prompt_id, "p-1");
        assert_eq!(event.turn_id, Some(7));
        assert_eq!(event.stop_reason, "cancelled");
        assert_eq!(event.cancel_trigger.as_deref(), Some("permission"));
        assert_eq!(
            event.cancellation_category.as_deref(),
            Some("PermissionRejected")
        );
        assert_eq!(event.agent_result.as_deref(), Some("permission denied"));
        assert_eq!(
            completion_disposition(&event),
            CompletionDisposition::Stopped
        );
    }

    #[test]
    fn complete_event_accepts_terminal_metadata_from_legacy_meta_envelope() {
        let event = parse_complete_event(&serde_json::json!({
            "sessionId": "s-1",
            "stopReason": "cancelled",
            "_meta": {
                "cancelTrigger": "send_now",
                "cancellationCategory": "MidTurnAbort"
            }
        }));
        assert_eq!(event.cancel_trigger.as_deref(), Some("send_now"));
        assert_eq!(event.cancellation_category.as_deref(), Some("MidTurnAbort"));
        assert_eq!(
            completion_disposition(&event),
            CompletionDisposition::Superseded
        );
    }

    #[test]
    fn terminal_disposition_treats_policy_cancellation_as_failure() {
        let event = parse_complete_event(&serde_json::json!({
            "session_id": "s-1",
            "prompt_id": "p-1",
            "stop_reason": "cancelled",
            "cancellation_category": "HookDenied"
        }));
        assert_eq!(
            completion_disposition(&event),
            CompletionDisposition::Failed
        );
    }

    #[test]
    fn update_event_serializes_replay_metadata_without_renaming_meta() {
        let event = UpdateEvent {
            session_id: Some("s-1".into()),
            meta: Some(serde_json::json!({ "agentTimestampMs": 123 })),
            update: serde_json::json!({ "sessionUpdate": "turn_completed" }),
        };
        let value = serde_json::to_value(event).expect("serializable update event");
        assert_eq!(value["sessionId"], "s-1");
        assert_eq!(value["_meta"]["agentTimestampMs"], 123);
        assert_eq!(value["sessionUpdate"], "turn_completed");
    }

    /// This fixture is also consumed through the real TypeScript subscription,
    /// transcript store and message UI. Validate it with the actual ACP serde
    /// producer and our production Tauri envelope, rather than two mock schemas.
    #[test]
    fn acp_fixture_matches_real_producer_and_tauri_envelope() {
        let text = |value: &str| acp::ContentBlock::Text(acp::TextContent::new(value));
        let updates = vec![
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(text("完成中文分析"))),
            acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk::new(text("核对计划"))),
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new("wire-tool", "读取 文件 说明.md")
                    .raw_input(serde_json::json!({"path":"/工作区/文件 说明.md"})),
            ),
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                "wire-tool",
                acp::ToolCallUpdateFields::new()
                    .status(acp::ToolCallStatus::Completed)
                    .raw_output(Value::Null)
                    .content(vec![
                        text("读取结果").into(),
                        acp::Diff::new("文件 说明.md", "新文件正文").into(),
                    ]),
            )),
            acp::SessionUpdate::Plan(acp::Plan::new(vec![acp::PlanEntry::new(
                "核验中文文件",
                acp::PlanEntryPriority::High,
                acp::PlanEntryStatus::Completed,
            )])),
        ];
        let emitted = updates
            .iter()
            .map(|update| {
                serde_json::to_value(UpdateEvent {
                    session_id: Some("wire-session".into()),
                    meta: Some(serde_json::json!({"promptId":"wire-prompt","isReplay":false})),
                    update: serialize_session_update(update),
                })
                .unwrap()
            })
            .collect::<Vec<_>>();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../src/lib/__tests__/fixtures/acp-wire.json"
        ))
        .unwrap();
        assert_eq!(serde_json::json!(emitted), fixture["updates"]);
        for (name, raw) in [
            (
                "complete",
                serde_json::json!({"session_id":"wire-session","prompt_id":"wire-prompt","stop_reason":"end_turn"}),
            ),
            (
                "failure",
                serde_json::json!({"session_id":"wire-session","prompt_id":"failed-prompt","stop_reason":"cancelled",
                "_meta":{"cancelTrigger":"hook","cancellationCategory":"HookDenied","agentResult":"策略拒绝"}}),
            ),
        ] {
            assert_eq!(
                serde_json::to_value(parse_complete_event(&raw)).unwrap(),
                fixture[name]
            );
        }
    }

    #[test]
    fn replay_update_merges_inner_prompt_metadata_without_duplicate_keys() {
        for kind in [
            "user_message_chunk",
            "agent_message_chunk",
            "tool_call_update",
        ] {
            let event = UpdateEvent {
                session_id: Some("s-1".into()),
                meta: Some(serde_json::json!({
                    "isReplay": true, "eventId": "durable-1", "agentTimestampMs": 123
                })),
                update: serde_json::json!({
                    "sessionUpdate": kind,
                    "_meta": { "modelId": "chat-qwen", "promptIndex": 0, "isReplay": false }
                }),
            };
            let encoded = serde_json::to_string(&event).expect("serialize replay update");
            assert_eq!(encoded.matches("\"_meta\"").count(), 1);
            let value: Value = serde_json::from_str(&encoded).expect("parse emitted update");
            assert_eq!(value["sessionId"], "s-1");
            assert_eq!(value["_meta"]["isReplay"], true);
            assert_eq!(value["_meta"]["eventId"], "durable-1");
            assert_eq!(value["_meta"]["agentTimestampMs"], 123);
            assert_eq!(value["_meta"]["modelId"], "chat-qwen");
            assert_eq!(value["_meta"]["promptIndex"], 0);
        }
    }

    #[test]
    fn live_update_keeps_inner_metadata_and_authoritative_session_identity() {
        let event = UpdateEvent {
            session_id: Some("s-1".into()),
            meta: None,
            update: serde_json::json!({
                "sessionUpdate": "user_message_chunk", "sessionId": "unrelated",
                "_meta": { "modelId": "chat-glm", "promptIndex": 2 }
            }),
        };
        let value = serde_json::to_value(event).expect("serialize live update");
        assert_eq!(value["sessionId"], "s-1");
        assert_eq!(value["_meta"]["modelId"], "chat-glm");
        assert_eq!(value["_meta"]["promptIndex"], 2);
        assert!(value["_meta"].get("isReplay").is_none());
    }

    #[test]
    fn rejects_turn_usage_without_prompt_or_ledger() {
        assert!(parse_turn_usage_event(
            "s",
            &Value::Null,
            &serde_json::json!({
                "usage": { "inputTokens": 1 }
            })
        )
        .is_none());
        assert!(parse_turn_usage_event(
            "s",
            &Value::Null,
            &serde_json::json!({
                "prompt_id": "p"
            })
        )
        .is_none());
    }
    #[test]
    fn public_completion_deduplication_is_per_prompt_not_historical_session() {
        let session = uuid::Uuid::now_v7().to_string();
        let first = CompleteEvent {
            session_id: session.clone(),
            prompt_id: "first".into(),
            turn_id: None,
            stop_reason: "end_turn".into(),
            cancel_trigger: None,
            cancellation_category: None,
            agent_result: None,
        };
        assert!(claim_public_completion(&first));
        assert!(!claim_public_completion(&first));
        let followup = CompleteEvent {
            prompt_id: "followup".into(),
            ..first
        };
        assert!(claim_public_completion(&followup));
        assert!(!claim_public_completion(&followup));
    }
}
