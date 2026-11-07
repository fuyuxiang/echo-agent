//! Personal knowledge indexing and retrieval.
//!
//! Source files remain in the user-selected folders. Extracted text is chunked
//! into a private SQLite FTS5/sqlite-vec index under the EchoAgent data home.
//! Retrieval combines indexed Chinese/English keywords with local BGE vectors,
//! then reranks on device. Keyword search remains available before model setup
//! and when optional local inference fails.

use echo_agent_memory::{
    backend::MemoryBackendImpl,
    embedding::{ConfiguredEmbeddingProvider, EmbeddingProvider},
    index::{MemoryIndex, init_sqlite_vec},
    reranker::ConfiguredReranker,
    search::SearchResult,
    storage::MemoryStorage,
};
use echo_agent_runtime::config::{MemoryEmbeddingConfig, MemoryIndexConfig, MemorySearchConfig};
use echo_agent_tools::types::memory_backend::MemoryBackend;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

const INDEX_VERSION: u32 = 1;
const INDEX_DIR: &str = "personal-knowledge";
const INDEX_DOCUMENTS_DIR: &str = "documents";
const INDEX_DATABASE: &str = "index.sqlite";
const INDEX_MANIFEST: &str = "manifest.json";
const MAX_LOCAL_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_OFFICE_ENTRY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OFFICE_XML_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LOCAL_SCAN_ENTRIES: usize = 10_000;
const MAX_LOCAL_FILES: usize = 500;
const MAX_INDEXED_CONTENT_CHARS: usize = 512 * 1024;
const MAX_QUERY_CHARS: usize = 8_192;
const MAX_SEARCH_RESULTS: usize = 20;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(6);
const RERANK_TIMEOUT: Duration = Duration::from_secs(4);
const EMBED_BATCH_TIMEOUT: Duration = Duration::from_secs(45);
const SEARCH_CANCELLED: &str = "personal knowledge search cancelled";
const BACKGROUND_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const MAX_SEARCH_REQUEST_ID_CHARS: usize = 128;

static INDEX_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static RUNTIME_STATUS: OnceLock<Mutex<RuntimeIndexStatus>> = OnceLock::new();
static REBUILD_SCHEDULE: OnceLock<Mutex<RebuildScheduleState>> = OnceLock::new();
static ACTIVE_SEARCHES: OnceLock<Mutex<HashMap<String, Arc<CancellationToken>>>> = OnceLock::new();

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSource {
    id: String,
    label: String,
    root: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone)]
