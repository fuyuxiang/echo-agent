use super::*;
use serde::{Deserialize, Serialize};

#[tauri::command]
pub async fn group_member_change(
    app: tauri::AppHandle,
    state: State<'_, crate::commands::AppState>,
    group_id: String,
    action: String,
    member_id: Option<String>,
    expert: Option<String>,
) -> Result<(), String> {
    let _dispatch = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        runtime::dispatch_lock(&group_id).lock_owned(),
    )
    .await
    .map_err(|_| "成员仍在准备执行，请稍后调整成员".to_string())?;
    let snapshot = db()?.get(&group_id)?;
    if snapshot.works.iter().any(|w| !w.terminal()) {
        return Err("请完成或停止当前工作后调整成员".into());
    }
    if action == "clear_memory" {
        let member = snapshot
            .members
            .iter()
            .find(|m| Some(&m.id) == member_id.as_ref() && !m.removed)
            .ok_or("成员不存在")?;
        let tx = state.tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            // Include retired contexts from earlier model/profile changes.
            for sid in db()?.sessions(&group_id)? {
                if !db()?
                    .owner(&sid)?
                    .as_ref()
                    .is_some_and(|(_, owner)| owner == &member.id)
                {
                    continue;
                }
                crate::ext::call_ext::<Value>(
                    &tx,
                    "echo.agent/session/close",
                    crate::ext::raw_params(&json!({"sessionId":sid})),
                )
                .await
                .map_err(|e| format!("关闭成员旧上下文失败，尚未清除记忆：{e}"))?;
            }
        }
        let result = reset_member_memory(
            db()?,
            &crate::paths::echo_agent_home_dir(),
            &group_id,
            &member.id,
        );
        changed(&group_id);
        return result;
    }
    let definition = if matches!(action.as_str(), "add" | "refresh") {
        let expert = expert
            .or_else(|| {
                snapshot
                    .members
                    .iter()
                    .find(|m| Some(&m.id) == member_id.as_ref())
                    .and_then(|m| m.expert.clone())
            })
            .ok_or("请选择本地专家")?;
        let entry = crate::agents_store::team_agents()?
            .into_iter()
            .find(|m| m.name == expert)
            .ok_or("本地专家已不存在，请先恢复专家配置")?;
        let mut parsed =
            echo_agent_core::AgentDefinition::parse(&entry.raw).map_err(|e| e.to_string())?;
        let model = if action == "refresh" {
            snapshot
                .members
                .iter()
                .find(|m| Some(&m.id) == member_id.as_ref())
                .ok_or("成员不存在")?
                .model_id
                .clone()
        } else {
            match &parsed.model {
                echo_agent_core::config::ModelOverride::Override(m) => m.clone(),
                _ => snapshot
                    .members
                    .iter()
                    .find(|m| Some(&m.id) == member_id.as_ref())
                    .map(|m| m.model_id.clone())
                    .unwrap_or_else(|| snapshot.members[0].model_id.clone()),
            }
        };
        crate::commands::ensure_runtime_ready(&app, &state, Some(&model)).await?;
        crate::policy::require_model(&model)?;
        parsed.permission_mode = Default::default();
        parsed.memory = None;
        parsed.model = Default::default();
        let mut profile = serde_json::to_value(&parsed).map_err(|e| e.to_string())?;
        profile["promptBody"] = json!(parsed.prompt_body.unwrap_or_default());
        Some((entry, profile, model))
    } else {
        None
    };
    db()?.update(&group_id, |g| {
        if g.works.iter().any(|w| !w.terminal()) {
            return Err("成员正在工作，请稍后调整".into());
        }
        match action.as_str() {
            "add" => {
                let (entry, profile, model) = definition.ok_or("专家配置缺失")?;
                if g.members
                    .iter()
                    .any(|m| !m.removed && m.expert.as_deref() == Some(&entry.name))
                {
                    return Err("专家已在本群中".into());
                }
                if g.members
                    .iter()
                    .filter(|m| !m.removed && m.expert.is_some())
                    .count()
                    >= 8
                {
                    return Err("每个群最多包含 8 名专家".into());
                }
                g.members.push(Member {
                    id: id(),
                    name: entry.name.clone(),
                    expert: Some(entry.name),
                    role: entry.description.unwrap_or_default(),
                    profile,
                    model_id: model,
                    session_id: None,
                    discussion_session_id: None,
                    seen_seq: 0,
                    discussion_seen_seq: 0,
                    generation: 0,
                    history_start_seq: 0,
                    removed: false,
                });
            }
            "remove" => {
                let member = g
                    .members
                    .iter_mut()
                    .find(|m| Some(&m.id) == member_id.as_ref() && !m.removed)
                    .ok_or("成员不存在")?;
                if member.id == g.coordinator_id {
                    return Err("请先在群设置中选择其他协调员，再移除此成员".into());
                }
                member.removed = true;
            }
            "refresh" => {
                let (_, profile, model) = definition.ok_or("专家配置缺失")?;
                let member = g
                    .members
                    .iter_mut()
                    .find(|m| Some(&m.id) == member_id.as_ref() && !m.removed)
                    .ok_or("成员不存在")?;
                member.profile = profile;
                member.model_id = model;
                member.reset_context(false);
                // A definition refresh retires the context, not its memory.
            }
            _ => return Err("不支持的成员操作".into()),
        }
        Ok(())
    })?;
    changed(&group_id);
    Ok(())
}

