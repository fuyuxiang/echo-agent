//! Parallel pipelined index builder with thread-local caching.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ahash::AHashMap as HashMap;
use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::languages::LanguageRegistry;
use crate::scope_graph::ScopeGraphIndex;
use crate::types::{FileMeta, SymbolAlias, SymbolOccurrence};
use echo_agent_paths::to_relative_path;

/// Error type for index building operations.
#[derive(Debug)]
pub enum IndexError {
    /// Error walking directory.
    WalkError { message: String },
    /// Thread panicked.
    ThreadPanic { message: String },
    /// IO error.
    IoError(std::io::Error),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::WalkError { message } => write!(f, "Walk error: {}", message),
            IndexError::ThreadPanic { message } => write!(f, "Thread panic: {}", message),
            IndexError::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<std::io::Error> for IndexError {
    fn from(e: std::io::Error) -> Self {
        IndexError::IoError(e)
    }
}

/// Result type for index building operations.
pub type Result<T> = std::result::Result<T, IndexError>;

// Thread-local caches for parsers and queries to avoid repeated initialization
thread_local! {
    static PARSER_CACHE: RefCell<HashMap<String, tree_sitter::Parser>> = RefCell::new(HashMap::new());
    static QUERY_CACHE: RefCell<HashMap<String, tree_sitter::Query>> = RefCell::new(HashMap::new());
}

/// Extracted symbols for a single file (lightweight, no ScopeGraph overhead)
struct FileSymbols {
    path: Arc<str>,
    definitions: Vec<SymbolOccurrence>,
    references: Vec<SymbolOccurrence>,
    aliases: Vec<SymbolAlias>,
    file_meta: FileMeta,
}

/// Builder for creating a symbol index.
///
/// Uses optimized parallel processing with:
/// - Thread-local parser and query caching
/// - Chunked parallel processing for cache locality
/// - Lightweight symbol extraction (no intermediate ScopeGraph)
/// - Bounded merge-batching to cap two-phase peak memory
pub struct IndexBuilder {
    registry: LanguageRegistry,
    num_threads: usize,
    /// Whether to respect .gitignore files (default: true)
    respect_gitignore: bool,
    /// Whether to skip hidden files/directories (default: true)
    skip_hidden: bool,
    /// Chunk size for parallel processing / thread-local cache locality (default: 100)
    chunk_size: usize,
    /// Maximum number of files whose symbols are held in memory at once during
    /// the merge phase.  Limiting this bounds the two-phase peak: parallel
    /// parsing produces at most `build_batch_size` FileSymbols before they are
    /// merged into the index and dropped.  Default: 5 000 files per batch.
    build_batch_size: usize,
}

/// Get the default number of threads (N-1 cores, minimum 1).
fn default_num_threads() -> usize {
    num_cpus::get().saturating_sub(1).max(1)
}

impl IndexBuilder {
    /// Create a new index builder.
    pub fn new() -> Self {
        Self {
            registry: LanguageRegistry::new(),
            num_threads: default_num_threads(),
            respect_gitignore: true,
            skip_hidden: true,
            chunk_size: 100,
            build_batch_size: 5_000,
        }
    }

    /// Create a new index builder with a custom language registry.
    pub fn with_registry(registry: LanguageRegistry) -> Self {
        Self {
            registry,
            num_threads: default_num_threads(),
            respect_gitignore: true,
            skip_hidden: true,
            chunk_size: 100,
            build_batch_size: 5_000,
        }
    }

    /// Set the number of threads to use (default: N-1 cores).
    #[must_use]
    pub fn with_threads(mut self, count: usize) -> Self {
        self.num_threads = count;
        self
    }

    /// Set the chunk size for parallel processing (default: 100).
    #[must_use]
    pub fn with_chunk_size(mut self, size: usize) -> Self {
        self.chunk_size = size;
        self
    }

    /// Set the merge-batch size (default: 5 000 files per batch).
    ///
    /// Controls how many files' symbols are held in memory simultaneously
    /// during the sequential merge phase.  Smaller values reduce peak RSS at
    /// the cost of slightly more pool scheduling overhead.  Values below
    /// `chunk_size` are clamped to `chunk_size` at build time, so the call
    /// order of `with_build_batch_size` and `with_chunk_size` does not matter.
    #[must_use]
    pub fn with_build_batch_size(mut self, size: usize) -> Self {
        self.build_batch_size = size;
        self
    }

