//! Real persistence/index regressions. No production memory or external model.
use echo_agent_memory::{
    init_sqlite_vec,
    records::{self, RecordInput},
    MemoryIndex, MemoryStorage,
};

fn input(scope: &str, key: &str, content: &str) -> RecordInput {
    RecordInput {
        scope: scope.into(),
        kind: "user".into(),
        key: key.into(),
        content: content.into(),
        evidence: content.into(),
        ..Default::default()
    }
}

#[test]
fn repeating_a_legacy_remember_command_preserves_one_editable_fact() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let old = "🧠工作偏好：周报使用中文，先给结论再说明依据。".repeat(6);
    let legacy_key: String = old
        .chars()
        .scan(0, |units, ch| {
            *units += ch.len_utf16();
            (*units <= 80).then_some(ch)
        })
        .collect();
    let mut original_input = input("global", &legacy_key, &old);
    original_input.pinned = true;
    let original = records::remember(&storage, original_input, Some("legacy-source"), false)
        .unwrap()
        .unwrap();
    let key: String = old.chars().take(60).collect();
    let mut repeated_input = input("global", &key, &old);
    repeated_input.pinned = true;
    let repeated = records::remember(&storage, repeated_input, None, false)
        .unwrap()
        .unwrap();
    assert_eq!(repeated, original);
    let current = "周报改用双语。";
    let mut update = input("global", current, current);
    update.id = Some(repeated.id.clone());
    update.expected_revision = Some(repeated.revision);
    update.pinned = true;
    let saved = records::remember(&storage, update, None, false)
        .unwrap()
        .unwrap();
    let reopened = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    assert_eq!(records::list(&reopened, true).unwrap(), vec![saved]);
    let context = records::core_context(&reopened).unwrap();
    assert!(context.contains(current) && !context.contains(&old));
    assert!(!records::permitted(&reopened, &old).unwrap());
}

#[test]
fn legacy_title_duplicates_preserve_all_sources_when_corrected_and_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let old = "用户要求周报使用中文，并先说明完整的工作结论。".repeat(6);
    let key: String = old.chars().take(80).collect();
    let original = records::remember(
        &storage,
        input("global", &key, &old),
        Some("original-source"),
        false,
    )
    .unwrap()
    .unwrap();
    let path = storage.global_dir().join("records.json");
    let mut ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut duplicate = ledger["records"][0].clone();
    duplicate["id"] = "legacy-duplicate".into();
    duplicate["key"] = old.chars().take(60).collect::<String>().into();
    duplicate["sources"] = serde_json::json!(["duplicate-source"]);
    ledger["records"].as_array_mut().unwrap().push(duplicate);
    records::write_json(&path, &ledger).unwrap();
    let before = std::fs::read(&path).unwrap();
    let visible = records::list(&storage, true).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, original.id);
    assert_eq!(visible[0].sources, ["original-source", "duplicate-source"]);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let current = "周报改用双语。";
    let mut update = input("global", current, current);
    update.id = Some(visible[0].id.clone());
    update.expected_revision = Some(visible[0].revision);
    let saved = records::remember(&storage, update, None, false)
        .unwrap()
        .unwrap();
    let reopened = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let context = records::core_context(&reopened).unwrap();
    assert!(context.contains(current) && !context.contains(&old));
    assert!(!records::permitted(&reopened, &old).unwrap());
    records::retire(&reopened, "global", &saved.id, Some(saved.revision), false).unwrap();
    assert!(records::source_blocked(&reopened, "original-source").unwrap());
    assert!(records::source_blocked(&reopened, "duplicate-source").unwrap());
    assert!(!records::permitted(&reopened, current).unwrap());
    assert!(
        records::remember(&reopened, input("global", current, current), None, false).is_err(),
        "repeating a command must not silently revive a forgotten record"
    );
    assert_eq!(records::list(&reopened, true).unwrap().len(), 1);
}

