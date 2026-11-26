use super::*;

fn make_snapshot(tools: Vec<ToolMetadata>) -> Arc<Mutex<ToolMetadataSnapshot>> {
    make_snapshot_with_servers(tools, vec![])
}

fn make_snapshot_with_servers(
    tools: Vec<ToolMetadata>,
    servers: Vec<ServerMetadata>,
) -> Arc<Mutex<ToolMetadataSnapshot>> {
    Arc::new(Mutex::new(ToolMetadataSnapshot {
        tools,
        servers,
        mcp_initialized: true,
    }))
}

fn linear_tools() -> Vec<ToolMetadata> {
    vec![
        ToolMetadata {
            qualified_name: "linear__save_issue".into(),
            server_name: "linear".into(),
            tool_name: "save_issue".into(),
            description: "Create or update a Linear issue".into(),
            parameters: vec![
                "title".into(),
                "team".into(),
                "description".into(),
                "assignee".into(),
                "priority".into(),
                "labels".into(),
                "project".into(),
            ],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "linear__list_issues".into(),
            server_name: "linear".into(),
            tool_name: "list_issues".into(),
            description: "List issues in the user's Linear workspace".into(),
            parameters: vec![
                "assignee".into(),
                "project".into(),
                "state".into(),
                "team".into(),
                "query".into(),
            ],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "linear__get_issue".into(),
            server_name: "linear".into(),
            tool_name: "get_issue".into(),
            description: "Retrieve detailed information about an issue by ID".into(),
            parameters: vec!["id".into()],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "linear__list_projects".into(),
            server_name: "linear".into(),
            tool_name: "list_projects".into(),
            description: "List projects in the user's Linear workspace".into(),
            parameters: vec!["query".into(), "team".into()],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "demo-mcp__sendMessage".into(),
            server_name: "demo-mcp".into(),
            tool_name: "sendMessage".into(),
            description: "Send a message in a Slack channel".into(),
            parameters: vec!["channel".into(), "text".into()],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "demo-mcp__readSlackThread".into(),
            server_name: "demo-mcp".into(),
            tool_name: "readSlackThread".into(),
            description: "Read a Slack thread history".into(),
            parameters: vec!["channel".into(), "thread_ts".into()],
            input_schema: serde_json::json!({}),
        },
    ]
}

#[test]
fn search_create_linear_issue() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(linear_tools()));
    let snap = index.search_snapshot("create linear issue", 3);
    assert!(!snap.results.is_empty());
    assert_eq!(snap.results[0].tool_name, "linear__save_issue");
    assert!(snap.is_ready);
}

#[test]
fn search_read_slack_thread() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(linear_tools()));
    let snap = index.search_snapshot("read slack thread", 3);
    assert!(!snap.results.is_empty());
    assert_eq!(snap.results[0].tool_name, "demo-mcp__readSlackThread");
}

#[test]
fn search_list_issues() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(linear_tools()));
    let snap = index.search_snapshot("list my issues", 3);
    assert!(!snap.results.is_empty());
    assert_eq!(snap.results[0].tool_name, "linear__list_issues");
}

#[test]
fn search_empty_index() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(vec![]));
    let snap = index.search_snapshot("anything", 5);
    assert!(snap.results.is_empty());
    assert_eq!(snap.total_hidden_tools, 0);
}

#[test]
fn search_no_match_returns_empty() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(linear_tools()));
    let snap = index.search_snapshot("xyzzy_nonexistent_gibberish", 5);
    for r in &snap.results {
        assert!(r.score >= 0.0);
    }
}

#[test]
fn total_hidden_tools_in_snapshot() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(linear_tools()));
    let snap = index.search_snapshot("anything", 1);
    assert_eq!(snap.total_hidden_tools, 6);
}

#[test]
fn search_surface_disambiguation() {
    let mut tools = linear_tools();
    tools.push(ToolMetadata {
        qualified_name: "demo-mcp__askQuestion".into(),
        server_name: "demo-mcp".into(),
        tool_name: "askQuestion".into(),
        description: "Ask the user a question in the UI".into(),
        parameters: vec!["question".into()],
        input_schema: serde_json::json!({}),
    });
    tools.push(ToolMetadata {
        qualified_name: "demo-mcp__slackAskQuestion".into(),
        server_name: "demo-mcp".into(),
        tool_name: "slackAskQuestion".into(),
        description: "Ask a question in a Slack channel".into(),
        parameters: vec!["question".into(), "channel".into()],
        input_schema: serde_json::json!({}),
    });
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));
    let snap = index.search_snapshot("ask user a question", 3);
    let names: Vec<&str> = snap.results.iter().map(|r| r.tool_name.as_str()).collect();
    assert!(
        names.contains(&"demo-mcp__askQuestion") || names.contains(&"demo-mcp__slackAskQuestion"),
        "expected at least one ask tool in top 3, got: {names:?}"
    );
}

#[test]
fn search_deploy_graceful_empty() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(linear_tools()));
    let snap = index.search_snapshot("deploy", 5);
    // "deploy" doesn't match any tool descriptions — should return low/no results
    // without panicking
    assert!(snap.total_hidden_tools > 0);
}

#[test]
fn extract_parameter_names_from_schema() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "team": {"type": "string"},
            "description": {"type": "string"},
        }
    });
    let mut names = extract_parameter_names(&schema);
    names.sort();
    assert_eq!(names, vec!["description", "team", "title"]);
}

#[test]
fn split_qualified_name_works() {
    assert_eq!(
        split_qualified_name("linear__save_issue"),
        ("linear", "save_issue")
    );
    assert_eq!(split_qualified_name("no_delimiter"), ("", "no_delimiter"));
}

#[test]
fn search_underscore_joined_identifier_components() {
    let tools = vec![
        ToolMetadata {
            qualified_name: "echo_agent_com_chronosphere__query_prometheus_range".into(),
            server_name: "echo_agent_com_chronosphere".into(),
            tool_name: "query_prometheus_range".into(),
            description: "Run a range query".into(),
            parameters: vec!["start".into(), "end".into()],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "echo_agent_com_chronosphere__list_metrics".into(),
            server_name: "echo_agent_com_chronosphere".into(),
            tool_name: "list_metrics".into(),
            description: "List available metrics".into(),
            parameters: vec![],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "linear__save_issue".into(),
            server_name: "linear".into(),
            tool_name: "save_issue".into(),
            description: "Create an issue".into(),
            parameters: vec!["title".into()],
            input_schema: serde_json::json!({}),
        },
    ];
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));
    let snap = index.search_snapshot("chronosphere", 5);
    assert_eq!(snap.results.len(), 2);
    let names: Vec<&str> = snap.results.iter().map(|r| r.tool_name.as_str()).collect();
    assert!(names.contains(&"echo_agent_com_chronosphere__query_prometheus_range"));
    assert!(names.contains(&"echo_agent_com_chronosphere__list_metrics"));

    let snap_exact = index.search_snapshot("query_prometheus_range", 3);
    assert_eq!(snap_exact.results.len(), 1);
    assert_eq!(
        snap_exact.results[0].tool_name,
        "echo_agent_com_chronosphere__query_prometheus_range"
    );
}

#[test]
fn gateway_tool_with_canonical_connector_tool_name_appears_in_results() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"},
        }
    });
    let index = Bm25ToolSearchIndex::new(make_snapshot(vec![ToolMetadata {
        qualified_name: "grafana__search_dashboards".into(),
        server_name: "grafana".into(),
        tool_name: "search_dashboards".into(),
        description: "Search Grafana dashboards".into(),
        parameters: extract_parameter_names(&schema),
        input_schema: schema.clone(),
    }]));

    let snap = index.search_snapshot("grafana dashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana__search_dashboards");
    assert_eq!(snap.results[0].server_name, "grafana");
    assert_eq!(snap.results[0].parameters, vec!["query"]);
    assert_eq!(snap.results[0].input_schema, schema);
}

