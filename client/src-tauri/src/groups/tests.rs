use super::*;

#[test]
fn retired_models_migrate_in_groups_previews_and_templates_without_losing_context() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("groups.sqlite");
    let store = Store::open(&path).unwrap();
    let mut g = group();
    g.members[0].model_id = "echoagent-ojlab/chat-qwen".into();
    g.members[0].session_id = Some("historical-session".into());
    g.members[1].model_id = "mine/chat-qwen".into();
    store.create(&g).unwrap();
    let template = management::TeamTemplate {
        id: "template".into(),
        name: "旧团队".into(),
        description: "chat-qwen 作为普通文字保留".into(),
        source: "常用团队".into(),
        coordinator_name: None,
        coordinator_role: None,
        default_model_id: Some("echoagent-ojlab/chat-glm".into()),
        members: vec![management::TemplateMember {
            name: "专家".into(),
            role: "核对资料".into(),
            model_id: Some("echoagent-ojlab/chat-xc".into()),
        }],
    };
    store.save_template(&template).unwrap();
    let migrated = Store::open(&path).unwrap();
    g.members[0].model_id = crate::providers::BUILTIN_DEFAULT_MODEL_ID.into();
    assert_eq!(migrated.get(&g.id).unwrap(), g);
    assert_eq!(
        migrated.list().unwrap()[0].members[0].model_id,
        crate::providers::BUILTIN_DEFAULT_MODEL_ID
    );
    let templates = migrated.templates().unwrap();
    assert_eq!(
        templates[0].default_model_id.as_deref(),
        Some(crate::providers::BUILTIN_DEFAULT_MODEL_ID)
    );
    assert_eq!(
        templates[0].members[0].model_id.as_deref(),
        Some(crate::providers::BUILTIN_DEFAULT_MODEL_ID)
    );
    assert_eq!(templates[0].description, template.description);
    assert_eq!(Store::open(&path).unwrap().get(&g.id).unwrap(), g);
}
pub(super) fn group() -> Group {
    let members = ["协调员", "分析师", "代码专家"]
        .map(|name| Member {
            id: id(),
            name: name.into(),
            expert: Some(name.into()),
            role: name.into(),
            profile: json!({"name":name,"description":name}),
            model_id: "test-model".into(),
            session_id: None,
            discussion_session_id: None,
            seen_seq: 0,
            discussion_seen_seq: 0,
            generation: 0,
            history_start_seq: 0,
            removed: false,
        })
        .to_vec();
    Group {
        id: id(),
        name: "交付组".into(),
        goal: "实现审批".into(),
        cwd: "/isolated".into(),
        source_session_id: None,
        source_team_id: None,
        coordinator_id: members[0].id.clone(),
        permission_mode: "ask".into(),
        members,
        messages: vec![],
        works: vec![],
        decisions: vec![],
        archived: false,
        pinned: false,
        paused: false,
        epoch: 0,
        revision: 1,
        created_at: now(),
        updated_at: now(),
        max_calls: 12,
        token_budget: 100_000,
    }
}
fn input() -> SendInput {
    SendInput {
        id: id(),
        text: "请分析需求".into(),
        member_ids: vec![],
        discussion: false,
        attachments: vec![],
        reply_to: None,
        knowledge: Knowledge::default(),
    }
}

#[test]
fn message_retry_is_idempotent_and_routes_to_coordinator() {
    let mut g = group();
    let input = input();
    send(&mut g, &input).unwrap();
    send(&mut g, &input).unwrap();
    assert_eq!(g.messages.len(), 1);
    assert_eq!(g.works.len(), 1);
    assert_eq!(g.works[0].member_id, g.coordinator_id);
}

#[test]
fn streaming_reasoning_never_enters_history_results_or_member_context() {
    let mut g = group();
    let member = g.members[0].id.clone();
    let mut i = input();
    i.text = "给出结论".into();
    send(&mut g, &i).unwrap();
    g.works[0].status = "running".into();
    g.works[0].session_id = Some("native".into());
    let work = g.works[0].id.clone();
    for (n, chunk) in [
        "<an",
        "alysis>私密推理",
        "</anal",
        "ysis>有效结论\n```xml\n<think>代码示例</think>\n```",
    ]
    .iter()
    .enumerate()
    {
        runtime::apply_update(&mut g, "native", &member, &json!({"sessionUpdate":"agent_message_chunk","content":{"text":chunk},"_groupEventId":format!("{n}")})).unwrap();
        // Simulate a restart of the host's incremental decoder between chunks.
        g = serde_json::from_value(serde_json::to_value(&g).unwrap()).unwrap();
        assert!(!g.works[0].output.contains("私密"));
    }
    g.finish(&work, None, false);
    assert_eq!(g.messages.len(), 2);
    assert!(g.messages[1].text.contains("<think>代码示例</think>"));
    assert!(!g.messages[1].text.contains("analysis"));
    let other = &g.members[1];
    let prompt = runtime::work_prompt(&g, &g.works[0], other, &Knowledge::default()).unwrap();
    assert!(prompt.contains("有效结论"));
    assert!(!prompt.contains("私密"));
    let before = g.messages[1].text.clone();
    g.stop_work(None).unwrap();
    runtime::apply_update(
        &mut g,
        "native",
        &member,
        &json!({"sessionUpdate":"agent_message_chunk","content":{"text":"迟到输出"}}),
    )
    .unwrap();
    assert_eq!(g.messages[1].text, before);
}

#[test]
fn public_text_preserves_code_and_handles_every_stream_boundary() {
    use echo_agent_memory::text_utils::{strip_reasoning_blocks, PublicText};
    for (raw, expected) in [
        (
            "<THINK mode=\"deep\">秘密<analysis>嵌套</analysis></THINK>公开",
            "公开",
        ),
        ("前文<analysis>秘密</analysis>结论", "前文结论"),
        ("<think>秘密</think>```text\n```not-a-closing-fence\n<analysis>示例</analysis>\n```\n公开<analysis>秘密</analysis>","```text\n```not-a-closing-fence\n<analysis>示例</analysis>\n```\n公开"),
        ("<thinking>未结束", ""),
        ("<an", ""),
        ("`<think>示例</think>`", "`<think>示例</think>`"),
        ("```text\n```not-a-closing-fence\n<analysis>示例</analysis>\n```\n公开<analysis>秘密</analysis>","```text\n```not-a-closing-fence\n<analysis>示例</analysis>\n```\n公开"),
        ("```<analysis>示例</analysis>```公开<analysis>秘密</analysis>","```<analysis>示例</analysis>```公开"),
        ("    <analysis>缩进示例</analysis>\n公开<analysis>秘密</analysis>","    <analysis>缩进示例</analysis>\n公开"),
        ("```xml\n    ```\n<analysis>示例</analysis>\n``` \t\n公开<analysis>秘密</analysis>","```xml\n    ```\n<analysis>示例</analysis>\n``` \t\n公开"),

        (
            "~~~xml\n<analysis>示例</analysis>\n~~~\n正常",
            "~~~xml\n<analysis>示例</analysis>\n~~~\n正常",
        ),
        (
            "比较 a < b，保留 <analysis-result> 和 HTML",
            "比较 a < b，保留 <analysis-result> 和 HTML",
        ),
    ] {
        assert_eq!(strip_reasoning_blocks(raw), expected);
        for split in raw.char_indices().map(|(i, _)| i).chain([raw.len()]) {
            let mut filter = PublicText::default();
            let mut text = filter.push(&raw[..split]);
            let saved = serde_json::to_string(&filter).unwrap();
            assert!(!saved.contains("秘密"));
            filter = serde_json::from_str(&saved).unwrap();
            text.push_str(&filter.push(&raw[split..]));
            text.push_str(&filter.finish());
            assert_eq!(text, expected, "split at {split}: {raw}");
        }
    }
}

#[test]
fn member_memory_corrections_and_reset_fence_every_shared_recall_path() {
    use echo_agent_memory::{records, MemoryStorage};
    let mut g = group();
    let member = g.members[1].id.clone();
    let root = crate::paths::echo_agent_home_dir()
        .join("group-memory")
        .join(&g.id)
        .join(&member)
        .join("generation-0");
    let storage = MemoryStorage::new_flat(std::path::Path::new(&g.cwd), &root);
    let saved = records::remember(
        &storage,
        records::RecordInput {
            id: None,
            scope: "workspace".into(),
            kind: "feedback".into(),
            key: "数据库选择".into(),
            content: "数据库使用 MySQL".into(),
            evidence: "用户明确确认".into(),
            pinned: true,
            valid_until: None,
            expected_revision: None,
            observed_at: None,
        },
        None,
        false,
    )
    .unwrap()
    .expect("explicit scoped memory must be saved");
    g.post(None, "user", "数据库使用 MySQL".into(), None);
    let updated = records::remember(
        &storage,
        records::RecordInput {
            id: Some(saved.id.clone()),
            scope: saved.scope,
            kind: saved.kind,
            key: saved.key,
            content: "数据库使用 PostgreSQL".into(),
            evidence: "用户明确纠正".into(),
            pinned: true,
            valid_until: None,
            expected_revision: Some(saved.revision),
            observed_at: None,
        },
        None,
        false,
    )
    .unwrap()
    .expect("explicit scoped memory must be saved");
    assert!(!records::core_context(&storage).unwrap().contains("MySQL"));
    assert!(records::core_context(&storage)
        .unwrap()
        .contains("PostgreSQL"));
    assert!(runtime::visible_history(&g, &g.members[1])
        .unwrap()
        .is_empty());
    assert_eq!(
        runtime::visible_history(&g, &g.members[2]).unwrap().len(),
        1
    );
    g.members[1].model_id = "another-model".into();
    g.members[1].reset_context(false);
    assert_eq!(g.members[1].generation, 0);
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(&home.path().join("groups.sqlite")).unwrap();
    store.create(&g).unwrap();
    let restored = store.get(&g.id).unwrap();
    assert_eq!(restored.members[1].model_id, "another-model");
    assert!(!runtime::memory_permits(&restored, &restored.members[1], "数据库使用 MySQL").unwrap());
    records::retire(
        &storage,
        "workspace",
        &updated.id,
        Some(updated.revision),
        false,
    )
    .unwrap();
    assert!(!runtime::memory_permits(&g, &g.members[1], "数据库使用 PostgreSQL").unwrap());
    g.members[1].history_start_seq = g.messages.last().unwrap().seq;
    g.members[1].reset_context(true);
    super::management::remove_member_memory(&crate::paths::echo_agent_home_dir(), &g.id, &member)
        .unwrap();
    assert!(!root.exists());
    assert!(runtime::visible_history(&g, &g.members[1])
        .unwrap()
        .is_empty());
    assert_eq!(
        runtime::visible_history(&g, &g.members[2]).unwrap().len(),
        1
    );
    g.post(None, "user", "新的工作".into(), None);
    assert_eq!(
        runtime::visible_history(&g, &g.members[1]).unwrap().len(),
        1
    );
}

