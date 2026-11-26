//! One binary, one home: `echo_agent_home()` memoizes the first read for the process, so tests that
//! need a temp home have to share one, and `#[serial]` keeps their env writes apart.

use std::sync::{Arc, OnceLock};

use agent_client_protocol as acp;
use echo_agent_runtime::auth::{AuthManager, EchoAgentComConfig};
use echo_agent_runtime::session::info::Info;
use echo_agent_runtime::session::persistence::delete_session_history;
use echo_agent_runtime::session::storage::search::{
    IndexDecision, SearchIndex, SearchIndexManager, SessionSearchRequest, execute_search,
    start_if_enabled,
};
use echo_agent_runtime::session::storage::{JsonlStorageAdapter, StorageAdapter};
use echo_agent_test_support::EnvGuard;

fn home() -> &'static std::path::Path {
    static HOME: OnceLock<(tempfile::TempDir, EnvGuard)> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::TempDir::new().unwrap();
        let guard = EnvGuard::set("ECHO_AGENT_HOME", dir.path());
        (dir, guard)
    })
    .0
    .path()
}

/// The feature is the only door to a manager, so the test walks through it with
/// the switch left at its default rather than reaching past it.
fn start_index() -> SearchIndexManager {
    let _default_on = EnvGuard::unset("ECHO_AGENT_SESSION_SEARCH");
    match start_if_enabled(&echo_agent_runtime::agent::config::Config::default()) {
        SearchIndex::Started(index) => index,
        SearchIndex::Off { reason } => {
            panic!("session search is on by default, got off: {reason}")
        }
    }
}

/// Titles are one made-up token, searched back verbatim: a query of ordinary words ORs its
/// tokens, which would let one session's row answer for another.
async fn seed_session(root: &std::path::Path, id: &str, cwd: &str) {
    let storage = JsonlStorageAdapter::with_root(root.to_path_buf());
    let info = Info {
        id: acp::SessionId::new(id),
        cwd: cwd.to_string(),
    };
    storage
        .init_session(&info, acp::ModelId::new("test-model"))
        .await
        .unwrap();
    storage
        .update_session_title(&info, title_for(id))
        .await
        .unwrap();
}

fn title_for(id: &str) -> String {
    format!("zzqq{id}")
}

async fn finds(index: &SearchIndexManager, root: &std::path::Path, id: &str) -> bool {
    let req = SessionSearchRequest {
        query: title_for(id),
        cwd: None,
        limit: 10,
        offset: 0,
        include_content: false,
    };
    !execute_search(IndexDecision::On(index), root, &req)
        .await
        .unwrap()
        .results
        .is_empty()
}

#[tokio::test]
#[serial_test::serial]
async fn deleting_a_session_clears_only_its_own_search_row() {
    let root = home();
    let index = start_index();

    // All three up front: only the first search bootstraps.
    seed_session(root, "orphan", "/ws-a").await;
    seed_session(root, "elsewhere", "/ws-b").await;
    seed_session(root, "scoped", "/ws-c").await;
    assert!(finds(&index, root, "orphan").await, "precondition: indexed");
    assert!(
        finds(&index, root, "elsewhere").await,
        "precondition: indexed"
    );
    assert!(finds(&index, root, "scoped").await, "precondition: indexed");

    let auth = Arc::new(AuthManager::new(root, EchoAgentComConfig::default()));

    let session_dir = echo_agent_runtime::util::echo_agent_home::sessions_cwd_dir_in(root, "/ws-a")
        .join("orphan");
    std::fs::remove_dir_all(&session_dir).unwrap();
    let deletion = delete_session_history("orphan", None, false, auth.clone(), Some(&index))
        .await
        .unwrap();
    assert!(!deletion.any_removed(), "nothing was left to remove");
    assert!(
        !finds(&index, root, "orphan").await,
        "a delete with no workspace must clear a row nothing else will ever prune",
    );

    delete_session_history(
        "elsewhere",
        Some("/ws-a"),
        false,
        auth.clone(),
        Some(&index),
    )
    .await
    .unwrap();
    assert!(
        finds(&index, root, "elsewhere").await,
        "a delete scoped to another workspace must not evict this session",
    );

    let deletion = delete_session_history("scoped", Some("/ws-c"), false, auth, Some(&index))
        .await
        .unwrap();
    assert!(deletion.local_removed, "the session was there to remove");
    assert!(
        !finds(&index, root, "scoped").await,
        "a delete that removed the session must take its row too",
    );
}

/// A host that keeps no index still prunes a row from an index built earlier.
/// The manager here only reads the row back; the delete runs with `None`.
#[tokio::test]
#[serial_test::serial]
async fn deleting_a_session_evicts_its_row_without_a_handle() {
    let root = home();
    let index = start_index();

    seed_session(root, "indexless", "/ws-d").await;
    assert!(
        finds(&index, root, "indexless").await,
        "precondition: indexed"
    );

    let auth = Arc::new(AuthManager::new(root, EchoAgentComConfig::default()));
    let deletion = delete_session_history("indexless", Some("/ws-d"), false, auth, None)
        .await
        .unwrap();
    assert!(deletion.local_removed, "the session was there to remove");
    assert!(
        !finds(&index, root, "indexless").await,
        "a delete from a host with no index must still take the row",
    );
}

#[test]
#[serial_test::serial]
fn loading_config_applies_requirement_pins() {
    // Removed again before returning: the other test resolves its config from this same home.
    let pin = home().join("requirements.toml");
    std::fs::write(&pin, "[features]\nsession_search = false\n").unwrap();

    let loaded = echo_agent_runtime::config::load_agent_config_disk_only();
    std::fs::remove_file(&pin).unwrap();
    let config = loaded.expect("config loads");

    let resolved = config.feature(echo_agent_runtime::agent::config::Feature::SessionSearch);
    assert!(
        !resolved.value,
        "a one-shot command must apply pins, or the environment outranks them",
    );
    assert_eq!(
        resolved.source,
        echo_agent_runtime::agent::config::ConfigSource::Requirement
    );
}
