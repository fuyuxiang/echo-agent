#[test]
fn websocket_config_offers_http11_only() {
    let config = echo_agent_extra_ca::rustls_client_config();
    assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
}