#[test]
fn gateway_exact_canonical_name_match_returns_tool() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(vec![ToolMetadata {
        qualified_name: "slack__search".into(),
        server_name: "slack".into(),
        tool_name: "search".into(),
        description: "Search Slack messages".into(),
        parameters: vec!["query".into()],
        input_schema: serde_json::json!({}),
    }]));

    let snap = index.search_snapshot("slack__search", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "slack__search");
    assert_eq!(snap.results[0].server_name, "slack");
}

#[test]
fn local_mcp_tool_indexing_still_uses_qualified_name() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(linear_tools()));
    let snap = index.search_snapshot("linear__save_issue", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "linear__save_issue");
    assert_eq!(snap.results[0].server_name, "linear");
}

#[test]
fn gateway_only_snapshot_can_stay_partial() {
    let index = Bm25ToolSearchIndex::new(Arc::new(Mutex::new(ToolMetadataSnapshot {
        tools: vec![ToolMetadata {
            qualified_name: "grafana__search_dashboards".into(),
            server_name: "grafana".into(),
            tool_name: "search_dashboards".into(),
            description: "Search Grafana dashboards".into(),
            parameters: vec![],
            input_schema: serde_json::json!({}),
        }],
        servers: vec![],
        mcp_initialized: false,
    })));

    let snap = index.search_snapshot("grafana", 5);
    assert_eq!(snap.results.len(), 1);
    assert!(!snap.is_ready);
}

#[test]
fn list_server_summaries_groups_gateway_tools_by_connector_id() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(vec![ToolMetadata {
        qualified_name: "grafana__search_dashboards".into(),
        server_name: "grafana".into(),
        tool_name: "search_dashboards".into(),
        description: "Search Grafana dashboards".into(),
        parameters: vec![],
        input_schema: serde_json::json!({}),
    }]));
    let summaries = index.list_server_summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].name, "grafana");
    assert_eq!(summaries[0].tool_count, 1);
    assert_eq!(summaries[0].tool_names, vec!["search_dashboards"]);
}

#[test]
fn list_server_summaries_includes_descriptions() {
    let tools = linear_tools();
    let servers = vec![
        ServerMetadata {
            name: "linear".into(),
            description: Some("Project management".into()),
        },
        ServerMetadata {
            name: "demo-mcp".into(),
            description: None,
        },
    ];
    let index = Bm25ToolSearchIndex::new(make_snapshot_with_servers(tools, servers));
    let summaries = index.list_server_summaries();
    assert_eq!(summaries.len(), 2);
    // BTreeMap order is alphabetical by server name (demo-mcp < linear).
    let linear = summaries
        .iter()
        .find(|s| s.name == "linear")
        .expect("linear summary");
    assert_eq!(linear.description.as_deref(), Some("Project management"));
    assert_eq!(linear.tool_count, 4);
    assert_eq!(
        linear.tool_names,
        vec!["get_issue", "list_issues", "list_projects", "save_issue"]
    );
    let demo = summaries
        .iter()
        .find(|s| s.name == "demo-mcp")
        .expect("demo-mcp summary");
    assert_eq!(demo.description, None);
    assert_eq!(demo.tool_count, 2);
    assert_eq!(demo.tool_names, vec!["readSlackThread", "sendMessage"]);
}

#[test]
fn list_server_summaries_excludes_removed_tools() {
    let mut tools = linear_tools();
    tools.retain(|t| t.qualified_name != "linear__get_issue");
    let servers = vec![
        ServerMetadata {
            name: "linear".into(),
            description: Some("Project management".into()),
        },
        ServerMetadata {
            name: "demo-mcp".into(),
            description: None,
        },
    ];
    let index = Bm25ToolSearchIndex::new(make_snapshot_with_servers(tools, servers));
    let summaries = index.list_server_summaries();
    // Look up by name: BTreeMap order is alphabetical (demo-mcp before linear).
    let linear = summaries
        .iter()
        .find(|s| s.name == "linear")
        .expect("linear summary");
    assert_eq!(linear.tool_count, 3);
}

#[test]
fn list_server_summaries_shows_server_with_zero_tools() {
    let servers = vec![ServerMetadata {
        name: "empty_server".into(),
        description: Some("No tools yet".into()),
    }];
    let index = Bm25ToolSearchIndex::new(make_snapshot_with_servers(vec![], servers));
    let summaries = index.list_server_summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].name, "empty_server");
    assert_eq!(summaries[0].tool_count, 0);
    assert_eq!(summaries[0].description.as_deref(), Some("No tools yet"));
}

// -- exact match tests --

#[test]
fn search_exact_qualified_name() {
    let tools = vec![
        ToolMetadata {
            qualified_name: "grafana_observability_ui__SearchDashboards".into(),
            server_name: "grafana_observability_ui".into(),
            tool_name: "SearchDashboards".into(),
            description: "Search for Grafana dashboards".into(),
            parameters: vec!["query".into()],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "linear__save_issue".into(),
            server_name: "linear".into(),
            tool_name: "save_issue".into(),
            description: "Create or update a Linear issue".into(),
            parameters: vec!["title".into()],
            input_schema: serde_json::json!({}),
        },
    ];
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));

    // Exact qualified name → instant match, no BM25 needed
    let snap = index.search_snapshot("grafana_observability_ui__SearchDashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "grafana_observability_ui__SearchDashboards"
    );
}

#[test]
fn search_exact_bare_tool_name() {
    let tools = vec![
        ToolMetadata {
            qualified_name: "grafana_observability_ui__SearchDashboards".into(),
            server_name: "grafana_observability_ui".into(),
            tool_name: "SearchDashboards".into(),
            description: "Search for Grafana dashboards".into(),
            parameters: vec!["query".into()],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "linear__save_issue".into(),
            server_name: "linear".into(),
            tool_name: "save_issue".into(),
            description: "Create or update a Linear issue".into(),
            parameters: vec!["title".into()],
            input_schema: serde_json::json!({}),
        },
    ];
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));

    // Bare tool name → exact match on tool_name
    let snap = index.search_snapshot("SearchDashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "grafana_observability_ui__SearchDashboards"
    );
}

#[test]
fn search_exact_match_case_insensitive() {
    let tools = vec![ToolMetadata {
        qualified_name: "grafana_observability_ui__SearchDashboards".into(),
        server_name: "grafana_observability_ui".into(),
        tool_name: "SearchDashboards".into(),
        description: "Search for Grafana dashboards".into(),
        parameters: vec!["query".into()],
        input_schema: serde_json::json!({}),
    }];
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));

    let snap = index.search_snapshot("searchdashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "grafana_observability_ui__SearchDashboards"
    );
}

#[test]
fn search_exact_bare_name_ambiguous_returns_first_match() {
    // Two servers register tools with the same bare name.
    // `find()` returns the first match — the model might have wanted the second.
    let tools = vec![
        ToolMetadata {
            qualified_name: "server_a__fetch".into(),
            server_name: "server_a".into(),
            tool_name: "fetch".into(),
            description: "Fetch data from server A".into(),
            parameters: vec![],
            input_schema: serde_json::json!({}),
        },
        ToolMetadata {
            qualified_name: "server_b__fetch".into(),
            server_name: "server_b".into(),
            tool_name: "fetch".into(),
            description: "Fetch data from server B".into(),
            parameters: vec![],
            input_schema: serde_json::json!({}),
        },
    ];
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));

    // Bare name "fetch" → returns server_a (first in Vec), silently ignoring server_b
    let snap = index.search_snapshot("fetch", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "server_a__fetch");
    // server_b__fetch is never returned — the model can't discover it
    // without knowing the qualified name
}

// -- e2e tests with real Grafana + Mattermost tool data --

