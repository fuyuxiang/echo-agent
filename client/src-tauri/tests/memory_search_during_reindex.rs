use echo_agent_memory::{
    EmbeddingMaintenance, MemoryBackendImpl, MemoryIndex, MemoryStorage,
    embedding::{ApiEmbeddingProvider, EmbeddingProvider},
};
use echo_agent_tools::types::memory_backend::MemoryBackend;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    endpoint: String,
    requests: Arc<Mutex<Vec<Vec<String>>>>,
    failures: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(delay: std::time::Duration, failures: usize, stall: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let failed = Arc::new(AtomicUsize::new(failures));
        let closed = Arc::new(AtomicBool::new(false));
        let (seen, remaining, disconnected) = (requests.clone(), failed.clone(), closed.clone());
        let server = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (mut socket, _) = connection.unwrap();
                        let (seen, remaining, disconnected) = (seen.clone(), remaining.clone(), disconnected.clone());
                        clients.spawn(async move {
                            let mut bytes = Vec::new();
                            let mut buffer = [0u8; 4096];
                            let (start, length) = loop {
                                let n = socket.read(&mut buffer).await.unwrap();
                                if n == 0 { return; }
                                bytes.extend_from_slice(&buffer[..n]);
                                assert!(bytes.len() < 1024 * 1024);
                                if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                                    let headers = String::from_utf8_lossy(&bytes[..i]);
                                    let length = headers.lines().find_map(|line| {
                                        let (key, value) = line.split_once(':')?;
                                        key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                                    }).unwrap();
                                    break (i + 4, length);
                                }
                            };
                            while bytes.len() < start + length {
                                let n = socket.read(&mut buffer).await.unwrap();
                                if n == 0 { return; }
                                bytes.extend_from_slice(&buffer[..n]);
                            }
                            let body: serde_json::Value = serde_json::from_slice(&bytes[start..start + length]).unwrap();
                            let input: Vec<String> = body["input"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().into()).collect();
                            let query = input == ["semantic paraphrase"];
                            let count = input.len();
                            seen.lock().unwrap().push(input);
                            if !query && stall {
                                while socket.read(&mut buffer).await.unwrap_or(0) > 0 {}
                                disconnected.store(true, Ordering::Release);
                                return;
                            }
                            if !query { tokio::time::sleep(delay).await; }
                            let fail = !query && remaining.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1)).is_ok();
                            let status = if fail { "400 Bad Request" } else { "200 OK" };
                            let response = if fail { "{}".into() } else {
                                serde_json::json!({"data": (0..count).map(|index| serde_json::json!({"index": index, "embedding": [1.0, 0.0, 0.0, 0.0]})).collect::<Vec<_>>() }).to_string()
                            };
                            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len());
                            let _ = socket.write_all(response.as_bytes()).await;
                        });
                    }
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        });
        Self {
            endpoint,
            requests,
            failures: failed,
            closed,
            server,
        }
    }
    fn document_requests(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|batch| batch.as_slice() != ["semantic paraphrase"])
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(12), async {
        while !condition() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background maintenance did not make progress");
}