#[test]
fn followups_join_current_request_and_survive_the_final_token_race() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    g.works[0].status = "running".into();
    g.works[0].context_seq = g.messages[0].seq;
    let work = g.works[0].id.clone();
    let mut correction = input();
    correction.text = "更正：使用 PostgreSQL，不用 MySQL".into();
    send(&mut g, &correction).unwrap();
    send(&mut g, &correction).unwrap();
    assert_eq!(g.works.len(), 1);
    assert_eq!(g.messages[1].request_id.as_deref(), Some(i.id.as_str()));
    g.finish(&work, None, false);
    let next = g.works[1].clone();
    assert_eq!(next.continuation_of.as_deref(), Some(work.as_str()));
    let prompt = runtime::work_prompt(&g, &next, &g.members[0], &Knowledge::default()).unwrap();
    assert!(prompt.contains("更正：使用 PostgreSQL"));
    g.works[1].context_seq = g.messages.last().unwrap().seq;
    g.finish(&next.id, None, false);
    assert_eq!(g.works.len(), 2);
    assert!(g.works.iter().all(Work::terminal));
    let mut stop = input();
    stop.text = "停止全部！".into();
    send(&mut g, &stop).unwrap();
    assert!(g.paused);
    assert_eq!(g.works.len(), 2);
}

#[test]
fn sequential_handoffs_use_actual_prior_output_and_do_not_skip_failure() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    let coordinator = g.members[0].id.clone();
    let analyst = g.members[1].id.clone();
    let coder = g.members[2].id.clone();
    g.works[0].status = "running".into();
    g.works[0].session_id = Some("coordinator".into());
    let root = g.works[0].id.clone();
    let a = mcp::call_in_group(&mut g, &coordinator, "coordinator", &json!({"name":"group_call_member","arguments":{"member":analyst,"prompt":"先形成接口方案"}})).unwrap()["workId"].as_str().unwrap().to_string();
    g.finish(&root, None, false);
    assert_eq!(g.works.len(), 2);
    g.works.iter_mut().find(|w| w.id == a).unwrap().output =
        "实际产物：POST /approve，必须包含幂等键".into();
    g.finish(&a, None, false);
    let continuation = g.works.last().unwrap().clone();
    assert!(continuation.prompt.contains("必须包含幂等键"));
    let next = g
        .works
        .iter_mut()
        .find(|w| w.id == continuation.id)
        .unwrap();
    next.status = "running".into();
    next.session_id = Some("coordinator".into());
    let b = mcp::call_in_group(&mut g, &coordinator, "coordinator", &json!({"name":"group_call_member","arguments":{"member":coder,"prompt":"按 POST /approve 与幂等键实现并验证"}})).unwrap()["workId"].as_str().unwrap().to_string();
    g.finish(&continuation.id, None, false);
    g.finish(&b, Some("缺少数据库连接，未执行写入".into()), false);
    let delivery = g.works.last().unwrap().clone();
    assert!(delivery.prompt.contains("缺少数据库连接"));
    assert!(delivery.prompt.contains("请分析需求"));
    g.finish(&delivery.id, None, false);
    assert!(g.works.iter().all(Work::terminal));
}

#[test]
fn resumed_children_cannot_call_their_waiting_ancestors() {
    let mut g = group();
    let c = g.members[0].id.clone();
    let a = g.members[1].id.clone();
    let b = g.members[2].id.clone();
    let root = g
        .enqueue("root", &c, "交付".into(), false, None, 0)
        .unwrap();
    let child = g
        .enqueue("root", &a, "分析".into(), false, Some(root.clone()), 1)
        .unwrap();
    let nested = g
        .enqueue("root", &b, "核查".into(), false, Some(child.clone()), 2)
        .unwrap();
    g.finish(&root, None, false);
    g.finish(&child, None, false);
    g.finish(&nested, None, false);
    let resumed = g.works.last_mut().unwrap();
    assert_eq!(resumed.member_id, a);
    resumed.status = "running".into();
    resumed.session_id = Some("resumed".into());
    assert!(mcp::call_in_group(
        &mut g,
        &a,
        "resumed",
        &json!({"name":"group_call_member","arguments":{"member":c,"prompt":"错误回调"}})
    )
    .unwrap_err()
    .contains("循环依赖"));
}

#[test]
fn exhausted_multi_member_delivery_has_a_visible_retryable_failure() {
    let mut g = group();
    let a = g.members[1].id.clone();
    let b = g.members[2].id.clone();
    g.max_calls = 2;
    let one = g
        .enqueue("root", &a, "分析".into(), false, None, 0)
        .unwrap();
    let two = g
        .enqueue("root", &b, "复核".into(), false, None, 0)
        .unwrap();
    g.finish(&one, None, false);
    g.finish(&two, None, false);
    let summary = g.works.last().unwrap();
    assert!(summary.summary);
    assert_eq!(summary.status, "failed");
    assert!(summary.error.as_ref().unwrap().contains("成员结果已保留"));
    g.ready_summaries();
    assert_eq!(g.works.len(), 3);
}

#[test]
fn legacy_reasoning_migration_is_atomic_and_does_not_replay_work() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("groups.sqlite");
    let store = Store::open(&path).unwrap();
    let mut g = group();
    let c = g.coordinator_id.clone();
    let work = g
        .enqueue("old", &c, "旧请求".into(), false, None, 0)
        .unwrap();
    g.works[0].status = "completed".into();
    g.works[0].output = "<analysis>隐藏内容</analysis>公开结果".into();
    g.post(
        Some(c.clone()),
        "assistant",
        "<ana".into(),
        Some(work.clone()),
    );
    g.post(
        Some(c),
        "assistant",
        "lysis>隐藏内容</analysis>公开结果".into(),
        Some(work),
    );
    g.works[0].prompt = "请解释 <analysis> 标签".into();
    let user = g.post(None, "user", "请解释 <analysis> 标签".into(), None);
    user.id = "old".into();
    store.create(&g).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM group_migrations", [])
        .unwrap();
    let migrated = Store::open(&path).unwrap().get(&g.id).unwrap();
    assert!(migrated.messages[0].text.is_empty());
    assert_eq!(migrated.messages[1].text, "公开结果");
    assert_eq!(migrated.works[0].output, "公开结果");
    assert_eq!(migrated.works[0].prompt, "请解释 <analysis> 标签");
    assert_eq!(migrated.works.len(), 1);
    assert_eq!(migrated.works[0].status, "completed");
    assert_eq!(Store::open(&path).unwrap().get(&g.id).unwrap(), migrated);
}

#[test]
fn explicit_user_corrections_update_shared_decisions_for_every_expert() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    let c = g.coordinator_id.clone();
    g.works[0].status = "running".into();
    g.works[0].session_id = Some("decision-session".into());
    g.decisions.push(Decision {
        id: "db".into(),
        text: "使用 MySQL".into(),
        message_id: Some(i.id.clone()),
        created_at: now(),
        confirmed: true,
        voided: false,
        history: vec![],
        updated_at: None,
    });
    let mut correction = input();
    correction.text = "更正已确认的数据库选择：改用 PostgreSQL。".into();
    send(&mut g, &correction).unwrap();
    let args = json!({"name":"group_correct_decision","arguments":{"decisionId":"db","action":"edit","text":"使用 PostgreSQL","messageId":correction.id,"evidence":"改用 PostgreSQL"}});
    mcp::call_in_group(&mut g, &c, "decision-session", &args).unwrap();
    assert_eq!(g.decisions[0].text, "使用 PostgreSQL");
    assert_eq!(g.decisions[0].history[0].text, "使用 MySQL");
    let context = runtime::decision_context(&g).unwrap();
    assert!(context.contains("PostgreSQL"));
    assert!(!context.contains("MySQL"));
    let mut spoof = args.clone();
    spoof["arguments"]["messageId"] = json!("unrelated-or-other-group");
    assert!(mcp::call_in_group(&mut g, &c, "decision-session", &spoof).is_err());
    spoof = args;
    spoof["arguments"]["evidence"] = json!("模型自行推断");
    assert!(mcp::call_in_group(&mut g, &c, "decision-session", &spoof).is_err());
}