/// Build a realistic tool index matching a production MCP environment
/// with Grafana and Mattermost servers.
fn grafana_mattermost_tools() -> Vec<ToolMetadata> {
    vec![
        ToolMetadata {
            qualified_name: "grafana-ai__SearchDashboards".into(),
            server_name: "grafana-ai".into(),
            tool_name: "SearchDashboards".into(),
            description: "Search for Grafana dashboards by query string.\n\
                Returns matching dashboards with title, UID, folder, tags, and full URL. \
                Supports pagination."
                .into(),
            parameters: vec!["query".into(), "limit".into(), "page".into()],
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"},
                    "page": {"type": "integer"}
                }
            }),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__GetDashboardByUID".into(),
            server_name: "grafana-ai".into(),
            tool_name: "GetDashboardByUID".into(),
            description: "Get a complete Grafana dashboard by its UID.\n\
                Returns full dashboard JSON including panels, variables, and settings."
                .into(),
            parameters: vec!["uid".into()],
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "uid": {"type": "string"}
                }
            }),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__GenerateDeeplink".into(),
            server_name: "grafana-ai".into(),
            tool_name: "GenerateDeeplink".into(),
            description: "Generate a direct URL to a Grafana resource \
                (dashboard, panel, explore page, or alert rule)."
                .into(),
            parameters: vec![
                "resource_type".into(),
                "uid".into(),
                "panel_id".into(),
                "from".into(),
                "to".into(),
                "query".into(),
            ],
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__GetDashboardProperty".into(),
            server_name: "grafana-ai".into(),
            tool_name: "GetDashboardProperty".into(),
            description: "Get a specific property value from a Grafana dashboard by JSONPath."
                .into(),
            parameters: vec!["uid".into(), "property".into()],
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__DeleteAlertRule".into(),
            server_name: "grafana-ai".into(),
            tool_name: "DeleteAlertRule".into(),
            description: "Delete a Grafana alert rule by UID. This action cannot be undone.".into(),
            parameters: vec!["uid".into()],
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__SearchFolders".into(),
            server_name: "grafana-ai".into(),
            tool_name: "SearchFolders".into(),
            description: "Search for Grafana folders by query string.\n\
                Returns matching folders with title, UID, and URL."
                .into(),
            parameters: vec!["query".into()],
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__ListDatasources".into(),
            server_name: "grafana-ai".into(),
            tool_name: "ListDatasources".into(),
            description: "List all configured datasources in Grafana.\n\
                Returns datasource summaries with UID, name, type."
                .into(),
            parameters: vec!["type".into()],
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__ListContactPoints".into(),
            server_name: "grafana-ai".into(),
            tool_name: "ListContactPoints".into(),
            description: "List Grafana notification contact points.\n\
                Returns summaries including UID, name, and type."
                .into(),
            parameters: vec!["name".into(), "limit".into()],
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata {
            qualified_name: "grafana-ai__GetDashboardPanelQueries".into(),
            server_name: "grafana-ai".into(),
            tool_name: "GetDashboardPanelQueries".into(),
            description: "Get panel queries from a Grafana dashboard.\n\
                Returns an array of panel queries with title, query expression, \
                and datasource info."
                .into(),
            parameters: vec!["uid".into()],
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolMetadata {
            qualified_name: "mattermost__SearchPosts".into(),
            server_name: "mattermost".into(),
            tool_name: "SearchPosts".into(),
            description: "Search for posts in Mattermost.\n\
                Parameters can be passed as direct fields OR in the input JSON."
                .into(),
            parameters: vec![
                "input".into(),
                "channel_id".into(),
                "chat_id".into(),
                "query".into(),
            ],
            input_schema: serde_json::json!({"type": "object"}),
        },
    ]
}

#[test]
fn e2e_exact_qualified_name_grafana() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    // Model queries with the exact qualified name
    let snap = index.search_snapshot("grafana-ai__SearchDashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

#[test]
fn e2e_exact_bare_tool_name_grafana() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    // Model queries with bare tool name (no server prefix)
    let snap = index.search_snapshot("SearchDashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

#[test]
fn e2e_exact_match_other_grafana_tools() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    // Each Grafana tool should be findable by exact bare name
    for (query, expected) in [
        ("GetDashboardByUID", "grafana-ai__GetDashboardByUID"),
        ("GenerateDeeplink", "grafana-ai__GenerateDeeplink"),
        ("GetDashboardProperty", "grafana-ai__GetDashboardProperty"),
        ("DeleteAlertRule", "grafana-ai__DeleteAlertRule"),
        ("SearchFolders", "grafana-ai__SearchFolders"),
        ("ListDatasources", "grafana-ai__ListDatasources"),
        ("ListContactPoints", "grafana-ai__ListContactPoints"),
        (
            "GetDashboardPanelQueries",
            "grafana-ai__GetDashboardPanelQueries",
        ),
    ] {
        let snap = index.search_snapshot(query, 5);
        assert_eq!(
            snap.results.len(),
            1,
            "query {query:?} should return exactly 1 result"
        );
        assert_eq!(
            snap.results[0].tool_name, expected,
            "query {query:?} should match {expected:?}"
        );
    }
}

#[test]
fn e2e_exact_match_mattermost() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    let snap = index.search_snapshot("mattermost__SearchPosts", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "mattermost__SearchPosts");

    let snap = index.search_snapshot("SearchPosts", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "mattermost__SearchPosts");
}

#[test]
fn e2e_exact_match_case_insensitive_grafana() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    // Model might lowercase the qualified name
    let snap = index.search_snapshot("grafana-ai__searchdashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

#[test]
fn e2e_fuzzy_search_dashboards() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    // Natural language query should find dashboard-related tools via BM25
    let snap = index.search_snapshot("search dashboards", 5);
    assert!(
        !snap.results.is_empty(),
        "fuzzy 'search dashboards' should return results"
    );
    assert_eq!(
        snap.results[0].tool_name, "grafana-ai__SearchDashboards",
        "'search dashboards' should rank SearchDashboards first"
    );
}

#[test]
fn e2e_fuzzy_grafana_alert() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    let snap = index.search_snapshot("delete alert rule", 5);
    assert!(
        !snap.results.is_empty(),
        "fuzzy 'delete alert rule' should return results"
    );
    assert_eq!(
        snap.results[0].tool_name, "grafana-ai__DeleteAlertRule",
        "'delete alert rule' should rank DeleteAlertRule first"
    );
}

#[test]
fn e2e_fuzzy_datasources() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    let snap = index.search_snapshot("list datasources prometheus", 5);
    assert!(
        !snap.results.is_empty(),
        "fuzzy 'list datasources prometheus' should return results"
    );
    assert_eq!(
        snap.results[0].tool_name, "grafana-ai__ListDatasources",
        "'list datasources prometheus' should rank ListDatasources first"
    );
}

#[test]
fn e2e_fuzzy_search_posts_mattermost() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    let snap = index.search_snapshot("search mattermost messages", 5);
    assert!(
        !snap.results.is_empty(),
        "fuzzy 'search mattermost messages' should return results"
    );
    // Mattermost SearchPosts should appear (it has "Search" and "Mattermost" in its doc)
    let names: Vec<&str> = snap.results.iter().map(|r| r.tool_name.as_str()).collect();
    assert!(
        names.contains(&"mattermost__SearchPosts"),
        "expected mattermost__SearchPosts in results, got: {names:?}"
    );
}

#[test]
fn e2e_no_match_falls_through_to_bm25() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(grafana_mattermost_tools()));

    // A query that doesn't exactly match any tool name should still
    // return BM25 results (not an empty exact-match response)
    let snap = index.search_snapshot("dashboard panel queries grafana", 5);
    assert!(
        !snap.results.is_empty(),
        "non-exact query should fall through to BM25"
    );
}