    /// Set whether to respect .gitignore files (default: true).
    #[must_use]
    pub fn respect_gitignore(mut self, respect: bool) -> Self {
        self.respect_gitignore = respect;
        self
    }

    /// Set whether to skip hidden files/directories (default: true).
    #[must_use]
    pub fn skip_hidden(mut self, skip: bool) -> Self {
        self.skip_hidden = skip;
        self
    }

    /// Build index from a directory, respecting .gitignore.
    ///
    /// This walks the directory tree, automatically respecting:
    /// - `.gitignore` files at any level
    /// - `.git/info/exclude`
    /// - Global gitignore (`~/.config/git/ignore`)
    /// - Hidden files/directories (configurable)
    ///
    /// **Note**: File paths in the index are stored as **relative paths** (to `root_path`)
    /// for portability across machines/sessions.
    pub fn build(&self, root_path: &Path) -> Result<ScopeGraphIndex> {
        // Collect files using the ignore crate
        let file_paths = self.collect_files(root_path)?;

        if file_paths.is_empty() {
            let mut index = ScopeGraphIndex::new();
            // Set query version even for empty index so cache validation works
            index.set_query_version(self.registry.compute_query_hash());
            return Ok(index);
        }

        self.build_fast(root_path, &file_paths)
    }

    /// Collect all supported files from a directory, respecting gitignore.
    /// Uses `git ls-files` when available (faster), falls back to directory walking.
    fn collect_files(&self, root_path: &Path) -> Result<Vec<PathBuf>> {
        // Try git ls-files first - it's much faster as it reads from git's index
        // But it only works for tracked files, so we also add untracked files
        if let Some(files) = self.collect_files_git(root_path)
            && !files.is_empty()
        {
            return Ok(files);
        }

        // Fall back to directory walking
        self.collect_files_walk(root_path)
    }

    /// Collect files using git2 - reads from the git index (tracked files).
    /// Untracked files are not included since:
    /// 1. They are typically a small minority
    /// 2. They will be picked up by fsnotify when created
    /// 3. The statuses() call for untracked files is very slow (~10x overhead)
    fn collect_files_git(&self, root_path: &Path) -> Option<Vec<PathBuf>> {
        use git2::Repository;

        // Open the repository
        let repo = Repository::open(root_path).ok()?;

        // Get all files from the index (tracked files)
        let index = repo.index().ok()?;
        let files: Vec<PathBuf> = index
            .iter()
            .filter_map(|entry| {
                // git2 stores paths as bytes, convert to str
                let path_str = std::str::from_utf8(&entry.path).ok()?;
                if self.registry.is_supported(path_str) {
                    Some(root_path.join(path_str))
                } else {
                    None
                }
            })
            .collect();

        Some(files)
    }

    /// Collect files by walking the directory tree.
    /// Used as fallback when not in a git repository.
    fn collect_files_walk(&self, root_path: &Path) -> Result<Vec<PathBuf>> {
        use std::sync::Mutex;

        let files = Mutex::new(Vec::with_capacity(50000));

        let walker = WalkBuilder::new(root_path)
            .hidden(self.skip_hidden)
            .git_ignore(self.respect_gitignore)
            .git_global(self.respect_gitignore)
            .git_exclude(self.respect_gitignore)
            .threads(self.num_threads.min(12)) // Use parallel walking (capped at 12)
            .build_parallel();

        walker.run(|| {
            let files = &files;
            let registry = &self.registry;
            Box::new(move |entry| {
                use ignore::WalkState;

                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => return WalkState::Continue,
                };

                let path = entry.path();

                // Skip directories
                if path.is_dir() {
                    return WalkState::Continue;
                }

                // Check if the file is supported
                if registry.is_supported(path) {
                    files.lock().unwrap().push(path.to_path_buf());
                }

                WalkState::Continue
            })
        });

        Ok(files.into_inner().unwrap())
    }

