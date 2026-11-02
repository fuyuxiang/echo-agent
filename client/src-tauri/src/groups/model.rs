use serde::{Deserialize, Serialize};

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    pub name: String,
    pub expert: Option<String>,
    pub role: String,
    pub profile: serde_json::Value,
    pub model_id: String,
    pub session_id: Option<String>,
    pub discussion_session_id: Option<String>,
    pub seen_seq: u64,
    #[serde(default)]
    pub discussion_seen_seq: u64,
    pub generation: u64,
    /// Explicit memory reset also fences historical recall and re-extraction.
    #[serde(default)]
    pub history_start_seq: u64,
    #[serde(default)]
    pub removed: bool,
}

impl Member {
    pub fn reset_context(&mut self, isolate_memory: bool) {
        self.session_id = None;
        self.discussion_session_id = None;
        self.seen_seq = self.history_start_seq;
        self.discussion_seen_seq = self.history_start_seq;
        if isolate_memory {
            self.generation += 1;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub seq: u64,
    pub member_id: Option<String>,
    pub kind: String,
    pub text: String,
    pub created_at: u64,
    pub work_id: Option<String>,
    pub reply_to: Option<String>,
    pub attachments: Vec<String>,
    pub tool_calls: Vec<serde_json::Value>,
    #[serde(default)]
    pub knowledge: Knowledge,
    /// Follow-ups share the original request instead of spawning another team.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Explicit recipients of a supplement; empty only for legacy records.
    #[serde(default)]
    pub member_ids: Vec<String>,
    #[serde(default)]
    pub stop_root_id: Option<String>,
    #[serde(default)]
    pub request_owner_id: Option<String>,
    /// Manual result summaries retain the original request's inputs on every retry.
    #[serde(default)]
    pub source_request_id: Option<String>,
    #[serde(default)]
    pub assigned_work_ids: Vec<String>,
    /// Changes only when public text or attachments change, never for tool progress.
    #[serde(default)]
    pub public_revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Knowledge {
    #[serde(default)]
    pub personal: bool,
    #[serde(default)]
    pub organization: bool,
    #[serde(default)]
    pub organization_scope_ids: Vec<String>,
    #[serde(default)]
    pub context: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Work {
    pub id: String,
    pub root_id: String,
    pub parent_id: Option<String>,
    pub continuation_of: Option<String>,
    pub member_id: String,
    pub prompt: String,
    pub status: String,
    pub discussion: bool,
    pub epoch: u64,
    pub depth: u32,
    pub created_at: u64,
    pub session_id: Option<String>,
    pub prompt_id: String,
    pub output: String,
    pub error: Option<String>,
    pub tokens: u64,
    pub summary: bool,
    /// A single read-only recovery of a model turn which returned no public answer.
    #[serde(default)]
    pub result_repair: bool,
    #[serde(default)]
    pub seen_events: Vec<String>,
    #[serde(default)]
    pub output_filter: echo_agent_memory::text_utils::PublicText,
    #[serde(default)]
    pub context_seq: u64,
    #[serde(default)]
    pub consumed_followups: Vec<String>,
    /// Terminal child results already included when preparing this prompt.
    #[serde(default)]
    pub context_children: Vec<String>,
}

impl Work {
    pub fn read_only(&self) -> bool {
        self.discussion || self.summary || self.result_repair
    }
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "dispatching" | "running" | "stopping")
    }
    pub fn terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed" | "cancelled")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub id: String,
    pub text: String,
    pub message_id: Option<String>,
    pub created_at: u64,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub voided: bool,
    #[serde(default)]
    pub history: Vec<DecisionVersion>,
    #[serde(default)]
    pub updated_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionVersion {
    #[serde(default)]
    pub action: String,
    pub text: String,
    pub changed_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub cwd: String,
    pub source_session_id: Option<String>,
    pub source_team_id: Option<String>,
    pub coordinator_id: String,
    pub permission_mode: String,
    pub members: Vec<Member>,
    pub messages: Vec<Message>,
    pub works: Vec<Work>,
    pub decisions: Vec<Decision>,
    pub archived: bool,
    pub pinned: bool,
    pub paused: bool,
    pub epoch: u64,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Public delivery is projected from the durable request, work and message records.
/// Model completion alone is never evidence that the user received an answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestView {
    pub id: String,
    pub owner_id: String,
    pub work_id: String,
    pub input_revision: u64,
    pub state: String,
    pub final_message_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSnapshot {
    #[serde(flatten)]
    pub group: Group,
    #[serde(default)]
    pub requests: Vec<RequestView>,
    #[serde(default)]
    pub public_seq: u64,
    #[serde(default)]
    pub latest_request_id: Option<String>,
}

impl Group {
    pub fn snapshot(mut self) -> GroupSnapshot {
        use std::collections::{HashMap, HashSet};
        // Restore only persisted public text in the response projection. A
        // propagated continuation result must not become another public reply.
        let continued = self
            .works
            .iter()
            .filter_map(|w| w.continuation_of.as_deref())
            .collect::<HashSet<_>>();
        let retained = self
            .works
            .iter()
            .filter(|w| !w.output.trim().is_empty() && !continued.contains(w.id.as_str()))
            .map(|w| {
                (
                    w.id.clone(),
                    w.member_id.clone(),
                    w.output.clone(),
                    w.created_at,
                )
            })
            .collect::<Vec<_>>();
        let mut reply_indices = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.kind == "assistant")
            .filter_map(|(index, m)| m.work_id.as_ref().map(|work| (work.clone(), index)))
            .collect::<HashMap<_, _>>();
        let activity_seq = self
            .messages
            .iter()
            .filter_map(|m| m.work_id.as_ref().map(|work| (work.clone(), m.seq)))
            .collect::<HashMap<_, _>>();
        let fallback_seq = self.messages.iter().map(|m| m.seq).max().unwrap_or(0);
        for (work, member, output, created) in retained {
            if let Some(index) = reply_indices.get(&work) {
                let message = &mut self.messages[*index];
                if message.text.is_empty() || output.starts_with(&message.text) {
                    message.text = output;
                }
            } else {
                let message = self.post(Some(member), "assistant", output, Some(work.clone()));
                message.id = format!("result-{work}");
                message.seq = activity_seq.get(&work).copied().unwrap_or(fallback_seq);
                message.created_at = created;
                reply_indices.insert(work, self.messages.len() - 1);
            }
        }
        let mut roots = Vec::new();
        let mut by_root = HashMap::<&str, Vec<&Work>>::new();
        for work in &self.works {
            if !by_root.contains_key(work.root_id.as_str()) {
                roots.push(work.root_id.as_str());
            }
            by_root.entry(&work.root_id).or_default().push(work);
        }
        let mut replies = HashMap::<&str, (usize, &Message)>::new();
        let mut revisions = HashMap::<&str, u64>::new();
        let owners = self
            .messages
            .iter()
            .filter_map(|m| {
                m.request_owner_id
                    .as_ref()
                    .map(|owner| (m.id.as_str(), owner))
            })
            .collect::<HashMap<_, _>>();
        for (index, message) in self.messages.iter().enumerate() {
            if message.kind == "assistant" && !message.text.trim().is_empty() {
                if let Some(work) = &message.work_id {
                    replies.insert(work, (index, message));
                }
            }
            if message.kind == "user" {
                let root = message.request_id.as_deref().unwrap_or(&message.id);
                let revision = revisions.entry(root).or_default();
                *revision = (*revision).max(message.seq);
            }
        }
        let stopped = self
            .messages
            .iter()
            .filter_map(|m| m.stop_root_id.as_deref())
            .collect::<HashSet<_>>();
        let requests = roots
            .into_iter()
            .map(|root| {
                let works = &by_root[root];
                let owner = owners
                    .get(root)
                    .map(|owner| (*owner).clone())
                    .unwrap_or_else(|| {
                        let initial = works
                            .iter()
                            .filter(|w| {
                                w.parent_id.is_none() && w.continuation_of.is_none() && !w.summary
                            })
                            .collect::<Vec<_>>();
                        if initial.len() == 1 {
                            initial[0].member_id.clone()
                        } else if initial.is_empty() {
                            works[0].member_id.clone()
                        } else {
                            works
                                .iter()
                                .find(|w| w.summary)
                                .map(|w| w.member_id.clone())
                                .unwrap_or_else(|| self.coordinator_id.clone())
                        }
                    });
                let work = works
                    .iter()
                    .rev()
                    .find(|w| w.member_id == owner)
                    .unwrap_or(&works[0]);
                let final_message = (work.member_id == owner && work.status == "completed")
                    .then(|| replies.get(work.id.as_str()))
                    .flatten()
                    .map(|(_, message)| *message);
                let state = if works.iter().any(|w| w.status == "uncertain") {
                    "needs_check"
                } else if works.iter().any(|w| w.status == "stopping") {
                    "stopping"
                } else if works
                    .iter()
                    .any(|w| !w.terminal() && (w.summary || w.result_repair))
                {
                    "summarizing"
                } else if works.iter().any(|w| w.active()) {
                    "working"
                } else if works.iter().any(|w| w.status == "waiting_children") {
                    "waiting"
                } else if works.iter().any(|w| w.status == "queued") {
                    "queued"
                } else if works
                    .iter()
                    .any(|w| w.member_id == owner && w.status == "cancelled")
                    || stopped.contains(root)
                {
                    "stopped"
                } else if final_message.is_some() {
                    if works
                        .iter()
                        .any(|w| matches!(w.status.as_str(), "failed" | "cancelled"))
                    {
                        "partial"
                    } else {
                        "delivered"
                    }
                } else if works.iter().any(|w| w.status == "failed") {
                    "failed"
                } else {
                    "missing_result"
                };
                RequestView {
                    id: root.into(),
                    owner_id: owner.clone(),
                    work_id: work.id.clone(),
                    input_revision: revisions.get(root).copied().unwrap_or(0),
                    state: state.into(),
                    final_message_id: if matches!(state, "delivered" | "partial") {
                        final_message.map(|m| m.id.clone())
                    } else {
                        None
                    },
                }
            })
            .collect();
        let public_seq = self.public_seq();
        let latest_request_id = self.latest_request_id();
        GroupSnapshot {
            group: self,
            requests,
            public_seq,
            latest_request_id,
        }
    }

    pub fn public_seq(&self) -> u64 {
        self.messages
            .iter()
            .map(|m| {
                m.public_revision
                    .max(if Self::public_message(m) { m.seq } else { 0 })
            })
            .max()
            .unwrap_or(0)
    }

    fn public_message(message: &Message) -> bool {
        matches!(message.kind.as_str(), "user" | "assistant")
            && (!message.text.trim().is_empty() || !message.attachments.is_empty())
    }

    pub fn refresh_public_revisions(&mut self, previous: &Group) {
        let old = previous
            .messages
            .iter()
            .map(|m| (m.id.as_str(), m))
            .collect::<std::collections::HashMap<_, _>>();
        let mut revision = self.public_seq().max(previous.public_seq());
        for message in &mut self.messages {
            let changed = old.get(message.id.as_str()).is_none_or(|before| {
                before.kind != message.kind
                    || before.text != message.text
                    || before.attachments != message.attachments
            });
            if changed
                && (Self::public_message(message)
                    || old
                        .get(message.id.as_str())
                        .is_some_and(|m| Self::public_message(m)))
            {
                revision += 1;
                message.public_revision = revision;
            }
        }
    }

    pub fn latest_request_id(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.kind == "user")
            .map(|m| {
                m.stop_root_id
                    .clone()
                    .or_else(|| m.request_id.clone())
                    .unwrap_or_else(|| m.id.clone())
            })
            .or_else(|| self.works.last().map(|w| w.root_id.clone()))
    }

    pub fn freeze_request_owners(&mut self) {
        let owners = self
            .messages
            .iter()
            .filter(|m| m.kind == "user" && m.request_id.is_none() && m.request_owner_id.is_none())
            .map(|m| (m.id.clone(), self.request_owner(&m.id)))
            .collect::<std::collections::HashMap<_, _>>();
        for message in &mut self.messages {
            if let Some(owner) = owners.get(&message.id) {
                message.request_owner_id = Some(owner.clone());
            }
        }
    }

    pub fn restore_saved_replies(&mut self) -> bool {
        let continued = self
            .works
            .iter()
            .filter_map(|work| work.continuation_of.as_deref())
            .collect::<std::collections::HashSet<_>>();
        let retained = self
            .works
            .iter()
            .filter(|work| !work.output.trim().is_empty() && !continued.contains(work.id.as_str()))
            .map(|work| work.id.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for work in retained {
            changed |= self.reconcile_reply(&work);
        }
        changed
    }

    fn reconcile_reply(&mut self, work_id: &str) -> bool {
        let Some(work) = self.works.iter().find(|w| w.id == work_id).cloned() else {
            return false;
        };
        if work.output.trim().is_empty() {
            return false;
        }
        if let Some(message) = self
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.work_id.as_deref() == Some(work_id) && m.kind == "assistant")
        {
            if message.text != work.output
                && (message.text.is_empty() || work.output.starts_with(&message.text))
            {
                message.text = work.output;
                return true;
            }
        } else {
            let message = self.post(
                Some(work.member_id),
                "assistant",
                work.output,
                Some(work.id.clone()),
            );
            message.id = format!("result-{}", work.id);
            message.created_at = work.created_at;
            return true;
        }
        false
    }

    pub fn summarize_request(&mut self, work_id: &str) -> Result<(), String> {
        self.summarize_request_in_context(work_id, 0, |_| true)
    }

    pub fn summarize_request_in_context(
        &mut self,
        work_id: &str,
        cutoff: u64,
        permits: impl Fn(&str) -> bool,
    ) -> Result<(), String> {
        let work = self
            .works
            .iter()
            .find(|w| w.id == work_id)
            .cloned()
            .ok_or("工作不存在")?;
        if self
            .works
            .iter()
            .any(|w| w.root_id == work.root_id && !w.terminal())
        {
            return Err("请等待或停止当前工作后，再整理已有结果".into());
        }
        let mut roots = std::collections::HashSet::new();
        let mut source_root = work.root_id.clone();
        while roots.insert(source_root.clone()) {
            let previous = self
                .messages
                .iter()
                .find(|m| m.id == source_root)
                .and_then(|m| m.source_request_id.clone());
            match previous {
                Some(root) if !roots.contains(&root) => source_root = root,
                _ => break,
            }
        }
        let inputs = self
            .messages
            .iter()
            .filter(|m| {
                m.kind == "user"
                    && m.seq > cutoff
                    && (roots.contains(&m.id)
                        || m.request_id
                            .as_ref()
                            .is_some_and(|root| roots.contains(root)))
                    && permits(&self.message_prompt(m))
            })
            .cloned()
            .collect::<Vec<_>>();
        let selected = self
            .works
            .iter()
            .filter(|w| {
                roots.contains(&w.root_id)
                    && permits(&w.output)
                    && (cutoff == 0
                        || self.messages.iter().any(|m| {
                            m.seq > cutoff
                                && (m.work_id.as_deref() == Some(&w.id) || m.id == w.root_id)
                        }))
            })
            .rev()
            .take(24)
            .collect::<Vec<_>>();
        let results = selected
            .iter()
            .map(|w| {
                format!(
                    "工作 [{}] 成员 [{}] 状态 {}：{}\n{}",
                    w.id,
                    w.member_id,
                    w.status,
                    w.output.chars().take(2400).collect::<String>(),
                    w.error
                        .as_deref()
                        .filter(|error| permits(error))
                        .unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let records = self
            .messages
            .iter()
            .rev()
            .filter(|m| selected.iter().any(|w| Some(&w.id) == m.work_id.as_ref()))
            .flat_map(|m| m.tool_calls.iter())
            .filter(|record| permits(&record.to_string()))
            .take(24)
            .collect::<Vec<_>>();
        let records = serde_json::to_string(&records)
            .unwrap_or_default()
            .chars()
            .take(24000)
            .collect::<String>();
        let source = inputs.iter().find(|m| m.id == source_root).cloned();
        let goal = source
            .as_ref()
            .map(|m| self.message_prompt(m))
            .unwrap_or_else(|| {
                "原始目标未在当前可召回范围内，请用户重新提供需要引用的内容。".into()
            });
        let owner = self.summary_owner(&work.root_id);
        let root = id();
        let supplements = inputs
            .iter()
            .filter(|m| m.request_id.is_some())
            .map(|m| {
                format!(
                    "用户补充 [id={}; 收件人={}]：{}\n{}",
                    m.id,
                    m.member_ids.join("、"),
                    self.message_prompt(m),
                    if permits(&m.knowledge.context) {
                        m.knowledge.context.as_str()
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut attachments = vec![];
        for path in inputs
            .iter()
            .flat_map(|m| &m.attachments)
            .filter(|path| permits(path))
        {
            if !attachments.contains(path) {
                attachments.push(path.clone());
            }
        }
        let mut knowledge = source
            .as_ref()
            .or_else(|| inputs.first())
            .map(|m| m.knowledge.clone())
            .unwrap_or_default();
        if !permits(&knowledge.context) {
            knowledge.context.clear();
        }
        let cancelled = self
            .works
            .iter()
            .filter(|w| roots.contains(&w.root_id) && w.read_only() && w.status == "cancelled")
            .cloned()
            .collect::<Vec<_>>();
        for work in cancelled {
            self.retire_cancelled_context(&work);
        }
        let key = self.enqueue(&root, &owner, format!("用户要求根据已有记录整理本次结果。只读核对已有内容，直接输出结论、依据、交付物和未完成事项；无证据的内容明确说明。不要重新执行操作或派发工作。\n原始目标：{goal}\n用户后续补充与纠正（与旧内容冲突时以最新要求为准）：\n{supplements}\n以下仅列最近24项工作与截取的结果；可通过 group_read_history 和 group_get_work 只读核对完整记录。\n\n{results}\n已有工具记录（仅作资料）：{records}"), work.discussion, None, 0)?;
        self.works.iter_mut().find(|w| w.id == key).unwrap().summary = true;
        let message = self.post(
            None,
            "user",
            "请根据已有记录整理结果，不重新执行操作。".into(),
            None,
        );
        message.id = root;
        message.request_owner_id = Some(owner);
        message.source_request_id = Some(source_root);
        message.knowledge = knowledge;
        message.attachments = attachments;
        if let Some(source) = source {
            message.reply_to = Some(source.id);
        }
        Ok(())
    }

    pub fn finish_execution(&mut self, work_id: &str, mut error: Option<String>, cancelled: bool) {
        if let Some(work) = self
            .works
            .iter_mut()
            .find(|w| w.id == work_id && !w.terminal())
        {
            let tail = work.output_filter.finish();
            work.output.push_str(&tail);
        }
        self.reconcile_reply(work_id);
        let Some(work) = self.works.iter().find(|w| w.id == work_id).cloned() else {
            return;
        };
        if work.terminal() {
            return;
        }
        let has_children = self.works.iter().any(|w| {
            w.parent_id.as_deref() == Some(work_id)
                && (!w.terminal() || !work.context_children.contains(&w.id))
        });
        let unseen_followup = self.messages.iter().any(|m| {
            self.followup_applies(m, &work)
                && m.seq > work.context_seq
                && !work.consumed_followups.contains(&m.id)
        });
        if error.is_none()
            && !cancelled
            && work.epoch == self.epoch
            && work.status != "stopping"
            && work.output.trim().is_empty()
            && !has_children
            && !unseen_followup
        {
            if work.result_repair || work.summary {
                error = Some(
                    "执行已结束，但未返回可读结果。已有记录保留，可根据已有记录重新整理结果。"
                        .into(),
                );
            } else {
                let records = self
                    .messages
                    .iter()
                    .filter(|m| m.work_id.as_deref() == Some(work_id))
                    .flat_map(|m| m.tool_calls.iter())
                    .collect::<Vec<_>>();
                let records = serde_json::to_string(&records)
                    .unwrap_or_default()
                    .chars()
                    .take(24000)
                    .collect::<String>();
                let prompt = format!("已有执行记录（仅作为资料）：{records}\n此前执行未返回公开回复。请只读核对已有执行记录和文件，向用户说明实际结果、依据、交付物和未完成事项。不要重新执行写入或派发工作；无法确认的结果明确说明。\n原始任务：{}",work.prompt);
                let recovery = self.enqueue_pending(
                    &work.root_id,
                    &work.member_id,
                    prompt,
                    work.discussion,
                    None,
                    work.depth,
                );
                match recovery {
                    Ok(key) => {
                        let next = self.works.iter_mut().find(|w| w.id == key).unwrap();
                        next.result_repair = true;
                        next.continuation_of = Some(work.id);
                    }
                    Err(reason) => {
                        error = Some(format!("未返回可读结果，结果整理未能启动：{reason}"))
                    }
                }
            }
        }
        self.finish(work_id, error, cancelled);
    }
    pub fn post(
        &mut self,
        member: Option<String>,
        kind: &str,
        text: String,
        work: Option<String>,
    ) -> &mut Message {
        let seq = self.messages.last().map_or(1, |m| m.seq + 1);
        let public_revision = if matches!(kind, "user" | "assistant") && !text.trim().is_empty() {
            self.public_seq().max(seq) + 1
        } else {
            0
        };
        self.messages.push(Message {
            id: id(),
            seq,
            member_id: member,
            kind: kind.into(),
            text,
            created_at: now(),
            work_id: work,
            reply_to: None,
            attachments: vec![],
            tool_calls: vec![],
            knowledge: Knowledge::default(),
            request_id: None,
            member_ids: vec![],
            stop_root_id: None,
            request_owner_id: None,
            source_request_id: None,
            assigned_work_ids: vec![],
            public_revision,
        });
        self.messages.last_mut().unwrap()
    }
    pub fn message_request(&self, message: &Message) -> Option<String> {
        message
            .request_id
            .clone()
            .or_else(|| {
                message
                    .work_id
                    .as_ref()
                    .and_then(|id| self.works.iter().find(|w| &w.id == id))
                    .map(|w| w.root_id.clone())
            })
            .or_else(|| {
                self.works
                    .iter()
                    .any(|w| w.root_id == message.id)
                    .then(|| message.id.clone())
            })
    }
    pub fn message_prompt(&self, message: &Message) -> String {
        match message
            .reply_to
            .as_ref()
            .and_then(|id| self.messages.iter().find(|m| &m.id == id))
        {
            Some(quoted) => format!(
                "回复引用 [id={}]：\n{}\n\n{}",
                quoted.id, quoted.text, message.text
            ),
            None => message.text.clone(),
        }
    }
    fn request_stopped(&self, root: &str) -> bool {
        self.messages
            .iter()
            .any(|m| m.stop_root_id.as_deref() == Some(root))
    }
    pub fn request_owner(&self, root: &str) -> String {
        if let Some(owner) = self
            .messages
            .iter()
            .find(|m| m.id == root)
            .and_then(|m| m.request_owner_id.as_ref())
        {
            return owner.clone();
        }
        let mut initial = self.works.iter().filter(|w| {
            w.root_id == root && w.parent_id.is_none() && w.continuation_of.is_none() && !w.summary
        });
        match (initial.next(), initial.next()) {
            (Some(owner), None) => owner.member_id.clone(),
            (None, None) => self
                .works
                .iter()
                .find(|w| w.root_id == root)
                .map(|w| w.member_id.clone())
                .unwrap_or_else(|| self.coordinator_id.clone()),
            _ => self
                .works
                .iter()
                .find(|w| w.root_id == root && w.summary)
                .map(|w| w.member_id.clone())
                .unwrap_or_else(|| self.coordinator_id.clone()),
        }
    }
    pub fn summary_owner(&self, root: &str) -> String {
        let owner = self.request_owner(root);
        if self.members.iter().any(|m| m.id == owner && !m.removed) {
            owner
        } else {
            self.coordinator_id.clone()
        }
    }
    pub fn followup_applies(&self, message: &Message, work: &Work) -> bool {
        if message.request_id.as_deref() != Some(&work.root_id) {
            return false;
        }
        let owner = self.request_owner(&work.root_id);
        message.member_ids.is_empty()
            || message.member_ids.contains(&work.member_id)
            || message.member_ids.contains(&owner)
            || work.member_id == owner
    }
    pub fn request_attachments(&self, root: &str) -> Vec<String> {
        let mut paths = vec![];
        for path in self
            .messages
            .iter()
            .filter(|m| m.id == root || m.request_id.as_deref() == Some(root))
            .flat_map(|m| &m.attachments)
        {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        paths
    }
    pub fn followup_context(&self, root: &str) -> Result<String, String> {
        let mut context = String::new();
        for m in self
            .messages
            .iter()
            .filter(|m| m.request_id.as_deref() == Some(root))
        {
            context.push_str(&format!(
                "用户补充 [id={}; 收件人={}]：{}\n{}\n\n",
                m.id,
                m.member_ids
                    .iter()
                    .filter_map(|id| self.members.iter().find(|member| &member.id == id))
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join("、"),
                self.message_prompt(m),
                m.knowledge.context
            ));
            if context.chars().count() > 50_000 {
                return Err("本次任务的补充超过上下文容量，请围绕最新目标继续；消息已保留".into());
            }
        }
        Ok(context)
    }
    pub fn retry_work(&mut self, work_id: &str) -> Result<Vec<(String, String)>, String> {
        let work = self
            .works
            .iter()
            .find(|w| w.id == work_id)
            .cloned()
            .ok_or("工作不存在")?;
        if !matches!(work.status.as_str(), "failed" | "uncertain" | "cancelled") {
            return Err("只能重试失败、停止或执行情况未确认的工作".into());
        }
        let sessions = if work.status == "uncertain" {
            self.stop_work(Some(work_id))?
        } else {
            vec![]
        };
        if self.result_only_retry(work_id) {
            self.paused = false;
            self.summarize_request(work_id)?;
            return Ok(sessions);
        }
        self.paused = false;
        let root = id();
        let source = self.messages.iter().find(|m| m.id == work.root_id).cloned();
        let supplements = self.followup_context(&work.root_id)?;
        let attachments = self.request_attachments(&work.root_id);
        self.enqueue(&root, &work.member_id, format!("此前工作未完成。请先检查已有文件和执行结果，避免重复产生副作用，再完成：\n{}\n用户最新补充与纠正：\n{supplements}",work.prompt),work.discussion,None,0)?;
        // A cancelled resident actor can still own an abandoned permission
        // interaction. Retry with a fresh actor, keeping the member's memory
        // namespace and public history. Never detach a newer or active context.
        self.retire_cancelled_context(&work);
        let message = self.post(None, "user", format!("检查后继续：{}", work.prompt), None);
        message.id = root;
        message.request_owner_id = Some(work.member_id);
        message.attachments = attachments;
        if let Some(source) = source {
            message.knowledge = source.knowledge;
        }
        Ok(sessions)
    }

    fn retire_cancelled_context(&mut self, work: &Work) {
        if work.status == "cancelled"
            && work.session_id.is_some()
            && !self
                .works
                .iter()
                .any(|other| other.member_id == work.member_id && other.active())
        {
            if let Some(member) = self.members.iter_mut().find(|m| m.id == work.member_id) {
                if work.read_only() && member.discussion_session_id == work.session_id {
                    member.discussion_session_id = None;
                    member.discussion_seen_seq = member.history_start_seq;
                } else if !work.read_only() && member.session_id == work.session_id {
                    member.session_id = None;
                    member.seen_seq = member.history_start_seq;
                }
            }
        }
    }
    pub fn result_only_retry(&self, work_id: &str) -> bool {
        let by_id = self
            .works
            .iter()
            .map(|work| (work.id.as_str(), work))
            .collect::<std::collections::HashMap<_, _>>();
        let mut pending = self
            .works
            .iter()
            .filter(|work| work.summary || work.result_repair)
            .collect::<Vec<_>>();
        let mut visited = std::collections::HashSet::new();
        while let Some(work) = pending.pop() {
            if !visited.insert(work.id.as_str()) {
                continue;
            }
            if work.id == work_id {
                return true;
            }
            if let Some(previous) = work.continuation_of.as_deref().and_then(|id| by_id.get(id)) {
                pending.push(previous);
            }
        }
        false
    }
    pub fn enqueue(
        &mut self,
        root: &str,
        member: &str,
        prompt: String,
        discussion: bool,
        parent: Option<String>,
        depth: u32,
    ) -> Result<String, String> {
        if self.paused {
            return Err("专家团已暂停或归档，请先恢复协作".into());
        }
        self.enqueue_pending(root, member, prompt, discussion, parent, depth)
    }

    /// Persist already-authorized work even while dispatch is paused.
    /// The scheduler and begin_prompt remain the only boundaries that start it.
    pub fn enqueue_pending(
        &mut self,
        root: &str,
        member: &str,
        prompt: String,
        discussion: bool,
        parent: Option<String>,
        depth: u32,
    ) -> Result<String, String> {
        if self.archived {
            return Err("专家团已归档，请先恢复协作".into());
        }
        if !self.members.iter().any(|m| m.id == member && !m.removed) {
            return Err("指定成员不在本专家团中".into());
        }
        let key = id();
        self.works.push(Work {
            id: key.clone(),
            root_id: root.into(),
            parent_id: parent,
            continuation_of: None,
            member_id: member.into(),
            prompt,
            status: "queued".into(),
            discussion,
            epoch: self.epoch,
            depth,
            created_at: now(),
            session_id: None,
            prompt_id: id(),
            output: String::new(),
            error: None,
            tokens: 0,
            summary: false,
            result_repair: false,
            seen_events: vec![],
            output_filter: Default::default(),
            context_seq: 0,
            consumed_followups: vec![],
            context_children: vec![],
        });
        Ok(key)
    }
    pub fn ready_callbacks(&mut self) {
        if self.paused || self.archived {
            return;
        }
        let parents = self
            .works
            .iter()
            .filter(|w| {
                w.status == "waiting_children"
                    && w.epoch == self.epoch
                    && !self.request_stopped(&w.root_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        for parent in parents {
            let children = self
                .works
                .iter()
                .filter(|w| w.parent_id.as_deref() == Some(&parent.id))
                .collect::<Vec<_>>();
            if children.is_empty()
                || children.iter().any(|w| !w.terminal())
                || self
                    .works
                    .iter()
                    .any(|w| w.continuation_of.as_deref() == Some(&parent.id))
            {
                continue;
            }
            let results = children
                .iter()
                .map(|w| {
                    format!(
                        "{} [{}]: {}{}",
                        self.members
                            .iter()
                            .find(|m| m.id == w.member_id)
                            .map(|m| m.name.as_str())
                            .unwrap_or("成员"),
                        w.status,
                        w.output,
                        w.error.as_deref().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let previous_error = parent.error.as_deref().unwrap_or("无");
            match self.enqueue(&parent.root_id, &parent.member_id, format!("你派发的工作已结束。请结合原始目标检查结果，向请求方汇总实际交付、验证情况及未完成事项。不要重复派发已完成工作。\n原始请求：{}\n此前回合异常：{previous_error}\n\n{results}", parent.prompt), parent.discussion, None, parent.depth) {
                Ok(next) => {
                    let next = self.works.iter_mut().find(|w| w.id == next).unwrap();
                    next.continuation_of = Some(parent.id);
                    next.summary = parent.summary;
                    next.result_repair = parent.result_repair;
                }
                Err(error) => { let w = self.works.iter_mut().find(|w| w.id == parent.id).unwrap(); w.status = "failed".into(); w.error = Some(error); self.propagate_completion(&parent.id); }
            }
        }
    }
    pub fn finish(&mut self, work_id: &str, error: Option<String>, cancelled: bool) {
        let Some(index) = self.works.iter().position(|w| w.id == work_id) else {
            return;
        };
        if self.works[index].terminal() {
            return;
        }
        let tail = self.works[index].output_filter.finish();
        self.works[index].output.push_str(&tail);
        if !tail.is_empty() {
            if let Some(message) = self
                .messages
                .iter_mut()
                .rev()
                .find(|m| m.work_id.as_deref() == Some(work_id) && m.kind == "assistant")
            {
                message.text.push_str(&tail);
            }
        }
        let old_epoch = self.works[index].epoch != self.epoch;
        let has_children = self
            .works
            .iter()
            .any(|w| w.parent_id.as_deref() == Some(work_id) && !w.terminal());
        let has_any_children = self.works.iter().any(|w| {
            w.parent_id.as_deref() == Some(work_id)
                && !self.works[index].context_children.contains(&w.id)
        });
        let has_continuation = self
            .works
            .iter()
            .any(|w| w.continuation_of.as_deref() == Some(work_id) && !w.terminal());
        let w = &mut self.works[index];
        w.error = error;
        w.status = if cancelled || old_epoch || w.status == "stopping" {
            "cancelled"
        } else if has_children || has_any_children || has_continuation {
            "waiting_children"
        } else if w.error.is_some() {
            "failed"
        } else {
            "completed"
        }
        .into();
        // A supplement can race the model's final token. Always give the
        // request owner a durable continuation if it has not seen that message.
        let w = self.works[index].clone();
        if w.status == "completed"
            && w.epoch == self.epoch
            && self.messages.iter().any(|m| {
                self.followup_applies(m, &w)
                    && m.seq > w.context_seq
                    && !w.consumed_followups.contains(&m.id)
            })
        {
            match self.enqueue_pending(&w.root_id, &w.member_id, format!("请按用户对本次任务的最新补充与纠正检查并完成交付，避免重复执行已完成操作。\n原始请求：{}\n已有结果：{}", w.prompt, w.output), w.discussion, None, w.depth) {
                Ok(next) => {
                    self.works[index].status = "waiting_children".into();
                    let next = self.works.iter_mut().find(|w| w.id == next).unwrap();
                    next.continuation_of = Some(w.id);
                    next.summary = w.summary;
                    next.result_repair = w.result_repair;
                }
                Err(error) => { self.works[index].status = "failed".into(); self.works[index].error = Some(error); }
            }
        }
        self.propagate_completion(work_id);
        self.ready_callbacks();
        self.ready_summaries();
    }

    /// A late session load must not replace a context changed after Stop.
    pub fn bind_session(&mut self, work_id: &str, session: &str) -> bool {
        let Some(work) = self.works.iter_mut().find(|w| w.id == work_id) else {
            return false;
        };
        if work.status != "dispatching" || work.epoch != self.epoch {
            return false;
        }
        let Some(member) = self
            .members
            .iter_mut()
            .find(|m| m.id == work.member_id && !m.removed)
        else {
            return false;
        };
        if work.read_only() {
            member.discussion_session_id = Some(session.into());
        } else {
            member.session_id = Some(session.into());
        }
        work.session_id = Some(session.into());
        true
    }

    /// Called only at the serialized native-dispatch boundary. Preparing a
    /// session/knowledge bridge never means a model turn has already started.
    pub fn begin_prompt(&mut self, work_id: &str) -> bool {
        let Some(work) = self.works.iter_mut().find(|w| w.id == work_id) else {
            return false;
        };
        if work.status != "dispatching" || work.epoch != self.epoch || self.archived {
            return false;
        }
        if self.paused {
            work.status = "queued".into();
            return false;
        }
        work.status = "running".into();
        true
    }

    pub fn stop_work(&mut self, target: Option<&str>) -> Result<Vec<(String, String)>, String> {
        if target.is_some_and(|id| !self.works.iter().any(|w| w.id == id)) {
            return Err("工作不存在".into());
        }
        if target.is_none() {
            self.epoch += 1;
            self.paused = true;
        }
        let mut ids = target
            .into_iter()
            .map(str::to_string)
            .collect::<std::collections::HashSet<_>>();
        loop {
            let before = ids.len();
            for work in &self.works {
                if work.parent_id.as_ref().is_some_and(|id| ids.contains(id))
                    || work
                        .continuation_of
                        .as_ref()
                        .is_some_and(|id| ids.contains(id))
                {
                    ids.insert(work.id.clone());
                }
            }
            if ids.len() == before {
                break;
            }
        }
        if target.is_none() {
            ids.extend(self.works.iter().map(|w| w.id.clone()));
        }
        Ok(self.settle_stop(ids))
    }
    pub fn stop_request(&mut self, root: &str) -> Vec<(String, String)> {
        let ids = self
            .works
            .iter()
            .filter(|w| w.root_id == root)
            .map(|w| w.id.clone())
            .collect();
        // No callbacks may be created between settling the root and recording
        // its stop receipt in the same database transaction.
        let paused = self.paused;
        self.paused = true;
        let sessions = self.settle_stop(ids);
        self.paused = paused;
        sessions
    }
    fn settle_stop(&mut self, ids: std::collections::HashSet<String>) -> Vec<(String, String)> {
        let mut sessions = vec![];
        let mut settled = vec![];
        for work in &mut self.works {
            if work.terminal() || !ids.contains(&work.id) {
                continue;
            }
            if matches!(work.status.as_str(), "running" | "stopping") && work.session_id.is_some() {
                work.status = "stopping".into();
                sessions.push((work.session_id.clone().unwrap(), work.prompt_id.clone()));
            } else {
                work.status = "cancelled".into();
                settled.push(work.id.clone());
            }
        }
        // Settle every selected node before considering callbacks. In
        // particular, cancelling a queued continuation must settle its parent.
        for id in settled {
            self.propagate_completion(&id);
        }
        self.ready_callbacks();
        self.ready_summaries();
        sessions
    }

    pub fn repair_dependencies(&mut self) {
        let terminal = self
            .works
            .iter()
            .filter(|w| w.terminal() && w.continuation_of.is_some())
            .map(|w| w.id.clone())
            .collect::<Vec<_>>();
        for id in terminal {
            self.propagate_completion(&id);
        }
        let failed_requesters = self
            .works
            .iter()
            .filter(|w| {
                w.status == "failed"
                    && w.epoch == self.epoch
                    && self
                        .works
                        .iter()
                        .any(|child| child.parent_id.as_deref() == Some(&w.id))
                    && !self
                        .works
                        .iter()
                        .any(|next| next.continuation_of.as_deref() == Some(&w.id))
            })
            .map(|w| w.id.clone())
            .collect::<Vec<_>>();
        for id in failed_requesters {
            self.works.iter_mut().find(|w| w.id == id).unwrap().status = "waiting_children".into();
        }
    }

    fn propagate_completion(&mut self, work_id: &str) {
        let Some(w) = self.works.iter().find(|w| w.id == work_id && w.terminal()) else {
            return;
        };
        let mut original = w.continuation_of.clone();
        let output = w.output.clone();
        let status = w.status.clone();
        let error = w.error.clone();
        // A resumed coordinator can request one more check. Completing that
        // nested continuation must settle the entire original request.
        let mut visited = std::collections::HashSet::new();
        while let Some(parent_id) = original {
            if !visited.insert(parent_id.clone()) {
                break;
            }
            let Some(parent) = self.works.iter_mut().find(|w| w.id == parent_id) else {
                break;
            };
            parent.status = status.clone();
            parent.output = output.clone();
            parent.error = error.clone();
            original = parent.continuation_of.clone();
        }
    }

    pub fn ready_summaries(&mut self) {
        if self.paused || self.archived {
            return;
        }
        let roots = self
            .works
            .iter()
            .filter(|w| w.parent_id.is_none() && w.continuation_of.is_none() && !w.summary)
            .map(|w| w.root_id.clone())
            .collect::<std::collections::HashSet<_>>();
        for root in roots {
            if self.request_stopped(&root) {
                continue;
            }
            let initial = self
                .works
                .iter()
                .filter(|w| {
                    w.root_id == root
                        && w.parent_id.is_none()
                        && w.continuation_of.is_none()
                        && !w.summary
                })
                .collect::<Vec<_>>();
            if initial.len() < 2
                || initial
                    .iter()
                    .any(|w| !w.terminal() || w.epoch != self.epoch)
                || self.works.iter().any(|w| w.root_id == root && w.summary)
            {
                continue;
            }
            let discussion = initial.iter().all(|w| w.discussion);
            let results = initial
                .iter()
                .map(|w| {
                    format!(
                        "{} [{}]：{} {}",
                        self.members
                            .iter()
                            .find(|m| m.id == w.member_id)
                            .map(|m| m.name.as_str())
                            .unwrap_or("成员"),
                        w.status,
                        w.output.chars().take(12000).collect::<String>(),
                        w.error.as_deref().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let prompt = format!("请归纳本轮各成员的观点与结果，回应用户原始目标，说明一致结论、分歧和未完成事项。不再派发新工作。\n\n{results}");
            let owner = self.request_owner(&root);
            match self.enqueue(&root, &owner, prompt.clone(), discussion, None, 0) {
                Ok(id) => self.works.iter_mut().find(|w| w.id == id).unwrap().summary = true,
                Err(error) => {
                    // Record a failed delivery without spending another model
                    // call. A user retry then summarizes the actual results.
                    let mut failed = self
                        .works
                        .iter()
                        .find(|w| w.root_id == root)
                        .unwrap()
                        .clone();
                    failed.id = id();
                    failed.member_id = owner;
                    failed.parent_id = None;
                    failed.continuation_of = None;
                    failed.prompt = prompt;
                    failed.prompt_id = id();
                    failed.status = "failed".into();
                    failed.summary = true;
                    failed.session_id = None;
                    failed.output.clear();
                    failed.tokens = 0;
                    failed.seen_events.clear();
                    failed.output_filter = Default::default();
                    failed.consumed_followups.clear();
                    failed.context_children.clear();
                    failed.created_at = now();
                    failed.error = Some(format!("成员结果已保留，但自动汇总未能启动：{error}"));
                    self.works.push(failed);
                }
            }
        }
    }
}