struct SourceFile {
    path: PathBuf,
    source_id: String,
    source_label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexManifest {
    version: u32,
    embedding_model: String,
    #[serde(default)]
    generation: String,
    #[serde(default)]
    index_fingerprint: String,
    #[serde(default)]
    warnings: Vec<String>,
    last_updated_at: Option<u64>,
    entries: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    original_path: String,
    cache_path: String,
    title: String,
    source_id: String,
    source_label: String,
    modified_millis: u64,
    size: u64,
}

#[derive(Debug, Clone, Default)]
struct RuntimeIndexStatus {
    state: String,
    message: Option<String>,
    file_count: usize,
    chunk_count: usize,
    embedded_chunk_count: usize,
    pending_embedding_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalKnowledgeIndexStatus {
    warnings: Vec<String>,
    state: String,
    message: Option<String>,
    file_count: usize,
    chunk_count: usize,
    embedded_chunk_count: usize,
    pending_embedding_count: usize,
    last_updated_at: Option<u64>,
    embedding_model: &'static str,
    rerank_model: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalKnowledgeEntry {
    id: String,
    title: String,
    snippet: String,
    source: String,
    source_label: String,
    url: String,
    path: String,
    start_line: usize,
    end_line: usize,
    score: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalKnowledgeSearchResponse {
    items: Vec<PersonalKnowledgeEntry>,
    retrieval_mode: String,
    degraded_reason: Option<String>,
    index: PersonalKnowledgeIndexStatus,
}

#[derive(Debug)]
struct SyncResult {
    root: PathBuf,
    manifest: IndexManifest,
    chunk_count: usize,
    pending_embedding_count: usize,
    vec_available: bool,
}

#[derive(Debug, Default)]
struct RebuildScheduleState {
    running: bool,
    pending: bool,
    force: bool,
    last_finished_at: Option<Instant>,
}

fn index_lock() -> &'static tokio::sync::Mutex<()> {
    INDEX_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn runtime_status() -> &'static Mutex<RuntimeIndexStatus> {
    RUNTIME_STATUS.get_or_init(|| {
        Mutex::new(RuntimeIndexStatus {
            state: "idle".to_owned(),
            ..RuntimeIndexStatus::default()
        })
    })
}

fn rebuild_schedule() -> &'static Mutex<RebuildScheduleState> {
    REBUILD_SCHEDULE.get_or_init(|| Mutex::new(RebuildScheduleState::default()))
}

fn active_searches() -> &'static Mutex<HashMap<String, Arc<CancellationToken>>> {
    ACTIVE_SEARCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn update_runtime_status(update: impl FnOnce(&mut RuntimeIndexStatus)) {
    if let Ok(mut status) = runtime_status().lock() {
        update(&mut status);
    }
}

fn base_index_root() -> PathBuf {
    crate::paths::echo_agent_home_dir().join(INDEX_DIR)
}

fn valid_generation(value: &str) -> bool {
    value.len() == 24 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn index_root() -> PathBuf {
    let base = base_index_root();
    let generation = std::fs::read(base.join("active.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<String>(&bytes).ok());
    generation
        .filter(|g| valid_generation(g))
        .map(|g| base.join("indexes").join(g))
        .unwrap_or(base)
}

fn target_fingerprint() -> String {
    format!(
        "{}:office-structural-v3:{}:tokenizers={}",
        echo_agent_memory::local::fingerprint(),
        echo_agent_memory::chunker::CHUNK_PIPELINE_VERSION,
        echo_agent_memory::local::available(false)
    )
}

fn target_index_root() -> PathBuf {
    let active = index_root();
    let manifest = load_manifest_at(&active);
    if valid_generation(&manifest.generation) && manifest.index_fingerprint == target_fingerprint()
    {
        return active;
    }
    let digest = hex::encode(Sha256::digest(target_fingerprint().as_bytes()));
    base_index_root().join("indexes").join(&digest[..24])
}

fn root_for_manifest(manifest: &IndexManifest) -> PathBuf {
    if valid_generation(&manifest.generation) {
        base_index_root().join("indexes").join(&manifest.generation)
    } else {
        base_index_root()
    }
}

fn activate_index(root: &Path) -> Result<(), String> {
    let generation = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|g| valid_generation(g))
        .ok_or("索引代次无效")?;
    std::fs::create_dir_all(base_index_root()).map_err(|e| e.to_string())?;
    crate::paths::write_private_file(
        &base_index_root().join("active.json"),
        &serde_json::to_vec(generation).map_err(|e| e.to_string())?,
    )?;
    prune_old_generations(root);
    Ok(())
}

fn prune_old_generations(active: &Path) {
    let Ok(entries) = std::fs::read_dir(base_index_root().join("indexes")) else {
        return;
    };
    let mut generations = entries
        .flatten()
        .filter_map(|entry| {
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
            (metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && entry.file_name().to_str().is_some_and(valid_generation))
            .then(|| (entry.path(), metadata.modified().unwrap_or(UNIX_EPOCH)))
        })
        .collect::<Vec<_>>();
    generations.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    for (path, modified) in generations.into_iter().skip(3) {
        // Keep a grace day for in-flight snapshots and recent rollback. These
        // directories contain derived caches only, never the original sources.
        if path != active && modified.elapsed().is_ok_and(|age| age.as_secs() > 86400) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn database_path() -> PathBuf {
    index_root().join(INDEX_DATABASE)
}
fn memory_storage_at(root: &Path) -> MemoryStorage {
    MemoryStorage::new_flat(root, root)
}
fn memory_storage() -> MemoryStorage {
    memory_storage_at(&index_root())
}

fn embedding_config() -> MemoryEmbeddingConfig {
    crate::retrieval_models::initialize();
    MemoryEmbeddingConfig {
        provider: "local".to_owned(),
        model: Some(crate::agent_runtime::MEMORY_EMBEDDING_MODEL.to_owned()),
        dimensions: crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
        endpoint: Some(crate::agent_runtime::MEMORY_EMBEDDING_ENDPOINT.to_owned()),
        api_key: None,
        send_dimensions: false,
    }
}

fn coarse_search_config(candidate_count: usize) -> MemorySearchConfig {
    let mut config = MemorySearchConfig {
        max_results: candidate_count,
        min_score: 0.1,
        vector_weight: 0.7,
        text_weight: 0.3,
        ..Default::default()
    };
    config.temporal_decay.enabled = false;
    config.mmr.enabled = false;
    config.reranker.enabled = false;
    config
}

fn reranker_config() -> MemorySearchConfig {
    let mut config = MemorySearchConfig::default();
    config.reranker.enabled = true;
    config.reranker.endpoint = Some(crate::agent_runtime::MEMORY_RERANK_ENDPOINT.to_owned());
    config.reranker.model = Some(crate::agent_runtime::MEMORY_RERANK_MODEL.to_owned());
    config.reranker.api_key = None;
    config
}

fn embedding_provider() -> Option<ConfiguredEmbeddingProvider> {
    ConfiguredEmbeddingProvider::from_session(
        &embedding_config(),
        crate::agent_runtime::MEMORY_EMBEDDING_ENDPOINT.to_owned(),
        String::new(),
    )
}

fn load_sources() -> Result<Vec<LocalSource>, String> {
    let value = crate::org::read_local_kb_sources()?;
    let items = value
        .as_array()
        .ok_or_else(|| "personal knowledge sources must be an array".to_owned())?;
    let mut sources = Vec::new();
    for item in items {
        let source: LocalSource = serde_json::from_value(item.clone())
            .map_err(|error| format!("decode personal knowledge source: {error}"))?;
        if !source.enabled || source.id.trim().is_empty() || source.root.trim().is_empty() {
            continue;
        }
        let Ok(root) = std::fs::canonicalize(&source.root) else {
            continue;
        };
        if root.is_dir() {
            sources.push(LocalSource {
                root: root.to_string_lossy().into_owned(),
                ..source
            });
        }
    }
    Ok(sources)
}

pub(crate) fn configured() -> bool {
    load_sources().is_ok_and(|sources| !sources.is_empty())
}

fn authorized_source<'a>(
    source_id: &str,
    original_path: &str,
    sources: &'a [LocalSource],
) -> Option<&'a LocalSource> {
    let path = Path::new(original_path);
    // Recheck the real file, too: a deleted file or a newly planted symlink
    // must not regain access through a still-persisted index entry.
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    sources.iter().find(|source| {
        source.enabled && source.id == source_id && canonical.starts_with(&source.root)
    })
}

fn authorize_manifest(manifest: &mut IndexManifest, sources: &[LocalSource]) {
    manifest.entries.retain_mut(|entry| {
        let Some(source) = authorized_source(&entry.source_id, &entry.original_path, sources)
        else {
            return false;
        };
        entry.source_label.clone_from(&source.label);
        true
    });
}

fn authorize_results(items: &mut Vec<PersonalKnowledgeEntry>, sources: &[LocalSource]) {
    items.retain_mut(|entry| {
        let Some(source) = authorized_source(&entry.source, &entry.path, sources) else {
            return false;
        };
        entry.source_label.clone_from(&source.label);
        true
    });
}

fn supported_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "mdx" | "txt" | "rst" | "log" | "docx" | "pptx" | "xlsx")
    )
}

fn scan_files() -> Result<(Vec<SourceFile>, bool), String> {
    let mut truncated = false;
    let mut out = Vec::new();
    let mut seen_files = HashSet::new();
    let mut scanned_entries = 0usize;
    for source in load_sources()? {
        let root = PathBuf::from(&source.root);
        let mut queue = VecDeque::from([(root.clone(), 0usize)]);
        while let Some((directory, depth)) = queue.pop_front() {
            if depth > 5
                || out.len() >= MAX_LOCAL_FILES
                || scanned_entries >= MAX_LOCAL_SCAN_ENTRIES
            {
                truncated = true;
                continue;
            }
            let Ok(canonical_directory) = std::fs::canonicalize(&directory) else {
                continue;
            };
            if !canonical_directory.starts_with(&root) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&canonical_directory) else {
                continue;
            };
            for entry in entries.flatten() {
                scanned_entries = scanned_entries.saturating_add(1);
                if scanned_entries > MAX_LOCAL_SCAN_ENTRIES {
                    break;
                }
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }
                let Ok(canonical) = std::fs::canonicalize(&path) else {
                    continue;
                };
                if !canonical.starts_with(&root) {
                    continue;
                }
                if metadata.is_dir() && depth < 5 {
                    queue.push_back((canonical, depth + 1));
                } else if metadata.is_dir() {
                    truncated = true;
                } else if metadata.is_file()
                    && supported_file(&canonical)
                    && seen_files.insert(canonical.clone())
                {
                    out.push(SourceFile {
                        path: canonical,
                        source_id: source.id.clone(),
                        source_label: source.label.clone(),
                    });
                    if out.len() >= MAX_LOCAL_FILES {
                        break;
                    }
                }
            }
        }
        if out.len() >= MAX_LOCAL_FILES || scanned_entries >= MAX_LOCAL_SCAN_ENTRIES {
            break;
        }
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    let truncated =
        truncated || out.len() >= MAX_LOCAL_FILES || scanned_entries >= MAX_LOCAL_SCAN_ENTRIES;
    Ok((out, truncated))
}

fn read_source_file(path: &Path) -> Result<String, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("read personal knowledge metadata: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("personal knowledge path must be a regular file".into());
    }
    if metadata.len() > MAX_LOCAL_FILE_BYTES {
        return Err("personal knowledge file exceeds 5MB".into());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = crate::shell_fs::read_regular_file_bounded(path, MAX_LOCAL_FILE_BYTES)
        .map_err(|error| format!("read personal knowledge: {error}"))?;
    if matches!(extension.as_str(), "docx" | "pptx" | "xlsx") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| format!("open personal knowledge Office ZIP: {error}"))?;
        let mut parts = Vec::new();
        let mut total_xml_bytes = 0_u64;
        for index in 0..archive.len().min(500) {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| format!("read personal knowledge Office entry: {error}"))?;
            let name = entry
                .name()
                .map_err(|error| format!("文档 ZIP 文件名无效：{error}"))?
                .into_owned();
            let selected = match extension.as_str() {
                "docx" => name == "word/document.xml",
                "pptx" => {
                    name == "ppt/presentation.xml"
                        || name == "ppt/_rels/presentation.xml.rels"
                        || (name.starts_with("ppt/slides/") && name.ends_with(".xml"))
                }
                "xlsx" => {
                    name == "xl/_rels/workbook.xml.rels"
                        || ((name == "xl/sharedStrings.xml"
                            || name == "xl/workbook.xml"
                            || name.starts_with("xl/worksheets/sheet"))
                            && name.ends_with(".xml"))
                }
                _ => false,
            };
            if !selected {
                continue;
            }
            if entry.size() > MAX_OFFICE_ENTRY_BYTES {
                return Err("personal knowledge Office XML entry exceeds 2MB".into());
            }
            let remaining = MAX_OFFICE_XML_BYTES.saturating_sub(total_xml_bytes);
            if remaining == 0 || entry.size() > remaining {
                return Err("personal knowledge Office XML content exceeds 8MB".into());
            }
            let read_limit = remaining.min(MAX_OFFICE_ENTRY_BYTES);
            let mut bytes = Vec::new();
            (&mut entry)
                .take(read_limit + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("read personal knowledge Office XML: {error}"))?;
            if bytes.len() as u64 > read_limit {
                return Err("personal knowledge Office XML content exceeds its size limit".into());
            }
            total_xml_bytes = total_xml_bytes.saturating_add(bytes.len() as u64);
            let xml = String::from_utf8(bytes)
                .map_err(|_| "personal knowledge Office XML is not valid UTF-8".to_owned())?;
            parts.push((name, xml));
        }
        return crate::knowledge_documents::extract(&extension, parts);
    }
    String::from_utf8(bytes).map_err(|_| "personal knowledge file is not valid UTF-8".into())
}

