//! End-to-end serde round-trip tests for every wire-format enum.
//!
//! Every variant of every wire-format enum
//! must JSON-round-trip cleanly. This integration test exercises the
//! entire crate from the outside (only public API, no `pub(crate)`
//! internals), which gives stronger coverage than the per-module tests.
//!
//! Each "rich" sample has at least one variant constructed with
//! non-default fields (meaningful strings, non-zero numbers, `Some(...)`
//! optionals, multi-element vecs, non-default enum members) so that
//! regressions on those fields are caught here, not at the next
//! protocol bug.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use echo_agent_workspace_types::*;

/// JSON round-trip helper. Asserts equality after a serialize +
/// deserialize cycle.
fn round_trip<T>(value: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
{
    let json = serde_json::to_string(&value).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(value, back);
}

/// Fixed timestamp used in tests (avoids non-deterministic `Utc::now`).
fn fixed_ts() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 23, 12, 0, 0).unwrap()
}

#[test]
fn workspace_request_round_trips_for_every_variant() {
    for r in [
        WorkspaceRequest::Tool(ToolRequest::Call(ToolCallArgs {
            session: SessionId::new("s1"),
            tool_name: "read_file".into(),
            input_json: r#"{"path":"/etc/hosts"}"#.into(),
            call_id: ToolCallId::new("c1"),
        })),
        WorkspaceRequest::Tool(ToolRequest::Definitions),
        WorkspaceRequest::Ops(WorkspaceOpsRequest::ListHunks),
        WorkspaceRequest::Session(SessionLifecycleRequest::List),
    ] {
        round_trip(r);
    }
}

#[test]
fn ops_request_round_trips_for_every_variant_including_rich_payloads() {
    for r in [
        // Rich GitStatusOpts: every bool flipped on.
        WorkspaceOpsRequest::GitStatus(GitStatusOpts {
            include_untracked: true,
            include_ignored: true,
        }),
        // Rich GitDiffArgs: range + paths + staged.
        WorkspaceOpsRequest::GitDiff(GitDiffArgs {
            range: Some("main..HEAD".into()),
            paths: vec!["src/lib.rs".into(), "src/main.rs".into()],
            staged: true,
        }),
        WorkspaceOpsRequest::GitBranchInfo,
        WorkspaceOpsRequest::GitMetadata,
        WorkspaceOpsRequest::ListHunks,
        WorkspaceOpsRequest::ActOnHunk(HunkAction::Reject {
            hunk_id: HunkId::new("h"),
        }),
        // Rich RipgrepArgs: pattern + cwd + globs + case_insensitive +
        // max_matches.
        WorkspaceOpsRequest::Ripgrep(RipgrepArgs {
            pattern: "TODO".into(),
            cwd: Some("src".into()),
            globs: vec!["*.rs".into(), "!target/**".into()],
            case_insensitive: true,
            max_matches: Some(100),
        }),
        WorkspaceOpsRequest::FuzzySearch(FuzzySearchArgs {
            query: "main".into(),
            cwd: Some("src".into()),
            limit: Some(50),
        }),
        WorkspaceOpsRequest::DiscoverSkills,
        WorkspaceOpsRequest::DiscoverPlugins,
        WorkspaceOpsRequest::LoadProjectConfig,
        WorkspaceOpsRequest::LoadPermissions,
        WorkspaceOpsRequest::LoadEnvrc,
        WorkspaceOpsRequest::ResolveFileRefs(vec!["@x".into(), "@docs/AGENTS.md".into()]),
        WorkspaceOpsRequest::MemorySearch {
            query: "auth middleware patterns".into(),
            limit: 5,
        },
        WorkspaceOpsRequest::MemoryWrite("note body".into()),
        WorkspaceOpsRequest::InstallPlugin("https://example".into()),
        WorkspaceOpsRequest::RefreshPlugins,
    ] {
        round_trip(r);
    }
}

