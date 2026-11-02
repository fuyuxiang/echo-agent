use super::*;
pub(super) fn tools() -> Value {
    let tool = |name: &str, description: &str, properties: Value, required: Vec<&str>| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}});
    json!({"tools":[
        tool("group_call_member","Assign work to another member of this group. Returns immediately; finish your turn, the Runtime resumes you when results are back. Use exact member id or name from group context.",json!({"member":{"type":"string"},"prompt":{"type":"string"}}),vec!["member","prompt"]),
        tool("group_read_history","Read this group's messages, oldest first. Other groups and private chats are not accessible.",json!({"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":50}}),vec![]),
        tool("group_get_work","Read the status and result of work in this group. Do not poll pending work; results are returned to its requester automatically.",json!({"workId":{"type":"string"}}),vec!["workId"]),
        tool("group_record_decision","Propose a decision citing the user message id. It becomes confirmed only when the user confirms it in the group.",json!({"text":{"type":"string"},"messageId":{"type":"string"}}),vec!["text","messageId"]),
        tool("group_correct_decision","Correct or retract a confirmed group decision ONLY on the user's explicit instruction in this request. Cite the current user message id and an exact evidence quote. Never use for your own conclusions. All members will use the corrected decision on subsequent work.",json!({"decisionId":{"type":"string"},"action":{"type":"string","enum":["edit","void"]},"text":{"type":"string"},"messageId":{"type":"string"},"evidence":{"type":"string"}}),vec!["decisionId","action","messageId","evidence"])
    ]})
}
pub(super) fn call(session: &str, params: &Value) -> Result<Value, String> {
    let (gid, member) = session_owner(session).ok_or("该会话不是群成员")?;
    let result = db()?.update(&gid, |g| call_in_group(g, &member, session, params))?;
    changed(&gid);
    if let Some(app) = APP.get() {
        runtime::kick(app.clone(), gid);
    }
    Ok(
        json!({"content":[{"type":"text","text":serde_json::to_string(&result).map_err(|e|e.to_string())?}],"isError":false}),
    )
}

