use super::support::*;
use super::*;
use echo_agent_core::prompt::user_message::UserMessageTemplate;
/// Helper: build an actor with `mcp_state` pre-loaded with the given
/// configs and a translated init-progress state. Reuses
/// `create_test_actor` then drives the typed transitions to match
/// the `(initialized, initializing_servers)` shape callers express.
///
/// Mapping:
/// - `initialized=false, init_servers=[]` → `InitProgress::NotStarted`
/// - `initialized=false, init_servers=[...]` → `Starting{handshaking=…}`
/// - `initialized=true,  init_servers=[]` → `Finished{handshaking=∅}`
/// - `initialized=true,  init_servers=[...]` → `Finished{handshaking=…}`
///   (early-finish + bg handshakes still in flight)
async fn actor_with_mcp(
    configs: Vec<acp::McpServer>,
    initialized: bool,
    initializing_servers: Vec<String>,
) -> SessionActor {
    let (gw_tx, _gw_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persist_tx, _persist_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = create_test_actor(100, 256_000, 80, gw_tx, persist_tx).await;
    {
        let mut state = actor.mcp_state.lock().await;
        state.configs = configs;
        state.cancel_init();
        if initialized || !initializing_servers.is_empty() {
            assert!(state.try_start_init());
            state.mark_servers_initializing(initializing_servers);
            if initialized {
                state.finish_init();
            }
        }
    }
    actor
}
fn dummy_stdio_config(name: &str) -> acp::McpServer {
    acp::McpServer::Stdio(
        acp::McpServerStdio::new(name.to_string(), "true")
            .args(vec![])
            .env(vec![]),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn returns_immediately_for_default_template() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = actor_with_mcp(
                vec![dummy_stdio_config("linear")],
                false,
                vec!["linear".into()],
            )
            .await;
            let start = std::time::Instant::now();
            actor
                .wait_for_mcp_templated_prefix_ready(&UserMessageTemplate::Default)
                .await;
            assert!(start.elapsed() < std::time::Duration::from_millis(50));
        })
        .await;
}
