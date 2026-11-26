//! Leader boots from local data when the endpoint is fully unreachable
//! (connection refused), not merely hanging. `#[ignore]`: needs the built binary.
//!
//! ```bash
//! cargo test -p echo-agent-runtime --test test_nonblocking_startup_offline -- --ignored
//! ```

#![cfg(unix)]

mod common;

use std::time::{Duration, Instant};

use echo_agent_test_support::leader::LeaderFixture;
use echo_agent_test_support::*;

/// A loopback URL on a closed port (bind, read addr, drop); refuses instantly.
fn closed_port_base_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);
    format!("http://{addr}/v1")
}

async fn assert_boots_fast(base_url: String, scenario: &'static str) {
    let workdir = git_workdir();
    let sandbox = TestSandbox::new();

    let started = Instant::now();
    let fixture = LeaderFixture::start_with_base_url(&base_url, workdir.workspace(), &sandbox)
        .await
        .unwrap_or_else(|error| {
            panic!("[{scenario}] leader never became ready with an unreachable endpoint: {error}")
        });
    let mut clients = Vec::new();
    common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
        Box::pin(async move {
            clients.push(
                fixture
                    .spawn_client_with_base_url(&base_url, workdir.workspace(), &sandbox)
                    .await
                    .unwrap_or_else(|error| panic!("[{scenario}] spawn leader client: {error}")),
            );
            clients[0].initialize().await;
            // Catalog resolves offline (built-in/cache), so session creation succeeds.
            let _session = clients[0].create_session(workdir.workspace()).await;

            let elapsed = started.elapsed();
            assert!(
                elapsed < Duration::from_secs(25),
                "[{scenario}] startup took {elapsed:?} with an unreachable endpoint; \
                 readiness appears to block on the network fetch\nstderr:\n{}",
                clients[0].stderr_text(),
            );
        })
    })
    .await;
}

#[tokio::test]
#[ignore] // needs the built binary
async fn leader_ready_with_connection_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            assert_boots_fast(closed_port_base_url(), "connection-refused").await;
        })
        .await;
}
