use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use std::io::{Read, Write};

#[test]
fn handshake_succeeds_against_ca_loaded_from_ssl_cert_file() {
    let ca_key = KeyPair::generate().expect("ca key");
    let mut ca_params = CertificateParams::new(vec![]).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");

    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf_params = CertificateParams::new(vec!["localhost".into()]).expect("leaf params");
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .expect("leaf cert");

    let dir = tempfile::tempdir().expect("tempdir");
    let ca_path = dir.path().join("ca.pem");
    std::fs::write(&ca_path, ca_cert.pem()).expect("write ca pem");

    // Safety: sole test in this binary; set before any OnceLock resolve.
    unsafe {
        std::env::remove_var(echo_agent_extra_ca::ENV_ECHO_AGENT_EXTRA_CA_BUNDLE);
        std::env::set_var(echo_agent_extra_ca::ENV_SSL_CERT_FILE, ca_path.as_os_str());
    }

    // The loader read the CA from SSL_CERT_FILE and attributes it to that var.
    assert_eq!(
        echo_agent_extra_ca::extra_root_ders(),
        &[ca_cert.der().as_ref().to_vec()]
    );
    assert_eq!(
        echo_agent_extra_ca::configured_bundle_env(),
        Some(echo_agent_extra_ca::ENV_SSL_CERT_FILE)
    );

    let server_config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf_cert.der().clone(), ca_cert.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    )
    .expect("server config");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut conn =
            rustls::ServerConnection::new(std::sync::Arc::new(server_config)).expect("conn");
        let mut tls = rustls::Stream::new(&mut conn, &mut sock);
        let mut buf = [0u8; 1024];
        let _ = tls.read(&mut buf);
        let _ = tls.write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n");
    });

    // Native roots off: the CA is trusted only if the crate's SSL_CERT_FILE
    // loader added it (rustls-native-certs also reads SSL_CERT_FILE, so leaving
    // native roots on would let the handshake pass without the loader).
    #[expect(clippy::expect_used)]
    let client = echo_agent_extra_ca::build_blocking_reqwest_client(|builder| {
        builder
            .timeout(std::time::Duration::from_secs(10))
            .no_proxy()
            .tls_built_in_native_certs(false)
    })
    .expect("test client builds");

    let resp = client
        .get(format!("https://localhost:{port}/"))
        .send()
        .expect("handshake against SSL_CERT_FILE-trusted CA succeeds");
    assert_eq!(resp.status(), 204);
}
