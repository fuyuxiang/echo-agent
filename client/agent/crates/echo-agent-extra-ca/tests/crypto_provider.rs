// The bare builder panics when ring and aws-lc-rs are both compiled in.
#[test]
fn ensure_is_idempotent_and_bare_client_config_builder_does_not_panic() {
    echo_agent_extra_ca::ensure_default_crypto_provider();
    echo_agent_extra_ca::ensure_default_crypto_provider();
    let _ = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
}

#[test]
fn rustls_client_config_builds_and_is_shared() {
    let a = echo_agent_extra_ca::rustls_client_config();
    let b = echo_agent_extra_ca::rustls_client_config();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}