    /// Build index with maximum throughput optimizations:
    /// - Memory-mapped I/O for zero-copy file reading
    /// - Direct parsing from mmap (no intermediate buffer copy)
    /// - Lightweight symbol extraction (skip building full ScopeGraph)
    /// - Thread-local parser and query caching
    /// - Chunked parallel processing for better cache locality
    /// - Single StringInterner for memory-efficient string deduplication
    ///
    /// Uses two-phase approach:
    /// 1. Parallel: parse files and extract symbols into Vec<FileSymbols>
    /// 2. Sequential: aggregate into single ScopeGraphIndex with single interner
    ///
    /// This ensures all strings are deduplicated in one interner, avoiding
    /// the memory overhead of multiple interners during parallel aggregation.
    ///
    /// File paths are stored as **relative paths** (to `root_path`) for portability.
    fn build_fast(&self, root_path: &Path, file_paths: &[PathBuf]) -> Result<ScopeGraphIndex> {
        // Configure thread pool with N-1 cores
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.num_threads)
            .build()
            .map_err(|e| IndexError::WalkError {
                message: format!("Failed to create thread pool: {}", e),
            })?;

        let registry = Arc::new(LanguageRegistry::new());
        let chunk_size = self.chunk_size;
        // Clamp here against the final chunk_size so that call order of
        // with_build_batch_size / with_chunk_size on the builder does not matter.
        let build_batch_size = self.build_batch_size.max(chunk_size);
        let root_arc: Arc<Path> = Arc::from(root_path);

        let mut index = ScopeGraphIndex::new();

        // Process files in bounded merge-batches to cap two-phase peak memory.
        //
        // Old approach: collect ALL FileSymbols in one go, then merge.
        // Peak = O(total_files) symbols + growing index simultaneously.
        //
        // New approach: for each batch of build_batch_size files:
        //   1. Parse in parallel (par_chunks preserves thread-local cache locality)
        //   2. Merge the batch into the index
        //   3. Drop the batch before starting the next one
        // Peak = O(build_batch_size) symbols + growing index simultaneously.
        for batch in file_paths.chunks(build_batch_size) {
            let batch_symbols: Vec<FileSymbols> = pool.install(|| {
                batch
                    .par_chunks(chunk_size)
                    .flat_map_iter(|chunk| {
                        chunk
                            .iter()
                            .filter_map(|path| process_file_fast(path, &root_arc, &registry))
                    })
                    .collect()
            });

            for file_syms in batch_symbols {
                let path_str: &str = &file_syms.path;
                for sym in file_syms.definitions {
                    index.add_definition(&sym.name, path_str, sym.line);
                }
                for sym in file_syms.references {
                    index.add_reference(&sym.name, path_str, sym.line);
                }
                for alias in file_syms.aliases {
                    index.add_alias_arc(alias.alias, alias.original);
                }
                index.set_file_meta(path_str, file_syms.file_meta);
            }
            // batch_symbols dropped here — frees the parallel-extracted symbols
            // before the next batch is parsed
        }

        // Set the query version hash so we can detect query changes on cache load
        index.set_query_version(self.registry.compute_query_hash());

        // Reclaim over-allocated Vec capacity that accumulated during bulk push().
        // This is a one-time cost paid here (O(symbols)) to permanently reduce RSS.
        index.compact();

        Ok(index)
    }
}