#[test]
fn session_request_round_trips_for_every_variant_including_rich_payloads() {
    let mut env = BTreeMap::new();
    env.insert("FOO".to_string(), "1".to_string());
    env.insert("BAR".to_string(), "2".to_string());
    let mut tool_args = BTreeMap::new();
    tool_args.insert("k".to_string(), "v".to_string());
    let rich_cfg = AgentSessionConfig {
        agent_id: "subagent-explore".into(),
        isolation: IsolationMode::Sandbox,
        capability_mode: CapabilityMode::ReadOnly,
        tool_config: vec![ToolServerConfig {
            id: "fs".into(),
            enabled: true,
            command: Some("/usr/bin/fs-mcp".into()),
            args: tool_args,
        }],
        max_depth: 3,
        cwd_override: Some("/tmp/work".into()),
        extra_env: env,
    };

    for r in [
        SessionLifecycleRequest::Fork(rich_cfg.clone()),
        SessionLifecycleRequest::Destroy(SessionId::new("s")),
        SessionLifecycleRequest::List,
        SessionLifecycleRequest::ApplyWorktree(SessionId::new("s")),
        SessionLifecycleRequest::BeginPrompt {
            session: SessionId::new("s"),
            idx: 7,
        },
        SessionLifecycleRequest::EndPrompt {
            session: SessionId::new("s"),
            idx: 7,
        },
        SessionLifecycleRequest::Rewind {
            session: SessionId::new("s"),
            target: 1,
        },
        SessionLifecycleRequest::GetRewindPoints(SessionId::new("s")),
    ] {
        round_trip(r);
    }
}

#[test]
fn tool_chunk_round_trips_for_every_variant_including_rich_payloads() {
    let rich_perm_request = PermissionRequest {
        tool_name: "run_terminal_cmd".into(),
        summary: "rm -rf /tmp/scratch".into(),
        input_json: r#"{"cmd":"rm -rf /tmp/scratch"}"#.into(),
        destructive: true,
    };
    let rich_questions = vec![
        UserQuestion {
            question: "Pick a color?".into(),
            options: vec![
                UserQuestionOption {
                    label: "Red".into(),
                    description: "warm".into(),
                    preview: Some("```css\ncolor: red;\n```".into()),
                },
                UserQuestionOption {
                    label: "Blue".into(),
                    description: "cool".into(),
                    preview: None,
                },
            ],
            multi_select: false,
        },
        UserQuestion {
            question: "Pick toppings".into(),
            options: vec![
                UserQuestionOption {
                    label: "Cheese".into(),
                    description: "extra".into(),
                    preview: None,
                },
                UserQuestionOption {
                    label: "Olives".into(),
                    description: "kalamata".into(),
                    preview: None,
                },
            ],
            multi_select: true,
        },
    ];
    for c in [
        ToolChunk::Output(ToolOutputChunk {
            call_id: ToolCallId::new("c"),
            stream: "stdout".into(),
            bytes: (0u8..=255).collect(),
            at: fixed_ts(),
        }),
        ToolChunk::Progress(ToolProgress::Started {
            call_id: ToolCallId::new("c"),
        }),
        ToolChunk::Progress(ToolProgress::Status {
            call_id: ToolCallId::new("c"),
            message: "running".into(),
        }),
        ToolChunk::Progress(ToolProgress::Percent {
            call_id: ToolCallId::new("c"),
            fraction: 0.5,
        }),
        ToolChunk::Final(ToolCallResult {
            call_id: ToolCallId::new("c"),
            exit_code: 42,
            summary: "ok".into(),
            output_json: r#"{"x":1}"#.into(),
            cancelled: false,
        }),
        ToolChunk::Definitions(vec![
            ToolDef {
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema_json: r#"{"type":"object"}"#.into(),
                requires_permission: false,
            },
            ToolDef::default(),
        ]),
        ToolChunk::NeedPermission {
            req_id: "perm-1".into(),
            request: rich_perm_request,
        },
        ToolChunk::NeedUserAnswer {
            req_id: "q-1".into(),
            questions: rich_questions,
        },
        ToolChunk::NeedPlanModeChange {
            req_id: "pm-1".into(),
            transition: PlanModeTransition::Enter {
                plan: Some("# Plan\n1. read files\n2. design\n".into()),
            },
        },
        ToolChunk::NeedPlanModeChange {
            req_id: "pm-2".into(),
            transition: PlanModeTransition::Enter { plan: None },
        },
        ToolChunk::NeedPlanModeChange {
            req_id: "pm-3".into(),
            transition: PlanModeTransition::Exit {
                final_plan: Some("# Final plan\n- step 1\n- step 2\n".into()),
            },
        },
        ToolChunk::NeedPlanModeChange {
            req_id: "pm-4".into(),
            transition: PlanModeTransition::Exit { final_plan: None },
        },
    ] {
        round_trip(c);
    }
}