#[test]
fn e2e_total_hidden_tools_correct() {
    let tools = grafana_mattermost_tools();
    let total = tools.len();
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));

    let snap = index.search_snapshot("grafana-ai__SearchDashboards", 5);
    assert_eq!(snap.total_hidden_tools, total);

    let snap = index.search_snapshot("search dashboards", 3);
    assert_eq!(snap.total_hidden_tools, total);
}

// -- split_identifier unit tests --

#[test]
fn split_empty() {
    let r: Vec<&str> = split_identifier("");
    assert!(r.is_empty());
}

#[test]
fn split_single_word() {
    assert_eq!(split_identifier("fetch"), vec!["fetch"]);
}

#[test]
fn split_single_char() {
    assert_eq!(split_identifier("a"), vec!["a"]);
}

#[test]
fn split_snake_case() {
    assert_eq!(split_identifier("save_issue"), vec!["save", "issue"]);
}

#[test]
fn split_kebab_case() {
    assert_eq!(split_identifier("notion-search"), vec!["notion", "search"]);
}

#[test]
fn split_camel_case() {
    assert_eq!(
        split_identifier("getIssueDetails"),
        vec!["get", "Issue", "Details"]
    );
}

#[test]
fn split_pascal_case() {
    assert_eq!(
        split_identifier("SearchDashboards"),
        vec!["Search", "Dashboards"]
    );
}

#[test]
fn split_double_underscore_delimiter() {
    assert_eq!(
        split_identifier("grafana-ai__SearchDashboards"),
        vec!["grafana", "ai", "Search", "Dashboards"]
    );
}

#[test]
fn split_all_uppercase_stays_together() {
    // No lowercase→uppercase transition, so stays as one token
    assert_eq!(split_identifier("UID"), vec!["UID"]);
    assert_eq!(split_identifier("HTTP"), vec!["HTTP"]);
}

#[test]
fn split_trailing_acronym() {
    // "ByUID" → "By" + "UID" (y→U transition)
    assert_eq!(
        split_identifier("GetDashboardByUID"),
        vec!["Get", "Dashboard", "By", "UID"]
    );
}

#[test]
fn split_consecutive_delimiters() {
    // triple underscore: split("__") → ["a", "_b"] → split('_') → ["a","","b"]
    assert_eq!(split_identifier("a___b"), vec!["a", "b"]);
}

#[test]
fn split_only_delimiters() {
    assert!(split_identifier("__").is_empty());
    assert!(split_identifier("_").is_empty());
    assert!(split_identifier("-").is_empty());
}

#[test]
fn split_leading_trailing_delimiters() {
    assert_eq!(split_identifier("_foo_"), vec!["foo"]);
    assert_eq!(split_identifier("-bar-"), vec!["bar"]);
}

#[test]
fn split_numbers_in_identifiers() {
    assert_eq!(split_identifier("v2_api"), vec!["v2", "api"]);
    assert_eq!(
        split_identifier("query_prometheus_range"),
        vec!["query", "prometheus", "range"]
    );
}

#[test]
fn split_mixed_formats() {
    assert_eq!(
        split_identifier("echo_agent_com_slack__slack_send_message"),
        vec!["echo", "agent", "com", "slack", "slack", "send", "message"]
    );
}

// -- normalize_query unit tests --

#[test]
fn normalize_plain_english_passthrough() {
    let q = "search for dashboards";
    assert_eq!(normalize_query(q), q);
}

#[test]
fn normalize_plain_english_with_numbers() {
    let q = "list 10 issues";
    assert_eq!(normalize_query(q), q);
}

#[test]
fn normalize_empty() {
    assert_eq!(normalize_query(""), "");
}

#[test]
fn normalize_whitespace_only() {
    // split_whitespace on "   " yields nothing → extra is empty → passthrough
    assert_eq!(normalize_query("   "), "   ");
}

#[test]
fn normalize_underscore_query() {
    let result = normalize_query("search_public");
    assert!(result.starts_with("search_public"));
    assert!(result.contains(" search"));
    assert!(result.contains(" public"));
}

#[test]
fn normalize_double_underscore_query() {
    let result = normalize_query("grafana-ai__SearchDashboards");
    assert!(result.starts_with("grafana-ai__SearchDashboards"));
    assert!(result.contains(" grafana"));
    assert!(result.contains(" ai"));
    assert!(result.contains(" Search"));
    assert!(result.contains(" Dashboards"));
}

#[test]
fn normalize_camel_case_query() {
    let result = normalize_query("SearchDashboards");
    assert!(result.starts_with("SearchDashboards"));
    assert!(result.contains(" Search"));
    assert!(result.contains(" Dashboards"));
}

#[test]
fn normalize_kebab_query() {
    let result = normalize_query("notion-create");
    assert!(result.starts_with("notion-create"));
    assert!(result.contains(" notion"));
    assert!(result.contains(" create"));
}

#[test]
fn normalize_hyphenated_english_harmless() {
    // "high-priority" triggers normalization but result is harmless —
    // just appends "high priority" which are already in the query
    let result = normalize_query("create a high-priority issue");
    assert!(result.starts_with("create a high-priority issue"));
    // Extra tokens are subsets of what's already there, won't hurt BM25
}

// -- MCP name format coverage --
//
// Real MCP qualified names follow the pattern `{server}__{tool}` where
// server and tool names independently use different conventions:
//
//   Server formats:  simple        ("linear")
//                    kebab-case    ("grafana-ai")
//                    snake_case    ("echo_agent_com_slack")
//
//   Tool formats:    snake_case    ("save_issue")
//                    PascalCase    ("SearchDashboards")
//                    camelCase     ("sendMessage")
//                    kebab-case    ("notion-search")
//                    single word   ("fetch")

/// Fixture covering every server × tool naming convention observed in
/// production MCP configs.
fn mcp_format_tools() -> Vec<ToolMetadata> {
    vec![
        // simple server + snake_case tool
        ToolMetadata {
            qualified_name: "linear__save_issue".into(),
            server_name: "linear".into(),
            tool_name: "save_issue".into(),
            description: "Create or update a Linear issue".into(),
            parameters: vec!["title".into()],
            input_schema: serde_json::json!({}),
        },
        // simple server + camelCase tool
        ToolMetadata {
            qualified_name: "linear__getIssueDetails".into(),
            server_name: "linear".into(),
            tool_name: "getIssueDetails".into(),
            description: "Retrieve detailed information about an issue".into(),
            parameters: vec!["id".into()],
            input_schema: serde_json::json!({}),
        },
        // kebab-case server + PascalCase tool
        ToolMetadata {
            qualified_name: "grafana-ai__SearchDashboards".into(),
            server_name: "grafana-ai".into(),
            tool_name: "SearchDashboards".into(),
            description: "Search for Grafana dashboards".into(),
            parameters: vec!["query".into()],
            input_schema: serde_json::json!({}),
        },
        // snake_case server + snake_case tool
        ToolMetadata {
            qualified_name: "echo_agent_com_slack__slack_send_message".into(),
            server_name: "echo_agent_com_slack".into(),
            tool_name: "slack_send_message".into(),
            description: "Send a message in a Slack channel".into(),
            parameters: vec!["channel_id".into(), "text".into()],
            input_schema: serde_json::json!({}),
        },
        // snake_case server + PascalCase tool
        ToolMetadata {
            qualified_name: "echo_agent_com_chronosphere__QueryPrometheusRange".into(),
            server_name: "echo_agent_com_chronosphere".into(),
            tool_name: "QueryPrometheusRange".into(),
            description: "Run a Prometheus range query".into(),
            parameters: vec!["query".into(), "start".into(), "end".into()],
            input_schema: serde_json::json!({}),
        },
        // simple server + kebab-case tool
        ToolMetadata {
            qualified_name: "notion__notion-search".into(),
            server_name: "notion".into(),
            tool_name: "notion-search".into(),
            description: "Search Notion pages and databases".into(),
            parameters: vec!["query".into()],
            input_schema: serde_json::json!({}),
        },
        // simple server + single word tool
        ToolMetadata {
            qualified_name: "jira__fetch".into(),
            server_name: "jira".into(),
            tool_name: "fetch".into(),
            description: "Fetch a Jira issue by key".into(),
            parameters: vec!["key".into()],
            input_schema: serde_json::json!({}),
        },
        // kebab-case server + snake_case tool
        ToolMetadata {
            qualified_name: "my-server__get_user_info".into(),
            server_name: "my-server".into(),
            tool_name: "get_user_info".into(),
            description: "Retrieve user profile information".into(),
            parameters: vec!["user_id".into()],
            input_schema: serde_json::json!({}),
        },
        // kebab-case server + camelCase tool
        ToolMetadata {
            qualified_name: "my-server__listProjects".into(),
            server_name: "my-server".into(),
            tool_name: "listProjects".into(),
            description: "List all projects".into(),
            parameters: vec![],
            input_schema: serde_json::json!({}),
        },
    ]
}