#[test]
fn selected_expert_coordinator_receives_delivery_responsibility() {
    let mut g = group();
    g.coordinator_id = g.members[1].id.clone();
    let i = input();
    send(&mut g, &i).unwrap();
    assert_eq!(g.works[0].member_id, g.members[1].id);
    let prompt =
        runtime::work_prompt(&g, &g.works[0], &g.members[1], &Knowledge::default()).unwrap();
    assert!(prompt.contains("你是本群协调员"));
    assert!(prompt.contains(&format!("当前协调员：分析师 [{}]", g.coordinator_id)));
    let expert =
        runtime::work_prompt(&g, &g.works[0], &g.members[2], &Knowledge::default()).unwrap();
    assert!(expert.contains("无需逐句请协调员审批"));
}
#[test]
fn explicit_recipients_and_quoted_reply_use_member_identity() {
    let mut g = group();
    let member = g.members[1].id.clone();
    let m = g
        .post(Some(member.clone()), "assistant", "已确认".into(), None)
        .id
        .clone();
    let mut i = input();
    i.reply_to = Some(m);
    send(&mut g, &i).unwrap();
    assert_eq!(g.works[0].member_id, member);
    assert!(g.works[0].prompt.contains("已确认"));
}
#[test]
fn callbacks_wait_for_every_child_and_return_results_to_requester() {
    let mut g = group();
    let coordinator = g.coordinator_id.clone();
    let expert = g.members[1].id.clone();
    let coder = g.members[2].id.clone();
    let p = g
        .enqueue("root", &coordinator, "分工".into(), false, None, 0)
        .unwrap();
    let a = g
        .enqueue("root", &expert, "分析".into(), false, Some(p.clone()), 1)
        .unwrap();
    let b = g
        .enqueue("root", &coder, "实现".into(), false, Some(p.clone()), 1)
        .unwrap();
    g.finish(&p, None, false);
    g.finish(&a, None, false);
    assert_eq!(
        g.works.iter().find(|w| w.id == p).unwrap().status,
        "waiting_children"
    );
    assert!(!g.works.iter().any(|w| w.continuation_of.is_some()));
    g.finish(&b, Some("接口未完成".into()), false);
    let callback = g
        .works
        .iter()
        .find(|w| w.continuation_of.as_deref() == Some(&p))
        .unwrap()
        .clone();
    assert_eq!(callback.member_id, coordinator);
    assert!(callback.prompt.contains("接口未完成"));
    g.finish(&callback.id, None, false);
    assert_eq!(
        g.works.iter().find(|w| w.id == p).unwrap().status,
        "completed"
    );
}
#[test]
fn stopped_generation_never_starts_a_callback() {
    let mut g = group();
    let c = g.coordinator_id.clone();
    let expert = g.members[1].id.clone();
    let p = g
        .enqueue("root", &c, "分工".into(), false, None, 0)
        .unwrap();
    let a = g
        .enqueue("root", &expert, "工作".into(), false, Some(p.clone()), 1)
        .unwrap();
    g.finish(&p, None, false);
    g.epoch += 1;
    g.finish(&a, None, false);
    assert_eq!(g.works.len(), 2);
    assert_eq!(
        g.works.iter().find(|w| w.id == a).unwrap().status,
        "cancelled"
    );
}
#[test]
fn limits_block_runaway_work() {
    let mut g = group();
    let c = g.coordinator_id.clone();
    g.max_calls = 2;
    g.enqueue("root", &c, "a".into(), false, None, 0).unwrap();
    g.enqueue("root", &c, "b".into(), false, None, 0).unwrap();
    assert!(g.enqueue("root", &c, "c".into(), false, None, 0).is_err());
    assert!(g.enqueue("other", &c, "x".into(), false, None, 4).is_err());
}
#[test]
fn store_rolls_back_a_partly_invalid_multi_recipient_send() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let g = group();
    store.create(&g).unwrap();
    let mut i = input();
    i.member_ids = vec![g.coordinator_id.clone(), "unknown".into()];
    assert!(store.update(&g.id, |g| send(g, &i)).is_err());
    let restored = store.get(&g.id).unwrap();
    assert!(restored.messages.is_empty());
    assert!(restored.works.is_empty());
}
#[test]
fn recovery_preserves_queued_work_and_marks_unconfirmed_execution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groups.sqlite");
    let store = Store::open(&path).unwrap();
    let mut g = group();
    let c = g.coordinator_id.clone();
    let expert = g.members[1].id.clone();
    let running = g
        .enqueue("root", &c, "执行".into(), false, None, 0)
        .unwrap();
    g.enqueue("next", &expert, "后续".into(), false, None, 0)
        .unwrap();
    g.works.iter_mut().find(|w| w.id == running).unwrap().status = "running".into();
    g.members[0].session_id = Some("native-session".into());
    store.create(&g).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    store.recover().unwrap();
    let restored = store.get(&g.id).unwrap();
    assert!(restored.paused);
    assert_eq!(restored.works[0].status, "uncertain");
    assert_eq!(restored.works[1].status, "queued");
    assert_eq!(
        store.owner("native-session").unwrap(),
        Some((g.id, g.coordinator_id))
    );
}
#[test]
fn concurrent_transactions_do_not_lose_messages() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let g = group();
    store.create(&g).unwrap();
    let workers = (0..6)
        .map(|n| {
            let store = store.clone();
            let gid = g.id.clone();
            std::thread::spawn(move || {
                for i in 0..6 {
                    store
                        .update(&gid, |g| {
                            g.post(None, "user", format!("{n}:{i}"), None);
                            Ok(())
                        })
                        .unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    let restored = store.get(&g.id).unwrap();
    assert_eq!(restored.messages.len(), 36);
    assert_eq!(restored.messages.last().unwrap().seq, 36);
}
#[test]
fn group_database_and_member_history_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut a = group();
    let b = group();
    a.post(None, "user", "仅 A 可见".into(), None);
    store.create(&a).unwrap();
    store.create(&b).unwrap();
    assert!(store.get(&b.id).unwrap().messages.is_empty());
    assert_eq!(store.get(&a.id).unwrap().messages[0].text, "仅 A 可见");
}

#[test]
fn native_events_are_correlated_deduplicated_and_usage_survives_completion() {
    let mut g = group();
    let c = g.coordinator_id.clone();
    let work = g
        .enqueue("root", &c, "执行".into(), false, None, 0)
        .unwrap();
    g.works[0].session_id = Some("native".into());
    g.works[0].status = "running".into();
    let prompt = g.works[0].prompt_id.clone();
    let update = json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"真实输出"},"_groupEventId":"native-event-1"});
    runtime::apply_update(&mut g, "native", &c, &update).unwrap();
    runtime::apply_update(&mut g, "native", &c, &update).unwrap();
    assert_eq!(g.works[0].output, "真实输出");
    assert_eq!(g.messages.len(), 1);
    runtime::apply_update(&mut g, "native", &c, &json!({"sessionUpdate":"agent_message_chunk","promptId":"another-prompt","content":{"text":"不属于本次请求"}})).unwrap();
    assert_eq!(g.works[0].output, "真实输出");
    g.finish(&work, None, false);
    runtime::apply_update(&mut g,"native",&c,&json!({"sessionUpdate":"turn_completed","prompt_id":prompt,"usage":{"inputTokens":120,"outputTokens":30}})).unwrap();
    assert_eq!(g.works[0].tokens, 150);
}
#[test]
fn member_handoffs_are_scoped_idempotent_and_refuse_cycles() {
    let mut g = group();
    let c = g.coordinator_id.clone();
    let expert = g.members[1].id.clone();
    g.enqueue("root", &c, "分工".into(), false, None, 0)
        .unwrap();
    g.works[0].session_id = Some("coordinator-session".into());
    g.works[0].status = "running".into();
    let args =
        json!({"name":"group_call_member","arguments":{"member":expert,"prompt":"分析需求"}});
    let first = mcp::call_in_group(&mut g, &c, "coordinator-session", &args).unwrap();
    let again = mcp::call_in_group(&mut g, &c, "coordinator-session", &args).unwrap();
    assert_eq!(first["workId"], again["workId"]);
    assert_eq!(g.works.len(), 2);
    assert!(mcp::call_in_group(&mut g,&c,"coordinator-session",&json!({"name":"group_call_member","arguments":{"member":"other-group-member","prompt":"越界"}})).is_err());
    g.works[0].status = "waiting_children".into();
    g.works[1].session_id = Some("expert-session".into());
    g.works[1].status = "running".into();
    assert!(mcp::call_in_group(
        &mut g,
        &expert,
        "expert-session",
        &json!({"name":"group_call_member","arguments":{"member":c,"prompt":"循环"}})
    )
    .is_err());
}
#[test]
fn discussion_and_plain_text_mention_routing_are_bounded() {
    let mut g = group();
    let mut i = input();
    i.text = "@代码专家 请评审".into();
    send(&mut g, &i).unwrap();
    assert_eq!(g.works[0].member_id, g.members[2].id);
    assert!(mentioned_targets(&g, "`@代码专家` user@代码专家.com @代码专家.md").is_empty());
    g.works[0].discussion = true;
    g.works[0].session_id = Some("discussion".into());
    g.works[0].status = "running".into();
    let member = g.members[2].id.clone();
    let target = g.members[1].id.clone();
    assert!(mcp::call_in_group(
        &mut g,
        &member,
        "discussion",
        &json!({"name":"group_call_member","arguments":{"member":target,"prompt":"写入"}})
    )
    .is_err());
}

#[test]
fn knowledge_selection_is_explicit_bounded_and_preserved_with_the_request() {
    let mut g = group();
    let mut request = input();
    request.knowledge.context = "private excerpt".into();
    assert!(send(&mut g, &request).is_err());
    assert!(g.messages.is_empty());
    request.knowledge.personal = true;
    request.knowledge.organization_scope_ids = vec!["team-a".into()];
    assert!(send(&mut g, &request).is_err());
    request.knowledge.organization = true;
    send(&mut g, &request).unwrap();
    assert_eq!(g.messages[0].knowledge, request.knowledge);
}

