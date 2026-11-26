use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use std::io::{Read, Write};

#[test]
fn http1_only_client_stays_on_http11_against_an_h2_server() {
    let ca_key = KeyPair::generate().expect("ca key");
    let mut ca_params = CertificateParams::new(vec![]).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");

    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf_cert = CertificateParams::new(vec!["localhost".into()])
        .expect("leaf params")
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .expect("leaf cert");

    let dir = tempfile::tempdir().expect("tempdir");
    let ca_path = dir.path().join("ca.pem");
    std::fs::write(&ca_path, ca_cert.pem()).expect("write ca pem");

    // SAFETY: sole test in this binary; set before any client build resolves
    // the crate's env-backed root snapshot.
    unsafe {
        std::env::remove_var(echo_agent_extra_ca::ENV_SSL_CERT_FILE);
        std::env::set_var(
            echo_agent_extra_ca::ENV_ECHO_AGENT_EXTRA_CA_BUNDLE,
            ca_path.as_os_str(),
        );
    }

    let mut server_config = rustls::ServerConfig::builder_with_provider(
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
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut conn =
            rustls::ServerConnection::new(std::sync::Arc::new(server_config)).expect("conn");
        let mut buf = [0u8; 1024];
        {
            let mut tls = rustls::Stream::new(&mut conn, &mut sock);
            let _ = tls.read(&mut buf);
        }
        if conn.alpn_protocol() == Some(b"http/1.1".as_ref()) {
            let mut tls = rustls::Stream::new(&mut conn, &mut sock);
            let _ = tls.write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n");
        }
    });

    let client = echo_agent_extra_ca::build_blocking_reqwest_client(|builder| {
        builder
            .http1_only()
            .timeout(std::time::Duration::from_secs(10))
            .no_proxy()
    })
    .expect("test client builds");

    let resp = client
        .get(format!("https://localhost:{port}/"))
        .send()
        .expect("http1-only client negotiates http/1.1 even when the server offers h2");
    assert_eq!(resp.status(), 204);
    assert_eq!(resp.version(), reqwest::Version::HTTP_11);
}