// ── Exact match: qualified names ────────────────────────────────

#[test]
fn fmt_exact_simple_snake() {
    // simple__snake_case
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("linear__save_issue", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "linear__save_issue");
}

#[test]
fn fmt_exact_simple_camel() {
    // simple__camelCase
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("linear__getIssueDetails", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "linear__getIssueDetails");
}

#[test]
fn fmt_exact_kebab_pascal() {
    // kebab-case__PascalCase
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("grafana-ai__SearchDashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

#[test]
fn fmt_exact_snake_server_snake_tool() {
    // snake_case__snake_case
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("echo_agent_com_slack__slack_send_message", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "echo_agent_com_slack__slack_send_message"
    );
}

#[test]
fn fmt_exact_snake_server_pascal_tool() {
    // snake_case__PascalCase
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("echo_agent_com_chronosphere__QueryPrometheusRange", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "echo_agent_com_chronosphere__QueryPrometheusRange"
    );
}

#[test]
fn fmt_exact_simple_kebab_tool() {
    // simple__kebab-case
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("notion__notion-search", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "notion__notion-search");
}

#[test]
fn fmt_exact_simple_single_word() {
    // simple__word
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("jira__fetch", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "jira__fetch");
}

#[test]
fn fmt_exact_kebab_server_snake_tool() {
    // kebab-case__snake_case
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("my-server__get_user_info", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "my-server__get_user_info");
}

#[test]
fn fmt_exact_kebab_server_camel_tool() {
    // kebab-case__camelCase
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("my-server__listProjects", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "my-server__listProjects");
}

// ── Exact match: bare tool names ────────────────────────────────

#[test]
fn fmt_bare_snake_case() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("save_issue", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "linear__save_issue");
}

#[test]
fn fmt_bare_camel_case() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("getIssueDetails", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "linear__getIssueDetails");
}

#[test]
fn fmt_bare_pascal_case() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("SearchDashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

#[test]
fn fmt_bare_kebab_case() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("notion-search", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "notion__notion-search");
}

#[test]
fn fmt_bare_single_word() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("fetch", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "jira__fetch");
}

// ── Exact match: case insensitivity across formats ──────────────

#[test]
fn fmt_case_insensitive_qualified_kebab_pascal() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("GRAFANA-AI__SEARCHDASHBOARDS", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

#[test]
fn fmt_case_insensitive_qualified_snake_snake() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("ECHO_AGENT_COM_SLACK__SLACK_SEND_MESSAGE", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "echo_agent_com_slack__slack_send_message"
    );
}

#[test]
fn fmt_case_insensitive_bare_camel() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("getissuedetails", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "linear__getIssueDetails");
}

// ── Whitespace handling ─────────────────────────────────────────

#[test]
fn fmt_leading_trailing_whitespace() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("  SearchDashboards  ", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

#[test]
fn fmt_whitespace_qualified() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("  linear__save_issue  ", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "linear__save_issue");
}

// ── Server name alone should NOT exact match ────────────────────

#[test]
fn fmt_server_name_only_falls_through() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));

    // "linear" is a server name, not a tool name — should fall through
    // to BM25, not return a single exact match
    let snap = index.search_snapshot("linear", 5);
    // BM25 may return multiple tools from the linear server
    assert!(
        snap.results.len() != 1 || snap.results[0].tool_name != "linear",
        "server name alone should not be an exact tool match"
    );
}

#[test]
fn fmt_kebab_server_only_falls_through() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("grafana-ai", 5);
    // Should fall through to BM25 — "grafana-ai" is not a tool name
    for r in &snap.results {
        assert_ne!(
            r.tool_name, "grafana-ai",
            "should not match a bare server name as a tool"
        );
    }
}

#[test]
fn fmt_snake_server_only_falls_through() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("echo_agent_com_slack", 5);
    for r in &snap.results {
        assert_ne!(
            r.tool_name, "echo_agent_com_slack",
            "should not match a bare server name as a tool"
        );
    }
}

// ── Nonexistent qualified names fall through to BM25 ────────────

#[test]
fn fmt_wrong_server_prefix_falls_through() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));

    // Model hallucinated the wrong server prefix — no exact match,
    // falls through to BM25 which may still find the right tool
    let snap = index.search_snapshot("slack__SearchDashboards", 5);
    // Not an exact match, so results.len() > 1 or different ordering is fine
    assert!(
        snap.results.is_empty() || snap.results[0].tool_name != "slack__SearchDashboards",
        "hallucinated qualified name should not exact match"
    );
}

#[test]
fn fmt_wrong_tool_name_falls_through() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(mcp_format_tools()));
    let snap = index.search_snapshot("linear__create_issue", 5);
    // "create_issue" doesn't exist — should fall through to BM25
    assert!(
        snap.results.is_empty() || snap.results[0].tool_name != "linear__create_issue",
        "nonexistent tool should not exact match"
    );
}

// ── Needle-in-haystack: production-scale index ──────────────────
//
// Realistic fixture with ~55 tools across 5 servers (Slack 17,
// Notion 14, Grafana 9, Linear 8, GitHub 7). Tests that BM25
// finds the right tool via partial / natural-language queries
// when there are many competing documents.

