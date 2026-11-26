// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 14. **MCP menu loads in a non-project dir** (fake `$HOME` as cwd).
/// The menu must populate the same way it does inside a project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mcp_menu_loads_servers_in_non_project_dir() {
    let content = ContentController::start().await.expect("start content");

    // cwd == home_dir() classifies as non-project.
    let home = content.home().to_path_buf();
    drive_mcp_menu_load(&content, &home).await;
}