fn canonical_authorized_path(raw: &str) -> Result<PathBuf, String> {
    let path = std::fs::canonicalize(raw)
        .map_err(|_| "personal knowledge file does not exist".to_owned())?;
    if !path.is_file() || !supported_file(&path) {
        return Err("unsupported personal knowledge file".into());
    }
    let roots = load_sources()?
        .into_iter()
        .map(|source| PathBuf::from(source.root))
        .collect::<Vec<_>>();
    if !roots.iter().any(|root| path.starts_with(root)) {
        return Err("personal knowledge path is outside configured roots".into());
    }
    Ok(path)
}

pub(crate) fn fetch(raw: &str) -> Result<String, String> {
    read_source_file(&canonical_authorized_path(raw)?)
}

fn modified_millis(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn cache_path_for(root: &Path, original: &Path) -> PathBuf {
    let digest = Sha256::digest(original.to_string_lossy().as_bytes());
    root.join(INDEX_DOCUMENTS_DIR)
        .join(format!("{:x}.md", digest))
}

fn title_for(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("未命名知识")
        .to_owned()
}

fn bounded_content(text: &str) -> String {
    text.chars().take(MAX_INDEXED_CONTENT_CHARS).collect()
}

fn index_document(title: &str, text: &str) -> String {
    format!("# {title}\n\n{}", bounded_content(text))
}

fn load_manifest() -> IndexManifest {
    load_manifest_at(&index_root())
}
fn load_manifest_at(root: &Path) -> IndexManifest {
    let Ok(bytes) = std::fs::read(root.join(INDEX_MANIFEST)) else {
        return IndexManifest::default();
    };
    serde_json::from_slice::<IndexManifest>(&bytes)
        .ok()
        .filter(|manifest| manifest.version == INDEX_VERSION)
        .unwrap_or_default()
}

fn save_manifest_at(root: &Path, manifest: &IndexManifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("encode personal knowledge index manifest: {error}"))?;
    crate::paths::write_private_file(&root.join(INDEX_MANIFEST), &bytes)
}

fn remove_database_files_at(root: &Path) {
    let database = root.join(INDEX_DATABASE);
    for path in [
        database.clone(),
        PathBuf::from(format!("{}-wal", database.to_string_lossy())),
        PathBuf::from(format!("{}-shm", database.to_string_lossy())),
    ] {
        let _ = std::fs::remove_file(path);
    }
}

fn open_index() -> Result<MemoryIndex, String> {
    open_index_at(&index_root())
}
fn open_index_at(root: &Path) -> Result<MemoryIndex, String> {
    init_sqlite_vec();
    MemoryIndex::open_or_create_preserving_dimensions(
        &root.join(INDEX_DATABASE),
        memory_storage_at(root),
        MemoryIndexConfig::default(),
        crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
    )
    .map_err(|error| format!("open personal knowledge index: {error}"))
}

fn sync_index(force: bool) -> Result<SyncResult, String> {
    let mut root = target_index_root();
    if force && root == index_root() {
        let identity = format!("{}:{}", target_fingerprint(), uuid::Uuid::now_v7());
        root = base_index_root()
            .join("indexes")
            .join(&hex::encode(Sha256::digest(identity.as_bytes()))[..24]);
    }
    let documents = root.join(INDEX_DOCUMENTS_DIR);
    std::fs::create_dir_all(&documents)
        .map_err(|error| format!("create personal knowledge index: {error}"))?;
    crate::paths::harden_private_dir(&root)?;
    crate::paths::harden_private_dir(&documents)?;

    let old_manifest = load_manifest_at(&root);
    if force {
        remove_database_files_at(&root);
    }
    let mut index = open_index_at(&root)?;
    let indexed_paths = index
        .all_indexed_paths()
        .map_err(|error| format!("read personal knowledge index paths: {error}"))?
        .into_iter()
        .collect::<HashSet<_>>();
    let old_by_original = old_manifest
        .entries
        .iter()
        .cloned()
        .map(|entry| (entry.original_path.clone(), entry))
        .collect::<HashMap<_, _>>();
    let mut next_entries = Vec::new();
    let (files, truncated) = scan_files()?;
    let mut skipped = Vec::new();
    for source_file in files {
        let Ok(metadata) = std::fs::metadata(&source_file.path) else {
            skipped.push(title_for(&source_file.path));
            continue;
        };
        if metadata.len() > MAX_LOCAL_FILE_BYTES {
            skipped.push(title_for(&source_file.path));
            continue;
        }
        let original_path = source_file.path.to_string_lossy().into_owned();
        let cache_path = cache_path_for(&root, &source_file.path);
        let cache_path_string = cache_path.to_string_lossy().into_owned();
        let title = title_for(&source_file.path);
        let modified = modified_millis(&metadata);
        let previous = old_by_original.get(&original_path);
        let unchanged = !force
            && previous.is_some_and(|entry| {
                entry.modified_millis == modified
                    && entry.size == metadata.len()
                    && Path::new(&entry.cache_path).is_file()
            });

        if !unchanged {
            match read_source_file(&source_file.path) {
                Ok(text) if !text.trim().is_empty() => {
                    crate::paths::write_private_file(
                        &cache_path,
                        index_document(&title, &text).as_bytes(),
                    )?;
                }
                Ok(_) => continue,
                Err(error) => {
                    skipped.push(title_for(&source_file.path));
                    tracing::warn!(path = %source_file.path.display(), %error, "personal knowledge file was skipped");
                    if let Some(previous) =
                        previous.filter(|entry| Path::new(&entry.cache_path).is_file())
                    {
                        if force || !indexed_paths.contains(&previous.cache_path) {
                            index
                                .reindex_file(Path::new(&previous.cache_path), "workspace")
                                .map_err(|error| {
                                    format!("reindex cached personal knowledge: {error}")
                                })?;
                        }
                        next_entries.push(previous.clone());
                    }
                    continue;
                }
            }
        }

        if force || !unchanged || !indexed_paths.contains(&cache_path_string) {
            index
                .reindex_file(&cache_path, "workspace")
                .map_err(|error| format!("index personal knowledge file: {error}"))?;
        }
        next_entries.push(ManifestEntry {
            original_path,
            cache_path: cache_path_string,
            title,
            source_id: source_file.source_id,
            source_label: source_file.source_label,
            modified_millis: modified,
            size: metadata.len(),
        });
    }

    let active_cache_paths = next_entries
        .iter()
        .map(|entry| entry.cache_path.clone())
        .collect::<HashSet<_>>();
    for old in &old_manifest.entries {
        if !active_cache_paths.contains(&old.cache_path) {
            index
                .delete_path(Path::new(&old.cache_path))
                .map_err(|error| format!("remove stale personal knowledge chunks: {error}"))?;
            let _ = std::fs::remove_file(&old.cache_path);
        }
    }
    for indexed in indexed_paths {
        if !active_cache_paths.contains(&indexed) {
            index
                .delete_path(Path::new(&indexed))
                .map_err(|error| format!("remove orphaned personal knowledge chunks: {error}"))?;
        }
    }

    next_entries.sort_by(|left, right| left.original_path.cmp(&right.original_path));
    let mut warnings = Vec::new();
    if truncated {
        warnings.push("已达到扫描范围上限（500 个文件、5 层目录或 10000 项），部分资料可能未索引；请缩小知识源范围".to_owned());
    }
    if !skipped.is_empty() {
        warnings.push(format!(
            "{} 个文件本次未能更新（超过 5MB、无法读取或解析失败）：{}{}",
            skipped.len(),
            skipped
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join("、"),
            if skipped.len() > 5 { "等" } else { "" }
        ));
    }
    let mut manifest = IndexManifest {
        warnings,
        version: INDEX_VERSION,
        generation: root.file_name().unwrap().to_string_lossy().into_owned(),
        index_fingerprint: target_fingerprint(),
        embedding_model: crate::agent_runtime::MEMORY_EMBEDDING_MODEL.to_owned(),
        last_updated_at: Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        ),
        entries: next_entries,
    };
    if manifest.entries.is_empty() {
        manifest.last_updated_at = None;
    }
    save_manifest_at(&root, &manifest)?;
    let vec_available = index.vec_available();
    let pending_embedding_count = index
        .chunks_without_embeddings()
        .map_err(|error| format!("read pending personal knowledge embeddings: {error}"))?
        .len();
    drop(index);
    let chunk_count = memory_storage_at(&root).total_chunk_count();
    // sqlite-vec is optional in the shared memory crate. Treat every chunk as
    // pending when the extension is unavailable so the UI never reports a
    // keyword-only index as fully semantic.
    let pending_embedding_count = if vec_available {
        pending_embedding_count
    } else {
        chunk_count
    };
    Ok(SyncResult {
        root,
        manifest,
        chunk_count,
        pending_embedding_count,
        vec_available,
    })
}