fn production_haystack() -> Vec<ToolMetadata> {
    let tool = |qn: &str, server: &str, name: &str, desc: &str, params: &[&str]| ToolMetadata {
        qualified_name: qn.into(),
        server_name: server.into(),
        tool_name: name.into(),
        description: desc.into(),
        parameters: params.iter().map(|s| (*s).into()).collect(),
        input_schema: serde_json::json!({}),
    };

    vec![
        // ── echo_agent_com_slack (17 tools) ───────────────────────────
        tool(
            "echo_agent_com_slack__slack_create_canvas",
            "echo_agent_com_slack",
            "slack_create_canvas",
            "Create a new Slack canvas in a channel",
            &["channel_id", "content"],
        ),
        tool(
            "echo_agent_com_slack__slack_get_reactions",
            "echo_agent_com_slack",
            "slack_get_reactions",
            "Retrieves all reactions (emoji) on a specific Slack message",
            &["channel_id", "message_ts"],
        ),
        tool(
            "echo_agent_com_slack__slack_list_channel_members",
            "echo_agent_com_slack",
            "slack_list_channel_members",
            "List members of a Slack channel",
            &["channel_id"],
        ),
        tool(
            "echo_agent_com_slack__slack_read_canvas",
            "echo_agent_com_slack",
            "slack_read_canvas",
            "Read a Slack canvas by ID",
            &["canvas_id"],
        ),
        tool(
            "echo_agent_com_slack__slack_read_channel",
            "echo_agent_com_slack",
            "slack_read_channel",
            "Reads messages from a Slack channel in reverse chronological order",
            &["channel_id", "limit"],
        ),
        tool(
            "echo_agent_com_slack__slack_read_file",
            "echo_agent_com_slack",
            "slack_read_file",
            "Reads a Slack file's content by file ID",
            &["file_id"],
        ),
        tool(
            "echo_agent_com_slack__slack_read_thread",
            "echo_agent_com_slack",
            "slack_read_thread",
            "Reads messages from a specific Slack thread (parent message + all replies)",
            &["channel_id", "message_ts"],
        ),
        tool(
            "echo_agent_com_slack__slack_read_user_profile",
            "echo_agent_com_slack",
            "slack_read_user_profile",
            "Read a Slack user's profile information",
            &["user_id"],
        ),
        tool(
            "echo_agent_com_slack__slack_schedule_message",
            "echo_agent_com_slack",
            "slack_schedule_message",
            "Schedule a message to be sent at a specific time",
            &["channel_id", "text", "post_at"],
        ),
        tool(
            "echo_agent_com_slack__slack_search_channels",
            "echo_agent_com_slack",
            "slack_search_channels",
            "Search for Slack channels by name or topic",
            &["query"],
        ),
        tool(
            "echo_agent_com_slack__slack_search_emojis",
            "echo_agent_com_slack",
            "slack_search_emojis",
            "Search for custom emoji in the Slack workspace",
            &["query"],
        ),
        tool(
            "echo_agent_com_slack__slack_search_public",
            "echo_agent_com_slack",
            "slack_search_public",
            "Searches for messages and files in public Slack channels only",
            &["query", "sort", "sort_dir"],
        ),
        tool(
            "echo_agent_com_slack__slack_search_public_and_private",
            "echo_agent_com_slack",
            "slack_search_public_and_private",
            "Searches for messages and files in both public and private Slack channels",
            &["query", "sort", "sort_dir"],
        ),
        tool(
            "echo_agent_com_slack__slack_search_users",
            "echo_agent_com_slack",
            "slack_search_users",
            "Search for users in the Slack workspace by name or email",
            &["query"],
        ),
        tool(
            "echo_agent_com_slack__slack_send_message",
            "echo_agent_com_slack",
            "slack_send_message",
            "Send a message in a Slack channel or thread",
            &["channel_id", "text", "thread_ts"],
        ),
        tool(
            "echo_agent_com_slack__slack_send_message_draft",
            "echo_agent_com_slack",
            "slack_send_message_draft",
            "Create a draft message for user review before sending",
            &["channel_id", "text"],
        ),
        tool(
            "echo_agent_com_slack__slack_update_canvas",
            "echo_agent_com_slack",
            "slack_update_canvas",
            "Update the content of an existing Slack canvas",
            &["canvas_id", "content"],
        ),
        // ── notion (14 tools) ───────────────────────────────────
        tool(
            "notion__notion-create-comment",
            "notion",
            "notion-create-comment",
            "Create a comment on a Notion page or discussion",
            &["page_id", "text"],
        ),
        tool(
            "notion__notion-create-database",
            "notion",
            "notion-create-database",
            "Create a new Notion database with specified properties",
            &["parent_id", "title", "properties"],
        ),
        tool(
            "notion__notion-create-pages",
            "notion",
            "notion-create-pages",
            "Create one or more new Notion pages",
            &["parent_id", "title", "content"],
        ),
        tool(
            "notion__notion-create-view",
            "notion",
            "notion-create-view",
            "Create a new view for a Notion database",
            &["database_id", "type"],
        ),
        tool(
            "notion__notion-duplicate-page",
            "notion",
            "notion-duplicate-page",
            "Duplicate an existing Notion page",
            &["page_id"],
        ),
        tool(
            "notion__notion-fetch",
            "notion",
            "notion-fetch",
            "Fetch the content of a Notion page or block by URL or ID",
            &["url"],
        ),
        tool(
            "notion__notion-get-comments",
            "notion",
            "notion-get-comments",
            "Get comments on a Notion page or discussion",
            &["page_id"],
        ),
        tool(
            "notion__notion-get-teams",
            "notion",
            "notion-get-teams",
            "Get the list of teams in the Notion workspace",
            &[],
        ),
        tool(
            "notion__notion-get-users",
            "notion",
            "notion-get-users",
            "Get the list of users in the Notion workspace",
            &[],
        ),
        tool(
            "notion__notion-move-pages",
            "notion",
            "notion-move-pages",
            "Move Notion pages to a different parent",
            &["page_ids", "target_parent_id"],
        ),
        tool(
            "notion__notion-search",
            "notion",
            "notion-search",
            "Search Notion pages and databases by title or content",
            &["query"],
        ),
        tool(
            "notion__notion-update-data-source",
            "notion",
            "notion-update-data-source",
            "Update the data source configuration for a Notion database",
            &["database_id"],
        ),
        tool(
            "notion__notion-update-page",
            "notion",
            "notion-update-page",
            "Update properties or content of an existing Notion page",
            &["page_id", "properties"],
        ),
        tool(
            "notion__notion-update-view",
            "notion",
            "notion-update-view",
            "Update a view configuration for a Notion database",
            &["view_id"],
        ),
        // ── grafana-ai (9 tools) ────────────────────────────────
        tool(
            "grafana-ai__SearchDashboards",
            "grafana-ai",
            "SearchDashboards",
            "Search for Grafana dashboards by query string. Returns matching dashboards with title, UID, folder, tags, and full URL.",
            &["query", "limit", "page"],
        ),
        tool(
            "grafana-ai__GetDashboardByUID",
            "grafana-ai",
            "GetDashboardByUID",
            "Get a complete Grafana dashboard by its UID. Returns full dashboard JSON including panels, variables, and settings.",
            &["uid"],
        ),
        tool(
            "grafana-ai__GenerateDeeplink",
            "grafana-ai",
            "GenerateDeeplink",
            "Generate a direct URL to a Grafana resource (dashboard, panel, explore page, or alert rule).",
            &["resource_type", "uid"],
        ),
        tool(
            "grafana-ai__GetDashboardProperty",
            "grafana-ai",
            "GetDashboardProperty",
            "Get a specific property value from a Grafana dashboard by JSONPath.",
            &["uid", "property"],
        ),
        tool(
            "grafana-ai__DeleteAlertRule",
            "grafana-ai",
            "DeleteAlertRule",
            "Delete a Grafana alert rule by UID. This action cannot be undone.",
            &["uid"],
        ),
        tool(
            "grafana-ai__SearchFolders",
            "grafana-ai",
            "SearchFolders",
            "Search for Grafana folders by query string. Returns matching folders with title, UID, and URL.",
            &["query"],
        ),
        tool(
            "grafana-ai__ListDatasources",
            "grafana-ai",
            "ListDatasources",
            "List all configured datasources in Grafana. Returns datasource summaries with UID, name, type.",
            &["type"],
        ),
        tool(
            "grafana-ai__ListContactPoints",
            "grafana-ai",
            "ListContactPoints",
            "List Grafana notification contact points. Returns summaries including UID, name, and type.",
            &["name", "limit"],
        ),
        tool(
            "grafana-ai__GetDashboardPanelQueries",
            "grafana-ai",
            "GetDashboardPanelQueries",
            "Get panel queries from a Grafana dashboard. Returns an array of panel queries with title, query expression, and datasource info.",
            &["uid"],
        ),
        // ── linear (8 tools) ────────────────────────────────────
        tool(
            "linear__save_issue",
            "linear",
            "save_issue",
            "Create or update a Linear issue",
            &["title", "team", "description", "assignee", "priority"],
        ),
        tool(
            "linear__list_issues",
            "linear",
            "list_issues",
            "List issues in the user's Linear workspace",
            &["assignee", "project", "state", "team", "query"],
        ),
        tool(
            "linear__get_issue",
            "linear",
            "get_issue",
            "Retrieve detailed information about a Linear issue by ID",
            &["id"],
        ),
        tool(
            "linear__list_projects",
            "linear",
            "list_projects",
            "List projects in the user's Linear workspace",
            &["query", "team"],
        ),
        tool(
            "linear__create_comment",
            "linear",
            "create_comment",
            "Add a comment to a Linear issue",
            &["issue_id", "body"],
        ),
        tool(
            "linear__list_teams",
            "linear",
            "list_teams",
            "List teams in the user's Linear workspace",
            &[],
        ),
        tool(
            "linear__search_issues",
            "linear",
            "search_issues",
            "Search for Linear issues by keyword",
            &["query"],
        ),
        tool(
            "linear__get_user",
            "linear",
            "get_user",
            "Get information about a Linear user",
            &["id"],
        ),
        // ── github (7 tools) ────────────────────────────────────
        tool(
            "github__create_pull_request",
            "github",
            "create_pull_request",
            "Create a new GitHub pull request",
            &["repo", "title", "head", "base", "body"],
        ),
        tool(
            "github__list_pull_requests",
            "github",
            "list_pull_requests",
            "List pull requests in a GitHub repository",
            &["repo", "state"],
        ),
        tool(
            "github__get_file_contents",
            "github",
            "get_file_contents",
            "Get the contents of a file from a GitHub repository",
            &["repo", "path", "ref"],
        ),
        tool(
            "github__search_code",
            "github",
            "search_code",
            "Search for code across GitHub repositories",
            &["query"],
        ),
        tool(
            "github__create_issue",
            "github",
            "create_issue",
            "Create a new GitHub issue",
            &["repo", "title", "body"],
        ),
        tool(
            "github__list_commits",
            "github",
            "list_commits",
            "List commits in a GitHub repository",
            &["repo", "sha"],
        ),
        tool(
            "github__get_pull_request",
            "github",
            "get_pull_request",
            "Get details about a specific GitHub pull request",
            &["repo", "pull_number"],
        ),
    ]
}

