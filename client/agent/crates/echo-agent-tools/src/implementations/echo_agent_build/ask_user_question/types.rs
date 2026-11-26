//! Shared protocol and channel types for the AskUserQuestion blocking flow.
//!
//! These types define the request/response contract between three crates:
//!
//! - **`echo-agent-tools`** — tool blocks on a oneshot, formats the result.
//! - **`echo-agent-runtime`** — coordinator receives requests over mpsc, calls the
//!   client via ACP `ext_method`, sends results back over the oneshot.
//! - **`echo-agent-pager`** — handles the `ExtMethod`, renders UI, returns a
//!   typed response.
//!
//! All three crates import these types from `echo-agent-tools`.

use std::collections::HashMap;

use educe::Educe;
use indexmap::IndexMap;
use tokio::sync::{mpsc, oneshot};

use super::Question;
use crate::register_resource;

// ── ACP wire-format types ────────────────────────────────────────────────

/// Annotation on a single question's answer.
///
/// Carried inside the `accepted` response alongside the selected label.
/// - `preview`: verbatim `Option.preview` of the selected option (single-select only).
/// - `notes`: free-text the user typed in the freeform input.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuestionAnnotation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Mode context for the question UI.
///
/// Sent as part of the ACP `ext_method` request so the pager knows whether
/// to show plan-mode-only actions (Chat about this / Skip interview).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskUserQuestionMode {
    /// Normal mode. Client shows only Accept and Cancel.
    Default,
    /// Plan mode. Client shows Accept, Cancel, Chat about this, Skip interview.
    Plan,
}

/// ACP `ext_method` request payload (shell coordinator sends to client/pager).
///
/// Serialized as `camelCase` for the ACP JSON-RPC wire format.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskUserQuestionExtRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub questions: Vec<Question>,
    /// Controls whether the client shows plan-mode-only actions.
    pub mode: AskUserQuestionMode,
    /// Whole-questionnaire response budget. `None` means no response timer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Accepts both `"value"` (old wire format) and `["value"]` (new wire format)
/// for each answer entry, normalizing strings into single-element vectors.
fn deserialize_string_or_vec_answers<'de, D>(
    deserializer: D,
) -> Result<IndexMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Vec(Vec<String>),
        String(String),
    }

    let raw: IndexMap<String, StringOrVec> = serde::Deserialize::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|(k, v)| match v {
            StringOrVec::Vec(vec) => (k, vec),
            StringOrVec::String(s) => (k, vec![s]),
        })
        .collect())
}

/// ACP `ext_method` response payload (client/pager returns to shell coordinator).
///
/// Internally tagged on `"outcome"` with `snake_case` variant names so the
/// JSON looks like `{ "outcome": "accepted", "answers": { ... } }`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AskUserQuestionExtResponse {
    /// User accepted and submitted answers (Path A).
    Accepted {
        /// Answered questions in original order; unanswered omitted.
        /// One element per selected option; freeform-only is `["Other"]`
        /// with typed text in `annotations[q].notes`.
        #[serde(deserialize_with = "deserialize_string_or_vec_answers")]
        answers: IndexMap<String, Vec<String>>,
        /// Per-question annotations (preview, notes). Absent when empty.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<HashMap<String, QuestionAnnotation>>,
    },
    /// User chose "Chat about this" (Path B, plan mode only).
    ChatAboutThis {
        /// Partial answers: answered questions only, label only (no notes).
        /// Freeform-only => `"Other"` (notes dropped in plan-mode paths).
        #[serde(default)]
        partial_answers: HashMap<String, String>,
    },
    /// User chose "Skip interview and plan immediately" (Path C, plan mode only).
    SkipInterview {
        /// Same partial-answer rules as `ChatAboutThis`.
        #[serde(default)]
        partial_answers: HashMap<String, String>,
    },
    /// User cancelled / dismissed (Path D). NOT an error.
    Cancelled,
}

// ── In-process types (coordinator <-> tool) ──────────────────────────────

/// In-process result: coordinator -> tool.
///
/// Uses `Result` so the tool can distinguish user actions from infrastructure
/// failures:
/// - `Ok(UserQuestionResponse)` for all 4 user paths (accepted, chat, skip, cancel).
/// - `Err(UserQuestionError)` for transport failures or malformed responses.
pub type UserQuestionResult = Result<UserQuestionResponse, UserQuestionError>;