#[test]
fn member_removal_keeps_history_but_cannot_receive_new_work() {
    let mut g = group();
    let member = g.members[1].id.clone();
    g.post(Some(member.clone()), "assistant", "已交付".into(), None);
    g.members[1].removed = true;
    assert!(g
        .enqueue("new", &member, "继续".into(), false, None, 0)
        .is_err());
    assert_eq!(g.messages[0].member_id.as_deref(), Some(member.as_str()));
}

#[test]
fn list_snapshot_keeps_old_active_work_when_newer_history_is_large() {
    let temporary = tempfile::tempdir().unwrap();
    let store = Store::open(&temporary.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    let member = g.members[1].id.clone();
    g.max_calls = 50;
    let running = g
        .enqueue("active-root", &member, "运行中".into(), false, None, 0)
        .unwrap();
    g.works[0].status = "running".into();
    for index in 0..110 {
        let key = g
            .enqueue(
                &format!("root-{index}"),
                &member,
                "旧工作".into(),
                false,
                None,
                0,
            )
            .unwrap();
        g.finish(&key, None, false);
    }
    store.create(&g).unwrap();
    assert!(store.list().unwrap()[0]
        .works
        .iter()
        .any(|w| w.id == running && w.active()));
}

#[test]
fn native_tool_activity_stays_in_one_member_turn_with_live_updates() {
    let mut g = group();
    let member = g.members[1].id.clone();
    g.enqueue("root", &member, "执行".into(), false, None, 0)
        .unwrap();
    g.works[0].session_id = Some("native".into());
    g.works[0].status = "running".into();
    for update in [
        json!({"sessionUpdate":"agent_message_chunk","content":{"text":"先检查"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"read-1","title":"读取文件","status":"in_progress"}),
        json!({"sessionUpdate":"agent_message_chunk","content":{"text":"结果已确认"}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"read-1","status":"completed"}),
    ] {
        runtime::apply_update(&mut g, "native", &member, &update).unwrap();
    }
    assert_eq!(
        g.messages
            .iter()
            .map(|m| m.kind.as_str())
            .collect::<Vec<_>>(),
        ["assistant"]
    );
    assert_eq!(g.messages[0].tool_calls[0]["status"], "completed");
    assert_eq!(g.messages[0].text, "先检查结果已确认");
}

#[test]
fn completing_a_nested_continuation_settles_the_original_request() {
    let mut g = group();
    let coordinator = g.coordinator_id.clone();
    let analyst = g.members[1].id.clone();
    let coder = g.members[2].id.clone();
    let original = g
        .enqueue("root", &coordinator, "交付".into(), false, None, 0)
        .unwrap();
    let first = g
        .enqueue(
            "root",
            &analyst,
            "分析".into(),
            false,
            Some(original.clone()),
            1,
        )
        .unwrap();
    g.finish(&original, None, false);
    g.finish(&first, None, false);
    let resumed = g
        .works
        .iter()
        .find(|w| w.continuation_of.as_deref() == Some(&original))
        .unwrap()
        .id
        .clone();
    let check = g
        .enqueue(
            "root",
            &coder,
            "复核".into(),
            false,
            Some(resumed.clone()),
            1,
        )
        .unwrap();
    g.finish(&resumed, None, false);
    g.finish(&check, None, false);
    let final_work = g
        .works
        .iter()
        .find(|w| w.continuation_of.as_deref() == Some(&resumed))
        .unwrap()
        .id
        .clone();
    g.works
        .iter_mut()
        .find(|w| w.id == final_work)
        .unwrap()
        .output = "交付已验证".into();
    g.finish(&final_work, None, false);
    assert!(g.works.iter().all(|w| w.terminal()));
    assert_eq!(
        g.works.iter().find(|w| w.id == original).unwrap().output,
        "交付已验证"
    );
}

#[test]
fn reaching_a_budget_settles_the_waiting_parent_and_its_queued_continuation() {
    let mut g = group();
    let coordinator = g.coordinator_id.clone();
    let expert = g.members[1].id.clone();
    let parent = g
        .enqueue("root", &coordinator, "交付".into(), false, None, 0)
        .unwrap();
    let child = g
        .enqueue(
            "root",
            &expert,
            "分析".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.finish(&parent, None, false);
    g.works.iter_mut().find(|w| w.id == child).unwrap().tokens = g.token_budget;
    g.finish(&child, None, false);
    g.enforce_budget();
    assert!(g.works.iter().all(|w| w.terminal()));
    assert_eq!(
        g.works.iter().find(|w| w.id == parent).unwrap().status,
        "failed"
    );
}

#[test]
fn duplicate_message_ids_cannot_overwrite_another_group() {
    let temporary = tempfile::tempdir().unwrap();
    let store = Store::open(&temporary.path().join("groups.sqlite")).unwrap();
    let mut first = group();
    let mut second = group();
    let request = input();
    send(&mut first, &request).unwrap();
    store.create(&first).unwrap();
    send(&mut second, &request).unwrap();
    assert!(store.create(&second).is_err());
    assert_eq!(store.get(&first.id).unwrap().messages[0].text, request.text);
    assert!(store.get(&second.id).is_err());
}

#[test]
fn pausing_dispatch_keeps_history_readable_but_refuses_new_handoffs_and_spoofed_members() {
    let mut g = group();
    let coordinator = g.coordinator_id.clone();
    let expert = g.members[1].id.clone();
    g.post(None, "user", "暂停期间仍可查阅的资料".into(), None);
    g.enqueue("root", &coordinator, "交付".into(), false, None, 0)
        .unwrap();
    g.works[0].session_id = Some("native".into());
    g.works[0].status = "running".into();
    g.paused = true;
    let history = json!({"name":"group_read_history","arguments":{}});
    assert!(mcp::call_in_group(&mut g, &coordinator, "native", &history)
        .unwrap()
        .to_string()
        .contains("暂停期间仍可查阅"));
    assert!(mcp::call_in_group(&mut g, &expert, "native", &history).is_err());
    assert!(mcp::call_in_group(
        &mut g,
        &coordinator,
        "native",
        &json!({"name":"group_call_member","arguments":{"member":expert,"prompt":"继续"}})
    )
    .is_err());
}

#[test]
fn historical_session_ownership_survives_a_member_context_reset() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    g.members[0].session_id = Some("previous-context".into());
    store.create(&g).unwrap();
    store
        .update(&g.id, |g| {
            g.members[0].session_id = None;
            g.members[0].generation += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store.owner("previous-context").unwrap(),
        Some((g.id.clone(), g.members[0].id.clone()))
    );
    store
        .bind_owner("late-cancelled-creation", &g.id, &g.members[0].id)
        .unwrap();
    assert!(store.get(&g.id).unwrap().members[0].session_id.is_none());
    assert_eq!(
        store.owner("late-cancelled-creation").unwrap(),
        Some((g.id, g.members[0].id.clone()))
    );
}

#[test]
fn stopping_a_queued_continuation_settles_the_original_and_never_resumes_it() {
    let mut g = group();
    let parent = g
        .enqueue(
            "root",
            &g.coordinator_id.clone(),
            "原始目标".into(),
            false,
            None,
            0,
        )
        .unwrap();
    let child = g
        .enqueue(
            "root",
            &g.members[1].id.clone(),
            "交付".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.finish(&parent, None, false);
    g.finish(&child, None, false);
    let callback = g
        .works
        .iter()
        .find(|w| w.continuation_of.as_deref() == Some(&parent))
        .unwrap()
        .id
        .clone();
    assert!(g.stop_work(Some(&callback)).unwrap().is_empty());
    assert!(g.works.iter().all(Work::terminal));
    assert_eq!(
        g.works.iter().find(|w| w.id == parent).unwrap().status,
        "cancelled"
    );
    let count = g.works.len();
    g.ready_callbacks();
    assert_eq!(g.works.len(), count);
}

#[test]
fn stopping_session_preparation_never_rebinds_a_replaced_member_context() {
    let mut g = group();
    let work = g
        .enqueue(
            "root",
            &g.coordinator_id.clone(),
            "启动".into(),
            false,
            None,
            0,
        )
        .unwrap();
    g.works[0].status = "dispatching".into();
    g.stop_work(None).unwrap();
    g.members[0].generation += 1;
    g.members[0].model_id = "replacement-model".into();
    assert!(!g.bind_session(&work, "obsolete-native-session"));
    assert!(g.members[0].session_id.is_none());
}

#[test]
fn stop_waits_for_a_running_continuation_and_propagates_its_terminal_state() {
    let mut g = group();
    let parent = g
        .enqueue(
            "root",
            &g.coordinator_id.clone(),
            "协调".into(),
            false,
            None,
            0,
        )
        .unwrap();
    let child = g
        .enqueue(
            "root",
            &g.members[1].id.clone(),
            "执行".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.finish(&parent, None, false);
    g.finish(&child, None, false);
    let callback = g
        .works
        .iter_mut()
        .find(|w| w.continuation_of.as_deref() == Some(&parent))
        .unwrap();
    callback.status = "running".into();
    callback.session_id = Some("native".into());
    let id = callback.id.clone();
    assert_eq!(g.stop_work(Some(&id)).unwrap().len(), 1);
    assert_eq!(
        g.works.iter().find(|w| w.id == id).unwrap().status,
        "stopping"
    );
    g.finish(&id, None, false);
    assert!(g.works.iter().all(Work::terminal));
    assert_eq!(
        g.works.iter().find(|w| w.id == parent).unwrap().status,
        "cancelled"
    );
}

#[test]
fn model_failure_after_dispatch_still_returns_real_results_without_replaying_work() {
    let mut g = group();
    let parent = g
        .enqueue(
            "root",
            &g.coordinator_id.clone(),
            "完成审批交付".into(),
            false,
            None,
            0,
        )
        .unwrap();
    let child = g
        .enqueue(
            "root",
            &g.members[1].id.clone(),
            "交付".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.finish(&parent, Some("模型连接中断".into()), false);
    assert_eq!(
        g.works.iter().find(|w| w.id == parent).unwrap().status,
        "waiting_children"
    );
    g.works.iter_mut().find(|w| w.id == child).unwrap().output = "文件已生成并验证".into();
    g.finish(&child, None, false);
    let callback = g
        .works
        .iter()
        .find(|w| w.continuation_of.as_deref() == Some(&parent))
        .unwrap()
        .clone();
    assert!(callback.prompt.contains("完成审批交付"));
    assert!(callback.prompt.contains("模型连接中断"));
    assert!(callback.prompt.contains("文件已生成并验证"));
    g.finish(&callback.id, None, false);
    assert!(g.works.iter().all(Work::terminal));
    assert_eq!(g.works.len(), 3);
    assert!(g
        .works
        .iter()
        .find(|w| w.id == parent)
        .unwrap()
        .error
        .is_none());
}

#[test]
fn recovery_repairs_legacy_dependency_states_and_pauses_before_any_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    let parent = g
        .enqueue(
            "root",
            &g.coordinator_id.clone(),
            "旧请求".into(),
            false,
            None,
            0,
        )
        .unwrap();
    let child = g
        .enqueue(
            "root",
            &g.members[1].id.clone(),
            "交付".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.finish(&parent, None, false);
    g.finish(&child, None, false);
    g.works
        .iter_mut()
        .find(|w| w.continuation_of.as_deref() == Some(&parent))
        .unwrap()
        .status = "cancelled".into();
    let other = g
        .enqueue(
            "other",
            &g.coordinator_id.clone(),
            "异常请求".into(),
            false,
            None,
            0,
        )
        .unwrap();
    g.enqueue(
        "other",
        &g.members[1].id.clone(),
        "尚未执行".into(),
        false,
        Some(other.clone()),
        1,
    )
    .unwrap();
    let w = g.works.iter_mut().find(|w| w.id == other).unwrap();
    w.status = "failed".into();
    w.error = Some("旧版本模型异常".into());
    store.create(&g).unwrap();
    store.recover().unwrap();
    let restored = store.get(&g.id).unwrap();
    assert_eq!(
        restored
            .works
            .iter()
            .find(|w| w.id == parent)
            .unwrap()
            .status,
        "cancelled"
    );
    assert_eq!(
        restored
            .works
            .iter()
            .find(|w| w.id == other)
            .unwrap()
            .status,
        "waiting_children"
    );
    assert!(restored.paused);
    assert_eq!(restored.works.len(), g.works.len());
}

#[tokio::test]
async fn stop_during_preparation_fences_a_delayed_native_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    let work = g
        .enqueue(
            "root",
            &g.coordinator_id.clone(),
            "写入文件".into(),
            false,
            None,
            0,
        )
        .unwrap();
    g.works[0].status = "dispatching".into();
    g.works[0].session_id = Some("native".into());
    let prompt_id = g.works[0].prompt_id.clone();
    store.create(&g).unwrap();
    let gate = runtime::dispatch_lock(&g.id).lock_owned().await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let pending = tokio::spawn({
        let store = store.clone();
        let id = g.id.clone();
        let work = work.clone();
        async move {
            runtime::dispatch_prompt(
                &store,
                &id,
                &work,
                &tx,
                crate::agent_runtime::GroupPromptRequest {
                    session: "native",
                    prompt_id: &prompt_id,
                    text: "写入文件",
                    attachments: &[],
                },
            )
            .await
        }
    });
    store
        .update(&g.id, |g| {
            assert!(g.stop_work(None)?.is_empty());
            Ok(())
        })
        .unwrap();
    drop(gate);
    assert!(pending.await.unwrap().unwrap().is_none());
    assert!(rx.try_recv().is_err());
    assert_eq!(store.get(&g.id).unwrap().works[0].status, "cancelled");
}

#[tokio::test]
async fn paused_preparation_returns_to_queue_and_dispatch_enqueues_before_awaiting_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    let work = g
        .enqueue(
            "root",
            &g.coordinator_id.clone(),
            "分析".into(),
            true,
            None,
            0,
        )
        .unwrap();
    g.works[0].status = "dispatching".into();
    g.paused = true;
    let prompt_id = g.works[0].prompt_id.clone();
    store.create(&g).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(runtime::dispatch_prompt(
        &store,
        &g.id,
        &work,
        &tx,
        crate::agent_runtime::GroupPromptRequest {
            session: "native",
            prompt_id: &prompt_id,
            text: "分析",
            attachments: &[],
        },
    )
    .await
    .unwrap()
    .is_none());
    assert!(rx.try_recv().is_err());
    assert_eq!(store.get(&g.id).unwrap().works[0].status, "queued");
    store
        .update(&g.id, |g| {
            g.paused = false;
            g.works[0].status = "dispatching".into();
            Ok(())
        })
        .unwrap();
    let pending = runtime::dispatch_prompt(
        &store,
        &g.id,
        &work,
        &tx,
        crate::agent_runtime::GroupPromptRequest {
            session: "native",
            prompt_id: &prompt_id,
            text: "分析",
            attachments: &[],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let echo_agent_acp::AcpAgentMessage::Prompt(args) = rx.try_recv().unwrap() else {
        panic!("expected native prompt");
    };
    assert_eq!(args.request.session_id.0.as_ref(), "native");
    args.response_tx
        .send(Ok(agent_client_protocol::PromptResponse::new(
            agent_client_protocol::StopReason::EndTurn,
        )))
        .unwrap();
    pending.complete().await.unwrap();
    assert_eq!(store.get(&g.id).unwrap().works[0].status, "running");
}

#[test]
fn group_delete_cascades_records_and_ownership_but_keeps_templates_and_other_groups() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut first = group();
    first.members[0].session_id = Some("native-current".into());
    first.post(None, "user", "需要删除的记录".into(), None);
    let work = first
        .enqueue(
            "request",
            &first.coordinator_id.clone(),
            "工作".into(),
            false,
            None,
            0,
        )
        .unwrap();
    first.finish(&work, None, false);
    store.create(&first).unwrap();
    store
        .bind_owner("native-old-workspace", &first.id, &first.members[0].id)
        .unwrap();
    store
        .stage_session_delete(&first.id, "native-old-workspace", "/old")
        .unwrap();
    let other = group();
    store.create(&other).unwrap();
    let template = management::TeamTemplate {
        id: id(),
        name: "保留模板".into(),
        description: "".into(),
        source: "常用团队".into(),
        members: vec![],
        coordinator_name: None,
        coordinator_role: None,
        default_model_id: None,
    };
    store.save_template(&template).unwrap();
    assert_eq!(store.sessions(&first.id).unwrap().len(), 2);
    store.delete(&first.id).unwrap();
    assert!(store.get(&first.id).is_err());
    assert!(store.owner("native-current").unwrap().is_none());
    assert!(store.owner("native-old-workspace").unwrap().is_none());
    assert!(store.deletion_sessions(&first.id).unwrap().is_empty());
    let receipts = Store::open(&store.path).unwrap().pending_cleanup().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].group_id, first.id);
    assert!(receipts[0]
        .session_ids
        .contains(&"native-current".to_string()));
    store.ack_cleanup(&first.id).unwrap();
    assert!(store.pending_cleanup().unwrap().is_empty());
    assert!(store.get(&other.id).is_ok());
    assert_eq!(store.templates().unwrap()[0].id, template.id);
    store
        .bind_owner("late-session", &first.id, &first.members[0].id)
        .unwrap();
    assert!(store.owner("late-session").unwrap().is_none());
}

#[test]
fn group_delete_refuses_unfinished_work_without_removing_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    g.post(None, "user", "保留我的记录".into(), None);
    g.enqueue(
        "request",
        &g.coordinator_id.clone(),
        "未完成".into(),
        false,
        None,
        0,
    )
    .unwrap();
    store.create(&g).unwrap();
    assert!(store.delete(&g.id).unwrap_err().contains("尚未确认停止"));
    assert_eq!(store.get(&g.id).unwrap().messages.len(), 1);
}

#[test]
fn group_catalog_does_not_lose_older_groups_behind_five_hundred_archives() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut old = group();
    old.updated_at = 1;
    store.create(&old).unwrap();
    for index in 0..500 {
        let mut g = group();
        g.archived = true;
        g.updated_at = index + 2;
        store.create(&g).unwrap();
    }
    let groups = store.list().unwrap();
    assert_eq!(groups.len(), 501);
    assert!(groups.iter().any(|g| g.id == old.id && !g.archived));
}

#[test]
fn catalog_previews_exclude_expert_and_decision_history_without_altering_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    g.members[0].profile = json!({"promptBody":"private expert instructions"});
    g.post(None, "user", "请求".into(), None).knowledge.context = "private knowledge".into();
    g.decisions.push(Decision {
        id: id(),
        text: "有效决定".into(),
        message_id: None,
        created_at: now(),
        confirmed: true,
        voided: false,
        history: vec![],
        updated_at: None,
    });
    store.create(&g).unwrap();
    let preview = store.list().unwrap().remove(0);
    assert!(preview.decisions.is_empty());
    assert_eq!(preview.members[0].profile, json!({}));
    assert!(preview.messages[0].knowledge.context.is_empty());
    let full = store.get(&g.id).unwrap();
    assert_eq!(full.members[0].profile, g.members[0].profile);
    assert_eq!(full.decisions.len(), 1);
    assert_eq!(full.messages[0].knowledge.context, "private knowledge");
}

