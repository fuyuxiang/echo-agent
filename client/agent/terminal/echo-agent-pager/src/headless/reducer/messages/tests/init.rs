//! Init / available-commands / skills projection.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn messages_init_is_deferred_and_carries_tools() {
    let mut r = messages(false);
    assert!(
        r.reduce(StreamEvent::AvailableCommands {
            tools: vec!["read_file".into(), "bash".into()],
            commands: vec!["review".into()],
            skills: Vec::new(),
        })
        .is_empty()
    );
    let out = r.reduce(StreamEvent::AgentMessage("hi".into()));
    assert_eq!(out[0]["type"], "system");
    assert_eq!(out[0]["subtype"], "init");
    assert_eq!(out[0]["model"], "echo-agent-4");
    assert_eq!(out[0]["permissionMode"], "bypassPermissions");
    assert_eq!(out[0]["tools"][0], "read_file");
    assert_eq!(out[0]["slash_commands"][0], "review");
    assert_eq!(out[0]["mcp_servers"][0]["name"], "linear");
    assert_eq!(out[0]["mcp_servers"][0]["status"], "connected");
    assert_eq!(out[0]["apiKeySource"], "user");
    assert!(out[0]["skills"].is_array());
    assert!(out[0]["claude_code_version"].is_null());
    assert!(out[0]["output_style"].is_null());
    assert!(out[0]["plugins"].is_null());
    assert!(
        !r.reduce(StreamEvent::AgentMessage(" there".into()))
            .iter()
            .any(|m| m["type"] == "system")
    );
}

#[test]
fn skill_names_extracts_only_skill_commands() {
    let commands = vec![
        builtin_command("clear"),
        skill_command("pdf"),
        workflow_command("ship-it"),
        skill_command("brainstorm"),
    ];
    assert_eq!(skill_names(&commands), vec!["pdf", "brainstorm"]);
}

#[test]
fn messages_init_carries_real_skills() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AvailableCommands {
        tools: vec!["bash".into()],
        commands: vec!["clear".into(), "pdf".into(), "brainstorm".into()],
        skills: vec!["pdf".into(), "brainstorm".into()],
    });
    let out = r.reduce(StreamEvent::AgentMessage("hi".into()));
    assert_eq!(out[0]["subtype"], "init");
    assert_eq!(out[0]["skills"][0], "pdf");
    assert_eq!(out[0]["skills"][1], "brainstorm");
}

#[test]
fn messages_init_skills_fallback_is_empty() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AvailableCommands {
        tools: vec!["bash".into()],
        commands: vec!["clear".into()],
        skills: Vec::new(),
    });
    let out = r.reduce(StreamEvent::AgentMessage("hi".into()));
    assert_eq!(out[0]["subtype"], "init");
    assert_eq!(out[0]["skills"], json!([]));
}

#[test]
fn messages_init_maps_permission_mode_and_api_key_source() {
    let mut r = MessagesReducer::new();
    r.begin(SessionContext {
        session_id: "s".into(),
        model: None,
        cwd: "/c".into(),
        permission_mode: Some("auto".into()),
        mcp_servers: Vec::new(),
        include_partial_messages: false,
        api_key_auth: false,
        context_window: None,
    });
    let out = r.reduce(StreamEvent::AgentMessage("hi".into()));
    assert_eq!(out[0]["permissionMode"], "default");
    assert_eq!(out[0]["apiKeySource"], "oauth");
    assert!(out[0]["model"].is_string(), "{:?}", out[0]["model"]);
}

#[test]
fn messages_skills_stay_subset_when_later_command_update_is_empty() {
    let mut r = messages(false);
    r.reduce(StreamEvent::AvailableCommands {
        tools: vec!["bash".into()],
        commands: vec!["review".into(), "pdf".into()],
        skills: vec!["pdf".into()],
    });
    r.reduce(StreamEvent::AvailableCommands {
        tools: Vec::new(),
        commands: Vec::new(),
        skills: Vec::new(),
    });
    let out = r.reduce(StreamEvent::AgentMessage("hi".into()));
    let cmds: Vec<String> = out[0]["slash_commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let skills: Vec<String> = out[0]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(skills.contains(&"pdf".to_string()));
    for s in &skills {
        assert!(
            cmds.contains(s),
            "skill {s} escaped slash_commands {cmds:?}"
        );
    }
}