/// Successful user response (all 4 user paths).
///
/// Every variant here produces `Ok(UserAnswered { message })` at the tool
/// level with `ToolCall` status `Completed`.
#[derive(Debug, Clone)]
pub enum UserQuestionResponse {
    /// User accepted and submitted answers (Path A).
    Accepted {
        /// See `AskUserQuestionExtResponse::Accepted::answers`.
        answers: IndexMap<String, Vec<String>>,
        annotations: Option<HashMap<String, QuestionAnnotation>>,
    },
    /// User chose "Chat about this" (Path B, plan mode only).
    /// Carries the original questions so the formatter can iterate all of them.
    ChatAboutThis {
        questions: Vec<Question>,
        partial_answers: HashMap<String, String>,
    },
    /// User chose "Skip interview" (Path C, plan mode only).
    /// Carries the original questions so the formatter can iterate all of them.
    SkipInterview {
        questions: Vec<Question>,
        partial_answers: HashMap<String, String>,
    },
    /// User explicitly dismissed (Esc). NOT an error.
    Cancelled,
}

/// Infrastructure failure (NOT a user action).
///
/// These produce `Err(ToolError::ExecutionError { .. })` at the tool level
/// with `ToolCall` status `Failed`.
#[derive(Debug, Clone)]
pub enum UserQuestionError {
    /// ACP `ext_method` call failed (client disconnect, timeout, etc.).
    TransportError(String),
    /// Client returned JSON that could not be deserialized into
    /// `AskUserQuestionExtResponse`.
    MalformedResponse(String),
}

/// In-process request: tool -> coordinator (carries oneshot for reply).
///
/// Sent over the `mpsc` channel. The coordinator receives this, performs the
/// ACP `ext_method` round-trip, and sends the result back on `result_tx`.
#[derive(Educe)]
#[educe(Debug)]
pub struct UserQuestionRequest {
    pub tool_call_id: String,
    pub questions: Vec<Question>,
    /// Exact response budget used by the tool's blocking wait.
    pub timeout_secs: Option<u64>,
    #[educe(Debug(ignore))]
    pub result_tx: oneshot::Sender<UserQuestionResult>,
}

// ── Resource type ────────────────────────────────────────────────────────

/// Resource: `mpsc` sender injected into `SharedResources`.
///
/// Same injection pattern as `SubagentEventSender`. Cloned into each
/// session so that any `AskUserQuestionTool` invocation can emit a
/// `UserQuestionRequest` to the session's coordinator.
#[derive(Clone, Educe)]
#[educe(Debug)]
pub struct UserQuestionSender(
    #[educe(Debug(ignore))] pub mpsc::UnboundedSender<UserQuestionRequest>,
);

register_resource!("echo_agent_build", "UserQuestionSender", UserQuestionSender);

// ── Conversion helper ────────────────────────────────────────────────────

impl AskUserQuestionExtResponse {
    /// Convert the wire-format ACP response into the in-process response type.
    ///
    /// Called by the shell coordinator after deserializing the client's JSON.
    /// The `questions` parameter carries the original question list so that
    /// `ChatAboutThis` and `SkipInterview` responses can iterate all questions
    /// (answered and unanswered) when formatting the tool result.
    pub fn into_response(self, questions: Vec<Question>) -> UserQuestionResponse {
        match self {
            Self::Accepted {
                answers,
                annotations,
            } => UserQuestionResponse::Accepted {
                answers,
                annotations,
            },
            Self::ChatAboutThis { partial_answers } => UserQuestionResponse::ChatAboutThis {
                questions,
                partial_answers,
            },
            Self::SkipInterview { partial_answers } => UserQuestionResponse::SkipInterview {
                questions,
                partial_answers,
            },
            Self::Cancelled => UserQuestionResponse::Cancelled,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helpers --

    fn sample_questions() -> Vec<Question> {
        vec![
            Question {
                question: "Which database?".to_string(),
                options: vec![
                    super::super::QuestionOption {
                        label: "Redis".to_string(),
                        description: "In-memory store".to_string(),
                        preview: Some("<div>redis preview</div>".to_string()),
                        id: None,
                    },
                    super::super::QuestionOption {
                        label: "Postgres".to_string(),
                        description: "Relational DB".to_string(),
                        preview: None,
                        id: None,
                    },
                ],
                multi_select: None,
                id: None,
            },
            Question {
                question: "Which framework?".to_string(),
                options: vec![
                    super::super::QuestionOption {
                        label: "React".to_string(),
                        description: "UI library".to_string(),
                        preview: None,
                        id: None,
                    },
                    super::super::QuestionOption {
                        label: "Vue".to_string(),
                        description: "Progressive framework".to_string(),
                        preview: None,
                        id: None,
                    },
                ],
                multi_select: None,
                id: None,
            },
        ]
    }

    // -- AskUserQuestionMode serde --

    #[test]
    fn mode_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(AskUserQuestionMode::Default).unwrap(),
            serde_json::json!("default")
        );
        assert_eq!(
            serde_json::to_value(AskUserQuestionMode::Plan).unwrap(),
            serde_json::json!("plan")
        );
    }

