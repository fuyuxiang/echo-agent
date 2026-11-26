//! Personal memory mutations and source-grounded conversation recall.
use super::*;
use echo_agent_memory::{MemoryStorage, records};
use echo_agent_tool_protocol::{ToolCapabilities, ToolId};
use echo_agent_tool_runtime::{ListToolsContext, Tool, ToolCallContext, ToolError};
use echo_agent_tool_types::ToolDescription;
use echo_agent_tools::types::output::ToolOutput;
use schemars::JsonSchema;
use serde::Deserialize;

#[path = "memory_tool_listing.rs"]
mod memory_tool_listing;
use memory_tool_listing::model_memory_list_response;

#[derive(Deserialize, JsonSchema)]
pub(super) struct ManageMemoryInput {
    /// list, remember, correct, forget, or complete. Forget accepts id OR topic.
    action: String,
    /// global for user preferences; workspace for project-specific facts.
    #[serde(default = "workspace_scope")]
    scope: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    /// user, feedback, project, reference, or task.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    valid_until: Option<i64>,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
    /// Include completed/expired history, never forgotten records.
    #[serde(default)]
    include_inactive: bool,
}
fn workspace_scope() -> String {
    "workspace".into()
}

#[derive(Debug)]
pub(super) struct ManageMemoryTool {
    storage: MemoryStorage,
    session_id: String,
}
impl echo_agent_tools::types::tool_metadata::ToolMetadata for ManageMemoryTool {
    fn kind(&self) -> echo_agent_tools::types::tool::ToolKind {
        echo_agent_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> echo_agent_tools::types::tool::ToolNamespace {
        echo_agent_tools::types::tool::ToolNamespace::EchoAgentBuild
    }
    fn description_template(&self) -> &str {
        "Manage saved personal memories: list, remember, correct, forget, or complete. Use only for explicit user instructions; list IDs/revisions before changing an existing memory. Global stores cross-project preferences, workspace stores project facts."
    }
    fn is_read_only(&self) -> bool {
        false
    }
}
impl echo_agent_tools::types::tool_metadata::ToolMetadata for ConversationHistoryTool {
    fn kind(&self) -> echo_agent_tools::types::tool::ToolKind {
        echo_agent_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> echo_agent_tools::types::tool::ToolNamespace {
        echo_agent_tools::types::tool::ToolNamespace::EchoAgentBuild
    }
    fn description_template(&self) -> &str {
        "Search and read actual past conversations. Search with query/date filters, then read a session_id and cite numbered source messages. Current workspace by default; across_workspaces only when requested. Empty query lists recent discussions. Excludes forgotten sources and reasoning."
    }
    fn is_read_only(&self) -> bool {
        true
    }
}
fn error(name: &str, message: impl ToString) -> ToolError {
    ToolError::execution(ToolId::new(name).unwrap(), message.to_string())
}

impl Tool for ManageMemoryTool {
    type Args = ManageMemoryInput;
    type Output = ToolOutput;
    fn id(&self) -> ToolId {
        ToolId::new("memory_manage").unwrap()
    }
    fn description(&self, _: &ListToolsContext) -> ToolDescription {
        ToolDescription::new(
            "memory_manage",
            "List or manage saved memories. Use for the user's explicit requests to remember, correct, forget a topic, or complete a tracked task. List first to obtain IDs and revisions before correcting or forgetting a particular record. Use global for cross-project personal preferences/background; workspace for current-project facts. Never claim a write succeeded without this tool succeeding. Forgotten records are never returned, even with include_inactive. An empty list is not permission to recover forgotten facts from old revisions or internal memory files; when no reliable evidence remains, say you do not know. Forgotten source conversations are excluded from automatic re-extraction and assistant history retrieval. Do not use this tool to store secrets or facts inferred only from third-party documents.",
        )
    }
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities {
            is_read_only: false,
            ..Default::default()
        }
    }
    fn should_list(&self, _: &ListToolsContext) -> bool {
        self.storage.enabled()
    }
    async fn run(&self, _: ToolCallContext, input: Self::Args) -> Result<Self::Output, ToolError> {
        if !self.storage.enabled() {
            return Err(error("memory_manage", "记忆已关闭，请先在设置中开启"));
        }
        if input.id.is_some() && input.expected_revision.is_none() {
            return Err(error("memory_manage", "修改前请先读取记忆 ID 和 revision"));
        }
        let result: Result<serde_json::Value, std::io::Error> = (|| match input.action.as_str() {
            "list" => Ok(model_memory_list_response(
                records::list(&self.storage, true)?,
                &input.query,
                input.include_inactive,
                input.offset,
                input.limit,
            )),
            "remember" | "correct" => {
                if input.action == "correct"
                    && (input.id.is_none() || input.expected_revision.is_none())
                {
                    return Err(std::io::Error::other("更正前请先读取记忆 ID 和 revision"));
                }
                let previous = input.id.as_ref().and_then(|id| {
                    records::list(&self.storage, true)
                        .ok()?
                        .into_iter()
                        .find(|r| &r.id == id)
                });
                let record = records::remember(
                    &self.storage,
                    records::RecordInput {
                        id: input.id,
                        scope: previous
                            .as_ref()
                            .map(|r| r.scope.clone())
                            .unwrap_or(input.scope),
                        kind: input
                            .kind
                            .or_else(|| previous.as_ref().map(|r| r.kind.clone()))
                            .unwrap_or_else(|| "project".into()),
                        key: input
                            .key
                            .or_else(|| previous.as_ref().map(|r| r.key.clone()))
                            .unwrap_or_default(),
                        content: input.content.unwrap_or_default(),
                        evidence: input.evidence.unwrap_or_default(),
                        pinned: true,
                        valid_until: input.valid_until,
                        expected_revision: input.expected_revision,
                        observed_at: None,
                        key_is_custom: None,
                    },
                    Some(&self.session_id),
                    false,
                )?;
                Ok(serde_json::json!({"saved": record}))
            }
            "forget" if input.id.is_none() => {
                let count = records::forget_topic(
                    &self.storage,
                    &input.scope,
                    input.topic.as_deref().unwrap_or(""),
                )?;
                Ok(serde_json::json!({"forgotten": count, "exclusionSaved": true}))
            }
            "forget" | "complete" => {
                let previous = records::list(&self.storage, true)?
                    .into_iter()
                    .find(|r| Some(&r.id) == input.id.as_ref());
                records::retire(
                    &self.storage,
                    previous
                        .as_ref()
                        .map(|r| r.scope.as_str())
                        .unwrap_or(&input.scope),
                    input.id.as_deref().unwrap_or(""),
                    input.expected_revision,
                    input.action == "complete",
                )?;
                Ok(serde_json::json!({"completed": true}))
            }
            _ => Err(std::io::Error::other("unsupported memory action")),
        })();
        result
            .map(|value| ToolOutput::Text(value.to_string().into()))
            .map_err(|e| error("memory_manage", e))
    }
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct ConversationHistoryInput {
    /// search or read. Search returns source IDs; read returns numbered messages.
    action: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    session_id: Option<String>,
    /// Search current workspace by default. Set true only when the user's request calls for other projects.
    #[serde(default)]
    across_workspaces: bool,
    /// Inclusive UTC date YYYY-MM-DD.
    #[serde(default)]
    since: Option<String>,
    /// Inclusive UTC date YYYY-MM-DD.
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    offset: usize,
    /// Continue a bounded fallback scan using nextScanOffset from the previous result.
    #[serde(default)]
    scan_offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}
#[derive(Debug)]
pub(super) struct ConversationHistoryTool {
    storage: MemoryStorage,
    current_session: String,
}

pub(super) fn message_text(item: &ConversationItem) -> Option<(&'static str, String)> {
    match item {
        ConversationItem::User(user) if user.synthetic_reason.is_none() => {
            let text = user
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let real =
                crate::session::helpers::session_compact::extract_real_user_queries(
                    &[item.clone()],
                );
            Some(("user", real.into_iter().next().unwrap_or(text)))
        }
        ConversationItem::Assistant(assistant) if !assistant.content.is_empty() => Some((
            "assistant",
            echo_agent_memory::text_utils::strip_reasoning_blocks(&assistant.content),
        )),
        _ => None,
    }
}

impl Tool for ConversationHistoryTool {
    type Args = ConversationHistoryInput;
    type Output = ToolOutput;
    fn id(&self) -> ToolId {
        ToolId::new("conversation_history").unwrap()
    }
    fn description(&self, _: &ListToolsContext) -> ToolDescription {
        ToolDescription::new(
            "conversation_history",
            "Find actual past conversations when the user refers to a previous event, report, choice or discussion. Search with a self-contained query and optional dates, then read a returned session_id with offset/limit to inspect the original numbered user/assistant messages. Cite session ID and message number when recalling material details. Empty query lists recent conversations. Only the current workspace is searched unless across_workspaces is explicitly requested. Never guess what was decided from a title alone. System instructions, reasoning, tool secrets and forgotten source conversations are excluded.",
        )
    }
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities {
            is_read_only: true,
            ..Default::default()
        }
    }
    fn should_list(&self, _: &ListToolsContext) -> bool {
        records::setting("memory", None, "enabled", true)
    }
    async fn run(&self, _: ToolCallContext, input: Self::Args) -> Result<Self::Output, ToolError> {
        use crate::session::storage::{StorageAdapter, jsonl::JsonlStorageAdapter};
        if !records::setting("memory", None, "enabled", true) {
            return Err(error("conversation_history", "记忆已关闭"));
        }
        if input.query.chars().count() > 1024
            || input.offset > 100_000
            || input.scan_offset > 100_000
        {
            return Err(error("conversation_history", "查询或偏移量过大"));
        }
        let adapter = JsonlStorageAdapter::new();
        let cwd = self.storage.workspace_path().to_string_lossy().to_string();
        let mut summaries = adapter
            .list_sessions(if input.across_workspaces {
                None
            } else {
                Some(&cwd)
            })
            .await
            .map_err(|e| error("conversation_history", e))?;
        let parse_date = |value: &Option<String>| -> Result<Option<chrono::NaiveDate>, ToolError> {
            value
                .as_deref()
                .map(|s| {
                    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .map_err(|_| error("conversation_history", "日期格式应为 YYYY-MM-DD"))
                })
                .transpose()
        };
        let since = parse_date(&input.since)?;
        let until = parse_date(&input.until)?;
        if since.zip(until).is_some_and(|(start, end)| start > end) {
            return Err(error("conversation_history", "开始日期不能晚于结束日期"));
        }
        summaries.retain(|s| {
            s.info.id.0.as_ref() != self.current_session
                && s.session_kind
                    .as_deref()
                    .is_none_or(|k| !k.contains("subagent"))
                && since.is_none_or(|date| s.updated_at.date_naive() >= date)
                && until.is_none_or(|date| s.created_at.date_naive() <= date)
        });
        summaries.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        let limit = input.limit.unwrap_or(10).clamp(1, 40);
        if input.action == "read" {
            let summary = summaries
                .into_iter()
                .find(|s| Some(s.info.id.0.as_ref()) == input.session_id.as_deref())
                .ok_or_else(|| error("conversation_history", "在所选范围内找不到该会话"))?;
            // Cross-project recall must also honor that project's forget ledger.
            let source_storage = MemoryStorage::new(std::path::Path::new(&summary.info.cwd), None);
            if records::source_blocked(&source_storage, &summary.info.id.0).unwrap_or(true) {
                return Err(error(
                    "conversation_history",
                    "该会话已从助手的记忆来源中排除",
                ));
            }
            let data = adapter
                .load_session_without_updates(&summary.info)
                .await
                .map_err(|e| error("conversation_history", e))?;
            let mut messages = Vec::new();
            let mut bytes = 0;
            let mut next = input.offset;
            for (index, item) in data.chat_history.iter().enumerate().skip(input.offset) {
                if let Some((role, text)) = message_text(item) {
                    if !records::permitted(&source_storage, &text).unwrap_or(false) {
                        next = index + 1;
                        continue;
                    }
                    let text: String = text.chars().take(6000).collect();
                    bytes += text.len();
                    if bytes > 48_000 || messages.len() >= limit {
                        break;
                    }
                    messages.push(
                        serde_json::json!({"message": index + 1, "role": role, "text": text}),
                    );
                }
                next = index + 1;
            }
            let title =
                if records::permitted(&source_storage, summary.display_title()).unwrap_or(false) {
                    summary.display_title()
                } else {
                    "历史对话"
                };
            return Ok(ToolOutput::Text(serde_json::json!({"sessionId": summary.info.id, "title": title, "cwd": summary.info.cwd, "messages": messages, "nextOffset": if next < data.chat_history.len() { Some(next) } else { None }}).to_string().into()));
        }
        if input.action != "search" {
            return Err(error("conversation_history", "action 应为 search 或 read"));
        }
        let keywords = echo_agent_memory::query_expansion::extract_keywords(&input.query);
        let cjk = echo_agent_memory::query_expansion::extract_cjk_bigrams(&input.query);
        let mut matches = Vec::new();
        // The persisted FTS index supplies old matches without scanning all logs.
        let root = crate::util::echo_agent_home::echo_agent_home();
        let indexed = crate::session::storage::search::execute_search(
            crate::session::storage::search::IndexDecision::Off,
            &root,
            &crate::session::storage::search::SessionSearchRequest {
                query: input.query.clone(),
                cwd: if input.across_workspaces {
                    None
                } else {
                    Some(cwd)
                },
                limit: 200,
                offset: 0,
                include_content: true,
            },
        )
        .await
        .ok();
        let indexed_ids: std::collections::HashSet<String> = indexed
            .as_ref()
            .map(|v| v.results.iter().map(|r| r.session_id.clone()).collect())
            .unwrap_or_default();
        let summary_count = summaries.len();
        let mut selected: Vec<_> = summaries
            .iter()
            .filter(|s| indexed_ids.contains(s.info.id.0.as_ref()))
            .take(200)
            .cloned()
            .collect();
        let selected_ids: std::collections::HashSet<_> =
            selected.iter().map(|s| s.info.id.to_string()).collect();
        selected.extend(
            summaries
                .into_iter()
                .skip(input.scan_offset)
                .take(200)
                .filter(|s| !selected_ids.contains(s.info.id.0.as_ref())),
        );
        let scanned = selected.len();
        for summary in selected {
            let source_storage = MemoryStorage::new(std::path::Path::new(&summary.info.cwd), None);
            if records::source_blocked(&source_storage, &summary.info.id.0).unwrap_or(true) {
                continue;
            }
            let title = summary.display_title().to_owned();
            let title = if records::permitted(&source_storage, &title).unwrap_or(false) {
                title
            } else {
                "历史对话".into()
            };
            let mut snippet = title.clone();
            let mut score = usize::from(indexed_ids.contains(summary.info.id.0.as_ref())) * 100;
            let mut matched_message = false;
            if !input.query.is_empty() {
                if let Ok(data) = adapter.load_session_without_updates(&summary.info).await {
                    for item in data.chat_history.iter() {
                        if let Some((_, text)) = message_text(item) {
                            if !records::permitted(&source_storage, &text).unwrap_or(false) {
                                continue;
                            }
                            let lowered = text.to_lowercase();
                            let hits = keywords
                                .iter()
                                .filter(|k| lowered.contains(k.as_str()))
                                .count()
                                + cjk.iter().filter(|k| text.contains(k.as_str())).count();
                            if hits > score % 100 {
                                score = score / 100 * 100 + hits.min(99);
                                snippet = text.chars().take(1000).collect();
                                matched_message = true;
                            }
                        }
                    }
                }
                if !matched_message {
                    continue;
                }
            }
            matches.push((score, serde_json::json!({"sessionId": summary.info.id, "cwd": summary.info.cwd, "title": title, "updatedAt": summary.updated_at, "snippet": snippet})));
        }
        matches.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        let total = matches.len();
        Ok(ToolOutput::Text(serde_json::json!({"results": matches.into_iter().skip(input.offset).take(limit).map(|(_, v)| v).collect::<Vec<_>>(), "matches": total, "scanned": scanned, "nextScanOffset": if input.scan_offset + 200 < summary_count { Some(input.scan_offset + 200) } else { None }, "nextOffset": if input.offset + limit < total { Some(input.offset + limit) } else { None }}).to_string().into()))
    }
}

impl SessionActor {
    pub(super) async fn register_memory_extras(
        &self,
        bridge: &echo_agent_tools::bridge::ToolBridge,
    ) -> Result<(), String> {
        let Some(storage) = self.memory.storage() else {
            return Ok(());
        };
        bridge
            .register_mcp_tools(
                "memory_manage".to_owned(),
                ManageMemoryTool {
                    storage: storage.clone(),
                    session_id: self.session_info.id.to_string(),
                },
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        // Scoped experts/groups cannot inspect the user's unrelated conversations.
        if !self.startup_hints.is_subagent
            && storage.global_dir()
                == crate::util::echo_agent_home::echo_agent_home().join("memory")
        {
            bridge
                .register_mcp_tools(
                    "conversation_history".to_owned(),
                    ConversationHistoryTool {
                        storage,
                        current_session: self.session_info.id.to_string(),
                    },
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