async fn embed_pending(root: &Path, max_batches: Option<usize>) -> Result<(usize, usize), String> {
    let Some(provider) = embedding_provider() else {
        return Err("本地语义模型尚未准备，请在知识库或记忆页面准备模型".into());
    };
    let pending = {
        let index = open_index_at(root)?;
        index
            .ensure_embedding_fingerprint(&provider.fingerprint())
            .map_err(|error| format!("validate personal knowledge embedding cache: {error}"))?;
        index
            .chunks_without_embeddings()
            .map_err(|error| format!("read personal knowledge embedding queue: {error}"))?
    };
    let total = pending.len();
    let batch_limit = max_batches.unwrap_or(usize::MAX);
    let mut embedded = 0usize;
    for batch in pending.chunks(32).take(batch_limit) {
        if !crate::org::local_knowledge_allowed().await {
            return Err("signed organization policy disables personal knowledge".into());
        }
        let mut authorized = load_manifest_at(root);
        authorize_manifest(&mut authorized, &load_sources()?);
        let allowed_paths = authorized
            .entries
            .iter()
            .map(|entry| entry.cache_path.as_str())
            .collect::<HashSet<_>>();
        let batch = {
            let index = open_index_at(root)?;
            batch
                .iter()
                .filter(|(chunk_id, _)| {
                    index
                        .get_chunk(chunk_id)
                        .ok()
                        .flatten()
                        .is_some_and(|chunk| allowed_paths.contains(chunk.path.as_str()))
                })
                .collect::<Vec<_>>()
        };
        if batch.is_empty() {
            continue;
        }
        let texts = batch
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>();
        let embeddings = tokio::time::timeout(EMBED_BATCH_TIMEOUT, provider.embed_batch(&texts))
            .await
            .map_err(|_| "personal knowledge embedding request timed out".to_owned())?
            .map_err(|error| format!("personal knowledge embedding request failed: {error}"))?;
        if embeddings.len() != batch.len()
            || embeddings.iter().any(|embedding| {
                embedding.len() != crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS
                    || embedding.iter().any(|value| !value.is_finite())
            })
        {
            return Err("personal knowledge embedding response has invalid dimensions".into());
        }
        // An in-flight provider call cannot be unsent. Do not commit its output
        // after a source/policy was revoked, or pass it to a later search.
        if !crate::org::local_knowledge_allowed().await {
            return Err("signed organization policy disables personal knowledge".into());
        }
        let mut current = load_manifest_at(root);
        authorize_manifest(&mut current, &load_sources()?);
        let current_paths = current
            .entries
            .iter()
            .map(|entry| entry.cache_path.as_str())
            .collect::<HashSet<_>>();
        let index = open_index_at(root)?;
        let mut batch_embedded = 0usize;
        for ((chunk_id, text), embedding) in batch.iter().zip(embeddings.iter()) {
            if !index
                .get_chunk(chunk_id)
                .map_err(|error| format!("validate embedded chunk: {error}"))?
                .is_some_and(|chunk| {
                    current_paths.contains(chunk.path.as_str())
                        && chunk.hash == echo_agent_memory::chunker::chunk_hash(text)
                })
            {
                continue;
            }
            index
                .upsert_embedding(chunk_id, embedding)
                .map_err(|error| format!("store personal knowledge embedding: {error}"))?;
            embedded += 1;
            batch_embedded += 1;
        }
        update_runtime_status(|status| {
            status.embedded_chunk_count =
                status.embedded_chunk_count.saturating_add(batch_embedded);
            status.pending_embedding_count = total.saturating_sub(embedded);
            status.message = Some(format!("正在生成语义索引（{embedded}/{total}）"));
        });
    }
    Ok((embedded, total.saturating_sub(embedded)))
}

fn index_status_snapshot() -> PersonalKnowledgeIndexStatus {
    let manifest = load_manifest();
    let disk_file_count = manifest.entries.len();
    let disk_chunk_count = if database_path().is_file() {
        memory_storage().total_chunk_count()
    } else {
        0
    };
    let disk_pending = if database_path().is_file() {
        open_index()
            .and_then(|index| {
                if !index.vec_available() {
                    return Ok(disk_chunk_count);
                }
                index
                    .chunks_without_embeddings()
                    .map(|pending| pending.len())
                    .map_err(|error| format!("read personal knowledge index status: {error}"))
            })
            .unwrap_or(disk_chunk_count)
    } else {
        0
    };
    let runtime = runtime_status()
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default();
    let active =
        runtime.state == "indexing" || runtime.state == "error" || runtime.state == "degraded";
    let file_count = if active {
        runtime.file_count
    } else {
        disk_file_count
    };
    let chunk_count = if active {
        runtime.chunk_count
    } else {
        disk_chunk_count
    };
    let pending_embedding_count = if active {
        runtime.pending_embedding_count
    } else {
        disk_pending
    };
    let embedded_chunk_count = if active {
        runtime.embedded_chunk_count
    } else {
        chunk_count.saturating_sub(pending_embedding_count)
    };
    let state = if active {
        runtime.state
    } else if file_count == 0 {
        "idle".to_owned()
    } else if pending_embedding_count > 0 {
        "degraded".to_owned()
    } else {
        "ready".to_owned()
    };
    PersonalKnowledgeIndexStatus {
        warnings: manifest.warnings,
        state,
        message: runtime.message,
        file_count,
        chunk_count,
        embedded_chunk_count,
        pending_embedding_count,
        last_updated_at: manifest.last_updated_at,
        embedding_model: crate::agent_runtime::MEMORY_EMBEDDING_MODEL,
        rerank_model: crate::agent_runtime::MEMORY_RERANK_MODEL,
    }
}

fn emit_status(app: &AppHandle) {
    let _ = app.emit("personal-knowledge://index-status", index_status_snapshot());
}