pub(super) fn call_in_group(
    g: &mut Group,
    member: &str,
    session: &str,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("工具名缺失")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let work = g
        .works
        .iter()
        .find(|w| {
            w.session_id.as_deref() == Some(session)
                && w.member_id == member
                && w.status == "running"
        })
        .cloned()
        .ok_or("没有正在执行的专家团内请求")?;
    if work.epoch != g.epoch || g.archived {
        return Err("专家团内协作已停止".into());
    }
    match name {
        "group_call_member" => {
            if g.paused {
                return Err("后续派发已暂停，请等待用户恢复协作".into());
            }
            if work.summary || work.result_repair {
                return Err("当前正在整理已有结果，请直接回复，不再派发工作".into());
            }
            let target = args
                .get("member")
                .and_then(Value::as_str)
                .ok_or("请指定成员")?;
            let by_id = g.members.iter().find(|m| !m.removed && m.id == target);
            let candidates = g
                .members
                .iter()
                .filter(|m| !m.removed && m.name == target)
                .collect::<Vec<_>>();
            let target = match (by_id, candidates.as_slice()) {
                (Some(member), _) => member.id.clone(),
                (None, [member]) => member.id.clone(),
                (None, []) => return Err("成员不在本专家团中".into()),
                _ => return Err("成员名称重复，请使用群上下文中的成员 ID".into()),
            };
            if target == member {
                return Err("不能向自己派发工作".into());
            }
            let mut cursor = Some(work.id.clone());
            let mut visited = std::collections::HashSet::new();
            while let Some(id) = cursor {
                if !visited.insert(id.clone()) {
                    return Err("工作依赖记录存在循环，请停止后检查".into());
                }
                let Some(parent) = g.works.iter().find(|w| w.id == id) else {
                    break;
                };
                if parent.member_id == target && !parent.terminal() {
                    return Err("该成员正在等待此项工作，不能形成循环依赖".into());
                }
                cursor = parent
                    .parent_id
                    .clone()
                    .or_else(|| parent.continuation_of.clone());
            }
            let prompt = text(
                &echo_agent_memory::text_utils::strip_reasoning_blocks(
                    args.get("prompt").and_then(Value::as_str).unwrap_or(""),
                ),
                20000,
                "工作说明",
            )?;
            if let Some(existing) = g.works.iter().find(|w| {
                w.parent_id.as_deref() == Some(&work.id)
                    && w.member_id == target
                    && (w.prompt == prompt
                        || (!w.terminal()
                            && g.messages.iter().any(|m| {
                                m.kind == "user"
                                    && (m.work_id.as_deref() == Some(&w.id)
                                        || m.assigned_work_ids.contains(&w.id))
                            })))
            }) {
                return Ok(json!({"workId":existing.id,"status":existing.status}));
            }
            let id = g.enqueue(
                &work.root_id,
                &target,
                prompt.clone(),
                work.discussion,
                Some(work.id.clone()),
                work.depth + 1,
            )?;
            g.post(
                Some(member.to_string()),
                "handoff",
                prompt,
                Some(id.clone()),
            );
            Ok(
                json!({"workId":id,"status":"queued","note":"End this turn. You will receive the results automatically."}),
            )
        }
        "group_read_history" => {
            let after = args.get("after").and_then(Value::as_u64).unwrap_or(0);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(30)
                .clamp(1, 50) as usize;
            let member = g
                .members
                .iter()
                .find(|m| m.id == member)
                .ok_or("成员不存在")?;
            Ok(json!(runtime::history_window(g, member, after, limit, false, false)?.into_iter().map(|m|json!({"id":m.id,"seq":m.seq,"memberId":m.member_id,"workId":m.work_id,"kind":m.kind,"requestId":m.request_id,"text":m.text.chars().take(10000).collect::<String>()})).collect::<Vec<_>>()))
        }
        "group_get_work" => {
            let id = args.get("workId").and_then(Value::as_str).unwrap_or("");
            let w = g
                .works
                .iter()
                .find(|w| w.id == id)
                .ok_or("工作不属于本专家团或不存在")?;
            let member = g
                .members
                .iter()
                .find(|m| m.id == member)
                .ok_or("成员不存在")?;
            let available = (member.history_start_seq == 0
                || g.messages.iter().any(|m| {
                    m.seq > member.history_start_seq
                        && (m.work_id.as_deref() == Some(id) || m.id == w.root_id)
                }))
                && runtime::memory_permits(g, member, &w.output)?;
            Ok(
                json!({"id":w.id,"status":w.status,"output":if available {w.output.as_str()} else {"该结果未在当前成员可召回的历史中"},"error":w.error}),
            )
        }
        "group_record_decision" => {
            let source = args.get("messageId").and_then(Value::as_str).unwrap_or("");
            if !g
                .messages
                .iter()
                .any(|m| m.id == source && m.kind == "user")
            {
                return Err("决定必须引用用户在本专家团的消息".into());
            }
            let text = text(
                &echo_agent_memory::text_utils::strip_reasoning_blocks(
                    args.get("text").and_then(Value::as_str).unwrap_or(""),
                ),
                2000,
                "决定",
            )?;
            let d = Decision {
                id: id(),
                text,
                message_id: Some(source.into()),
                created_at: now(),
                confirmed: false,
                voided: false,
                history: vec![],
                updated_at: None,
            };
            let result = json!(d);
            g.decisions.push(d);
            Ok(result)
        }
        "group_correct_decision" => {
            if work.discussion {
                return Err("只读讨论不能修改决定".into());
            }
            let source = args.get("messageId").and_then(Value::as_str).unwrap_or("");
            let evidence = args
                .get("evidence")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let source = g
                .messages
                .iter()
                .find(|m| {
                    m.id == source
                        && m.kind == "user"
                        && (m.id == work.root_id || m.request_id.as_deref() == Some(&work.root_id))
                })
                .ok_or("更正必须引用当前请求中的用户指令")?;
            if evidence.is_empty() || !source.text.contains(evidence) {
                return Err("请引用用户明确更正或撤回决定的原文依据".into());
            }
            let source_id = source.id.clone();
            let decision = args.get("decisionId").and_then(Value::as_str).unwrap_or("");
            if !g
                .decisions
                .iter()
                .any(|d| d.id == decision && d.confirmed && !d.voided)
            {
                return Err("只能更正或作废已确认的有效决定".into());
            }
            let action = args.get("action").and_then(Value::as_str).unwrap_or("");
            if !matches!(action, "edit" | "void") {
                return Err("只支持更正或作废决定".into());
            }
            let value = args
                .get("text")
                .and_then(Value::as_str)
                .map(echo_agent_memory::text_utils::strip_reasoning_blocks);
            management::change_decision(g, decision, action, value.as_deref())?;
            let updated = g.decisions.iter_mut().find(|d| d.id == decision).unwrap();
            updated.message_id = Some(source_id);
            Ok(json!(updated))
        }
        _ => Err("该工具不属于群协作工具".into()),
    }
}