    #[test]
    fn mode_round_trips() {
        for mode in [AskUserQuestionMode::Default, AskUserQuestionMode::Plan] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: AskUserQuestionMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    // -- AskUserQuestionExtRequest serde --

    #[test]
    fn ext_request_serializes_camel_case() {
        let req = AskUserQuestionExtRequest {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc-1".to_string(),
            questions: vec![],
            mode: AskUserQuestionMode::Plan,
            timeout_secs: Some(45),
        };
        let json = serde_json::to_value(&req).unwrap();
        // camelCase field names
        assert!(json.get("sessionId").is_some());
        assert!(json.get("toolCallId").is_some());
        assert_eq!(json["mode"], "plan");
        assert_eq!(json["timeoutSecs"], 45);
    }

    #[test]
    fn ext_request_round_trips() {
        let req = AskUserQuestionExtRequest {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc-1".to_string(),
            questions: sample_questions(),
            mode: AskUserQuestionMode::Default,
            timeout_secs: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: AskUserQuestionExtRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "sess-1");
        assert_eq!(back.tool_call_id, "tc-1");
        assert_eq!(back.questions.len(), 2);
        assert_eq!(back.mode, AskUserQuestionMode::Default);
        assert_eq!(back.timeout_secs, None);
    }

    // -- AskUserQuestionExtResponse serde --

    #[test]
    fn ext_response_accepted_serializes_tagged() {
        let mut answers = IndexMap::new();
        answers.insert("Which database?".to_string(), vec!["Redis".to_string()]);

        let mut annotations = HashMap::new();
        annotations.insert(
            "Which database?".to_string(),
            QuestionAnnotation {
                preview: Some("<div>redis preview</div>".to_string()),
                notes: None,
            },
        );

        let resp = AskUserQuestionExtResponse::Accepted {
            answers,
            annotations: Some(annotations),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["outcome"], "accepted");
        assert!(json["answers"].is_object());
        assert!(json["annotations"].is_object());
    }

    #[test]
    fn ext_response_accepted_omits_empty_annotations() {
        let mut answers = IndexMap::new();
        answers.insert("Q?".to_string(), vec!["A".to_string()]);

        let resp = AskUserQuestionExtResponse::Accepted {
            answers,
            annotations: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["outcome"], "accepted");
        assert!(json.get("annotations").is_none());
    }

    #[test]
    fn ext_response_chat_about_this_serializes() {
        let mut partial = HashMap::new();
        partial.insert("Which database?".to_string(), "Redis".to_string());

        let resp = AskUserQuestionExtResponse::ChatAboutThis {
            partial_answers: partial,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["outcome"], "chat_about_this");
        assert!(json["partial_answers"].is_object());
    }

    #[test]
    fn ext_response_skip_interview_serializes() {
        let resp = AskUserQuestionExtResponse::SkipInterview {
            partial_answers: HashMap::new(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["outcome"], "skip_interview");
    }

    #[test]
    fn ext_response_cancelled_serializes() {
        let resp = AskUserQuestionExtResponse::Cancelled;
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["outcome"], "cancelled");
    }

    #[test]
    fn ext_response_round_trips_all_variants() {
        // Accepted
        let mut answers = IndexMap::new();
        answers.insert("Q1?".to_string(), vec!["A1".to_string()]);
        let accepted = AskUserQuestionExtResponse::Accepted {
            answers,
            annotations: None,
        };
        let json = serde_json::to_string(&accepted).unwrap();
        let back: AskUserQuestionExtResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, AskUserQuestionExtResponse::Accepted { .. }));