#[test]
fn correcting_a_legacy_command_title_preserves_recall_after_reopen() {
    // Exercise the shipped dependency graph as well as the memory crate's
    // own workspace tests: the application and upstream have separate locks.
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let old = "用户的偏好是仅使用中文，并且每份报告保持完整。".repeat(6);
    let legacy_key: String = old.chars().take(80).collect();
    let current = "用户现在希望报告使用中英双语。";
    let original = records::remember(
        &storage,
        input("global", &legacy_key, &old),
        Some("source-legacy"),
        false,
    )
    .unwrap()
    .unwrap();
    records::remember(
        &storage,
        input("global", "独立事项", "独立的项目约定仍需保留"),
        None,
        false,
    )
    .unwrap();
    let mut update = input("global", &legacy_key, current);
    update.id = Some(original.id.clone());
    update.expected_revision = Some(original.revision);
    let saved = records::remember(&storage, update, None, false)
        .unwrap()
        .unwrap();
    assert_eq!(saved.key, current);
    assert_eq!(saved.revision, original.revision + 1);
    assert_eq!(saved.sources, original.sources);
    let reopened = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let context = records::core_context(&reopened).unwrap();
    assert!(context.contains(current));
    assert!(context.contains("独立的项目约定仍需保留"));
    assert!(!context.contains(&legacy_key));
    assert!(records::permitted(&reopened, &context).unwrap());
    assert!(!records::permitted(&reopened, &old).unwrap());
}

#[test]
fn saved_preferences_are_global_but_project_decisions_stay_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let a = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let b = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("b"));
    records::remember(
        &a,
        input("global", "回答方式", "先给结论，再说明依据"),
        Some("session-a"),
        true,
    )
    .unwrap();
    records::remember(
        &a,
        input("workspace", "报告结构", "项目 A 报告使用五部分结构"),
        Some("session-a"),
        true,
    )
    .unwrap();
    let context_b = records::core_context(&b).unwrap();
    assert!(context_b.contains("先给结论"));
    assert!(!context_b.contains("五部分"));
    assert_eq!(records::list(&a, true).unwrap().len(), 2);
    assert_eq!(records::list(&b, true).unwrap().len(), 1);
}

#[test]
fn correction_is_durable_and_automatic_extraction_cannot_overwrite_it() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let original = records::remember(
        &storage,
        input("global", "报告语言", "报告使用英文"),
        Some("source-old"),
        true,
    )
    .unwrap()
    .unwrap();
    let mut update = input("global", "报告语言", "报告使用中文");
    update.id = Some(original.id.clone());
    update.expected_revision = Some(original.revision);
    let updated = records::remember(&storage, update.clone(), None, false)
        .unwrap()
        .unwrap();
    assert!(
        records::remember(&storage, update, None, false).is_err(),
        "stale edits must fail"
    );
    assert!(records::remember(
        &storage,
        input("global", "报告语言", "报告使用英文"),
        Some("source-new"),
        true
    )
    .unwrap()
    .is_none());
    let reopened = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    assert!(records::core_context(&reopened)
        .unwrap()
        .contains("报告使用中文"));
    assert!(!records::permitted(&reopened, "旧会话：报告使用英文").unwrap());
    assert!(records::permitted(&reopened, "报告使用中文").unwrap());
    records::retire(
        &reopened,
        "global",
        &updated.id,
        Some(updated.revision),
        false,
    )
    .unwrap();
    assert!(records::core_context(&reopened).unwrap().is_empty());
    assert!(records::source_blocked(&reopened, "source-old").unwrap());
    assert!(records::remember(
        &reopened,
        input("global", "另一种表述", "报告使用中文"),
        Some("source-old"),
        true
    )
    .unwrap()
    .is_none());
}

#[test]
fn expired_and_deleted_source_records_leave_no_searchable_projection() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let mut task = input("workspace", "客户初稿", "月底提交客户初稿");
    task.kind = "task".into();
    task.valid_until = Some(records::now() - 1);
    records::remember(&storage, task, Some("source-task"), true).unwrap();
    records::remember(
        &storage,
        input("workspace", "项目决定", "采用分阶段交付"),
        Some("source-task"),
        true,
    )
    .unwrap();
    init_sqlite_vec();
    let path = storage.workspace_dir().join(records::RECORD_DOCUMENT);
    let mut index = MemoryIndex::open_or_create(
        &storage.workspace_dir().join("index.sqlite"),
        storage.clone(),
        Default::default(),
        4,
    )
    .unwrap();
    index.reindex_file(&path, "workspace").unwrap();
    assert!(!index.search_fts("客户初稿", 5).unwrap().iter().any(|r| {
        index
            .get_chunk(&r.chunk_id)
            .unwrap()
            .unwrap()
            .text
            .contains("月底提交")
    }));
    assert!(!index.search_fts("分阶段交付", 5).unwrap().is_empty());
    assert_eq!(records::forget_source(&storage, "source-task").unwrap(), 1);
    index.reindex_file(&path, "workspace").unwrap();
    assert!(index.search_fts("分阶段交付", 5).unwrap().is_empty());
    assert!(records::core_context(&storage).unwrap().is_empty());
}

