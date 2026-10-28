//! Structured, source-attributed memories. Markdown remains an editable legacy
//! source; the ledger is authoritative for new records and forgotten sources.
use crate::{storage::MemoryStorage, text_utils::strip_reasoning_blocks};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
};

const MAX_LEDGER_BYTES: u64 = 8 * 1024 * 1024;
pub const RECORD_DOCUMENT: &str = "REMEMBERED.md";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecord {
    pub id: String,
    pub scope: String,
    pub kind: String,
    pub key: String,
    pub content: String,
    pub status: String,
    pub pinned: bool,
    pub origin: String,
    pub confidence: f64,
    pub sources: Vec<String>,
    pub evidence: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub valid_until: Option<i64>,
    pub revision: u64,
    #[serde(default)]
    pub observed_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordInput {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub valid_until: Option<i64>,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Supplied by the checkpoint, never by generated JSON.
    #[serde(skip)]
    pub observed_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryActivity {
    pub attempted_at: Option<i64>,
    pub succeeded_at: Option<i64>,
    pub outcome: String,
    pub detail: String,
    pub records_written: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalActivity {
    pub at: i64,
    pub mode: String,
    pub count: usize,
    pub sources: Vec<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Ledger {
    records: Vec<MemoryRecord>,
    forgotten: Vec<String>,
    replaced: Vec<String>,
    blocked_sessions: HashSet<String>,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Read settings on use, so turning memory off stops running sessions too.
/// A malformed settings file fails closed instead of silently enabling memory.
pub fn setting(section: &str, child: Option<&str>, key: &str, default: bool) -> bool {
    setting_from_path(
        &echo_agent_config_home().join("config.toml"),
        section,
        child,
        key,
        default,
    )
}

fn setting_from_path(
    path: &Path,
    section: &str,
    child: Option<&str>,
    key: &str,
    default: bool,
) -> bool {
    let raw = match std::fs::read_to_string(path) {
        Ok(value) => value,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return default,
        Err(_) => return false,
    };
    let Ok(config) = toml::from_str::<toml::Value>(&raw) else {
        return false;
    };
    let mut value = config.get(section);
    if let Some(child) = child {
        value = value.and_then(|v| v.get(child));
    }
    value
        .and_then(|v| v.get(key))
        .and_then(toml::Value::as_bool)
        .unwrap_or(default)
}

fn echo_agent_config_home() -> PathBuf {
    echo_agent_tools::util::echo_agent_home::echo_agent_home()
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(e),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_LEDGER_BYTES
    {
        return Err(io::Error::other("invalid or oversized memory ledger"));
    }
    serde_json::from_slice(&std::fs::read(path)?).map_err(io::Error::other)
}

pub fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    use std::io::Write;
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        return Err(io::Error::other("memory ledger is full"));
    }
    if std::fs::read(path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("invalid memory path"))?;
    std::fs::create_dir_all(parent)?;
    if std::fs::symlink_metadata(parent)?.file_type().is_symlink()
        || std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink() || !m.is_file())
    {
        return Err(io::Error::other("memory files cannot be symbolic links"));
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn root(storage: &MemoryStorage, scope: &str) -> io::Result<PathBuf> {
    for directory in [storage.global_dir(), storage.workspace_dir()] {
        if std::fs::symlink_metadata(directory)
            .is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir())
        {
            return Err(io::Error::other(
                "memory directories must be real directories",
            ));
        }
    }
    match scope {
        "global" => Ok(storage.global_dir().to_owned()),
        "workspace" if !storage.is_ephemeral() => Ok(storage.workspace_dir().to_owned()),
        _ => Err(io::Error::other("请选择全局或持久工作区记忆")),
    }
}

fn load(storage: &MemoryStorage, scope: &str) -> io::Result<Ledger> {
    read_json(&root(storage, scope)?.join("records.json"))
}

fn lock_storage(storage: &MemoryStorage) -> io::Result<std::fs::File> {
    use fs2::FileExt;
    let dir = storage.global_dir();
    std::fs::create_dir_all(dir)?;
    if std::fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Err(io::Error::other("memory root cannot be a symbolic link"));
    }
    let path = dir.join(".records.lock");
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other("invalid memory lock"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn normalized(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn terms(text: &str) -> HashSet<String> {
    let mut values: HashSet<_> = crate::query_expansion::extract_keywords(text)
        .into_iter()
        .collect();
    values.extend(crate::query_expansion::extract_cjk_bigrams(text));
    values
}

fn related(a: &str, b: &str) -> bool {
    let na = normalized(a);
    let nb = normalized(b);
    if na.chars().count() >= 3 && (nb.contains(&na) || na.contains(&nb) && nb.chars().count() >= 3)
    {
        return true;
    }
    let ta = terms(a);
    let tb = terms(b);
    let common = ta.intersection(&tb).count();
    common >= 2 && common as f64 / ta.len().min(tb.len()).max(1) as f64 >= 0.65
}

#[derive(Debug)]
struct MemoryProjectionError(io::Error);
impl std::fmt::Display for MemoryProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "记忆事实已保存，但检索副本尚未更新：{}", self.0)
    }
}
impl std::error::Error for MemoryProjectionError {}

fn projection_document(ledger: &Ledger) -> String {
    let mut document = String::from("# Saved memories\n\n");
    for record in &ledger.records {
        if active(record) {
            document.push_str(&format!(
                "## {} · {}\n\n{}\n\nSource: {}\nUpdated: {}\n\n",
                record.kind,
                record.key,
                record.content,
                record.sources.join(", "),
                record.updated_at
            ));
        }
    }
    document
}

fn save(storage: &MemoryStorage, scope: &str, ledger: &Ledger) -> io::Result<()> {
    let dir = root(storage, scope)?;
    // Commit truth first. Search filters against this ledger even if projection
    // writing fails; the next index refresh regenerates it.
    write_json(&dir.join("records.json"), ledger)?;
    (|| -> io::Result<()> {
        let document = projection_document(ledger);
        let path = dir.join(RECORD_DOCUMENT);
        if std::fs::symlink_metadata(&path)
            .is_ok_and(|m| m.file_type().is_symlink() || !m.is_file())
        {
            return Err(io::Error::other("invalid memory projection"));
        }
        if std::fs::read_to_string(&path).ok().as_deref() == Some(document.as_str()) {
            return Ok(());
        }
        // This is a disposable projection. Its old contents must never become a
        // second source of forgotten/expired records.
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        file.write_all(document.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    })()
    .map_err(|error| io::Error::other(MemoryProjectionError(error)))
}

pub fn active(record: &MemoryRecord) -> bool {
    record.status == "active" && record.valid_until.is_none_or(|until| until > now())
}

pub fn list(storage: &MemoryStorage, include_workspace: bool) -> io::Result<Vec<MemoryRecord>> {
    let mut records = load(storage, "global")?.records;
    if include_workspace
        && storage.workspace_dir() != storage.global_dir()
        && !storage.is_ephemeral()
    {
        records.extend(load(storage, "workspace")?.records);
    }
    records.sort_by_key(|r| (std::cmp::Reverse(r.pinned), std::cmp::Reverse(r.updated_at)));
    Ok(records)
}

pub fn source_blocked(storage: &MemoryStorage, session_id: &str) -> io::Result<bool> {
    for scope in ["global", "workspace"] {
        if scope == "workspace" && storage.is_ephemeral() {
            continue;
        }
        if load(storage, scope)?.blocked_sessions.contains(session_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Immutable recall rules for one read operation. Load again for subsequent
/// requests so corrections and forgetting take effect without stale caches.
#[derive(Clone, Debug, Default)]
pub struct RecallRules {
    replaced: Vec<String>,
    forgotten: Vec<RecallTopic>,
    blocked_sessions: HashSet<String>,
    projections: std::collections::HashMap<PathBuf, RecallProjection>,
}

#[derive(Clone, Debug)]
struct RecallProjection {
    document: String,
    chunks: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<(usize, usize), HashSet<String>>>,
    >,
}

impl RecallProjection {
    fn contains(&self, content: &str, config: &echo_agent_config_types::MemoryIndexConfig) -> bool {
        let content = content.trim();
        if content.is_empty() {
            return false;
        }
        if self.document.contains(content) {
            return true;
        }
        // Chunking adds ancestor headers and overlap. Validate those exact
        // derived chunks rather than stripping arbitrary caller-supplied text.
        let mut cache = self
            .chunks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cache
            .entry((config.max_chunk_chars, config.chunk_overlap_chars))
            .or_insert_with(|| {
                crate::chunker::chunk_markdown(&self.document, config)
                    .into_iter()
                    .map(|chunk| chunk.text.trim().to_owned())
                    .collect()
            })
            .contains(content)
    }
}
#[derive(Clone, Debug)]
struct RecallTopic {
    normalized: String,
    length: usize,
    terms: HashSet<String>,
}
impl RecallTopic {
    fn new(text: String) -> Self {
        let normalized = normalized(&text);
        Self {
            length: normalized.chars().count(),
            normalized,
            terms: terms(&text),
        }
    }
}

impl RecallRules {
    pub fn load(storage: &MemoryStorage) -> io::Result<Self> {
        let mut rules = Self::default();
        for scope in ["global", "workspace"] {
            if scope == "workspace"
                && (storage.is_ephemeral() || storage.workspace_dir() == storage.global_dir())
            {
                continue;
            }
            let ledger = load(storage, scope)?;
            rules.projections.insert(
                root(storage, scope)?.join(RECORD_DOCUMENT),
                RecallProjection {
                    document: projection_document(&ledger),
                    chunks: Default::default(),
                },
            );
            rules.blocked_sessions.extend(ledger.blocked_sessions);
            rules
                .replaced
                .extend(ledger.replaced.into_iter().map(|text| normalized(&text)));
            rules
                .forgotten
                .extend(ledger.forgotten.into_iter().map(RecallTopic::new));
            for record in ledger.records {
                if record.status == "forgotten" {
                    rules.forgotten.push(RecallTopic::new(record.content));
                } else if !active(&record) {
                    rules.replaced.push(normalized(&record.content));
                }
            }
        }
        Ok(rules)
    }
    pub fn permits(&self, content: &str) -> bool {
        if self.replaced.is_empty() && self.forgotten.is_empty() {
            return true;
        }
        let normalized = normalized(content);
        if self.replaced.iter().any(|old| normalized.contains(old)) {
            return false;
        }
        self.permits_forgotten(content, &normalized)
    }

    fn permits_forgotten(&self, content: &str, normalized: &str) -> bool {
        let length = normalized.chars().count();
        if self.forgotten.iter().any(|topic| {
            topic.length >= 3
                && (normalized.contains(&topic.normalized)
                    || length >= 3 && topic.normalized.contains(normalized))
        }) {
            return false;
        }
        if self.forgotten.is_empty() {
            return true;
        }
        let content_terms = terms(content);
        !self.forgotten.iter().any(|topic| {
            let common = topic.terms.intersection(&content_terms).count();
            common >= 2
                && common as f64 / topic.terms.len().min(content_terms.len()).max(1) as f64 >= 0.65
        })
    }

    pub fn permits_candidate(&self, path: &Path, content: &str) -> bool {
        self.permits_candidate_with_config(path, content, &Default::default())
    }

    pub(crate) fn permits_candidate_with_config(
        &self,
        path: &Path,
        content: &str,
        config: &echo_agent_config_types::MemoryIndexConfig,
    ) -> bool {
        if !self.permits_path(path) {
            return false;
        }
        if let Some(current) = self.projections.get(path) {
            // Explicit current facts may extend corrected text. Only the
            // authoritative projection gets this exception; legacy text does not.
            return current.contains(content, config)
                && self.permits_forgotten(content, &normalized(content));
        }
        self.permits(content)
    }

    pub fn permits_path(&self, path: &Path) -> bool {
        let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
            return false;
        };
        !self.blocked_sessions.iter().any(|sid| {
            sid.chars().count() >= 8 && stem.ends_with(&sid.chars().take(8).collect::<String>())
        })
    }
}
// Single-document callers retain their short-circuiting path; group history
// explicitly loads RecallRules once for a bounded page.
pub fn permitted(storage: &MemoryStorage, content: &str) -> io::Result<bool> {
    for scope in ["global", "workspace"] {
        if scope == "workspace" && storage.is_ephemeral() {
            continue;
        }
        let ledger = load(storage, scope)?;
        if ledger
            .replaced
            .iter()
            .any(|old| normalized(content).contains(&normalized(old)))
        {
            return Ok(false);
        }
        if ledger.forgotten.iter().any(|topic| related(topic, content)) {
            return Ok(false);
        }
        if ledger.records.iter().any(|r| {
            if r.status == "forgotten" {
                related(&r.content, content)
            } else if !active(r) {
                normalized(content).contains(&normalized(&r.content))
            } else {
                false
            }
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_input(input: &mut RecordInput) -> io::Result<()> {
    if !matches!(input.scope.as_str(), "global" | "workspace") {
        return Err(io::Error::other("无效记忆范围"));
    }
    input.content = strip_reasoning_blocks(&input.content).trim().to_owned();
    input.key = input.key.trim().to_owned();
    if !["user", "feedback", "project", "reference", "task"].contains(&input.kind.as_str())
        || input.content.is_empty()
        || input.content.chars().count() > 4000
        || input.key.is_empty()
        || input.key.chars().count() > 160
        || input.evidence.chars().count() > 2000
    {
        return Err(io::Error::other(
            "记忆需要有效的类型、主题和内容（最多 4000 字）",
        ));
    }
    if input.kind == "task" && input.valid_until.is_none() {
        input.valid_until = Some(now() + 30 * 86400);
    }
    Ok(())
}

pub fn remember(
    storage: &MemoryStorage,
    mut input: RecordInput,
    source: Option<&str>,
    automatic: bool,
) -> io::Result<Option<MemoryRecord>> {
    validate_input(&mut input)?;
    let _guard = crate::storage::memory_mutation_lock()
        .lock()
        .map_err(|_| io::Error::other("memory lock poisoned"))?;
    let _file_lock = lock_storage(storage)?;
    if automatic && !storage.enabled() {
        return Ok(None);
    }
    if automatic
        && (source.is_some_and(|sid| source_blocked(storage, sid).unwrap_or(true))
            || !permitted(storage, &format!("{} {}", input.key, input.content))?)
    {
        return Ok(None);
    }
    let mut ledger = load(storage, &input.scope)?;
    let existing = if let Some(id) = &input.id {
        Some(
            ledger
                .records
                .iter()
                .position(|r| &r.id == id)
                .ok_or_else(|| io::Error::other("记忆已被删除，请刷新"))?,
        )
    } else {
        ledger
            .records
            .iter()
            .position(|r| normalized(&r.key) == normalized(&input.key) && r.kind == input.kind)
    };
    if let Some(index) = existing {
        if ledger.records.iter().enumerate().any(|(other, r)| {
            other != index && r.kind == input.kind && normalized(&r.key) == normalized(&input.key)
        }) {
            return Err(io::Error::other("已有同主题和类型的记忆，请更正原条目"));
        }
        let previous = &mut ledger.records[index];
        if !automatic && input.id.is_none() {
            if previous.content == input.content
                && previous.key == input.key
                && previous.kind == input.kind
                && previous.valid_until == input.valid_until
                && previous.pinned == input.pinned
                && previous.status == "active"
            {
                let record = previous.clone();
                save(storage, &input.scope, &ledger)?;
                return Ok(Some(record));
            }
            return Err(io::Error::other(
                "已有同主题和类型的记忆，请更正原条目；已有内容未修改",
            ));
        }
        if automatic && input.observed_at.unwrap_or_else(now) < previous.observed_at {
            return Ok(None);
        }
        if input
            .expected_revision
            .is_some_and(|rev| rev != previous.revision)
        {
            return Err(io::Error::other("记忆已被其他会话修改，请刷新后重试"));
        }
        if automatic
            && (previous.pinned || previous.origin == "manual" || previous.status != "active")
        {
            return Ok(None);
        }
        let new_source = source.filter(|s| !previous.sources.iter().any(|old| old == s));
        if previous.content == input.content
            && previous.valid_until == input.valid_until
            && previous.key == input.key
            && previous.kind == input.kind
            && previous.pinned == input.pinned
            && previous.status == "active"
            && (automatic || previous.origin == "manual")
            && new_source.is_none()
            && (!automatic || previous.observed_at == input.observed_at.unwrap_or_else(now))
        {
            let record = previous.clone();
            save(storage, &input.scope, &ledger)?;
            return Ok(if automatic { None } else { Some(record) });
        }
        // Old facts are excluded from legacy summaries and historical reads too.
        if previous.content != input.content {
            ledger.replaced.push(previous.content.clone());
        }
        previous.content = input.content;
        previous.key = input.key;
        previous.kind = input.kind;
        previous.evidence = input.evidence;
        previous.updated_at = now();
        previous.observed_at = input.observed_at.unwrap_or_else(now);
        previous.revision += 1;
        previous.status = "active".into();
        previous.valid_until = input.valid_until;
        previous.pinned = input.pinned;
        if !automatic {
            previous.origin = "manual".into();
        }
        if let Some(source) = source {
            if !previous.sources.iter().any(|s| s == source) {
                previous.sources.push(source.to_owned());
            }
        }
        let result = previous.clone();
        save(storage, &input.scope, &ledger)?;
        return Ok(Some(result));
    }
    if ledger.records.len() >= 10_000 {
        return Err(io::Error::other("记忆条目已满，请先整理"));
    }
    let timestamp = now();
    let id = blake3::hash(
        format!(
            "{}:{}:{}:{}",
            input.scope,
            input.kind,
            normalized(&input.key),
            ledger.records.len()
        )
        .as_bytes(),
    )
    .to_hex()[..24]
        .to_owned();
    let record = MemoryRecord {
        id,
        scope: input.scope.clone(),
        kind: input.kind,
        key: input.key,
        content: input.content,
        status: "active".into(),
        pinned: input.pinned,
        origin: if automatic { "automatic" } else { "manual" }.into(),
        confidence: if automatic { 0.85 } else { 1.0 },
        sources: source.into_iter().map(str::to_owned).collect(),
        evidence: input.evidence,
        created_at: timestamp,
        updated_at: timestamp,
        valid_until: input.valid_until,
        revision: 1,
        observed_at: input.observed_at.unwrap_or(timestamp),
    };
    ledger.records.push(record.clone());
    save(storage, &input.scope, &ledger)?;
    Ok(Some(record))
}

/// Forgetting keeps a small exclusion rule, never the original content as an
/// active memory. Known source conversations cannot silently teach it again.
pub fn retire(
    storage: &MemoryStorage,
    scope: &str,
    id: &str,
    revision: Option<u64>,
    completed: bool,
) -> io::Result<()> {
    retire_with_policy(storage, scope, id, revision, completed, false)
}

fn retire_with_policy(
    storage: &MemoryStorage,
    scope: &str,
    id: &str,
    revision: Option<u64>,
    completed: bool,
    automatic: bool,
) -> io::Result<()> {
    let _guard = crate::storage::memory_mutation_lock()
        .lock()
        .map_err(|_| io::Error::other("memory lock poisoned"))?;
    let _file_lock = lock_storage(storage)?;
    if automatic && !storage.enabled() {
        return Ok(());
    }
    let mut ledger = load(storage, scope)?;
    let record = ledger
        .records
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or_else(|| io::Error::other("记忆不存在"))?;
    if revision.is_some_and(|rev| rev != record.revision) {
        return Err(io::Error::other("记忆已变更，请刷新"));
    }
    if !completed {
        ledger.forgotten.push(record.content.clone());
        ledger.forgotten.push(record.key.clone());
        ledger
            .blocked_sessions
            .extend(record.sources.iter().cloned());
    }
    record.status = if completed { "completed" } else { "forgotten" }.into();
    record.updated_at = now();
    record.revision += 1;
    save(storage, scope, &ledger)
}

pub fn forget_topic(storage: &MemoryStorage, scope: &str, topic: &str) -> io::Result<usize> {
    if topic.trim().chars().count() < 2 || topic.chars().count() > 4000 {
        return Err(io::Error::other("请提供明确的遗忘内容"));
    }
    let _guard = crate::storage::memory_mutation_lock()
        .lock()
        .map_err(|_| io::Error::other("memory lock poisoned"))?;
    let _file_lock = lock_storage(storage)?;
    let mut ledger = load(storage, scope)?;
    let mut count = 0;
    for record in &mut ledger.records {
        if active(record) && (related(topic, &record.key) || related(topic, &record.content)) {
            ledger
                .blocked_sessions
                .extend(record.sources.iter().cloned());
            record.status = "forgotten".into();
            record.updated_at = now();
            record.revision += 1;
            count += 1;
        }
    }
    ledger.forgotten.push(topic.trim().to_owned());
    save(storage, scope, &ledger)?;
    Ok(count)
}

pub fn forget_source(storage: &MemoryStorage, session_id: &str) -> io::Result<usize> {
    let _guard = crate::storage::memory_mutation_lock()
        .lock()
        .map_err(|_| io::Error::other("memory lock poisoned"))?;
    let _file_lock = lock_storage(storage)?;
    let mut count = 0;
    for scope in ["global", "workspace"] {
        if scope == "workspace" && storage.is_ephemeral() {
            continue;
        }
        let mut ledger = load(storage, scope)?;
        ledger.blocked_sessions.insert(session_id.to_owned());
        for record in &mut ledger.records {
            if active(record) && record.sources.iter().any(|s| s == session_id) {
                record.status = "forgotten".into();
                record.updated_at = now();
                record.revision += 1;
                ledger.forgotten.push(record.content.clone());
                count += 1;
            }
        }
        save(storage, scope, &ledger)?;
    }
    Ok(count)
}

pub fn source_path_blocked(storage: &MemoryStorage, path: &Path) -> io::Result<bool> {
    let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    for scope in ["global", "workspace"] {
        if scope == "workspace" && storage.is_ephemeral() {
            continue;
        }
        if load(storage, scope)?.blocked_sessions.iter().any(|sid| {
            sid.chars().count() >= 8 && stem.ends_with(&sid.chars().take(8).collect::<String>())
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn core_context(storage: &MemoryStorage) -> io::Result<String> {
    let mut context = String::new();
    for record in list(storage, true)?.iter().filter(|r| {
        active(r) && (r.pinned || r.kind == "user" || r.kind == "feedback" || r.kind == "task")
    }) {
        let line = format!(
            "- [{} / {} / {}] {}: {}\n",
            record.id, record.scope, record.kind, record.key, record.content
        );
        if context.chars().count() + line.chars().count() > 6000 {
            continue;
        }
        context.push_str(&line);
    }
    // Existing global documents were maintained explicitly before records
    // existed. Keep their preferences available during migration too.
    if let Ok(legacy) = std::fs::read_to_string(storage.global_memory_file()) {
        if !crate::dream::is_scaffold_template(&legacy) {
            let remaining = 8000usize.saturating_sub(context.chars().count());
            let mut legacy_context = String::new();
            for paragraph in legacy.split("\n\n") {
                if permitted(storage, paragraph)? {
                    legacy_context.push_str(paragraph);
                    legacy_context.push_str("\n\n");
                }
            }
            context.extend(legacy_context.chars().take(remaining));
        }
    }
    Ok(context)
}

pub fn refresh_documents(storage: &MemoryStorage) -> io::Result<()> {
    let _guard = crate::storage::memory_mutation_lock()
        .lock()
        .map_err(|_| io::Error::other("memory lock poisoned"))?;
    let _file_lock = lock_storage(storage)?;
    for scope in ["global", "workspace"] {
        if scope == "workspace"
            && (storage.is_ephemeral() || storage.workspace_dir() == storage.global_dir())
        {
            continue;
        }
        let ledger = load(storage, scope)?;
        if !ledger.records.is_empty() {
            save(storage, scope, &ledger)?;
        }
    }
    Ok(())
}

pub fn activity(storage: &MemoryStorage) -> MemoryActivity {
    read_json(&storage.workspace_dir().join("activity.json")).unwrap_or_default()
}
pub fn set_activity(storage: &MemoryStorage, value: &MemoryActivity) -> io::Result<()> {
    write_json(&storage.workspace_dir().join("activity.json"), value)
}

pub fn defer_extraction(storage: &MemoryStorage) {
    let owners = extraction_owners()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !owners.contains(storage.workspace_dir()) {
        let _ = set_activity(
            storage,
            &MemoryActivity {
                outcome: "queued".into(),
                detail: "待整理内容已保存在本机，将在工作区可用时继续处理".into(),
                ..activity(storage)
            },
        );
    }
}
fn extraction_owners() -> &'static std::sync::Mutex<HashSet<PathBuf>> {
    static OWNERS: std::sync::OnceLock<std::sync::Mutex<HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    OWNERS.get_or_init(Default::default)
}

pub struct ExtractionActivityGuard(MemoryStorage);

fn has_pending_extractions(storage: &MemoryStorage) -> bool {
    std::fs::read_dir(storage.workspace_dir().join("pending")).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.path().extension().is_some_and(|ext| ext == "json")
        })
    })
}

impl Drop for ExtractionActivityGuard {
    fn drop(&mut self) {
        let mut owners = extraction_owners()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let current = activity(&self.0);
        if current.outcome == "running" {
            let pending = has_pending_extractions(&self.0);
            let _ = set_activity(
                &self.0,
                &MemoryActivity {
                    outcome: if pending { "queued" } else { "failed" }.into(),
                    detail: if pending {
                        "待整理内容已保存在本机，将在工作区可用时继续处理"
                    } else {
                        "提取已中断，请重试"
                    }
                    .into(),
                    ..current
                },
            );
        }
        owners.remove(self.0.workspace_dir());
    }
}

pub fn begin_extraction_activity(
    storage: &MemoryStorage,
) -> io::Result<Option<ExtractionActivityGuard>> {
    let mut owners = extraction_owners()
        .lock()
        .map_err(|_| io::Error::other("memory activity lock poisoned"))?;
    if owners.contains(storage.workspace_dir()) {
        return Ok(None);
    }
    set_activity(
        storage,
        &MemoryActivity {
            attempted_at: Some(now()),
            outcome: "running".into(),
            detail: String::new(),
            records_written: 0,
            ..activity(storage)
        },
    )?;
    owners.insert(storage.workspace_dir().to_owned());
    Ok(Some(ExtractionActivityGuard(storage.clone())))
}

pub fn record_extraction_recovery_error(storage: &MemoryStorage, detail: String) {
    let owners = extraction_owners()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !owners.contains(storage.workspace_dir()) {
        let _ = set_activity(
            storage,
            &MemoryActivity {
                outcome: "failed".into(),
                detail,
                ..activity(storage)
            },
        );
    }
}

pub fn recover_extraction_activity(storage: &MemoryStorage) {
    let owners = extraction_owners()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let current = activity(storage);
    if !owners.contains(storage.workspace_dir()) && current.outcome == "running" {
        let pending = has_pending_extractions(storage);
        let _ = set_activity(
            storage,
            &MemoryActivity {
                outcome: if pending { "queued" } else { "failed" }.into(),
                detail: if pending {
                    "上次待整理内容已保存在本机，将自动继续处理"
                } else {
                    "上次提取已中断，请重试"
                }
                .into(),
                ..current
            },
        );
    }
}

pub fn retrieval(storage: &MemoryStorage) -> RetrievalActivity {
    read_json(&storage.workspace_dir().join("retrieval.json")).unwrap_or_default()
}
pub fn set_retrieval(storage: &MemoryStorage, value: &RetrievalActivity) {
    let _ = write_json(&storage.workspace_dir().join("retrieval.json"), value);
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extraction {
    pub memories: Vec<RecordInput>,
    #[serde(default)]
    pub completed_keys: Vec<CompletedTask>,
}

#[derive(Debug, Deserialize)]
pub struct CompletedTask {
    pub key: String,
    pub scope: String,
    pub evidence: String,
}

/// A generated quote must actually occur in the supplied conversation. This
/// also prevents accidentally storing the model's private reasoning as fact.
pub fn grounded_extraction(json: &str, source_text: &str) -> io::Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(json).map_err(io::Error::other)?;
    let source: String = source_text.chars().filter(|c| !c.is_whitespace()).collect();
    let supported = |memory: &serde_json::Value| {
        let quote: String = memory
            .get("evidence")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        quote.chars().count() >= 4 && source.contains(&quote)
    };
    value
        .get_mut("memories")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| io::Error::other("invalid memory extraction"))?
        .retain(&supported);
    if let Some(completed) = value
        .get_mut("completedKeys")
        .and_then(serde_json::Value::as_array_mut)
    {
        completed.retain(supported);
    }
    serde_json::to_string(&value).map_err(io::Error::other)
}

pub fn extraction_context(storage: &MemoryStorage) -> io::Result<String> {
    let mut output = String::new();
    for record in list(storage, true)? {
        let line = serde_json::json!({ "id": record.id, "scope": record.scope, "kind": record.kind, "key": record.key, "content": record.content, "status": record.status, "pinned": record.pinned }).to_string();
        if output.chars().count() + line.chars().count() > 24_000 {
            continue;
        }
        output.push_str(&line);
        output.push('\n');
    }
    Ok(output)
}

pub fn apply_extraction(storage: &MemoryStorage, json: &str, source: &str) -> io::Result<String> {
    apply_extraction_at(storage, json, source, now())
}

pub fn apply_extraction_at(
    storage: &MemoryStorage,
    json: &str,
    source: &str,
    observed_at: i64,
) -> io::Result<String> {
    let mut extraction: Extraction = serde_json::from_str(json).map_err(io::Error::other)?;
    if extraction.memories.len() > 20 || extraction.completed_keys.len() > 20 {
        return Err(io::Error::other("too many extracted memories"));
    }
    for input in &mut extraction.memories {
        validate_input(input)?;
    }
    for completion in &extraction.completed_keys {
        if !matches!(completion.scope.as_str(), "global" | "workspace")
            || completion.key.trim().is_empty()
            || completion.key.chars().count() > 160
            || completion.evidence.chars().count() > 2000
        {
            return Err(io::Error::other("无效完成事项"));
        }
    }
    if source_blocked(storage, source)? {
        return Ok(String::new());
    }
    let partial_error = |written: usize, completed: usize, completing: bool, error: io::Error| {
        let committed = usize::from(
            error
                .get_ref()
                .is_some_and(|error| error.is::<MemoryProjectionError>()),
        );
        let written = written + if completing { 0 } else { committed };
        let completed = completed + if completing { committed } else { 0 };
        let detail = format!(
            "已保存 {written} 条，已完成 {completed} 项，其余尚未完成；可安全重试：{error}"
        );
        let _ = set_activity(
            storage,
            &MemoryActivity {
                outcome: "failed".into(),
                detail: detail.clone(),
                records_written: written,
                ..activity(storage)
            },
        );
        io::Error::other(detail)
    };
    let mut summary = String::new();
    let mut written = 0;
    for mut input in extraction.memories {
        input.id = None;
        input.expected_revision = None;
        input.pinned = false;
        input.observed_at = Some(observed_at);
        if input.evidence.trim().is_empty() {
            continue;
        }
        if let Some(record) = remember(storage, input, Some(source), true)
            .map_err(|e| partial_error(written, 0, false, e))?
        {
            summary.push_str(&format!("## {}\n\n{}\n\n", record.key, record.content));
            written += 1;
        }
    }
    let mut completed = 0;
    for completion in extraction.completed_keys {
        if completion.evidence.trim().is_empty() {
            continue;
        }
        for record in list(storage, true)
            .map_err(|e| partial_error(written, completed, true, e))?
            .into_iter()
            .filter(|r| {
                active(r)
                    && r.kind == "task"
                    && r.key == completion.key
                    && r.scope == completion.scope
                    && r.observed_at <= observed_at
            })
        {
            retire_with_policy(
                storage,
                &record.scope,
                &record.id,
                Some(record.revision),
                true,
                true,
            )
            .map_err(|e| partial_error(written, completed, true, e))?;
            completed += 1;
            summary.push_str(&format!("## 已完成\n\n{}\n\n", record.key));
        }
    }
    set_activity(
        storage,
        &MemoryActivity {
            attempted_at: Some(now()),
            succeeded_at: Some(now()),
            outcome: if written > 0 { "saved" } else { "unchanged" }.into(),
            detail: String::new(),
            records_written: written,
        },
    )?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(scope: &str, content: &str) -> RecordInput {
        RecordInput {
            scope: scope.into(),
            kind: "user".into(),
            key: "response style".into(),
            content: content.into(),
            ..Default::default()
        }
    }
    #[test]
    fn live_settings_parse_complete_toml_documents_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(setting_from_path(&path, "memory", None, "enabled", true));
        assert!(!setting_from_path(&path, "memory", None, "enabled", false));
        std::fs::write(
            &path,
            "[memory]\nenabled = true\n[memory.flush]\nenabled = false\n",
        )
        .unwrap();
        assert!(setting_from_path(&path, "memory", None, "enabled", false));
        assert!(!setting_from_path(
            &path,
            "memory",
            Some("flush"),
            "enabled",
            true
        ));
        assert!(setting_from_path(
            &path,
            "memory",
            Some("dream"),
            "enabled",
            true
        ));
        std::fs::write(&path, "[memory]\nenabled = false\n").unwrap();
        assert!(!setting_from_path(&path, "memory", None, "enabled", true));
        std::fs::write(&path, "[invalid").unwrap();
        assert!(!setting_from_path(&path, "memory", None, "enabled", true));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(!setting_from_path(&path, "memory", None, "enabled", true));
    }

    #[test]
    fn extraction_prevalidates_the_entire_batch_and_manual_add_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let storage =
            MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("workspace"));
        let json = r#"{"memories":[{"scope":"global","kind":"user","key":"first","content":"first fact","evidence":"first fact"},{"scope":"global","kind":"invalid","key":"second","content":"bad","evidence":"bad"}]}"#;
        assert!(apply_extraction(&storage, json, "source").is_err());
        assert!(list(&storage, true).unwrap().is_empty());
        let original = remember(&storage, input("global", "original"), None, false)
            .unwrap()
            .unwrap();
        assert!(remember(&storage, input("global", "different"), None, false).is_err());
        assert_eq!(
            remember(&storage, input("global", "original"), None, false)
                .unwrap()
                .unwrap()
                .revision,
            original.revision
        );
        assert_eq!(list(&storage, true).unwrap()[0].content, "original");
    }

    #[test]
    fn projection_failure_counts_the_committed_fact_and_retry_repairs_the_projection() {
        let dir = tempfile::tempdir().unwrap();
        let storage =
            MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("workspace"));
        let projection = storage.global_dir().join(RECORD_DOCUMENT);
        std::fs::create_dir_all(&projection).unwrap();
        let json = r#"{"memories":[{"scope":"global","kind":"user","key":"first","content":"first fact","evidence":"first fact"}]}"#;
        assert!(
            apply_extraction_at(&storage, json, "source", 123)
                .unwrap_err()
                .to_string()
                .contains("已保存 1 条")
        );
        assert_eq!(activity(&storage).records_written, 1);
        std::fs::remove_dir(&projection).unwrap();
        apply_extraction_at(&storage, json, "source", 123).unwrap();
        assert_eq!(list(&storage, true).unwrap().len(), 1);
        assert!(
            std::fs::read_to_string(projection)
                .unwrap()
                .contains("first fact")
        );
    }

    #[test]
    fn manual_retry_repairs_projection_without_revising_truth() {
        let dir = tempfile::tempdir().unwrap();
        let storage =
            MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("workspace"));
        let projection = storage.global_dir().join(RECORD_DOCUMENT);
        std::fs::create_dir_all(&projection).unwrap();
        assert!(remember(&storage, input("global", "manual fact"), None, false).is_err());
        let committed = list(&storage, true).unwrap()[0].clone();
        assert!(remember(&storage, input("global", "manual fact"), None, false).is_err());
        std::fs::remove_dir(&projection).unwrap();
        let retried = remember(&storage, input("global", "manual fact"), None, false)
            .unwrap()
            .unwrap();
        assert_eq!(retried.revision, committed.revision);
        assert!(
            std::fs::read_to_string(projection)
                .unwrap()
                .contains("manual fact")
        );
    }

    #[cfg(unix)]
    #[test]
    fn partial_completion_reports_committed_progress() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let storage =
            MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("workspace"));
        for scope in ["global", "workspace"] {
            remember(
                &storage,
                RecordInput {
                    scope: scope.into(),
                    kind: "task".into(),
                    key: scope.into(),
                    content: format!("task in {scope}"),
                    ..Default::default()
                },
                None,
                false,
            )
            .unwrap();
        }
        std::fs::set_permissions(
            storage.workspace_dir(),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        let result = apply_extraction(
            &storage,
            r#"{"memories":[],"completedKeys":[{"scope":"global","key":"global","evidence":"done global"},{"scope":"workspace","key":"workspace","evidence":"done workspace"}]}"#,
            "source",
        );
        std::fs::set_permissions(
            storage.workspace_dir(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("已完成 1 项"));
        assert_eq!(
            load(&storage, "global").unwrap().records[0].status,
            "completed"
        );
        assert_eq!(
            load(&storage, "workspace").unwrap().records[0].status,
            "active"
        );
    }

    #[test]
    fn activity_ownership_recovers_cancellation_and_stale_process_state() {
        let dir = tempfile::tempdir().unwrap();
        let storage =
            MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("workspace"));
        let owner = begin_extraction_activity(&storage).unwrap().unwrap();
        assert_eq!(activity(&storage).outcome, "running");
        assert!(begin_extraction_activity(&storage).unwrap().is_none());
        recover_extraction_activity(&storage);
        assert_eq!(activity(&storage).outcome, "running");
        drop(owner);
        assert_eq!(activity(&storage).outcome, "failed");
        set_activity(
            &storage,
            &MemoryActivity {
                outcome: "running".into(),
                ..Default::default()
            },
        )
        .unwrap();
        recover_extraction_activity(&storage);
        assert_eq!(activity(&storage).outcome, "failed");
    }

    #[cfg(unix)]
    #[test]
    fn extraction_disk_failure_reports_partial_progress_and_retry_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let storage =
            MemoryStorage::with_paths(dir.path().join("global"), dir.path().join("workspace"));
        std::fs::create_dir_all(storage.workspace_dir()).unwrap();
        std::fs::set_permissions(
            storage.workspace_dir(),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        let json = r#"{"memories":[{"scope":"global","kind":"user","key":"first","content":"first fact","evidence":"first fact"},{"scope":"workspace","kind":"project","key":"second","content":"second fact","evidence":"second fact"}]}"#;
        let failed = apply_extraction_at(&storage, json, "source", 123);
        std::fs::set_permissions(
            storage.workspace_dir(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(failed.unwrap_err().to_string().contains("已保存 1 条"));
        assert_eq!(list(&storage, true).unwrap().len(), 1);
        apply_extraction_at(&storage, json, "source", 123).unwrap();
        assert_eq!(list(&storage, true).unwrap().len(), 2);
    }

    #[test]
    fn correction_forgetting_and_source_deletion_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let storage =
            MemoryStorage::with_paths(temp.path().join("global"), temp.path().join("workspace"));
        let first = remember(&storage, input("global", "回答先给结论"), Some("s1"), true)
            .unwrap()
            .unwrap();
        let mut corrected = input("global", "回答要给详细过程");
        corrected.id = Some(first.id.clone());
        corrected.expected_revision = Some(first.revision);
        let second = remember(&storage, corrected, None, false).unwrap().unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(
            remember(&storage, input("global", "回答先给结论"), Some("s2"), true).unwrap(),
            None
        );
        retire(&storage, "global", &first.id, Some(2), false).unwrap();
        assert!(core_context(&storage).unwrap().is_empty());
        assert!(source_blocked(&storage, "s1").unwrap());
        assert!(!permitted(&storage, "回答要给详细过程").unwrap());
    }
    #[test]
    fn workspace_isolation_and_expiration() {
        let temp = tempfile::tempdir().unwrap();
        let a = MemoryStorage::with_paths(temp.path().join("global"), temp.path().join("a"));
        let b = MemoryStorage::with_paths(temp.path().join("global"), temp.path().join("b"));
        remember(&a, input("workspace", "只属于项目 A"), Some("s1"), true).unwrap();
        assert_eq!(list(&a, true).unwrap().len(), 1);
        assert!(list(&b, true).unwrap().is_empty());
        let mut task = input("workspace", "明天提交初稿");
        task.kind = "task".into();
        task.key = "初稿".into();
        task.valid_until = Some(now() - 1);
        remember(&a, task, Some("s1"), true).unwrap();
        assert!(!core_context(&a).unwrap().contains("明天"));
        assert_eq!(forget_source(&a, "s1").unwrap(), 1);
        assert!(core_context(&a).unwrap().is_empty());
    }
}
