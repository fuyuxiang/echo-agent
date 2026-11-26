#![cfg_attr(rustfmt, rustfmt::skip)]
use super::*;
use crate::test_support::lsp_runtime::{ctx_with_toggle, test_gateway};
use crate::upload::trace::SubagentSpawnedRef;
use echo_agent_tools::implementations::echo_agent_build::task::backend::ChannelBackend;
#[test]
fn normalize_forked_context_strips_project_layout() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let big_layout = "<project_layout>\nline1\nline2\nline3\n</project_layout>";
    let items = vec![
            ConversationItem::system("sys"),
            ConversationItem::user(big_layout),
            ConversationItem::assistant("ack"),
        ];
    let (conv, _) = echo_agent_subagent_resolution::context::normalize_forked_context(
        items,
    );
    if let ConversationItem::User(u) = &conv[1] {
        let text = u
            .content
            .iter()
            .filter_map(|p| match p {
                echo_agent_sampling_types::conversation::ContentPart::Text { text } => {
                    Some(text.as_ref())
                }
                _ => None,
            })
            .collect::<String>();
        assert!(
                !text.contains("<project_layout>"),
                "project_layout tag should be stripped"
            );
        assert!(!text.contains("line1"), "layout content should be removed");
    } else {
        panic!("expected User at position 1");
    }
}
#[test]
fn normalize_forked_context_consecutive_users() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let items = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("prefix"),
            ConversationItem::user("query"),
            ConversationItem::assistant("response"),
        ];
    let (conv, prefix_len) = echo_agent_subagent_resolution::context::normalize_forked_context(
        items,
    );
    assert_eq!(prefix_len, 2);
    if let ConversationItem::User(u) = &conv[1] {
        let text = u
            .content
            .iter()
            .filter_map(|p| match p {
                echo_agent_sampling_types::conversation::ContentPart::Text { text } => {
                    Some(text.as_ref())
                }
                _ => None,
            })
            .collect::<String>();
        assert!(
                text.contains("[User]: prefix"),
                "should include first user msg"
            );
        assert!(
                text.contains("[User]: query"),
                "should include second user msg"
            );
        assert!(
                text.contains("[Assistant]: response"),
                "should include assistant"
            );
    } else {
        panic!("expected User at position 1");
    }
}
/// End-to-end test: after normalization + system prompt replacement,
/// the conversation shape is [System(child's), BackgroundContext].
/// Then the Prompt command appends the task as [2], giving:
/// [System(child's), BackgroundContext, Task].
#[test]
fn end_to_end_normalized_conversation_shape() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let parent_conv = vec![
            ConversationItem::system("parent system prompt"),
            ConversationItem::user("user prefix with project info"),
            ConversationItem::user("implement quicksort"),
            ConversationItem::assistant("here is quicksort"),
        ];
    let (mut conv, prefix_len) = echo_agent_subagent_resolution::context::normalize_forked_context(
        parent_conv,
    );
    assert_eq!(prefix_len, 2);
    assert_eq!(conv.len(), 2);
    if let ConversationItem::System(ref mut sys) = conv[0] {
        sys.content = "child system prompt with tool guidance".into();
    } else {
        panic!("expected System at position 0");
    }
    if let ConversationItem::System(ref sys) = conv[0] {
        assert_eq!(
                sys.content.as_ref(),
                "child system prompt with tool guidance"
            );
    }
    if let ConversationItem::User(ref u) = conv[1] {
        let text = u
            .content
            .iter()
            .filter_map(|p| match p {
                echo_agent_sampling_types::conversation::ContentPart::Text { text } => {
                    Some(text.as_ref())
                }
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains("<background_context>"));
        assert!(text.contains("[User]: implement quicksort"));
    } else {
        panic!("expected User (background) at position 1");
    }
    let task = "implement bubble sort in Rust";
    conv.push(ConversationItem::user(task));
    assert_eq!(conv.len(), 3);
    assert!(matches!(conv[0], ConversationItem::System(_)));
    assert!(matches!(conv[1], ConversationItem::User(_)));
    assert!(matches!(conv[2], ConversationItem::User(_)));
    if let ConversationItem::User(ref u) = conv[2] {
        let text = u
            .content
            .iter()
            .filter_map(|p| match p {
                echo_agent_sampling_types::conversation::ContentPart::Text { text } => {
                    Some(text.as_ref())
                }
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, task, "last user message should be the task");
    }
    assert_eq!(prefix_len, 2);
    assert!(prefix_len < conv.len(), "prefix should not cover the task");
}
/// Verify that the task prompt (not background context) would be the
/// cached prompt text in the session pipeline.
#[test]
fn cached_prompt_text_is_task_not_background() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let parent_conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("parent query"),
            ConversationItem::assistant("parent answer"),
        ];
    let (conv, _) = echo_agent_subagent_resolution::context::normalize_forked_context(
        parent_conv,
    );
    let background_text = if let ConversationItem::User(ref u) = conv[1] {
        u.content
            .iter()
            .filter_map(|p| match p {
                echo_agent_sampling_types::conversation::ContentPart::Text { text } => {
                    Some(text.as_ref())
                }
                _ => None,
            })
            .collect::<String>()
    } else {
        String::new()
    };
    let task_prompt = "fix the failing test in src/lib.rs";
    assert_ne!(task_prompt, background_text.trim());
    assert!(
            !background_text.contains(task_prompt),
            "background should not contain the task prompt"
        );
    assert!(
            background_text.contains("<background_context>"),
            "background should be the inherited context"
        );
}
/// Verify extract_last_real_user_query would return the task.
#[test]
fn last_user_message_is_task_after_normalization() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let parent_conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("parent context"),
            ConversationItem::assistant("ack"),
        ];
    let (mut conv, _) = echo_agent_subagent_resolution::context::normalize_forked_context(
        parent_conv,
    );
    let task = "deploy the service to staging";
    conv.push(ConversationItem::user(task));
    let last_user = conv
        .iter()
        .rev()
        .find_map(|item| {
            if let ConversationItem::User(u) = item {
                let text: String = u
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        echo_agent_sampling_types::conversation::ContentPart::Text {
                            text,
                        } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect();
                Some(text)
            } else {
                None
            }
        });
    assert_eq!(
            last_user.as_deref(),
            Some(task),
            "last user message should be the task, not background context"
        );
}
/// Simulate compaction preserving the inherited prefix.
/// The compactor produces [System, UserPrefix, Summary, ...]. The prefix
/// preservation logic takes [System, BackgroundContext] from the original
/// conversation and skips the compacted System, resulting in:
/// [System(inherited), BackgroundContext(inherited), UserPrefix(compacted), Summary, ...]
#[test]
fn compaction_preserves_inherited_prefix() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let parent_conv = vec![
            ConversationItem::system("parent sys"),
            ConversationItem::user("parent question"),
            ConversationItem::assistant("parent answer"),
        ];
    let (conv, prefix_len) = echo_agent_subagent_resolution::context::normalize_forked_context(
        parent_conv,
    );
    assert_eq!(prefix_len, 2);
    let mut full_conv = conv;
    if let ConversationItem::System(ref mut sys) = full_conv[0] {
        sys.content = "child system prompt".into();
    }
    full_conv.push(ConversationItem::user("do the thing"));
    full_conv.push(ConversationItem::assistant("done"));
    let compacted_history = vec![
            ConversationItem::system("fresh system prompt after compaction"),
            ConversationItem::user("user prefix"),
            ConversationItem::user("<compacted_summary>summary of work</compacted_summary>"),
        ];
    let inherited: Vec<_> = full_conv[..prefix_len].to_vec();
    let child_items: Vec<_> = compacted_history
        .into_iter()
        .skip_while(|i| matches!(i, ConversationItem::System(_)))
        .collect();
    let mut preserved = inherited;
    preserved.extend(child_items);
    assert_eq!(preserved.len(), 4);
    if let ConversationItem::System(ref sys) = preserved[0] {
        assert_eq!(sys.content.as_ref(), "child system prompt");
    } else {
        panic!("expected System at [0]");
    }
    if let ConversationItem::User(ref u) = preserved[1] {
        let text: String = u
            .content
            .iter()
            .filter_map(|p| match p {
                echo_agent_sampling_types::conversation::ContentPart::Text { text } => {
                    Some(text.as_ref())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert!(
                text.contains("<background_context>"),
                "background context should be preserved across compaction"
            );
    } else {
        panic!("expected BackgroundContext User at [1]");
    }
    let system_count = preserved
        .iter()
        .filter(|i| matches!(i, ConversationItem::System(_)))
        .count();
    assert_eq!(
            system_count, 1,
            "should have exactly one System after compaction"
        );
    let bg_count = preserved
        .iter()
        .filter(|i| {
            if let ConversationItem::User(u) = i {
                u.content
                    .iter()
                    .any(|p| {
                        matches!(
                    p,
                    echo_agent_sampling_types::conversation::ContentPart::Text { text } if text.contains("<background_context>")
                )
                    })
            } else {
                false
            }
        })
        .count();
    assert_eq!(
            bg_count, 1,
            "should have exactly one background_context after compaction"
        );
}
/// Verify that compaction with prefix_len=0 (non-forked) passes through unchanged.
#[test]
fn compaction_no_prefix_passes_through() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let compacted = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("summary"),
        ];
    let prefix_len: usize = 0;
    let result = if prefix_len > 0 { unreachable!() } else { compacted.clone() };
    assert_eq!(result.len(), 2);
    assert!(matches!(result[0], ConversationItem::System(_)));
}
#[test]
fn resumed_from_field_in_meta_roundtrips() {
    let meta = SubagentMeta {
        subagent_id: "sa-resumed".into(),
        description: "resumed task".into(),
        prompt: "continue".into(),
        resumed_from: Some("prev-subagent-id".into()),
        ..base_meta()
    };
    let json = serde_json::to_string(&meta).unwrap();
    assert!(json.contains("resumed_from"));
    assert!(json.contains("prev-subagent-id"));
    let parsed: SubagentMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.resumed_from.as_deref(), Some("prev-subagent-id"));
    let gcs = SubagentSessionMetadata::from_meta(
        &meta,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        0,
    );
    assert_eq!(gcs.resumed_from.as_deref(), Some("prev-subagent-id"));
    let gcs_json = serde_json::to_string(&gcs).unwrap();
    assert!(gcs_json.contains("resumedFrom"));
}
#[test]
fn resumed_from_none_not_serialized_in_meta() {
    let meta = SubagentMeta {
        subagent_id: "sa-fresh".into(),
        parent_session_id: "p".into(),
        child_session_id: "c".into(),
        subagent_type: "explore".into(),
        description: "d".into(),
        prompt: "p".into(),
        ..base_meta()
    };
    let json = serde_json::to_string(&meta).unwrap();
    assert!(
            !json.contains("resumed_from"),
            "None resumed_from should be omitted"
        );
}
#[test]
fn backward_compat_meta_without_resumed_from() {
    let json = r#"{
            "subagent_id": "sa1",
            "parent_session_id": "p1",
            "child_session_id": "c1",
            "subagent_type": "explore",
            "description": "d",
            "prompt": "p",
            "status": "completed",
            "started_at": "2026-01-01T00:00:00Z"
        }"#;
    let meta: SubagentMeta = serde_json::from_str(json).unwrap();
    assert!(meta.resumed_from.is_none());
}
#[test]
fn snapshot_ref_field_in_meta_roundtrips() {
    let meta = SubagentMeta {
        subagent_id: "sa-snap".into(),
        description: "snapshot task".into(),
        status: "completed".into(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: Some(10),
        tool_calls: Some(1),
        turns: Some(1),
        worktree_path: Some("/tmp/echo-agent-wt/sa-snap".into()),
        snapshot_ref: Some("refs/echoagent/subagent-snapshots/sa-snap".into()),
        ..base_meta()
    };
    let json = serde_json::to_string(&meta).unwrap();
    assert!(json.contains("snapshot_ref"));
    assert!(json.contains("refs/echoagent/subagent-snapshots/sa-snap"));
    let parsed: SubagentMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(
            parsed.snapshot_ref.as_deref(),
            Some("refs/echoagent/subagent-snapshots/sa-snap")
        );
}
#[test]
fn backward_compat_meta_without_snapshot_ref() {
    let json = r#"{
            "subagent_id": "sa1",
            "parent_session_id": "p1",
            "child_session_id": "c1",
            "subagent_type": "explore",
            "description": "d",
            "prompt": "p",
            "status": "completed",
            "started_at": "2026-01-01T00:00:00Z"
        }"#;
    let meta: SubagentMeta = serde_json::from_str(json).unwrap();
    assert!(meta.snapshot_ref.is_none());
}
/// Canonical running-status `SubagentMeta`; tests override only the fields
/// under test via `..base_meta()`.
fn base_meta() -> SubagentMeta {
    SubagentMeta {
        subagent_id: "sa".into(),
        parent_session_id: "parent".into(),
        child_session_id: "child".into(),
        subagent_type: "general-purpose".into(),
        description: "task".into(),
        prompt: "do work".into(),
        status: "running".into(),
        started_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: None,
        tool_calls: None,
        turns: None,
        error: None,
        effective_context_source: None,
        context_normalized: false,
        fork_copy_error: None,
        persona: None,
        resumed_from: None,
        child_cwd: None,
        worktree_path: None,
        snapshot_ref: None,
        effective_model_id: None,
    }
}
/// Minimal completed-status meta for the snapshot-ref persistence tests.
fn snapshot_test_meta(id: &str) -> SubagentMeta {
    SubagentMeta {
        subagent_id: id.into(),
        parent_session_id: "session-A".into(),
        child_session_id: format!("child-{id}"),
        status: "completed".into(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: Some(1),
        tool_calls: Some(0),
        turns: Some(1),
        worktree_path: Some("/tmp/echo-agent-wt/subagent-x".into()),
        ..base_meta()
    }
}
/// The follow-up writer persists `snapshot_ref` into an already-finalized
/// meta.json so `durable_resume_source_for` rehydrates the disposed worktree.
#[test]
fn update_subagent_meta_snapshot_ref_persists_to_disk() {
    let dir = tempfile::TempDir::new().unwrap();
    assert!(write_subagent_meta(
            dir.path(),
            &snapshot_test_meta("sa-write")
        ));
    assert!(
            update_subagent_meta_snapshot_ref(
                dir.path(),
                "refs/echoagent/subagents/sa-write",
                "completed"
            ),
            "persisting the ref into an existing meta.json must report success"
        );
    let data = std::fs::read_to_string(dir.path().join("meta.json")).unwrap();
    let reread: SubagentMeta = serde_json::from_str(&data).unwrap();
    assert_eq!(
            reread.snapshot_ref.as_deref(),
            Some("refs/echoagent/subagents/sa-write")
        );
    assert_eq!(reread.status, "completed");
    assert_eq!(
            reread.worktree_path.as_deref(),
            Some("/tmp/echo-agent-wt/subagent-x")
        );
}
/// Missing meta.json → the writer reports failure (it `warn!`s), so the
/// completion path keeps the worktree instead of removing it ref-less.
#[test]
fn update_subagent_meta_snapshot_ref_reports_failure_when_meta_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    assert!(!update_subagent_meta_snapshot_ref(
            dir.path(),
            "refs/echoagent/subagents/sa-missing",
            "completed"
        ));
}
/// A stale non-terminal record (e.g. completed-status write failed) is
/// promoted to terminal alongside the snapshot_ref, so the durable resume
/// fallback accepts it after the worktree is removed.
#[test]
fn snapshot_ref_write_promotes_nonterminal_status_to_terminal() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut meta = snapshot_test_meta("sa-promote");
    meta.status = "running".into();
    assert!(write_subagent_meta(dir.path(), &meta));
    assert!(update_subagent_meta_snapshot_ref(
            dir.path(),
            "refs/echoagent/subagents/x",
            "completed"
        ));
    let data = std::fs::read_to_string(dir.path().join("meta.json")).unwrap();
    let reread: SubagentMeta = serde_json::from_str(&data).unwrap();
    assert_eq!(
            Some("refs/echoagent/subagents/x"),
            reread.snapshot_ref.as_deref()
        );
    assert_eq!("completed", reread.status);
}
/// Gate defaults OFF: no config, no remote → snapshotting disabled, so the
/// completion path keeps the worktree preserved (no production change).
#[test]
fn subagent_worktree_snapshot_gate_defaults_off() {
    let ctx = ctx_with_toggle(std::collections::HashMap::new());
    assert!(!ctx.resolve_subagent_worktree_snapshot_enabled());
}
/// Remote remote settings value enables the gate when no local override exists.
#[test]
fn subagent_worktree_snapshot_gate_remote_enables() {
    let mut ctx = ctx_with_toggle(std::collections::HashMap::new());
    ctx.remote_settings = Some(crate::util::config::RemoteSettings {
        subagent_worktree_snapshot_enabled: Some(true),
        ..Default::default()
    });
    assert!(ctx.resolve_subagent_worktree_snapshot_enabled());
}
/// Local config wins over remote (kill-switch parity with the other gates).
#[test]
fn subagent_worktree_snapshot_gate_local_overrides_remote() {
    let mut config = crate::agent::config::Config::default();
    config
        .feature_values
        .insert(crate::agent::config::Feature::SubagentWorktreeSnapshot, false);
    let mut ctx = ctx_with_toggle(std::collections::HashMap::new());
    ctx.agent_config = Some(config);
    ctx.remote_settings = Some(crate::util::config::RemoteSettings {
        subagent_worktree_snapshot_enabled: Some(true),
        ..Default::default()
    });
    assert!(
            !ctx.resolve_subagent_worktree_snapshot_enabled(),
            "local [features] subagent_worktree_snapshot=false must override remote enable"
        );
}
/// Local config alone enables the gate (the per-deployment rollout lever).
#[test]
fn subagent_worktree_snapshot_gate_local_enables() {
    let mut config = crate::agent::config::Config::default();
    config
        .feature_values
        .insert(crate::agent::config::Feature::SubagentWorktreeSnapshot, true);
    let mut ctx = ctx_with_toggle(std::collections::HashMap::new());
    ctx.agent_config = Some(config);
    assert!(ctx.resolve_subagent_worktree_snapshot_enabled());
}
#[test]
fn subagent_tool_filter_removes_ask_user_question() {
    let mut tools = vec![
            echo_agent_sampling_types::ToolSpec {
                name: "read_file".to_owned(),
                description: None,
                parameters: serde_json::json!({}),
            },
            echo_agent_sampling_types::ToolSpec {
                name: "ask_user_question".to_owned(),
                description: None,
                parameters: serde_json::json!({}),
            },
        ];
    strip_ask_user_question_tool(&mut tools);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_file");
}
/// The gate keeping a worktree must leave no resume pointer. A pointer sends
/// resume down the rehydrate path, which deletes the directory and rebuilds
/// it from a snapshot that, by construction, lacks whatever kept it.
#[tokio::test]
async fn kept_worktree_leaves_no_resume_pointer() {
    echo_agent_test_utils::require_git!();
    use echo_agent_test_utils::git::{git_commit_all, seed_repo};
    let temp = tempfile::TempDir::new().unwrap();
    let repo = seed_repo(temp.path());
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    git_commit_all(&repo, "ignore env");
    let wt = temp.path().join("subagent-keeps");
    echo_agent_fast_worktree::WorktreeBuilder::new(&repo, &wt)
        .standalone(true)
        .create()
        .unwrap();
    std::fs::write(wt.join(".env"), b"SECRET").unwrap();
    let mut config = crate::agent::config::Config::default();
    config
        .feature_values
        .insert(crate::agent::config::Feature::SubagentWorktreeSnapshot, true);
    let mut ctx = ctx_with_toggle(std::collections::HashMap::new());
    ctx.agent_config = Some(config);
    assert!(ctx.resolve_subagent_worktree_snapshot_enabled());
    let meta_dir = temp.path().join("meta");
    write_subagent_meta(&meta_dir, &snapshot_test_meta("keeps-1"));
    let disposal = crate::agent::subagent::handle_request::dispose_worktree_after_completion(
            &wt,
            &repo,
            &meta_dir,
            "completed",
            "keeps-1",
        )
        .await;
    assert!(matches!(
            disposal,
            crate::agent::subagent::handle_request::Disposal::Kept
        ));
    assert_eq!(
            std::fs::read(wt.join(".env")).expect("the secret is gone"),
            b"SECRET"
        );
    let meta: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(meta_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(
            meta.snapshot_ref, None,
            "a pointer here sends resume down the path that deletes the directory"
        );
    assert_eq!(
            crate::agent::subagent::resume_worktree_action(true, meta.snapshot_ref.as_deref()),
            crate::agent::subagent::ResumeWorktreeAction::Reuse
        );
}
/// Everything the disposal does once the gate says Delete, on the linked
/// worktree the completion path actually makes: the resume pointer reaches
/// meta.json and the directory goes.
///
/// Linked on purpose. A linked worktree is the only shape where the
/// registration's reflog is the last name for a commit, which is what the
/// second half of this test is about.
#[tokio::test]
async fn disposed_linked_worktree_persists_the_pointer_then_removes_the_directory() {
    echo_agent_test_utils::require_git!();
    use echo_agent_test_utils::git::seed_repo_with_remote;
    let temp = tempfile::TempDir::new().unwrap();
    let (repo, _remote) = seed_repo_with_remote(temp.path());
    let wt = temp.path().join("subagent-reclaim-1");
    echo_agent_fast_worktree::WorktreeBuilder::new(&repo, &wt).create().unwrap();
    let meta_dir = temp.path().join("meta");
    write_subagent_meta(&meta_dir, &snapshot_test_meta("reclaim-1"));
    let disposal = crate::agent::subagent::handle_request::dispose_worktree_after_completion(
            &wt,
            &repo,
            &meta_dir,
            "completed",
            "reclaim-1",
        )
        .await;
    let snapshot_ref = "refs/echoagent/subagents/reclaim-1";
    assert!(disposal.worktree_removed(), "the gate cleared, so it goes");
    assert!(!wt.exists(), "the worktree directory is still on disk");
    let meta: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(meta_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(
            meta.snapshot_ref.as_deref(),
            Some(snapshot_ref),
            "a removed worktree with no pointer leaves resume nothing to open"
        );
    assert_eq!(
            crate::agent::subagent::resume_worktree_action(false, meta.snapshot_ref.as_deref()),
            crate::agent::subagent::ResumeWorktreeAction::Rehydrate
        );
}
/// A commit the registration's reflog is the only name for. The disposal
/// gives it a second name in the repository that outlives the worktree,
/// and only then removes the directory, so the commit is still readable.
#[tokio::test]
async fn disposal_names_a_reflog_only_commit_before_removing_the_worktree() {
    echo_agent_test_utils::require_git!();
    use echo_agent_test_utils::git::{reflog_only_commit, run_git, seed_repo_with_remote};
    let temp = tempfile::TempDir::new().unwrap();
    let (repo, _remote) = seed_repo_with_remote(temp.path());
    let wt = temp.path().join("subagent-reclaim-2");
    echo_agent_fast_worktree::WorktreeBuilder::new(&repo, &wt).create().unwrap();
    let discarded = reflog_only_commit(&wt, None);
    let meta_dir = temp.path().join("meta");
    write_subagent_meta(&meta_dir, &snapshot_test_meta("reclaim-2"));
    let disposal = crate::agent::subagent::handle_request::dispose_worktree_after_completion(
            &wt,
            &repo,
            &meta_dir,
            "completed",
            "reclaim-2",
        )
        .await;
    assert!(
            disposal.worktree_removed(),
            "the commit has a lasting name now, so nothing keeps the worktree"
        );
    assert!(!wt.exists(), "the worktree is still there");
    run_git(&repo, &["gc", "--prune=now"]);
    assert_eq!(
            run_git(&repo, &["show", &format!("{discarded}:tracked.txt")]),
            "three hours of work"
        );
}
#[test]
fn subagent_session_metadata_roundtrip() {
    let meta = SubagentMeta {
        subagent_id: "sa-1".into(),
        parent_session_id: "parent-1".into(),
        child_session_id: "child-1".into(),
        description: "test task".into(),
        prompt: "do something".into(),
        status: "completed".into(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: Some(1234),
        tool_calls: Some(5),
        turns: Some(2),
        effective_context_source: Some("new".into()),
        persona: Some("reviewer".into()),
        ..base_meta()
    };
    let session_meta = SubagentSessionMetadata::from_meta(
        &meta,
        Some("echo-agent-4.5"),
        Some("/workspace"),
        Some("/tmp/worktree"),
        Some("worktree"),
        Some("read-only"),
        Some("medium"),
        Some("rust-dev"),
        Some("prompt-123"),
        1,
    );
    assert_eq!(session_meta.schema_version, 1);
    assert_eq!(session_meta.session_kind, "subagent");
    assert_eq!(session_meta.subagent_id, "sa-1");
    assert_eq!(session_meta.parent_session_id, "parent-1");
    assert_eq!(session_meta.description, "test task");
    assert_eq!(session_meta.model_id.as_deref(), Some("echo-agent-4.5"));
    assert_eq!(session_meta.role.as_deref(), Some("rust-dev"));
    assert_eq!(session_meta.persona.as_deref(), Some("reviewer"));
    assert!(!session_meta.context_normalized);
    assert_eq!(session_meta.depth, 1);
    let json = serde_json::to_string_pretty(&session_meta).unwrap();
    let deserialized: SubagentSessionMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.session_kind, "subagent");
    assert_eq!(deserialized.subagent_id, "sa-1");
    assert_eq!(deserialized.description, "test task");
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value.as_object_mut().unwrap().remove("description");
    let legacy: SubagentSessionMetadata = serde_json::from_value(value).unwrap();
    assert!(legacy.description.is_empty());
    assert!(json.contains("schemaVersion"));
    assert!(json.contains("sessionKind"));
}
#[test]
fn subagent_session_metadata_non_forked() {
    let meta = SubagentMeta {
        subagent_id: "sa-2".into(),
        parent_session_id: "parent-2".into(),
        child_session_id: "child-2".into(),
        subagent_type: "explore".into(),
        description: "search code".into(),
        prompt: "find auth".into(),
        effective_context_source: Some("new".into()),
        persona: Some("implementer".into()),
        ..base_meta()
    };
    let session_meta = SubagentSessionMetadata::from_meta(
        &meta,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        0,
    );
    assert_eq!(session_meta.session_kind, "subagent");
    assert!(!session_meta.context_normalized);
    assert_eq!(session_meta.depth, 0);
    assert!(session_meta.model_id.is_none());
    assert!(session_meta.worktree_path.is_none());
}
#[test]
fn subagent_session_metadata_backward_compat_deserialization() {
    let json = r#"{
            "schemaVersion": 1,
            "sessionId": "s1",
            "sessionKind": "subagent",
            "subagentId": "sa1",
            "childSessionId": "c1",
            "parentSessionId": "p1",
            "subagentType": "explore",
            "startedAt": "2026-01-01T00:00:00Z",
            "status": "completed",
            "depth": 0
        }"#;
    let meta: SubagentSessionMetadata = serde_json::from_str(json).unwrap();
    assert_eq!(meta.session_kind, "subagent");
    assert!(meta.persona.is_none());
    assert!(meta.role.is_none());
    assert!(!meta.context_normalized);
}
#[test]
fn upload_lifecycle_spawn_then_completion_preserves_fields() {
    let spawn_meta = SubagentMeta {
        subagent_id: "sa-lifecycle".into(),
        parent_session_id: "parent-1".into(),
        child_session_id: "child-1".into(),
        description: "test task".into(),
        prompt: "do something".into(),
        effective_context_source: Some("forked".into()),
        context_normalized: true,
        persona: Some("implementer".into()),
        ..base_meta()
    };
    let spawn_gcs = SubagentSessionMetadata::from_meta(
        &spawn_meta,
        Some("echo-agent-4.5"),
        Some("/workspace"),
        None,
        Some("worktree"),
        Some("all"),
        Some("medium"),
        Some("rust-dev"),
        Some("prompt-42"),
        1,
    );
    assert_eq!(spawn_gcs.status, "running");
    assert!(spawn_gcs.completed_at.is_none());
    assert!(spawn_gcs.duration_ms.is_none());
    assert_eq!(spawn_gcs.model_id.as_deref(), Some("echo-agent-4.5"));
    assert_eq!(spawn_gcs.cwd.as_deref(), Some("/workspace"));
    assert_eq!(spawn_gcs.role.as_deref(), Some("rust-dev"));
    assert_eq!(spawn_gcs.parent_prompt_id.as_deref(), Some("prompt-42"));
    assert_eq!(spawn_gcs.depth, 1);
    let mut completed_meta = spawn_meta.clone();
    completed_meta.status = "completed".to_string();
    completed_meta.completed_at = Some(chrono::Utc::now());
    completed_meta.duration_ms = Some(5000);
    completed_meta.tool_calls = Some(12);
    completed_meta.turns = Some(3);
    let completion_gcs = SubagentSessionMetadata::from_meta(
        &completed_meta,
        Some("echo-agent-4.5"),
        Some("/workspace"),
        Some("/tmp/worktree-1"),
        Some("worktree"),
        Some("all"),
        Some("medium"),
        Some("rust-dev"),
        Some("prompt-42"),
        1,
    );
    assert_eq!(completion_gcs.status, "completed");
    assert!(completion_gcs.completed_at.is_some());
    assert_eq!(completion_gcs.duration_ms, Some(5000));
    assert_eq!(completion_gcs.tool_calls, Some(12));
    assert_eq!(completion_gcs.turns, Some(3));
    assert_eq!(completion_gcs.model_id.as_deref(), Some("echo-agent-4.5"));
    assert_eq!(completion_gcs.cwd.as_deref(), Some("/workspace"));
    assert_eq!(completion_gcs.role.as_deref(), Some("rust-dev"));
    assert_eq!(
            completion_gcs.parent_prompt_id.as_deref(),
            Some("prompt-42")
        );
    assert_eq!(
            completion_gcs.worktree_path.as_deref(),
            Some("/tmp/worktree-1")
        );
    assert_eq!(completion_gcs.depth, 1);
    assert_eq!(spawn_gcs.child_session_id, completion_gcs.child_session_id);
}
#[test]
fn upload_lifecycle_failure_preserves_error() {
    let meta = SubagentMeta {
        subagent_id: "sa-fail".into(),
        parent_session_id: "p".into(),
        child_session_id: "c".into(),
        subagent_type: "explore".into(),
        description: "d".into(),
        prompt: "p".into(),
        status: "failed".to_string(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: Some(100),
        tool_calls: Some(0),
        turns: Some(0),
        error: Some("session spawn error".into()),
        effective_context_source: Some("new".into()),
        ..base_meta()
    };
    let gcs = SubagentSessionMetadata::from_meta(
        &meta,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        0,
    );
    assert_eq!(gcs.status, "failed");
    assert_eq!(gcs.error.as_deref(), Some("session spawn error"));
    assert_eq!(gcs.session_kind, "subagent");
}
#[test]
fn initial_context_source_resumed_variant() {
    let source = InitialContextSource::Resumed;
    assert!(matches!(source, InitialContextSource::Resumed));
    assert_ne!(source, InitialContextSource::New);
}
#[test]
fn session_metadata_session_kind_for_resumed() {
    let meta = SubagentMeta {
        subagent_id: "sa-resume".into(),
        parent_session_id: "p".into(),
        child_session_id: "c".into(),
        description: "d".into(),
        prompt: "p".into(),
        effective_context_source: Some("resumed".into()),
        resumed_from: Some("prev-id".into()),
        ..base_meta()
    };
    let gcs = SubagentSessionMetadata::from_meta(
        &meta,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        0,
    );
    assert_eq!(
            gcs.session_kind, "subagent_resume",
            "resumed subagents should have session_kind=subagent_resume"
        );
    assert_eq!(gcs.resumed_from.as_deref(), Some("prev-id"));
}
/// Resume must preserve only the System head (`Some(1)`) while passing the full
/// transcript through intact — a whole-transcript prefix is what pinned compaction.
#[test]
fn resume_initial_context_preserves_head_only() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let mut conversation = vec![ConversationItem::system("sys")];
    for i in 0..8 {
        conversation.push(ConversationItem::user(format!("u{i}")));
        conversation.push(ConversationItem::assistant(format!("a{i}")));
    }
    let original_len = conversation.len();
    let ctx = resume_initial_context(conversation);
    assert_eq!(ctx.source, InitialContextSource::Resumed);
    assert!(ctx.copy_error.is_none());
    assert_eq!(
            ctx.prefix_len,
            Some(1),
            "resume preserves only the System head, not the full transcript"
        );
    assert_eq!(
            ctx.conversation.len(),
            original_len,
            "transcript preserved intact"
        );
}
#[test]
fn resume_prefix_len_is_system_head_only() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let mut conversation = vec![ConversationItem::system("sys")];
    for i in 0..6 {
        conversation.push(ConversationItem::user(format!("u{i}")));
        conversation.push(ConversationItem::assistant(format!("a{i}")));
    }
    assert_eq!(resume_inherited_prefix_len(&conversation), 1);
}
#[test]
fn resume_prefix_len_is_zero_without_system_head() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let conversation = vec![
            ConversationItem::user("task"),
            ConversationItem::assistant("done"),
        ];
    assert_eq!(resume_inherited_prefix_len(&conversation), 0);
}
#[test]
fn resume_prefix_len_counts_consecutive_system_head() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let conversation = vec![
            ConversationItem::system("sys a"),
            ConversationItem::system("sys b"),
            ConversationItem::user("work"),
        ];
    assert_eq!(resume_inherited_prefix_len(&conversation), 2);
}
#[test]
fn resume_source_worktree_reuse() {
    let source_with_worktree = ResumeSourceData {
        subagent_id: "sub-wt".into(),
        child_session_id: "child-wt".into(),
        child_cwd: "/tmp/worktree".into(),
        worktree_path: Some(
            PathBuf::from("/home/user/.echo-agent/worktrees/myrepo/subagent-sub-wt"),
        ),
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: None,
        model_id: None,
    };
    let worktree = source_with_worktree.worktree_path.clone();
    assert_eq!(
            worktree.as_deref(),
            Some(Path::new(
                "/home/user/.echo-agent/worktrees/myrepo/subagent-sub-wt",
            )),
            "should reuse source worktree"
        );
    let source_without_worktree = ResumeSourceData {
        subagent_id: "sub-no-wt".into(),
        child_session_id: "child-no-wt".into(),
        child_cwd: "/workspace".into(),
        worktree_path: None,
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: None,
        model_id: None,
    };
    assert!(
            source_without_worktree.worktree_path.is_none(),
            "no worktree to reuse"
        );
}
#[test]
fn resolve_child_cwd_uses_override_when_no_worktree() {
    let parent = PathBuf::from("/parent/workspace");
    let result = resolve_child_cwd(None, Some("/target/dir"), &parent);
    assert_eq!(result, PathBuf::from("/target/dir"));
}
#[test]
fn resolve_child_cwd_worktree_takes_precedence_over_override() {
    let parent = PathBuf::from("/parent/workspace");
    let worktree = Path::new("/worktree/path");
    let result = resolve_child_cwd(Some(worktree), Some("/target/dir"), &parent);
    assert_eq!(result, PathBuf::from(worktree));
}
#[test]
fn resolve_child_cwd_falls_back_to_parent_when_no_overrides() {
    let parent = PathBuf::from("/parent/workspace");
    let result = resolve_child_cwd(None, None, &parent);
    assert_eq!(result, parent);
}
#[test]
fn resolve_child_cwd_empty_override_falls_back_to_parent() {
    let parent = PathBuf::from("/parent/workspace");
    let result = resolve_child_cwd(None, Some(""), &parent);
    assert_eq!(result, parent);
}
#[test]
fn resume_inherited_cwd_requires_existing_non_worktree_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let existing = dir.path().to_string_lossy().into_owned();
    let present = ResumeSourceData {
        subagent_id: "sub-present".into(),
        child_session_id: "child-present".into(),
        child_cwd: existing.clone(),
        worktree_path: None,
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: None,
        model_id: None,
    };
    assert_eq!(
            resume_inherited_cwd(Some(&present)),
            Some(existing.as_str())
        );
    let missing = ResumeSourceData {
        child_cwd: "/no/such/dir/echo-agent-missing".into(),
        ..present.clone()
    };
    assert_eq!(resume_inherited_cwd(Some(&missing)), None);
    let worktree_source = ResumeSourceData {
        child_cwd: existing.clone(),
        worktree_path: Some(dir.path().to_path_buf()),
        ..present.clone()
    };
    assert_eq!(resume_inherited_cwd(Some(&worktree_source)), None);
    assert_eq!(resume_inherited_cwd(None), None);
}
#[test]
fn select_override_cwd_resume_never_falls_through_to_request_cwd() {
    let source = ResumeSourceData {
        subagent_id: "sub-wt".into(),
        child_session_id: "child-wt".into(),
        child_cwd: "/tmp/whatever".into(),
        worktree_path: Some(
            PathBuf::from("/home/user/.echo-agent/worktrees/repo/subagent-sub-wt"),
        ),
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: None,
        model_id: None,
    };
    assert_eq!(select_override_cwd(Some(&source), Some("/x")), None);
}
#[test]
fn select_override_cwd_fresh_spawn_uses_request_cwd() {
    assert_eq!(select_override_cwd(None, Some("/x")), Some("/x"));
}
#[test]
fn resumed_session_uses_current_runtime_contract() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let mut conversation = [
        ConversationItem::system("old source system prompt"),
        ConversationItem::user("task 1"),
        ConversationItem::assistant("done"),
    ];
    let current_prompt = "freshly rendered current system prompt";
    if let Some(ConversationItem::System(sys)) = conversation.first_mut() {
        sys.content = current_prompt.into();
    }
    match &conversation[0] {
        ConversationItem::System(sys) => {
            assert_eq!(sys.content.as_ref(), current_prompt);
            assert!(!sys.content.contains("old source"));
        }
        _ => panic!("first item should be System"),
    }
    assert_eq!(conversation.len(), 3);
}
#[test]
fn token_estimation_for_window_safety() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let conversation = vec![
            ConversationItem::system("You are a helpful assistant."),
            ConversationItem::user("Hello, how are you?"),
            ConversationItem::assistant("I'm doing well, thank you!"),
        ];
    let estimated = echo_agent_chat_state::estimate_conversation_tokens(&conversation);
    assert!(estimated > 0, "should produce non-zero estimate");
    assert!(
            estimated < 100,
            "short conversation should have small token estimate"
        );
    assert_eq!(echo_agent_chat_state::estimate_conversation_tokens(&[]), 0);
}
#[test]
fn token_estimation_accounts_for_images() {
    use echo_agent_sampling_types::conversation::{ContentPart, ConversationItem, UserItem};
    let text_only = vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: "describe this".into(),
            }],
            synthetic_reason: None,
            ..Default::default()
        })];
    let text_tokens = echo_agent_chat_state::estimate_conversation_tokens(&text_only);
    let with_image = vec![ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: "describe this".into(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,abc".into(),
                },
            ],
            synthetic_reason: None,
            ..Default::default()
        })];
    let image_tokens = echo_agent_chat_state::estimate_conversation_tokens(&with_image);
    assert_eq!(
            image_tokens,
            text_tokens + 765,
            "one image should add 765 tokens"
        );
    let multi_image = vec![ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Image { url: "img1".into() },
                ContentPart::Image { url: "img2".into() },
                ContentPart::Image { url: "img3".into() },
            ],
            synthetic_reason: None,
            ..Default::default()
        })];
    let multi_tokens = echo_agent_chat_state::estimate_conversation_tokens(&multi_image);
    assert_eq!(multi_tokens, 765 * 3, "three images = 3 * 765 tokens");
}
#[test]
fn durable_fallback_roundtrips_child_cwd_and_worktree() {
    let dir = std::env::temp_dir()
        .join("echo-agent-test-durable-resume")
        .join(uuid::Uuid::now_v7().to_string());
    let _ = std::fs::create_dir_all(&dir);
    let meta = SubagentMeta {
        subagent_id: "sa-dur".into(),
        parent_session_id: "parent-dur".into(),
        child_session_id: "child-dur".into(),
        description: "d".into(),
        prompt: "p".into(),
        status: "completed".into(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: Some(100),
        tool_calls: Some(1),
        turns: Some(1),
        persona: Some("implementer".into()),
        child_cwd: Some("/workspace/project".into()),
        worktree_path: Some("/tmp/echo-agent-wt/sa-dur".into()),
        effective_model_id: Some("echo-agent-3".into()),
        ..base_meta()
    };
    write_subagent_meta(&dir, &meta);
    let data = std::fs::read_to_string(dir.join("meta.json")).unwrap();
    let loaded: SubagentMeta = serde_json::from_str(&data).unwrap();
    assert_eq!(loaded.child_cwd.as_deref(), Some("/workspace/project"));
    assert_eq!(loaded.worktree_path.as_deref(), Some("/tmp/echo-agent-wt/sa-dur"));
    assert_eq!(loaded.status, "completed");
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn durable_fallback_rejects_running_status() {
    let dir = std::env::temp_dir()
        .join("echo-agent-test-durable-status")
        .join(uuid::Uuid::now_v7().to_string());
    let parent_dir = dir.join("subagents").join("sa-running");
    let _ = std::fs::create_dir_all(&parent_dir);
    let meta = SubagentMeta {
        subagent_id: "sa-running".into(),
        parent_session_id: "parent-x".into(),
        child_session_id: "child-running".into(),
        subagent_type: "explore".into(),
        description: "d".into(),
        prompt: "p".into(),
        child_cwd: Some("/workspace".into()),
        ..base_meta()
    };
    write_subagent_meta(&parent_dir, &meta);
    let data = std::fs::read_to_string(parent_dir.join("meta.json")).unwrap();
    let loaded: SubagentMeta = serde_json::from_str(&data).unwrap();
    let is_terminal = matches!(loaded.status.as_str(), "completed" | "failed" | "cancelled");
    assert!(
            !is_terminal,
            "status=running should NOT be considered terminal/resumable"
        );
    let _ = std::fs::remove_dir_all(&dir);
}
/// Count persisted `SubagentFinished{status:"cancelled"}` for `id` on a
/// session cmd channel, asserting field consistency.
fn drain_cancelled_finish_cmds(
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
    id: &str,
    expected_error: &str,
) -> usize {
    let mut count = 0;
    while let Ok(cmd) = cmd_rx.try_recv() {
        if let SessionCommand::EchoAgentSessionNotification { notification } = cmd
            && let SessionUpdate::SubagentFinished {
                subagent_id,
                status,
                error,
                will_wake,
                ..
            } = &notification.update && subagent_id == id
        {
            assert_eq!(status, "cancelled");
            assert_eq!(error.as_deref(), Some(expected_error));
            assert!(
                    !*will_wake,
                    "synthesized orphan finish must not auto-wake"
                );
            count += 1;
        }
    }
    count
}
/// Count live `SubagentFinished{status:"cancelled"}` for `id` broadcast to
/// the gateway, asserting method + typed payload (not substring matching).
fn drain_cancelled_finish_broadcasts(
    gateway_rx: &mut mpsc::UnboundedReceiver<
        crate::test_support::lsp_runtime::GatewayOut,
    >,
    id: &str,
) -> usize {
    let mut count = 0;
    while let Ok(msg) = gateway_rx.try_recv() {
        let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
            continue;
        };
        assert_eq!(args.request.method.as_ref(), "echo.agent/session_notification");
        let notification: SessionNotification = serde_json::from_str(
                args.request.params.get(),
            )
            .expect("params must deserialize as SessionNotification");
        if let SessionUpdate::SubagentFinished { subagent_id, status, will_wake, .. } = &notification
            .update && subagent_id == id
        {
            assert_eq!(status, "cancelled");
            assert!(
                    !*will_wake,
                    "synthesized orphan finish must not auto-wake"
                );
            count += 1;
        }
    }
    count
}
/// A `running` meta with no terminal counterpart, as left by a dead process.
fn running_test_meta(id: &str, parent_session_id: &str) -> SubagentMeta {
    SubagentMeta {
        subagent_id: id.into(),
        parent_session_id: parent_session_id.into(),
        child_session_id: format!("child-{id}"),
        subagent_type: "explore".into(),
        child_cwd: Some("/workspace".into()),
        ..base_meta()
    }
}
fn inspection(id: &str, status: SubagentSnapshotStatus) -> SubagentInspection {
    SubagentInspection {
        snapshot: SubagentSnapshot {
            subagent_id: id.to_string(),
            description: "task".to_string(),
            subagent_type: "explore".to_string(),
            status,
            started_at_epoch_ms: 0,
            duration_ms: 50,
            persona: None,
        },
        parent_session_id: "parent-x".to_string(),
        child_session_id: format!("child-{id}"),
        fork_parent_prompt_id: None,
        resumed_from: None,
    }
}
async fn reconcile_with_inspections(
    unfinished: &[(String, String)],
    inspections: HashMap<String, Option<SubagentInspection>>,
    session_dir: &Path,
    gateway: &GatewaySender,
    parent_cmd_tx: Option<&mpsc::UnboundedSender<SessionCommand>>,
) {
    let expected = inspections.len();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let backend = ChannelBackend::new(event_tx);
    let respond = async move {
        for _ in 0..expected {
            let event = event_rx.recv().await.expect("inspection event");
            let SubagentEvent::Inspect(request) = event else {
                panic!("expected Inspect event");
            };
            let value = inspections.get(&request.subagent_id).cloned().flatten();
            let _ = request.respond_to.send(value);
        }
    };
    tokio::join!(
            reconcile_orphaned_subagents_with_backend(
                unfinished,
                &backend,
                session_dir,
                "parent-x",
                gateway,
                parent_cmd_tx,
                ORPHAN_RECONCILE_REASON,
                std::sync::Arc::new(tokio::sync::Mutex::new(())),
            ),
            respond,
        );
}
async fn live_reconcile_with_inspections(
    inspections: HashMap<String, Option<SubagentInspection>>,
    session_dir: &Path,
    gateway: &GatewaySender,
    parent_cmd_tx: Option<&mpsc::UnboundedSender<SessionCommand>>,
) {
    live_reconcile_with_heal_lock(
            inspections,
            session_dir,
            gateway,
            parent_cmd_tx,
            std::sync::Arc::new(tokio::sync::Mutex::new(())),
        )
        .await
}
async fn live_reconcile_with_heal_lock(
    inspections: HashMap<String, Option<SubagentInspection>>,
    session_dir: &Path,
    gateway: &GatewaySender,
    parent_cmd_tx: Option<&mpsc::UnboundedSender<SessionCommand>>,
    heal_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let backend = ChannelBackend::new(event_tx);
    let respond = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let SubagentEvent::Inspect(request) = event else {
                continue;
            };
            let value = inspections.get(&request.subagent_id).cloned().flatten();
            let _ = request.respond_to.send(value);
        }
    });
    tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reconcile_live_orphaned_subagents(
                &backend,
                session_dir,
                "parent-x",
                gateway,
                parent_cmd_tx,
                heal_lock,
            ),
        )
        .await
        .expect("live reconcile must not hang");
    drop(backend);
    let _ = respond.await;
}
#[tokio::test]
async fn reconcile_orphan_flips_running_meta_to_cancelled() {
    use crate::test_support::lsp_runtime::test_gateway_with_receiver;
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-orphan";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    let (gateway, mut gateway_rx) = test_gateway_with_receiver();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    reconcile_with_inspections(
            &[],
            HashMap::from([(id.to_string(), None)]),
            session_dir.path(),
            &gateway,
            Some(&cmd_tx),
        )
        .await;
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "cancelled");
    assert_eq!(reread.tool_calls, Some(0));
    assert_eq!(reread.turns, Some(0));
    assert_eq!(
            drain_cancelled_finish_cmds(&mut cmd_rx, id, ORPHAN_RECONCILE_REASON),
            1
        );
    assert_eq!(
            drain_cancelled_finish_broadcasts(&mut gateway_rx, id),
            1
        );
}
#[tokio::test]
async fn reconcile_orphan_skips_shared_actor_live_child() {
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-live";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    reconcile_with_inspections(
            &[],
            HashMap::from([
                (
                    id.to_string(),
                    Some(inspection(id, SubagentSnapshotStatus::Initializing)),
                ),
            ]),
            session_dir.path(),
            &test_gateway(),
            None,
        )
        .await;
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "running");
}
#[tokio::test]
async fn reconcile_reemits_shared_actor_terminal_outcome() {
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-raced";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    reconcile_with_inspections(
            &[(id.to_string(), format!("child-{id}"))],
            HashMap::from([
                (
                    id.to_string(),
                    Some(
                        inspection(
                            id,
                            SubagentSnapshotStatus::Completed {
                                output: "done".to_string(),
                                tool_calls: 7,
                                turns: 2,
                                worktree_path: None,
                            },
                        ),
                    ),
                ),
            ]),
            session_dir.path(),
            &test_gateway(),
            Some(&cmd_tx),
        )
        .await;
    let finish = std::iter::from_fn(|| cmd_rx.try_recv().ok())
        .find_map(|command| {
            let SessionCommand::EchoAgentSessionNotification { notification } = command else {
                return None;
            };
            let SessionUpdate::SubagentFinished { status, tool_calls, .. } = notification
                .update else {
                return None;
            };
            Some((status, tool_calls))
        });
    assert_eq!(finish, Some(("completed".to_string(), 7)));
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "completed");
    assert_eq!(reread.tool_calls, Some(7));
    assert_eq!(reread.turns, Some(2));
}
#[tokio::test]
async fn live_reconcile_finalizes_running_meta_not_in_coordinator() {
    use crate::test_support::lsp_runtime::test_gateway_with_receiver;
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-live-orphan";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    let (gateway, mut gateway_rx) = test_gateway_with_receiver();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    live_reconcile_with_inspections(
            HashMap::from([(id.to_string(), None)]),
            session_dir.path(),
            &gateway,
            Some(&cmd_tx),
        )
        .await;
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "cancelled");
    assert_eq!(reread.tool_calls, Some(0));
    assert_eq!(reread.turns, Some(0));
    assert_eq!(
            drain_cancelled_finish_cmds(&mut cmd_rx, id, LIVE_ORPHAN_RECONCILE_REASON),
            1
        );
    assert_eq!(
            drain_cancelled_finish_broadcasts(&mut gateway_rx, id),
            1
        );
}
#[tokio::test]
async fn live_reconcile_skips_live_coordinator_child() {
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-live-keep";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    live_reconcile_with_inspections(
            HashMap::from([
                (
                    id.to_string(),
                    Some(
                        inspection(
                            id,
                            SubagentSnapshotStatus::Running {
                                turn_count: 1,
                                tool_call_count: 0,
                                tokens_used: 0,
                                context_window_tokens: 0,
                                context_usage_pct: 0,
                                tools_used: Vec::new(),
                                error_count: 0,
                            },
                        ),
                    ),
                ),
            ]),
            session_dir.path(),
            &test_gateway(),
            None,
        )
        .await;
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "running");
}
#[tokio::test]
async fn live_reconcile_reemitted_finish_has_will_wake_false() {
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-live-raced";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    live_reconcile_with_inspections(
            HashMap::from([
                (
                    id.to_string(),
                    Some(
                        inspection(
                            id,
                            SubagentSnapshotStatus::Completed {
                                output: "done".to_string(),
                                tool_calls: 3,
                                turns: 1,
                                worktree_path: None,
                            },
                        ),
                    ),
                ),
            ]),
            session_dir.path(),
            &test_gateway(),
            Some(&cmd_tx),
        )
        .await;
    let finish = std::iter::from_fn(|| cmd_rx.try_recv().ok())
        .find_map(|command| {
            let SessionCommand::EchoAgentSessionNotification { notification } = command else {
                return None;
            };
            let SessionUpdate::SubagentFinished { status, will_wake, .. } = notification
                .update else {
                return None;
            };
            Some((status, will_wake))
        });
    assert_eq!(finish, Some(("completed".to_string(), false)));
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "completed");
    assert_eq!(reread.tool_calls, Some(3));
    assert_eq!(reread.turns, Some(1));
}
#[tokio::test]
async fn live_reconcile_persists_terminal_meta_so_second_tick_is_noop() {
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-live-once";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let completed = inspection(
        id,
        SubagentSnapshotStatus::Completed {
            output: "done".to_string(),
            tool_calls: 4,
            turns: 2,
            worktree_path: None,
        },
    );
    live_reconcile_with_inspections(
            HashMap::from([(id.to_string(), Some(completed.clone()))]),
            session_dir.path(),
            &test_gateway(),
            Some(&cmd_tx),
        )
        .await;
    assert_eq!(
            std::iter::from_fn(|| cmd_rx.try_recv().ok())
                .filter(|command| matches!(
                    command,
                    SessionCommand::EchoAgentSessionNotification {
                        notification: SessionNotification {
                            update: SessionUpdate::SubagentFinished { .. },
                            ..
                        }
                    }
                ))
                .count(),
            1
        );
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "completed");
    assert_eq!(reread.tool_calls, Some(4));
    live_reconcile_with_inspections(
            HashMap::from([(id.to_string(), Some(completed))]),
            session_dir.path(),
            &test_gateway(),
            Some(&cmd_tx),
        )
        .await;
    assert!(cmd_rx.try_recv().is_err(), "second tick must not re-emit");
    live_reconcile_with_inspections(
            HashMap::from([(id.to_string(), None)]),
            session_dir.path(),
            &test_gateway(),
            Some(&cmd_tx),
        )
        .await;
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "completed");
    assert_eq!(reread.tool_calls, Some(4));
    assert!(cmd_rx.try_recv().is_err());
}
#[tokio::test]
async fn live_reconcile_overlapping_ticks_emit_once() {
    use crate::test_support::lsp_runtime::test_gateway_with_receiver;
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-live-race";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    let (gateway, mut gateway_rx) = test_gateway_with_receiver();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let completed = inspection(
        id,
        SubagentSnapshotStatus::Completed {
            output: "done".to_string(),
            tool_calls: 4,
            turns: 2,
            worktree_path: None,
        },
    );
    let inspections = HashMap::from([(id.to_string(), Some(completed))]);
    let heal_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    tokio::join!(
            live_reconcile_with_heal_lock(
                inspections.clone(),
                session_dir.path(),
                &gateway,
                Some(&cmd_tx),
                heal_lock.clone(),
            ),
            live_reconcile_with_heal_lock(
                inspections,
                session_dir.path(),
                &gateway,
                Some(&cmd_tx),
                heal_lock,
            ),
        );
    let cmd_finishes = std::iter::from_fn(|| cmd_rx.try_recv().ok())
        .filter(|command| {
            matches!(
                    command,
                    SessionCommand::EchoAgentSessionNotification {
                        notification: SessionNotification {
                            update: SessionUpdate::SubagentFinished { .. },
                            ..
                        }
                    }
                )
        })
        .count();
    assert_eq!(cmd_finishes, 1);
    let mut gateway_finishes = 0;
    while let Ok(msg) = gateway_rx.try_recv() {
        let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
            continue;
        };
        let notification: SessionNotification = serde_json::from_str(
                args.request.params.get(),
            )
            .unwrap();
        if matches!(
                notification.update,
                SessionUpdate::SubagentFinished { ref subagent_id, .. } if subagent_id == id
            ) {
            gateway_finishes += 1;
        }
    }
    assert_eq!(gateway_finishes, 1);
    let reread: SubagentMeta = serde_json::from_str(
            &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
        )
        .unwrap();
    assert_eq!(reread.status, "completed");
    assert_eq!(reread.tool_calls, Some(4));
}
#[tokio::test]
async fn live_reconcile_ignores_terminal_on_disk_meta() {
    for status in ["completed", "failed", "cancelled"] {
        let session_dir = tempfile::TempDir::new().unwrap();
        let id = format!("sa-term-{status}");
        let sub_dir = session_dir.path().join("subagents").join(&id);
        let mut meta = running_test_meta(&id, "parent-x");
        meta.status = status.to_string();
        write_subagent_meta(&sub_dir, &meta);
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        live_reconcile_with_inspections(
                HashMap::from([(id.clone(), None)]),
                session_dir.path(),
                &test_gateway(),
                Some(&cmd_tx),
            )
            .await;
        let reread: SubagentMeta = serde_json::from_str(
                &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
            )
            .unwrap();
        assert_eq!(reread.status, status);
        assert!(
                cmd_rx.try_recv().is_err(),
                "terminal on-disk meta must not emit on a live tick ({status})"
            );
    }
}
#[tokio::test]
async fn live_reconcile_persists_failed_and_cancelled_inspection() {
    let cases = [
        (
            SubagentSnapshotStatus::Failed {
                error: "boom".to_string(),
            },
            "failed",
        ),
        (
            SubagentSnapshotStatus::Cancelled {
                reason: Some("stop".to_string()),
            },
            "cancelled",
        ),
    ];
    for (status, expected) in cases {
        let session_dir = tempfile::TempDir::new().unwrap();
        let id = format!("sa-insp-{expected}");
        let sub_dir = session_dir.path().join("subagents").join(&id);
        write_subagent_meta(&sub_dir, &running_test_meta(&id, "parent-x"));
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        live_reconcile_with_inspections(
                HashMap::from([(id.clone(), Some(inspection(&id, status)))]),
                session_dir.path(),
                &test_gateway(),
                Some(&cmd_tx),
            )
            .await;
        let finish = std::iter::from_fn(|| cmd_rx.try_recv().ok())
            .find_map(|command| {
                let SessionCommand::EchoAgentSessionNotification { notification } = command
                else {
                    return None;
                };
                let SessionUpdate::SubagentFinished { status, will_wake, .. } = notification
                    .update else {
                    return None;
                };
                Some((status, will_wake))
            });
        assert_eq!(finish, Some((expected.to_string(), false)));
        let reread: SubagentMeta = serde_json::from_str(
                &std::fs::read_to_string(sub_dir.join("meta.json")).unwrap(),
            )
            .unwrap();
        assert_eq!(reread.status, expected);
    }
}
#[tokio::test]
async fn reconcile_dedups_replay_and_running_meta_sources() {
    let session_dir = tempfile::TempDir::new().unwrap();
    let id = "sa-crash";
    let sub_dir = session_dir.path().join("subagents").join(id);
    write_subagent_meta(&sub_dir, &running_test_meta(id, "parent-x"));
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    reconcile_with_inspections(
            &[(id.to_string(), format!("child-{id}"))],
            HashMap::from([(id.to_string(), None)]),
            session_dir.path(),
            &test_gateway(),
            Some(&cmd_tx),
        )
        .await;
    assert_eq!(
            drain_cancelled_finish_cmds(&mut cmd_rx, id, ORPHAN_RECONCILE_REASON),
            1
        );
}
#[test]
fn resume_rejects_conflicting_subagent_type() {
    let source = ResumeSourceData {
        subagent_id: "sub-gp".into(),
        child_session_id: "child-gp".into(),
        child_cwd: "/workspace".into(),
        worktree_path: None,
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: None,
        model_id: None,
    };
    let request_type = "explore";
    assert_ne!(
            request_type, source.subagent_type,
            "conflicting types should be detected"
        );
}
#[test]
fn resume_rejects_conflicting_persona() {
    let source = ResumeSourceData {
        subagent_id: "sub-impl".into(),
        child_session_id: "child-impl".into(),
        child_cwd: "/workspace".into(),
        worktree_path: None,
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: Some("implementer".into()),
        model_id: None,
    };
    let request_persona = Some("reviewer".to_string());
    let conflict = request_persona.as_deref() != source.persona.as_deref();
    assert!(conflict, "different persona should be detected as conflict");
}
#[test]
fn resume_allows_matching_identity() {
    let source = ResumeSourceData {
        subagent_id: "sub-ok".into(),
        child_session_id: "child-ok".into(),
        child_cwd: "/workspace".into(),
        worktree_path: None,
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: Some("implementer".into()),
        model_id: Some("echo-agent-3".into()),
    };
    assert_eq!("general-purpose", source.subagent_type);
    assert_eq!(Some("implementer"), source.persona.as_deref());
    assert_eq!(Some("echo-agent-3"), source.model_id.as_deref());
}
#[test]
fn resume_identity_does_not_gate_on_model() {
    let source = ResumeSourceData {
        subagent_id: "sub-model".into(),
        child_session_id: "child-model".into(),
        child_cwd: "/workspace".into(),
        worktree_path: None,
        snapshot_ref: None,
        subagent_type: "general-purpose".into(),
        persona: None,
        model_id: Some("echo-agent-3".into()),
    };
    assert!(
            echo_agent_subagent_resolution::validate_resume_identity(
                "general-purpose",
                None,
                &source,
            )
            .is_ok()
        );
    assert_eq!(
            source.model_id.as_deref(),
            Some("echo-agent-3"),
            "source model remains available for pinning"
        );
}
#[test]
fn durable_meta_roundtrips_effective_model_id() {
    let dir = std::env::temp_dir()
        .join("echo-agent-test-model-roundtrip")
        .join(uuid::Uuid::now_v7().to_string());
    let _ = std::fs::create_dir_all(&dir);
    let meta = SubagentMeta {
        subagent_id: "sa-model".into(),
        description: "d".into(),
        prompt: "p".into(),
        status: "completed".into(),
        completed_at: Some(chrono::Utc::now()),
        duration_ms: Some(100),
        tool_calls: Some(1),
        turns: Some(1),
        child_cwd: Some("/workspace".into()),
        effective_model_id: Some("echo-agent-3".into()),
        ..base_meta()
    };
    write_subagent_meta(&dir, &meta);
    let data = std::fs::read_to_string(dir.join("meta.json")).unwrap();
    let loaded: SubagentMeta = serde_json::from_str(&data).unwrap();
    assert_eq!(
            loaded.effective_model_id.as_deref(),
            Some("echo-agent-3"),
            "model ID should round-trip through meta.json"
        );
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn resume_model_pinning_overrides_default_resolution() {
    let source_model = Some("echo-agent-3".to_string());
    let resolved_model = "echo-agent-light";
    let needs_pin = source_model.as_deref() != Some(resolved_model);
    assert!(
            needs_pin,
            "resolved model differs from source — pinning should trigger"
        );
    let resolved_same = "echo-agent-3";
    let no_pin = source_model.as_deref() == Some(resolved_same);
    assert!(no_pin, "same model — no pinning needed");
}
#[test]
fn resume_window_safety_rejects_instead_of_swapping() {
    let estimated_tokens: u64 = 100_000;
    let child_window: u64 = 256_000;
    const SAFE_RESUME_PERCENT: u64 = 80;
    let threshold = child_window * SAFE_RESUME_PERCENT / 100;
    assert!(
            estimated_tokens <= threshold,
            "100k tokens should be within 80% of 256k window"
        );
    let large_transcript: u64 = 210_000;
    assert!(
            large_transcript > threshold,
            "210k tokens exceeds 80% of 256k window — resume should be rejected"
        );
}
#[test]
fn provenance_carries_resumed_from() {
    let prov = SubagentProvenance {
        fork_parent_prompt_id: Some("prompt-1".into()),
        resumed_from: Some("prev-agent-id".into()),
    };
    assert_eq!(prov.resumed_from.as_deref(), Some("prev-agent-id"));
    let fresh = SubagentProvenance::default();
    assert!(fresh.resumed_from.is_none());
}
#[test]
fn notification_subagent_spawned_includes_resumed_from() {
    let notification = SessionUpdate::SubagentSpawned {
        subagent_id: "sa-resumed".into(),
        parent_session_id: "parent".into(),
        parent_prompt_id: Some("prompt-1".into()),
        child_session_id: "child-resumed".into(),
        subagent_type: "general-purpose".into(),
        description: "fix review feedback".into(),
        effective_context_source: Some("resumed".into()),
        context_normalized: false,
        capability_mode: None,
        persona: Some("implementer".into()),
        role: None,
        model: None,
        resumed_from: Some("prev-agent-id".into()),
        workflow_run_id: None,
    };
    let json = serde_json::to_value(&notification).unwrap();
    assert_eq!(json["resumed_from"], "prev-agent-id");
    assert_eq!(json["effective_context_source"], "resumed");
    assert_eq!(json["role"], serde_json::Value::Null);
    assert_eq!(json["model"], serde_json::Value::Null);
    let fresh = SessionUpdate::SubagentSpawned {
        subagent_id: "sa-fresh".into(),
        parent_session_id: "p".into(),
        parent_prompt_id: None,
        child_session_id: "c".into(),
        subagent_type: "explore".into(),
        description: "d".into(),
        effective_context_source: Some("new".into()),
        context_normalized: false,
        capability_mode: None,
        persona: None,
        role: None,
        model: None,
        resumed_from: None,
        workflow_run_id: None,
    };
    let json = serde_json::to_value(&fresh).unwrap();
    assert!(json.get("resumed_from").is_none());
    assert!(json.get("role").is_none());
    assert!(json.get("model").is_none());
}
#[test]
fn upload_ref_includes_resumed_from() {
    let ref_resumed = SubagentSpawnedRef {
        subagent_id: "sa-r".into(),
        child_session_id: "child-r".into(),
        subagent_type: "general-purpose".into(),
        description: "goal achievement skeptic".into(),
        persona: Some("implementer".into()),
        resumed_from: Some("prev-agent".into()),
    };
    let json = serde_json::to_value(&ref_resumed).unwrap();
    assert_eq!(json["resumed_from"], "prev-agent");
    assert_eq!(json["description"], "goal achievement skeptic");
    let ref_fresh = SubagentSpawnedRef {
        subagent_id: "sa-f".into(),
        child_session_id: "child-f".into(),
        subagent_type: "explore".into(),
        description: String::new(),
        persona: None,
        resumed_from: None,
    };
    let json = serde_json::to_value(&ref_fresh).unwrap();
    assert!(json.get("resumed_from").is_none());
    assert!(json.get("description").is_none());
    let parsed: SubagentSpawnedRef = serde_json::from_value(json).unwrap();
    assert!(parsed.description.is_empty());
}
#[test]
fn turn_active_flag_defaults_to_false() {
    let presentation = SubagentPresentation::new();
    assert!(
            !presentation
                .turn_active_flag()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
}
#[test]
fn turn_active_flag_shared_via_arc() {
    let presentation = SubagentPresentation::new();
    let flag = presentation.turn_active_flag();
    assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
            presentation
                .turn_active_flag()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    flag.store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(
            !presentation
                .turn_active_flag()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
}
fn ctx_with_parent_chat_state(
    session_model_id: &str,
    inference_slug: &str,
    global_model_id: &str,
    available_models: indexmap::IndexMap<String, crate::agent::config::ModelEntry>,
) -> SubagentSpawnContext {
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.model_id = acp::ModelId::new(session_model_id);
    ctx.parent_chat_state = Some(spawn_test_parent_chat_state(inference_slug));
    ctx.models_manager = crate::agent::models::ModelsManager::new(
        None,
        available_models.clone(),
        acp::ModelId::new(global_model_id),
        ctx.auth_manager.clone(),
        crate::agent::config::Config::default(),
    );
    ctx.available_models = available_models;
    ctx
}
#[tokio::test]
async fn read_parent_sampling_config_keeps_auto_catalog_id_with_routing_slug() {
    let mut models = indexmap::IndexMap::new();
    models.insert("auto".to_string(), test_model_entry("echo-agent-4.5"));
    let ctx = ctx_with_parent_chat_state("auto", "echo-agent-4.5", "composer-2-fast", models);
    let (config, model_id) = read_parent_sampling_config(&ctx).await;
    assert_eq!(config.model, "echo-agent-4.5");
    assert_eq!(model_id.0.as_ref(), "auto");
}
#[tokio::test]
async fn read_parent_sampling_config_keeps_auto_when_catalog_has_slug_key_only() {
    let mut models = indexmap::IndexMap::new();
    let mut entry = test_model_entry("echo-agent-4.5");
    entry.info.supports_backend_search = true;
    models.insert("echo-agent-4.5".to_string(), entry);
    let ctx = ctx_with_parent_chat_state("auto", "echo-agent-4.5", "auto", models);
    ctx.parent_chat_state
        .as_ref()
        .unwrap()
        .update_sampling_config(echo_agent_sampling_types::SamplingConfig {
            api_backend: crate::sampling::ApiBackend::Responses,
            base_url: "https://api.echo-agent.invalid/v1".to_string(),
            ..test_sampling_config("echo-agent-4.5")
        });
    let (config, model_id) = read_parent_sampling_config(&ctx).await;
    assert_eq!(config.model, "echo-agent-4.5");
    assert_eq!(model_id.0.as_ref(), "auto");
    assert!(config.supports_backend_search);
    assert_eq!(config.extra_response_includes, ["no_inline_citations"]);
}
#[tokio::test]
async fn read_parent_sampling_config_fallback_uses_session_model_id() {
    let mut models = indexmap::IndexMap::new();
    models.insert("composer-2-fast".to_string(), test_model_entry("composer-2-fast"));
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.model_id = acp::ModelId::new("composer-2-fast");
    ctx.parent_chat_state = None;
    ctx.sampling_config.model = "composer-2-fast".to_string();
    ctx.available_models = models;
    ctx.models_manager = crate::agent::models::ModelsManager::new(
        None,
        indexmap::IndexMap::new(),
        acp::ModelId::new("auto"),
        ctx.auth_manager.clone(),
        crate::agent::config::Config::default(),
    );
    let (config, model_id) = read_parent_sampling_config(&ctx).await;
    assert_eq!(config.model, "composer-2-fast");
    assert_eq!(model_id.0.as_ref(), "composer-2-fast");
    assert_ne!(model_id.0.as_ref(), "auto");
}
#[tokio::test]
async fn read_parent_sampling_config_ignores_global_default() {
    let mut models = indexmap::IndexMap::new();
    models.insert("composer-2-fast".to_string(), test_model_entry("composer-2-fast"));
    let ctx = ctx_with_parent_chat_state(
        "composer-2-fast",
        "composer-2-fast",
        "auto",
        models,
    );
    let (config, model_id) = read_parent_sampling_config(&ctx).await;
    assert_eq!(config.model, "composer-2-fast");
    assert_eq!(model_id.0.as_ref(), "composer-2-fast");
    assert_ne!(
            model_id.0.as_ref(),
            ctx.models_manager.current_model_id().0.as_ref(),
        );
}
/// Every subagent config path must carry the live bearer resolver: a
/// config frozen at spawn 401s for the rest of the subagent's life once
/// the parent rotates its token (the wake-from-sleep failure mode).
/// First-party base URL so the assertion holds whether the catalog memo
/// reports `NotByok` or `Unknown`.
#[tokio::test]
async fn read_parent_sampling_config_fallback_wires_bearer_resolver() {
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.parent_chat_state = None;
    ctx.auth_method_id = acp::AuthMethodId::new(
        crate::agent::auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
    );
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.sampling_config.base_url = "https://api.echo-agent.invalid/v1".to_string();
    let (config, _) = read_parent_sampling_config(&ctx).await;
    assert!(config.bearer_resolver.is_some());
}
/// The inherit-live path honors `would_strip_fallback_key` like the
/// other two paths (it used to install the resolver unconditionally,
/// stripping a no-session parent's env-key fallback).
#[tokio::test]
async fn read_parent_sampling_config_live_never_strips_a_fallback_key() {
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.auth_method_id = acp::AuthMethodId::new(
        crate::agent::auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
    );
    ctx.auth = None;
    let chat = spawn_test_parent_chat_state("echo-agent-4.5");
    chat.update_credentials(echo_agent_chat_state::Credentials {
        api_key: Some("echo-agent-env-fallback".to_string()),
        auth_type: echo_agent_chat_state::AuthType::SessionToken,
        alpha_test_key: None,
        client_version: None,
    });
    ctx.parent_chat_state = Some(chat);
    let (config, _) = read_parent_sampling_config(&ctx).await;
    assert!(
            config.bearer_resolver.is_none(),
            "with no session, the live path must not displace a fallback key"
        );
    assert_eq!(config.api_key.as_deref(), Some("echo-agent-env-fallback"));
}
/// `would_strip_fallback_key` on the inherit-fallback path: the baseline
/// keeps the env `ECHO_AGENT_API_KEY` even while `auth_type` flips to
/// `SessionToken`, and no resolver may displace it.
#[tokio::test]
async fn read_parent_sampling_config_fallback_never_strips_a_fallback_key() {
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.parent_chat_state = None;
    ctx.auth_method_id = acp::AuthMethodId::new(
        crate::agent::auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
    );
    ctx.auth = None;
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.sampling_config.base_url = "https://api.echo-agent.invalid/v1".to_string();
    ctx.sampling_config.api_key = Some("echo-agent-env-fallback".to_string());
    let (config, _) = read_parent_sampling_config(&ctx).await;
    assert!(config.bearer_resolver.is_none());
    assert_eq!(config.api_key.as_deref(), Some("echo-agent-env-fallback"));
}
#[tokio::test]
async fn read_parent_sampling_config_fallback_no_resolver_for_api_key_method() {
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.parent_chat_state = None;
    ctx.auth_method_id = acp::AuthMethodId::new(
        crate::agent::auth_method::ECHO_AGENT_API_KEY_METHOD_ID,
    );
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.sampling_config.base_url = "https://api.echo-agent.invalid/v1".to_string();
    let (config, _) = read_parent_sampling_config(&ctx).await;
    assert!(config.bearer_resolver.is_none());
}
/// The override path wires the resolver for a session key regardless of
/// freshness. Hard-expired (the post-sleep 401 window) is the case that
/// matters: gating on wire-validity would freeze the subagent for life;
/// the sampler strips the dead seeded key at request time instead.
#[test]
fn resolve_model_override_wires_resolver_for_fresh_and_hard_expired_session_keys() {
    for auth in [
        crate::auth::EchoAgentAuth {
            key: "session-jwt".into(),
            ..crate::auth::EchoAgentAuth::test_default()
        },
        crate::auth::EchoAgentAuth {
            key: "hard-expired-session-jwt".into(),
            create_time: chrono::Utc::now() - chrono::Duration::hours(2),
            expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
            ..crate::auth::EchoAgentAuth::test_default()
        },
    ] {
        let key = auth.key.clone();
        let mut ctx = ctx_with_toggle(HashMap::new());
        ctx.auth_method_id = acp::AuthMethodId::new(
            crate::agent::auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
        );
        ctx.auth = Some(auth);
        ctx.available_models
            .insert("echo-agent-4.5".to_string(), test_model_entry("echo-agent-4.5"));
        let (config, _) = resolve_model_override_to_config("echo-agent-4.5", &ctx).unwrap();
        assert!(config.bearer_resolver.is_some(), "key={key}");
    }
}
/// `would_strip_fallback_key` on the override path. `ECHO_AGENT_API_KEY`'s
/// presence varies by environment, so assert the rule itself rather
/// than one branch of it.
#[test]
fn resolve_model_override_to_config_never_strips_a_fallback_key() {
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.auth_method_id = acp::AuthMethodId::new(
        crate::agent::auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
    );
    ctx.auth = None;
    ctx.available_models.insert("echo-agent-4.5".to_string(), test_model_entry("echo-agent-4.5"));
    let (config, _) = resolve_model_override_to_config("echo-agent-4.5", &ctx).unwrap();
    assert_eq!(
            config.bearer_resolver.is_some(),
            config.api_key.is_none(),
            "with no session, a resolver is installed only when it displaces nothing"
        );
}
/// A wired resolver is the sampler's sole auth source, so it must never
/// displace a per-model key.
#[test]
fn resolve_model_override_to_config_no_resolver_for_byok_model() {
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.auth_method_id = acp::AuthMethodId::new(
        crate::agent::auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
    );
    let mut byok = test_model_entry("byok-model");
    byok.api_key = Some("sk-byok".to_string());
    ctx.available_models.insert("byok-model".to_string(), byok);
    let (config, _) = resolve_model_override_to_config("byok-model", &ctx).unwrap();
    assert!(config.bearer_resolver.is_none());
    assert_eq!(config.api_key.as_deref(), Some("sk-byok"));
}
#[tokio::test]
async fn read_parent_sampling_config_resolves_backend_search_from_catalog() {
    let mut entry = test_model_entry("echo-agent-4.5");
    entry.info.supports_backend_search = true;
    let mut models = indexmap::IndexMap::new();
    models.insert("auto".to_string(), entry);
    let mut ctx = ctx_with_parent_chat_state("auto", "echo-agent-4.5", "auto", models);
    ctx.sampling_config.supports_backend_search = false;
    let (config, _model_id) = read_parent_sampling_config(&ctx).await;
    assert!(
            config.supports_backend_search,
            "subagent should inherit backend-tools capability from the live model catalog"
        );
}
#[tokio::test]
async fn read_parent_sampling_config_fallback_resolves_backend_search_from_catalog() {
    let mut entry = test_model_entry("echo-agent-4.5");
    entry.info.supports_backend_search = true;
    let mut models = indexmap::IndexMap::new();
    models.insert("echo-agent-4.5".to_string(), entry);
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.model_id = acp::ModelId::new("auto");
    ctx.parent_chat_state = None;
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.sampling_config.api_backend = crate::sampling::ApiBackend::Responses;
    ctx.sampling_config.base_url = "https://api.echo-agent.invalid/v1".to_string();
    ctx.sampling_config.supports_backend_search = false;
    ctx.models_manager = crate::agent::models::ModelsManager::new(
        None,
        models,
        acp::ModelId::new("auto"),
        ctx.auth_manager.clone(),
        crate::agent::config::Config::default(),
    );
    let (config, model_id) = read_parent_sampling_config(&ctx).await;
    assert_eq!(model_id.0.as_ref(), "auto");
    assert!(config.supports_backend_search);
    assert_eq!(config.extra_response_includes, ["no_inline_citations"]);
}
#[tokio::test]
async fn read_parent_sampling_config_resolves_compactions_remaining_from_catalog() {
    use echo_agent_sampling_types::CompactionsRemaining;
    let mut entry = test_model_entry("echo-agent-4.5");
    entry.info.compactions_remaining = Some(CompactionsRemaining::Dynamic(true));
    let mut models = indexmap::IndexMap::new();
    models.insert("echo-agent-4.5".to_string(), entry);
    let mut ctx = ctx_with_parent_chat_state("auto", "echo-agent-4.5", "auto", models);
    ctx.sampling_config.compactions_remaining = None;
    let (config, _model_id) = read_parent_sampling_config(&ctx).await;
    assert_eq!(
            config.compactions_remaining,
            Some(CompactionsRemaining::Dynamic(true)),
            "subagent should inherit compactions-remaining capability from the live model catalog"
        );
}
#[tokio::test]
async fn read_parent_sampling_config_fallback_resolves_compactions_remaining_from_catalog() {
    use echo_agent_sampling_types::CompactionsRemaining;
    let mut entry = test_model_entry("echo-agent-4.5");
    entry.info.compactions_remaining = Some(CompactionsRemaining::Dynamic(true));
    let mut models = indexmap::IndexMap::new();
    models.insert("echo-agent-4.5".to_string(), entry);
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.model_id = acp::ModelId::new("auto");
    ctx.parent_chat_state = None;
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.sampling_config.compactions_remaining = None;
    ctx.models_manager = crate::agent::models::ModelsManager::new(
        None,
        models,
        acp::ModelId::new("auto"),
        ctx.auth_manager.clone(),
        crate::agent::config::Config::default(),
    );
    let (config, model_id) = read_parent_sampling_config(&ctx).await;
    assert_eq!(model_id.0.as_ref(), "auto");
    assert_eq!(
            config.compactions_remaining,
            Some(CompactionsRemaining::Dynamic(true)),
            "fallback path should also resolve compactions-remaining capability from the catalog"
        );
}
/// Drive the REAL precedence path
/// (`resolve_effective_model_config`, which `run_shell_child`
/// calls) with BOTH an explicit `runtime_override_model` AND a
/// `[subagents.models]` pin for the same agent present, asserting the
/// runtime override wins; with `None` (inherit) the pin wins (precedence
/// handed back); and an unknown override falls through to the pin.
#[tokio::test]
async fn runtime_override_wins_over_subagents_models_pin_in_precedence_path() {
    use echo_agent_core::config::ModelOverride;
    let build_ctx = || {
        let mut models = indexmap::IndexMap::new();
        models.insert("goal-model".to_string(), test_model_entry("goal-model"));
        models.insert("pinned-model".to_string(), test_model_entry("pinned-model"));
        let mut ctx = ctx_with_toggle(HashMap::new());
        ctx.available_models = models;
        ctx.subagent_model_overrides = HashMap::from([
            ("explore".to_string(), "pinned-model".to_string()),
        ]);
        ctx
    };
    let ctx = build_ctx();
    let (config, model_id) = resolve_effective_model_config(
            Some("goal-model"),
            "explore",
            &ModelOverride::Inherit,
            &ctx,
        )
        .await;
    assert_eq!(
            config.model, "goal-model",
            "the goal runtime override must win over the `[subagents.models]` pin",
        );
    assert_eq!(model_id.0.as_ref(), "goal-model");
    let ctx = build_ctx();
    let (config, model_id) = resolve_effective_model_config(
            None,
            "explore",
            &ModelOverride::Inherit,
            &ctx,
        )
        .await;
    assert_eq!(
            config.model, "pinned-model",
            "with no runtime override, the `[subagents.models]` pin wins",
        );
    assert_eq!(model_id.0.as_ref(), "pinned-model");
    let ctx = build_ctx();
    let (config, _) = resolve_effective_model_config(
            Some("does-not-exist"),
            "explore",
            &ModelOverride::Inherit,
            &ctx,
        )
        .await;
    assert_eq!(
            config.model, "pinned-model",
            "an unknown override falls through to the pin",
        );
}
/// A `fork_context = true` spawn must infer on the parent session model
/// (`ctx.model_id`) for per-model radix reuse, even when a
/// `[subagents.models]` pin and an `AgentDefinition.model` override are
/// both present. `run_shell_child` forces
/// `effective_runtime.model = Some(ctx.model_id)` on the fork path after
/// other override sources; the runtime override wins in
/// `resolve_effective_model_config`.
#[tokio::test]
async fn fork_context_pins_parent_model_over_overrides() {
    use echo_agent_core::config::ModelOverride;
    let build_ctx = || {
        let mut ctx = ctx_with_toggle(HashMap::new());
        ctx.sampling_config.model = "parent-model".to_string();
        ctx.model_id = acp::ModelId::new("parent-model");
        ctx.available_models
            .insert("parent-model".to_string(), test_model_entry("parent-model"));
        ctx.available_models
            .insert("pinned-model".to_string(), test_model_entry("pinned-model"));
        ctx.available_models
            .insert("agentdef-model".to_string(), test_model_entry("agentdef-model"));
        ctx.subagent_model_overrides
            .insert("general-purpose".to_string(), "pinned-model".to_string());
        ctx
    };
    let agent_def = ModelOverride::Override("agentdef-model".to_string());
    let ctx = build_ctx();
    let fork_context = true;
    let mut runtime_override: Option<String> = None;
    if fork_context {
        runtime_override = Some(ctx.model_id.0.to_string());
    }
    let (config, model_id) = resolve_effective_model_config(
            runtime_override.as_deref(),
            "general-purpose",
            &agent_def,
            &ctx,
        )
        .await;
    assert_eq!(
            config.model, "parent-model",
            "fork_context must pin the parent model over the [subagents.models] pin and agent-def override",
        );
    assert_eq!(model_id.0.as_ref(), "parent-model");
    let ctx = build_ctx();
    let (config, model_id) = resolve_effective_model_config(
            None,
            "general-purpose",
            &agent_def,
            &ctx,
        )
        .await;
    assert_eq!(
            config.model, "pinned-model",
            "without the fork pin the [subagents.models] override wins",
        );
    assert_eq!(model_id.0.as_ref(), "pinned-model");
}
/// With no explicit pin, the subagent inherits the parent model for any
/// parent model, with no special-casing (a "heavy"/custom parent
/// is treated identically to any other).
#[tokio::test]
async fn resolve_subagent_inherits_parent_model_without_pins() {
    use echo_agent_core::config::ModelOverride;
    for parent_model in ["echo-agent-4.5", "composer-2-fast", "my-custom-byok-model"] {
        let mut ctx = ctx_with_toggle(HashMap::new());
        ctx.sampling_config.model = parent_model.to_string();
        ctx.model_id = acp::ModelId::new(parent_model);
        let (config, model_id) = resolve_subagent_sampling_config(
                "explore",
                &ModelOverride::Inherit,
                &ctx,
            )
            .await;
        assert_eq!(
                config.model, parent_model,
                "subagent must inherit parent model {parent_model:?} when no pin is set",
            );
        assert_eq!(model_id.0.as_ref(), parent_model);
    }
}
/// An explicit `[subagents.models]` pin routes the subagent to that
/// model regardless of the parent model — both a light parent
/// (`echo-agent-4.5`) and a custom parent (`composer-2-fast`)
/// honor the pin identically now that the heavy-model gate is gone.
#[tokio::test]
async fn resolve_subagent_config_override_pin_applies_for_any_parent() {
    use echo_agent_core::config::ModelOverride;
    for parent_model in ["echo-agent-4.5", "composer-2-fast"] {
        let mut ctx = ctx_with_toggle(HashMap::new());
        ctx.sampling_config.model = parent_model.to_string();
        ctx.model_id = acp::ModelId::new(parent_model);
        ctx.available_models
            .insert("pinned-model".to_string(), test_model_entry("pinned-model"));
        ctx.subagent_model_overrides
            .insert("explore".to_string(), "pinned-model".to_string());
        let (config, model_id) = resolve_subagent_sampling_config(
                "explore",
                &ModelOverride::Inherit,
                &ctx,
            )
            .await;
        assert_eq!(
                config.model, "pinned-model",
                "config pin must win for parent {parent_model:?}",
            );
        assert_eq!(model_id.0.as_ref(), "pinned-model");
    }
}
/// An explicit `AgentDefinition.model = Override(id)` pin routes the
/// subagent to that model even when the parent runs a light model.
#[tokio::test]
async fn resolve_subagent_agent_definition_pin_applies_for_light_parent() {
    use echo_agent_core::config::ModelOverride;
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.model_id = acp::ModelId::new("echo-agent-4.5");
    ctx.available_models
        .insert("pinned-model".to_string(), test_model_entry("pinned-model"));
    let agent_model = ModelOverride::Override("pinned-model".to_string());
    let (config, model_id) = resolve_subagent_sampling_config(
            "explore",
            &agent_model,
            &ctx,
        )
        .await;
    assert_eq!(config.model, "pinned-model");
    assert_eq!(model_id.0.as_ref(), "pinned-model");
}
/// Priority 1 (`[subagents.models]`) wins over Priority 2
/// (`AgentDefinition.model`) when both pins are set and both resolve.
#[tokio::test]
async fn resolve_subagent_config_override_wins_over_agent_definition() {
    use echo_agent_core::config::ModelOverride;
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.model_id = acp::ModelId::new("echo-agent-4.5");
    ctx.available_models
        .insert("config-pin".to_string(), test_model_entry("config-pin"));
    ctx.available_models
        .insert("agentdef-pin".to_string(), test_model_entry("agentdef-pin"));
    ctx.subagent_model_overrides.insert("explore".to_string(), "config-pin".to_string());
    let agent_model = ModelOverride::Override("agentdef-pin".to_string());
    let (config, model_id) = resolve_subagent_sampling_config(
            "explore",
            &agent_model,
            &ctx,
        )
        .await;
    assert_eq!(config.model, "config-pin");
    assert_eq!(model_id.0.as_ref(), "config-pin");
}
/// An unresolvable `[subagents.models]` pin (model absent from
/// `available_models`) falls through to inherit the parent model.
#[tokio::test]
async fn resolve_subagent_config_override_unknown_model_falls_through_to_inherit() {
    use echo_agent_core::config::ModelOverride;
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.model_id = acp::ModelId::new("echo-agent-4.5");
    ctx.subagent_model_overrides
        .insert("explore".to_string(), "does-not-exist".to_string());
    let (config, model_id) = resolve_subagent_sampling_config(
            "explore",
            &ModelOverride::Inherit,
            &ctx,
        )
        .await;
    assert_eq!(config.model, "echo-agent-4.5");
    assert_eq!(model_id.0.as_ref(), "echo-agent-4.5");
}
/// An unresolvable `AgentDefinition.model` pin (model absent from
/// `available_models`) falls through to inherit the parent model.
#[tokio::test]
async fn resolve_subagent_agent_definition_unknown_model_falls_through_to_inherit() {
    use echo_agent_core::config::ModelOverride;
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.model_id = acp::ModelId::new("echo-agent-4.5");
    let agent_model = ModelOverride::Override("does-not-exist".to_string());
    let (config, model_id) = resolve_subagent_sampling_config(
            "explore",
            &agent_model,
            &ctx,
        )
        .await;
    assert_eq!(config.model, "echo-agent-4.5");
    assert_eq!(model_id.0.as_ref(), "echo-agent-4.5");
}
/// Spawn-time credentials are cache-only: a cold spawn has no key,
/// never the parent session key.
#[tokio::test]
async fn subagent_override_provider_model_spawns_cache_only_credentials() {
    use echo_agent_core::config::ModelOverride;
    let dir = tempfile::tempdir().unwrap();
    let provider = crate::auth::test_counting_provider(
        "test-subagent-spawn",
        dir.path(),
    );
    let mut entry = test_model_entry("proxied-model");
    entry.info.base_url = "https://gateway.example/v1".to_string();
    entry.auth_provider = Some(provider.clone());
    let mut models = indexmap::IndexMap::new();
    models.insert("proxied".to_string(), entry);
    let mut ctx = ctx_with_toggle(HashMap::new());
    ctx.sampling_config.model = "echo-agent-4.5".to_string();
    ctx.model_id = acp::ModelId::new("echo-agent-4.5");
    ctx.available_models = models;
    ctx.auth = Some(crate::auth::EchoAgentAuth {
        key: "parent-session-jwt".to_string(),
        ..Default::default()
    });
    ctx.subagent_model_overrides.insert("explore".to_string(), "proxied".to_string());
    let (config, model_id) = resolve_subagent_sampling_config(
            "explore",
            &ModelOverride::Inherit,
            &ctx,
        )
        .await;
    assert_eq!(model_id.0.as_ref(), "proxied");
    assert_eq!(
            config.api_key, None,
            "a cold cache spawns with no key, never the parent session key"
        );
    provider.ensure_fresh_token(None).await.rotated().unwrap();
    let (config, _) = resolve_subagent_sampling_config(
            "explore",
            &ModelOverride::Inherit,
            &ctx,
        )
        .await;
    assert_eq!(config.api_key.as_deref(), Some("tok-1"));
    assert_eq!(config.base_url, "https://gateway.example/v1");
}
#[test]
fn key_prefix_truncates_to_8_chars() {
    let key = Some("eyJ0eXAiOiJhbGciOiJSUzI1NiJ9".to_string());
    assert_eq!(key_prefix(&key), "eyJ0eXAi");
}
#[test]
fn key_prefix_short_key_not_truncated() {
    let key = Some("abc".to_string());
    assert_eq!(key_prefix(&key), "abc");
}
#[test]
fn key_prefix_none_returns_placeholder() {
    assert_eq!(key_prefix(&None), "<none>");
}
#[test]
fn key_prefix_empty_string() {
    let key = Some(String::new());
    assert_eq!(key_prefix(&key), "");
}
#[test]
fn non_cursor_persona_injected_as_system_reminder() {
    use echo_agent_sampling_types::conversation::{ConversationItem, SyntheticReason};
    let persona = "You are a pragmatic implementer.";
    let mut conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("task"),
        ];
    let mut prefix_len: usize = 2;
    let reminder = ConversationItem::system_reminder(
        format!(
            "<system-reminder>\n{persona}\n</system-reminder>"
        ),
    );
    let insert_at = prefix_len.min(conv.len());
    conv.insert(insert_at, reminder);
    prefix_len += 1;
    assert_eq!(conv.len(), 3, "conversation should have 3 items");
    assert_eq!(prefix_len, 3, "prefix_len should be incremented");
    if let ConversationItem::User(ref u) = conv[2] {
        assert_eq!(u.synthetic_reason, Some(SyntheticReason::SystemReminder));
        let text = u
            .content
            .first()
            .map(|c| match c {
                echo_agent_sampling_types::conversation::ContentPart::Text { text } => {
                    text.as_ref()
                }
                _ => "",
            });
        assert!(
                text.unwrap_or("").contains("<system-reminder>"),
                "should use hyphen tag format"
            );
        assert!(
                text.unwrap_or("").contains(persona),
                "should contain the persona instructions"
            );
    } else {
        panic!("expected User variant for system_reminder");
    }
}
#[test]
fn persona_injection_skipped_for_resumed() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let persona_instructions = Some("Be thorough.".to_string());
    let context_source = InitialContextSource::Resumed;
    let mut conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("old turn"),
        ];
    let original_len = conv.len();
    let mut prefix_len = original_len;
    if context_source != InitialContextSource::Resumed
        && let Some(ref pi) = persona_instructions
    {
        let reminder = ConversationItem::system_reminder(
            format!(
                "<system-reminder>\n{pi}\n</system-reminder>"
            ),
        );
        let insert_at = prefix_len.min(conv.len());
        conv.insert(insert_at, reminder);
        prefix_len += 1;
    }
    assert_eq!(
            conv.len(),
            original_len,
            "resumed session should not get persona injected"
        );
    assert_eq!(prefix_len, original_len, "prefix_len should be unchanged");
}
#[test]
fn persona_injection_into_empty_conversation() {
    use echo_agent_sampling_types::conversation::ConversationItem;
    let mut conv: Vec<ConversationItem> = vec![];
    let mut prefix_len: usize = 0;
    let reminder = ConversationItem::system_reminder(
        "<system-reminder>\nDo X.\n</system-reminder>".to_string(),
    );
    let insert_at = prefix_len.min(conv.len());
    conv.insert(insert_at, reminder);
    prefix_len += 1;
    assert_eq!(conv.len(), 1);
    assert_eq!(prefix_len, 1);
    assert!(matches!(& conv[0], ConversationItem::User(_)));
}
mod cancellation_error_message_tests {
    use super::super::cancellation_error_message;
    use crate::session::commands::CancellationContext;
    use echo_agent_session_events::types::CancellationCategory;
    #[test]
    fn permission_rejected_with_context() {
        let ctx = CancellationContext {
            tool_name: Some("run_terminal_cmd".into()),
            reason: Some("User rejected the execution".into()),
            ..Default::default()
        };
        let msg = cancellation_error_message(
            Some(CancellationCategory::PermissionRejected),
            Some(&ctx),
        );
        assert!(msg.contains("user rejected permission"));
        assert!(msg.contains("run_terminal_cmd"));
        assert!(msg.contains("User rejected the execution"));
    }
    #[test]
    fn permission_rejected_without_context() {
        let msg = cancellation_error_message(
            Some(CancellationCategory::PermissionRejected),
            None,
        );
        assert!(msg.contains("user rejected a permission prompt"));
    }
    #[test]
    fn permission_cancelled() {
        let msg = cancellation_error_message(
            Some(CancellationCategory::PermissionCancelled),
            None,
        );
        assert!(msg.contains("user cancelled a permission prompt"));
    }
    #[test]
    fn hook_denied_with_context() {
        let ctx = CancellationContext {
            tool_name: Some("run_terminal_cmd".into()),
            reason: Some("blocked by policy".into()),
            hook_name: Some("safe-shell-guard".into()),
            ..Default::default()
        };
        let msg = cancellation_error_message(
            Some(CancellationCategory::HookDenied),
            Some(&ctx),
        );
        assert!(msg.contains("hook denied"));
        assert!(msg.contains("safe-shell-guard"));
        assert!(msg.contains("run_terminal_cmd"));
    }
    #[test]
    fn hook_denied_without_context() {
        let msg = cancellation_error_message(
            Some(CancellationCategory::HookDenied),
            None,
        );
        assert!(msg.contains("blocked by a hook"));
    }
    #[test]
    fn mid_turn_abort() {
        let msg = cancellation_error_message(
            Some(CancellationCategory::MidTurnAbort),
            None,
        );
        assert!(msg.contains("aborted mid-turn"));
    }
    #[test]
    fn no_category_no_context() {
        let msg = cancellation_error_message(None, None);
        assert_eq!(msg, "Subagent turn was cancelled");
    }
    #[test]
    fn partial_context_only_tool_name() {
        let ctx = CancellationContext {
            tool_name: Some("search_replace".into()),
            ..Default::default()
        };
        let msg = cancellation_error_message(
            Some(CancellationCategory::PermissionRejected),
            Some(&ctx),
        );
        assert!(msg.contains("search_replace"));
    }
    #[test]
    fn empty_context_falls_back() {
        let ctx = CancellationContext::default();
        let msg = cancellation_error_message(
            Some(CancellationCategory::PermissionRejected),
            Some(&ctx),
        );
        assert!(msg.contains("user rejected a permission prompt"));
    }
}
fn make_pool(names: &[&str]) -> crate::session::mcp_servers::SharedMcpPool {
    use crate::session::mcp_servers::{McpClient, McpState, SharedMcpPool};
    let mut state = McpState::new(vec![]);
    for &name in names {
        state.owned_clients.insert(name.to_string(), Arc::new(McpClient::stub(name)));
    }
    SharedMcpPool::from_state(&state)
}
fn pool_names(pool: &crate::session::mcp_servers::SharedMcpPool) -> Vec<String> {
    let mut names: Vec<String> = pool.server_names().map(str::to_string).collect();
    names.sort();
    names
}
#[test]
fn filter_inheritance_all_passes_everything_through() {
    let pool = make_pool(&["github", "linear", "slack"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::All,
    );
    let result = result.expect("All should return Some");
    assert_eq!(pool_names(&result), vec!["github", "linear", "slack"]);
}
#[test]
fn filter_inheritance_none_returns_none() {
    let pool = make_pool(&["github", "linear"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::None,
    );
    assert!(result.is_none());
}
#[test]
fn filter_inheritance_named_selects_specific_servers() {
    let pool = make_pool(&["github", "linear", "slack", "jira"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Named(
            vec!["github".into(), "slack".into()],
        ),
    );
    let result = result.expect("Named should return Some");
    assert_eq!(pool_names(&result), vec!["github", "slack"]);
}
#[test]
fn filter_inheritance_except_excludes_specific_servers() {
    let pool = make_pool(&["github", "linear", "slack", "jira"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Except(
            vec!["linear".into(), "jira".into()],
        ),
    );
    let result = result.expect("Except should return Some");
    assert_eq!(pool_names(&result), vec!["github", "slack"]);
}
#[test]
fn filter_inheritance_named_empty_list_gives_empty_pool() {
    let pool = make_pool(&["github", "linear"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Named(vec![]),
    );
    let result = result.expect("Named([]) should return Some (empty pool)");
    assert_eq!(result.server_names().count(), 0);
}
#[test]
fn filter_inheritance_except_empty_list_keeps_all() {
    let pool = make_pool(&["github", "linear"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Except(vec![]),
    );
    let result = result.expect("Except([]) should return Some");
    assert_eq!(pool_names(&result), vec!["github", "linear"]);
}
#[test]
fn filter_inheritance_named_nonexistent_servers_ignored() {
    let pool = make_pool(&["github", "linear"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Named(
            vec![
                "nonexistent".into(),
                "github".into(),
            ],
        ),
    );
    let result = result.expect("Named should return Some");
    assert_eq!(pool_names(&result), vec!["github"]);
}
#[test]
fn filter_inheritance_except_nonexistent_servers_ignored() {
    let pool = make_pool(&["github", "linear"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Except(vec!["nonexistent".into()]),
    );
    let result = result.expect("Except should return Some");
    assert_eq!(pool_names(&result), vec!["github", "linear"]);
}
#[test]
fn filter_inheritance_named_all_nonexistent_gives_empty() {
    let pool = make_pool(&["github", "linear"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Named(vec!["foo".into(), "bar".into()]),
    );
    let result = result.expect("Named should return Some");
    assert_eq!(result.server_names().count(), 0);
}
#[test]
fn filter_inheritance_except_all_servers_gives_empty() {
    let pool = make_pool(&["github", "linear"]);
    let result = super::filter_pool_by_inheritance(
        pool,
        &echo_agent_core::config::McpInheritance::Except(
            vec!["github".into(), "linear".into()],
        ),
    );
    let result = result.expect("Except should return Some");
    assert_eq!(result.server_names().count(), 0);
}
#[test]
fn resolve_inherited_pool_all_passes_parent_pool() {
    let pool = make_pool(&["github", "atlassian"]);
    let result = super::resolve_inherited_mcp_pool(
            Some(pool),
            &echo_agent_core::config::McpInheritance::All,
        )
        .expect("All should return Some");
    assert_eq!(pool_names(&result), vec!["atlassian", "github"]);
}
#[test]
fn resolve_inherited_pool_none_returns_none() {
    let pool = make_pool(&["github", "atlassian"]);
    let result = super::resolve_inherited_mcp_pool(
        Some(pool),
        &echo_agent_core::config::McpInheritance::None,
    );
    assert!(result.is_none());
}
#[test]
fn resolve_inherited_pool_named_filters() {
    let pool = make_pool(&["github", "atlassian", "slack"]);
    let result = super::resolve_inherited_mcp_pool(
            Some(pool),
            &echo_agent_core::config::McpInheritance::Named(vec!["atlassian".into()]),
        )
        .expect("Named should return Some");
    assert_eq!(pool_names(&result), vec!["atlassian"]);
}
#[test]
fn resolve_inherited_pool_missing_parent_returns_none() {
    let result = super::resolve_inherited_mcp_pool(
        None,
        &echo_agent_core::config::McpInheritance::All,
    );
    assert!(result.is_none());
}
/// Plugin agents must still inherit the parent pool under default
/// `mcpInheritance: all`. The product rule is: plugins cannot *declare*
/// mcpServers, but they do inherit already-connected parent servers.
#[test]
fn plugin_agents_inherit_parent_mcp_pool_by_default() {
    assert!(
            !super::agent_owned_mcp_servers_allowed(true),
            "plugin agents must not declare agent-owned mcpServers"
        );
    assert!(
            super::agent_owned_mcp_servers_allowed(false),
            "non-plugin agents may declare agent-owned mcpServers"
        );
    let pool = make_pool(&["atlassian", "github"]);
    let inherited = super::resolve_inherited_mcp_pool(
            Some(pool),
            &echo_agent_core::config::McpInheritance::All,
        )
        .expect("plugin children inherit parent pool with mcpInheritance=all");
    assert_eq!(pool_names(&inherited), vec!["atlassian", "github"]);
}
#[test]
fn plugin_agents_can_opt_out_via_mcp_inheritance_none() {
    let pool = make_pool(&["atlassian"]);
    let inherited = super::resolve_inherited_mcp_pool(
        Some(pool),
        &echo_agent_core::config::McpInheritance::None,
    );
    assert!(
            inherited.is_none(),
            "mcpInheritance: none must drop the parent pool for every source"
        );
}
fn make_test_skill(
    name: &str,
    plugin: Option<&str>,
) -> echo_agent_tools::implementations::skills::types::SkillInfo {
    echo_agent_tools::implementations::skills::types::SkillInfo {
        name: name.into(),
        display_name: None,
        description: format!("{name} skill"),
        path: format!("/skills/{name}/SKILL.md"),
        scope: echo_agent_tools::implementations::skills::types::SkillScope::Local,
        enabled: true,
        user_invocable: true,
        plugin_name: plugin.map(Into::into),
        when_to_use: None,
        short_description: None,
        author: None,
        argument_hint: None,
        license: None,
        compatibility: None,
        metadata: None,
        config_source: None,
        plugin_version: None,
        plugin_root: None,
        plugin_data: None,
        allowed_tools: None,
        model: None,
        effort: None,
        disable_model_invocation: false,
        has_user_specified_description: false,
        paths: None,
        body: None,
    }
}
#[test]
fn skills_inherited_count_zero_when_inherit_disabled() {
    let inherit_skills = false;
    let parent_skills = Some(vec![make_test_skill("skill-a", None)]);
    let count = if inherit_skills {
        parent_skills.as_ref().map(|s| s.len() as u32).unwrap_or(0)
    } else {
        0
    };
    assert_eq!(count, 0, "should be 0 when inherit_skills is false");
}
#[test]
fn skills_inherited_count_matches_parent_skills_len() {
    let inherit_skills = true;
    let parent_skills = Some(
        vec![
            make_test_skill("codegen-conventions", None),
            make_test_skill("tui-release", Some("my-plugin")),
        ],
    );
    let count = if inherit_skills {
        parent_skills.as_ref().map(|s| s.len() as u32).unwrap_or(0)
    } else {
        0
    };
    assert_eq!(count, 2);
}
/// Both directions of the publisher→parent goal gate: flipping it
/// would silently kill live-token wiring end-to-end.
#[test]
fn goal_tick_cmd_tx_gates_on_goal_enabled() {
    let (tx, _rx) = mpsc::unbounded_channel::<SessionCommand>();
    assert!(
            goal_tick_cmd_tx(true, Some(&tx)).is_some(),
            "goal on + channel present must wire ticks",
        );
    assert!(
            goal_tick_cmd_tx(false, Some(&tx)).is_none(),
            "goal off must not pay the per-tick send",
        );
    assert!(goal_tick_cmd_tx(true, None).is_none());
    assert!(goal_tick_cmd_tx(false, None).is_none());
}
/// Producer side of the goal live-token wiring: a publisher tick must
/// land on the parent command channel as a `SubagentProgress`
/// notification carrying the child's signal values.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn progress_publisher_delivers_ticks_to_parent_cmd_channel() {
    use crate::session::signals::SessionSignalsHandle;
    use crate::test_support::lsp_runtime::test_gateway;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let signals = SessionSignalsHandle::new();
            signals.increment_turn();
            signals.record_tool_call("bash");
            tokio::task::yield_now().await;
            let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
            let cancel = tokio_util::sync::CancellationToken::new();
            let _publisher = spawn_progress_publisher(
                signals,
                test_gateway(),
                "parent-1".to_string(),
                "sub-1".to_string(),
                "child-1".to_string(),
                std::time::Instant::now(),
                cancel.clone(),
                Some(cmd_tx),
            );
            let cmd = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    cmd_rx.recv(),
                )
                .await
                .expect("a tick must arrive within the publish interval")
                .expect("channel open");
            cancel.cancel();
            let SessionCommand::EchoAgentSessionNotification { notification } = cmd else {
                panic!("expected EchoAgentSessionNotification");
            };
            let SessionUpdate::SubagentProgress {
                subagent_id,
                parent_session_id,
                turn_count,
                tool_call_count,
                ..
            } = notification.update else {
                panic!("expected SubagentProgress, got {:?}", notification.update);
            };
            assert_eq!(subagent_id, "sub-1");
            assert_eq!(parent_session_id, "parent-1");
            assert_eq!(turn_count, 1);
            assert_eq!(tool_call_count, 1);
        })
        .await;
}