#[test]
fn tool_response_round_trips_for_every_variant_including_rich_payloads() {
    for r in [
        ToolResponse::Permission {
            req_id: "perm-1".into(),
            decision: PermissionDecision::AllowOnce,
        },
        ToolResponse::Permission {
            req_id: "perm-2".into(),
            decision: PermissionDecision::AllowSession,
        },
        ToolResponse::Permission {
            req_id: "perm-3".into(),
            decision: PermissionDecision::AllowProject,
        },
        ToolResponse::Permission {
            req_id: "perm-4".into(),
            decision: PermissionDecision::Deny {
                reason: "no thank you".into(),
            },
        },
        ToolResponse::UserAnswer {
            req_id: "q-1".into(),
            answers: vec![UserAnswer::Selected("Red".into())],
        },
        ToolResponse::UserAnswer {
            req_id: "q-2".into(),
            answers: vec![UserAnswer::Other("freeform answer".into())],
        },
        ToolResponse::UserAnswer {
            req_id: "q-3".into(),
            answers: vec![UserAnswer::Multiple(vec!["Cheese".into(), "Olives".into()])],
        },
        ToolResponse::PlanModeChange {
            req_id: "pm-1".into(),
            decision: PlanModeDecision::Approve,
        },
        ToolResponse::PlanModeChange {
            req_id: "pm-2".into(),
            decision: PlanModeDecision::Reject {
                feedback: Some("read more first".into()),
            },
        },
        ToolResponse::PlanModeChange {
            req_id: "pm-3".into(),
            decision: PlanModeDecision::Reject { feedback: None },
        },
        ToolResponse::PlanModeChange {
            req_id: "pm-4".into(),
            decision: PlanModeDecision::Defer,
        },
    ] {
        round_trip(r);
    }
}

#[test]
fn user_question_round_trips_standalone() {
    let q = UserQuestion {
        question: "Confirm?".into(),
        options: vec![
            UserQuestionOption {
                label: "Yes".into(),
                description: "Proceed".into(),
                preview: Some("# preview".into()),
            },
            UserQuestionOption {
                label: "No".into(),
                description: "Abort".into(),
                preview: None,
            },
        ],
        multi_select: true,
    };
    round_trip(q);
}

