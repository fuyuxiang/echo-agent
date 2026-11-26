//! Upload destination config and archive-restore metadata shared by the
//! always-on upload queue and session restore paths.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Method for uploading to object storage.
#[derive(Clone, Debug)]
pub enum UploadMethod {
    Direct {
        service_account_key: Option<String>,
    },
    Proxy {
        proxy_base_url: String,
        user_token: String,
        deployment_key: Option<String>,
        alpha_test_key: Option<String>,
    },
    S3 {
        bucket: String,
        region: String,
        credentials_file: Option<String>,
        credentials_content: Option<String>,
        endpoint_url: Option<String>,
    },
}

/// Configuration for object-storage export.
#[derive(Clone, Debug)]
pub struct TraceExportConfig {
    pub bucket_url: Option<String>,
    pub service_account_key: Option<String>,
    pub upload_method: UploadMethod,
    pub prefix_dir: Option<String>,
    pub gcs_prefix: Option<String>,
    pub absolute_paths: bool,
    pub archive_name_override: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobCompression {
    #[default]
    None,
    Zstd,
}

pub const SKIP_DIR_NAMES: &[&str] = &[
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "env",
    ".env",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".output",
    ".cache",
    ".parcel-cache",
    ".turbo",
    "vendor",
    "bower_components",
    ".tox",
    ".nox",
    ".eggs",
    ".idea",
    ".vscode",
    ".gradle",
    ".dart_tool",
    "coverage",
    ".nyc_output",
    "htmlcov",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
];

pub fn skip_dir_set() -> &'static std::collections::HashSet<&'static str> {
    use std::collections::HashSet;
    use std::sync::LazyLock;
    static SET: LazyLock<HashSet<&str>> =
        LazyLock::new(|| SKIP_DIR_NAMES.iter().copied().collect());
    &SET
}

pub const SKIP_FILE_PATTERNS: &[&str] = &[
    "*.egg-info",
    "*.pyc",
    "*.pyo",
    "*.o",
    "*.so",
    "*.dylib",
    "*.class",
    "*.jar",
    ".DS_Store",
    "Thumbs.db",
    "*.swp",
    "*.swo",
    "*~",
    "*.iml",
];

pub fn default_untracked_exclude_globs() -> Vec<String> {
    let mut globs: Vec<String> = SKIP_DIR_NAMES.iter().map(|d| format!("{d}/")).collect();
    globs.extend(SKIP_FILE_PATTERNS.iter().map(|p| p.to_string()));
    globs
}

pub fn default_excludes_as_gitignore() -> String {
    default_untracked_exclude_globs().join("\n")
}

pub const ARCHIVE_SCHEMA_VERSION: &str = "v2";
pub const ARCHIVE_SCHEMA_VERSION_V3: &str = "v3";
pub const DEDUP_GCS_PREFIX: &str = "repo_changes_dedup";
pub const DEDUP_PATCH_SUBDIR: &str = "patches";
pub const DEDUP_BLOB_SUBDIR: &str = "blobs";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchReference {
    #[serde(rename = "type")]
    pub ref_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReference {
    #[serde(rename = "type")]
    pub ref_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub sha256: String,
    pub size_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedContent {
    pub path: String,
    pub reason: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_archive_url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub patch_references: HashMap<String, PatchReference>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub file_references: HashMap<String, FileReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<ExcludedContent>,
}