/// Helper: assert the needle is in the top-N results.
fn assert_top_n(snap: &SearchSnapshot, expected_tool: &str, n: usize, query: &str) {
    let names: Vec<&str> = snap
        .results
        .iter()
        .take(n)
        .map(|r| r.tool_name.as_str())
        .collect();
    assert!(
        names.contains(&expected_tool),
        "query {query:?}: expected {expected_tool:?} in top-{n}, got {names:?}"
    );
}

// ── Needle-in-haystack: exact match still works at scale ────────

#[test]
fn haystack_exact_qualified_name() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("echo_agent_com_slack__slack_search_public", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "echo_agent_com_slack__slack_search_public"
    );
}

#[test]
fn haystack_exact_bare_name() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("slack_search_public", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(
        snap.results[0].tool_name,
        "echo_agent_com_slack__slack_search_public"
    );
}

#[test]
fn haystack_exact_notion_kebab() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("notion-search", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "notion__notion-search");
}

#[test]
fn haystack_exact_grafana_pascal() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("SearchDashboards", 5);
    assert_eq!(snap.results.len(), 1);
    assert_eq!(snap.results[0].tool_name, "grafana-ai__SearchDashboards");
}

// ── Needle-in-haystack: BM25 fuzzy queries ──────────────────────

#[test]
fn haystack_bm25_search_slack_public() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // Natural language query for the Slack public search tool
    let snap = index.search_snapshot("search public slack messages", 5);
    assert_top_n(
        &snap,
        "echo_agent_com_slack__slack_search_public",
        3,
        "search public slack messages",
    );
}

#[test]
fn haystack_bm25_send_slack_message() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("send a message in slack", 5);
    assert_top_n(
        &snap,
        "echo_agent_com_slack__slack_send_message",
        3,
        "send a message in slack",
    );
}

#[test]
fn haystack_bm25_read_slack_thread() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("read thread replies slack", 5);
    assert_top_n(
        &snap,
        "echo_agent_com_slack__slack_read_thread",
        3,
        "read thread replies slack",
    );
}

#[test]
fn haystack_bm25_search_notion_pages() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("search notion pages", 5);
    assert_top_n(&snap, "notion__notion-search", 3, "search notion pages");
}

#[test]
fn haystack_bm25_create_notion_page() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("create a new notion page", 5);
    assert_top_n(
        &snap,
        "notion__notion-create-pages",
        3,
        "create a new notion page",
    );
}

#[test]
fn haystack_bm25_grafana_dashboards() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("search grafana dashboards", 5);
    assert_top_n(
        &snap,
        "grafana-ai__SearchDashboards",
        3,
        "search grafana dashboards",
    );
}

#[test]
fn haystack_bm25_grafana_alert_rule() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("delete alert rule grafana", 5);
    assert_top_n(
        &snap,
        "grafana-ai__DeleteAlertRule",
        3,
        "delete alert rule grafana",
    );
}

#[test]
fn haystack_bm25_linear_create_issue() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("create a linear issue", 5);
    assert_top_n(&snap, "linear__save_issue", 3, "create a linear issue");
}

#[test]
fn haystack_bm25_github_pull_request() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("create pull request github", 5);
    assert_top_n(
        &snap,
        "github__create_pull_request",
        3,
        "create pull request github",
    );
}

#[test]
fn haystack_bm25_github_search_code() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    let snap = index.search_snapshot("search code in github repos", 5);
    assert_top_n(
        &snap,
        "github__search_code",
        3,
        "search code in github repos",
    );
}

// ── Disambiguation: similar tools rank correctly ────────────────

#[test]
fn haystack_disambiguate_search_public_vs_private() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // "public" should rank the public-only tool above the public+private tool
    let snap = index.search_snapshot("search public channels only", 5);
    let names: Vec<&str> = snap.results.iter().map(|r| r.tool_name.as_str()).collect();
    let pub_pos = names
        .iter()
        .position(|n| *n == "echo_agent_com_slack__slack_search_public");
    let priv_pos = names
        .iter()
        .position(|n| *n == "echo_agent_com_slack__slack_search_public_and_private");
    assert!(
        pub_pos.is_some(),
        "slack_search_public should appear for 'search public channels only', got {names:?}"
    );
    // If both appear, public-only should rank first
    if let (Some(p), Some(pp)) = (pub_pos, priv_pos) {
        assert!(
            p < pp,
            "slack_search_public (pos {p}) should rank above slack_search_public_and_private (pos {pp})"
        );
    }
}

#[test]
fn haystack_disambiguate_linear_create_vs_github_create() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // "linear issue" should rank Linear's save_issue above GitHub's create_issue
    let snap = index.search_snapshot("create linear issue", 5);
    assert_top_n(&snap, "linear__save_issue", 2, "create linear issue");
}

#[test]
fn haystack_disambiguate_notion_create_comment_vs_linear() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // "notion comment" should find the Notion tool, not Linear's create_comment
    let snap = index.search_snapshot("add comment notion page", 5);
    assert_top_n(
        &snap,
        "notion__notion-create-comment",
        3,
        "add comment notion page",
    );
}

// ── Scale: total_hidden_tools correct ───────────────────────────

// ── Non-exact identifier queries (query normalization) ─────────

#[test]
fn haystack_wrong_server_prefix_finds_tool() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // Model hallucinated "grafana-ai__SearchDashboards" but correct
    // server is different. Not an exact match — BM25 with normalized
    // query should still surface the tool via "Search" + "Dashboards".
    let snap = index.search_snapshot("wrong_server__SearchDashboards", 5);
    assert_top_n(
        &snap,
        "grafana-ai__SearchDashboards",
        3,
        "wrong_server__SearchDashboards",
    );
}

#[test]
fn haystack_partial_snake_case_query() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // Query is a partial snake_case identifier — not an exact tool name
    let snap = index.search_snapshot("search_public", 5);
    assert_top_n(
        &snap,
        "echo_agent_com_slack__slack_search_public",
        3,
        "search_public",
    );
}