fn setup(
    root: &std::path::Path,
    fixture: &Fixture,
    count: usize,
) -> (
    MemoryStorage,
    MemoryIndex,
    MemoryBackendImpl,
    Arc<EmbeddingMaintenance>,
) {
    echo_agent_memory::init_sqlite_vec();
    let storage = MemoryStorage::with_paths(root.join("global"), root.join("workspace"));
    let db = root.join("index.sqlite");
    let mut index =
        MemoryIndex::open_or_create(&db, storage.clone(), Default::default(), 4).unwrap();
    let saved = root.join("saved.md");
    std::fs::write(&saved, "Established project knowledge").unwrap();
    index.reindex_file(&saved, "workspace").unwrap();
    for n in 0..count {
        let pending = root.join(format!("pending-{n}.md"));
        std::fs::write(&pending, format!("Pending document indexing {n}")).unwrap();
        index.reindex_file(&pending, "workspace").unwrap();
    }
    let config = echo_agent_runtime::config::MemoryEmbeddingConfig {
        model: Some("isolated-query-test".into()),
        dimensions: 4,
        ..Default::default()
    };
    let provider =
        ApiEmbeddingProvider::from_session(&config, fixture.endpoint.clone(), String::new())
            .unwrap();
    index
        .ensure_embedding_fingerprint(&provider.fingerprint())
        .unwrap();
    index
        .upsert_embedding(&format!("{}:0", saved.display()), &[1.0, 0.0, 0.0, 0.0])
        .unwrap();
    let owner = Arc::new(EmbeddingMaintenance::default());
    let backend = MemoryBackendImpl::new(db, storage.clone())
        .with_embedding(config, fixture.endpoint.clone(), Some(String::new()))
        .with_embedding_maintenance(owner.clone());
    (storage, index, backend, owner)
}

#[tokio::test]
async fn failed_document_indexing_does_not_disable_healthy_query_vectors() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(Default::default(), usize::MAX, false).await;
    let (_, _, backend, owner) = setup(root.path(), &fixture, 1);
    let found = backend.search("semantic paraphrase", 5, 0.0).await.unwrap();
    assert!(
        found
            .iter()
            .any(|hit| hit.snippet.contains("Established project knowledge"))
    );
    until(|| fixture.document_requests() == 1).await;
    owner.shutdown().await;
}

#[tokio::test]
async fn slow_healthy_service_finishes_all_batches_without_blocking_search_or_duplicate_requests() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(std::time::Duration::from_millis(1500), 0, false).await;
    let (_, index, backend, owner) = setup(root.path(), &fixture, 40);
    for _ in 0..3 {
        let found = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            backend.search("semantic paraphrase", 5, 0.0),
        )
        .await
        .expect("foreground search waited for indexing")
        .unwrap();
        assert!(!found.is_empty());
    }
    until(|| index.chunks_without_embeddings().unwrap().is_empty()).await;
    assert_eq!(
        fixture.document_requests(),
        2,
        "repeated searches must coalesce into one pass"
    );
    owner.shutdown().await;
}

#[tokio::test]
async fn service_recovery_retries_without_another_chat_or_search() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(Default::default(), 1, false).await;
    let (_, index, backend, owner) = setup(root.path(), &fixture, 1);
    backend.search("semantic paraphrase", 5, 0.0).await.unwrap();
    until(|| index.chunks_without_embeddings().unwrap().is_empty()).await;
    assert_eq!(fixture.failures.load(Ordering::Acquire), 0);
    assert_eq!(fixture.document_requests(), 2);
    owner.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_and_joins_stalled_indexing_and_prevents_more_document_requests() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(Default::default(), 0, true).await;
    let (_, index, backend, owner) = setup(root.path(), &fixture, 1);
    backend.search("semantic paraphrase", 5, 0.0).await.unwrap();
    until(|| fixture.document_requests() == 1).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), owner.shutdown())
        .await
        .unwrap();
    until(|| fixture.closed.load(Ordering::Acquire)).await;
    backend.search("semantic paraphrase", 5, 0.0).await.unwrap();
    assert_eq!(fixture.document_requests(), 1);
    assert_eq!(index.chunks_without_embeddings().unwrap().len(), 1);
}

#[tokio::test]
async fn forgetting_during_background_indexing_prevents_commit() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(std::time::Duration::from_millis(1500), 0, false).await;
    let (storage, index, backend, owner) = setup(root.path(), &fixture, 1);
    backend.search("semantic paraphrase", 5, 0.0).await.unwrap();
    until(|| fixture.document_requests() == 1).await;
    echo_agent_memory::records::forget_topic(&storage, "workspace", "Pending document indexing 0")
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1650)).await;
    assert_eq!(index.chunks_without_embeddings().unwrap().len(), 1);
    assert_eq!(fixture.document_requests(), 1);
    owner.shutdown().await;
}
