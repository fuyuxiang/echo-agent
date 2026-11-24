use std::collections::BTreeMap;

use super::config::LspServerConfig;
pub struct DiagnosticsSummary {
    pub text: String,
    pub file_count: usize,
    pub diagnostic_count: usize,
}

/// LSP configuration passed from shell. Same pattern as `WebSearchConfig`.
#[derive(Debug, Clone, Default)]
pub enum LspConfig {
    #[default]
    Disabled,
    Enabled {
        cwd: std::path::PathBuf,
        servers: BTreeMap<String, LspServerConfig>,
    },
}

impl LspConfig {
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { servers, .. } if !servers.is_empty())
    }
}

pub struct LspToolResult {
    pub text: String,
    pub is_error: bool,
}

/// Trait object interface for LSP operations.
///
/// Implemented by `LspBackendAdapter` which wraps `LspManager`.
#[async_trait::async_trait]
pub trait LspBackend: Send + Sync + 'static {
    fn ensure_started_background(&self);

    async fn ensure_ready(&self) -> Result<(), String>;

    fn is_ready(&self) -> bool;

    async fn dispatch(&self, input: &LspToolInput) -> LspToolResult;

    async fn drain_diagnostics(&self, timeout: std::time::Duration) -> Option<DiagnosticsSummary>;

    async fn notify_file_changed(&self, path: &std::path::Path, content: &str);

    /// Read diagnostics for specific file paths.
    ///
    /// For each path, opens the file with the LSP if not already open,
    /// waits briefly for diagnostics to settle, then returns all
    /// ERROR/WARNING diagnostics grouped by file.
    async fn read_diagnostics(&self, paths: &[std::path::PathBuf]) -> Vec<FileDiagnosticEntry>;
}

/// A single diagnostic entry returned by `LspBackend::read_diagnostics`.
#[derive(Debug, Clone)]
pub struct DiagnosticEntry {
    pub severity: DiagnosticSeverityLevel,
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub source: Option<String>,
    /// Diagnostic code from the language server (e.g. `"2322"` for TS type errors).
    pub code: Option<String>,
    /// `true` when this diagnostic was computed against an older version of the file.
    pub is_stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverityLevel {
    Error,
    Warning,
}

/// Diagnostics for a single file.
#[derive(Debug, Clone)]
pub struct FileDiagnosticEntry {
    pub path: String,
    pub diagnostics: Vec<DiagnosticEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LspOperation {
    GoToDefinition,
    FindReferences,
    Hover,
    GoToImplementation,
    DocumentSymbol,
    WorkspaceSymbol,
}

impl std::fmt::Display for LspOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GoToDefinition => write!(f, "goToDefinition"),
            Self::FindReferences => write!(f, "findReferences"),
            Self::Hover => write!(f, "hover"),
            Self::GoToImplementation => write!(f, "goToImplementation"),
            Self::DocumentSymbol => write!(f, "documentSymbol"),
            Self::WorkspaceSymbol => write!(f, "workspaceSymbol"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LspToolInput {
    #[schemars(description = "The LSP operation to perform.")]
    pub operation: LspOperation,
    #[schemars(description = "Absolute path to the file.")]
    #[serde(default)]
    pub file_path: Option<String>,
    #[schemars(description = "0-indexed line number.")]
    #[serde(default)]
    pub line: Option<u32>,
    #[schemars(description = "0-indexed column number.")]
    #[serde(default)]
    pub character: Option<u32>,
    #[schemars(description = "Symbol name or partial name (workspaceSymbol only).")]
    #[serde(default)]
    pub query: Option<String>,
}