#[test]
fn extraction_preserves_personal_information_and_finishes_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    records::apply_extraction(&storage, r#"{"memories":[{"scope":"global","kind":"user","key":"职责","content":"负责企业客户实施","evidence":"我负责企业客户实施"},{"scope":"workspace","kind":"task","key":"客户汇报","content":"本周提交汇报","evidence":"这周提交汇报"}]}"#, "source-extract").unwrap();
    assert_eq!(records::list(&storage, true).unwrap().len(), 2);
    assert!(records::list(&storage, true)
        .unwrap()
        .iter()
        .find(|r| r.kind == "task")
        .unwrap()
        .valid_until
        .is_some());
    records::apply_extraction(
        &storage,
        r#"{"memories":[],"completedKeys":[{"key":"客户汇报","scope":"workspace","evidence":"客户汇报已完成"}]}"#,
        "source-extract",
    )
    .unwrap();
    let context = records::core_context(&storage).unwrap();
    assert!(context.contains("企业客户实施"));
    assert!(!context.contains("本周提交汇报"));
    assert_eq!(records::activity(&storage).outcome, "unchanged");
}

#[test]
fn generated_memories_require_actual_supporting_quotes() {
    let output = records::grounded_extraction(r#"{"memories":[{"scope":"global","kind":"user","key":"风格","content":"先给结论","evidence":"请先给结论"},{"scope":"global","kind":"user","key":"虚构职业","content":"用户是律师","evidence":"我是律师"}],"completedKeys":[]}"#, "用户说：请先给结论，再给细节").unwrap();
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["memories"].as_array().unwrap().len(), 1);
    assert_eq!(value["memories"][0]["key"], "风格");
}

#[tokio::test]
async fn semantic_only_matches_pass_the_default_injection_threshold() {
    use echo_agent_memory::embedding::EmbeddingProvider;
    struct SameMeaning;
    #[async_trait::async_trait]
    impl EmbeddingProvider for SameMeaning {
        async fn embed_batch(
            &self,
            texts: &[&str],
        ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect())
        }
        fn model_name(&self) -> &str {
            "semantic-test"
        }
        fn dimensions(&self) -> usize {
            4
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    std::fs::create_dir_all(storage.workspace_dir()).unwrap();
    let path = storage.workspace_memory_file();
    std::fs::write(
        &path,
        "## Communication\n\nStart with the conclusion before explaining the evidence.",
    )
    .unwrap();
    init_sqlite_vec();
    let mut index = MemoryIndex::open_or_create(
        &storage.workspace_dir().join("index.sqlite"),
        storage.clone(),
        Default::default(),
        4,
    )
    .unwrap();
    index.reindex_file(&path, "workspace").unwrap();
    echo_agent_memory::embed_missing_chunks(&index, &SameMeaning).await;
    let mut config = echo_agent_runtime::config::MemorySearchConfig::default();
    config.min_score = echo_agent_runtime::config::MemoryInitialInjectionConfig::default()
        .min_score
        .unwrap();
    let results = echo_agent_memory::search::hybrid_search(
        &index,
        Some(&SameMeaning),
        "汇报时按我的表达习惯来",
        &config,
    )
    .await
    .unwrap();
    assert!(
        !results.is_empty(),
        "a semantic-only paraphrase was incorrectly filtered out"
    );
}

#[test]
fn metadata_only_edits_are_saved_and_renames_do_not_reuse_ids() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let original = records::remember(
        &storage,
        input("global", "旧主题", "汇报先给结论"),
        None,
        false,
    )
    .unwrap()
    .unwrap();
    let mut update = input("global", "新主题", &original.content);
    update.id = Some(original.id.clone());
    update.expected_revision = Some(original.revision);
    update.pinned = true;
    update.kind = "feedback".into();
    let saved = records::remember(&storage, update, None, false)
        .unwrap()
        .unwrap();
    assert_eq!(saved.kind, "feedback");
    assert_eq!(saved.key, "新主题");
    assert!(saved.pinned);
    assert!(
        records::permitted(&storage, &saved.content).unwrap(),
        "metadata edits must not suppress current content"
    );
    let second = records::remember(
        &storage,
        input("global", "旧主题", "保留另一个独立主题"),
        None,
        false,
    )
    .unwrap()
    .unwrap();
    assert_ne!(saved.id, second.id);
}

