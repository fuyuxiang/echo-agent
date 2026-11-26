#[test]
fn earlier_ring_install_is_preserved() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("first install wins");
    echo_agent_extra_ca::ensure_default_crypto_provider();
    let provider = rustls::crypto::CryptoProvider::get_default().expect("default installed");
    assert!(
        !provider
            .signature_verification_algorithms
            .supported_schemes()
            .contains(&rustls::SignatureScheme::ECDSA_NISTP521_SHA512),
        "ensure must not replace the host's provider (ring lacks P-521)"
    );
}
