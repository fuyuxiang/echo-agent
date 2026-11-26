//! `reqwest-middleware` layer: stamps auth headers and retries on 401.
//! Gated behind the `middleware` cargo feature.

use std::sync::Arc;

use reqwest::{Request, Response, StatusCode, header::HeaderValue};
use reqwest_middleware::{Error, Middleware, Next};

use crate::AuthCredentialProvider;
use crate::bearer_fragment::bearer_suffix;

/// Tail fragment of the bearer
/// this middleware stamped, recorded into the request's `http::Extensions`
/// at stamp time. 401-attribution sites read it back via
/// [`execute_with_stamp`] instead of re-resolving at record time, which
/// races with the refresh the 401 itself triggers. Absent ⇒ nothing was
/// stamped; a retry overwrites it, so it always describes the attempt whose
/// response the caller holds. Only the tail crosses this boundary — JWT
/// heads are a shared constant, and the tail is safe for sinks to log.
#[derive(Clone, Debug)]
pub struct StampedBearerSuffix(pub String);

/// Optional live permission checked at the actual send boundary, including
/// retries after asynchronous credential refresh.
#[derive(Clone)]
pub struct RequestPermit(pub Arc<dyn Fn() -> bool + Send + Sync>);

pub async fn execute_with_permit(
    client: &reqwest_middleware::ClientWithMiddleware,
    req: Request,
    permit: RequestPermit,
) -> reqwest_middleware::Result<Response> {
    let mut extensions = http::Extensions::new();
    extensions.insert(permit);
    client.execute_with_extensions(req, &mut extensions).await
}

fn check_permit(extensions: &http::Extensions) -> reqwest_middleware::Result<()> {
    if extensions
        .get::<RequestPermit>()
        .is_some_and(|permit| !(permit.0)())
    {
        return Err(Error::Middleware(
            std::io::Error::other("Request permission changed before sending").into(),
        ));
    }
    Ok(())
}

/// Execute `req` on a middleware-wrapped client and return the response
/// together with the [`StampedBearerSuffix`] the auth middleware recorded
/// (if it stamped anything). The one blessed way for 401-attribution
/// call sites to learn what was actually sent on the wire.
pub async fn execute_with_stamp(
    client: &reqwest_middleware::ClientWithMiddleware,
    req: Request,
) -> reqwest_middleware::Result<(Response, Option<StampedBearerSuffix>)> {
    let mut ext = http::Extensions::new();
    let resp = client.execute_with_extensions(req, &mut ext).await?;
    Ok((resp, ext.get::<StampedBearerSuffix>().cloned()))
}

pub struct AuthRetryMiddleware {
    credentials: Arc<dyn AuthCredentialProvider>,
    max_retries: u32,
}

impl AuthRetryMiddleware {
    pub fn new(credentials: Arc<dyn AuthCredentialProvider>, max_retries: u32) -> Self {
        Self {
            credentials,
            max_retries,
        }
    }
}

fn apply_auth_header(req: &mut Request, token: &str, extensions: &mut http::Extensions) {
    match HeaderValue::from_str(&format!("Bearer {token}")) {
        Ok(val) => {
            req.headers_mut()
                .insert(reqwest::header::AUTHORIZATION, val);
            extensions.insert(StampedBearerSuffix(bearer_suffix(token).to_string()));
        }
        Err(e) => {
            tracing::warn!(error = %e, "auth retry: failed to build Authorization header");
        }
    }
}

#[async_trait::async_trait]
impl Middleware for AuthRetryMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> Result<Response, Error> {
        if let Some(ref token) = self.credentials.snapshot().token {
            apply_auth_header(&mut req, token, extensions);
        }

        let backup = req.try_clone();
        check_permit(extensions)?;
        let resp = next.clone().run(req, extensions).await?;

        if resp.status() != StatusCode::UNAUTHORIZED || self.max_retries == 0 {
            return Ok(resp);
        }
        let Some(backup) = backup else {
            return Ok(resp);
        };

