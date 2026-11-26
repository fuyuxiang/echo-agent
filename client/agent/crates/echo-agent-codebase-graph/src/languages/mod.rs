mod golang;
mod javascript;
mod python;
mod rust;
mod ts;
pub mod types;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

pub use golang::golang;
pub use javascript::js_lang;
pub use python::python_lang;
pub use rust::rust_lang;
pub use ts::ts_lang;
pub use types::TSLanguageConfig;

/// Registry of all supported languages.
///
/// Provides lookup by extension and language ID, and can check if two
/// extensions belong to the same language family.
pub struct LanguageRegistry {
    /// All registered language configs.
    configs: Vec<Arc<TSLanguageConfig>>,
    /// Mapping from file extension to language config.
    by_extension: HashMap<String, Arc<TSLanguageConfig>>,
    /// Mapping from language ID to language config.
    by_id: HashMap<String, Arc<TSLanguageConfig>>,
}

impl LanguageRegistry {
    /// Create a new language registry with all supported languages.
    pub fn new() -> Self {
        let configs: Vec<Arc<TSLanguageConfig>> = vec![
            Arc::new(rust_lang()),
            Arc::new(ts_lang()),
            Arc::new(js_lang()),
            Arc::new(golang()),
            Arc::new(python_lang()),
        ];

        let mut by_extension = HashMap::new();
        let mut by_id = HashMap::new();

        for config in &configs {
            for ext in config.file_extensions() {
                by_extension.insert(ext.clone(), Arc::clone(config));
            }
            for id in config.language_ids() {
                by_id.insert(id.clone(), Arc::clone(config));
            }
        }

        Self {
            configs,
            by_extension,
            by_id,
        }
    }

    /// Get a language config by file extension.
    pub fn for_extension(&self, ext: &str) -> Option<Arc<TSLanguageConfig>> {
        self.by_extension.get(ext).cloned()
    }

    /// Get a language config by language ID.
    pub fn for_id(&self, id: &str) -> Option<Arc<TSLanguageConfig>> {
        self.by_id.get(id).cloned()
    }

    /// Get a language config for a file path.
    ///
    /// Extracts the extension from the path and looks up the config.
    pub fn for_file_path(&self, path: impl AsRef<Path>) -> Option<Arc<TSLanguageConfig>> {
        let path = path.as_ref();
        let ext = path.extension()?.to_str()?;
        self.for_extension(ext)
    }

    /// Check if a file path is supported (has a supported extension).
    pub fn is_supported(&self, path: impl AsRef<Path>) -> bool {
        self.for_file_path(path).is_some()
    }

    /// Get all supported file extensions.
    pub fn supported_extensions(&self) -> Vec<&str> {
        self.by_extension.keys().map(|s| s.as_str()).collect()
    }

    /// Get all language configs.
    pub fn all_configs(&self) -> &[Arc<TSLanguageConfig>] {
        &self.configs
    }

    /// Check if two file extensions belong to the same language.
    ///
    /// Returns true if both extensions are registered under the same language config.
    pub fn extensions_same_language(&self, ext1: &str, ext2: &str) -> bool {
        if ext1 == ext2 {
            return true;
        }

        match (self.by_extension.get(ext1), self.by_extension.get(ext2)) {
            (Some(config1), Some(config2)) => {
                // Compare by primary language ID (they point to the same config)
                config1.primary_language_id() == config2.primary_language_id()
            }
            _ => false,
        }
    }

    /// Compute a hash of all tree-sitter queries across all languages.
    ///
    /// This is used to detect when queries change, which should trigger
    /// a rebuild of the index even if file contents haven't changed.
    ///
    /// The hash is computed by:
    /// 1. Sorting languages by their primary ID for deterministic ordering
    /// 2. Hashing each language's query string in order
    /// 3. Combining into a single u64 hash
    pub fn compute_query_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;

        // Sort configs by primary language ID for deterministic ordering
        let mut sorted_configs: Vec<_> = self.configs.iter().collect();
        sorted_configs.sort_by_key(|c| c.primary_language_id());

        let mut hasher = DefaultHasher::new();

        for config in sorted_configs {
            // Hash the language ID and query together
            config.primary_language_id().hash(&mut hasher);
            config.file_definition_queries().hash(&mut hasher);
        }

        hasher.finish()
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}
