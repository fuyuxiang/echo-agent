use super::*;

// Self-signed PEMs for unit tests only (CN=test-extra-ca-1 / -2).
const VALID_CERT_1: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDFTCCAf2gAwIBAgIUT2czXTuxSAjDjEh92UMB1OVahZYwDQYJKoZIhvcNAQEL\n\
BQAwGjEYMBYGA1UEAwwPdGVzdC1leHRyYS1jYS0xMB4XDTI2MDcyOTE4MzUwNFoX\n\
DTM2MDcyNjE4MzUwNFowGjEYMBYGA1UEAwwPdGVzdC1leHRyYS1jYS0xMIIBIjAN\n\
BgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA1gNk2BQwUy+n5cCaTFtGpSzVQv//\n\
d7QD+3QWeE411wIGJzp3nrd7np55X8JHxeg/pRhspQvLQAF7bt55LSkL/+sSth3S\n\
QTbBqhftic9CXik3llAwbdQkAM9srz5zXWW9KVjZ57dxjjxrS15SCXu/UmvGZy98\n\
faJcS++TRkczsNFzwQEqeDYARVc/no0C0I++NhGLPaNMfFAevvnu6Kt3CYMI5ls4\n\
KCFgnlau4CjgRCMSfRDCRcwEwUAp+DyX9IU+tvDAQY1ncVoa/05tvaEvw7pQ+UgW\n\
0wRG0lk7PLlcWmUkLcFpO+sL5GRkC8RoWM4cFbIOiXoVxUFks/z2y0GCEQIDAQAB\n\
o1MwUTAdBgNVHQ4EFgQU+lyC70W5aR6BIf4VNtjfiWMNzzkwHwYDVR0jBBgwFoAU\n\
+lyC70W5aR6BIf4VNtjfiWMNzzkwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0B\n\
AQsFAAOCAQEA02972nA7LshRgubz6BwXbh1gA5pLzTd5KEae+94Hq6mP2zJ1T0gk\n\
x+me0NtSgG4BJLdBIylUzo2UmsfB/sz+ght6WX1uB38Vc2UQsp0sRPeeiMovSd6n\n\
I7xZyuZEF3noYJVBBlKQ8XsCUIBNIROlyKlNjNcWY8tGqPh9cepvtZYkBgRZr1vW\n\
hJAE3EOL2ZddrMPF64QeU9UhvCm0Ch+Ceqa1ZWE0MygccggX5s2yQwtXO2ovJdjH\n\
6vW0I02r8sE+NX0d1u8rIPJEKlp89UwCwniD7SxHTNw8bbsTCWz+AMod7vC7De3X\n\
4Daxme+vD8adOfCeOIu5vNrlXLNST2yaTw==\n\
-----END CERTIFICATE-----\n";

const VALID_CERT_2: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDFTCCAf2gAwIBAgIUKckMakNVssdBbRUlVtyWZZPx7EcwDQYJKoZIhvcNAQEL\n\
BQAwGjEYMBYGA1UEAwwPdGVzdC1leHRyYS1jYS0yMB4XDTI2MDcyOTE4MzUwNFoX\n\
DTM2MDcyNjE4MzUwNFowGjEYMBYGA1UEAwwPdGVzdC1leHRyYS1jYS0yMIIBIjAN\n\
BgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA3pVKr4xNdWm+RIYVRuOv+8Pg3I3/\n\
wsmC7m84I4bw6EofraYY1vTT8XYcWAspo++Tj1hYNAyfdtdrgdZT8dgsTqsVPzYz\n\
rluGu03mu0aE9Ix2IieLvR9C0s+mYpsfCQYRjsL2wDD6fOAWN4wjj1R4XGgUZKCF\n\
q8JirftcRBLGjAa8XXD496dUGXzURQ7C9jAxFmPWGbyz3f1ymOLBvp8RdzrJNCsA\n\
zdEjqJODMMf0czJH5gtt06hIQG9JkPHNqZXVxEIBIDlkmkr9Nk/asqZGhbHILkHX\n\
/jqfdOMb4Xu95iglbwbACgAtfysNQdjUU7hbjKxx4S4FCjf+gyb4whQo/QIDAQAB\n\
o1MwUTAdBgNVHQ4EFgQUVrqEwVrKpoc/GinOYZR13TkjdwgwHwYDVR0jBBgwFoAU\n\
VrqEwVrKpoc/GinOYZR13TkjdwgwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0B\n\
AQsFAAOCAQEAtK9ylmMIEQsuYm5Qo1pi4xp5rFywO0g5zkWEl/fIMBevP9Thhnco\n\
gHiOFBhQcuo+Go65p3Fbbt3Vrx30Oi0hQUlYLlY44BO3/TgfZ0VbIheeDfyYaq97\n\
S3I1cLHJ1qmKq99zKcqvCcD+NmifbuMi03Zo35Kp+jm8GXpONumnPlu17WZLw5N7\n\
KFHbC1eO3iat27z4WRhPHG4vmPfMHIIvrbA+aEwc1b88NO5UdRmSHvkt4MDEOsIe\n\
IgKmdcW5+BG5ffCRJ9wNsCCy165AFUmuNWz0aqDWybjK4eiEb88sHKbVv7fyXpwi\n\
YwiFroodmakt1behpPy1p9Ih94MTqy9pQw==\n\
-----END CERTIFICATE-----\n";