#[test]
fn decisions_can_be_corrected_voided_and_restored_with_audit_history() {
    let mut g = group();
    for index in 0..101 {
        g.decisions.push(Decision {
            id: index.to_string(),
            text: "旧决定".into(),
            message_id: None,
            created_at: now(),
            confirmed: true,
            voided: false,
            history: vec![],
            updated_at: None,
        });
    }
    management::change_decision(&mut g, "100", "edit", Some("更正后的决定")).unwrap();
    assert_eq!(g.decisions[100].text, "更正后的决定");
    assert_eq!(g.decisions[100].history[0].text, "旧决定");
    management::change_decision(&mut g, "100", "void", None).unwrap();
    assert!(g.decisions[100].voided);
    assert!(
        management::change_decision(&mut g, "100", "edit", Some("不能偷偷更改作废记录")).is_err()
    );
    management::change_decision(&mut g, "100", "restore", None).unwrap();
    assert!(!g.decisions[100].voided);
    assert!(management::change_decision(&mut g, "100", "edit", Some(" ")).is_err());
}

#[test]
fn decision_context_uses_current_versions_and_bounds_prompt_without_losing_history() {
    let mut g = group();
    g.decisions.push(Decision {
        id: id(),
        text: "当前决定".into(),
        message_id: None,
        created_at: now(),
        confirmed: true,
        voided: false,
        history: vec![DecisionVersion {
            action: "edit".into(),
            text: "已过期的旧决定".into(),
            changed_at: now(),
        }],
        updated_at: None,
    });
    let context = runtime::decision_context(&g).unwrap();
    assert!(context.contains("当前决定"));
    assert!(!context.contains("已过期的旧决定"));
    g.decisions[0].text = "长".repeat(24_001);
    assert!(runtime::decision_context(&g).is_err());
    assert_eq!(g.decisions[0].text.chars().count(), 24_001);
    g.decisions[0].voided = true;
    assert!(runtime::decision_context(&g).is_ok());
}