#[test]
fn ops_chunk_round_trips_for_every_variant_including_rich_payloads() {
    let rich_status = GitStatus {
        branch: "main".into(),
        head_commit: "deadbeef".into(),
        root: "/repo".into(),
        staged: vec!["a".into()],
        unstaged: vec!["b".into()],
        untracked: vec!["c".into()],
        clean: false,
        vcs: VcsKind::Jj,
    };
    let rich_diff = GitDiff {
        patch: "@@ -1 +1 @@\n-a\n+b".into(),
        files: vec!["src/lib.rs".into()],
    };
    let rich_branch = GitBranchInfo {
        current: Some("main".into()),
        local: vec!["main".into(), "dev".into()],
        upstream: Some("origin/main".into()),
    };
    let rich_meta = GitMetadata {
        origin_url: Some("git@github.com:org/repo.git".into()),
        root: "/repo".into(),
        default_branch: Some("main".into()),
        vcs: VcsKind::Git,
    };
    let rich_hunk = Hunk {
        id: HunkId::new("h1"),
        path: "src/lib.rs".into(),
        added: 5,
        removed: 2,
        start_line: 12,
        summary: "add hello".into(),
    };
    let rich_skill = SkillInfo {
        id: "review".into(),
        display_name: "Code Review".into(),
        description: "perform code review".into(),
        path: "/skills/review/SKILL.md".into(),
        source: "global".into(),
    };
    let rich_plugin = PluginInfo {
        id: "sample-plugin".into(),
        name: "Sample Plugin".into(),
        version: "1.2.3".into(),
        path: "/plugins/sample-plugin".into(),
        source: "marketplace".into(),
        enabled: true,
    };
    let rich_project = ProjectConfig {
        values: BTreeMap::from([("a".into(), "1".into())]),
        trusted: true,
    };
    let rich_perms = PermissionPolicy {
        allow: vec!["read_file".into()],
        deny: vec!["run_terminal_cmd".into()],
        ask: vec!["edit_file".into()],
    };
    let envrc = BTreeMap::from([("FOO".to_string(), "1".to_string())]);
    let rich_resolved = ResolvedFile {
        reference: "@README.md".into(),
        path: "/repo/README.md".into(),
        resolved: true,
        preview: Some("# Repo".into()),
        error: None,
    };
    let rich_memory = MemoryChunk {
        id: "m1".into(),
        content: "auth uses JWT".into(),
        source: Some("/memory/auth.md".into()),
        score: Some(0.95),
    };
    let rich_fuzzy = FuzzyMatch {
        path: "src/main.rs".into(),
        score: 100,
        matched_indices: vec![0, 1, 2],
    };
    let rich_hit = ContentMatch {
        path: "src/lib.rs".into(),
        line_number: 12,
        line: "// TODO: fix".into(),
        spans: vec![],
    };
    let rich_stats = RipgrepStats {
        files_matched: 3,
        lines_matched: 10,
        truncated: false,
    };

    for c in [
        OpsChunk::GitStatus(rich_status),
        OpsChunk::GitDiff(rich_diff),
        OpsChunk::GitBranchInfo(rich_branch),
        OpsChunk::GitMetadata(None),
        OpsChunk::GitMetadata(Some(rich_meta)),
        OpsChunk::Hunks(vec![rich_hunk]),
        OpsChunk::Skills(vec![rich_skill]),
        OpsChunk::Plugins(vec![rich_plugin.clone()]),
        OpsChunk::ProjectConfig(rich_project),
        OpsChunk::Permissions(rich_perms),
        OpsChunk::Envrc(envrc),
        OpsChunk::ResolvedFiles(vec![rich_resolved]),
        OpsChunk::MemoryChunks(vec![rich_memory]),
        OpsChunk::Plugin(rich_plugin),
        OpsChunk::Ack,
        OpsChunk::FuzzyMatch(rich_fuzzy),
        OpsChunk::RipgrepHit(rich_hit),
        OpsChunk::RipgrepDone(rich_stats),
    ] {
        round_trip(c);
    }
}

#[test]
fn session_chunk_round_trips_for_every_variant_including_rich_payloads() {
    let rich_info = AgentSessionInfo {
        id: SessionId::new("s1"),
        parent: Some(SessionId::new("p1")),
        agent_id: "subagent-explore".into(),
        isolation: IsolationMode::Worktree,
        created_at: fixed_ts(),
    };
    let rich_rewind = RewindResult {
        session: SessionId::new("s1"),
        head_prompt_index: 5,
        prompts_dropped: 2,
    };
    let rich_point = RewindPoint {
        prompt_index: 3,
        at: fixed_ts(),
        summary: "prompt 3".into(),
    };
    for c in [
        SessionChunk::SessionId(SessionId::new("s")),
        SessionChunk::SessionInfo(rich_info),
        SessionChunk::RewindResult(rich_rewind),
        SessionChunk::RewindPoints(vec![rich_point]),
        SessionChunk::Ack,
    ] {
        round_trip(c);
    }
}

