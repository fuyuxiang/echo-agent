//! Shared helpers for echo-agent-runtime integration tests.

use echo_agent_runtime::sampling::{ApiBackend, Client, SamplerConfig};

#[cfg(unix)]
pub mod leader {
    use std::future::Future;
    use std::io;
    use std::pin::Pin;

    use echo_agent_test_support::leader::{LeaderFixture, LeaderStdioClient};
    use futures::FutureExt as _;

    #[allow(dead_code)]
    pub type TestBody<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

    type PanicPayload = Box<dyn std::any::Any + Send>;

    fn finish_body(body_result: Result<(), PanicPayload>, cleanup_error: Option<io::Error>) {
        match body_result {
            Ok(()) => {
                if let Some(error) = cleanup_error {
                    panic!("leader integration cleanup failed: {error}");
                }
            }
            Err(payload) => {
                if let Some(error) = cleanup_error {
                    eprintln!("leader integration cleanup after panic failed: {error}");
                }
                std::panic::resume_unwind(payload);
            }
        }
    }

    trait CleanupClient {
        async fn graceful_close(&mut self) -> io::Result<()>;
        async fn hard_close(&mut self) -> io::Result<()>;
        fn contain_failed_cleanup_for_unwind(&mut self);
    }

    impl CleanupClient for LeaderStdioClient {
        async fn graceful_close(&mut self) -> io::Result<()> {
            self.close().await.map(|_| ())
        }

        async fn hard_close(&mut self) -> io::Result<()> {
            self.kill_and_close().await.map(|_| ())
        }

        fn contain_failed_cleanup_for_unwind(&mut self) {
            LeaderStdioClient::contain_failed_cleanup_for_unwind(self);
        }
    }

    trait CleanupFixture {
        async fn close_fixture(&self) -> io::Result<()>;
        fn contain_failed_cleanup_for_unwind(&self);
    }

    impl CleanupFixture for LeaderFixture {
        async fn close_fixture(&self) -> io::Result<()> {
            self.close().await
        }

        fn contain_failed_cleanup_for_unwind(&self) {
            LeaderFixture::contain_failed_cleanup_for_unwind(self);
        }
    }

    struct ClientCleanupOutcome {
        all_closed: bool,
        error: Option<io::Error>,
    }

    async fn close_clients<C: CleanupClient>(clients: &mut Vec<C>) -> ClientCleanupOutcome {
        let pending = std::mem::take(clients).into_iter();
        let mut retained = Vec::new();
        let mut first_error = None;
        for mut client in pending {
            match client.graceful_close().await {
                Ok(()) => {}
                Err(close_error) => match client.hard_close().await {
                    Ok(()) => {}
                    Err(kill_error) => {
                        if first_error.is_none() {
                            first_error = Some(io::Error::new(
                                close_error.kind(),
                                format!(
                                    "leader client close failed: {close_error}; bounded hard cleanup also failed: {kill_error}"
                                ),
                            ));
                        }
                        retained.push(client);
                    }
                },
            }
        }
        *clients = retained;
        ClientCleanupOutcome {
            all_closed: clients.is_empty(),
            error: first_error,
        }
    }

    async fn cleanup_owned_processes<C, F>(fixture: &F, clients: &mut Vec<C>) -> Option<io::Error>
    where
        C: CleanupClient,
        F: CleanupFixture,
    {
        let cleanup = close_clients(clients).await;
        let mut cleanup_error = cleanup.error;
        if !cleanup.all_closed {
            // This error-only path requests hard kills, then intentionally
            // leaks concrete owners so panic unwind cannot run blocking Drop.
            // The leak is bounded by the lifetime of the test process.
            for client in clients.iter_mut() {
                client.contain_failed_cleanup_for_unwind();
            }
            let retained = std::mem::take(clients);
            std::mem::forget(retained);
            fixture.contain_failed_cleanup_for_unwind();
            return cleanup_error;
        }
        if let Err(error) = fixture.close_fixture().await {
            cleanup_error = Some(match cleanup_error {
                Some(client_error) => io::Error::new(
                    client_error.kind(),
                    format!("{client_error}; fixture cleanup also failed: {error}"),
                ),
                None => error,
            });
        }
        cleanup_error
    }