async fn rebuild_inner(
    app: Option<&AppHandle>,
    force: bool,
) -> Result<PersonalKnowledgeIndexStatus, String> {
    let _guard = index_lock().lock().await;
    update_runtime_status(|status| {
        *status = RuntimeIndexStatus {
            state: "indexing".to_owned(),
            message: Some("正在扫描个人知识文件".to_owned()),
            ..RuntimeIndexStatus::default()
        };
    });
    if let Some(app) = app {
        emit_status(app);
    }
    // Directory walking, Office extraction and SQLite indexing are blocking
    // work. Keep them off Tauri's async workers; the mutex only serializes
    // rebuild jobs and is never consulted by prompt-time search.
    let synced = match tokio::task::spawn_blocking(move || sync_index(force)).await {
        Ok(Ok(synced)) => synced,
        Ok(Err(error)) => {
            update_runtime_status(|status| {
                status.state = "error".to_owned();
                status.message = Some(error.clone());
            });
            if let Some(app) = app {
                emit_status(app);
            }
            return Err(error);
        }
        Err(error) => {
            let error = format!("personal knowledge indexing worker failed: {error}");
            update_runtime_status(|status| {
                status.state = "error".to_owned();
                status.message = Some(error.clone());
            });
            if let Some(app) = app {
                emit_status(app);
            }
            return Err(error);
        }
    };
    if load_manifest().entries.is_empty() {
        activate_index(&synced.root)?;
    }
    update_runtime_status(|status| {
        status.file_count = synced.manifest.entries.len();
        status.chunk_count = synced.chunk_count;
        status.embedded_chunk_count = synced
            .chunk_count
            .saturating_sub(synced.pending_embedding_count);
        status.pending_embedding_count = synced.pending_embedding_count;
        status.message = Some(if synced.pending_embedding_count > 0 {
            format!("正在生成语义索引（0/{}）", synced.pending_embedding_count)
        } else {
            "索引已是最新状态".to_owned()
        });
    });
    if let Some(app) = app {
        emit_status(app);
    }

    let embedding_result = if synced.vec_available && synced.pending_embedding_count > 0 {
        embed_pending(&synced.root, None).await
    } else {
        Ok((0, 0))
    };
    let remaining = if synced.vec_available {
        open_index_at(&synced.root)
            .and_then(|index| {
                index.chunks_without_embeddings().map_err(|error| {
                    format!("read remaining personal knowledge embeddings: {error}")
                })
            })
            .map(|pending| pending.len())
            .unwrap_or(synced.pending_embedding_count)
    } else {
        synced.chunk_count
    };
    if crate::org::local_knowledge_allowed().await {
        activate_index(&synced.root)?;
    }
    update_runtime_status(|status| match embedding_result {
        Ok(_) if remaining == 0 => {
            status.state = if status.file_count == 0 {
                "idle"
            } else {
                "ready"
            }
            .to_owned();
            status.message = if status.file_count == 0 {
                None
            } else {
                Some("语义索引已就绪".to_owned())
            };
            status.embedded_chunk_count = status.chunk_count;
            status.pending_embedding_count = 0;
        }
        Ok(_) => {
            status.state = "degraded".to_owned();
            status.message = Some("部分内容正在后台生成向量，当前仍可使用关键词检索".to_owned());
            status.pending_embedding_count = remaining;
            status.embedded_chunk_count = status.chunk_count.saturating_sub(remaining);
        }
        Err(ref error) => {
            status.state = "degraded".to_owned();
            status.message = Some(format!("本地语义检索暂不可用，已保留关键词检索：{error}"));
            status.pending_embedding_count = remaining;
            status.embedded_chunk_count = status.chunk_count.saturating_sub(remaining);
        }
    });
    if let Some(app) = app {
        emit_status(app);
    }
    Ok(index_status_snapshot())
}

pub(crate) fn schedule_rebuild(app: AppHandle, force: bool) {
    {
        let mut schedule = rebuild_schedule().lock().unwrap();
        schedule.pending = true;
        schedule.force |= force;
        if schedule.running {
            return;
        }
        schedule.running = true;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            let force = {
                let mut schedule = rebuild_schedule().lock().unwrap();
                schedule.pending = false;
                std::mem::take(&mut schedule.force)
            };
            if crate::org::local_knowledge_allowed().await {
                if let Err(error) = rebuild_inner(Some(&app), force).await {
                    tracing::warn!(%error, "personal knowledge background indexing failed");
                }
            }
            let mut schedule = rebuild_schedule().lock().unwrap();
            schedule.last_finished_at = Some(Instant::now());
            if schedule.pending {
                continue;
            }
            schedule.running = false;
            break;
        }
    });
}

pub(crate) fn start_background_index(app: AppHandle) {
    if configured() {
        schedule_rebuild(app, false);
    }
}

fn strip_cached_heading(snippet: &str, title: &str) -> String {
    snippet
        .strip_prefix(&format!("# {title}\n\n"))
        .unwrap_or(snippet)
        .trim()
        .to_owned()
}

