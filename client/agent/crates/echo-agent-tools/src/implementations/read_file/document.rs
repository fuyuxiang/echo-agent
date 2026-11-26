//! Indexed, bounded document reads. The source stays outside model history;
//! stable records, coverage and checkpoints survive context compaction/restart.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::types::output::{FileContent, ReadFileOutput};
use crate::types::resources::{SessionFolder, SharedResources};

const RECORD_BYTES: usize = 2048;
pub const DEFAULT_PAGE_BYTES: usize = 16 * 1024;
#[derive(Clone)]
pub struct DocumentReadBudget(pub usize);
const MAX_RECORDS: usize = 500_000;
const MAX_DOCUMENTS: usize = 128;

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentMode {
    #[default]
    Read,
    Overview,
    Search,
    Checkpoint,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentOptions {
    #[serde(default)]
    pub mode: DocumentMode,
    /// Exact worksheet/section name from the overview. Omit for the whole file.
    #[serde(default)]
    pub section: Option<String>,
    /// Literal case-insensitive search; search results are also paginated.
    #[serde(default)]
    pub query: Option<String>,
    /// Require complete reading of this scope before delivering a final answer.
    /// Set false for reference-only material or when using a verified local computation.
    #[serde(default)]
    pub exhaustive: Option<bool>,
    /// Persist an analysis result with source references before moving to another batch.
    #[serde(default)]
    pub checkpoint: Option<String>,
    /// Replace the working checkpoint list with a consolidated result. Existing
    /// checkpoint files remain archived and retrievable in the session folder.
    #[serde(default)]
    pub consolidate_checkpoints: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRecord {
    pub section: String,
    pub locator: String,
    /// Identity of the original line/cell, shared by all of its bounded pieces.
    pub source_locator: String,
    pub text: String,
    #[serde(default)]
    pub requires_visual: bool,
}

/// Split a long cell/line without losing a byte. Each piece has its own stable
/// source locator, including byte range; UTF-8 boundaries are respected.
pub fn push_record(
    records: &mut Vec<DocumentRecord>,
    section: &str,
    locator: &str,
    text: &str,
) -> Result<(), String> {
    let mut start = 0;
    loop {
        let mut end = (start + RECORD_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if records.len() >= MAX_RECORDS {
            return Err("Document contains too many indexed records".into());
        }
        records.push(DocumentRecord {
            section: section.to_owned(),
            source_locator: locator.to_owned(),
            locator: if text.len() > RECORD_BYTES {
                format!("{locator} bytes {}-{end}", start + 1)
            } else {
                locator.to_owned()
            },
            text: text[start..end].to_owned(),
            requires_visual: false,
        });
        if end == text.len() {
            break;
        }
        start = end;
    }
    Ok(())
}

pub fn text_records(text: &str) -> Result<Vec<DocumentRecord>, String> {
    let mut records = Vec::new();
    let mut section = "Document".to_string();
    for (i, line) in text.split('\n').enumerate() {
        if let Some(label) = line
            .strip_prefix("--- ")
            .and_then(|s| s.strip_suffix(" ---"))
        {
            section = label.to_owned();
        }
        push_record(&mut records, &section, &format!("line {}", i + 1), line)?;
    }
    Ok(records)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentCoverage {
    pub revision: String,
    pub total: usize,
    pub read: Vec<[usize; 2]>,
    pub required: Vec<[usize; 2]>,
    pub checkpoints: Vec<String>,
    #[serde(default)]
    pub exhaustive_default: Option<bool>,
    #[serde(default)]
    pub section_policies: BTreeMap<String, bool>,
    #[serde(default)]
    pub update_sequence: u64,
}

impl DocumentCoverage {
    pub fn covered(&self) -> usize {
        self.read.iter().map(|r| r[1] - r[0]).sum()
    }
    fn contains(&self, index: usize) -> bool {
        let pos = self.read.partition_point(|r| r[1] <= index);
        self.read.get(pos).is_some_and(|r| r[0] <= index)
    }

    fn refresh_required(&mut self, records: &[DocumentRecord], exhaustive: bool) {
        self.required.clear();
        for (i, record) in records.iter().enumerate() {
            if self
                .section_policies
                .get(&record.section)
                .copied()
                .or(self.exhaustive_default)
                .unwrap_or(exhaustive)
            {
                insert_range(&mut self.required, i, i + 1);
            }
        }
    }
    pub fn next_missing(&self) -> Option<usize> {
        for required in &self.required {
            let mut next = required[0];
            for read in &self.read {
                if read[0] > next {
                    break;
                }
                if read[1] > next {
                    next = read[1];
                }
            }
            if next < required[1] {
                return Some(next);
            }
        }
        None
    }
}

fn insert_range(ranges: &mut Vec<[usize; 2]>, start: usize, end: usize) {
    if start >= end {
        return;
    }
    ranges.push([start, end]);
    ranges.sort_unstable();
    let mut merged: Vec<[usize; 2]> = Vec::new();
    for range in ranges.drain(..) {
        if let Some(last) = merged.last_mut()
            && range[0] <= last[1]
        {
            last[1] = last[1].max(range[1]);
        } else {
            merged.push(range);
        }
    }
    *ranges = merged;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentTask {
    pub objective: String,
    pub exhaustive: bool,
    pub documents: BTreeMap<String, DocumentCoverage>,
    #[serde(default)]
    pub expected_sources: Vec<String>,
    /// Set only by a successful runtime turn, never inferred from user wording.
    #[serde(default)]
    pub completed: bool,
    #[serde(skip)]
    pub recovery_error: Option<String>,
}

impl DocumentTask {
    pub fn pending_feedback(&self) -> Option<String> {
        let mut pending = Vec::new();
        if self.exhaustive {
            for source in &self.expected_sources {
                if !self.documents.contains_key(source) {
                    pending.push(format!(
                        "Attached source has not been inspected: read_file target_file={}",
                        serde_json::to_string(source).unwrap_or_default()
                    ));
                }
            }
        }
        for (path, coverage) in &self.documents {
            if let Some(next) = coverage.next_missing() {
                pending.push(format!("{}: read_file target_file={}, document.mode=read, offset={} (indexed records), limit=100; read {}/{} records", Path::new(path).file_name().unwrap_or_default().to_string_lossy(), serde_json::to_string(path).unwrap_or_default(), next + 1, coverage.covered(), coverage.total));
            }
        }
        if pending.is_empty() {
            return None;
        }
        Some(format!(
            "The requested document scope is not fully read yet. Continue the existing task, save source-backed intermediate findings using document.mode=checkpoint, and read missing records before final delivery. Do not repeat completed work. If local computation already processed the full scope, record its verification and set document.exhaustive=false for that scope.\n{}",
            pending.into_iter().take(8).collect::<Vec<_>>().join("\n")
        ))
    }
}

#[derive(Clone)]
pub struct DocumentSession(pub Arc<Mutex<DocumentTask>>);

#[derive(Clone, Default)]
struct DocumentCache(Arc<Mutex<BTreeMap<String, Arc<Vec<DocumentRecord>>>>>);

pub async fn extract_cached(
    bytes: Vec<u8>,
    resources: &SharedResources,
    extractor: fn(&[u8]) -> Result<Vec<DocumentRecord>, String>,
) -> Result<(String, Arc<Vec<DocumentRecord>>), String> {
    if bytes.len() > 50 * 1024 * 1024 {
        return Err("Document exceeds the 50 MiB parsing limit".into());
    }
    let revision = revision(&bytes);
    let cache = {
        let mut res = resources.lock().await;
        if res.get::<DocumentCache>().is_none() {
            res.insert(DocumentCache::default());
        }
        res.get::<DocumentCache>().unwrap().clone()
    };
    // Serialize extraction per session; parallel model reads cannot multiply
    // parser memory. All later pages reuse the same immutable index.
    let mut entries = cache.0.lock().await;
    if let Some(records) = entries.get(&revision) {
        return Ok((revision, records.clone()));
    }
    let records = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::task::spawn_blocking(move || extractor(&bytes)),
    )
    .await
    .map_err(|_| "Document parsing timed out after 60 seconds".to_string())?
    .map_err(|e| format!("Document parser failed: {e}"))??;
    let records = Arc::new(records);
    let folder = resources
        .lock()
        .await
        .get::<SessionFolder>()
        .map(|f| f.0.join("documents/indices"));
    if let Some(folder) = folder {
        let data = records.clone();
        let name = revision.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            use std::io::Write;
            std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
            let path = folder.join(format!("{name}.txt"));
            if !path.exists() {
                let mut file =
                    tempfile::NamedTempFile::new_in(&folder).map_err(|e| e.to_string())?;
                for (i, record) in data.iter().enumerate() {
                    writeln!(file, "{}→[{}] {}", i + 1, record.locator, record.text)
                        .map_err(|e| e.to_string())?;
                }
                file.persist(path).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    // Cache is an optimization, never the only copy of user data.
    if entries.len() >= 2 {
        entries.clear();
    }
    entries.insert(revision.clone(), records.clone());
    Ok((revision, records))
}

async fn session(resources: &SharedResources) -> (DocumentSession, Option<PathBuf>) {
    let mut res = resources.lock().await;
    let folder = res.get::<SessionFolder>().map(|s| {
        let root = dunce::canonicalize(&s.0).unwrap_or_else(|_| s.0.clone());
        root.join("documents")
    });
    let state = res.get::<DocumentSession>().cloned().unwrap_or_else(|| {
        let restored = folder
            .as_ref()
            .map(|folder| {
                let path = folder.join("task.json");
                let loaded = (|| -> Result<DocumentTask, String> {
                    let meta = match std::fs::metadata(&path) {
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(DocumentTask::default());
                        }
                        other => other.map_err(|e| e.to_string())?,
                    };
                    if meta.len() > 4 * 1024 * 1024 {
                        return Err("Document task journal exceeds its restoration limit".into());
                    }
                    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())
                })();
                loaded.unwrap_or_else(|e| DocumentTask {
                    recovery_error: Some(e),
                    ..Default::default()
                })
            })
            .unwrap_or_default();
        let state = DocumentSession(Arc::new(Mutex::new(restored)));
        res.insert(state.clone());
        state
    });
    (state, folder)
}

fn persist(folder: Option<&Path>, task: &DocumentTask) -> Result<(), String> {
    use std::io::Write;
    let Some(folder) = folder else {
        return Ok(());
    };
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(folder).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(task).map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Analysis journal is full; consolidate checkpoints with document.consolidate_checkpoints=true".into());
    }
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    file.persist(folder.join("task.json"))
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn requires_complete_read(objective: &str) -> bool {
    let text = objective.to_lowercase();
    [
        "每个",
        "每位",
        "逐个",
        "逐项",
        "逐一",
        "全文",
        "完整分析",
        "全部分析",
        "所有选手",
        "each participant",
        "each candidate",
        "every row",
        "all rows",
        "entire document",
        "entire workbook",
        "all participants",
    ]
    .iter()
    .any(|word| text.contains(word))
}

pub async fn begin_task(resources: &SharedResources, objective: &str) -> Result<(), String> {
    let (session, folder) = session(resources).await;
    let mut state = session.0.lock().await;
    if let Some(error) = &state.recovery_error {
        return Err(format!(
            "无法恢复资料任务进度：{error}。原文件和记录仍保留。"
        ));
    }
    // The session owns durable evidence. A follow-up is never permission to
    // delete it. Completed scopes expire at the next user turn; unfinished
    // scopes survive ordinary steering, cancellation and process restarts.
    let mut task = state.clone();
    if task.completed {
        task.exhaustive = false;
        task.expected_sources.clear();
        for coverage in task.documents.values_mut() {
            coverage.required.clear();
            coverage.exhaustive_default = None;
            coverage.section_policies.clear();
        }
    }
    task.objective = objective.trim().chars().take(8000).collect();
    task.completed = false;
    task.exhaustive |= requires_complete_read(objective);
    persist(folder.as_deref(), &task)?;
    *state = task;
    Ok(())
}

pub async fn task_snapshot(resources: &SharedResources) -> DocumentTask {
    session(resources).await.0.0.lock().await.clone()
}

pub async fn finish_task(resources: &SharedResources) -> Result<(), String> {
    let (session, folder) = session(resources).await;
    let mut state = session.0.lock().await;
    if state.pending_feedback().is_some() {
        return Err("Document task still has unread required content".into());
    }
    let mut task = state.clone();
    task.completed = true;
    persist(folder.as_deref(), &task)?;
    *state = task;
    Ok(())
}

pub async fn expect_sources(
    resources: &SharedResources,
    sources: Vec<String>,
) -> Result<(), String> {
    if sources.len() > MAX_DOCUMENTS {
        return Err("One task supports at most 128 attached documents".into());
    }
    let (session, folder) = session(resources).await;
    let mut state = session.0.lock().await;
    let mut task = state.clone();
    for source in sources {
        let source = crate::util::fs::try_canonicalize(Path::new(&source))
            .await
            .unwrap_or_else(|_| PathBuf::from(source))
            .to_string_lossy()
            .to_string();
        if !task.expected_sources.contains(&source) {
            if task.expected_sources.len() >= MAX_DOCUMENTS {
                return Err("One task supports at most 128 attached documents".into());
            }
            task.expected_sources.push(source);
        }
    }
    persist(folder.as_deref(), &task)?;
    *state = task;
    Ok(())
}

pub async fn read_document(
    records: &[DocumentRecord],
    source: &Path,
    revision: &str,
    offset: Option<i64>,
    limit: Option<usize>,
    options: &DocumentOptions,
    resources: &SharedResources,
) -> Result<ReadFileOutput, String> {
    read_document_pages(
        records, source, revision, offset, limit, options, None, resources,
    )
    .await
}

/// PDF page selection narrows the returned records, not the task's required scope.
pub async fn read_document_pages(
    records: &[DocumentRecord],
    source: &Path,
    revision: &str,
    offset: Option<i64>,
    limit: Option<usize>,
    options: &DocumentOptions,
    pages: Option<&[usize]>,
    resources: &SharedResources,
) -> Result<ReadFileOutput, String> {
    let (session, folder) = session(resources).await;
    let page_bytes = resources
        .lock()
        .await
        .get::<DocumentReadBudget>()
        .map(|b| b.0)
        .unwrap_or(DEFAULT_PAGE_BYTES)
        .clamp(2048, DEFAULT_PAGE_BYTES);
    let mut state = session.0.lock().await;
    let mut task = state.clone();
    let key = source.to_string_lossy().to_string();
    if let Some(error) = &task.recovery_error {
        return Err(format!(
            "Cannot update an unreadable document journal: {error}"
        ));
    }
    // Journals and snapshots are working references, never new obligations in
    // their own journal. Otherwise reading task.json changes its own revision
    // on every page and can never finish after a restart.
    let internal = folder.as_ref().is_some_and(|p| {
        source.starts_with(p)
            || p.parent()
                .is_some_and(|root| source.starts_with(root.join("tool-results")))
    });
    if internal && options.checkpoint.is_some() {
        return Err("Save checkpoints against the original source, not the task journal".into());
    }
    if !internal && !task.documents.contains_key(&key) && task.documents.len() >= MAX_DOCUMENTS {
        return Err("Too many documents in one task; finish or start a new task".into());
    }
    if limit == Some(0) {
        return Err("Document limit must be greater than zero".into());
    }
    let task_exhaustive = task.exhaustive && !internal;
    let mut reference = DocumentCoverage::default();
    let coverage = if internal {
        &mut reference
    } else {
        task.documents.entry(key).or_default()
    };
    if coverage.revision != revision {
        *coverage = DocumentCoverage {
            revision: revision.into(),
            total: records.len(),
            update_sequence: coverage.update_sequence,
            ..Default::default()
        };
    }
    if let Some(policy) = options.exhaustive {
        if let Some(section) = &options.section {
            coverage.section_policies.insert(section.clone(), policy);
        } else {
            coverage.exhaustive_default = Some(policy);
            coverage.section_policies.clear();
        }
    }
    let mut indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| options.section.as_ref().is_none_or(|s| &r.section == s))
        .map(|(i, _)| i)
        .collect();
    if options.section.is_some() && indices.is_empty() {
        return Err(
            "Section not found. Read document.mode=overview to see exact section names".into(),
        );
    }
    coverage.refresh_required(records, task_exhaustive);
    if let Some(pages) = pages {
        indices.retain(|&i| {
            pages
                .iter()
                .any(|p| records[i].section == format!("Page {p}"))
        });
        if indices.is_empty() {
            return Err("Selected pages do not intersect document.section".into());
        }
    }
    if let Some(note) = &options.checkpoint {
        if note.len() > 12 * 1024 {
            return Err(
                "Checkpoint exceeds 12 KiB; save concise findings with source locators".into(),
            );
        }
        if options.consolidate_checkpoints {
            coverage.checkpoints.clear();
        }
        if coverage.checkpoints.len() >= 128 {
            return Err("Checkpoint capacity reached; consolidate previous findings".into());
        }
        if !coverage.checkpoints.contains(note) {
            if let Some(folder) = &folder {
                let dir = folder.join("checkpoints");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = dir.join(format!("{}.txt", self::revision(note.as_bytes())));
                if !path.exists() {
                    std::fs::write(path, note).map_err(|e| e.to_string())?;
                }
            }
            coverage.checkpoints.push(note.clone());
        }
    }
    let mut sections: BTreeMap<&str, usize> = BTreeMap::new();
    for record in records {
        *sections.entry(&record.section).or_default() += 1;
    }
    let mut content = format!(
        "Document: {}\nRevision: {}\nIndexed records: {}. Each record includes its source locator; offsets address these records, not original file lines.\n",
        source.display(),
        revision,
        records.len()
    );
    if matches!(options.mode, DocumentMode::Overview) || offset.is_none() {
        if let Some(folder) = &folder
            && !internal
        {
            content.push_str(&format!("Source snapshot: {}. Analysis journal: {}. Both survive context compaction and restart.\n", folder.join("indices").join(format!("{revision}.txt")).display(), folder.join("task.json").display()));
        }
        content.push_str("Sections (exact names / indexed records):\n");
        for (name, count) in sections.iter().take(100) {
            if content.len() > page_bytes / 2 {
                content.push_str("Remaining section names are available through indexed reads.\n");
                break;
            }
            use std::fmt::Write;
            let _ = writeln!(
                content,
                "{}: {count}",
                serde_json::to_string(name).unwrap_or_default()
            );
        }
        if sections.len() > 100 {
            content.push_str(
                "Additional sections omitted from overview; indexed reads still include them.\n",
            );
        }
    }
    let query = options.query.as_deref().unwrap_or("").to_lowercase();
    if matches!(options.mode, DocumentMode::Search) && query.trim().is_empty() {
        return Err("Search requires a non-empty document.query".into());
    }
    if query.len() > 4096 {
        return Err("Search query exceeds 4096 bytes; use a shorter literal".into());
    }
    let candidates = if matches!(options.mode, DocumentMode::Search) {
        search_records(records, &indices, &query)
    } else {
        indices.clone()
    };
    let start = match offset.unwrap_or(1) {
        n if n < 0 => candidates.len().saturating_sub(n.unsigned_abs() as usize),
        n => (n as usize).saturating_sub(1),
    };
    let max = limit.unwrap_or(100).min(1000);
    let mut taken = 0;
    let mut raw = String::new();
    if !matches!(
        options.mode,
        DocumentMode::Overview | DocumentMode::Checkpoint
    ) {
        for &i in candidates.iter().skip(start).take(max) {
            let record = &records[i];
            let line = format!("{}→[{}] {}\n", i + 1, record.locator, record.text);
            if taken > 0 && content.len() + line.len() > page_bytes {
                break;
            }
            content.push_str(&line);
            raw.push_str(&record.text);
            raw.push('\n');
            if !record.requires_visual {
                insert_range(&mut coverage.read, i, i + 1);
            }
            taken += 1;
        }
    }
    let next_offset = (start + taken < candidates.len()).then_some(start + taken + 1);
    coverage.update_sequence = coverage.update_sequence.saturating_add(1);
    let mut progress = document_progress(source, coverage, records, folder.as_deref())?;
    progress["nextOffset"] = serde_json::json!(next_offset);
    progress["scope"] = serde_json::json!(options.section.clone().or_else(|| pages.map(|pages| {
        format!(
            "第 {} 页",
            pages
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join("、")
        )
    })));
    progress["scopeRecords"] = serde_json::json!(indices.len());
    progress["returnedRecords"] = serde_json::json!(taken);
    progress["matchedRecords"] = serde_json::json!(candidates.len());
    content.push_str(&format!(
        "\nRead coverage: {}/{} records. Checkpoints: {}.\n",
        coverage.covered(),
        coverage.total,
        coverage.checkpoints.len()
    ));
    if let Some(next) = next_offset {
        let mode = if matches!(options.mode, DocumentMode::Search) {
            "search"
        } else {
            "read"
        };
        content.push_str(&format!("Continue automatically with document.mode={mode}, offset={next}, preserving pages and document.section/query if used. More source content remains.\n"));
    }
    if matches!(options.mode, DocumentMode::Checkpoint) {
        for note in &coverage.checkpoints {
            content.push_str(note);
            content.push('\n');
        }
        if content.len() > DEFAULT_PAGE_BYTES {
            // Checkpoints are individually retained; return their directory instead of overflowing.
            content = format!(
                "Saved {} checkpoints. Findings are persisted in the task journal at {}. Use read_file to read it in ranges.\n",
                coverage.checkpoints.len(),
                folder
                    .as_ref()
                    .map(|p| p.join("task.json").display().to_string())
                    .unwrap_or_default()
            );
        }
    }
    if !internal {
        persist(folder.as_deref(), &task)?;
        *state = task;
    }
    Ok(ReadFileOutput::FileContent(FileContent {
        content,
        content_concise: None,
        absolute_path: source.to_path_buf(),
        offset: Some(start + 1),
        limit: Some(taken),
        raw_output: raw,
        total_lines: records.len(),
        extracted_images: Vec::new(),
        document_progress: (!internal).then_some(progress),
    }))
}

/// Search across bounded pieces of the same source cell/line, returning every
/// piece touched by a match. Never join unrelated rows, cells or sections.
fn search_records(records: &[DocumentRecord], indices: &[usize], query: &str) -> Vec<usize> {
    let mut matched = vec![false; records.len()];
    for (position, &i) in indices.iter().enumerate() {
        let record = &records[i];
        if record.locator.to_lowercase().contains(query) {
            matched[i] = true;
        }
        let mut window = record.text.to_lowercase();
        let own_len = window.len();
        let mut ends = vec![(i, own_len)];
        for &j in indices.iter().skip(position + 1) {
            if window.len().saturating_sub(own_len) >= query.len()
                || records[j].source_locator != record.source_locator
                || records[j].section != record.section
                || j != ends.last().unwrap().0 + 1
            {
                break;
            }
            window.push_str(&records[j].text.to_lowercase());
            ends.push((j, window.len()));
        }
        let mut from = 0;
        while from < own_len {
            let Some(found) = window[from..].find(query) else {
                break;
            };
            let start = from + found;
            if start >= own_len {
                break;
            }
            let end = start + query.len();
            let mut previous_end = 0;
            for &(j, record_end) in &ends {
                if previous_end < end && record_end > start {
                    matched[j] = true;
                }
                previous_end = record_end;
            }
            from = start + window[start..].chars().next().unwrap().len_utf8();
        }
    }
    indices.iter().copied().filter(|&i| matched[i]).collect()
}

fn document_progress(
    source: &Path,
    coverage: &DocumentCoverage,
    records: &[DocumentRecord],
    folder: Option<&Path>,
) -> Result<serde_json::Value, String> {
    let mut progress = serde_json::json!({
        "source": source.file_name().unwrap_or_default().to_string_lossy(),
        "sourcePath": source.to_string_lossy(), "revision": coverage.revision,
        "readRecords": coverage.covered(), "totalRecords": coverage.total,
        "complete": coverage.covered() == coverage.total,
        "checkpointCount": coverage.checkpoints.len(), "updateSequence": coverage.update_sequence,
    });
    // PDF progress is presented in pages, including a page only after all of
    // its records have been read (or its image has been returned).
    let mut pages: BTreeMap<&str, bool> = BTreeMap::new();
    if source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        for (i, record) in records.iter().enumerate() {
            *pages.entry(&record.section).or_insert(true) &= coverage.contains(i);
        }
        progress["totalPages"] = serde_json::json!(pages.len());
        progress["readPages"] = serde_json::json!(pages.values().filter(|&&read| read).count());
    }
    if let Some(folder) = folder.filter(|_| !coverage.checkpoints.is_empty()) {
        use std::io::Write;
        let dir = folder.join("findings");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let key = revision(
            format!(
                "{}\n{}\n{}",
                source.display(),
                coverage.revision,
                coverage.checkpoints.join("\n\n")
            )
            .as_bytes(),
        );
        let path = dir.join(format!("{key}.md"));
        if !path.exists() {
            let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
            writeln!(file, "# 阶段记录\n\n来源：{}\n", source.display())
                .map_err(|e| e.to_string())?;
            for (i, note) in coverage.checkpoints.iter().enumerate() {
                writeln!(file, "## 记录 {}\n\n{note}\n", i + 1).map_err(|e| e.to_string())?;
            }
            file.persist(&path).map_err(|e| e.to_string())?;
        }
        progress["checkpointPath"] = serde_json::json!(path.to_string_lossy());
    }
    Ok(progress)
}

pub fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub async fn mark_visual_pages(
    resources: &SharedResources,
    source: &Path,
    revision: &str,
    records: &[DocumentRecord],
    pages: &[usize],
) -> Result<serde_json::Value, String> {
    let (session, folder) = session(resources).await;
    let mut state = session.0.lock().await;
    let mut task = state.clone();
    let exhaustive = task.exhaustive;
    if let Some(error) = &task.recovery_error {
        return Err(format!(
            "Cannot update an unreadable document journal: {error}"
        ));
    }
    if !task
        .documents
        .contains_key(&source.to_string_lossy().to_string())
        && task.documents.len() >= MAX_DOCUMENTS
    {
        return Err("Too many documents in one task; start a new session".into());
    }
    let coverage = task
        .documents
        .entry(source.to_string_lossy().to_string())
        .or_default();
    if coverage.revision != revision {
        *coverage = DocumentCoverage {
            revision: revision.into(),
            total: records.len(),
            update_sequence: coverage.update_sequence,
            ..Default::default()
        };
    }
    coverage.refresh_required(records, exhaustive);
    for (i, record) in records.iter().enumerate() {
        if pages.iter().any(|n| record.section == format!("Page {n}")) {
            insert_range(&mut coverage.read, i, i + 1);
        }
    }
    coverage.update_sequence = coverage.update_sequence.saturating_add(1);
    let mut progress = document_progress(source, coverage, records, folder.as_deref())?;
    progress["scope"] = serde_json::json!(format!(
        "第 {} 页",
        pages
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join("、")
    ));
    persist(folder.as_deref(), &task)?;
    *state = task;
    Ok(progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::resources::Resources;
    #[test]
    fn coverage_merges_overlap_and_preserves_gaps() {
        let mut c = DocumentCoverage {
            required: vec![[0, 10]],
            ..Default::default()
        };
        insert_range(&mut c.read, 4, 8);
        insert_range(&mut c.read, 0, 3);
        insert_range(&mut c.read, 2, 6);
        assert_eq!(c.covered(), 8);
        assert_eq!(c.next_missing(), Some(8));
        insert_range(&mut c.read, 8, 10);
        assert_eq!(c.next_missing(), None);
    }
    #[test]
    fn long_unicode_cell_is_lossless_and_locatable() {
        let text = "中文代码🙂".repeat(8000);
        let mut records = Vec::new();
        push_record(&mut records, "Sheet1", "Sheet1!B8", &text).unwrap();
        assert!(records.len() > 10);
        assert_eq!(
            records.iter().map(|r| r.text.as_str()).collect::<String>(),
            text
        );
        assert!(
            records
                .iter()
                .all(|r| r.locator.starts_with("Sheet1!B8 bytes") && r.text.len() <= RECORD_BYTES)
        );
    }
    #[tokio::test]
    async fn pages_cover_every_record_and_restore_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let mut res = Resources::new();
        res.insert(SessionFolder(temp.path().to_path_buf()));
        let resources = res.into_shared();
        begin_task(&resources, "逐项分析全部内容").await.unwrap();
        let text = (0..1200)
            .map(|i| format!("row {i} {}", "代码".repeat(60)))
            .collect::<Vec<_>>()
            .join("\n");
        let records = text_records(&text).unwrap();
        let mut offset = None;
        let mut count = 0;
        loop {
            let ReadFileOutput::FileContent(fc) = read_document(
                &records,
                Path::new("/data/book.xlsx"),
                "v1",
                offset,
                None,
                &DocumentOptions::default(),
                &resources,
            )
            .await
            .unwrap() else {
                panic!()
            };
            assert!(fc.content.len() < DEFAULT_PAGE_BYTES + 4096);
            let p = fc.document_progress.unwrap();
            count += p["returnedRecords"].as_u64().unwrap();
            offset = p["nextOffset"].as_i64();
            if offset.is_none() {
                break;
            }
        }
        assert_eq!(count, 1200);
        assert!(task_snapshot(&resources).await.pending_feedback().is_none());
        let mut res = Resources::new();
        res.insert(SessionFolder(temp.path().to_path_buf()));
        let restored = task_snapshot(&res.into_shared()).await;
        assert_eq!(restored.documents["/data/book.xlsx"].covered(), 1200);
    }
    #[tokio::test]
    async fn overview_does_not_claim_content_read_and_revision_invalidates_coverage() {
        let resources = Resources::new().into_shared();
        begin_task(&resources, "每位选手").await.unwrap();
        let records = text_records("one\ntwo\nthree").unwrap();
        let overview = DocumentOptions {
            mode: DocumentMode::Overview,
            ..Default::default()
        };
        read_document(
            &records,
            Path::new("a.docx"),
            "v1",
            None,
            None,
            &overview,
            &resources,
        )
        .await
        .unwrap();
        assert!(task_snapshot(&resources).await.pending_feedback().is_some());
        read_document(
            &records,
            Path::new("a.docx"),
            "v1",
            None,
            None,
            &DocumentOptions::default(),
            &resources,
        )
        .await
        .unwrap();
        assert!(task_snapshot(&resources).await.pending_feedback().is_none());
        read_document(
            &records,
            Path::new("a.docx"),
            "v2",
            None,
            None,
            &overview,
            &resources,
        )
        .await
        .unwrap();
        assert_eq!(
            task_snapshot(&resources).await.documents["a.docx"].covered(),
            0
        );
    }
}