#[test]
fn group_memory_cleanup_removes_only_the_owned_namespace_and_can_be_retried() {
    let home = tempfile::tempdir().unwrap();
    let owned = home
        .path()
        .join("group-memory/owned-group/member/generation-1");
    let other = home.path().join("group-memory/another-group");
    let experts = home.path().join("agents");
    for path in [&owned, &other, &experts] {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("MEMORY.md"), "保留相应范围的记忆").unwrap();
    }
    remove_group_memory(home.path(), "owned-group").unwrap();
    assert!(!home.path().join("group-memory/owned-group").exists());
    assert!(other.join("MEMORY.md").exists());
    assert!(experts.join("MEMORY.md").exists());
    remove_group_memory(home.path(), "owned-group").unwrap();
    assert!(remove_group_memory(home.path(), "../agents").is_err());
    #[cfg(unix)]
    {
        let linked = home.path().join("group-memory/linked-group");
        std::os::unix::fs::symlink(&experts, &linked).unwrap();
        assert!(remove_group_memory(home.path(), "linked-group").is_err());
        assert!(experts.join("MEMORY.md").exists());
        let redirected = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            home.path().join("group-memory"),
            redirected.path().join("group-memory"),
        )
        .unwrap();
        assert!(remove_group_memory(redirected.path(), "another-group").is_err());
        assert!(other.join("MEMORY.md").exists());
    }
}

#[test]
fn template_library_only_contains_explicit_saves_and_is_independent_of_session_teams() {
    if std::env::var_os("ECHOAGENT_TEMPLATE_LIBRARY_TEST").is_none() {
        let home = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "groups::tests::template_library_only_contains_explicit_saves_and_is_independent_of_session_teams"])
            .env("ECHO_AGENT_HOME", home.path())
            .env("ECHOAGENT_TEMPLATE_LIBRARY_TEST", "1")
            .status().unwrap();
        assert!(status.success());
        return;
    }
    let home = crate::paths::echo_agent_home_dir();
    let store = Store::open(&home.join("collaboration.sqlite")).unwrap();
    assert!(STORE.set(store.clone()).is_ok());
    // Unreadable session assignments must not make saved templates unreadable.
    std::fs::write(
        home.join("echoagent-teams.json"),
        "invalid historical teams",
    )
    .unwrap();
    assert!(management::group_team_templates().unwrap().is_empty());
    let mut original = group();
    original.members[1].role = "分析与验收".into();
    original.members[1].model_id = "member-model".into();
    store.create(&original).unwrap();
    let saved = management::group_team_save(original.id.clone(), "复用模板".into()).unwrap();
    assert_eq!(saved.members[1].role, "分析与验收");
    assert_eq!(saved.members[1].model_id.as_deref(), Some("member-model"));
    assert_eq!(saved.coordinator_name.as_deref(), Some("协调员"));
    let updated = management::group_team_save(original.id.clone(), "复用模板".into()).unwrap();
    assert_eq!(updated.id, saved.id);
    assert_eq!(management::group_team_templates().unwrap().len(), 1);
    management::group_team_delete(saved.id).unwrap();
    assert!(management::group_team_templates().unwrap().is_empty());
    assert!(store.get(&original.id).is_ok());
}

#[test]
fn source_creation_is_atomic_idempotent_and_includes_archived_groups() {
    let dir = tempfile::tempdir().unwrap();
    let store = store::Store::open(&dir.path().join("groups.sqlite")).unwrap();
    let mut original = group();
    original.source_session_id = Some("source".into());
    original.source_team_id = Some("team".into());
    original.archived = true;
    let attempts: Vec<_> = (0..6)
        .map(|_| {
            let store = store.clone();
            let mut candidate = original.clone();
            candidate.id = id();
            std::thread::spawn(move || store.create(&candidate).unwrap().id)
        })
        .collect();
    let ids: Vec<_> = attempts.into_iter().map(|a| a.join().unwrap()).collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(store.list().unwrap().len(), 1);
    let existing = store.get(&ids[0]).unwrap();
    assert!(existing.archived);
    assert_eq!(store.create(&existing).unwrap().id, existing.id);
    let mut other = original;
    other.source_session_id = Some("other-source".into());
    store.create(&other).unwrap();
    assert_eq!(store.list().unwrap().len(), 2);
}

#[test]
fn explicit_reply_fences_supplements_and_running_children_receive_corrections() {
    let mut g = group();
    let old = input();
    send(&mut g, &old).unwrap();
    let owner = g.coordinator_id.clone();
    g.works[0].status = "completed".into();
    let old_work = g.works[0].id.clone();
    let answer = g
        .post(
            Some(owner),
            "assistant",
            "旧方案的明确引用".into(),
            Some(old_work),
        )
        .id
        .clone();
    let current = input();
    send(&mut g, &current).unwrap();
    g.works[1].status = "running".into();
    let mut reply = input();
    reply.reply_to = Some(answer.clone());
    reply.text = "请展开这个方案".into();
    send(&mut g, &reply).unwrap();
    assert!(g.messages.last().unwrap().request_id.is_none());
    assert!(g.works.last().unwrap().prompt.contains("旧方案的明确引用"));
    // A correction aimed at the currently running delegated expert joins its root.
    let parent = g.works[1].id.clone();
    g.works[1].status = "waiting_children".into();
    let expert = g.members[2].id.clone();
    let child = g
        .enqueue(
            &current.id,
            &expert,
            "实现接口".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.works.iter_mut().find(|w| w.id == child).unwrap().status = "running".into();
    let mut correction = input();
    correction.member_ids = vec![expert.clone()];
    correction.text = "改用 PostgreSQL".into();
    send(&mut g, &correction).unwrap();
    assert_eq!(
        g.messages.last().unwrap().request_id.as_deref(),
        Some(current.id.as_str())
    );
    assert_eq!(g.messages.last().unwrap().member_ids, vec![expert]);
    g.finish(&child, None, false);
    let continuation = g
        .works
        .iter()
        .find(|w| w.continuation_of.as_deref() == Some(&child))
        .unwrap()
        .clone();
    assert_eq!(
        g.works.iter().find(|w| w.id == parent).unwrap().status,
        "waiting_children"
    );
    assert!(g
        .followup_context(&current.id)
        .unwrap()
        .contains("PostgreSQL"));
    g.works
        .iter_mut()
        .find(|w| w.id == continuation.id)
        .unwrap()
        .context_seq = g.messages.last().unwrap().seq;
    g.finish(&continuation.id, None, false);
    assert!(g
        .works
        .iter()
        .any(|w| w.continuation_of.as_deref() == Some(&parent)));
}

#[test]
fn consumed_followup_completes_without_an_extra_call_at_the_member_limit() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    let owner = g.coordinator_id.clone();
    g.works[0].status = "completed".into();
    for _ in 0..2 {
        let previous = g.works.last().unwrap().id.clone();
        g.enqueue(&i.id, &owner, "汇总交付".into(), false, None, 0)
            .unwrap();
        let next = g.works.last_mut().unwrap();
        next.status = "completed".into();
        next.continuation_of = Some(previous);
    }
    let w = g.works.last_mut().unwrap();
    w.status = "running".into();
    w.session_id = Some("consumed-native".into());
    w.context_seq = g.messages.last().unwrap().seq;
    let work = w.id.clone();
    let prompt = w.prompt_id.clone();
    let mut correction = input();
    correction.text = "使用中文交付".into();
    send(&mut g, &correction).unwrap();
    let ack = json!({"sessionUpdate":"group_interjection_consumed","promptId":"stale","interjectionId":correction.id});
    runtime::apply_update(&mut g, "consumed-native", &owner, &ack).unwrap();
    assert!(g.works[2].consumed_followups.is_empty());
    let ack = json!({"sessionUpdate":"group_interjection_consumed","promptId":prompt,"interjectionId":correction.id});
    runtime::apply_update(&mut g, "consumed-native", &owner, &ack).unwrap();
    runtime::apply_update(&mut g, "consumed-native", &owner, &ack).unwrap();
    assert_eq!(g.works[2].consumed_followups.len(), 1);
    g.finish(&work, None, false);
    assert_eq!(g.works.len(), 3);
    assert_eq!(g.works[2].status, "completed");
}