fn map_results(
    results: Vec<SearchResult>,
    manifest: &IndexManifest,
) -> Vec<PersonalKnowledgeEntry> {
    let by_cache = manifest
        .entries
        .iter()
        .map(|entry| (entry.cache_path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    results
        .into_iter()
        .filter_map(|result| {
            let entry = by_cache.get(result.path.as_str())?;
            let start_line = result.start_line.saturating_sub(2) + 1;
            let end_line = result.end_line.saturating_sub(2).max(start_line);
            Some(PersonalKnowledgeEntry {
                id: format!(
                    "{}#L{}-{}",
                    entry.original_path,
                    start_line,
                    &hex::encode(Sha256::digest(result.chunk_id.as_bytes()))[..12]
                ),
                title: entry.title.clone(),
                snippet: strip_cached_heading(&result.snippet, &entry.title),
                source: entry.source_id.clone(),
                source_label: entry.source_label.clone(),
                url: entry.original_path.clone(),
                path: entry.original_path.clone(),
                start_line,
                end_line,
                score: result.score,
            })
        })
        .collect()
}

fn expand_neighbor_context(results: &mut [SearchResult], manifest: &IndexManifest) {
    let Ok(index) = open_index_at(&root_for_manifest(manifest)) else {
        return;
    };
    let selected = results
        .iter()
        .map(|result| result.chunk_id.clone())
        .collect::<HashSet<_>>();
    for result in results {
        let Ok(neighbors) = index.neighboring_chunks(&result.chunk_id) else {
            continue;
        };
        for neighbor in neighbors {
            if selected.contains(&neighbor.id) {
                continue;
            }
            let candidate = if neighbor.rowid
                < index
                    .get_chunk(&result.chunk_id)
                    .ok()
                    .flatten()
                    .map_or(0, |chunk| chunk.rowid)
            {
                format!("{}\n\n{}", neighbor.text, result.snippet)
            } else {
                format!("{}\n\n{}", result.snippet, neighbor.text)
            };
            if echo_agent_memory::local::token_count(&candidate) > 700 {
                continue;
            }
            result.snippet = candidate;
            result.start_line = result.start_line.min(neighbor.start_line);
            result.end_line = result.end_line.max(neighbor.end_line);
        }
    }
}

async fn rerank_candidates(
    query: &str,
    candidates: Vec<SearchResult>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<SearchResult>, String> {
    let config = reranker_config();
    let reranker = ConfiguredReranker::from_config(&config.reranker)
        .ok_or_else(|| "本地相关性模型尚未准备".to_owned())?;
    tokio::select! {
        _ = cancellation.cancelled() => Err(SEARCH_CANCELLED.to_owned()),
        result = tokio::time::timeout(RERANK_TIMEOUT, reranker.rerank(query, &candidates, limit)) => {
            result
                .map_err(|_| "personal knowledge rerank request timed out".to_owned())?
                .map_err(|error| format!("personal knowledge rerank request failed: {error}"))
        }
    }
}

async fn search_hybrid(
    query: &str,
    limit: usize,
    manifest: &IndexManifest,
    cancellation: &CancellationToken,
) -> Result<(Vec<PersonalKnowledgeEntry>, bool, Option<String>), String> {
    let candidate_count = limit.saturating_mul(3).clamp(limit, 20);
    let config = coarse_search_config(candidate_count);
    let root = root_for_manifest(manifest);
    let backend = MemoryBackendImpl::new(root.join(INDEX_DATABASE), memory_storage_at(&root))
        .with_embedding(
            embedding_config(),
            crate::agent_runtime::MEMORY_EMBEDDING_ENDPOINT.to_owned(),
            None,
        )
        .with_search_config(config)
        .with_query_only_embedding_search();
    let coarse = tokio::select! {
        _ = cancellation.cancelled() => return Err(SEARCH_CANCELLED.to_owned()),
        result = tokio::time::timeout(SEARCH_TIMEOUT, backend.search(query, candidate_count, 0.1)) => {
            result
                .map_err(|_| "personal knowledge hybrid search timed out".to_owned())?
                .map_err(|error| format!("personal knowledge hybrid search failed: {error}"))?
        }
    };
    // A source may have been revoked while the embedding query was running.
    // Exclude it before reranking so revoked snippets never reach that service.
    let mut authorized = manifest.clone();
    authorize_manifest(&mut authorized, &load_sources()?);
    let allowed_cache_paths = authorized
        .entries
        .iter()
        .map(|entry| entry.cache_path.as_str())
        .collect::<HashSet<_>>();
    let candidates = coarse
        .into_iter()
        .filter(|result| allowed_cache_paths.contains(result.path.as_str()))
        .map(|result| SearchResult {
            chunk_id: result.chunk_id,
            path: result.path,
            start_line: result.start_line,
            end_line: result.end_line,
            score: result.score,
            snippet: result.snippet,
            source: result.source,
            created_at: result.created_at.unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok((map_results(candidates, manifest), false, None));
    }
    match rerank_candidates(query, candidates.clone(), limit, cancellation).await {
        Ok(mut reranked) => {
            echo_agent_memory::search::select_evidence(&mut reranked, limit);
            expand_neighbor_context(&mut reranked, manifest);
            Ok((map_results(reranked, manifest), true, None))
        }
        Err(error) if error == SEARCH_CANCELLED => Err(error),
        Err(error) => {
            tracing::warn!(%error, "personal knowledge reranking degraded to hybrid ordering");
            Ok((
                map_results(
                    candidates
                        .into_iter()
                        .filter(|candidate| candidate.score >= 0.6)
                        .take(limit)
                        .collect(),
                    manifest,
                ),
                false,
                Some(error),
            ))
        }
    }
}

fn search_terms(query: &str) -> Vec<String> {
    let normalized = query.trim().to_lowercase();
    let mut terms = vec![normalized.clone()];
    let mut current = String::new();
    let mut sequences = Vec::new();
    for character in normalized.chars() {
        if character.is_alphanumeric() || matches!(character, '_' | '-' | '.') {
            current.push(character);
        } else if !current.is_empty() {
            sequences.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        sequences.push(current);
    }
    let ignored = [
        "什么",
        "怎么",
        "如何",
        "是否",
        "可以",
        "请问",
        "一下",
        "哪些",
        "为什么",
        "关于",
        "介绍",
        "告诉",
        "帮我",
        "根据",
        "知识",
        "文件",
        "里面",
        "内容",
        "这个",
        "那个",
        "是什么",
        "请说明",
    ];
    for sequence in sequences {
        let chars = sequence.chars().collect::<Vec<_>>();
        if chars
            .iter()
            .any(|character| matches!(*character, '\u{3400}'..='\u{9fff}'))
        {
            for size in [4usize, 3, 2] {
                for window in chars.windows(size) {
                    let term = window.iter().collect::<String>();
                    if !ignored.contains(&term.as_str()) {
                        terms.push(term);
                    }
                }
            }
        } else if chars.len() >= 2 {
            terms.push(sequence);
        }
    }
    let mut seen = HashSet::new();
    terms.retain(|term| term.chars().count() >= 2 && seen.insert(term.clone()));
    terms.sort_by_key(|right| std::cmp::Reverse(right.chars().count()));
    terms.truncate(12);
    terms
}

fn snippet_around(text: &str, byte_position: usize, max_chars: usize) -> String {
    let approximate_character = text
        .get(..byte_position)
        .map(|prefix| prefix.chars().count())
        .unwrap_or_default();
    let start = approximate_character.saturating_sub(max_chars / 4);
    let snippet = text.chars().skip(start).take(max_chars).collect::<String>();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        snippet,
        if start + max_chars < text.chars().count() {
            "…"
        } else {
            ""
        }
    )
}

/// Fast degradation path backed by the already-built SQLite FTS index.
/// Prompt-time retrieval must never rescan and reread the user's source tree.
fn indexed_lexical_search(
    query: &str,
    limit: usize,
    manifest: &IndexManifest,
) -> Result<Vec<PersonalKnowledgeEntry>, String> {
    let root = root_for_manifest(manifest);
    if !root.join(INDEX_DATABASE).is_file() {
        return Ok(Vec::new());
    }
    let index = open_index_at(&root)?;
    let terms = search_terms(query);
    let by_cache = manifest
        .entries
        .iter()
        .map(|entry| (entry.cache_path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let mut by_id: HashMap<String, PersonalKnowledgeEntry> = HashMap::new();
    let candidate_limit = limit.saturating_mul(4).clamp(limit, 80);
    for (term_index, term) in terms.iter().enumerate() {
        let hits = index
            .search_fts(term, candidate_limit)
            .map_err(|error| format!("search personal knowledge FTS index: {error}"))?;
        for hit in hits {
            let Some(chunk) = index
                .get_chunk(&hit.chunk_id)
                .map_err(|error| format!("read personal knowledge FTS result: {error}"))?
            else {
                continue;
            };
            let Some(source) = by_cache.get(chunk.path.as_str()) else {
                continue;
            };
            if echo_agent_memory::search::lexical_query_coverage(query, &chunk.text) < 0.2 {
                continue;
            }
            let start_line = chunk.start_line.saturating_sub(2) + 1;
            let end_line = chunk.end_line.saturating_sub(2).max(start_line);
            let id = format!(
                "{}#L{}-{}",
                source.original_path,
                start_line,
                &hex::encode(Sha256::digest(chunk.id.as_bytes()))[..12]
            );
            let title_hit = source.title.to_lowercase().contains(term);
            let rank_quality = 1.0 / (1.0 + hit.rank.abs());
            let score = (terms.len().saturating_sub(term_index) * 10 + term.chars().count() * 2)
                as f64
                + if title_hit { 20.0 } else { 0.0 }
                + rank_quality;
            let text = strip_cached_heading(&chunk.text, &source.title);
            let candidate = PersonalKnowledgeEntry {
                id: id.clone(),
                title: source.title.clone(),
                snippet: snippet_around(&text, 0, 640),
                source: source.source_id.clone(),
                source_label: source.source_label.clone(),
                url: source.original_path.clone(),
                path: source.original_path.clone(),
                start_line,
                end_line,
                score,
            };
            if by_id
                .get(&id)
                .is_none_or(|previous| candidate.score > previous.score)
            {
                by_id.insert(id, candidate);
            }
        }
    }
    let mut results = by_id.into_values().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.title.cmp(&right.title))
    });
    results.truncate(limit);
    Ok(results)
}

async fn indexed_lexical_search_async(
    query: &str,
    limit: usize,
    manifest: &IndexManifest,
    cancellation: &CancellationToken,
) -> Result<Vec<PersonalKnowledgeEntry>, String> {
    let query = query.to_owned();
    let manifest = manifest.clone();
    let mut task =
        tokio::task::spawn_blocking(move || indexed_lexical_search(&query, limit, &manifest));
    tokio::select! {
        _ = cancellation.cancelled() => {
            task.abort();
            Err(SEARCH_CANCELLED.to_owned())
        }
        result = &mut task => result
            .map_err(|error| format!("personal knowledge keyword search worker failed: {error}"))?,
    }
}

fn should_refresh_in_background(manifest: &IndexManifest) -> bool {
    if let Ok(schedule) = rebuild_schedule().lock() {
        if schedule.running
            || schedule
                .last_finished_at
                .is_some_and(|finished| finished.elapsed() < BACKGROUND_REFRESH_INTERVAL)
        {
            return false;
        }
    }
    let Some(last_updated_at) = manifest.last_updated_at else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.saturating_sub(Duration::from_millis(last_updated_at)) >= BACKGROUND_REFRESH_INTERVAL
}

fn validate_search_request_id(request_id: &str) -> Result<(), String> {
    let request_id = request_id.trim();
    if request_id.is_empty()
        || request_id.chars().count() > MAX_SEARCH_REQUEST_ID_CHARS
        || request_id.chars().any(char::is_control)
    {
        return Err("personal knowledge search request id is invalid".to_owned());
    }
    Ok(())
}

fn register_search(request_id: &str) -> Result<Arc<CancellationToken>, String> {
    validate_search_request_id(request_id)?;
    let token = Arc::new(CancellationToken::new());
    let previous = active_searches()
        .lock()
        .map_err(|_| "personal knowledge search registry is unavailable".to_owned())?
        .insert(request_id.to_owned(), token.clone());
    if let Some(previous) = previous {
        previous.cancel();
    }
    Ok(token)
}

fn unregister_search(request_id: &str, token: &Arc<CancellationToken>) {
    if let Ok(mut searches) = active_searches().lock() {
        if searches
            .get(request_id)
            .is_some_and(|current| Arc::ptr_eq(current, token))
        {
            searches.remove(request_id);
        }
    }
}

pub(crate) async fn search(
    query: &str,
    limit: usize,
    app: Option<&AppHandle>,
    cancellation: &CancellationToken,
) -> Result<PersonalKnowledgeSearchResponse, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("personal knowledge query cannot be empty".into());
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(format!(
            "personal knowledge query cannot exceed {MAX_QUERY_CHARS} characters"
        ));
    }
    let limit = limit.clamp(1, MAX_SEARCH_RESULTS);
    if !crate::org::local_knowledge_allowed().await {
        return Err("signed organization policy disables personal knowledge".into());
    }
    if !configured() {
        return Ok(PersonalKnowledgeSearchResponse {
            items: Vec::new(),
            retrieval_mode: "none".to_owned(),
            degraded_reason: None,
            index: index_status_snapshot(),
        });
    }
    if cancellation.is_cancelled() {
        return Err(SEARCH_CANCELLED.to_owned());
    }

    // Prompt-time retrieval reads only the last committed index snapshot. File
    // walking, document extraction and embeddings are always background work.
    let mut manifest = load_manifest();
    // Revocation is synchronous authorization, independent of eventual index
    // cleanup and its potentially slow embedding worker.
    authorize_manifest(&mut manifest, &load_sources()?);
    if should_refresh_in_background(&manifest) {
        if let Some(app) = app {
            schedule_rebuild(app.clone(), false);
        }
    }
    let status = index_status_snapshot();
    if manifest.entries.is_empty() || !database_path().is_file() {
        return Ok(PersonalKnowledgeSearchResponse {
            items: Vec::new(),
            retrieval_mode: "keyword".to_owned(),
            degraded_reason: Some("个人知识索引正在后台建立，本次暂无可检索内容".to_owned()),
            index: status,
        });
    }

    let mut degraded_reason = (status.state == "indexing")
        .then(|| "个人知识索引正在后台更新，本次使用上一版完整索引".to_owned());
    let mut retrieval_mode = "keyword";
    let mut items = if status.embedded_chunk_count > 0
        && manifest.index_fingerprint == target_fingerprint()
        && echo_agent_memory::local::available(false)
    {
        match search_hybrid(query, limit, &manifest, cancellation).await {
            Ok((items, reranked, rerank_error)) if !items.is_empty() => {
                if let Some(error) = rerank_error {
                    degraded_reason = Some(format!("相关性重排暂不可用：{error}"));
                }
                retrieval_mode = if reranked {
                    "hybrid-reranked"
                } else {
                    "hybrid"
                };
                items
            }
            Ok((_, true, _)) => {
                retrieval_mode = "hybrid-reranked";
                Vec::new()
            }
            Ok(_) => indexed_lexical_search_async(query, limit, &manifest, cancellation).await?,
            Err(error) if error == SEARCH_CANCELLED => return Err(error),
            Err(error) => {
                tracing::warn!(%error, "personal knowledge semantic search degraded to indexed keyword retrieval");
                degraded_reason = Some(format!("语义检索暂不可用，本次已使用本地索引：{error}"));
                indexed_lexical_search_async(query, limit, &manifest, cancellation).await?
            }
        }
    } else {
        degraded_reason
            .get_or_insert_with(|| "语义索引正在后台生成，本次已使用关键词索引".to_owned());
        indexed_lexical_search_async(query, limit, &manifest, cancellation).await?
    };
    // Lexical jobs, embeddings and reranking can outlive a source/policy edit.
    // Reconcile against the current authority immediately before returning.
    if !crate::org::local_knowledge_allowed().await {
        return Err("signed organization policy disables personal knowledge".into());
    }
    authorize_results(&mut items, &load_sources()?);
    Ok(PersonalKnowledgeSearchResponse {
        items,
        retrieval_mode: retrieval_mode.to_owned(),
        degraded_reason,
        index: index_status_snapshot(),
    })
}

#[tauri::command]
pub async fn personal_knowledge_search(
    app: AppHandle,
    query: String,
    limit: Option<usize>,
    request_id: Option<String>,
) -> Result<PersonalKnowledgeSearchResponse, String> {
    let request_id = request_id
        .as_deref()
        .map(str::trim)
        .filter(|request_id| !request_id.is_empty());
    let token = match request_id {
        Some(request_id) => register_search(request_id)?,
        None => Arc::new(CancellationToken::new()),
    };
    let result = search(&query, limit.unwrap_or(5), Some(&app), token.as_ref()).await;
    if let Some(request_id) = request_id {
        unregister_search(request_id, &token);
    }
    result
}

#[tauri::command]
pub fn personal_knowledge_cancel_search(request_id: String) -> Result<bool, String> {
    validate_search_request_id(&request_id)?;
    let token = active_searches()
        .lock()
        .map_err(|_| "personal knowledge search registry is unavailable".to_owned())?
        .remove(request_id.trim());
    if let Some(token) = token {
        token.cancel();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
pub async fn personal_knowledge_rebuild(
    app: AppHandle,
) -> Result<PersonalKnowledgeIndexStatus, String> {
    if !crate::org::local_knowledge_allowed().await {
        return Err("当前组织策略或连接状态不允许读取个人知识库".into());
    }
    let result = rebuild_inner(Some(&app), true).await;
    if let Ok(mut schedule) = rebuild_schedule().lock() {
        schedule.last_finished_at = Some(Instant::now());
    }
    result
}

#[tauri::command]
pub fn personal_knowledge_index_status() -> PersonalKnowledgeIndexStatus {
    index_status_snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_zip_reader_preserves_reordered_presentation_pages_and_wide_sheet_cells() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let write_package = |name: &str, parts: &[(&str, &str)]| {
            let path = directory.path().join(name);
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for (part, content) in parts {
                zip.start_file(*part, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(content.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
            path
        };
        let presentation = write_package(
            "调整顺序.pptx",
            &[
                ("ppt/slides/slide1.xml", "<p><t>实际第二页</t></p>"),
                ("ppt/slides/slide2.xml", "<p><t>实际第一页</t></p>"),
                (
                    "ppt/presentation.xml",
                    r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="r2"/><p:sldId id="257" r:id="r1"/></p:sldIdLst></p:presentation>"#,
                ),
                (
                    "ppt/_rels/presentation.xml.rels",
                    r#"<Relationships><Relationship Id="r1" Target="slides/slide1.xml"/><Relationship Id="r2" Target="slides/slide2.xml"/></Relationships>"#,
                ),
            ],
        );
        assert_eq!(
            read_source_file(&presentation).unwrap(),
            "## 第 1 页\n\n实际第一页\n\n## 第 2 页\n\n实际第二页"
        );
        let spreadsheet = write_package(
            "宽表.xlsx",
            &[(
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><row r="1"><c r="AMJ1"><v>1024</v></c><c r="AMK1"><v>1025</v></c></row></worksheet>"#,
            )],
        );
        let extracted = read_source_file(&spreadsheet).unwrap();
        assert!(extracted.contains("AMJ1：1024"));
        assert!(extracted.contains("AMK1：1025"));
    }

    fn source_fixture() -> (tempfile::TempDir, Vec<LocalSource>, IndexManifest) {
        let directory = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        let mut entries = Vec::new();
        for id in ["active", "revoked"] {
            let root = directory.path().join(id);
            std::fs::create_dir(&root).unwrap();
            let path = root.join("中文 资料.md");
            std::fs::write(&path, format!("{id} 资料指纹")).unwrap();
            sources.push(LocalSource {
                id: id.into(),
                label: format!("{id} 当前名称"),
                root: root.canonicalize().unwrap().to_string_lossy().into_owned(),
                enabled: true,
            });
            entries.push(ManifestEntry {
                original_path: path.to_string_lossy().into_owned(),
                cache_path: format!("/cache/{id}.md"),
                title: format!("{id} 资料"),
                source_id: id.into(),
                source_label: "旧名称".into(),
                modified_millis: 1,
                size: 10,
            });
        }
        (
            directory,
            sources,
            IndexManifest {
                version: INDEX_VERSION,
                embedding_model: "test".into(),
                generation: String::new(),
                index_fingerprint: String::new(),
                warnings: Vec::new(),
                last_updated_at: Some(1),
                entries,
            },
        )
    }

    #[test]
    fn revoked_source_is_excluded_from_existing_manifest_without_rebuild() {
        let (_directory, sources, mut manifest) = source_fixture();
        assert_eq!(manifest.entries.len(), 2);
        authorize_manifest(&mut manifest, &sources[..1]);
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.entries[0].source_id, "active");
        assert_eq!(manifest.entries[0].source_label, "active 当前名称");
    }

    #[test]
    fn disabled_or_rebound_source_cannot_reuse_old_index_paths() {
        let (_directory, mut sources, manifest) = source_fixture();
        sources[0].enabled = false;
        let mut disabled = manifest.clone();
        authorize_manifest(&mut disabled, &sources);
        assert_eq!(disabled.entries.len(), 1);
        assert_eq!(disabled.entries[0].source_id, "revoked");
        sources[0].enabled = true;
        sources[0].root = sources[1].root.clone();
        let mut rebound = manifest;
        authorize_manifest(&mut rebound, &sources);
        assert_eq!(rebound.entries.len(), 1);
        assert_eq!(rebound.entries[0].source_id, "revoked");
    }

    #[test]
    fn completed_query_drops_results_revoked_while_it_was_running() {
        let (_directory, sources, manifest) = source_fixture();
        let mut items = map_results(
            manifest
                .entries
                .iter()
                .map(|entry| SearchResult {
                    chunk_id: format!("{}:0", entry.cache_path),
                    path: entry.cache_path.clone(),
                    start_line: 3,
                    end_line: 4,
                    score: 0.9,
                    snippet: "真实索引结果".into(),
                    source: "workspace".into(),
                    created_at: 1,
                })
                .collect(),
            &manifest,
        );
        assert_eq!(items.len(), 2);
        authorize_results(&mut items, &sources[..1]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, "active");
        assert_eq!(items[0].source_label, "active 当前名称");
    }

    #[test]
    fn deleted_source_files_are_excluded_before_index_cleanup() {
        let (_directory, sources, mut manifest) = source_fixture();
        std::fs::remove_file(&manifest.entries[1].original_path).unwrap();
        authorize_manifest(&mut manifest, &sources);
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.entries[0].source_id, "active");
    }

    #[cfg(unix)]
    #[test]
    fn source_file_replaced_with_symlink_cannot_escape_authorized_root() {
        let (_directory, sources, mut manifest) = source_fixture();
        let original = PathBuf::from(&manifest.entries[0].original_path);
        std::fs::remove_file(&original).unwrap();
        std::os::unix::fs::symlink(&manifest.entries[1].original_path, &original).unwrap();
        authorize_manifest(&mut manifest, &sources[..1]);
        assert!(manifest.entries.is_empty());
    }

    #[test]
    fn personal_retrieval_uses_requested_models() {
        let embedding = embedding_config();
        let search = reranker_config();
        assert_eq!(
            embedding.model.as_deref(),
            Some(echo_agent_memory::local::EMBEDDING_MODEL)
        );
        assert_eq!(embedding.dimensions, 768);
        assert_eq!(
            embedding.endpoint.as_deref(),
            Some(crate::agent_runtime::MEMORY_EMBEDDING_ENDPOINT)
        );
        assert!(embedding.api_key.is_none());
        assert_eq!(
            search.reranker.model.as_deref(),
            Some(echo_agent_memory::local::RERANK_MODEL)
        );
        assert_eq!(
            search.reranker.endpoint.as_deref(),
            Some(crate::agent_runtime::MEMORY_RERANK_ENDPOINT)
        );
        assert!(search.reranker.api_key.is_none());
        assert!(search.reranker.enabled);
    }

    #[test]
    fn lexical_terms_keep_exact_query_and_add_cjk_subphrases() {
        let terms = search_terms("公司的差旅费用标准是什么？");
        assert!(
            terms
                .iter()
                .any(|term| term == "公司的差旅费用标准是什么？")
        );
        assert!(terms.iter().any(|term| term == "差旅费用"));
        assert!(terms.iter().any(|term| term == "费用标准"));
    }

    #[test]
    fn cached_heading_is_not_exposed_as_answer_content() {
        assert_eq!(
            strip_cached_heading("# 休假制度\n\n员工每年享有十天年假。", "休假制度"),
            "员工每年享有十天年假。"
        );
    }

    #[test]
    fn cancelling_registered_search_reaches_its_native_token() {
        let request_id = "personal-search-cancel-test";
        let token = register_search(request_id).expect("register search");

        assert!(personal_knowledge_cancel_search(request_id.to_owned()).expect("cancel search"));
        assert!(token.is_cancelled());
        assert!(!personal_knowledge_cancel_search(request_id.to_owned()).expect("cancel again"));
    }

    #[test]
    fn search_request_ids_are_bounded() {
        assert!(validate_search_request_id("request-1").is_ok());
        assert!(validate_search_request_id("  ").is_err());
        assert!(validate_search_request_id(&"x".repeat(MAX_SEARCH_REQUEST_ID_CHARS + 1)).is_err());
    }

    #[test]
    fn indexed_cache_results_are_mapped_back_to_original_sources() {
        let manifest = IndexManifest {
            version: INDEX_VERSION,
            embedding_model: crate::agent_runtime::MEMORY_EMBEDDING_MODEL.to_owned(),
            generation: String::new(),
            index_fingerprint: String::new(),
            warnings: Vec::new(),
            last_updated_at: Some(1),
            entries: vec![ManifestEntry {
                original_path: "/notes/travel.md".to_owned(),
                cache_path: "/cache/hash.md".to_owned(),
                title: "差旅制度".to_owned(),
                source_id: "personal-notes".to_owned(),
                source_label: "个人笔记".to_owned(),
                modified_millis: 1,
                size: 10,
            }],
        };
        let mapped = map_results(
            vec![SearchResult {
                chunk_id: "/cache/hash.md:0".to_owned(),
                path: "/cache/hash.md".to_owned(),
                start_line: 3,
                end_line: 5,
                score: 0.91,
                snippet: "住宿标准为每晚 500 元。".to_owned(),
                source: "workspace".to_owned(),
                created_at: 1,
            }],
            &manifest,
        );

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].source, "personal-notes");
        assert_eq!(mapped[0].source_label, "个人笔记");
        assert_eq!(mapped[0].path, "/notes/travel.md");
        assert!(!mapped[0].path.starts_with("/cache"));
        assert_eq!(mapped[0].start_line, 2);
        assert_eq!(mapped[0].end_line, 3);
    }
}
