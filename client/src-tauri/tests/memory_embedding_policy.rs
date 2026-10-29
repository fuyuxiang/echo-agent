//! Exercise the production index and batch coordinator with isolated ledgers.
use echo_agent_memory::{
    embed_missing_chunks,
    embedding::{EmbeddingProvider, MemoryPolicyError},
    init_sqlite_vec, records, MemoryIndex, MemoryStorage,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

struct Provider {
    inputs: Mutex<Vec<Vec<String>>>,
    fail: AtomicBool,
    during_request: Option<Box<dyn Fn() + Send + Sync>>,
}
impl Default for Provider {
    fn default() -> Self {
        Self {
            inputs: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            during_request: None,
        }
    }
}
fn vector(text: &str) -> Vec<f32> {
    let number = text
        .split_whitespace()
        .last()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or_else(|| text.bytes().map(u32::from).sum());
    vec![1.0, number as f32 / 1000.0, 0.0, 0.0]
}
#[async_trait::async_trait]
impl EmbeddingProvider for Provider {
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        self.inputs
            .lock()
            .unwrap()
            .push(texts.iter().map(|s| (*s).into()).collect());
        if self.fail.load(Ordering::Acquire) {
            return Err("simulated service failure".into());
        }
        if let Some(action) = &self.during_request {
            action();
        }
        Ok(texts.iter().map(|s| vector(s)).collect())
    }
    fn model_name(&self) -> &str {
        "isolated-policy-test"
    }
    fn dimensions(&self) -> usize {
        4
    }
}
fn setup(root: &std::path::Path) -> (MemoryStorage, MemoryIndex) {
    init_sqlite_vec();
    let storage = MemoryStorage::with_paths(root.join("memory"), root.join("memory/workspace"));
    let index = MemoryIndex::open_or_create(
        &root.join("index.sqlite"),
        storage.clone(),
        Default::default(),
        4,
    )
    .unwrap();
    assert!(index.vec_available());
    (storage, index)
}

#[tokio::test]
async fn one_privacy_veto_does_not_starve_64_allowed_chunks_or_misalign_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(dir.path());
    records::forget_topic(&storage, "workspace", "PrivateSaturnArchivePhrase").unwrap();
    for n in 0..65 {
        let path = dir.path().join(format!("document-{n:03}.md"));
        let text = if n == 0 {
            "PrivateSaturnArchivePhrase".into()
        } else {
            format!("Allowed document number {n}")
        };
        std::fs::write(&path, text).unwrap();
        index.reindex_file(&path, "session").unwrap();
    }
    let provider = Provider::default();
    assert_eq!(embed_missing_chunks(&index, &provider).await, 64);
    let missing = index.chunks_without_embeddings().unwrap();
    assert_eq!(missing.len(), 1);
    assert!(missing[0].1.contains("PrivateSaturn"));
    let sent = provider.inputs.lock().unwrap().clone();
    assert_eq!(sent.iter().map(Vec::len).sum::<usize>(), 64);
    for text in sent.iter().flatten() {
        assert!(records::permitted(&storage, text).unwrap());
        let hits = index.vector_search(&vector(text), 1).unwrap();
        assert!(hits[0].1 < 0.0001);
        assert_eq!(&index.get_chunk(&hits[0].0).unwrap().unwrap().text, text);
    }
    assert_eq!(embed_missing_chunks(&index, &provider).await, 0);
    assert_eq!(
        *provider.inputs.lock().unwrap(),
        sent,
        "policy skips must not produce repeated HTTP calls"
    );
}

#[tokio::test]
async fn blocked_source_is_excluded_even_if_its_chunk_has_no_forgotten_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(dir.path());
    records::forget_source(&storage, "12345678-private-source").unwrap();
    let path = dir.path().join("2026-10-28-summary-12345678.md");
    std::fs::write(&path, "An otherwise allowed excerpt from a blocked source").unwrap();
    index.reindex_file(&path, "session").unwrap();
    let provider = Provider::default();
    assert_eq!(embed_missing_chunks(&index, &provider).await, 0);
    assert!(provider.inputs.lock().unwrap().is_empty());
    let permit = storage.request_permit_for_paths(vec![path]);
    assert!(matches!(
        permit(&["new wording without the forgotten phrase"]),
        Err(MemoryPolicyError::ContentVetoed)
    ));
}

#[tokio::test]
async fn forgetting_during_request_prevents_the_vector_commit() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(dir.path());
    let path = dir.path().join("memory.md");
    std::fs::write(&path, "PrivateSaturnArchivePhrase").unwrap();
    index.reindex_file(&path, "workspace").unwrap();
    let provider = Provider {
        during_request: Some(Box::new(move || {
            records::forget_topic(&storage, "workspace", "PrivateSaturnArchivePhrase").unwrap();
        })),
        ..Default::default()
    };
    assert_eq!(embed_missing_chunks(&index, &provider).await, 0);
    assert_eq!(index.chunks_without_embeddings().unwrap().len(), 1);
}

