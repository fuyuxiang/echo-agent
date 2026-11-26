//! `ECHO_AGENT_HOME` override tests in an isolated binary so `echo_agent_home()`'s
//! process-wide `OnceLock` initializes from the overridden env var.

use std::path::PathBuf;

#[test]
#[serial_test::serial(ECHO_AGENT_HOME)]
fn echo_agent_home_override_path_helpers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let echo_agent_home = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", &echo_agent_home);
    }

    assert_eq!(
        echo_agent_pager::util::pager_toml_path(),
        echo_agent_home.join("pager.toml")
    );
    assert_eq!(
        echo_agent_pager::util::display_echo_agent_home_prefix(),
        "$ECHO_AGENT_HOME"
    );
    assert_eq!(
        echo_agent_pager::util::display_user_echo_agent_path("config.toml"),
        "$ECHO_AGENT_HOME/config.toml"
    );

    let memory_path = echo_agent_home.join("memory/MEMORY.md");
    assert_eq!(
        echo_agent_pager::util::abbreviate_path(&memory_path.display().to_string()),
        "$ECHO_AGENT_HOME/memory/MEMORY.md"
    );

    // Copy-toast paths follow the same abbreviation convention, so a custom
    // $ECHO_AGENT_HOME outside $HOME still displays short.
    assert_eq!(
        echo_agent_pager::clipboard::display_copy_path(&echo_agent_home.join("last-copy.txt")),
        "$ECHO_AGENT_HOME/last-copy.txt"
    );

    assert!(echo_agent_pager::util::is_under_user_echo_agent_home(
        &memory_path
    ));
    assert!(!echo_agent_pager::util::is_under_user_echo_agent_home(
        PathBuf::from("/tmp/other").as_path()
    ));
}

/// Isolated because `echo_agent_home()`'s `OnceLock` is already initialized by the
/// time the shared lib-test binary reaches a case like this.
#[test]
#[serial_test::serial(ECHO_AGENT_HOME)]
fn disk_usage_run_creates_no_echo_agent_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ghost = tmp.path().join("ghost-home");
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", &ghost);
    }

    for json in [false, true] {
        echo_agent_pager::disk_usage_cmd::run(echo_agent_pager::disk_usage_cmd::DiskUsageArgs {
            json,
        })
        .expect("a missing home is not an error");
        assert!(
            !ghost.exists(),
            "echoagent du must not create the home it reports on (json={json})"
        );
    }
}