pub(super) fn reset_member_memory(
    store: &Store,
    home: &std::path::Path,
    group: &str,
    member: &str,
) -> Result<(), String> {
    member_memory_target(home, group, member)?;
    // Advance the recall fence durably before deleting files. A database error
    // leaves the old namespace intact; a cleanup error can never restore it.
    store.update(group, |g| {
        if g.works.iter().any(|w| !w.terminal()) {
            return Err("成员正在工作，请稍后清除记忆".into());
        }
        let cutoff = g.messages.last().map_or(0, |m| m.seq);
        let member = g
            .members
            .iter_mut()
            .find(|m| m.id == member && !m.removed)
            .ok_or("成员不存在")?;
        member.history_start_seq = cutoff;
        member.reset_context(true);
        Ok(())
    })?;
    remove_member_memory(home, group, member)
        .map_err(|error| format!("已重建上下文并隔离旧记忆，但旧文件清理失败，请重试：{error}"))?;
    Ok(())
}
pub(super) fn remove_member_memory(
    home: &std::path::Path,
    group: &str,
    member: &str,
) -> Result<(), String> {
    if let Some(target) = member_memory_target(home, group, member)? {
        std::fs::remove_dir_all(target).map_err(|e| format!("成员记忆清理失败，可重试：{e}"))?;
    }
    Ok(())
}
fn member_memory_target(
    home: &std::path::Path,
    group: &str,
    member: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    let valid = |s: &str| {
        !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    };
    if !valid(group) || !valid(member) {
        return Err("成员记忆标识无效".into());
    }
    let root = home.join("group-memory");
    let group = root.join(group);
    let target = group.join(member);
    for path in [&root, &group, &target] {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            _ => return Err("成员记忆目录无效，尚未清除，请检查后重试".into()),
        }
    }
    Ok(Some(target))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub members: Vec<TemplateMember>,
    pub source: String,
    #[serde(default)]
    pub coordinator_name: Option<String>,
    #[serde(default)]
    pub coordinator_role: Option<String>,
    #[serde(default)]
    pub default_model_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateMember {
    pub name: String,
    pub role: String,
    pub model_id: Option<String>,
}

