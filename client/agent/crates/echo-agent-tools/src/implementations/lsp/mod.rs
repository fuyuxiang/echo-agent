#[cfg(not(feature = "desktop"))]
pub mod capabilities;
#[cfg(not(feature = "desktop"))]
pub mod client;
pub mod config;
#[cfg(not(feature = "desktop"))]
pub mod diagnostics;
#[cfg(not(feature = "desktop"))]
pub mod dispatch;
#[cfg(not(feature = "desktop"))]
pub mod documents;
#[cfg(not(feature = "desktop"))]
pub mod format;
#[cfg(not(feature = "desktop"))]
pub mod manager;
#[cfg(not(feature = "desktop"))]
pub mod pending;
#[cfg(not(feature = "desktop"))]
pub mod pull;
#[cfg(not(feature = "desktop"))]
pub mod refresh;
#[cfg(not(feature = "desktop"))]
pub mod restart;
mod types;
#[cfg(not(feature = "desktop"))]
pub mod workspace_open;

#[cfg(all(test, not(feature = "desktop")))]
mod tests;

#[cfg(not(feature = "desktop"))]
pub use dispatch::LspBackendAdapter;
#[cfg(not(feature = "desktop"))]
pub use manager::{LspManager, drain_lsp_diagnostics};
pub use types::DiagnosticsSummary;
#[cfg(not(feature = "desktop"))]
pub use restart::restart_monitor;
pub use types::{
    DiagnosticEntry, DiagnosticSeverityLevel, FileDiagnosticEntry, LspBackend, LspConfig,
    LspOperation, LspToolInput, LspToolResult,
};

// ── Shared types used across submodules ─────────────────────────────────

#[cfg(not(feature = "desktop"))]
use std::path::Path;
#[cfg(not(feature = "desktop"))]
use std::sync::Arc;

#[cfg(not(feature = "desktop"))]
use async_lsp::lsp_types::{Position, TextDocumentIdentifier, TextDocumentPositionParams, Url};

/// How long a reader will wait for diagnostics to arrive after an edit before
/// reporting what it has.
///
/// This is the budget the whole after-edit diagnostics path is sized against:
/// anything scheduled to happen later than this — a pull retry, say — answers
/// after the reader has already given up. Kept here, next to the pieces that
/// have to agree on it, rather than as a number at the call site.
pub const DIAGNOSTICS_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(not(feature = "desktop"))]
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("failed to spawn LSP server: {0}")]
    SpawnFailed(String),
    #[error("LSP server '{0}' timed out after {1:?}")]
    Timeout(String, std::time::Duration),
    #[error("LSP initialization failed: {0}")]
    InitFailed(String),
    #[error("LSP request failed: {0}")]
    RequestFailed(String),
    #[error("invalid file path")]
    InvalidPath,
}

#[cfg(not(feature = "desktop"))]
pub type DiagnosticsNotify = Arc<tokio::sync::Notify>;
#[cfg(not(feature = "desktop"))]
pub type LspMainLoop = async_lsp::MainLoop<async_lsp::router::Router<()>>;

#[cfg(not(feature = "desktop"))]
pub fn file_uri(path: &Path) -> Result<Url, LspError> {
    Url::from_file_path(path).map_err(|_| LspError::InvalidPath)
}

#[cfg(not(feature = "desktop"))]
pub fn text_document_position(
    path: &Path,
    line: u32,
    column: u32,
) -> Result<TextDocumentPositionParams, LspError> {
    Ok(TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
            uri: file_uri(path)?,
        },
        position: Position {
            line,
            character: column,
        },
    })
}