#[tokio::test]
async fn forgetting_an_earlier_batch_before_the_pass_finishes_prevents_its_commit() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(dir.path());
    for n in 0..33 {
        let path = dir.path().join(format!("item-{n}.md"));
        let text = if n == 0 {
            "PrivateSaturnArchivePhrase".into()
        } else {
            format!("Allowed independent memory {n}")
        };
        std::fs::write(&path, text).unwrap();
        index.reindex_file(&path, "workspace").unwrap();
    }
    let requests = std::sync::atomic::AtomicUsize::new(0);
    let provider = Provider {
        during_request: Some(Box::new(move || {
            if requests.fetch_add(1, Ordering::AcqRel) == 1 {
                records::forget_topic(&storage, "workspace", "PrivateSaturnArchivePhrase").unwrap();
            }
        })),
        ..Default::default()
    };
    assert_eq!(embed_missing_chunks(&index, &provider).await, 32);
    assert_eq!(
        index.chunks_without_embeddings().unwrap()[0].1,
        "PrivateSaturnArchivePhrase"
    );
}

#[tokio::test]
async fn editing_chunk_during_request_does_not_install_an_obsolete_vector() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(dir.path());
    let path = dir.path().join("memory.md");
    std::fs::write(&path, "old value").unwrap();
    index.reindex_file(&path, "workspace").unwrap();
    let db = dir.path().join("index.sqlite");
    let provider = Provider {
        during_request: Some(Box::new(move || {
            std::fs::write(&path, "new value").unwrap();
            let mut other =
                MemoryIndex::open_or_create(&db, storage.clone(), Default::default(), 4).unwrap();
            other.reindex_file(&path, "workspace").unwrap();
        })),
        ..Default::default()
    };
    assert_eq!(embed_missing_chunks(&index, &provider).await, 0);
    assert_eq!(index.chunks_without_embeddings().unwrap()[0].1, "new value");
}