#[test]
fn haystack_camel_case_query_finds_tool() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // camelCase query that doesn't exactly match any tool name but
    // shares components after normalization
    let snap = index.search_snapshot("getDashboardByUID", 5);
    assert_top_n(
        &snap,
        "grafana-ai__GetDashboardByUID",
        2,
        "getDashboardByUID",
    );
}

#[test]
fn haystack_qualified_name_typo_server() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // Model got the server prefix wrong but the tool name right
    let snap = index.search_snapshot("slack__slack_read_thread", 5);
    assert_top_n(
        &snap,
        "echo_agent_com_slack__slack_read_thread",
        3,
        "slack__slack_read_thread",
    );
}

#[test]
fn haystack_underscore_joined_natural_query() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // User types tool name with underscores as a query
    let snap = index.search_snapshot("slack_send_message", 5);
    // Exact match on bare tool name catches this
    assert_eq!(
        snap.results[0].tool_name,
        "echo_agent_com_slack__slack_send_message"
    );
}

#[test]
fn haystack_kebab_tool_partial() {
    let index = Bm25ToolSearchIndex::new(make_snapshot(production_haystack()));
    // Query uses kebab-case but is not an exact tool name
    let snap = index.search_snapshot("notion-create", 5);
    // Should find multiple notion-create-* tools
    let notion_create_count = snap
        .results
        .iter()
        .filter(|r| r.tool_name.contains("notion-create"))
        .count();
    assert!(
        notion_create_count >= 2,
        "expected multiple notion-create-* tools, got {notion_create_count}"
    );
}

#[test]
fn haystack_total_tools() {
    let tools = production_haystack();
    let expected = tools.len();
    assert!(
        expected >= 55,
        "production fixture should have 55+ tools, got {expected}"
    );
    let index = Bm25ToolSearchIndex::new(make_snapshot(tools));
    let snap = index.search_snapshot("slack_search_public", 5);
    assert_eq!(snap.total_hidden_tools, expected);
}

// ── Score comparison: before / after each rule ───────────────────
//
// Measures BM25 scores for the same queries under four configs:
//   baseline       = old to_document (only _ split) + raw query
//   +doc_norm      = new to_document (split_identifier) + raw query
//   +query_norm    = old to_document + normalize_query
//   +both          = new to_document + normalize_query
//
// Asserts that each rule independently improves the score for
// identifier-style queries and that the combined score is best.

/// Old to_document: only splits words containing `_`.
fn to_document_baseline(t: &ToolMetadata) -> String {
    let params = t.parameters.join(" ");
    let doc = format!(
        "{} {} {} {}",
        t.server_name, t.tool_name, t.description, params
    );
    let split: String = doc
        .split_whitespace()
        .filter(|w| w.contains('_'))
        .map(|w| w.replace('_', " "))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{doc} {split}")
}

/// Search BM25 and return the score for `target`, or 0.0 if not found.
fn bm25_score_for(tools: &[ToolMetadata], docs: Vec<String>, query: &str, target: &str) -> f32 {
    let engine = SearchEngineBuilder::<u32>::with_corpus(Language::English, docs).build();
    engine
        .search(query, 10)
        .iter()
        .find(|r| {
            tools
                .get(r.document.id as usize)
                .is_some_and(|t| t.qualified_name == target)
        })
        .map(|r| r.score)
        .unwrap_or(0.0)
}

#[test]
fn score_comparison_document_normalization() {
    let tools = production_haystack();
    let old_docs: Vec<String> = tools.iter().map(to_document_baseline).collect();
    let new_docs: Vec<String> = tools.iter().map(|t| t.to_document()).collect();

    // Queries that benefit from to_document splitting camelCase / kebab / __
    let cases: &[(&str, &str)] = &[
        // camelCase tool name — old doc has "SearchDashboards" as one
        // token, new doc adds "Search Dashboards"
        (
            "search dashboards visibility",
            "grafana-ai__SearchDashboards",
        ),
        // kebab-case tool name — old doc doesn't split on -,
        // new doc adds "notion create pages"
        ("create pages", "notion__notion-create-pages"),
        // qualified name in doc — old doc doesn't include qualified_name,
        // new doc indexes split components of "grafana-ai__SearchDashboards"
        ("grafana dashboards", "grafana-ai__SearchDashboards"),
        // PascalCase tool — "GetDashboardByUID" split into
        // "Get Dashboard By UID"
        ("get dashboard uid", "grafana-ai__GetDashboardByUID"),
        // kebab server name — old doc has "grafana-ai" as one token,
        // new doc adds "grafana ai"
        ("grafana alert", "grafana-ai__DeleteAlertRule"),
    ];

    for &(query, target) in cases {
        let old_score = bm25_score_for(&tools, old_docs.clone(), query, target);
        let new_score = bm25_score_for(&tools, new_docs.clone(), query, target);
        assert!(
            new_score >= old_score,
            "to_document: query {query:?} target {target:?}: \
             new ({new_score:.3}) should be >= old ({old_score:.3})"
        );
        assert!(
            new_score > 0.0,
            "to_document: query {query:?} target {target:?}: \
             new score should be > 0 (was {new_score:.3})"
        );
    }
}

#[test]
fn score_comparison_query_normalization() {
    let tools = production_haystack();
    // Use the NEW to_document for both — isolate the query normalization effect
    let docs: Vec<String> = tools.iter().map(|t| t.to_document()).collect();

    // Queries containing compound identifiers that benefit from
    // normalize_query splitting __, _, -, camelCase
    let cases: &[(&str, &str)] = &[
        // __ delimiter — "wrong_server__SearchDashboards" splits into
        // extra tokens "wrong server Search Dashboards"
        (
            "wrong_server__SearchDashboards",
            "grafana-ai__SearchDashboards",
        ),
        // partial snake_case — "search_public" adds "search public"
        ("search_public", "echo_agent_com_slack__slack_search_public"),
        // wrong server prefix — "slack__slack_read_thread" adds
        // "slack slack read thread"
        (
            "slack__slack_read_thread",
            "echo_agent_com_slack__slack_read_thread",
        ),
        // kebab query — "notion-create" adds "notion create"
        ("notion-create", "notion__notion-create-pages"),
    ];

    for &(query, target) in cases {
        let raw_score = bm25_score_for(&tools, docs.clone(), query, target);
        let normalized = normalize_query(query);
        let norm_score = bm25_score_for(&tools, docs.clone(), &normalized, target);
        assert!(
            norm_score >= raw_score,
            "normalize_query: query {query:?} target {target:?}: \
             normalized ({norm_score:.3}) should be >= raw ({raw_score:.3})"
        );
        assert!(
            norm_score > 0.0,
            "normalize_query: query {query:?} target {target:?}: \
             normalized score should be > 0 (was {norm_score:.3})"
        );
    }
}

#[test]
fn score_comparison_combined_best() {
    let tools = production_haystack();
    let old_docs: Vec<String> = tools.iter().map(to_document_baseline).collect();
    let new_docs: Vec<String> = tools.iter().map(|t| t.to_document()).collect();

    // These queries benefit from BOTH rules together
    let cases: &[(&str, &str)] = &[
        (
            "wrong_server__SearchDashboards",
            "grafana-ai__SearchDashboards",
        ),
        (
            "slack__slack_read_thread",
            "echo_agent_com_slack__slack_read_thread",
        ),
        ("notion-create", "notion__notion-create-pages"),
    ];

    for &(query, target) in cases {
        let baseline = bm25_score_for(&tools, old_docs.clone(), query, target);
        let normalized = normalize_query(query);
        let combined = bm25_score_for(&tools, new_docs.clone(), &normalized, target);
        assert!(
            combined >= baseline,
            "combined: query {query:?} target {target:?}: \
             combined ({combined:.3}) should be >= baseline ({baseline:.3})"
        );
        assert!(
            combined > 0.0,
            "combined: query {query:?} target {target:?}: \
             combined score should be > 0 (was {combined:.3})"
        );
    }
}