        // ChatAboutThis
        let chat = AskUserQuestionExtResponse::ChatAboutThis {
            partial_answers: HashMap::new(),
        };
        let json = serde_json::to_string(&chat).unwrap();
        let back: AskUserQuestionExtResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            AskUserQuestionExtResponse::ChatAboutThis { .. }
        ));

        // SkipInterview
        let skip = AskUserQuestionExtResponse::SkipInterview {
            partial_answers: HashMap::new(),
        };
        let json = serde_json::to_string(&skip).unwrap();
        let back: AskUserQuestionExtResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            AskUserQuestionExtResponse::SkipInterview { .. }
        ));

        // Cancelled
        let cancel = AskUserQuestionExtResponse::Cancelled;
        let json = serde_json::to_string(&cancel).unwrap();
        let back: AskUserQuestionExtResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, AskUserQuestionExtResponse::Cancelled));
    }

    // -- into_response conversion --

    #[test]
    fn into_response_accepted() {
        let mut answers = IndexMap::new();
        answers.insert("Which database?".to_string(), vec!["Redis".to_string()]);

        let mut annotations = HashMap::new();
        annotations.insert(
            "Which database?".to_string(),
            QuestionAnnotation {
                preview: Some("preview".to_string()),
                notes: Some("my notes".to_string()),
            },
        );

        let ext = AskUserQuestionExtResponse::Accepted {
            answers: answers.clone(),
            annotations: Some(annotations),
        };
        let resp = ext.into_response(sample_questions());

        match resp {
            UserQuestionResponse::Accepted {
                answers: a,
                annotations: ann,
            } => {
                assert_eq!(a, answers);
                assert!(ann.is_some());
                let ann = ann.unwrap();
                assert!(ann.contains_key("Which database?"));
            }
            other => panic!("Expected Accepted, got {:?}", other),
        }
    }

    #[test]
    fn into_response_chat_about_this_carries_questions() {
        let mut partial = HashMap::new();
        partial.insert("Which database?".to_string(), "Redis".to_string());

        let questions = sample_questions();
        let ext = AskUserQuestionExtResponse::ChatAboutThis {
            partial_answers: partial.clone(),
        };
        let resp = ext.into_response(questions.clone());

        match resp {
            UserQuestionResponse::ChatAboutThis {
                questions: q,
                partial_answers: p,
            } => {
                assert_eq!(q.len(), questions.len());
                assert_eq!(p, partial);
            }
            other => panic!("Expected ChatAboutThis, got {:?}", other),
        }
    }

    #[test]
    fn into_response_skip_interview_carries_questions() {
        let questions = sample_questions();
        let ext = AskUserQuestionExtResponse::SkipInterview {
            partial_answers: HashMap::new(),
        };
        let resp = ext.into_response(questions.clone());

        match resp {
            UserQuestionResponse::SkipInterview {
                questions: q,
                partial_answers: p,
            } => {
                assert_eq!(q.len(), questions.len());
                assert!(p.is_empty());
            }
            other => panic!("Expected SkipInterview, got {:?}", other),
        }
    }

    #[test]
    fn into_response_cancelled() {
        let ext = AskUserQuestionExtResponse::Cancelled;
        let resp = ext.into_response(sample_questions());
        assert!(matches!(resp, UserQuestionResponse::Cancelled));
    }

    // -- QuestionAnnotation serde --

    #[test]
    fn annotation_omits_none_fields() {
        let ann = QuestionAnnotation {
            preview: None,
            notes: None,
        };
        let json = serde_json::to_value(&ann).unwrap();
        assert!(json.get("preview").is_none());
        assert!(json.get("notes").is_none());
    }

    #[test]
    fn annotation_includes_present_fields() {
        let ann = QuestionAnnotation {
            preview: Some("prev".to_string()),
            notes: Some("note".to_string()),
        };
        let json = serde_json::to_value(&ann).unwrap();
        assert_eq!(json["preview"], "prev");
        assert_eq!(json["notes"], "note");
    }

    // -- Backwards-compatible deserialization (string -> vec) --

    #[test]
    fn deserialize_accepted_old_string_format() {
        let raw = r#"{
            "outcome": "accepted",
            "answers": {"Which cache?": "Only hot-path caches"}
        }"#;
        let resp: AskUserQuestionExtResponse = serde_json::from_str(raw).unwrap();
        match resp {
            AskUserQuestionExtResponse::Accepted { answers, .. } => {
                assert_eq!(
                    answers["Which cache?"],
                    vec!["Only hot-path caches".to_string()]
                );
            }
            other => panic!("Expected Accepted, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_accepted_mixed_string_and_vec() {
        let raw = r#"{
            "outcome": "accepted",
            "answers": {"Q1?": "old-style", "Q2?": ["new-style"]}
        }"#;
        let resp: AskUserQuestionExtResponse = serde_json::from_str(raw).unwrap();
        match resp {
            AskUserQuestionExtResponse::Accepted { answers, .. } => {
                assert_eq!(answers["Q1?"], vec!["old-style".to_string()]);
                assert_eq!(answers["Q2?"], vec!["new-style".to_string()]);
            }
            other => panic!("Expected Accepted, got {:?}", other),
        }
    }

    // -- Deserialization from raw JSON (simulating pager responses) --

    #[test]
    fn deserialize_accepted_from_raw_json() {
        let raw = r#"{
            "outcome": "accepted",
            "answers": {"Which database?": ["Redis"]},
            "annotations": {
                "Which database?": {
                    "preview": "<div>redis preview</div>"
                }
            }
        }"#;
        let resp: AskUserQuestionExtResponse = serde_json::from_str(raw).unwrap();
        match resp {
            AskUserQuestionExtResponse::Accepted {
                answers,
                annotations,
            } => {
                assert_eq!(answers["Which database?"], vec!["Redis".to_string()]);
                let ann = annotations.unwrap();
                assert_eq!(
                    ann["Which database?"].preview.as_deref(),
                    Some("<div>redis preview</div>")
                );
                assert!(ann["Which database?"].notes.is_none());
            }
            other => panic!("Expected Accepted, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_accepted_without_annotations() {
        let raw = r#"{"outcome": "accepted", "answers": {"Q?": ["A"]}}"#;
        let resp: AskUserQuestionExtResponse = serde_json::from_str(raw).unwrap();
        match resp {
            AskUserQuestionExtResponse::Accepted { annotations, .. } => {
                assert!(annotations.is_none());
            }
            other => panic!("Expected Accepted, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_chat_about_this_empty_partials() {
        let raw = r#"{"outcome": "chat_about_this"}"#;
        let resp: AskUserQuestionExtResponse = serde_json::from_str(raw).unwrap();
        match resp {
            AskUserQuestionExtResponse::ChatAboutThis { partial_answers } => {
                assert!(partial_answers.is_empty());
            }
            other => panic!("Expected ChatAboutThis, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_cancelled() {
        let raw = r#"{"outcome": "cancelled"}"#;
        let resp: AskUserQuestionExtResponse = serde_json::from_str(raw).unwrap();
        assert!(matches!(resp, AskUserQuestionExtResponse::Cancelled));
    }

    // -- IndexMap ordering preservation --

    #[test]
    fn accepted_answers_preserve_insertion_order() {
        let mut answers = IndexMap::new();
        answers.insert("Third?".to_string(), vec!["C".to_string()]);
        answers.insert("First?".to_string(), vec!["A".to_string()]);
        answers.insert("Second?".to_string(), vec!["B".to_string()]);

        let resp = AskUserQuestionExtResponse::Accepted {
            answers,
            annotations: None,
        };

        // Round-trip through JSON
        let json = serde_json::to_string(&resp).unwrap();
        let back: AskUserQuestionExtResponse = serde_json::from_str(&json).unwrap();

        if let AskUserQuestionExtResponse::Accepted { answers, .. } = back {
            let keys: Vec<&String> = answers.keys().collect();
            assert_eq!(keys, vec!["Third?", "First?", "Second?"]);
        } else {
            panic!("Expected Accepted");
        }
    }
}