    /// Run a leader test body, then close only directly-owned stdio clients and
    /// the concrete initial fixture leader. Detached replacement leaders are
    /// intentionally outside cleanup ownership; tests that create one remain
    /// ignored/manual until OS containment or a test-only leader binary exists.
    #[allow(dead_code)]
    pub async fn run_with_cleanup<F>(
        fixture: &LeaderFixture,
        clients: &mut Vec<LeaderStdioClient>,
        body: F,
    ) where
        F: for<'a> FnOnce(&'a LeaderFixture, &'a mut Vec<LeaderStdioClient>) -> TestBody<'a>,
    {
        let body_result = std::panic::AssertUnwindSafe(body(fixture, clients))
            .catch_unwind()
            .await;
        let cleanup_error = cleanup_owned_processes(fixture, clients).await;
        finish_body(body_result, cleanup_error);
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use super::*;

        struct FakeClient {
            graceful_fails: bool,
            hard_fails: bool,
            graceful_calls: Arc<AtomicUsize>,
            hard_calls: Arc<AtomicUsize>,
            drops: Arc<AtomicUsize>,
            containment_calls: Arc<AtomicUsize>,
        }

        impl CleanupClient for FakeClient {
            async fn graceful_close(&mut self) -> io::Result<()> {
                self.graceful_calls.fetch_add(1, Ordering::SeqCst);
                if self.graceful_fails {
                    Err(io::Error::other("injected graceful failure"))
                } else {
                    Ok(())
                }
            }

            async fn hard_close(&mut self) -> io::Result<()> {
                self.hard_calls.fetch_add(1, Ordering::SeqCst);
                if self.hard_fails {
                    Err(io::Error::other("injected hard failure"))
                } else {
                    Ok(())
                }
            }

            fn contain_failed_cleanup_for_unwind(&mut self) {
                self.containment_calls.fetch_add(1, Ordering::SeqCst);
            }
        }

        impl Drop for FakeClient {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }

        #[derive(Default)]
        struct FakeFixture {
            close_calls: AtomicUsize,
            containment_calls: AtomicUsize,
        }

        impl CleanupFixture for FakeFixture {
            async fn close_fixture(&self) -> io::Result<()> {
                self.close_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }

            fn contain_failed_cleanup_for_unwind(&self) {
                self.containment_calls.fetch_add(1, Ordering::SeqCst);
            }
        }

        #[tokio::test]
        async fn double_failed_client_transfers_to_unwind_containment() {
            let graceful_calls = Arc::new(AtomicUsize::new(0));
            let hard_calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let containment_calls = Arc::new(AtomicUsize::new(0));
            let mut clients = vec![FakeClient {
                graceful_fails: true,
                hard_fails: true,
                graceful_calls: graceful_calls.clone(),
                hard_calls: hard_calls.clone(),
                drops: drops.clone(),
                containment_calls: containment_calls.clone(),
            }];
            let fixture = FakeFixture::default();

            let error = cleanup_owned_processes(&fixture, &mut clients)
                .await
                .expect("double failure must be reported");

            assert!(error.to_string().contains("injected graceful failure"));
            assert!(error.to_string().contains("injected hard failure"));
            assert!(
                clients.is_empty(),
                "retained owner must transfer to leaked containment"
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert_eq!(graceful_calls.load(Ordering::SeqCst), 1);
            assert_eq!(hard_calls.load(Ordering::SeqCst), 1);
            assert_eq!(containment_calls.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.close_calls.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.containment_calls.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn successful_owners_drop_before_fixture_close() {
            let graceful_calls = Arc::new(AtomicUsize::new(0));
            let hard_calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let containment_calls = Arc::new(AtomicUsize::new(0));
            let mut clients = vec![
                FakeClient {
                    graceful_fails: false,
                    hard_fails: false,
                    graceful_calls: graceful_calls.clone(),
                    hard_calls: hard_calls.clone(),
                    drops: drops.clone(),
                    containment_calls: containment_calls.clone(),
                },
                FakeClient {
                    graceful_fails: true,
                    hard_fails: false,
                    graceful_calls: graceful_calls.clone(),
                    hard_calls: hard_calls.clone(),
                    drops: drops.clone(),
                    containment_calls: containment_calls.clone(),
                },
            ];
            let fixture = FakeFixture::default();

            let error = cleanup_owned_processes(&fixture, &mut clients).await;

            assert!(
                error.is_none(),
                "successful bounded hard cleanup must recover the graceful failure"
            );
            assert!(clients.is_empty());
            assert_eq!(drops.load(Ordering::SeqCst), 2);
            assert_eq!(graceful_calls.load(Ordering::SeqCst), 2);
            assert_eq!(hard_calls.load(Ordering::SeqCst), 1);
            assert_eq!(containment_calls.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.close_calls.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.containment_calls.load(Ordering::SeqCst), 0);
        }
    }
}

/// Create a sampling client configured for a mock server. Shared by the
/// integration tests so the ~30-field `SamplerConfig` literal lives in one
/// place (`SamplerConfig` has no `Default`).
#[allow(dead_code)]
pub fn create_test_client(base_url: &str, api_backend: ApiBackend) -> Client {
    create_test_client_with_extra_headers(base_url, api_backend, &[])
}

/// Like [`create_test_client`] but seeds `SamplerConfig::extra_headers`, so a
/// test can assert that session-injected headers reach the wire.
#[allow(dead_code)]
pub fn create_test_client_with_extra_headers(
    base_url: &str,
    api_backend: ApiBackend,
    extra_headers: &[(&str, &str)],
) -> Client {
    Client::new(test_sampler_config(base_url, api_backend, extra_headers)).unwrap()
}

/// The shared mock-server `SamplerConfig`; tests needing a non-default field
/// (e.g. `doom_loop_recovery`) mutate the returned value before building the
/// client themselves.
#[allow(dead_code)]
pub fn test_sampler_config(
    base_url: &str,
    api_backend: ApiBackend,
    extra_headers: &[(&str, &str)],
) -> SamplerConfig {
    // Shell `Client` is `echo_agent_sampler::SamplingClient`, which takes a
    // `SamplerConfig` directly. Construct one inline here.
    SamplerConfig {
        api_key: Some("test-api-key".to_string()),
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        max_completion_tokens: Some(1000),
        temperature: Some(0.7),
        top_p: None,
        api_backend,
        auth_scheme: Default::default(),
        extra_headers: extra_headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        extra_response_includes: Vec::new(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: 256_000,
        client_version: None,
        force_http1: false,
        max_retries: None,
        stream_tool_calls: false,
        supports_tool_calls: None,
        idle_timeout_secs: None,
        client_identifier: None,
        reasoning_effort: None,
        deployment_id: None,
        user_id: None,
        origin_client: None,
        attribution_callback: None,
        bearer_resolver: None,
        supports_backend_search: false,
        compactions_remaining: None,
        compaction_at_tokens: None,
        doom_loop_recovery: None,
        header_injector: None,
    }
}