#[test]
fn retry_retains_effective_attachments_knowledge_and_quoted_supplement() {
    let mut g = group();
    let mut i = input();
    i.knowledge.personal = true;
    i.attachments = vec!["/isolated/old.png".into()];
    send(&mut g, &i).unwrap();
    g.works[0].status = "running".into();
    let owner = g.coordinator_id.clone();
    let work = g.works[0].id.clone();
    let quoted = g
        .post(
            Some(owner),
            "assistant",
            "必须核对的引用".into(),
            Some(work.clone()),
        )
        .id
        .clone();
    let mut correction = input();
    correction.knowledge.personal = true;
    correction.knowledge.context = "来自新知识摘录的约束".into();
    correction.attachments = vec!["/isolated/new.png".into(), "/isolated/old.png".into()];
    correction.reply_to = Some(quoted);
    send(&mut g, &correction).unwrap();
    g.works[0].status = "failed".into();
    g.retry_work(&work).unwrap();
    let retry = g.works.last().unwrap();
    let root = g.messages.last().unwrap();
    assert_eq!(
        root.attachments,
        vec!["/isolated/old.png", "/isolated/new.png"]
    );
    assert!(root.knowledge.personal);
    assert!(retry.prompt.contains("来自新知识摘录的约束"));
    assert!(retry.prompt.contains("必须核对的引用"));
}

#[test]
fn current_task_stop_respects_reference_and_ambiguity_without_stopping_other_requests() {
    let mut g = group();
    let a = input();
    send(&mut g, &a).unwrap();
    g.works[0].status = "running".into();
    g.works[0].session_id = Some("a".into());
    let mut b = input();
    b.member_ids = vec![g.members[1].id.clone()];
    send(&mut g, &b).unwrap();
    g.works[1].status = "running".into();
    g.works[1].session_id = Some("b".into());
    let mut stop = input();
    stop.text = "停止当前任务".into();
    assert!(send(&mut g, &stop).is_err());
    stop.reply_to = Some(a.id.clone());
    send(&mut g, &stop).unwrap();
    assert_eq!(g.works[0].status, "stopping");
    assert_eq!(g.works[1].status, "running");
    assert!(!g.paused);
    let epoch = g.epoch;
    send(&mut g, &stop).unwrap();
    assert_eq!(g.epoch, epoch);
    let mut all = input();
    all.text = "停止全部".into();
    send(&mut g, &all).unwrap();
    assert!(g.paused);
    assert!(g.works.iter().all(|w| w.status == "stopping"));
}

#[tokio::test]
async fn cancellation_attempts_every_session_and_can_retry_unacknowledged_work() {
    use echo_agent_acp::AcpAgentMessage;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let peer = tokio::spawn(async move {
        let mut attempts = vec![];
        for n in 0..4 {
            match rx.recv().await.unwrap() {
                AcpAgentMessage::Cancel(args) => {
                    attempts.push(args.request.session_id.0.to_string());
                    let result = if n == 0 {
                        Err(agent_client_protocol::Error::internal_error()
                            .data("injected cancellation failure"))
                    } else {
                        Ok(())
                    };
                    args.response_tx.send(result).unwrap();
                }
                _ => panic!("expected cancellation"),
            }
        }
        attempts
    });
    let sessions = vec![
        ("first".into(), "p1".into()),
        ("second".into(), "p2".into()),
    ];
    assert!(cancel_group_sessions(Some(&tx), sessions.clone())
        .await
        .is_err());
    cancel_group_sessions(Some(&tx), sessions).await.unwrap();
    assert_eq!(
        peer.await.unwrap(),
        vec!["first", "second", "first", "second"]
    );
}

#[test]
fn migration_preserves_confirmed_user_literals_and_a_recoverable_snapshot() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("groups.sqlite");
    let store = Store::open(&path).unwrap();
    let mut g = group();
    let original = "接口返回 <analysis> 节点，必须保留原来的 XML 结构";
    let user = g.post(None, "user", original.into(), None).id.clone();
    let owner = g.coordinator_id.clone();
    g.enqueue(
        &user,
        &owner,
        format!("回复引用：示例\n{original}"),
        false,
        None,
        0,
    )
    .unwrap();
    g.decisions.push(Decision {
        id: id(),
        text: original.into(),
        message_id: Some(user),
        created_at: now(),
        confirmed: true,
        voided: false,
        history: vec![DecisionVersion {
            text: original.into(),
            changed_at: now(),
            action: "edit".into(),
        }],
        updated_at: None,
    });
    store.create(&g).unwrap();
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute("DELETE FROM group_migrations", []).unwrap();
    c.execute("DELETE FROM group_migration_backups", [])
        .unwrap();
    let migrated = Store::open(&path).unwrap().get(&g.id).unwrap();
    assert_eq!(migrated.decisions, g.decisions);
    assert_eq!(migrated.works[0].prompt, g.works[0].prompt);
    let snapshot: String = c
        .query_row(
            "SELECT data FROM group_migration_backups WHERE group_id=?",
            [&g.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<Group>(&snapshot).unwrap(), g);
}

#[test]
fn history_window_applies_member_cutoff_direction_and_limits() {
    let mut g = group();
    for n in 0..5000 {
        g.post(None, "user", format!("消息{n}"), None);
    }
    g.members[0].history_start_seq = 4900;
    let page = runtime::history_window(&g, &g.members[0], 4950, 3, false, false).unwrap();
    assert_eq!(
        page.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![4951, 4952, 4953]
    );
    let tail = runtime::history_window(&g, &g.members[0], 0, 3, true, false).unwrap();
    assert_eq!(
        tail.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![4998, 4999, 5000]
    );
}

#[test]
fn duplicate_stop_retries_pending_cancellations_without_repeating_state_changes() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    g.works[0].status = "running".into();
    g.works[0].session_id = Some("pending-stop".into());
    let mut stop = input();
    stop.text = "停止全部".into();
    send(&mut g, &stop).unwrap();
    let epoch = g.epoch;
    let count = g.messages.len();
    let first = stopping_sessions(&g, &stop.id).unwrap();
    send(&mut g, &stop).unwrap();
    assert_eq!(g.epoch, epoch);
    assert_eq!(g.messages.len(), count);
    assert_eq!(stopping_sessions(&g, &stop.id).unwrap(), first);
    assert_eq!(first.len(), 1);
    assert!(stopping_sessions(&g, &i.id).is_err());
}

#[test]
fn migration_literal_masking_handles_short_overlapping_user_messages() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("groups.sqlite");
    let store = Store::open(&path).unwrap();
    let mut g = group();
    let mut i = input();
    i.text = "a <analysis> XML literal".into();
    send(&mut g, &i).unwrap();
    g.works[0].status = "running".into();
    let mut supplement = input();
    supplement.text = "a".into();
    send(&mut g, &supplement).unwrap();
    g.works[0].prompt = format!(
        "原始请求：{}\n补充：a\n<think>模型隐藏内容</think>公开交付",
        i.text
    );
    store.create(&g).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM group_migrations", [])
        .unwrap();
    let migrated = Store::open(&path).unwrap().get(&g.id).unwrap();
    assert!(migrated.works[0].prompt.contains(&i.text));
    assert!(migrated.works[0].prompt.contains("补充：a"));
    assert!(!migrated.works[0].prompt.contains("模型隐藏内容"));
    assert!(migrated.works[0].prompt.ends_with("公开交付"));
}

#[test]
fn stopping_a_partially_completed_multi_member_request_never_starts_a_summary() {
    let mut g = group();
    let mut i = input();
    i.discussion = true;
    i.member_ids = vec![g.members[1].id.clone(), g.members[2].id.clone()];
    send(&mut g, &i).unwrap();
    g.works[0].status = "completed".into();
    g.works[1].status = "running".into();
    g.works[1].session_id = Some("still-running".into());
    let running = g.works[1].id.clone();
    let mut stop = input();
    stop.text = "停止当前任务".into();
    stop.reply_to = Some(i.id.clone());
    send(&mut g, &stop).unwrap();
    assert!(!g.paused);
    assert_eq!(g.works.len(), 2);
    g.finish(&running, None, false);
    g.ready_callbacks();
    g.ready_summaries();
    assert_eq!(g.works.len(), 2);
    assert!(g.works.iter().all(Work::terminal));
    assert!(!g.works.iter().any(|w| w.summary));
}

