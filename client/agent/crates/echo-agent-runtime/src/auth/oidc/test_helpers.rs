//! Shared test helpers for `oidc::protocol::tests` and `oidc::login::tests`.
//! Both test modules need a mock IdP server (`start_mock_idp`), JWT
//! signing primitives (`generate_test_rsa_key`, `mock_idp_token`), and
//! the same constants. Extracted here so neither test mod has to
//! re-implement them.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::protocol::{Discovery, discover};

pub(super) const TEST_KID: &str = "test-kid";
pub(super) fn test_nonce() -> String {
    format!("tn-{:x}", std::process::id())
}
pub(super) const TEST_CLIENT_ID: &str = "test-client-id";
pub(super) fn ensure_crypto_provider() {
    echo_agent_extra_ca::ensure_default_crypto_provider();
    let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
}
pub(super) fn generate_test_rsa_key() -> (String, String, String) {
    use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
    use aws_lc_rs::rsa::{KeyPair, KeySize};
    use aws_lc_rs::signature::KeyPair as _;
    let private_key = KeyPair::generate(KeySize::Rsa2048).unwrap();
    let der: Pkcs8V1Der<'static> = private_key.as_der().unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
    let body = encoded
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n");
    let public_key = private_key.public_key();
    let jwk_n = URL_SAFE_NO_PAD.encode(public_key.modulus().big_endian_without_leading_zero());
    let jwk_e = URL_SAFE_NO_PAD.encode(public_key.exponent().big_endian_without_leading_zero());
    (pem, jwk_n, jwk_e)
}
pub(super) async fn mock_idp_token() -> (String, String, Discovery, tokio::task::JoinHandle<()>) {
    let (issuer, handle) = start_mock_idp().await;
    let discovery = discover(&issuer).await.unwrap();
    let resp: serde_json::Value = crate::http::shared_client()
        .post(&discovery.token_endpoint)
        .form(&[("grant_type", "authorization_code")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id_token = resp["id_token"]
        .as_str()
        .expect("mock missing id_token")
        .to_string();
    (issuer, id_token, discovery, handle)
}

// A listener's port can be reused while the previous fixture's discovery is cached.
pub(super) fn mock_issuer(listener: &tokio::net::TcpListener) -> (String, String) {
    let path = format!("/oidc-test/{}", uuid::Uuid::new_v4());
    let issuer = format!("http://{}{path}", listener.local_addr().unwrap());
    (issuer, path)
}

pub(super) async fn start_mock_idp() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (issuer, issuer_path) = mock_issuer(&listener);
    let issuer_for_discovery = issuer.clone();
    let (rsa_pem, jwk_n, jwk_e) = generate_test_rsa_key();

    #[derive(serde::Serialize)]
    struct Claims {
        sub: &'static str,
        email: &'static str,
        iss: String,
        aud: &'static str,
        nonce: String,
        exp: usize,
    }

    let id_token = {
        let mut hdr = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        hdr.kid = Some(TEST_KID.to_owned());
        jsonwebtoken::encode(
            &hdr,
            &Claims {
                sub: "user-42",
                email: "test@corp.com",
                iss: issuer.clone(),
                aud: TEST_CLIENT_ID,
                nonce: test_nonce(),
                exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as usize,
            },
            &jsonwebtoken::EncodingKey::from_rsa_pem(rsa_pem.as_bytes()).unwrap(),
        )
        .unwrap()
    };

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let iss = issuer_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{iss}/authorize"),
                        "token_endpoint": format!("{iss}/token"),
                        "jwks_uri": format!("{iss}/jwks"),
                        "id_token_signing_alg_values_supported": ["RS256"],
                    }))
                }
            }),
        )
        .route(
            "/jwks",
            axum::routing::get(move || {
                let n = jwk_n.clone();
                let e = jwk_e.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "keys": [{
                            "kty": "RSA", "alg": "RS256", "kid": TEST_KID,
                            "n": n, "e": e,
                        }]
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(move || {
                let tok = id_token.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "access_token": "mock-access-token",
                        "refresh_token": "mock-refresh-token",
                        "id_token": tok,
                        "expires_in": 3600,
                    }))
                }
            }),
        );

    let app = axum::Router::new().nest(&issuer_path, app);
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (issuer, handle)
}