#[test]
fn completions_need_a_real_quote_as_well_as_the_matching_scope() {
    let output = records::grounded_extraction(r#"{"memories":[],"completedKeys":[{"key":"汇报","scope":"workspace","evidence":"汇报已经交付"},{"key":"方案","scope":"workspace","evidence":"方案已经完成"}]}"#, "用户：汇报已经交付，方案还在写").unwrap();
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["completedKeys"].as_array().unwrap().len(), 1);
    assert_eq!(value["completedKeys"][0]["key"], "汇报");
}

#[tokio::test]
async fn forgotten_memories_leave_live_recall_without_affecting_knowledge_reads() {
    use echo_agent_tools::types::memory_backend::MemoryBackend;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("global");
    let storage = MemoryStorage::with_paths(root.clone(), root.join("a"));
    storage.ensure_initialized().unwrap();
    let db = storage.workspace_dir().join("index.sqlite");
    let path = storage.workspace_memory_file();
    let content = "## Atlas delivery\n\nAtlas reports use the obsolete delivery checklist.";
    std::fs::write(&path, content).unwrap();
    init_sqlite_vec();
    let mut index =
        MemoryIndex::open_or_create(&db, storage.clone(), Default::default(), 1024).unwrap();
    index.reindex_file(&path, "workspace").unwrap();
    drop(index);
    let record = records::remember(
        &storage,
        input(
            "workspace",
            "Atlas delivery",
            "Atlas reports use the obsolete delivery checklist.",
        ),
        Some("source-atlas"),
        false,
    )
    .unwrap()
    .unwrap();
    let personal = echo_agent_memory::MemoryBackendImpl::new(db.clone(), storage.clone());
    assert!(!personal
        .search("Atlas delivery", 10, 0.0)
        .await
        .unwrap()
        .is_empty());
    records::retire(
        &storage,
        "workspace",
        &record.id,
        Some(record.revision),
        false,
    )
    .unwrap();
    assert!(personal
        .search("Atlas delivery", 10, 0.0)
        .await
        .unwrap()
        .iter()
        .all(|r| !r.snippet.contains("obsolete delivery checklist")));
    assert!(personal.get(path.to_str().unwrap(), None, None).is_err());
    let knowledge =
        echo_agent_memory::MemoryBackendImpl::new(db, storage).with_query_only_embedding_search();
    assert_eq!(
        knowledge.get(path.to_str().unwrap(), None, None).unwrap(),
        content
    );
    assert!(knowledge
        .search("Atlas delivery", 10, 0.0)
        .await
        .unwrap()
        .iter()
        .any(|r| r.snippet.contains("obsolete delivery checklist")));
}

#[test]
fn completing_one_occurrence_does_not_forget_future_work_on_the_same_topic() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let mut task = input(
        "workspace",
        "客户汇报-2026-10-19",
        "2026-10-19 提交客户汇报",
    );
    task.kind = "task".into();
    let record = records::remember(&storage, task, Some("source-report"), true)
        .unwrap()
        .unwrap();
    records::retire(
        &storage,
        "workspace",
        &record.id,
        Some(record.revision),
        true,
    )
    .unwrap();
    assert!(!records::source_blocked(&storage, "source-report").unwrap());
    let mut next = input(
        "workspace",
        "客户汇报-2026-10-26",
        "2026-10-26 提交客户汇报",
    );
    next.kind = "task".into();
    assert!(
        records::remember(&storage, next, Some("source-next-report"), true)
            .unwrap()
            .is_some()
    );
    assert!(records::core_context(&storage)
        .unwrap()
        .contains("2026-10-26"));
    assert!(!records::core_context(&storage)
        .unwrap()
        .contains("2026-10-19"));
}

#[test]
fn older_recovery_jobs_cannot_replace_more_recent_facts() {
    let dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("a"));
    let newer = r#"{"memories":[{"scope":"workspace","kind":"project","key":"汇报日期","content":"汇报改到周五","evidence":"改到周五"}]}"#;
    let older = r#"{"memories":[{"scope":"workspace","kind":"project","key":"汇报日期","content":"汇报安排在周三","evidence":"安排在周三"}]}"#;
    records::apply_extraction_at(&storage, newer, "new-source", 200).unwrap();
    records::apply_extraction_at(&storage, older, "old-source", 100).unwrap();
    let records = records::list(&storage, true).unwrap();
    assert_eq!(records[0].content, "汇报改到周五");
    assert_eq!(records[0].observed_at, 200);
}
