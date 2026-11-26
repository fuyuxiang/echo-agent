//! Actor-level tests for the `/context` usage categories: populated rows
//! with counts, compat-harness suppression of the MCP row, and parity
//! between the MCP snapshot and the injected reminder.
use super::support::*;
use super::*;
use crate::session::tool_index::{ServerMetadata, ToolMetadata};
fn mcp_tool(server: &str, tool: &str) -> ToolMetadata {
    ToolMetadata {
        qualified_name: format!("{server}__{tool}"),
        server_name: server.to_string(),
        tool_name: tool.to_string(),
        description: format!("{tool} description"),
        parameters: vec!["arg".to_string()],
        input_schema: serde_json::json!({"type": "object"}),
    }
}
fn install_mcp_servers(actor: &SessionActor) {
    let mut snapshot = actor.tool_metadata_snapshot.lock().unwrap();
    snapshot.tools = vec![mcp_tool("demo", "echo"), mcp_tool("demo", "add")];
    snapshot.servers = vec![ServerMetadata {
        name: "demo".to_string(),
        description: Some("A demo server.".to_string()),
    }];
    snapshot.mcp_initialized = true;
}
async fn seed_skills(actor: &SessionActor, names: &[&str]) {
    let skills = names
        .iter()
        .map(
            |name| echo_agent_tools::implementations::skills::types::SkillInfo {
                name: name.to_string(),
                description: format!("Does {name} things."),
                path: format!("/skills/{name}/SKILL.md"),
                ..Default::default()
            },
        )
        .collect();
    let bridge = actor.tool_bridge_handle();
    bridge
        .seed_skill_discovery(None, None, skills, None, None, None, Default::default())
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn usage_categories_include_skills_and_mcp_with_counts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            seed_skills(&actor, &["alpha", "beta"]).await;
            install_mcp_servers(&actor);
            let rows = actor.usage_categories().await;
            assert_eq!(rows.len(), 2, "{rows:?}");
            let skills = &rows[0];
            assert_eq!(skills.label, "Skills");
            assert_eq!(skills.detail.as_deref(), Some("2 skills"));
            assert!(skills.tokens > 0);
            let mcp = &rows[1];
            assert_eq!(mcp.label, "MCP servers");
            assert_eq!(mcp.detail.as_deref(), Some("1 server"));
            assert!(mcp.tokens > 0);
            let info = actor.build_session_info().await;
            assert_eq!(info.context.usage_categories.len(), 2);
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn usage_categories_include_workflows_when_enabled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            let rows = actor.usage_categories().await;
            let workflows = rows
                .iter()
                .find(|row| row.label == "Workflows")
                .expect("workflows row");
            assert!(workflows.tokens > 0, "{workflows:?}");
            assert!(
                workflows
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("workflow")),
                "{workflows:?}"
            );
            let listing = actor.workflow_listing_for_prompt().expect("listing");
            assert!(listing.contains("deep-research"), "{listing}");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn baseline_reminder_lists_workflows_under_skills() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            seed_skills(&actor, &["commit"]).await;
            let mut conversation = vec![ConversationItem::system("sys")];
            actor
                .inject_baseline_skill_reminder(&mut conversation)
                .await;
            let reminder = conversation
                .iter()
                .find_map(|item| {
                    matches!(
                        item,
                        ConversationItem::User(u)
                            if u.synthetic_reason
                                == Some(echo_agent_sampling_types::SyntheticReason::SystemReminder)
                    )
                    .then(|| item.text_content())
                })
                .expect("baseline reminder");
            let skills_at = reminder
                .find("The following skills are available")
                .expect("skills header");
            let workflows_at = reminder
                .find("The following workflows are available")
                .expect("workflows header");
            assert!(
                skills_at < workflows_at,
                "workflows must sit under skills:\n{reminder}"
            );
            assert!(reminder.contains("commit"), "{reminder}");
            assert!(reminder.contains("deep-research"), "{reminder}");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn baseline_reminder_lists_workflows_when_there_are_no_skills() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            let mut conversation = vec![ConversationItem::system("sys")];
            actor
                .inject_baseline_skill_reminder(&mut conversation)
                .await;
            let reminder = conversation
                .iter()
                .find_map(|item| {
                    matches!(
                        item,
                        ConversationItem::User(u)
                            if u.synthetic_reason
                                == Some(echo_agent_sampling_types::SyntheticReason::SystemReminder)
                    )
                    .then(|| item.text_content())
                })
                .expect("workflow-only reminder");
            assert!(
                reminder.contains("The following workflows are available"),
                "{reminder}"
            );
            assert!(reminder.contains("deep-research"), "{reminder}");
            assert!(
                !reminder.contains("The following skills are available"),
                "{reminder}"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn subagent_session_does_not_list_workflows() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.background_workflows_enabled = true;
            actor.startup_hints.is_subagent = true;
            seed_skills(&actor, &["commit"]).await;
            let mut conversation = vec![ConversationItem::system("sys")];
            actor
                .inject_baseline_skill_reminder(&mut conversation)
                .await;
            let reminder = conversation
                .iter()
                .find_map(|item| {
                    matches!(
                        item,
                        ConversationItem::User(u)
                            if u.synthetic_reason
                                == Some(echo_agent_sampling_types::SyntheticReason::SystemReminder)
                    )
                    .then(|| item.text_content())
                })
                .expect("skill reminder");
            assert!(
                reminder.contains("The following skills are available"),
                "{reminder}"
            );
            assert!(
                !reminder.contains("The following workflows are available"),
                "subagents cannot launch workflows:\n{reminder}"
            );
            assert!(actor.workflow_listing_for_prompt().is_none());
        })
        .await;
}
/// Anti-drift pin for the MCP row: the estimated snapshot must equal the body
/// `maybe_inject_mcp_reminder` injects in `Full` mode, minus the
/// `<system-reminder>` wrapper. Composing the two texts differently (for
/// example, dropping the tool usage hint from one side) fails this test.
#[tokio::test(flavor = "current_thread")]
async fn mcp_snapshot_matches_full_mode_injected_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.mcp_reminder_mode = McpReminderMode::Full;
            install_mcp_servers(&actor);
            let snapshot = actor
                .mcp_announcement_snapshot()
                .await
                .expect("servers installed");
            assert_eq!(snapshot.server_count, 1);
            actor
                .mcp_reminder_dirty
                .store(true, std::sync::atomic::Ordering::Relaxed);
            actor.maybe_inject_mcp_reminder().await;
            let conversation = actor.chat_state_handle.get_conversation().await;
            let injected = conversation
                .last()
                .expect("reminder injected")
                .text_content();
            let body = injected
                .strip_prefix("<system-reminder>\n")
                .and_then(|s| s.strip_suffix("\n</system-reminder>"))
                .unwrap_or_else(|| panic!("unexpected wrapper: {injected}"));
            assert_eq!(body, snapshot.text);
        })
        .await;
}
