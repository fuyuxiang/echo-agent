//! The session-owned MCP client map.

use std::collections::HashMap;
use std::sync::Arc;

use crate::servers::{McpClient, McpServerName};

#[derive(Default)]
pub struct OwnedClients {
    clients: HashMap<McpServerName, Arc<McpClient>>,
}

impl OwnedClients {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        name: McpServerName,
        client: Arc<McpClient>,
    ) -> Option<Arc<McpClient>> {
        let displaced = self.clients.insert(name, client);
        if let Some(old) = &displaced {
            cancel_and_shutdown(old);
        }
        displaced
    }

    pub fn remove(&mut self, name: &str) -> Option<Arc<McpClient>> {
        let removed = self.clients.remove(name);
        if let Some(old) = &removed {
            cancel_and_shutdown(old);
        }
        removed
    }

    pub fn clear(&mut self) {
        // Collect Arc clones first so the closure can move them into the
        // spawned shutdown tasks without holding a borrow on `self`.
        let to_shutdown: Vec<Arc<McpClient>> =
            self.clients.values().map(Arc::clone).collect();
        self.clients.clear();
        for client in to_shutdown {
            cancel_and_shutdown(&client);
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<McpClient>> {
        self.clients.get(name)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.clients.contains_key(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&McpServerName, &Arc<McpClient>)> {
        self.clients.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &McpServerName> {
        self.clients.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &Arc<McpClient>> {
        self.clients.values()
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

/// Cancel the liveness watcher AND actively shut down the client's
/// transport so the stdio child dies immediately on eviction, regardless
/// of whether other `Arc<McpClient>` references elsewhere
/// (e.g. `McpState::shared_clients`, `SharedMcpPool` snapshots, stale
/// liveness task clones) keep the client struct alive past the eviction.
///
/// The previous implementation only cancelled the watcher and relied on
/// the Arc-release chain `McpClient::drop → SafeTokioChildProcess::drop
/// → kill_process_group → child.kill`. Any holder keeping the Arc alive
/// past eviction orphaned the child process permanently — see
/// `owned_clients_clear_reaps_stdio_child_even_with_external_arc` in
/// `servers_tests.rs` for the regression contract.
///
/// We `tokio::spawn` so the call stays sync and non-blocking: the
/// spawn is the only thing we await. The spawned task acquires the
/// state lock, swaps `Ready → Empty`, and drops the
/// `RunningService` *outside* the lock — which tears down the
/// transport synchronously.
fn cancel_and_shutdown(client: &Arc<McpClient>) {
    // Step 1: cancel the liveness watcher so the polling task exits on
    // its next select! tick instead of competing for the state lock.
    client.set_liveness_handle(None);

    // Step 2: actively reap the transport. The Arc clone keeps the
    // client alive long enough for the spawned task to acquire the
    // state lock and drop the RunningService — even if `self` (the
    // OwnedClients map) has already dropped the Arc entry.
    //
    // Runtime guard: sync `#[test]` contexts (e.g. McpState unit tests
    // that build OwnedClients without a tokio runtime) have no
    // `Handle::current()`. In those contexts we fall back to the
    // pre-fix behavior (watcher cancel + Arc-release chain); the
    // regression test
    // `owned_clients_clear_reaps_stdio_child_even_with_external_arc`
    // exercises the spawn path under `#[tokio::test]`.
    let client = Arc::clone(client);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            client.shutdown_transport().await;
        });
    }
}

impl Drop for OwnedClients {
    fn drop(&mut self) {
        // Same teardown contract as `clear`: cancel watchers and
        // actively reap transports so the map's drop also reaps
        // grandchildren regardless of external Arc holders.
        let to_shutdown: Vec<Arc<McpClient>> =
            self.clients.values().map(Arc::clone).collect();
        self.clients.clear();
        for client in to_shutdown {
            cancel_and_shutdown(&client);
        }
    }
}

impl FromIterator<(McpServerName, Arc<McpClient>)> for OwnedClients {
    fn from_iter<I: IntoIterator<Item = (McpServerName, Arc<McpClient>)>>(iter: I) -> Self {
        Self {
            clients: iter.into_iter().collect(),
        }
    }
}