/// Valid PEM framing / base64, but DER is not an X.509 certificate.
const INVALID_DER_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MAMBAf8=\n\
-----END CERTIFICATE-----\n";

#[test]
fn parse_garbage_non_pem_flags_no_blocks_without_panic() {
    let o = parse_and_validate_pem(b"this is not a certificate");
    assert!(o.accepted.is_empty());
    assert_eq!(o.rejected, 0);
    assert!(o.no_pem_blocks);
}

#[test]
fn parse_valid_single_cert_pem() {
    let o = parse_and_validate_pem(VALID_CERT_1.as_bytes());
    assert_eq!(o.accepted.len(), 1);
    assert_eq!(o.rejected, 0);
    assert!(!o.no_pem_blocks);
}

#[test]
fn parse_invalid_der_pem_rejected() {
    let o = parse_and_validate_pem(INVALID_DER_PEM.as_bytes());
    assert!(o.accepted.is_empty());
    assert!(o.rejected >= 1);
    assert!(!o.no_pem_blocks);
}

#[test]
fn parse_mixed_bundle_keeps_valid_drops_invalid() {
    let o = parse_and_validate_pem(
        format!("{VALID_CERT_1}\n{INVALID_DER_PEM}\n{VALID_CERT_2}").as_bytes(),
    );
    assert_eq!(o.accepted.len(), 2);
    assert!(o.rejected >= 1);
}

#[test]
fn validated_ders_build_reqwest_client() {
    let o = parse_and_validate_pem(VALID_CERT_1.as_bytes());
    assert_eq!(o.accepted.len(), 1);
    let mut builder = reqwest::Client::builder();
    for der in &o.accepted {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_der(der).expect("from_der after rustls validation"),
        );
    }
    builder
        .build()
        .expect("client with validated roots must construct");
}

#[test]
fn read_bundle_capped_rejects_oversized() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.pem");
    std::fs::write(&path, vec![b'A'; (MAX_EXTRA_CA_BUNDLE_BYTES as usize) + 1]).unwrap();
    match read_bundle_capped(&path) {
        Err(BundleReadError::TooLarge) => {}
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn read_bundle_capped_accepts_at_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ok.pem");
    std::fs::write(&path, vec![b'B'; MAX_EXTRA_CA_BUNDLE_BYTES as usize]).unwrap();
    let got = read_bundle_capped(&path).expect("at-limit read");
    assert_eq!(got.len(), MAX_EXTRA_CA_BUNDLE_BYTES as usize);
}

