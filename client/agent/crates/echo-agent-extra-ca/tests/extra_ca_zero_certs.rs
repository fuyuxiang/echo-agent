//! Process-isolated: configured garbage file → zero roots; client still builds.

#[test]
fn configured_garbage_file_yields_zero_roots_and_builds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("garbage.pem");
    std::fs::write(&path, b"not a pem at all").expect("write");

    // Safety: sole test in this binary; set before any OnceLock resolve.
    unsafe {
        std::env::set_var(
            echo_agent_extra_ca::ENV_ECHO_AGENT_EXTRA_CA_BUNDLE,
            path.as_os_str(),
        );
    }

    assert!(echo_agent_extra_ca::extra_root_ders().is_empty());

    echo_agent_extra_ca::build_reqwest_client(|builder| builder)
        .expect("client builds after zero-cert configured file");
}