#[tauri::command]
pub fn group_team_templates() -> Result<Vec<TeamTemplate>, String> {
    // Session assignments are not saved templates. They can be reused from
    // their owning conversation without polluting the template library.
    db()?.templates()
}
#[tauri::command]
pub fn group_team_save(group_id: String, name: String) -> Result<TeamTemplate, String> {
    let g = db()?.get(&group_id)?;
    let coordinator = g
        .members
        .iter()
        .find(|m| m.id == g.coordinator_id)
        .ok_or("协调员配置缺失")?
        .clone();
    let mut template = TeamTemplate {
        id: id(),
        name: text(&name, 80, "专家团名称")?,
        description: g.goal,
        members: g
            .members
            .into_iter()
            .filter(|m| !m.removed && m.expert.is_some())
            .map(|m| TemplateMember {
                name: m.expert.unwrap(),
                role: m.role,
                model_id: Some(m.model_id),
            })
            .collect(),
        source: "常用团队".into(),
        coordinator_name: coordinator.expert,
        coordinator_role: Some(coordinator.role),
        default_model_id: Some(coordinator.model_id),
    };
    if let Some(previous) = db()?
        .templates()?
        .into_iter()
        .find(|t| t.name == template.name)
    {
        template.id = previous.id;
    }
    db()?.save_template(&template)?;
    Ok(template)
}
#[tauri::command]
pub fn group_team_delete(template_id: String) -> Result<(), String> {
    db()?.delete_template(&template_id)
}

#[tauri::command]
pub async fn group_decision(
    group_id: String,
    text: String,
    message_id: String,
    decision_id: Option<String>,
) -> Result<(), String> {
    let _dispatch = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        runtime::dispatch_lock(&group_id).lock_owned(),
    )
    .await
    .map_err(|_| "成员仍在准备执行，请稍后更新决定".to_string())?;
    db()?.update(&group_id, |g| {
        if let Some(id) = decision_id {
            let decision = g
                .decisions
                .iter_mut()
                .find(|d| d.id == id)
                .ok_or("决定记录不存在")?;
            if decision.voided {
                return Err("该决定已作废，请先恢复决定".into());
            }
            decision.confirmed = true;
            return Ok(());
        }
        if !g.messages.iter().any(|m| m.id == message_id) {
            return Err("来源消息不存在".into());
        }
        g.decisions.push(Decision {
            id: id(),
            text: super::text(&text, 2000, "决定")?,
            message_id: Some(message_id),
            created_at: now(),
            confirmed: true,
            voided: false,
            history: vec![],
            updated_at: None,
        });
        Ok(())
    })?;
    changed(&group_id);
    Ok(())
}

pub(super) fn change_decision(
    g: &mut Group,
    decision_id: &str,
    action: &str,
    value: Option<&str>,
) -> Result<(), String> {
    let decision = g
        .decisions
        .iter_mut()
        .find(|d| d.id == decision_id)
        .ok_or("决定记录不存在")?;
    match action {
        "edit" => {
            if decision.voided {
                return Err("请先恢复已作废的决定".into());
            }
            let value = text(value.unwrap_or(""), 2000, "决定内容")?;
            if decision.text != value {
                decision.history.push(DecisionVersion {
                    action: "edit".into(),
                    text: decision.text.clone(),
                    changed_at: now(),
                });
                decision.text = value;
            }
            decision.confirmed = true;
        }
        "void" => {
            if !decision.voided {
                decision.history.push(DecisionVersion {
                    action: "void".into(),
                    text: decision.text.clone(),
                    changed_at: now(),
                });
            }
            decision.voided = true;
        }
        "restore" => {
            if decision.voided {
                decision.history.push(DecisionVersion {
                    action: "restore".into(),
                    text: decision.text.clone(),
                    changed_at: now(),
                });
            }
            decision.voided = false;
            decision.confirmed = true;
        }
        _ => return Err("不支持的决定操作".into()),
    }
    decision.updated_at = Some(now());
    Ok(())
}
#[tauri::command]
pub async fn group_decision_change(
    group_id: String,
    decision_id: String,
    action: String,
    text: Option<String>,
) -> Result<(), String> {
    let _dispatch = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        runtime::dispatch_lock(&group_id).lock_owned(),
    )
    .await
    .map_err(|_| "成员仍在准备执行，请稍后更新决定".to_string())?;
    db()?.update(&group_id, |g| {
        change_decision(g, &decision_id, &action, text.as_deref())
    })?;
    changed(&group_id);
    Ok(())
}
