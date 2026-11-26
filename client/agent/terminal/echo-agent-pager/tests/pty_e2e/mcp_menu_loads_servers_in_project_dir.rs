// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 13. **MCP menu loads in a project dir** (temp dir with a `.git` ancestor).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mcp_menu_loads_servers_in_project_dir() {
    let content = ContentController::start().await.expect("start content");

    let project = tempfile::tempdir().expect("create project dir");
    // The `.git` ancestor check precedes the system-temp exclusion in `is_project_dir`.
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    drive_mcp_menu_load(&content, project.path()).await;
}