        let mut last_resp = resp;
        for _ in 0..self.max_retries {
            if !self.credentials.refresh_after_unauthorized().await {
                break;
            }
            let Some(ref token) = self.credentials.snapshot().token else {
                break;
            };
            let Some(mut retry) = backup.try_clone() else {
                break;
            };
            apply_auth_header(&mut retry, token, extensions);
            check_permit(extensions)?;
            last_resp = next.clone().run(retry, extensions).await?;
            if last_resp.status() != StatusCode::UNAUTHORIZED {
                return Ok(last_resp);
            }
        }

        Ok(last_resp)
    }
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialSnapshot, HttpAuth};
    use reqwest_middleware::ClientBuilder;
    use std::sync::Mutex;

    struct MockProvider {
        token: Mutex<Option<String>>,
        refresh_result: bool,
        refresh_count: Mutex<u32>,
    }

    impl MockProvider {
        fn new(token: Option<&str>, refresh_result: bool) -> Self {
            Self {
                token: Mutex::new(token.map(|s| s.to_owned())),
                refresh_result,
                refresh_count: Mutex::new(0),
            }
        }

        fn refresh_count(&self) -> u32 {
            *self.refresh_count.lock().unwrap()
        }
    }

    impl HttpAuth for MockProvider {
        fn apply(&self, b: reqwest::RequestBuilder, _: &str) -> reqwest::RequestBuilder {
            b
        }
    }

    #[async_trait::async_trait]
    impl AuthCredentialProvider for MockProvider {
        fn snapshot(&self) -> CredentialSnapshot {
            CredentialSnapshot {
                token: self.token.lock().unwrap().clone(),
                ..Default::default()
            }
        }

        async fn refresh_after_unauthorized(&self) -> bool {
            *self.refresh_count.lock().unwrap() += 1;
            self.refresh_result
        }
    }

    async fn build_client(
        provider: Arc<dyn AuthCredentialProvider>,
        max_retries: u32,
    ) -> reqwest_middleware::ClientWithMiddleware {
        ClientBuilder::new(reqwest::Client::new())
            .with(AuthRetryMiddleware::new(provider, max_retries))
            .build()
    }

    #[tokio::test]
    async fn live_request_permit_prevents_401_retry_after_refresh() {
        let mut server = mockito::Server::new_async().await;
        let request = server
            .mock("GET", "/")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;
        let provider = Arc::new(MockProvider::new(Some("token"), true));
        let client = build_client(provider.clone(), 1).await;
        let current = provider.clone();
        let result = execute_with_permit(
            &client,
            client.get(server.url()).build().unwrap(),
            RequestPermit(Arc::new(move || current.refresh_count() == 0)),
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("permission changed")
        );
        assert_eq!(provider.refresh_count(), 1);
        request.assert_async().await;
    }

    #[tokio::test]
    async fn test_401_no_refresh_returns_401() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;

        let p = Arc::new(MockProvider::new(Some("tok"), false));
        let client = build_client(p.clone(), 1).await;

        let resp = client.get(server.url()).send().await.unwrap();
        assert_eq!(resp.status(), 401);
        assert_eq!(p.refresh_count(), 1);
        m.assert_async().await;
    }

    /// Simulates a real auth manager: starts with stale token, refresh swaps to fresh.
    struct SimulatedAuthManager {
        token: Mutex<Option<String>>,
        fresh_token: String,
        refresh_count: Mutex<u32>,
    }

    impl SimulatedAuthManager {
        fn simulated(stale: &str, fresh: &str) -> Self {
            Self {
                token: Mutex::new(Some(stale.to_owned())),
                fresh_token: fresh.to_owned(),
                refresh_count: Mutex::new(0),
            }
        }
    }

    impl HttpAuth for SimulatedAuthManager {
        fn apply(&self, b: reqwest::RequestBuilder, _: &str) -> reqwest::RequestBuilder {
            b
        }
    }

    #[async_trait::async_trait]
    impl AuthCredentialProvider for SimulatedAuthManager {
        fn snapshot(&self) -> CredentialSnapshot {
            CredentialSnapshot {
                token: self.token.lock().unwrap().clone(),
                ..Default::default()
            }
        }

        async fn refresh_after_unauthorized(&self) -> bool {
            *self.refresh_count.lock().unwrap() += 1;
            *self.token.lock().unwrap() = Some(self.fresh_token.clone());
            true
        }
    }

    #[tokio::test]
    async fn test_e2e_stale_token_refreshed_and_retried() {
        let mut server = mockito::Server::new_async().await;

        let m401 = server
            .mock("GET", "/api")
            .match_header("authorization", "Bearer stale-token")
            .with_status(401)
            .create_async()
            .await;
        let m200 = server
            .mock("GET", "/api")
            .match_header("authorization", "Bearer fresh-token")
            .with_status(200)
            .with_body(r#"{"ok":true}"#)
            .create_async()
            .await;

        let p = Arc::new(SimulatedAuthManager::simulated(
            "stale-token",
            "fresh-token",
        ));
        let client = build_client(p.clone(), 1).await;

        let resp = client
            .get(format!("{}/api", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(*p.refresh_count.lock().unwrap(), 1);
        m401.assert_async().await;
        m200.assert_async().await;
    }

    #[tokio::test]
    async fn test_e2e_auth_header_stamped_automatically() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api")
            .match_header("authorization", "Bearer my-token")
            .with_status(200)
            .create_async()
            .await;

        let p = Arc::new(MockProvider::new(Some("my-token"), false));
        let client = build_client(p.clone(), 1).await;

        let resp = client
            .get(format!("{}/api", server.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(p.refresh_count(), 0);
        mock.assert_async().await;
    }

    /// The stamp must describe the bearer of the attempt whose response
    /// the caller holds: after a 401 → refresh → retry, that is the
    /// FRESH token, not the stale one stamped on the first attempt.
    #[tokio::test]
    async fn execute_with_stamp_reports_last_stamped_bearer() {
        let mut server = mockito::Server::new_async().await;
        let m401 = server
            .mock("GET", "/api")
            .match_header("authorization", "Bearer stale-token")
            .with_status(401)
            .create_async()
            .await;
        let m200 = server
            .mock("GET", "/api")
            .match_header("authorization", "Bearer fresh-token")
            .with_status(200)
            .create_async()
            .await;

        let p = Arc::new(SimulatedAuthManager::simulated(
            "stale-token",
            "fresh-token",
        ));
        let client = build_client(p, 1).await;

        let req = client.get(format!("{}/api", server.url())).build().unwrap();
        let (resp, stamp) = execute_with_stamp(&client, req).await.unwrap();
        assert_eq!(resp.status(), 200);
        // ≤ 12 chars → the suffix is the whole token.
        assert_eq!(stamp.expect("bearer was stamped").0, "fresh-token");
        m401.assert_async().await;
        m200.assert_async().await;
    }

    /// No credential ⇒ no stamp: attribution must see "nothing was sent",
    /// not an empty string or a stale record.
    #[tokio::test]
    async fn execute_with_stamp_is_none_when_nothing_stamped() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/")
            .with_status(401)
            .create_async()
            .await;

        let p = Arc::new(MockProvider::new(None, false));
        let client = build_client(p, 0).await;

        let req = client.get(server.url()).build().unwrap();
        let (resp, stamp) = execute_with_stamp(&client, req).await.unwrap();
        assert_eq!(resp.status(), 401);
        assert!(stamp.is_none(), "no credential must mean no stamp");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn test_max_retries_bounds_attempts() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/")
            .with_status(401)
            .expect(4)
            .create_async()
            .await;

        let p = Arc::new(MockProvider::new(Some("tok"), true));
        let client = build_client(p.clone(), 3).await;

        let resp = client.get(server.url()).send().await.unwrap();
        assert_eq!(resp.status(), 401);
        assert_eq!(p.refresh_count(), 3);
        m.assert_async().await;
    }
}
