//! Isolated binary so `echo_agent_home()`'s process-wide OnceLock initializes from
//! our `ECHO_AGENT_HOME`. A lib-test EnvGuard is a no-op if another test already
//! resolved it, and then doctor reads the real ~/.echo-agent.

use std::path::PathBuf;
use std::sync::OnceLock;

fn isolate_home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::TempDir::new().unwrap().keep();
        let echoagent = dir.join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(echoagent.join("config.toml"), "").unwrap();
        // SAFETY: this binary's only test; set before any echo_agent_home() call.
        unsafe {
            std::env::set_var("HOME", &dir);
            std::env::set_var("USERPROFILE", &dir);
            std::env::set_var("ECHO_AGENT_HOME", &echoagent);
        }
        dir
    })
}

#[tokio::test]
async fn run_doctor_skips_managed_gateway_without_configs_probe() {
    let _home = isolate_home();
    let cwd = tempfile::tempdir().unwrap();

    let report = echo_agent_runtime::mcp_doctor::run_doctor(cwd.path(), None).await;
    assert!(
        !report
            .sources
            .iter()
            .any(|s| s.path == "cloud.echo-agent.invalid"),
        "doctor must not invent a cloud.echo-agent.invalid source: {:?}",
        report.sources
    );
    assert!(
        report.servers.is_empty(),
        "isolated cwd must not probe managed HTTP servers: {:?}",
        report.servers
    );
}