#[test]
fn workspace_event_round_trips_for_every_variant() {
    let events = vec![
        WorkspaceEvent::FsChanged {
            path: PathBuf::from("/a"),
            kind: FsEventKind::Created,
        },
        WorkspaceEvent::FsChanged {
            path: PathBuf::from("/a"),
            kind: FsEventKind::Modified,
        },
        WorkspaceEvent::FsChanged {
            path: PathBuf::from("/a"),
            kind: FsEventKind::Removed,
        },
        WorkspaceEvent::FsChanged {
            path: PathBuf::from("/a"),
            kind: FsEventKind::Renamed,
        },
        WorkspaceEvent::GitHeadChanged {
            commit: "abc".into(),
            branch: None,
            vcs: VcsKind::Git,
        },
        WorkspaceEvent::GitHeadChanged {
            commit: "abc".into(),
            branch: Some("main".into()),
            vcs: VcsKind::Jj,
        },
        WorkspaceEvent::GitLockHeld { until: fixed_ts() },
        WorkspaceEvent::SkillsChanged {
            added: vec![SkillInfo {
                id: "s".into(),
                ..Default::default()
            }],
            removed: vec!["x".into()],
        },
        WorkspaceEvent::PluginsChanged {
            plugins: vec![PluginInfo::default()],
            project_trusted: true,
        },
        WorkspaceEvent::HooksChanged {
            hooks: vec![HookInfo::default()],
            project_trusted: false,
        },
        WorkspaceEvent::McpServerStateChanged {
            server: "fs".into(),
            status: McpServerStatus::Stopped,
        },
        WorkspaceEvent::LspServerStateChanged {
            server: "rust".into(),
            status: LspServerStatus::Failed,
        },
        WorkspaceEvent::CodebaseIndexUpdated { files_indexed: 99 },
        WorkspaceEvent::ProjectConfigChanged,
        WorkspaceEvent::PermissionPolicyChanged,
        WorkspaceEvent::ToolsChanged {
            session_id: "session-7".into(),
        },
    ];
    for ev in events {
        round_trip(ev);
    }
}

#[test]
fn workspace_error_round_trips_for_every_variant() {
    let errs = vec![
        WorkspaceError::Io {
            message: "x".into(),
            kind: IoKind::PermissionDenied,
        },
        WorkspaceError::Vcs("oops".into()),
        WorkspaceError::Permission {
            reason: "no".into(),
        },
        WorkspaceError::NotFound("/x".into()),
        WorkspaceError::Cancelled,
        WorkspaceError::Timeout { elapsed_ms: 10 },
        WorkspaceError::SessionNotFound(SessionId::new("s")),
        WorkspaceError::Tool {
            code: "c".into(),
            message: "m".into(),
        },
        WorkspaceError::Remote("x".into()),
        WorkspaceError::ProtocolMismatch {
            expected: "GitStatus".into(),
            got: ChunkKind::Ack,
        },
        WorkspaceError::ProtocolViolation("x".into()),
        WorkspaceError::EmptyStream,
        WorkspaceError::Internal("x".into()),
    ];
    for err in errs {
        round_trip(err);
    }
}

#[test]
fn event_lag_round_trips() {
    round_trip(EventLag::Lagged(0));
    round_trip(EventLag::Lagged(1_000_000));
}

#[test]
fn standard_metadata_keys_are_unique() {
    use std::collections::HashSet;
    let set: HashSet<&&str> = STANDARD_META_KEYS.iter().collect();
    assert_eq!(set.len(), STANDARD_META_KEYS.len());
}

#[test]
fn workspace_error_display_renders_for_every_variant() {
    let errs = vec![
        WorkspaceError::Io {
            message: "x".into(),
            kind: IoKind::NotFound,
        },
        WorkspaceError::Vcs("x".into()),
        WorkspaceError::Permission { reason: "x".into() },
        WorkspaceError::NotFound("/x".into()),
        WorkspaceError::Cancelled,
        WorkspaceError::Timeout { elapsed_ms: 1 },
        WorkspaceError::SessionNotFound(SessionId::new("s")),
        WorkspaceError::Tool {
            code: "c".into(),
            message: "m".into(),
        },
        WorkspaceError::Remote("x".into()),
        WorkspaceError::ProtocolMismatch {
            expected: "X".into(),
            got: ChunkKind::Ack,
        },
        WorkspaceError::ProtocolViolation("x".into()),
        WorkspaceError::EmptyStream,
        WorkspaceError::Internal("x".into()),
    ];
    for err in errs {
        let s = err.to_string();
        assert!(!s.is_empty(), "Display empty for {err:?}");
    }
}

// ---------------------------------------------------------------------------
// JSON-shape assertions: lock down the wire field
// names so any future drift back to camelCase fails loudly.
// ---------------------------------------------------------------------------