impl Default for IndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Process a single file using thread-local caching.
/// Returns lightweight FileSymbols (no ScopeGraph overhead).
///
/// File path is stored as **relative** (to `root_path`) for portability.
fn process_file_fast(
    path: &Path,
    root_path: &Path,
    registry: &LanguageRegistry,
) -> Option<FileSymbols> {
    use crate::index_manager::MAX_INDEXABLE_FILE_SIZE;

    let lang_config = registry.for_file_path(path)?;
    let lang_id = lang_config.primary_language_id().to_string();

    let metadata = fs::metadata(path).ok()?;
    if metadata.len() == 0 || metadata.len() > MAX_INDEXABLE_FILE_SIZE {
        return None;
    }

    // Prefix-read binary check: only reads 8KB, not the whole file
    {
        use std::io::Read;
        let mut f = fs::File::open(path).ok()?;
        let mut buf = [0u8; 8000];
        let n = f.read(&mut buf).ok()?;
        if buf[..n].contains(&0) {
            return None;
        }
    }

    let content = fs::read(path).ok()?;

    // Parse using thread-local cached parser
    let tree = PARSER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let parser = cache.entry(lang_id.clone()).or_insert_with(|| {
            let mut p = tree_sitter::Parser::new();
            let ts_lang = lang_config.language();
            let _ = p.set_language(&ts_lang);
            p
        });
        parser.parse(&content, None)
    })?;

    let root_node = tree.root_node();

    // Extract symbols using thread-local cached query
    let (definitions, references, aliases) = QUERY_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let query = cache.entry(lang_id.clone()).or_insert_with(|| {
            lang_config.compile_query().unwrap_or_else(|_| {
                let ts_lang = lang_config.language();
                tree_sitter::Query::new(&ts_lang, "").expect("empty query should always work")
            })
        });
        extract_symbols_fast_inline(query, root_node, &content)
    });

    // Reuse metadata from the size check above (no re-stat needed)

    // Convert absolute path to relative for portable storage
    let rel_path = to_relative_path(root_path, path);

    Some(FileSymbols {
        path: rel_path.to_string_lossy().into(),
        definitions,
        references,
        aliases,
        file_meta: FileMeta::from_metadata(&metadata),
    })
}

/// Lightweight symbol extraction - returns proper typed vectors.
/// Inlined for maximum performance (avoids function call overhead in hot loop).
#[inline]
fn extract_symbols_fast_inline(
    query: &tree_sitter::Query,
    root_node: tree_sitter::Node<'_>,
    src: &[u8],
) -> (
    Vec<SymbolOccurrence>,
    Vec<SymbolOccurrence>,
    Vec<SymbolAlias>,
) {
    use tree_sitter::StreamingIterator;

    // Pre-compute capture indices for fast lookup
    let capture_names = query.capture_names();
    let mut is_def = vec![false; capture_names.len()];
    let mut is_ref = vec![false; capture_names.len()];
    let mut alias_original_idx: Option<usize> = None;
    let mut alias_name_idx: Option<usize> = None;

    for (i, name) in capture_names.iter().enumerate() {
        if name.starts_with("name.definition.") {
            is_def[i] = true;
        } else if name.starts_with("name.reference.") {
            is_ref[i] = true;
        } else if *name == "alias.original" {
            alias_original_idx = Some(i);
        } else if *name == "alias.name" {
            alias_name_idx = Some(i);
        }
    }

    // Pre-allocate with reasonable capacity
    let mut definitions: Vec<SymbolOccurrence> = Vec::with_capacity(64);
    let mut references: Vec<SymbolOccurrence> = Vec::with_capacity(256);
    let mut aliases: Vec<SymbolAlias> = Vec::with_capacity(8);

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, root_node, src);

    while let Some(match_) = matches.next() {
        let mut alias_original: Option<&[u8]> = None;
        let mut alias_name: Option<&[u8]> = None;

        for capture in match_.captures {
            let idx = capture.index as usize;
            let node = capture.node;
            let byte_range = node.byte_range();

            if is_def.get(idx).copied().unwrap_or(false) {
                // Convert Cow<str> directly to Arc<str> - avoids intermediate String allocation
                let text: Arc<str> = String::from_utf8_lossy(&src[byte_range]).into();
                // Line numbers are 1-indexed
                definitions.push(SymbolOccurrence::new(text, node.start_position().row + 1));
            } else if is_ref.get(idx).copied().unwrap_or(false) {
                let text: Arc<str> = String::from_utf8_lossy(&src[byte_range]).into();
                references.push(SymbolOccurrence::new(text, node.start_position().row + 1));
            } else if Some(idx) == alias_original_idx {
                alias_original = Some(&src[byte_range]);
            } else if Some(idx) == alias_name_idx {
                alias_name = Some(&src[byte_range]);
            }
        }

        if let (Some(original), Some(alias)) = (alias_original, alias_name) {
            // Convert Cow<str> directly to Arc<str> - avoids intermediate String allocation
            let orig_arc: Arc<str> = String::from_utf8_lossy(original).into();
            let alias_arc: Arc<str> = String::from_utf8_lossy(alias).into();
            aliases.push(SymbolAlias::new(alias_arc, orig_arc));
        }
    }

    (definitions, references, aliases)
}