#[test]
fn openssl_trust_bundle_parses_to_exactly_the_certificate() {
    // The RHEL trust-bundle shape; covers label normalization and trimming.
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = rcgen::CertificateParams::new(vec![])
        .expect("params")
        .self_signed(&key)
        .expect("cert");
    let der = cert.der().as_ref().to_vec();
    let mut with_aux = der.clone();
    with_aux.extend_from_slice(&[0x30, 0x03, 0x02, 0x01, 0x01]);

    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&with_aux);
    let mut pem = String::from("-----BEGIN TRUSTED CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("ascii"));
        pem.push('\n');
    }
    pem.push_str("-----END TRUSTED CERTIFICATE-----\n");

    let out = parse_and_validate_pem(pem.as_bytes());
    assert_eq!(
        out.accepted,
        vec![der],
        "aux bytes must be trimmed, cert kept"
    );
    assert_eq!(out.rejected, 0);
}

#[test]
fn first_der_item_parses_der_length_forms_and_rejects_malformed() {
    // Short form: exact fit, then trailing bytes past the length dropped.
    assert_eq!(
        first_der_item(&[0x30, 0x02, 0xAA, 0xBB]),
        Some(&[0x30, 0x02, 0xAA, 0xBB][..])
    );
    assert_eq!(
        first_der_item(&[0x30, 0x01, 0xAA, 0xFF, 0xFF]),
        Some(&[0x30, 0x01, 0xAA][..])
    );
    // Long form (one length octet), and a whole-blob SEQUENCE passes unchanged.
    assert_eq!(
        first_der_item(&[0x30, 0x81, 0x01, 0xAA]),
        Some(&[0x30, 0x81, 0x01, 0xAA][..])
    );
    assert_eq!(
        first_der_item(&[0x30, 0x03, 0x02, 0x01, 0x07]),
        Some(&[0x30, 0x03, 0x02, 0x01, 0x07][..])
    );
    // Rejected: non-SEQUENCE tag, indefinite length, over-claimed length, truncated header.
    assert_eq!(first_der_item(&[0x31, 0x02, 0xAA, 0xBB]), None);
    assert_eq!(first_der_item(&[0x30, 0x80, 0xAA]), None);
    assert_eq!(first_der_item(&[0x30, 0x05, 0xAA]), None);
    assert_eq!(first_der_item(&[0x30]), None);
}

#[test]
fn native_trust_store_loads_once_across_many_builds() {
    for _ in 0..4 {
        build_reqwest_client(|builder| builder).expect("h2 client");
        build_reqwest_client(|builder| builder.http1_only()).expect("http1 client");
    }
    assert_eq!(
        NATIVE_ROOT_LOADS.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the OS trust store must load exactly once no matter how many clients build"
    );
}

#[test]
fn select_bundle_precedence() {
    use std::ffi::OsString;
    let os = OsString::from;
    let p = std::path::PathBuf::from;
    // Bundle wins; empty disables both; empty is treated as unset.
    assert_eq!(
        select_bundle(Some(os("/b")), Some(os("/s"))),
        Some((ENV_ECHO_AGENT_EXTRA_CA_BUNDLE, p("/b")))
    );
    assert_eq!(select_bundle(Some(os("")), Some(os("/s"))), None);
    assert_eq!(
        select_bundle(None, Some(os("/s"))),
        Some((ENV_SSL_CERT_FILE, p("/s")))
    );
    assert_eq!(select_bundle(None, Some(os(""))), None);
    assert_eq!(select_bundle(None, None), None);
}

#[test]
fn load_fails_open_on_read_errors() {
    // Both read_bundle_capped failures (missing file, over the size cap) yield
    // zero roots rather than propagating, so the size cap stays on the load path.
    assert!(
        load_extra_root_ders(
            ENV_ECHO_AGENT_EXTRA_CA_BUNDLE,
            std::path::Path::new("/nonexistent/echo-agent-ca.pem"),
        )
        .is_empty()
    );

    let dir = tempfile::tempdir().unwrap();
    let oversized = dir.path().join("huge.pem");
    std::fs::write(
        &oversized,
        vec![b'A'; (MAX_EXTRA_CA_BUNDLE_BYTES as usize) + 1],
    )
    .unwrap();
    assert!(load_extra_root_ders(ENV_ECHO_AGENT_EXTRA_CA_BUNDLE, &oversized).is_empty());
}