#[test]
fn owner_corrections_reach_running_children_but_direct_expert_feedback_is_scoped() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    g.works[0].status = "waiting_children".into();
    let parent = g.works[0].id.clone();
    let a = g.members[1].id.clone();
    let b = g.members[2].id.clone();
    g.enqueue(&i.id, &a, "分析".into(), false, Some(parent.clone()), 1)
        .unwrap();
    g.works[1].status = "running".into();
    g.enqueue(&i.id, &b, "核对".into(), false, Some(parent), 1)
        .unwrap();
    let owner = input();
    send(&mut g, &owner).unwrap();
    let owner = g.messages.last().unwrap();
    assert_eq!(owner.request_id.as_deref(), Some(i.id.as_str()));
    assert!(g.followup_applies(owner, &g.works[1]));
    assert!(g.followup_applies(owner, &g.works[2]));
    let mut directed = input();
    directed.member_ids = vec![a];
    send(&mut g, &directed).unwrap();
    let directed = g.messages.last().unwrap();
    assert!(g.followup_applies(directed, &g.works[1]));
    assert!(!g.followup_applies(directed, &g.works[2]));
    assert!(g.followup_applies(directed, &g.works[0]));
}

#[test]
fn coordinator_can_receive_corrections_for_a_multi_recipient_request_before_summary() {
    let mut g = group();
    let mut i = input();
    i.discussion = true;
    i.member_ids = vec![g.members[1].id.clone(), g.members[2].id.clone()];
    send(&mut g, &i).unwrap();
    let mut correction = input();
    correction.discussion = true;
    correction.reply_to = Some(i.id.clone());
    send(&mut g, &correction).unwrap();
    assert_eq!(
        g.messages.last().unwrap().request_id.as_deref(),
        Some(i.id.as_str())
    );
    assert_eq!(g.works.len(), 2);
}

#[test]
fn replying_to_completed_child_while_owner_delivers_adds_a_dependency_to_that_request() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    let parent = g.works[0].id.clone();
    let expert = g.members[1].id.clone();
    let child = g
        .enqueue(
            &i.id,
            &expert,
            "前一阶段".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.finish(&parent, None, false);
    g.finish(&child, None, false);
    let callback = g.works.last_mut().unwrap();
    callback.status = "running".into();
    callback.session_id = Some("owner-final".into());
    let callback = callback.id.clone();
    let answer = g
        .post(
            Some(expert.clone()),
            "assistant",
            "前一阶段实际产物".into(),
            Some(child),
        )
        .id
        .clone();
    let mut correction = input();
    correction.reply_to = Some(answer);
    correction.text = "请修正这个产物".into();
    send(&mut g, &correction).unwrap();
    let message = g.messages.last().unwrap();
    assert_eq!(message.request_id.as_deref(), Some(i.id.as_str()));
    let assigned = g
        .works
        .iter()
        .find(|w| Some(&w.id) == message.work_id.as_ref())
        .unwrap()
        .clone();
    assert_eq!(assigned.member_id, expert);
    assert_eq!(assigned.parent_id.as_deref(), Some(callback.as_str()));
    let owner = g.coordinator_id.clone();
    let repeat = mcp::call_in_group(
        &mut g,
        &owner,
        "owner-final",
        &json!({"name":"group_call_member","arguments":{"member":expert,"prompt":"无需重复追加"}}),
    )
    .unwrap();
    assert_eq!(repeat["workId"], assigned.id);
    g.finish(&callback, None, false);
    assert_eq!(
        g.works.iter().find(|w| w.id == callback).unwrap().status,
        "waiting_children"
    );
    g.works
        .iter_mut()
        .find(|w| w.id == assigned.id)
        .unwrap()
        .context_seq = g.messages.last().unwrap().seq;
    g.finish(&assigned.id, None, false);
    assert!(g
        .works
        .iter()
        .any(|w| w.continuation_of.as_deref() == Some(callback.as_str())));
}

#[test]
fn recall_rule_snapshots_preserve_forgetting_semantics_and_refresh_between_reads() {
    use echo_agent_memory::{records, MemoryStorage};
    let home = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::new_flat(std::path::Path::new("/workspace"), home.path());
    let before = records::RecallRules::load(&storage).unwrap();
    records::forget_topic(&storage, "workspace", "回答应简短并先给结论").unwrap();
    let after = records::RecallRules::load(&storage).unwrap();
    assert!(before.permits("请先给结论，回答应简短"));
    assert!(!after.permits("请先给结论，回答应简短"));
    assert!(after.permits("审批接口必须校验幂等键"));
    // Once loaded, checking a page does not reopen the file for every row.
    std::fs::rename(
        home.path().join("records.json"),
        home.path().join("saved.json"),
    )
    .unwrap();
    assert!(!after.permits("请先给结论，回答应简短"));
    assert!(records::RecallRules::load(&storage)
        .unwrap()
        .permits("请先给结论，回答应简短"));
}

#[test]
fn owner_prepared_after_user_child_completion_does_not_repeat_delivery() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    let owner = g.works[0].id.clone();
    let expert = g.members[1].id.clone();
    let child = g
        .enqueue(
            &i.id,
            &expert,
            "用户先交给专家的补充".into(),
            false,
            Some(owner.clone()),
            1,
        )
        .unwrap();
    g.finish(&child, None, false);
    let work = g.works.iter_mut().find(|w| w.id == owner).unwrap();
    work.status = "running".into();
    work.context_seq = g.messages.last().unwrap().seq;
    work.context_children = vec![child];
    g.finish(&owner, None, false);
    assert_eq!(g.works.len(), 2);
    assert!(g.works.iter().all(Work::terminal));
}

#[test]
fn queued_owner_continuation_waits_for_direct_user_correction_and_receives_its_actual_result() {
    let mut g = group();
    let i = input();
    send(&mut g, &i).unwrap();
    let parent = g.works[0].id.clone();
    let expert = g.members[1].id.clone();
    let child = g
        .enqueue(
            &i.id,
            &expert,
            "前一阶段".into(),
            false,
            Some(parent.clone()),
            1,
        )
        .unwrap();
    g.finish(&parent, None, false);
    g.finish(&child, None, false);
    let callback = g.works.last().unwrap().id.clone();
    let answer = g
        .post(
            Some(expert.clone()),
            "assistant",
            "旧产物".into(),
            Some(child),
        )
        .id
        .clone();
    let mut correction = input();
    correction.reply_to = Some(answer);
    correction.text = "把产物改为带幂等键的接口".into();
    send(&mut g, &correction).unwrap();
    let assigned = g.messages.last().unwrap().work_id.clone().unwrap();
    let blocked = runtime::blocked_work(&g);
    assert!(blocked.contains(&callback));
    assert!(!blocked.contains(&assigned));
    g.post(
        Some(expert),
        "assistant",
        "实际修正产物：POST /approve，包含幂等键".into(),
        Some(assigned.clone()),
    );
    g.works
        .iter_mut()
        .find(|w| w.id == assigned)
        .unwrap()
        .context_seq = g.messages.last().unwrap().seq;
    g.finish(&assigned, None, false);
    assert!(!runtime::blocked_work(&g).contains(&callback));
    let work = g.works.iter().find(|w| w.id == callback).unwrap();
    let prompt = runtime::work_prompt(&g, work, &g.members[0], &Knowledge::default()).unwrap();
    assert!(prompt.contains("实际修正产物：POST /approve，包含幂等键"));
    assert!(prompt.contains("前置失败时先处理阻塞"));
}

#[test]
fn memory_reset_database_failure_leaves_files_and_context_intact() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(&home.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    g.members[0].session_id = Some("previous".into());
    g.post(None, "user", "旧背景".into(), None);
    store.create(&g).unwrap();
    let member = &g.members[0];
    let namespace = home
        .path()
        .join("group-memory")
        .join(&g.id)
        .join(&member.id)
        .join("generation-0");
    std::fs::create_dir_all(&namespace).unwrap();
    let memory = namespace.join("records.json");
    std::fs::write(&memory, "{}").unwrap();
    rusqlite::Connection::open(&store.path).unwrap().execute_batch("CREATE TRIGGER reject_context_change BEFORE UPDATE ON groups BEGIN SELECT RAISE(ABORT,'injected save failure'); END;").unwrap();
    assert!(management::reset_member_memory(&store, home.path(), &g.id, &member.id).is_err());
    assert!(memory.exists());
    assert_eq!(store.get(&g.id).unwrap().members[0], *member);
}

#[cfg(unix)]
#[test]
fn memory_reset_cleanup_failure_is_fenced_and_retryable() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(&home.path().join("groups.sqlite")).unwrap();
    let mut g = group();
    g.members[0].session_id = Some("old-context".into());
    g.post(None, "user", "清除前的背景".into(), None);
    store.create(&g).unwrap();
    let member = &g.members[0];
    let dir = home
        .path()
        .join("group-memory")
        .join(&g.id)
        .join(&member.id);
    let old = dir.join("generation-0");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("records.json"), "{}").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = management::reset_member_memory(&store, home.path(), &g.id, &member.id);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.unwrap_err().contains("已重建上下文"));
    let after = store.get(&g.id).unwrap();
    assert!(after.members[0].generation > member.generation);
    assert!(after.members[0].session_id.is_none());
    assert_eq!(
        after.members[0].history_start_seq,
        g.messages.last().unwrap().seq
    );
    management::reset_member_memory(&store, home.path(), &g.id, &member.id).unwrap();
    assert!(!dir.exists());
}

#[test]
fn permanent_group_deletion_also_removes_only_its_private_migration_backup() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("groups.sqlite");
    let store = Store::open(&path).unwrap();
    let first = group();
    let second = group();
    store.create(&first).unwrap();
    store.create(&second).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM group_migrations", [])
        .unwrap();
    let store = Store::open(&path).unwrap();
    store.delete(&first.id).unwrap();
    let c = rusqlite::Connection::open(&path).unwrap();
    let ids = c
        .prepare("SELECT group_id FROM group_migration_backups")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(ids, vec![second.id]);
    assert!(store.get(&first.id).is_err());
}