#[tokio::test]
async fn ledger_failure_is_distinct_and_sends_no_content() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(dir.path());
    let path = dir.path().join("memory.md");
    std::fs::write(&path, "allowed content").unwrap();
    index.reindex_file(&path, "workspace").unwrap();
    std::fs::create_dir_all(storage.workspace_dir()).unwrap();
    std::fs::write(storage.workspace_dir().join("records.json"), b"corrupt").unwrap();
    assert!(matches!(
        storage.request_permit()(&["allowed content"]),
        Err(MemoryPolicyError::LedgerUnavailable(_))
    ));
    let provider = Provider::default();
    assert_eq!(embed_missing_chunks(&index, &provider).await, 0);
    assert!(provider.inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn service_failure_stops_one_pass_and_later_retries_recover() {
    let dir = tempfile::tempdir().unwrap();
    let (_, mut index) = setup(dir.path());
    for n in 0..65 {
        let path = dir.path().join(format!("memory-{n}.md"));
        std::fs::write(&path, format!("allowed item {n}")).unwrap();
        index.reindex_file(&path, "workspace").unwrap();
    }
    let provider = Arc::new(Provider::default());
    provider.fail.store(true, Ordering::Release);
    assert_eq!(embed_missing_chunks(&index, provider.as_ref()).await, 0);
    assert_eq!(provider.inputs.lock().unwrap().len(), 1);
    provider.fail.store(false, Ordering::Release);
    assert_eq!(embed_missing_chunks(&index, provider.as_ref()).await, 65);
    assert!(index.chunks_without_embeddings().unwrap().is_empty());
}

#[tokio::test]
async fn live_policy_race_reselects_the_batch_and_preserves_its_allowed_companions() {
    struct RacingProvider {
        storage: MemoryStorage,
        first: AtomicBool,
    }
    #[async_trait::async_trait]
    impl EmbeddingProvider for RacingProvider {
        async fn embed_batch(
            &self,
            texts: &[&str],
        ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
            if self.first.swap(false, Ordering::AcqRel) {
                records::forget_topic(&self.storage, "workspace", "PrivateSaturnArchivePhrase")
                    .unwrap();
                return Err(MemoryPolicyError::ContentVetoed.into());
            }
            self.storage.request_permit()(texts)?;
            Ok(texts.iter().map(|s| vector(s)).collect())
        }
        fn model_name(&self) -> &str {
            "racing-test"
        }
        fn dimensions(&self) -> usize {
            4
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(root.path());
    for (name, text) in [
        ("private.md", "PrivateSaturnArchivePhrase"),
        ("allowed.md", "Allowed independent memory"),
    ] {
        let path = root.path().join(name);
        std::fs::write(&path, text).unwrap();
        index.reindex_file(&path, "workspace").unwrap();
    }
    let provider = RacingProvider {
        storage,
        first: AtomicBool::new(true),
    };
    assert_eq!(embed_missing_chunks(&index, &provider).await, 1);
    assert_eq!(
        index.chunks_without_embeddings().unwrap()[0].1,
        "PrivateSaturnArchivePhrase"
    );
}

#[tokio::test]
async fn an_explicit_correction_can_extend_old_text_without_reviving_the_legacy_fact() {
    use echo_agent_tools::types::memory_backend::MemoryBackend;
    let root = tempfile::tempdir().unwrap();
    let (storage, mut index) = setup(root.path());
    let original = records::remember(
        &storage,
        records::RecordInput {
            scope: "workspace".into(),
            kind: "project".into(),
            key: "report language".into(),
            content: "Report in English".into(),
            ..Default::default()
        },
        None,
        false,
    )
    .unwrap()
    .unwrap();
    let corrected = records::remember(
        &storage,
        records::RecordInput {
            id: Some(original.id),
            expected_revision: Some(original.revision),
            scope: "workspace".into(),
            kind: "project".into(),
            key: "report language".into(),
            content: "Report in English and Chinese".into(),
            ..Default::default()
        },
        None,
        false,
    )
    .unwrap()
    .unwrap();
    let path = storage.workspace_dir().join(records::RECORD_DOCUMENT);
    index.reindex_file(&path, "workspace").unwrap();
    let legacy = root.path().join("legacy.md");
    std::fs::write(&legacy, "Report in English").unwrap();
    index.reindex_file(&legacy, "session").unwrap();
    let provider = Provider::default();
    assert!(embed_missing_chunks(&index, &provider).await > 0);
    assert_eq!(index.chunks_without_embeddings().unwrap().len(), 1);
    assert!(!records::permitted(&storage, "Report in English").unwrap());
    let db = root.path().join("index.sqlite");
    let backend = echo_agent_memory::MemoryBackendImpl::new(db, storage.clone()).with_embedding(
        echo_agent_runtime::config::MemoryEmbeddingConfig {
            dimensions: 4,
            model: None,
            ..Default::default()
        },
        String::new(),
        None,
    );
    let found = backend.search("English Chinese", 5, 0.0).await.unwrap();
    assert!(found.iter().any(|r| r.snippet.contains(&corrected.content)));
    assert!(found.iter().all(|r| r.path != legacy.to_string_lossy()));
    // Current workspace facts cannot override an explicit global forgetting rule.
    records::forget_topic(&storage, "global", "English").unwrap();
    assert!(backend
        .search("English Chinese", 5, 0.0)
        .await
        .unwrap()
        .is_empty());
    let projection = std::fs::read_to_string(&path).unwrap();
    assert!(matches!(
        storage.request_permit_for_paths(vec![path])(&[&projection]),
        Err(MemoryPolicyError::ContentVetoed)
    ));
}

#[tokio::test]
async fn managed_records_remain_searchable_and_embeddable_after_chunking() {
    use echo_agent_tools::types::memory_backend::MemoryBackend;
    for config in [
        echo_agent_runtime::config::MemoryIndexConfig::default(),
        echo_agent_runtime::config::MemoryIndexConfig {
            max_chunk_chars: 160,
            chunk_overlap_chars: 80,
        },
    ] {
        let root = tempfile::tempdir().unwrap();
        let (storage, _) = setup(root.path());
        for n in 0..10 {
            records::remember(
                &storage,
                records::RecordInput {
                    scope: "workspace".into(),
                    kind: "project".into(),
                    key: format!("topic{n}"),
                    content: if n == 0 {
                        format!(
                            "UniqueFact{n}\n\n{}",
                            (0..12)
                                .map(|paragraph| {
                                    format!(
                                        "Detail{paragraph}: {}",
                                        "A useful project fact. ".repeat(10)
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        )
                    } else {
                        format!("UniqueFact{n} {}", "A useful project fact. ".repeat(14))
                    },
                    ..Default::default()
                },
                None,
                false,
            )
            .unwrap();
        }
        let path = storage.workspace_dir().join(records::RECORD_DOCUMENT);
        let db = root.path().join("index.sqlite");
        let mut index = MemoryIndex::open_or_create(&db, storage.clone(), config, 4).unwrap();
        index.reindex_file(&path, "workspace").unwrap();
        let count = index.chunks_without_embeddings().unwrap().len();
        assert!(
            count > 10,
            "exercise ancestor headers and overlapping chunks"
        );
        assert_eq!(
            embed_missing_chunks(&index, &Provider::default()).await,
            count
        );
        assert!(index.chunks_without_embeddings().unwrap().is_empty());
        let backend = echo_agent_memory::MemoryBackendImpl::new(db, storage.clone())
            .with_embedding(
                echo_agent_runtime::config::MemoryEmbeddingConfig {
                    dimensions: 4,
                    model: None,
                    ..Default::default()
                },
                String::new(),
                None,
            );
        for n in 0..10 {
            let found = backend
                .search(&format!("UniqueFact{n}"), 5, 0.0)
                .await
                .unwrap();
            assert!(
                found
                    .iter()
                    .any(|hit| hit.snippet.contains(&format!("UniqueFact{n}"))),
                "missing fact {n}"
            );
        }
        let permit = storage.request_permit_for_paths(vec![path]);
        assert!(permit(&["[Context: private invented fact]\n\n# Saved memories"]).is_err());
    }
}