#[test]
fn tool_call_args_uses_snake_case_field_names() {
    let args = ToolCallArgs {
        session: SessionId::new("s"),
        tool_name: "n".into(),
        input_json: "{}".into(),
        call_id: ToolCallId::new("c"),
    };
    let json = serde_json::to_string(&args).unwrap();
    assert!(json.contains("\"tool_name\""), "got {json}");
    assert!(json.contains("\"input_json\""), "got {json}");
    assert!(json.contains("\"call_id\""), "got {json}");
    assert!(!json.contains("\"toolName\""), "got {json}");
    assert!(!json.contains("\"inputJson\""), "got {json}");
    assert!(!json.contains("\"callId\""), "got {json}");
}

#[test]
fn ops_request_inline_struct_variants_use_snake_case() {
    // Use MemorySearch (an inline-struct variant with a non-trivial
    // field name) to lock down snake_case rendering for ops-request
    // inline-struct variants in general.
    let req = WorkspaceOpsRequest::MemorySearch {
        query: "auth".into(),
        limit: 5,
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"query\""), "got {json}");
    assert!(json.contains("\"limit\""), "got {json}");
}

#[test]
fn session_request_inline_struct_variants_use_snake_case() {
    let req = SessionLifecycleRequest::BeginPrompt {
        session: SessionId::new("s"),
        idx: 3,
    };
    let json = serde_json::to_string(&req).unwrap();
    // The field name is `session`, but assert the explicit shape:
    assert!(json.contains("\"session\""), "got {json}");
    assert!(json.contains("\"idx\""), "got {json}");
}

#[test]
fn workspace_event_inline_struct_variants_use_snake_case() {
    // CodebaseIndexUpdated has a snake_case-able field (`files_indexed`)
    // and is one of the remaining inline-struct WorkspaceEvent variants
    // after the hunk events were removed.
    let ev = WorkspaceEvent::CodebaseIndexUpdated { files_indexed: 7 };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"files_indexed\""), "got {json}");
    assert!(!json.contains("\"filesIndexed\""), "got {json}");
}

#[test]
fn workspace_request_envelope_uses_adjacent_tag() {
    let req = WorkspaceRequest::Ops(WorkspaceOpsRequest::ListHunks);
    let json = serde_json::to_string(&req).unwrap();
    // Adjacent tagging: `{"type":"ops","data":{"type":"list_hunks"}}`
    assert!(json.starts_with(r#"{"type":"ops","data":"#), "got {json}");
}

#[test]
fn permission_decision_uses_adjacent_type_data_tag() {
    // Variant with payload: data wrapped under "data".
    let deny = PermissionDecision::Deny {
        reason: "no".into(),
    };
    let json = serde_json::to_string(&deny).unwrap();
    assert_eq!(json, r#"{"type":"deny","data":{"reason":"no"}}"#);
    // Tag key must be `type` (not the legacy `decision`).
    assert!(!json.starts_with(r#"{"decision":"#), "got {json}");
    // Variant payload must NOT be inlined alongside `type` (would
    // indicate a regression to internal tagging).
    assert!(!json.contains(r#""type":"deny","reason":"#), "got {json}");

    // Unit variant: no `data` field.
    let allow = PermissionDecision::AllowOnce;
    let json = serde_json::to_string(&allow).unwrap();
    assert_eq!(json, r#"{"type":"allow_once"}"#);
}

#[test]
fn plan_mode_decision_uses_adjacent_type_data_tag() {
    // Variant with payload: data wrapped under "data".
    let reject = PlanModeDecision::Reject {
        feedback: Some("not yet".into()),
    };
    let json = serde_json::to_string(&reject).unwrap();
    assert_eq!(json, r#"{"type":"reject","data":{"feedback":"not yet"}}"#);
    // Tag key must be `type` (not the legacy `decision`).
    assert!(!json.starts_with(r#"{"decision":"#), "got {json}");
    // Variant payload must NOT be inlined alongside `type` (would
    // indicate a regression to internal tagging).
    assert!(
        !json.contains(r#""type":"reject","feedback":"#),
        "got {json}"
    );

    // Unit variants: no `data` field.
    let approve = PlanModeDecision::Approve;
    let json = serde_json::to_string(&approve).unwrap();
    assert_eq!(json, r#"{"type":"approve"}"#);
    let defer = PlanModeDecision::Defer;
    let json = serde_json::to_string(&defer).unwrap();
    assert_eq!(json, r#"{"type":"defer"}"#);
}

#[test]
fn plan_mode_transition_uses_adjacent_type_data_tag() {
    let enter = PlanModeTransition::Enter {
        plan: Some("draft".into()),
    };
    let json = serde_json::to_string(&enter).unwrap();
    assert_eq!(json, r#"{"type":"enter","data":{"plan":"draft"}}"#);
    assert!(!json.contains(r#""type":"enter","plan":"#), "got {json}");

    let exit = PlanModeTransition::Exit { final_plan: None };
    let json = serde_json::to_string(&exit).unwrap();
    assert_eq!(json, r#"{"type":"exit","data":{"final_plan":null}}"#);
    // Snake-case field guard.
    assert!(json.contains("\"final_plan\""), "got {json}");
    assert!(!json.contains("\"finalPlan\""), "got {json}");
}

#[test]
fn hunk_action_uses_adjacent_type_data_tag() {
    let h = HunkAction::Accept {
        hunk_id: HunkId::new("h"),
    };
    let json = serde_json::to_string(&h).unwrap();
    assert_eq!(json, r#"{"type":"accept","data":{"hunk_id":"h"}}"#);
    assert!(!json.starts_with(r#"{"action":"#), "got {json}");
    assert!(
        !json.contains(r#""type":"accept","hunk_id":"#),
        "got {json}"
    );
}

#[test]
fn tool_progress_uses_adjacent_type_data_tag() {
    // Variant with a single field: payload wrapped under "data".
    let started = ToolProgress::Started {
        call_id: ToolCallId::new("c1"),
    };
    let json = serde_json::to_string(&started).unwrap();
    assert_eq!(json, r#"{"type":"started","data":{"call_id":"c1"}}"#);
    // Payload must NOT be inlined alongside "type" (would indicate a
    // regression to internal tagging).
    assert!(
        !json.contains(r#""type":"started","call_id":"#),
        "got {json}"
    );

    // Variant with multiple fields including an f32: still adjacent.
    let percent = ToolProgress::Percent {
        call_id: ToolCallId::new("c1"),
        fraction: 0.5,
    };
    let json = serde_json::to_string(&percent).unwrap();
    assert_eq!(
        json,
        r#"{"type":"percent","data":{"call_id":"c1","fraction":0.5}}"#
    );
    assert!(
        !json.contains(r#""type":"percent","call_id":"#),
        "got {json}"
    );

    // Lock down the "no two tagging styles in one document" property:
    // ToolChunk::Progress(ToolProgress::Percent { ... }) should be a
    // single uniform shape end-to-end.
    let chunk = ToolChunk::Progress(percent);
    let json = serde_json::to_string(&chunk).unwrap();
    assert_eq!(
        json,
        r#"{"type":"progress","data":{"type":"percent","data":{"call_id":"c1","fraction":0.5}}}"#
    );
}

#[test]
fn tool_chunk_need_permission_uses_adjacent_type_data_tag() {
    // NeedPermission is the new bidi-stream variant carrying the
    // workspace's permission ask. Lock down the snake_case tag
    // ("need_permission"), the snake_case field name ("req_id"), and
    // adjacent tagging.
    let chunk = ToolChunk::NeedPermission {
        req_id: "perm-1".into(),
        request: PermissionRequest {
            tool_name: "rm".into(),
            summary: "deletes a file".into(),
            input_json: r#"{"path":"/tmp/x"}"#.into(),
            destructive: true,
        },
    };
    let json = serde_json::to_string(&chunk).unwrap();
    assert_eq!(
        json,
        r#"{"type":"need_permission","data":{"req_id":"perm-1","request":{"tool_name":"rm","summary":"deletes a file","input_json":"{\"path\":\"/tmp/x\"}","destructive":true}}}"#
    );
    // Snake_case field-name guard.
    assert!(json.contains("\"req_id\""), "got {json}");
    assert!(!json.contains("\"reqId\""), "got {json}");
    // Payload must NOT be inlined alongside "type" (would indicate a
    // regression to internal tagging).
    assert!(
        !json.contains(r#""type":"need_permission","req_id":"#),
        "got {json}"
    );
}

#[test]
fn tool_chunk_need_user_answer_uses_adjacent_type_data_tag() {
    let chunk = ToolChunk::NeedUserAnswer {
        req_id: "q-1".into(),
        questions: vec![UserQuestion {
            question: "Pick?".into(),
            options: vec![UserQuestionOption {
                label: "A".into(),
                description: "first".into(),
                preview: None,
            }],
            multi_select: false,
        }],
    };
    let json = serde_json::to_string(&chunk).unwrap();
    // Snake_case discriminator + snake_case fields.
    assert!(json.starts_with(r#"{"type":"need_user_answer","data":{"req_id":"q-1","questions":["#));
    assert!(json.contains("\"multi_select\""), "got {json}");
    assert!(!json.contains("\"multiSelect\""), "got {json}");
    // Adjacent tagging guard.
    assert!(
        !json.contains(r#""type":"need_user_answer","req_id":"#),
        "got {json}"
    );
}

#[test]
fn tool_response_user_answer_uses_adjacent_type_data_tag() {
    let resp = ToolResponse::UserAnswer {
        req_id: "q-1".into(),
        answers: vec![UserAnswer::Selected("A".into())],
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert_eq!(
        json,
        r#"{"type":"user_answer","data":{"req_id":"q-1","answers":[{"type":"selected","data":"A"}]}}"#
    );
    // Snake_case field guard.
    assert!(json.contains("\"req_id\""), "got {json}");
    assert!(!json.contains("\"reqId\""), "got {json}");
    // Adjacent tagging guard.
    assert!(
        !json.contains(r#""type":"user_answer","req_id":"#),
        "got {json}"
    );
}

#[test]
fn tool_response_permission_uses_adjacent_type_data_tag() {
    let resp = ToolResponse::Permission {
        req_id: "perm-1".into(),
        decision: PermissionDecision::AllowOnce,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert_eq!(
        json,
        r#"{"type":"permission","data":{"req_id":"perm-1","decision":{"type":"allow_once"}}}"#
    );
}

#[test]
fn tool_chunk_need_plan_mode_change_uses_adjacent_type_data_tag() {
    // NeedPlanModeChange is the bidi-stream variant carrying the
    // workspace's plan-mode ask. Lock down the snake_case tag
    // ("need_plan_mode_change"), the snake_case field name ("req_id"),
    // and adjacent tagging.
    let chunk = ToolChunk::NeedPlanModeChange {
        req_id: "pm-1".into(),
        transition: PlanModeTransition::Enter {
            plan: Some("draft".into()),
        },
    };
    let json = serde_json::to_string(&chunk).unwrap();
    assert_eq!(
        json,
        r#"{"type":"need_plan_mode_change","data":{"req_id":"pm-1","transition":{"type":"enter","data":{"plan":"draft"}}}}"#
    );
    // Snake_case field-name guard.
    assert!(json.contains("\"req_id\""), "got {json}");
    assert!(!json.contains("\"reqId\""), "got {json}");
    // Payload must NOT be inlined alongside "type" (would indicate a
    // regression to internal tagging).
    assert!(
        !json.contains(r#""type":"need_plan_mode_change","req_id":"#),
        "got {json}"
    );
}

#[test]
fn tool_response_plan_mode_change_uses_adjacent_type_data_tag() {
    let resp = ToolResponse::PlanModeChange {
        req_id: "pm-1".into(),
        decision: PlanModeDecision::Approve,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert_eq!(
        json,
        r#"{"type":"plan_mode_change","data":{"req_id":"pm-1","decision":{"type":"approve"}}}"#
    );
    // Snake_case field guard.
    assert!(json.contains("\"req_id\""), "got {json}");
    assert!(!json.contains("\"reqId\""), "got {json}");
    // Adjacent tagging guard.
    assert!(
        !json.contains(r#""type":"plan_mode_change","req_id":"#),
        "got {json}"
    );

    // Payload variant with an inner option.
    let resp = ToolResponse::PlanModeChange {
        req_id: "pm-2".into(),
        decision: PlanModeDecision::Reject {
            feedback: Some("not yet".into()),
        },
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert_eq!(
        json,
        r#"{"type":"plan_mode_change","data":{"req_id":"pm-2","decision":{"type":"reject","data":{"feedback":"not yet"}}}}"#
    );
}
